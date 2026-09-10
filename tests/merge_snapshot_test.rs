//! CTX-0145 integration tests: two-parent merge snapshot commits written by
//! `import --mode merge --snapshot-ref` and `conflict apply --snapshot-ref`
//! (design `2026-09-10-mergeable-git-managed-state.md` §3.1–§3.4, AC8).
//!
//! Every fixture uses disposable Git repositories and a local bare remote with
//! no network. The two-clone cycle is fully offline: A pushes its local-only
//! snapshot ref to the bare remote with an explicit refspec, B fetches it,
//! imports, diverges, merges, and writes its own two-parent merge commit.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use carryctx_cli::adapter::git::{GitBackend, GitCli, SnapshotTrailers, VcsBackend};

/// Default local-only unredacted snapshot ref (never pushed by the binary).
const LOCAL_SNAP_REF: &str = "refs/carryctx/local";

fn json(output: &Output) -> serde_json::Value {
    let stream = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(stream).unwrap_or_else(|error| {
        panic!(
            "expected JSON output, got {error}: {}",
            String::from_utf8_lossy(stream)
        )
    })
}

fn run(repo: &Path, bin: &Path, args: &[&str]) -> Output {
    common::run_cmd(repo, bin, args)
}

fn git_out(repo: &Path, args: &[&str]) -> Output {
    const GIT_STATE_VARS: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
    ];
    let mut command = Command::new("git");
    command.args(args).current_dir(repo);
    for var in GIT_STATE_VARS {
        command.env_remove(var);
    }
    command.output().expect("git should spawn")
}

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let output = git_out(repo, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ctx0145_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A local bare remote (no network transport).
fn bare_remote(root: &Path) -> PathBuf {
    let remote = root.join("remote.git");
    git_ok(root, &["init", "--bare", remote.to_str().unwrap()]);
    remote
}

fn init_project(dir: &Path, bin: &Path, tasks: &[&str]) {
    common::init_and_agent(dir, bin);
    for title in tasks {
        let created = run(dir, bin, &["task", "create", "--title", title, "--json"]);
        assert!(created.status.success(), "task create failed: {created:?}");
    }
}

fn export(dir: &Path, bin: &Path, out: &Path, snapshot: bool) -> serde_json::Value {
    let mut args = vec![
        "export",
        "--pack-format",
        "dir",
        "-o",
        out.to_str().unwrap(),
    ];
    if snapshot {
        args.push("--snapshot");
    }
    args.push("--json");
    let output = run(dir, bin, &args);
    assert!(output.status.success(), "export failed: {output:?}");
    json(&output)["data"].clone()
}

fn fetch_into(dest: &Path, remote: &Path, source_ref: &str, dest_ref: &str) {
    git_ok(
        dest,
        &[
            "fetch",
            "--quiet",
            remote.to_str().unwrap(),
            &format!("{source_ref}:{dest_ref}"),
        ],
    );
}

fn state_value(repo: &Path, key: &str) -> Option<String> {
    let db = repo.join(".git/carryctx/state.sqlite");
    let conn = rusqlite::Connection::open(db).ok()?;
    conn.query_row(
        "SELECT value FROM snapshot_state WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .ok()
}

fn task_id_by_title(repo: &Path, title: &str) -> String {
    let db = repo.join(".git/carryctx/state.sqlite");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT id FROM tasks WHERE title = ?1", [title], |row| {
        row.get(0)
    })
    .unwrap()
}

/// The single staged merge session directory.
fn only_session_dir(repo: &Path) -> PathBuf {
    let merges = repo.join(".git/carryctx/merges");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&merges)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs.len(),
        1,
        "expected exactly one staged session: {dirs:?}"
    );
    dirs.pop().unwrap()
}

