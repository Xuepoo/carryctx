//! CTX-0144 integration tests: the local-only snapshot ref (commit-per-
//! snapshot export, import-from-git) and snapshot-ref base resolution
//! (design `2026-09-10-mergeable-git-managed-state.md` §3.1–§3.6, AC8).
//!
//! DEC-0052 / issue #138: the unredacted snapshot ref is local-only and must
//! never be the public redacted publication ref `refs/heads/carryctx-snapshots`
//! (or any other `refs/heads/*`), and CarryCtx never pushes it. The guard test
//! [`unredacted_snapshots_never_use_the_public_ref_or_push`] enforces that.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use carryctx_cli::adapter::git::{GitBackend, SnapshotTrailers, VcsBackend};

/// Default local-only unredacted snapshot ref (never pushed by the binary).
const LOCAL_SNAP_REF: &str = "refs/carryctx/local";
/// Public redacted publication ref reserved for the publication flow
/// (DEC-0052, issue #138); unredacted `--snapshot` export must refuse it.
const PUBLIC_SNAP_REF: &str = "refs/heads/carryctx-snapshots";

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

fn init_project(dir: &Path, bin: &Path, tasks: &[&str]) {
    common::init_and_agent(dir, bin);
    for title in tasks {
        let created = run(dir, bin, &["task", "create", "--title", title, "--json"]);
        assert!(created.status.success(), "task create failed: {created:?}");
    }
}

/// Export `out` and, with `snapshot`, commit it to the snapshot ref.
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

fn pack_manifest(out: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap()
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

fn event_count(repo: &Path, event_type: &str) -> i64 {
    let db = repo.join(".git/carryctx/state.sqlite");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = ?1",
        [event_type],
        |row| row.get(0),
    )
    .unwrap()
}

fn ref_commits(repo: &Path, git_ref: &str) -> Vec<String> {
    git_ok(repo, &["rev-list", git_ref])
        .lines()
        .map(str::to_string)
        .collect()
}

/// Fetch one repo's snapshot branch into another as a remote-tracking ref,
/// using local Git transport only (no network).
fn fetch_snapshot(source: &Path, dest: &Path) {
    git_ok(
        dest,
        &[
            "fetch",
            "--quiet",
            source.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:refs/remotes/origin/carryctx-local"),
        ],
    );
}

fn empty_repo(name: &str) -> (PathBuf, PathBuf) {
    common::setup_test_project(name)
}

#[test]
fn two_snapshots_form_a_parent_chain_with_trailers() {
    let (dir, bin) = empty_repo("snapshot_chain");
    init_project(&dir, &bin, &["first"]);

    let first_pack = dir.join("pack-one");
    let first = export(&dir, &bin, &first_pack, true);
    let first_export_id = first["manifest"]["export_id"].as_str().unwrap().to_string();
    let first_commit = first["snapshot"]["commit"].as_str().unwrap().to_string();

    let created = run(
        &dir,
        &bin,
        &["task", "create", "--title", "second", "--json"],
    );
    assert!(created.status.success());

    let second_pack = dir.join("pack-two");
    let second = export(&dir, &bin, &second_pack, true);
    let second_export_id = second["manifest"]["export_id"]
        .as_str()
        .unwrap()
        .to_string();
    let second_commit = second["snapshot"]["commit"].as_str().unwrap().to_string();

    assert_eq!(second["snapshot"]["previousCommit"], first_commit);
    assert_eq!(second["snapshot"]["parentExportIds"][0], first_export_id);

    // Self-describing bundle: the second manifest names the first export.
    assert_eq!(pack_manifest(&first_pack)["parents"], serde_json::json!([]));
    assert_eq!(
        pack_manifest(&second_pack)["parents"],
        serde_json::json!([first_export_id])
    );

    // Two commits, newest first, linked by Git parenthood.
    let commits = ref_commits(&dir, LOCAL_SNAP_REF);
    assert_eq!(commits, vec![second_commit.clone(), first_commit.clone()]);
    let parents = git_ok(&dir, &["rev-list", "--parents", "-n", "1", LOCAL_SNAP_REF]);
    assert_eq!(parents.split_whitespace().count(), 2, "one Git parent");

    // Trailers reconstruct the export-id DAG without any index file.
    let message = git_ok(&dir, &["show", "-s", "--format=%B", LOCAL_SNAP_REF]);
    let trailers = SnapshotTrailers::parse(&message);
    assert_eq!(
        trailers.export_id.as_deref(),
        Some(second_export_id.as_str())
    );
    assert_eq!(trailers.parents, vec![first_export_id]);
    assert!(trailers.source.is_some());
}

