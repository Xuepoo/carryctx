use serde::{Deserialize, Serialize};

/// Which entity kind a search hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Task,
    Progress,
    Checkpoint,
    Decision,
}

impl SearchKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchKind::Task => "task",
            SearchKind::Progress => "progress",
            SearchKind::Checkpoint => "checkpoint",
            SearchKind::Decision => "decision",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task" => Some(SearchKind::Task),
            "progress" => Some(SearchKind::Progress),
            "checkpoint" => Some(SearchKind::Checkpoint),
            "decision" => Some(SearchKind::Decision),
            _ => None,
        }
    }
}

/// A single full-text search hit, always resolved back to its owning task
/// (display ID + status) and, where available, the branch it was worked on
/// — the reason most searches like this happen in the first place, per
/// https://github.com/Xuepoo/carryctx/issues/45.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: SearchKind,
    /// ULID of the matched record itself (task/progress/checkpoint/decision).
    pub id: String,
    /// Human-facing display ID of the matched record, if it has its own
    /// (progress items and decisions do; checkpoints don't).
    pub display_id: Option<String>,
    pub task_id: String,
    pub task_display_id: String,
    pub task_status: String,
    /// Best-known branch for the owning task: the task's own worktree
    /// binding if present, else the branch recorded on the matched
    /// checkpoint (if this hit is a checkpoint), else `None`.
    pub branch: Option<String>,
    /// A short excerpt around the match, with `[...]` bracketing the hit
    /// (SQLite FTS5's `snippet()` semantics).
    pub snippet: String,
    /// BM25 relevance score. Lower is more relevant (SQLite FTS5
    /// convention); results are already sorted by this ascending.
    pub score: f64,
    pub created_at: String,
}

/// FTS5 operator keywords, recognized only in uppercase per SQLite's
/// grammar (`and`/`or`/`not` lowercase are ordinary search terms).
const FTS5_OPERATORS: [&str; 3] = ["AND", "OR", "NOT"];

/// Rewrite a raw, user-typed search query into a SQLite FTS5 `MATCH`
/// expression that treats hyphens, colons, and other FTS5-special
/// characters as literal text rather than query syntax, while still
/// letting a caller opt into FTS5 syntax explicitly.
///
/// FTS5's grammar treats a bare (unquoted) token containing `-` as a
/// `column:term`-style filter/exclusion rather than literal text — e.g.
/// `aria-owns` is parsed as "exclude column `owns`", which fails with a
/// misleading `no such column: owns` for any project column that isn't
/// named `owns`. See <https://github.com/Xuepoo/carryctx/issues/47>.
///
/// This wraps every bare token containing FTS5-special characters in
/// double quotes (doubling any embedded `"` per FTS5's escaping rule),
/// which makes SQLite treat it as literal text. It leaves untouched:
/// - phrases already wrapped in double quotes (the caller opted into
///   exact-phrase syntax),
/// - the uppercase boolean operators `AND`, `OR`, `NOT` (documented FTS5
///   syntax `--help` advertises),
/// - a trailing `*` on a token (prefix-query syntax), which is moved
///   outside the closing quote so `foo*` still means "prefix `foo`" and
///   `aria-owns*` becomes `"aria-owns"*` rather than losing the prefix
///   match.
pub fn sanitize_fts5_query(query: &str) -> String {
    let mut out_tokens: Vec<String> = Vec::new();
    let mut chars = query.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            // A quote at a token boundary opens an already-quoted phrase:
            // copy through verbatim, including the closing quote and any
            // immediately-following `*`.
            let mut phrase = String::from(chars.next().unwrap());
            for ch in chars.by_ref() {
                phrase.push(ch);
                if ch == '"' {
                    break;
                }
            }
            if chars.peek() == Some(&'*') {
                phrase.push(chars.next().unwrap());
            }
            out_tokens.push(phrase);
            continue;
        }
        // Bare token: read up to the next whitespace. A `"` appearing
        // here is mid-token (we already saw a non-quote, non-whitespace
        // character first), not a phrase boundary, so it's swept into
        // the token and escaped like any other special character.
        let mut token = String::new();
        while let Some(&ch) = chars.peek() {
            if ch.is_whitespace() {
                break;
            }
            token.push(ch);
            chars.next();
        }
        if token.is_empty() {
            continue;
        }
        if FTS5_OPERATORS.contains(&token.as_str()) {
            out_tokens.push(token);
            continue;
        }
        let needs_escaping = token
            .chars()
            .any(|ch| !ch.is_alphanumeric() && ch != '_' && ch != '*');
        if !needs_escaping {
            out_tokens.push(token);
            continue;
        }
        let trailing_star = token.ends_with('*') && token.len() > 1;
        let body = if trailing_star {
            &token[..token.len() - 1]
        } else {
            token.as_str()
        };
        let escaped_body = body.replace('"', "\"\"");
        if trailing_star {
            out_tokens.push(format!("\"{escaped_body}\"*"));
        } else {
            out_tokens.push(format!("\"{escaped_body}\""));
        }
    }

    out_tokens.join(" ")
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_fts5_query;

    #[test]
    fn bare_hyphenated_token_is_quoted() {
        assert_eq!(sanitize_fts5_query("aria-owns"), "\"aria-owns\"");
    }

    #[test]
    fn multiple_bare_hyphenated_tokens_each_quoted() {
        assert_eq!(
            sanitize_fts5_query("aria-owns screen-reader"),
            "\"aria-owns\" \"screen-reader\""
        );
    }

    #[test]
    fn plain_alphanumeric_token_is_untouched() {
        assert_eq!(sanitize_fts5_query("markdown"), "markdown");
    }

    #[test]
    fn already_quoted_phrase_is_untouched() {
        assert_eq!(sanitize_fts5_query("\"aria-owns\""), "\"aria-owns\"");
    }

    #[test]
    fn uppercase_boolean_operators_are_preserved() {
        assert_eq!(sanitize_fts5_query("term1 OR term2"), "term1 OR term2");
        assert_eq!(
            sanitize_fts5_query("pointer-events AND cross-origin"),
            "\"pointer-events\" AND \"cross-origin\""
        );
    }

    #[test]
    fn lowercase_and_or_not_are_treated_as_plain_terms() {
        // Lowercase and/or/not are not FTS5 operators, so they pass
        // through unescaped since they contain no special characters.
        assert_eq!(sanitize_fts5_query("cat or dog"), "cat or dog");
    }

    #[test]
    fn trailing_star_prefix_query_preserved_on_hyphenated_token() {
        assert_eq!(sanitize_fts5_query("aria-owns*"), "\"aria-owns\"*");
    }

    #[test]
    fn trailing_star_prefix_query_preserved_on_plain_token() {
        assert_eq!(sanitize_fts5_query("mark*"), "mark*");
    }

    #[test]
    fn embedded_quote_in_bare_token_is_doubled() {
        assert_eq!(sanitize_fts5_query("foo\"bar-baz"), "\"foo\"\"bar-baz\"");
    }

    #[test]
    fn column_filter_colon_syntax_is_escaped() {
        assert_eq!(sanitize_fts5_query("content:plain"), "\"content:plain\"");
    }

    #[test]
    fn empty_query_returns_empty_string() {
        assert_eq!(sanitize_fts5_query(""), "");
        assert_eq!(sanitize_fts5_query("   "), "");
    }

    #[test]
    fn mixed_quoted_and_bare_tokens() {
        assert_eq!(
            sanitize_fts5_query("\"exact phrase\" aria-owns"),
            "\"exact phrase\" \"aria-owns\""
        );
    }
}
