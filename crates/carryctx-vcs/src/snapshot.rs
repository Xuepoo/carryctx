//! Local snapshot-ref value types and commit-trailer parsing (CTX-0144).
//!
//! The local-only snapshot ref (design
//! `2026-09-10-mergeable-git-managed-state.md` §3.1, decision DEC-0052)
//! stores one commit per snapshot at [`SNAPSHOT_REF_DEFAULT`], with the
//! ctxpack directory at the commit root. Each commit message ends in
//! machine-readable trailers so the export-id DAG can be reconstructed from
//! local Git objects alone:
//!
//! ```text
//! CarryCtx-Export-Id: 01M...
//! CarryCtx-Parents: 01M...,01M...
//! CarryCtx-Source: <repo>@<short-sha> (branch)
//! ```
//!
//! The ref is deliberately *not* a `refs/heads/*` branch (DEC-0052, issue
//! #138): a plain user `git push` cannot move it, so unredacted state cannot
//! reach a remote without an explicit refspec. The public redacted publication
//! ref [`PUBLIC_SNAPSHOT_REF`] is reserved for the redaction/publication flow
//! and is never written by an unredacted `export --snapshot`; CarryCtx itself
//! never pushes any ref.
//!
//! This module is pure: no Git, filesystem, or database I/O. The plumbing that
//! reads and writes commits lives in [`crate::git`]; the DAG itself
//! ([`carryctx_pack::merge::ExportDag`]) is built by the CLI application layer
//! so `carryctx-vcs` keeps its core-only dependency graph. Parsing returns the
//! neutral [`SnapshotRefCommit`] node the caller converts.

/// Default local-only unredacted snapshot ref: one ref per clone, shared by
/// linked worktrees via the repository's common Git directory (design §3.1).
///
/// It lives outside `refs/heads/*` on purpose (DEC-0052): a plain `git push`
/// (even `--all`) cannot move it, so publishing unredacted state requires an
/// explicit user-supplied refspec. The public redacted publication ref
/// [`PUBLIC_SNAPSHOT_REF`] is reserved for the publication flow.
pub const SNAPSHOT_REF_DEFAULT: &str = "refs/carryctx/local";

/// Namespace prefix every local-only snapshot ref must live under.
pub const LOCAL_SNAPSHOT_REF_PREFIX: &str = "refs/carryctx/";

/// Public redacted publication ref reserved for the publication flow
/// (DEC-0052, issue #138). Unredacted `--snapshot` exports must never write
/// it; only the redaction/publication flow may.
pub const PUBLIC_SNAPSHOT_REF: &str = "refs/heads/carryctx-snapshots";

/// Manifest file name read from the snapshot commit root.
pub const SNAPSHOT_MANIFEST_FILE: &str = "manifest.json";

/// Trailer key carrying a commit's own export id.
pub const EXPORT_ID_TRAILER: &str = "CarryCtx-Export-Id";

/// Trailer key carrying a commit's ordered parent export ids (comma-separated).
pub const PARENTS_TRAILER: &str = "CarryCtx-Parents";

/// Trailer key carrying the source label (`<repo>@<short-sha> (branch)`).
pub const SOURCE_TRAILER: &str = "CarryCtx-Source";

/// Parsed commit-message trailers of one snapshot commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotTrailers {
    /// The commit's own export id, when the trailer is present and non-empty.
    pub export_id: Option<String>,
    /// Ordered parent export ids (`[]` for a first snapshot).
    pub parents: Vec<String>,
    /// Free-form source label, when present and non-empty.
    pub source: Option<String>,
}

impl SnapshotTrailers {
    /// Parse the CarryCtx trailers out of a commit message.
    ///
    /// Trailer lines are matched anywhere in the message (Git convention is the
    /// final paragraph); the last occurrence of each key wins, mirroring
    /// `git interpret-trailers`. Malformed/empty values degrade to absent
    /// rather than erroring so a corrupt message cannot fail a ref walk.
    pub fn parse(message: &str) -> Self {
        let mut trailers = Self::default();
        for line in message.lines() {
            let line = line.trim_end();
            if let Some(value) = line.strip_prefix(EXPORT_ID_TRAILER) {
                if let Some(value) = value.strip_prefix(':') {
                    let value = value.trim();
                    if !value.is_empty() {
                        trailers.export_id = Some(value.to_string());
                    }
                }
            } else if let Some(value) = line.strip_prefix(PARENTS_TRAILER) {
                if let Some(value) = value.strip_prefix(':') {
                    trailers.parents = value
                        .split(',')
                        .map(str::trim)
                        .filter(|parent| !parent.is_empty())
                        .map(str::to_string)
                        .collect();
                }
            } else if let Some(value) = line.strip_prefix(SOURCE_TRAILER) {
                if let Some(value) = value.strip_prefix(':') {
                    let value = value.trim();
                    if !value.is_empty() {
                        trailers.source = Some(value.to_string());
                    }
                }
            }
        }
        trailers
    }