fn session_merge_json(repo: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(only_session_dir(repo).join("merge.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn conflicts_json(repo: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(only_session_dir(repo).join("conflicts.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn first_open_conflict_id(repo: &Path) -> String {
    conflicts_json(repo)
        .as_array()
        .unwrap()
        .iter()
        .find(|conflict| conflict["resolution"].is_null())
        .expect("at least one open conflict")
        .get("id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

fn parents_of(repo: &Path, commit: &str) -> Vec<String> {
    git_ok(repo, &["rev-list", "--parents", "-n", "1", commit])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn commit_trailers(repo: &Path, commit: &str) -> SnapshotTrailers {
    let message = git_ok(repo, &["log", "-1", "--format=%B", commit]);
    SnapshotTrailers::parse(&message)
}

// ── two-clone AC8 ─────────────────────────────────────────────────────────

struct TwoClone {
    a: PathBuf,
    b: PathBuf,
    bin: PathBuf,
    remote: PathBuf,
    a1_export: String,
    b1_export: String,
    b1_commit: String,
}

/// A at its first snapshot A1 pushed to the remote; B has fetched A1 into its
/// local-only ref, imported it, diverged with `b-edit`, and snapshotted B1.
fn setup_two_clone(name: &str) -> TwoClone {
    let out = scratch(name);
    let remote = bare_remote(&out);

    let (a, bin) = common::setup_test_project(&format!("{name}_a"));
    init_project(&a, &bin, &["base task"]);
    let a1 = export(&a, &bin, &out.join("a1"), true);
    let a1_export = a1["manifest"]["export_id"].as_str().unwrap().to_string();
    git_ok(
        &a,
        &[
            "push",
            "--quiet",
            remote.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:refs/heads/state"),
        ],
    );

    let (b, _) = common::setup_test_project(&format!("{name}_b"));
    fetch_into(&b, &remote, "refs/heads/state", LOCAL_SNAP_REF);
    let imported = run(
        &b,
        &bin,
        &["import", "--from-git", LOCAL_SNAP_REF, "--json"],
    );
    assert!(imported.status.success(), "B import failed: {imported:?}");
    let created = run(&b, &bin, &["task", "create", "--title", "b-edit", "--json"]);
    assert!(created.status.success());
    let b1 = export(&b, &bin, &out.join("b1"), true);
    let b1_export = b1["manifest"]["export_id"].as_str().unwrap().to_string();
    let b1_commit = b1["snapshot"]["commit"].as_str().unwrap().to_string();

    TwoClone {
        a,
        b,
        bin,
        remote,
        a1_export,
        b1_export,
        b1_commit,
    }
}

/// A diverges from its current state with a new task, snapshots, and pushes.
fn a_diverges(tc: &TwoClone, title: &str) -> (String, String) {
    let created = run(
        &tc.a,
        &tc.bin,
        &["task", "create", "--title", title, "--json"],
    );
    assert!(created.status.success());
    let out = tc.a.parent().unwrap().join(format!("a-{title}"));
    let data = export(&tc.a, &tc.bin, &out, true);
    git_ok(
        &tc.a,
        &[
            "push",
            "--quiet",
            tc.remote.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:refs/heads/state"),
        ],
    );
    (
        data["manifest"]["export_id"].as_str().unwrap().to_string(),
        data["snapshot"]["commit"].as_str().unwrap().to_string(),
    )
}

#[test]
fn two_clone_merge_writes_a_two_parent_commit_and_records_state() {
    let tc = setup_two_clone("two_clone");
    let (a2_export, a2_commit) = a_diverges(&tc, "a-edit");

    fetch_into(
        &tc.b,
        &tc.remote,
        "refs/heads/state",
        "refs/remotes/origin/state",
    );
    let head_before = git_ok(&tc.b, &["rev-parse", "HEAD"]);
    let status_before = git_ok(&tc.b, &["status", "--porcelain"]);
    let exported_before = state_value(&tc.b, "last_export_id");

    let merged = run(
        &tc.b,
        &tc.bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/state",
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert!(merged.status.success(), "merge failed: {merged:?}");
    let data = json(&merged)["data"].clone();
    let merge_commit = data["snapshot"]["commit"].as_str().unwrap().to_string();

    // Two Git parents: the local B tip first, the incoming A commit second.
    let parents = parents_of(&tc.b, &merge_commit);
    assert_eq!(
        parents,
        vec![tc.b1_commit.clone(), a2_commit.clone()],
        "merge commit must have [local tip, incoming] parents"
    );

    // The `CarryCtx-Parents` trailer lists both export ids in the same order.
    let trailers = commit_trailers(&tc.b, &merge_commit);
    assert_eq!(
        trailers.parents,
        vec![tc.b1_export.clone(), a2_export.clone()]
    );
    assert!(trailers.export_id.is_some());

    // The committed tree is the merged bundle: both divergent edits survive.
    let committed_tasks = git_ok(
        &tc.b,
        &["cat-file", "-p", &format!("{merge_commit}:tasks.jsonl")],
    );
    assert!(
        committed_tasks.contains("b-edit"),
        "local edit missing from the merge commit tree"
    );
    assert!(
        committed_tasks.contains("a-edit"),
        "incoming edit missing from the merge commit tree"
    );

    // The committed manifest's parents agree with the trailer (no dropped edge).
    let committed_manifest = git_ok(
        &tc.b,
        &["cat-file", "-p", &format!("{merge_commit}:manifest.json")],
    );
    let committed_manifest: serde_json::Value = serde_json::from_str(&committed_manifest).unwrap();
    assert_eq!(
        committed_manifest["parents"],
        serde_json::json!([tc.b1_export.clone(), a2_export.clone()]),
        "manifest.parents must match the CarryCtx-Parents trailer"
    );

    // `snapshot_state` records the new commit and export id.
    assert_eq!(
        state_value(&tc.b, "last_snapshot_commit").as_deref(),
        Some(merge_commit.as_str())
    );
    assert_eq!(
        state_value(&tc.b, "last_export_id").as_deref(),
        trailers.export_id.as_deref()
    );
    let default_ref = git_ok(&tc.b, &["rev-parse", LOCAL_SNAP_REF]);
    assert_eq!(default_ref, merge_commit, "ref tip is the merge commit");

    // No index, worktree, or HEAD mutation.
    assert_eq!(git_ok(&tc.b, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git_ok(&tc.b, &["status", "--porcelain"]), status_before);
    assert_ne!(
        exported_before.as_deref(),
        state_value(&tc.b, "last_export_id").as_deref(),
        "merge snapshot must record a fresh export id"
    );
}

#[test]
fn second_merge_resolves_the_ancestor_base_through_the_merge_commit() {
    let tc = setup_two_clone("second_merge");
    let (a2_export, _a2_commit) = a_diverges(&tc, "a-edit");

    fetch_into(
        &tc.b,
        &tc.remote,
        "refs/heads/state",
        "refs/remotes/origin/state",
    );
    let first = run(
        &tc.b,
        &tc.bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/state",
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert!(first.status.success(), "first merge failed: {first:?}");
    assert_eq!(
        json(&first)["data"]["baseExportId"].as_str(),
        Some(tc.a1_export.as_str())
    );

    // A diverges again from A2; B fetches and merges a second time.
    let (_a3_export, _a3_commit) = a_diverges(&tc, "a2-edit");
    fetch_into(
        &tc.b,
        &tc.remote,
        "refs/heads/state",
        "refs/remotes/origin/state",
    );
    let second = run(
        &tc.b,
        &tc.bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/state",
            "--mode",
            "merge",
            "--require-base",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert!(second.status.success(), "second merge failed: {second:?}");
    let data = json(&second)["data"].clone();
    assert_eq!(data["baseSource"], "ancestor");
    assert_eq!(data["baseExportId"].as_str(), Some(a2_export.as_str()));
    assert_eq!(data["degraded"], false);
}

// ── conflict apply ────────────────────────────────────────────────────────

struct ConflictClone {
    b: PathBuf,
    bin: PathBuf,
    b1_commit: String,
    a2_commit: String,
}

/// A and B both edit the same base task; B stages a strict conflict from the
/// remote A2 snapshot while its own local-only ref sits at B1.
fn setup_conflict_clone(name: &str) -> ConflictClone {
    let out = scratch(name);
    let remote = bare_remote(&out);

    let (a, bin) = common::setup_test_project(&format!("{name}_a"));
    init_project(&a, &bin, &["base task"]);
    let a1 = export(&a, &bin, &out.join("a1"), true);
    assert!(a1["snapshot"]["commit"].is_string());
    git_ok(
        &a,
        &[
            "push",
            "--quiet",
            remote.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:refs/heads/state"),
        ],
    );

    let (b, _) = common::setup_test_project(&format!("{name}_b"));
    fetch_into(&b, &remote, "refs/heads/state", LOCAL_SNAP_REF);
    let imported = run(
        &b,
        &bin,
        &["import", "--from-git", LOCAL_SNAP_REF, "--json"],
    );
    assert!(imported.status.success(), "B import failed: {imported:?}");
    let base_task = task_id_by_title(&b, "base task");
    let b_edit = run(
        &b,
        &bin,
        &["task", "edit", &base_task, "--title", "b edit", "--json"],
    );
    assert!(b_edit.status.success(), "B edit failed: {b_edit:?}");
    let b1 = export(&b, &bin, &out.join("b1"), true);
    let b1_commit = b1["snapshot"]["commit"].as_str().unwrap().to_string();

    let a_edit = run(
        &a,
        &bin,
        &["task", "edit", &base_task, "--title", "a edit", "--json"],
    );
    assert!(a_edit.status.success(), "A edit failed: {a_edit:?}");
    let a2 = export(&a, &bin, &out.join("a2"), true);
    let a2_commit = a2["snapshot"]["commit"].as_str().unwrap().to_string();
    git_ok(
        &a,
        &[
            "push",
            "--quiet",
            remote.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:refs/heads/state"),
        ],
    );
    fetch_into(&b, &remote, "refs/heads/state", "refs/remotes/origin/state");

    ConflictClone {
        b,
        bin,
        b1_commit,
        a2_commit,
    }
}

#[test]
fn conflict_apply_with_snapshot_ref_writes_a_two_parent_commit() {
    let tc = setup_conflict_clone("conflict");
    let staged = run(
        &tc.b,
        &tc.bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/state",
            "--mode",
            "merge",
            "--strict-edits",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert_eq!(staged.status.code(), Some(3), "staging: {staged:?}");
    assert_eq!(json(&staged)["error"]["code"], "MERGE_CONFLICTS");

    // The incoming snapshot commit is persisted at staging time.
    let merge_json = session_merge_json(&tc.b);
    assert_eq!(
        merge_json["sourceCommit"].as_str(),
        Some(tc.a2_commit.as_str())
    );
    assert_eq!(merge_json["sourceRef"], "refs/remotes/origin/state");

    // `--dry-run` writes no ref and no `snapshot_state`.
    let ref_before = git_ok(&tc.b, &["rev-parse", LOCAL_SNAP_REF]);
    let commit_before = state_value(&tc.b, "last_snapshot_commit");
    let dry = run(
        &tc.b,
        &tc.bin,
        &[
            "conflict",
            "apply",
            "--snapshot-ref=refs/carryctx/local",
            "--dry-run",
            "--json",
        ],
    );
    assert!(dry.status.success(), "dry-run failed: {dry:?}");
    let dry_data = json(&dry)["data"].clone();
    assert_eq!(dry_data["applied"], false);
    assert_eq!(dry_data["snapshot"]["wouldCommit"], true);
    assert_eq!(git_ok(&tc.b, &["rev-parse", LOCAL_SNAP_REF]), ref_before);
    assert_eq!(state_value(&tc.b, "last_snapshot_commit"), commit_before);

    // Resolve the open conflict at `theirs`, then apply for real.
    let conflict_id = first_open_conflict_id(&tc.b);
    let resolved = run(
        &tc.b,
        &tc.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(
        &tc.b,
        &tc.bin,
        &[
            "conflict",
            "apply",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert!(applied.status.success(), "apply failed: {applied:?}");
    let data = json(&applied)["data"].clone();
    let merge_commit = data["snapshot"]["commit"].as_str().unwrap().to_string();

    let parents = parents_of(&tc.b, &merge_commit);
    assert_eq!(
        parents,
        vec![tc.b1_commit.clone(), tc.a2_commit.clone()],
        "conflict apply merge commit must have [local tip, incoming] parents"
    );
    let trailers = commit_trailers(&tc.b, &merge_commit);
    assert_eq!(trailers.parents.len(), 2);
    assert_eq!(
        state_value(&tc.b, "last_snapshot_commit").as_deref(),
        Some(merge_commit.as_str())
    );
    // The applied row is the chosen (theirs) title.
    assert_eq!(git_ok(&tc.b, &["rev-parse", LOCAL_SNAP_REF]), merge_commit);
}

// ── skip / no-op / dry-run / CAS ──────────────────────────────────────────

#[test]
fn directory_merge_without_incoming_snapshot_skips_with_warning() {
    let (dir, bin) = common::setup_test_project("ctx0145_dir_skip");
    let out = scratch("ctx0145_dir_skip_out");
    init_project(&dir, &bin, &["base task"]);
    let _snap = export(&dir, &bin, &out.join("snap"), true);
    let ref_before = git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]);
    let export_before = state_value(&dir, "last_export_id");
    let commit_before = state_value(&dir, "last_snapshot_commit");

    // A plain export has a fresh export id that was never committed to the ref.
    let bundle = out.join("plain");
    export(&dir, &bin, &bundle, false);
    let merged = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert!(merged.status.success(), "merge failed: {merged:?}");
    let data = json(&merged)["data"].clone();
    assert!(
        data.get("snapshot").is_none(),
        "an unresolvable incoming commit must skip the snapshot: {data}"
    );
    let warnings = data["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .unwrap()
            .contains("No incoming snapshot commit")),
        "expected a skip warning: {warnings:?}"
    );
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), ref_before);
    assert_eq!(state_value(&dir, "last_export_id"), export_before);
    assert_eq!(state_value(&dir, "last_snapshot_commit"), commit_before);
}

#[test]
fn merge_without_snapshot_ref_writes_no_ref_or_state() {
    let (dir, bin) = common::setup_test_project("ctx0145_no_ref");
    let out = scratch("ctx0145_no_ref_out");
    init_project(&dir, &bin, &["base task"]);
    let _snap = export(&dir, &bin, &out.join("snap"), true);
    let ref_before = git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]);
    let export_before = state_value(&dir, "last_export_id");
    let commit_before = state_value(&dir, "last_snapshot_commit");

    let bundle = out.join("plain");
    export(&dir, &bin, &bundle, false);
    let merged = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "merge failed: {merged:?}");
    let data = json(&merged)["data"].clone();
    assert!(data.get("snapshot").is_none(), "no snapshot key: {data}");
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), ref_before);
    assert_eq!(state_value(&dir, "last_export_id"), export_before);
    assert_eq!(state_value(&dir, "last_snapshot_commit"), commit_before);
}

#[test]
fn merge_snapshot_dry_run_writes_nothing() {
    let (dir, bin) = common::setup_test_project("ctx0145_merge_dry");
    let out = scratch("ctx0145_merge_dry_out");
    init_project(&dir, &bin, &["base task"]);
    let _snap = export(&dir, &bin, &out.join("snap"), true);
    let ref_before = git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]);
    let commit_before = state_value(&dir, "last_snapshot_commit");

    let bundle = out.join("plain");
    export(&dir, &bin, &bundle, false);
    let dry = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/local",
            "--dry-run",
            "--json",
        ],
    );
    assert!(dry.status.success(), "dry-run failed: {dry:?}");
    let data = json(&dry)["data"].clone();
    assert_eq!(data["applied"], false);
    assert_eq!(data["snapshot"]["wouldCommit"], true);
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), ref_before);
    assert_eq!(state_value(&dir, "last_snapshot_commit"), commit_before);
}

