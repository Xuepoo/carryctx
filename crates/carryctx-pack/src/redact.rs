//! Export-time secret redaction for redacted publication bundles (CTX-0155).
//!
//! Publication artifacts are pushed to public repositories, so secret-shaped
//! values must be replaced with [`REDACTED`] before the bundle is written and
//! committed to the public ref (DEC-0052, issue #138; design
//! `2026-09-10-mergeable-git-managed-state.md` §3.6). The heuristics are a
//! faithful port of the established `bitty-devtools` `publish-ctxpack-redact`
//! pipeline:
//!
//! 1. **Field-name rule**: a JSON object field whose name is secret-shaped
//!    ([`is_secret_name`]) has a string value replaced wholesale. Segments are
//!    `KEY`/`TOKEN`/`SECRET`/`PASSWORD`/`PAT`, plus `GH_PAT` anywhere and
//!    `CLOUDFLARE_`/`AWS_` prefixes (over-matching is intentional:
//!    fail-closed).
//! 2. **Free-text rule**: in every remaining string, `NAME=value` /
//!    `NAME: value` pairs with a secret-shaped NAME keep the name and lose the
//!    value; and standalone 40+-character token-like runs are replaced. Git
//!    SHA-1 runs and single-class slugs are exempt so commits, branches, and
//!    paths survive.
//! 3. Nested JSON-encoded strings (ctxpack `payload`-style columns) are
//!    decoded, redacted recursively, and re-encoded only when a replacement
//!    fired, so untouched values stay byte-identical.
//!
//! Redaction mutates values in place: row count, row order, and the bundle
//! file set are preserved, so manifest counts and import validation still
//! hold. The local database is never touched.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// Replacement marker for every redacted value.
pub const REDACTED: &str = "***REDACTED***";

/// Field/variable name segments that mark a value as secret-shaped.
const SECRET_SEGMENTS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PAT"];

/// `NAME = value` / `NAME: value` pairs (quoted or bare value) on one line.
///
/// The name matcher mirrors the reference regex without backreferences: the
/// opening and closing quote are captured separately and must match in the
/// callback, which is what the Python backreference enforced.
static ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?P<oq>["']?)\b(?P<name>[A-Za-z_][A-Za-z0-9_.\-]*)(?P<cq>["']?)[ \t]*(?P<sep>[:=])[ \t]*(?:"(?P<dqval>[^"]*)"|'(?P<sqval>[^']*)'|(?P<val>[^\s"'`,;]+))"#,
    )
    .expect("assignment regex is valid")
});

/// Standalone 40+-character run that may be a token. `/` and mid-run `=` are
/// deliberately outside the alphabet so paths and URLs are never mangled;
/// up to two trailing `=` absorb base64 padding.
static RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-+]{40,}={0,2}").expect("run regex is valid"));

/// Exactly-40 hex run: a Git SHA-1, ubiquitous in workflow prose.
static SHA1_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]{40}$").expect("sha1 regex is valid"));

/// True when `value` already carries the redaction marker (or a `***`
/// prefix), so re-running the pass is a no-op.
fn already_redacted(value: &str) -> bool {
    value == REDACTED || value.starts_with("***")
}

/// True when a field/variable name is secret-shaped (fail-closed spec list).
///
/// Segment-based: `monkey`, `keyboard`, `tokenizer`, `DispatchPriority`, and
/// `GOPATH_BIN` do not match, while `api_key`, `OPENAI_API_KEY`, `gh_pat`,
/// and `handle_key_event` do.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_uppercase();
    if upper.contains("GH_PAT") {
        return true;
    }
    if upper.starts_with("CLOUDFLARE_") || upper.starts_with("AWS_") {
        return true;
    }
    segments(name)
        .iter()
        .any(|segment| SECRET_SEGMENTS.contains(&segment.as_str()))
}

/// Split a field/variable name into UPPER-cased word segments the way the
/// reference pipeline does: separators (`_`, `-`, `.`) first, then camelCase
/// boundaries (`APIKey` -> `API` `KEY`).
fn segments(name: &str) -> Vec<String> {
    let mut found = Vec::new();
    for part in name.split(['_', '-', '.']) {
        let chars: Vec<char> = part.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            let start = index;
            if chars[index].is_ascii_uppercase() {
                while index < chars.len() && chars[index].is_ascii_uppercase() {
                    index += 1;
                }
                if index - start > 1 && index < chars.len() && chars[index].is_ascii_lowercase() {
                    index -= 1;
                }
                while index < chars.len()
                    && (chars[index].is_ascii_lowercase() || chars[index].is_ascii_digit())
                {
                    index += 1;
                }
            } else {
                while index < chars.len() && !chars[index].is_ascii_uppercase() {
                    index += 1;
                }
            }
            let segment: String = chars[start..index].iter().collect();
            found.push(segment.to_uppercase());
        }
    }
    found
}