#[test]
fn first_snapshot_creates_the_ref_without_a_parent() {
    let (dir, bin) = empty_repo("snapshot_first");
    init_project(&dir, &bin, &["only"]);

    let data = export(&dir, &bin, &dir.join("pack"), true);
    assert!(data["snapshot"]["previousCommit"].is_null());
    assert_eq!(data["snapshot"]["parentExportIds"], serde_json::json!([]));

    let commits = ref_commits(&dir, LOCAL_SNAP_REF);
    assert_eq!(commits.len(), 1);
    let parents = git_ok(&dir, &["rev-list", "--parents", "-n", "1", LOCAL_SNAP_REF]);
    assert_eq!(parents.split_whitespace().count(), 1, "root commit");
}

#[test]
fn snapshot_does_not_mutate_index_worktree_or_head() {
    let (dir, bin) = empty_repo("snapshot_no_mutation");
    init_project(&dir, &bin, &["task"]);
    // Commit CarryCtx's own files so a clean baseline exists.
    git_ok(&dir, &["add", "-A"]);
    git_ok(
        &dir,
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t.dev",
            "commit",
            "-q",
            "-m",
            "carryctx setup",
        ],
    );

    let head_before = git_ok(&dir, &["rev-parse", "HEAD"]);
    let status_before = git_ok(&dir, &["status", "--porcelain"]);
    assert!(status_before.is_empty(), "baseline must be clean");

    // Export outside the worktree so the bundle itself cannot dirty status.
    let out = std::env::temp_dir().join(format!("ctx0144_nomut_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let data = export(&dir, &bin, &out, true);
    assert!(data["snapshot"]["commit"].is_string());

    assert_eq!(git_ok(&dir, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git_ok(&dir, &["status", "--porcelain"]), status_before);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn concurrent_ref_move_fails_closed() {
    let (dir, _bin) = empty_repo("snapshot_cas");
    let backend = GitBackend::new();
    let files = |content: &str| vec![("manifest.json".to_string(), content.as_bytes().to_vec())];

    let first = backend
        .create_snapshot_commit(
            &dir,
            LOCAL_SNAP_REF,
            &files("{}"),
            "01A",
            &[],
            "repo@abc (main)",
            "main @ abc1234",
        )
        .expect("first snapshot");
    assert!(first.previous.is_none());

    // Simulate another worktree moving the ref between our read and our write.
    let head = git_ok(&dir, &["rev-parse", "HEAD"]);
    git_ok(&dir, &["update-ref", LOCAL_SNAP_REF, &head]);

    // The caller still believes the ref is at `first`; CAS must refuse.
    let error = backend
        .create_snapshot_commit(
            &dir,
            LOCAL_SNAP_REF,
            &files("{\"x\":1}"),
            "01B",
            std::slice::from_ref(&first.commit),
            "repo@def (main)",
            "main @ def5678",
        )
        .unwrap_err();
    assert_eq!(error.code, "GIT_ERROR");
    // The ref is untouched by the failed attempt.
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), head);
}

#[test]
fn import_from_git_matches_importing_the_directory_bare_fresh() {
    let (source, bin) = empty_repo("snapshot_import_src");
    init_project(&source, &bin, &["one", "two"]);
    let pack = source.join("pack");
    export(&source, &bin, &pack, true);

    // From the ref.
    let (git_target, _) = empty_repo("snapshot_import_git");
    fetch_snapshot(&source, &git_target);
    let from_git = run(
        &git_target,
        &bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/carryctx-local",
            "--json",
        ],
    );
    assert!(
        from_git.status.success(),
        "from-git import failed: {from_git:?}"
    );

    // From the directory.
    let (dir_target, _) = empty_repo("snapshot_import_dir");
    let from_dir = run(
        &dir_target,
        &bin,
        &["import", pack.to_str().unwrap(), "--json"],
    );
    assert!(from_dir.status.success(), "dir import failed: {from_dir:?}");

    let git_counts = json(&from_git)["data"]["counts"].clone();
    let dir_counts = json(&from_dir)["data"]["counts"].clone();
    assert_eq!(git_counts, dir_counts);
    assert_eq!(event_count(&git_target, "project.imported"), 1);
    assert_eq!(event_count(&dir_target, "project.imported"), 1);

    for target in [&git_target, &dir_target] {
        let doctor = run(target, &bin, &["doctor", "--json"]);
        assert!(doctor.status.success(), "doctor failed: {doctor:?}");
    }
}