#[test]
fn merge_snapshot_cas_move_fails_closed_without_state_change() {
    let (dir, bin) = common::setup_test_project("ctx0145_cas");
    init_project(&dir, &bin, &["one"]);
    let first = export(&dir, &bin, &dir.join("p1"), true);
    let first_commit = first["snapshot"]["commit"].as_str().unwrap().to_string();
    let first_export = first["manifest"]["export_id"].as_str().unwrap().to_string();

    let created = run(&dir, &bin, &["task", "create", "--title", "two", "--json"]);
    assert!(created.status.success());
    let second = export(&dir, &bin, &dir.join("p2"), true);
    let second_commit = second["snapshot"]["commit"].as_str().unwrap().to_string();
    let second_export = second["manifest"]["export_id"]
        .as_str()
        .unwrap()
        .to_string();

    let export_before = state_value(&dir, "last_export_id");
    let commit_before = state_value(&dir, "last_snapshot_commit");
    assert_eq!(export_before.as_deref(), Some(second_export.as_str()));

    // Simulate another worktree moving the ref between our read and our write.
    git_ok(&dir, &["update-ref", LOCAL_SNAP_REF, &first_commit]);

    let db = dir.join(".git/carryctx/state.sqlite");
    let gp = GitCli::new().discover(&dir).unwrap();
    let error = carryctx_cli::application::merge_snapshot::write_merge_snapshot_commit(
        &db,
        &gp,
        LOCAL_SNAP_REF,
        (&second_commit, &second_export),
        (&first_commit, &first_export),
    )
    .expect_err("stale local tip must fail the compare-and-swap");
    assert_eq!(error.code, "GIT_ERROR");
    // No partial `snapshot_state`: both keys still describe the last snapshot.
    assert_eq!(state_value(&dir, "last_export_id"), export_before);
    assert_eq!(state_value(&dir, "last_snapshot_commit"), commit_before);
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), first_commit);
}