    /// Render the trailer block (no leading blank line, trailing newline).
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(export_id) = &self.export_id {
            out.push_str(EXPORT_ID_TRAILER);
            out.push_str(": ");
            out.push_str(export_id);
            out.push('\n');
        }
        out.push_str(PARENTS_TRAILER);
        out.push_str(": ");
        out.push_str(&self.parents.join(","));
        out.push('\n');
        if let Some(source) = &self.source {
            out.push_str(SOURCE_TRAILER);
            out.push_str(": ");
            out.push_str(source);
            out.push('\n');
        }
        out
    }
}

/// One parsed snapshot commit read back from a ref's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRefCommit {
    /// Git commit sha.
    pub commit: String,
    /// The commit's `CarryCtx-Export-Id`.
    pub export_id: String,
    /// The commit's ordered `CarryCtx-Parents` export ids.
    pub parents: Vec<String>,
    /// The optional `CarryCtx-Source` label.
    pub source: Option<String>,
}

/// Render the full commit message for a snapshot commit (subject + trailers).
///
/// `subject` is a one-line summary; the trailer block is appended after a blank
/// line so `git log --oneline` stays readable and `git interpret-trailers`
/// could parse it.
pub fn render_snapshot_message(subject: &str, trailers: &SnapshotTrailers) -> String {
    format!("{subject}\n\n{}", trailers.render())
}

/// Result of [`crate::VcsBackend::create_snapshot_commit`]: the new commit, the
/// ref tip it replaced (if any), and the resolved parent export ids recorded in
/// the new commit's `CarryCtx-Parents` trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCommit {
    /// The newly created commit sha.
    pub commit: String,
    /// The previous ref tip commit sha (`None` when the ref was created).
    pub previous: Option<String>,
    /// Export ids resolved from the parent commits' trailers.
    pub parent_export_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_all_three_trailers() {
        let message = "chore(ctxpack): snapshot 01EXP (repo @ abc1234)\n\n\
CarryCtx-Export-Id: 01EXP\n\
CarryCtx-Parents: 01P1,01P2\n\
CarryCtx-Source: repo@abc1234 (main)\n";
        let trailers = SnapshotTrailers::parse(message);
        assert_eq!(trailers.export_id.as_deref(), Some("01EXP"));
        assert_eq!(trailers.parents, ["01P1", "01P2"]);
        assert_eq!(trailers.source.as_deref(), Some("repo@abc1234 (main)"));
    }

    #[test]
    fn parse_empty_parents_is_a_root_snapshot() {
        let message = "subject\n\nCarryCtx-Export-Id: 01ROOT\nCarryCtx-Parents: \n";
        let trailers = SnapshotTrailers::parse(message);
        assert_eq!(trailers.export_id.as_deref(), Some("01ROOT"));
        assert!(trailers.parents.is_empty());
    }

    #[test]
    fn parse_ignores_unknown_lines_and_blank_values() {
        let trailers = SnapshotTrailers::parse("hello\nworld\n");
        assert_eq!(trailers, SnapshotTrailers::default());
        let blank = SnapshotTrailers::parse("CarryCtx-Export-Id:   \nCarryCtx-Parents: , ,\n");
        assert_eq!(blank, SnapshotTrailers::default());
    }

    #[test]
    fn render_round_trips_through_parse() {
        let trailers = SnapshotTrailers {
            export_id: Some("01EXP".into()),
            parents: vec!["01P1".into(), "01P2".into()],
            source: Some("repo@abc (main)".into()),
        };
        let message =
            render_snapshot_message("chore(ctxpack): snapshot 01EXP (repo@abc)", &trailers);
        assert_eq!(SnapshotTrailers::parse(&message), trailers);
    }
}