/// True when a 40+ run mixes character classes like a real token rather than
/// a lowercase slug; `+`/`=` base64 punctuation already qualifies.
fn looks_tokenish(run: &str) -> bool {
    if run.contains('+') || run.ends_with('=') {
        return true;
    }
    let mut classes = 0;
    if run.chars().any(|c| c.is_ascii_lowercase()) {
        classes += 1;
    }
    if run.chars().any(|c| c.is_ascii_uppercase()) {
        classes += 1;
    }
    if run.chars().any(|c| c.is_ascii_digit()) {
        classes += 1;
    }
    classes >= 2
}

/// Apply the free-text rules to one string; returns `(new_text, replacements)`.
pub fn redact_text(text: &str) -> (String, u64) {
    let mut count = 0u64;

    let mut stage = String::with_capacity(text.len());
    let mut last = 0usize;
    for captures in ASSIGN_RE.captures_iter(text) {
        let name = captures.name("name").expect("named group").as_str();
        let opening_quote = captures.name("oq").expect("named group").as_str();
        let closing_quote = captures.name("cq").expect("named group").as_str();
        if opening_quote != closing_quote || !is_secret_name(name) {
            continue;
        }
        let value = captures
            .name("dqval")
            .or_else(|| captures.name("sqval"))
            .or_else(|| captures.name("val"))
            .expect("one value alternative matches");
        if value.as_str().is_empty() || already_redacted(value.as_str()) {
            continue;
        }
        count += 1;
        stage.push_str(&text[last..value.start()]);
        stage.push_str(REDACTED);
        last = value.end();
    }
    stage.push_str(&text[last..]);
    let text = stage;

    let mut stage = String::with_capacity(text.len());
    let mut last = 0usize;
    for found in RUN_RE.find_iter(&text) {
        let run = found.as_str();
        if already_redacted(run) || SHA1_RE.is_match(run) || !looks_tokenish(run) {
            continue;
        }
        count += 1;
        stage.push_str(&text[last..found.start()]);
        stage.push_str(REDACTED);
        last = found.end();
    }
    stage.push_str(&text[last..]);
    (stage, count)
}

/// Redact one JSON value in place (key rule, nested JSON strings, free text);
/// returns the number of replacements.
pub fn redact_value(node: &mut Value) -> u64 {
    match node {
        Value::Object(map) => {
            let mut count = 0;
            for (key, value) in map.iter_mut() {
                if let Value::String(text) = value {
                    if is_secret_name(key) {
                        if !text.is_empty() && !already_redacted(text) {
                            *text = REDACTED.to_string();
                            count += 1;
                        }
                        continue;
                    }
                }
                count += redact_value(value);
            }
            count
        }
        Value::Array(items) => items.iter_mut().map(redact_value).sum(),
        Value::String(text) => {
            let (redacted, count) = redact_string(text);
            if count > 0 {
                *text = redacted;
            }
            count
        }
        _ => 0,
    }
}

/// Redact one string: decode JSON-looking values recursively, then fall back
/// to the free-text rules. Only re-encoded when a replacement fired.
fn redact_string(text: &str) -> (String, u64) {
    let stripped = text.trim();
    if stripped.len() >= 2 && (stripped.starts_with('{') || stripped.starts_with('[')) {
        if let Ok(mut decoded) = serde_json::from_str::<Value>(stripped) {
            if decoded.is_object() || decoded.is_array() {
                let count = redact_value(&mut decoded);
                if count > 0 {
                    return (
                        serde_json::to_string(&decoded).unwrap_or_else(|_| text.to_string()),
                        count,
                    );
                }
                return (text.to_string(), 0);
            }
        }
    }
    redact_text(text)
}