#[test]
fn import_from_git_merge_matches_directory_merge() {
    let (source, bin) = empty_repo("snapshot_merge_src");
    init_project(&source, &bin, &["base task"]);
    let base_pack = source.join("base");
    export(&source, &bin, &base_pack, true);
    let created = run(
        &source,
        &bin,
        &["task", "create", "--title", "tip task", "--json"],
    );
    assert!(created.status.success());
    let tip_pack = source.join("tip");
    export(&source, &bin, &tip_pack, true);

    // Target A: bare-import the ref tip, diverge locally, then merge from ref.
    let (git_target, _) = empty_repo("snapshot_merge_git");
    fetch_snapshot(&source, &git_target);
    let imported = run(
        &git_target,
        &bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/carryctx-local",
            "--json",
        ],
    );
    assert!(imported.status.success(), "import failed: {imported:?}");
    let created = run(
        &git_target,
        &bin,
        &["task", "create", "--title", "local git", "--json"],
    );
    assert!(created.status.success());
    let merged = run(
        &git_target,
        &bin,
        &[
            "import",
            "--from-git",
            "refs/remotes/origin/carryctx-local",
            "--mode",
            "merge",
            "--base",
            base_pack.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(merged.status.success(), "merge from-git failed: {merged:?}");
    let merged = json(&merged);
    assert_eq!(merged["data"]["mode"], "merge");
    assert_eq!(merged["data"]["baseSource"], "explicit");

    // Target B: identical flow from the directory.
    let (dir_target, _) = empty_repo("snapshot_merge_dir");
    let imported = run(
        &dir_target,
        &bin,
        &["import", tip_pack.to_str().unwrap(), "--json"],
    );
    assert!(imported.status.success());
    let created = run(
        &dir_target,
        &bin,
        &["task", "create", "--title", "local dir", "--json"],
    );
    assert!(created.status.success());
    let merged_dir = run(
        &dir_target,
        &bin,
        &[
            "import",
            tip_pack.to_str().unwrap(),
            "--mode",
            "merge",
            "--base",
            base_pack.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        merged_dir.status.success(),
        "merge from dir failed: {merged_dir:?}"
    );
    let merged_dir = json(&merged_dir);

    assert_eq!(
        merged["data"]["counts"]["tasks"],
        merged_dir["data"]["counts"]["tasks"]
    );
    assert_eq!(
        merged["data"]["baseSource"],
        merged_dir["data"]["baseSource"]
    );
    assert_eq!(event_count(&git_target, "project.merged"), 1);
    assert_eq!(event_count(&dir_target, "project.merged"), 1);
}

#[test]
fn snapshot_ref_history_resolves_the_merge_base() {
    let (dir, bin) = empty_repo("snapshot_base_history");
    init_project(&dir, &bin, &["one"]);
    let first_pack = dir.join("p1");
    let first = export(&dir, &bin, &first_pack, true);
    let first_export_id = first["manifest"]["export_id"].as_str().unwrap().to_string();

    let created = run(&dir, &bin, &["task", "create", "--title", "two", "--json"]);
    assert!(created.status.success());
    export(&dir, &bin, &dir.join("p2"), true);

    // Rewind the clone's recorded position to the first snapshot, so the ref
    // history (not a local cache dir) is what resolves the ancestor.
    let db = dir.join(".git/carryctx/state.sqlite");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute(
        "UPDATE snapshot_state SET value = ?1 WHERE key = 'last_export_id'",
        [&first_export_id],
    )
    .unwrap();
    drop(conn);

    let merged = run(
        &dir,
        &bin,
        &[
            "import",
            "--from-git",
            LOCAL_SNAP_REF,
            "--mode",
            "merge",
            "--require-base",
            "--json",
        ],
    );
    assert!(merged.status.success(), "ancestor merge failed: {merged:?}");
    let merged = json(&merged);
    assert_eq!(merged["data"]["baseSource"], "ancestor");
    assert_eq!(merged["data"]["base"], first_export_id);
    assert_eq!(merged["data"]["degraded"], false);
}

#[test]
fn malformed_source_selection_and_missing_ref_exit_codes() {
    let (dir, bin) = empty_repo("snapshot_args");
    init_project(&dir, &bin, &["t"]);
    let pack = dir.join("pack");
    export(&dir, &bin, &pack, false);

    // Both sources.
    let both = run(
        &dir,
        &bin,
        &[
            "import",
            pack.to_str().unwrap(),
            "--from-git",
            LOCAL_SNAP_REF,
            "--json",
        ],
    );
    assert_eq!(both.status.code(), Some(2));
    assert_eq!(json(&both)["error"]["code"], "INVALID_ARGUMENTS");

    // Neither source.
    let neither = run(&dir, &bin, &["import", "--json"]);
    assert_eq!(neither.status.code(), Some(2));
    assert_eq!(json(&neither)["error"]["code"], "INVALID_ARGUMENTS");

    // Missing ref.
    let missing = run(
        &dir,
        &bin,
        &[
            "import",
            "--from-git",
            "refs/heads/does-not-exist",
            "--json",
        ],
    );
    assert_eq!(missing.status.code(), Some(4));
    assert_eq!(json(&missing)["error"]["code"], "GIT_ERROR");

    // Not a Git repository.
    let outside = std::env::temp_dir().join(format!("ctx0144_nogit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    let no_repo = run(
        &outside,
        &bin,
        &["import", "--from-git", LOCAL_SNAP_REF, "--json"],
    );
    assert_eq!(no_repo.status.code(), Some(4));
    assert_eq!(json(&no_repo)["error"]["code"], "GIT_ERROR");
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn snapshot_dry_run_writes_nothing() {
    let (dir, bin) = empty_repo("snapshot_dry_run");
    init_project(&dir, &bin, &["one"]);
    let pack = dir.join("pack");
    export(&dir, &bin, &pack, true);

    let ref_before = git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]);
    let state_before = state_value(&dir, "last_snapshot_commit");
    let out = dir.join("dry-pack");

    let planned = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            out.to_str().unwrap(),
            "--snapshot",
            "--dry-run",
            "--json",
        ],
    );
    assert!(planned.status.success(), "dry-run failed: {planned:?}");
    let body = json(&planned);
    assert!(body["data"]["snapshot"]["wouldCommit"].as_bool().unwrap());
    assert!(!out.exists(), "dry-run must not create the bundle dir");
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), ref_before);
    assert_eq!(state_value(&dir, "last_snapshot_commit"), state_before);
}

