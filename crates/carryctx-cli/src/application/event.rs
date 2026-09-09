use crate::adapter::sqlite_repos::{DEFAULT_EVENT_LIST_LIMIT, SqliteEventRepository};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::error::CarryCtxError;
use crate::repository::event::{EventFilter, EventRecord, EventRepository};

/// Result of listing events with cursor-based pagination
#[derive(serde::Serialize)]
pub struct CursorList {
    pub events: Vec<EventRecord>,
    pub next_cursor: Option<String>,
}

/// List events with cursor-based keyset pagination.
///
/// The cursor is an opaque token encoding the `(occurred_at, id)` tuple of
/// the last event in the previous page. Pagination is strict on the tuple:
/// bulk transitions emit many events sharing one timestamp, and an inclusive
/// `occurred_at <=` bound alone re-served those rows forever. Pass the
/// returned `next_cursor` back as `cursor` to fetch the following page.
pub fn list_events(
    project_id: &str,
    filter: &EventFilter,
    cursor: Option<&str>,
    uow: &UnitOfWork,
) -> Result<CursorList, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteEventRepository::new(conn);

    let (before_ts, before_id) = match cursor {
        Some(token) => {
            let (ts, id) = decode_cursor(token)?;
            (Some(ts), Some(id))
        }
        None => (None, None),
    };

    // The effective page size is the explicit limit or the repository's
    // default cap; one extra lookahead row detects whether another page
    // follows so `next_cursor` is real even when the caller omitted
    // `--limit` (CTX-0080).
    let effective_limit = filter.limit.unwrap_or(DEFAULT_EVENT_LIST_LIMIT);
    let fetch_limit = effective_limit
        .checked_add(1)
        .ok_or_else(|| CarryCtxError::validation_error("Event list limit is too large."))?;
    let adjusted_filter = EventFilter {
        project_id: project_id.to_string(),
        task_id: filter.task_id.clone(),
        agent_id: filter.agent_id.clone(),
        session_id: filter.session_id.clone(),
        event_type: filter.event_type.clone(),
        since: filter.since.clone(),
        until: filter.until.clone(),
        limit: Some(fetch_limit),
    };

    let mut events =
        repo.list_before_cursor(&adjusted_filter, before_ts.as_deref(), before_id.as_deref())?;

    // Determine next cursor from the lookahead row.
    let next_cursor = if events.len() > effective_limit as usize {
        events.truncate(effective_limit as usize);
        events.last().map(|e| encode_cursor(&e.occurred_at, &e.id))
    } else {
        None
    };

    Ok(CursorList {
        events,
        next_cursor,
    })
}

/// Domain-separation pepper for the cursor checksum. Cursors are tamper-
/// evidence tokens, not secrets: the goal (CTX-0083) is that hand-edited
/// plaintext tuples fail validation instead of silently skipping rows.
const CURSOR_PEPPER: &str = "carryctx.event.cursor.v1";

/// Short checksum over the keyset payload: first 4 bytes of
/// SHA-256(pepper ‖ payload), hex-encoded (8 chars).
fn cursor_checksum(payload: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(CURSOR_PEPPER.as_bytes());
    hasher.update([0u8]);
    hasher.update(payload.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..4])
}