// ── regressions from independent review ───────────────────────────────────

/// MINOR 1: an unflagged merge must resolve its base exactly as before
/// CTX-0145. The local snapshot-ref history only joins the DAG when the caller
/// opts in with `--snapshot-ref`.
#[test]
fn unflagged_dir_merge_ignores_the_local_snapshot_ref_for_base_resolution() {
    let out = scratch("unflagged_gate");
    let (src, bin) = common::setup_test_project("unflagged_gate_src");
    init_project(&src, &bin, &["base task"]);
    let s1 = export(&src, &bin, &out.join("s1"), true);
    let m1 = s1["manifest"]["export_id"].as_str().unwrap().to_string();

    // tgt fetches src's snapshot ref, bare-imports it, then snapshots locally:
    // its local ref now holds U1 -> M1 (a real common ancestor for src's M2).
    let (tgt, _) = common::setup_test_project("unflagged_gate_tgt");
    git_ok(
        &tgt,
        &[
            "fetch",
            "--quiet",
            src.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:{LOCAL_SNAP_REF}"),
        ],
    );
    let imported = run(
        &tgt,
        &bin,
        &["import", "--from-git", LOCAL_SNAP_REF, "--json"],
    );
    assert!(
        imported.status.success(),
        "bare import failed: {imported:?}"
    );
    let t1 = export(&tgt, &bin, &out.join("t1"), true);
    let u1 = t1["manifest"]["export_id"].as_str().unwrap().to_string();
    assert_eq!(
        t1["snapshot"]["parentExportIds"][0], m1,
        "tgt local snapshot must chain to src's M1"
    );
    assert_eq!(
        state_value(&tgt, "last_export_id").as_deref(),
        Some(u1.as_str())
    );

    // src diverges: s2's manifest.parents is [M1].
    let created = run(
        &src,
        &bin,
        &["task", "create", "--title", "src extra", "--json"],
    );
    assert!(created.status.success());
    let s2_dir = out.join("s2");
    export(&src, &bin, &s2_dir, true);

    // Unflagged: the divergent local ref must NOT widen base resolution, so no
    // common ancestor is found and the merge degrades (baseSource "none").
    let unflagged = run(
        &tgt,
        &bin,
        &[
            "import",
            s2_dir.to_str().unwrap(),
            "--mode",
            "merge",
            "--dry-run",
            "--json",
        ],
    );
    assert!(unflagged.status.success(), "unflagged: {unflagged:?}");
    let unflagged = json(&unflagged)["data"].clone();
    assert_eq!(
        unflagged["baseSource"], "none",
        "unflagged dir merge must not consult the local snapshot ref"
    );
    assert!(unflagged["base"].is_null());

    // Opting in with `--snapshot-ref` consults the local ref: M1 is common.
    let flagged = run(
        &tgt,
        &bin,
        &[
            "import",
            s2_dir.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref",
            "--dry-run",
            "--json",
        ],
    );
    assert!(flagged.status.success(), "flagged: {flagged:?}");
    let flagged = json(&flagged)["data"].clone();
    assert_eq!(flagged["baseSource"], "ancestor");
    assert_eq!(flagged["base"], m1);

    // Same result in a repo without the ref: deleting it changes nothing.
    git_ok(&tgt, &["update-ref", "-d", LOCAL_SNAP_REF]);
    let after = run(
        &tgt,
        &bin,
        &[
            "import",
            s2_dir.to_str().unwrap(),
            "--mode",
            "merge",
            "--dry-run",
            "--json",
        ],
    );
    assert!(after.status.success(), "after delete: {after:?}");
    assert_eq!(json(&after)["data"]["baseSource"], unflagged["baseSource"]);
}