/// Redact every row of one table in place; returns the replacement count.
/// Row count and order are preserved by construction.
pub fn redact_rows(rows: &mut [Value]) -> u64 {
    rows.iter_mut().map(redact_value).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn redact(text: &str) -> (String, u64) {
        redact_text(text)
    }

    #[test]
    fn secret_names_match_the_spec_list() {
        for secret in [
            "api_key",
            "OPENAI_API_KEY",
            "gh_pat",
            "handle_key_event",
            "AWS_SECRET_ACCESS_KEY",
            "CLOUDFLARE_API_TOKEN",
            "dbPassword",
            "token",
            "PAT",
            "apiKey",
        ] {
            assert!(is_secret_name(secret), "{secret} must be secret-shaped");
        }
        for benign in [
            "monkey",
            "keyboard",
            "tokenizer",
            "DispatchPriority",
            "GOPATH_BIN",
            "patrol",
            "path",
            "name",
        ] {
            assert!(!is_secret_name(benign), "{benign} must stay readable");
        }
    }

    #[test]
    fn secret_keyed_fields_are_replaced_wholesale() {
        let mut row = json!({
            "id": "01ABC",
            "api_key": "sk-live-abc123",
            "name": "monkey project",
            "nested": {"access_token": "t0ken", "note": "tokenizer"},
        });
        let count = redact_value(&mut row);
        assert_eq!(count, 2);
        assert_eq!(row["api_key"], REDACTED);
        assert_eq!(row["nested"]["access_token"], REDACTED);
        assert_eq!(row["name"], "monkey project");
        assert_eq!(row["nested"]["note"], "tokenizer");
        assert_eq!(row["id"], "01ABC");
    }

    #[test]
    fn assignment_pairs_keep_names_and_lose_values() {
        let cases = [
            (
                "OPENAI_API_KEY=sk-abc123",
                format!("OPENAI_API_KEY={REDACTED}"),
            ),
            ("password: hunter2", format!("password: {REDACTED}")),
            ("token = \"a b c\"", format!("token = \"{REDACTED}\"")),
            ("secret='x y'", format!("secret='{REDACTED}'")),
        ];
        for (input, expected) in cases {
            let (output, count) = redact(input);
            assert_eq!(output, expected, "input {input}");
            assert_eq!(count, 1, "input {input}");
        }
        for benign in [
            "monkey=banana",
            "GOPATH_BIN=/usr/bin",
            "DispatchPriority: high",
        ] {
            let (output, count) = redact(benign);
            assert_eq!(output, benign);
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn token_like_runs_are_replaced_with_exemptions() {
        let mixed = "Abcdeg0123456789Abcdef0123456789Abcdef01";
        assert_eq!(mixed.len(), 40);
        let (output, count) = redact(&format!("value {mixed} end"));
        assert_eq!(output, format!("value {REDACTED} end"));
        assert_eq!(count, 1);

        let sha = "0123456789abcdef0123456789abcdef01234567";
        let (output, count) = redact(&format!("commit {sha}"));
        assert_eq!(output, format!("commit {sha}"));
        assert_eq!(count, 0);

        let slug = "soak_read_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (output, count) = redact(slug);
        assert_eq!(output, slug);
        assert_eq!(count, 0);

        let path = "/home/user/workspace/Abcdef0123456789Abcdef0123456789";
        let (output, count) = redact(path);
        assert_eq!(output, path);
        assert_eq!(count, 0);
    }

    #[test]
    fn nested_json_strings_are_decoded_only_when_redaction_fires() {
        let mut row = json!({"payload": "{\"api_key\":\"abc\",\"n\":1}"});
        let count = redact_value(&mut row);
        assert_eq!(count, 1);
        let decoded: Value = serde_json::from_str(row["payload"].as_str().unwrap()).unwrap();
        assert_eq!(decoded["api_key"], REDACTED);
        assert_eq!(decoded["n"], 1);

        let untouched = "{\"task\":\"one\",\"n\":1}";
        let mut row = json!({"payload": untouched});
        assert_eq!(redact_value(&mut row), 0);
        assert_eq!(row["payload"], untouched);
    }

    #[test]
    fn redaction_is_idempotent_and_row_count_preserving() {
        let mut rows = vec![
            json!({"api_key": "abc", "token_count": 3}),
            json!({"log": "OPENAI_API_KEY=sk-abc123"}),
            json!({"commit": "0123456789abcdef0123456789abcdef01234567"}),
        ];
        let original_len = rows.len();
        let first = redact_rows(&mut rows);
        assert!(first >= 2);
        let after_first = rows.clone();
        assert_eq!(redact_rows(&mut rows), 0);
        assert_eq!(rows, after_first);
        assert_eq!(rows.len(), original_len);
        assert_eq!(rows[0]["api_key"], REDACTED);
        assert_eq!(rows[0]["token_count"], 3);
        assert!(rows[1]["log"].as_str().unwrap().contains(REDACTED));
        assert_eq!(
            rows[2]["commit"],
            "0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn assigned_values_inside_nested_json_are_redacted() {
        let mut row = json!({"payload": "{\"env\":{\"GH_PAT\":\"ghp_x\"}}"});
        let count = redact_value(&mut row);
        assert_eq!(count, 1);
        let decoded: Value = serde_json::from_str(row["payload"].as_str().unwrap()).unwrap();
        assert_eq!(decoded["env"]["GH_PAT"], REDACTED);
    }
}
