use crate::adapter::sqlite_repos::SqliteEventRepository;
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

    // Fetch one extra row to detect whether another page follows.
    let adjusted_limit = filter.limit.map(|l| l + 1);
    let adjusted_filter = EventFilter {
        project_id: project_id.to_string(),
        task_id: filter.task_id.clone(),
        agent_id: filter.agent_id.clone(),
        session_id: filter.session_id.clone(),
        event_type: filter.event_type.clone(),
        since: filter.since.clone(),
        until: filter.until.clone(),
        limit: adjusted_limit,
    };

    let mut events =
        repo.list_before_cursor(&adjusted_filter, before_ts.as_deref(), before_id.as_deref())?;

    // Determine next cursor from the lookahead row.
    let next_cursor = match filter.limit {
        Some(limit) => {
            if events.len() > limit as usize {
                events.truncate(limit as usize);
                events.last().map(|e| encode_cursor(&e.occurred_at, &e.id))
            } else {
                None
            }
        }
        None => None,
    };

    Ok(CursorList {
        events,
        next_cursor,
    })
}

/// Opaque cursor encoding: `{occurred_at}|{id}`. RFC3339 timestamps never
/// contain `|` and ids are ULIDs, so a single split is unambiguous; anything
/// else is rejected instead of silently returning wrong pages.
fn encode_cursor(occurred_at: &str, id: &str) -> String {
    format!("{occurred_at}|{id}")
}

fn decode_cursor(token: &str) -> Result<(String, String), CarryCtxError> {
    let (ts, id) = token.split_once('|').ok_or_else(|| {
        CarryCtxError::validation_error(
            "Invalid event cursor format; use a cursor previously returned by this command.",
        )
    })?;
    if ts.is_empty() || id.is_empty() {
        return Err(CarryCtxError::validation_error(
            "Invalid event cursor format; use a cursor previously returned by this command.",
        ));
    }
    Ok((ts.to_string(), id.to_string()))
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
    fn cursor_rejects_garbage() {
        assert!(decode_cursor("no-separator").is_err());
        assert!(decode_cursor("|only-id").is_err());
        assert!(decode_cursor("only-ts|").is_err());
        assert!(decode_cursor("").is_err());
    }
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