/// MINOR 2: the merge commit's `CarryCtx-Parents` trailer uses the caller's
/// resolved export ids verbatim, so an incoming commit without a trailer can
/// never silently drop the DAG edge; a length mismatch fails closed.
#[test]
fn merge_commit_trailer_uses_explicit_ids_and_fails_closed_on_mismatch() {
    let (dir, _bin) = common::setup_test_project("merge_trailer_explicit");
    let backend = GitBackend::new();
    let files = |content: &str| vec![("manifest.json".to_string(), content.as_bytes().to_vec())];

    let local = backend
        .create_snapshot_commit(
            &dir,
            LOCAL_SNAP_REF,
            &files("{}"),
            "01LOCAL",
            &[],
            "repo@a (main)",
            "main @ aaa1111",
        )
        .expect("local snapshot");

    // An "incoming" commit whose message has no CarryCtx trailer but whose tree
    // is nonetheless the merged parent.
    let head = git_ok(&dir, &["rev-parse", "HEAD"]);

    let merged = backend
        .create_merge_snapshot_commit(
            &dir,
            LOCAL_SNAP_REF,
            &files("{\"x\":1}"),
            "01MERGE",
            &[local.commit.clone(), head.clone()],
            &["01LOCAL".to_string(), "01INCOMING".to_string()],
            "repo@b (main)",
            "main @ bbb2222",
        )
        .expect("merge snapshot with explicit ids");

    let trailer = commit_trailers(&dir, &merged.commit);
    assert_eq!(
        trailer.parents,
        vec!["01LOCAL".to_string(), "01INCOMING".to_string()],
        "explicit ids must be used verbatim even without a parent trailer"
    );
    assert_eq!(
        merged.parent_export_ids,
        vec!["01LOCAL".to_string(), "01INCOMING".to_string()]
    );

    // A mismatch between parent commits and export ids fails closed and leaves
    // the ref untouched.
    let error = backend
        .create_merge_snapshot_commit(
            &dir,
            LOCAL_SNAP_REF,
            &files("{}"),
            "01BROKEN",
            &[merged.commit.clone(), head.clone()],
            &["01MERGE".to_string()],
            "repo@c (main)",
            "main @ ccc3333",
        )
        .unwrap_err();
    assert_eq!(error.code, "GIT_ERROR");
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), merged.commit);
}