#[test]
fn snapshot_state_records_export_and_commit() {
    let (dir, bin) = empty_repo("snapshot_state");
    init_project(&dir, &bin, &["one"]);
    let pack = dir.join("pack");
    let data = export(&dir, &bin, &pack, true);

    let export_id = data["manifest"]["export_id"].as_str().unwrap();
    let commit = data["snapshot"]["commit"].as_str().unwrap();
    assert_eq!(
        state_value(&dir, "last_export_id").as_deref(),
        Some(export_id)
    );
    assert_eq!(
        state_value(&dir, "last_snapshot_commit").as_deref(),
        Some(commit)
    );
    assert_eq!(git_ok(&dir, &["rev-parse", LOCAL_SNAP_REF]), commit);

    // A plain export must not write a ref or move the recorded position.
    let plain = export(&dir, &bin, &dir.join("plain"), false);
    assert!(plain["snapshot"].is_null());
    assert_eq!(
        state_value(&dir, "last_snapshot_commit").as_deref(),
        Some(commit)
    );
}

#[test]
fn snapshot_is_fully_offline_and_touches_only_local_refs() {
    let (dir, bin) = empty_repo("snapshot_offline");
    init_project(&dir, &bin, &["one"]);
    assert!(
        git_ok(&dir, &["remote"]).is_empty(),
        "no remotes configured"
    );

    let data = export(&dir, &bin, &dir.join("pack"), true);
    assert_eq!(data["snapshot"]["ref"], LOCAL_SNAP_REF);

    // No remote-tracking refs were created or updated.
    let remote_refs = git_ok(&dir, &["for-each-ref", "refs/remotes"]);
    assert!(
        remote_refs.is_empty(),
        "unexpected remote refs: {remote_refs}"
    );

    // The only refs are the code branch and the local-only snapshot ref; the
    // public redacted publication ref is never created by an unredacted export.
    let refs = git_ok(&dir, &["for-each-ref", "--format=%(refname)"]);
    for git_ref in refs.lines() {
        assert!(
            git_ref == LOCAL_SNAP_REF || git_ref == "refs/heads/main",
            "unexpected ref after offline export: {git_ref}"
        );
    }
    assert!(
        !git_out(&dir, &["rev-parse", "--verify", "--quiet", PUBLIC_SNAP_REF])
            .status
            .success()
    );
}

