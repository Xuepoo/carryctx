//! Two-parent merge snapshot commits (CTX-0145, design
//! `2026-09-10-mergeable-git-managed-state.md` §3.1–§3.4).
//!
//! After a merge has been durably applied (the database swap is the commit
//! point), `import --mode merge --snapshot-ref <ref>` and
//! `conflict apply --snapshot-ref <ref>` write one commit whose Git parents are
//! `[local ref tip, incoming snapshot commit]` and whose `CarryCtx-Parents`
//! trailer lists both parent export ids in the same order. The bundle tree is
//! serialized from the just-merged live database with the same serializer as
//! `export` ([`crate::application::export::serialize_bundle_files`]), so the
//! merge commit and a directory export of the same state are byte-identical.
//!
//! ## Ordering (documented decision)
//!
//! The database swap is the authoritative commit point. This module runs *after*
//! a successful swap, so:
//!
//! - the ref can only ever point at a state that is actually live, never at a
//!   candidate that failed to become live;
//! - `snapshot_state.last_export_id`/`last_snapshot_commit` are written in one
//!   transaction only *after* the commit exists, so `snapshot_state` never
//!   claims a commit that does not exist;
//! - if the commit itself fails (for example the compare-and-swap detects a
//!   concurrent ref move), the merge stays applied and the caller surfaces a
//!   `GIT_ERROR` (exit 4) with a re-run hint. It does **not** roll the database
//!   back, because reverting a durable, already-audited merge would be worse
//!   than a missing snapshot commit.
//!
//! The caller reads the local ref tip once (needed for the skip decision and
//! the envelope) and passes it in; [`GitBackend::create_merge_snapshot_commit`]
//! re-reads the tip and applies the compare-and-swap guard against it.
//!
//! [`GitBackend::create_merge_snapshot_commit`]: crate::adapter::git::VcsBackend::create_merge_snapshot_commit

use std::path::Path;

use crate::adapter::git::{GitCli, GitProject, VcsBackend};
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteSnapshotStateRepository;
use crate::application::export::{
    build_manifest, collect_snapshot, counts_of, read_snapshot_tip, serialize_bundle_files,
    snapshot_source_label, snapshot_subject_label, validate_snapshot_ref,
};
use crate::domain::pack;
use crate::error::CarryCtxError;
use carryctx_core::repository::snapshot_state::{
    LAST_EXPORT_ID, LAST_SNAPSHOT_COMMIT, SnapshotStateRepository,
};

/// Write one two-parent merge snapshot commit against the just-merged live
/// database and record it in `snapshot_state`.
///
/// `local_tip` and `incoming` are `(commit sha, export id)` pairs; the new
/// commit's Git parents and manifest `parents` are
/// `[local_tip, incoming]` in that order. Callers must only invoke this when
/// both are resolvable.
pub fn write_merge_snapshot_commit(
    db_path: &Path,
    gp: &GitProject,
    git_ref: &str,
    local_tip: (&str, &str),
    incoming: (&str, &str),
) -> Result<serde_json::Value, CarryCtxError> {
    validate_snapshot_ref(&gp.repository_root, git_ref)?;
    let (local_commit, local_export_id) = local_tip;
    let (incoming_commit, incoming_export_id) = incoming;

    let git = GitCli::new();
    let snapshot = {
        let database = ProjectDatabase::open_readonly(db_path)?;
        collect_snapshot(database.connection())?
    };
    let counts = counts_of(&snapshot.tables);

    let export_id = ulid::Ulid::generate().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let parents = vec![local_export_id.to_string(), incoming_export_id.to_string()];
    let manifest = build_manifest(
        &snapshot,
        counts.clone(),
        export_id,
        created_at.clone(),
        gp.branch.clone(),
        gp.head.clone(),
        parents.clone(),
    );
    pack::check_counts(&manifest, &counts)?;

    let files = serialize_bundle_files(&manifest, &snapshot.project, &snapshot.tables)?;
    let source_label = snapshot_source_label(gp);
    let subject_label = snapshot_subject_label(gp);
    let commit = git.create_merge_snapshot_commit(
        &gp.repository_root,
        git_ref,
        &files,
        &manifest.export_id,
        &[local_commit.to_string(), incoming_commit.to_string()],
        &parents,
        &source_label,
        &subject_label,
    )?;

    // Both keys move together: one transaction so a failure between the two
    // upserts can never leave `last_export_id` and `last_snapshot_commit`
    // describing different snapshots. The commit already exists, so this only
    // ever records a real commit.
    let database = ProjectDatabase::open(db_path)?;
    let tx = database.connection().unchecked_transaction().map_err(|e| {
        CarryCtxError::database_error(format!(
            "Failed to start the merge snapshot_state transaction: {e}"
        ))
    })?;
    {
        let state = SqliteSnapshotStateRepository::new(&tx);
        let project_id = &manifest.project_id;
        state.set(project_id, LAST_EXPORT_ID, &manifest.export_id, &created_at)?;
        state.set(
            project_id,
            LAST_SNAPSHOT_COMMIT,
            &commit.commit,
            &created_at,
        )?;
    }
    tx.commit().map_err(|e| {
        CarryCtxError::database_error(format!(
            "Failed to commit the merge snapshot_state transaction: {e}"
        ))
    })?;

    Ok(serde_json::json!({
        "ref": git_ref,
        "commit": commit.commit,
        "previousCommit": commit.previous,
        "parentExportIds": commit.parent_export_ids,
        "parents": parents,
        "source": source_label,
    }))
}

/// Read the current tip `(commit sha, export id)` of the local snapshot ref,
/// or `None` when the ref does not exist.
///
/// Shared by the merge paths so the skip decision and the compare-and-swap
/// guard read the ref the same way.
pub(crate) fn local_snapshot_tip(
    git: &GitCli,
    gp: &GitProject,
    git_ref: &str,
) -> Result<Option<(String, String)>, CarryCtxError> {
    read_snapshot_tip(git, &gp.repository_root, git_ref)
}