/// NIT: `--snapshot-ref` requires `=` for a custom value, so it cannot swallow
/// a following positional; the bare form still defaults.
#[test]
fn snapshot_ref_bare_uses_default_and_custom_requires_equals() {
    let (dir, bin) = common::setup_test_project("snapshot_ref_equals");
    init_project(&dir, &bin, &["one"]);
    let bundle = dir.join("plain");
    export(&dir, &bin, &bundle, false);

    let bare = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref",
            "--dry-run",
            "--json",
        ],
    );
    assert!(bare.status.success(), "bare: {bare:?}");
    assert_eq!(
        json(&bare)["data"]["snapshot"]["ref"],
        "refs/carryctx/local"
    );

    let custom = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/custom",
            "--dry-run",
            "--json",
        ],
    );
    assert!(custom.status.success(), "custom: {custom:?}");
    assert_eq!(
        json(&custom)["data"]["snapshot"]["ref"],
        "refs/carryctx/custom"
    );

    // The space form is rejected by clap rather than treated as a positional.
    let spaced = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref",
            "refs/carryctx/custom",
            "--dry-run",
            "--json",
        ],
    );
    assert!(!spaced.status.success(), "space form must fail: {spaced:?}");
}

/// Test gap: a non-merge import rejects `--snapshot-ref` with
/// `INVALID_ARGUMENTS` (exit 2).
#[test]
fn non_merge_import_rejects_snapshot_ref() {
    let (dir, bin) = common::setup_test_project("snapshot_ref_non_merge");
    init_project(&dir, &bin, &["one"]);
    let bundle = dir.join("plain");
    export(&dir, &bin, &bundle, false);

    let bare = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--snapshot-ref",
            "--json",
        ],
    );
    assert_eq!(bare.status.code(), Some(2), "bare: {bare:?}");
    assert_eq!(json(&bare)["error"]["code"], "INVALID_ARGUMENTS");

    let replace = run(
        &dir,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "replace",
            "--snapshot-ref",
            "--json",
        ],
    );
    assert_eq!(replace.status.code(), Some(2), "replace: {replace:?}");
    assert_eq!(json(&replace)["error"]["code"], "INVALID_ARGUMENTS");
}