#[test]
fn subject_uses_branch_at_sha_without_doubled_parens() {
    let (dir, bin) = empty_repo("snapshot_subject");
    init_project(&dir, &bin, &["one"]);
    let data = export(&dir, &bin, &dir.join("pack"), true);
    let export_id = data["manifest"]["export_id"].as_str().unwrap();
    let head = git_ok(&dir, &["rev-parse", "HEAD"]);
    let short = &head[..7];

    let subject = git_ok(&dir, &["log", "-1", "--format=%s", LOCAL_SNAP_REF]);
    assert_eq!(
        subject,
        format!("chore(ctxpack): snapshot {export_id} (main @ {short})")
    );
    assert!(!subject.ends_with("))"), "subject kept doubled parentheses");
}

#[test]
fn snapshot_ref_namespace_is_validated() {
    let (dir, bin) = empty_repo("snapshot_ref_guard");
    init_project(&dir, &bin, &["one"]);
    let pack = dir.join("pack");

    // A bare name is not a full ref name.
    let bare = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            pack.to_str().unwrap(),
            "--snapshot",
            "--snapshot-ref",
            "main",
            "--json",
        ],
    );
    assert_eq!(bare.status.code(), Some(2), "bare name: {bare:?}");
    assert_eq!(json(&bare)["error"]["code"], "INVALID_ARGUMENTS");
    assert!(!pack.exists(), "validation must run before writing");

    // A normal branch (here the checked-out branch) must never be clobbered.
    let normal = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            pack.to_str().unwrap(),
            "--snapshot",
            "--snapshot-ref",
            "refs/heads/main",
            "--json",
        ],
    );
    assert_eq!(normal.status.code(), Some(2), "normal branch: {normal:?}");
    assert_eq!(json(&normal)["error"]["code"], "INVALID_ARGUMENTS");

    // Any other `refs/heads/*` branch is refused too: a default `git push`
    // could publish unredacted state (DEC-0052).
    let heads = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            pack.to_str().unwrap(),
            "--snapshot",
            "--snapshot-ref",
            "refs/heads/carryctx-custom",
            "--json",
        ],
    );
    assert_eq!(heads.status.code(), Some(2), "carryctx-* branch: {heads:?}");
    assert_eq!(json(&heads)["error"]["code"], "INVALID_ARGUMENTS");

    // A local-only override in the dedicated namespace is accepted.
    let ok = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            pack.to_str().unwrap(),
            "--snapshot",
            "--snapshot-ref",
            "refs/carryctx/custom",
            "--json",
        ],
    );
    assert!(ok.status.success(), "valid override failed: {ok:?}");
    assert_eq!(json(&ok)["data"]["snapshot"]["ref"], "refs/carryctx/custom");
    assert!(!git_ok(&dir, &["rev-parse", "refs/carryctx/custom"]).is_empty());
}