const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Unpadded base64url encoding of the raw cursor payload.
fn base64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(BASE64URL_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(BASE64URL_ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(BASE64URL_ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(BASE64URL_ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

/// Inverse of [`base64url_encode`]; rejects invalid characters and lengths.
fn base64url_decode(token: &str) -> Option<Vec<u8>> {
    fn value_of(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    if token.is_empty() {
        return Some(Vec::new());
    }
    let bytes = token.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut n: u32 = 0;
        for (i, &byte) in chunk.iter().enumerate() {
            n |= value_of(byte)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Opaque cursor encoding (CTX-0083): the `(occurred_at, id)` tuple is
/// base64url-wrapped and suffixed with a short checksum so hand-edited or
/// forged plaintext tokens fail validation instead of silently returning
/// wrong pages. Tokens from the original plaintext format (`ts|id`) are
/// still accepted for compatibility; they are naturally rewritten whenever
/// a new `next_cursor` is emitted.
fn encode_cursor(occurred_at: &str, id: &str) -> String {
    let payload = format!("{occurred_at}|{id}");
    format!(
        "{}.{}",
        base64url_encode(payload.as_bytes()),
        cursor_checksum(&payload)
    )
}

fn decode_cursor(token: &str) -> Result<(String, String), CarryCtxError> {
    const INVALID: &str =
        "Invalid event cursor format; use a cursor previously returned by this command.";

    // Current format: `<base64url(payload)><checksum>`.
    if let Some(split_index) = token.rfind('.') {
        let (body, checksum) = token.split_at(split_index);
        let checksum = &checksum[1..];
        return match base64url_decode(body).and_then(|bytes| String::from_utf8(bytes).ok()) {
            Some(payload) if cursor_checksum(&payload) == checksum => {
                parse_cursor_payload(&payload)
            }
            _ => Err(CarryCtxError::validation_error(INVALID)),
        };
    }

    // Legacy fallback: plaintext `ts|id` tokens issued by the pre-opaque
    // format.
    parse_cursor_payload(token)
}

fn parse_cursor_payload(payload: &str) -> Result<(String, String), CarryCtxError> {
    const INVALID: &str =
        "Invalid event cursor format; use a cursor previously returned by this command.";
    let (ts, id) = payload
        .split_once('|')
        .ok_or_else(|| CarryCtxError::validation_error(INVALID))?;
    if ts.is_empty() || id.is_empty() || id.contains('|') {
        return Err(CarryCtxError::validation_error(INVALID));
    }
    Ok((ts.to_string(), id.to_string()))
}

/// Show a single event by ID
pub fn show_event(
    project_id: &str,
    event_id: &str,
    uow: &UnitOfWork,
) -> Result<EventRecord, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteEventRepository::new(conn);

    let event = repo.find_by_id(project_id, event_id)?;
    event.ok_or_else(|| CarryCtxError::resource_not_found(format!("Event '{event_id}' not found.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip() {
        let token = encode_cursor("2026-08-24T10:00:00+00:00", "01JABCDEF");
        assert_eq!(
            decode_cursor(&token).unwrap(),
            (
                "2026-08-24T10:00:00+00:00".to_string(),
                "01JABCDEF".to_string()
            )
        );
    }

    #[test]
    fn cursor_token_is_opaque_not_plaintext() {
        // CTX-0083: the emitted token must not contain the raw `ts|id`
        // plaintext; it is base64url-wrapped with a checksum suffix.
        let token = encode_cursor("2026-08-24T10:00:00+00:00", "01JABCDEF");
        assert!(
            !token.contains('|'),
            "token must not leak the raw keyset tuple: {token}"
        );
        assert!(token.contains('.'), "token must carry a checksum suffix");
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert!(decode_cursor("no-separator").is_err());
        assert!(decode_cursor("|only-id").is_err());
        assert!(decode_cursor("only-ts|").is_err());
        assert!(decode_cursor("").is_err());
    }

    #[test]
    fn cursor_rejects_tampered_tokens() {
        let token = encode_cursor("2026-08-24T10:00:00+00:00", "01JABCDEF");

        // Flipping any payload byte invalidates the checksum.
        let (body, mac) = token.split_once('.').unwrap();
        let mut tampered_body = body.as_bytes().to_vec();
        let last = tampered_body
            .iter_mut()
            .rev()
            .find(|b| **b != b'A')
            .unwrap();
        *last = b'A';
        assert!(
            decode_cursor(&format!(
                "{}.{mac}",
                String::from_utf8(tampered_body).unwrap()
            ))
            .is_err()
        );

        // A valid base64url body with a wrong checksum is rejected.
        assert!(decode_cursor(&format!("{body}.00000000")).is_err());
        // Missing checksum suffix is rejected.
        assert!(decode_cursor(body).is_err());
    }

    #[test]
    fn cursor_still_accepts_legacy_plaintext_tokens() {
        // Back-compat: tokens issued by the pre-opaque format keep working;
        // they are rewritten on the next emitted next_cursor.
        assert_eq!(
            decode_cursor("2026-08-24T10:00:00+00:00|01JABCDEF").unwrap(),
            (
                "2026-08-24T10:00:00+00:00".to_string(),
                "01JABCDEF".to_string()
            )
        );
    }

    #[test]
    fn cursor_accepts_fractional_second_timestamps() {
        let token = encode_cursor("2026-08-25T06:53:42.256236685+00:00", "01JABCDEF");
        assert_eq!(
            decode_cursor(&token).unwrap(),
            (
                "2026-08-25T06:53:42.256236685+00:00".to_string(),
                "01JABCDEF".to_string()
            )
        );
    }
}