/// Test gap: an unflagged directory conflict session's `merge.json` only gains
/// the new `sourceRef`/`sourceCommit` keys, and they are null.
#[test]
fn unflagged_directory_conflict_session_merge_json_is_additive_only() {
    let out = scratch("unflagged_merge_json");
    let (src, bin) = common::setup_test_project("unflagged_merge_json_src");
    init_project(&src, &bin, &["base task"]);
    let base = out.join("base");
    export(&src, &bin, &base, false);

    let (tgt, _) = common::setup_test_project("unflagged_merge_json_tgt");
    let imported = run(&tgt, &bin, &["import", base.to_str().unwrap(), "--json"]);
    assert!(imported.status.success(), "import failed: {imported:?}");

    let src_task = task_id_by_title(&src, "base task");
    let tgt_task = task_id_by_title(&tgt, "base task");
    assert!(
        run(
            &src,
            &bin,
            &[
                "task",
                "edit",
                &src_task,
                "--title",
                "source edit",
                "--json"
            ],
        )
        .status
        .success()
    );
    assert!(
        run(
            &tgt,
            &bin,
            &[
                "task",
                "edit",
                &tgt_task,
                "--title",
                "target edit",
                "--json"
            ],
        )
        .status
        .success()
    );
    let src_bundle = out.join("src-bundle");
    export(&src, &bin, &src_bundle, false);
    let src_export = {
        let raw = std::fs::read_to_string(src_bundle.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
        manifest["export_id"].as_str().unwrap().to_string()
    };

    let staged = run(
        &tgt,
        &bin,
        &[
            "import",
            src_bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--strict-edits",
            "--json",
        ],
    );
    assert_eq!(staged.status.code(), Some(3), "staging: {staged:?}");

    let merge_json = session_merge_json(&tgt);
    // Pre-existing contract fields.
    assert_eq!(merge_json["status"], "conflicts_open");
    assert_eq!(merge_json["sourceExportId"], src_export);
    assert_eq!(
        merge_json["sourceDir"].as_str(),
        Some(src_bundle.to_string_lossy().as_ref())
    );
    assert!(merge_json["baseSource"].is_string());
    // New keys are additive and null when no `--from-git` ref was used.
    assert!(merge_json["sourceRef"].is_null(), "{merge_json}");
    assert!(merge_json["sourceCommit"].is_null(), "{merge_json}");
}