/// DEC-0052 / issue #138 guard: unredacted snapshots never use the public
/// redacted publication ref and CarryCtx never pushes the local-only ref.
#[test]
fn unredacted_snapshots_never_use_the_public_ref_or_push() {
    let (dir, bin) = empty_repo("snapshot_public_guard");
    init_project(&dir, &bin, &["one"]);

    // A bare origin with the code branch pushed, so a plain user `git push`
    // has a real destination and would move anything under `refs/heads/*`.
    let origin = dir.join("origin.git");
    git_ok(&dir, &["init", "--bare", origin.to_str().unwrap()]);
    git_ok(&dir, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git_ok(&dir, &["push", "--quiet", "origin", "main"]);

    // The default export ref is the local-only namespace, not the public ref.
    let pack = dir.join("pack");
    let data = export(&dir, &bin, &pack, true);
    assert_eq!(data["snapshot"]["ref"], LOCAL_SNAP_REF);
    assert!(
        git_out(&dir, &["rev-parse", "--verify", "--quiet", LOCAL_SNAP_REF])
            .status
            .success()
    );
    assert!(
        !git_out(&dir, &["rev-parse", "--verify", "--quiet", PUBLIC_SNAP_REF])
            .status
            .success(),
        "unredacted export created the public redacted ref"
    );

    // The public ref is refused outright for an unredacted export, before any
    // file or ref write.
    let public_pack = dir.join("public-pack");
    let refused = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            public_pack.to_str().unwrap(),
            "--snapshot",
            "--snapshot-ref",
            PUBLIC_SNAP_REF,
            "--json",
        ],
    );
    assert_eq!(refused.status.code(), Some(2), "public ref: {refused:?}");
    assert_eq!(json(&refused)["error"]["code"], "INVALID_ARGUMENTS");
    assert!(!public_pack.exists(), "refusal must precede any write");

    // Local-only means not moved by default user transport: even `push --all`
    // only touches `refs/heads/*`, so nothing carryctx-shaped reaches origin.
    git_ok(&dir, &["push", "--quiet", "--all", "origin"]);
    let advertised = git_ok(&dir, &["ls-remote", origin.to_str().unwrap()]);
    assert!(
        !advertised.contains("carryctx"),
        "local-only snapshot ref leaked to origin: {advertised}"
    );
    assert!(!advertised.contains("refs/carryctx"));
}

#[test]
fn import_rejects_leading_dash_revision_values() {
    let (dir, bin) = empty_repo("snapshot_dash_args");
    init_project(&dir, &bin, &["one"]);
    let pack = dir.join("pack");
    export(&dir, &bin, &pack, false);

    let from_git = run(&dir, &bin, &["import", "--from-git=-evil", "--json"]);
    assert_eq!(from_git.status.code(), Some(2), "from-git: {from_git:?}");
    assert_eq!(json(&from_git)["error"]["code"], "INVALID_ARGUMENTS");

    let base = run(
        &dir,
        &bin,
        &[
            "import",
            pack.to_str().unwrap(),
            "--mode",
            "merge",
            "--base=-evil",
            "--json",
        ],
    );
    assert_eq!(base.status.code(), Some(2), "base: {base:?}");
    assert_eq!(json(&base)["error"]["code"], "INVALID_ARGUMENTS");
}
