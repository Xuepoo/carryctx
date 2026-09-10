//! CTX-0146 offline two-clone Git e2e (design
//! `2026-09-10-mergeable-git-managed-state.md` §3.1–§3.4, AC8).
//!
//! Disposable repositories talk to a **local bare remote on disk** over the
//! `file` protocol only: A snapshots and pushes its local-only ref with an
//! explicit refspec, B clones and fetches it, both diverge, and B merges to a
//! two-parent snapshot commit. No network transport is touched.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use carryctx_cli::adapter::git::SnapshotTrailers;

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
    let dir = std::env::temp_dir().join(format!("ctx0146_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn bare_remote(root: &Path) -> PathBuf {
    let remote = root.join("remote.git");
    git_ok(root, &["init", "--bare", remote.to_str().unwrap()]);
    remote
}

fn configure_identity(repo: &Path) {
    git_ok(repo, &["config", "user.email", "test@carryctx.dev"]);
    git_ok(repo, &["config", "user.name", "Test"]);
}

fn init_project(dir: &Path, bin: &Path, tasks: &[&str]) {
    common::init_and_agent(dir, bin);
    for title in tasks {
        let created = run(dir, bin, &["task", "create", "--title", title, "--json"]);
        assert!(created.status.success(), "task create failed: {created:?}");
    }
}

fn export_snapshot(dir: &Path, bin: &Path, out: &Path) -> serde_json::Value {
    let output = run(
        dir,
        bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            out.to_str().unwrap(),
            "--snapshot",
            "--json",
        ],
    );
    assert!(output.status.success(), "export failed: {output:?}");
    json(&output)["data"].clone()
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

fn push_state(repo: &Path, remote: &Path) {
    git_ok(
        repo,
        &[
            "push",
            "--quiet",
            remote.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:refs/heads/state"),
        ],
    );
}

fn diverge(repo: &Path, bin: &Path, remote: &Path, out: &Path, title: &str) -> (String, String) {
    let created = run(repo, bin, &["task", "create", "--title", title, "--json"]);
    assert!(created.status.success(), "task create failed: {created:?}");
    let data = export_snapshot(repo, bin, out);
    push_state(repo, remote);
    (
        data["manifest"]["export_id"].as_str().unwrap().to_string(),
        data["snapshot"]["commit"].as_str().unwrap().to_string(),
    )
}

/// AC8 / §3.3–§3.4: a fully offline clone → fetch → import → diverge → push →
/// merge → snapshot cycle over a local bare remote. Asserts the two-parent
/// merge commit (Git parents, manifest parents, trailers), the merged tree,
/// `snapshot_state`, and that the next merge resolves the ancestor base.
#[test]
fn offline_two_clone_clone_fetch_merge_snapshot_cycle() {
    let out = scratch("two_clone");
    let remote = bare_remote(&out);

    // 1. A initializes, snapshots, and publishes its local-only ref.
    let (a, bin) = common::setup_test_project("ctx0146_git_e2e_a");
    init_project(&a, &bin, &["base task"]);
    let a1 = export_snapshot(&a, &bin, &out.join("a1"));
    let a1_export = a1["manifest"]["export_id"].as_str().unwrap().to_string();
    assert_eq!(a1_export.len(), 26);
    assert!(a1["snapshot"]["commit"].is_string());
    push_state(&a, &remote);
    git_ok(&remote, &["symbolic-ref", "HEAD", "refs/heads/state"]);

    // 2. B clones the remote over the file protocol, fetches A1 into its own
    //    local-only ref, imports, diverges, and snapshots B1.
    let b = out.join("b");
    git_ok(
        &out,
        &[
            "clone",
            "--quiet",
            "--branch",
            "state",
            remote.to_str().unwrap(),
            b.to_str().unwrap(),
        ],
    );
    configure_identity(&b);
    let remote_url = git_ok(&b, &["remote", "get-url", "origin"]);
    assert!(
        remote_url.starts_with('/'),
        "e2e must use a local filesystem remote, got {remote_url}"
    );
    git_ok(
        &b,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("refs/heads/state:{LOCAL_SNAP_REF}"),
        ],
    );
    // A fresh import adopts A's project identity and brings its agents.
    let imported = run(
        &b,
        &bin,
        &["import", "--from-git", LOCAL_SNAP_REF, "--json"],
    );
    assert!(imported.status.success(), "B import failed: {imported:?}");
    let created = run(&b, &bin, &["task", "create", "--title", "b-edit", "--json"]);
    assert!(created.status.success());
    let b1 = export_snapshot(&b, &bin, &out.join("b1"));
    let b1_export = b1["manifest"]["export_id"].as_str().unwrap().to_string();
    let b1_commit = b1["snapshot"]["commit"].as_str().unwrap().to_string();

    // 3. A diverges differently and pushes A2.
    let (a2_export, a2_commit) = diverge(&a, &bin, &remote, &out.join("a2"), "a-edit");

    // 4. B fetches and merges, producing a two-parent commit.
    git_ok(
        &b,
        &[
            "fetch",
            "--quiet",
            "origin",
            "refs/heads/state:refs/remotes/origin/state",
        ],
    );
    let head_before = git_ok(&b, &["rev-parse", "HEAD"]);
    let status_before = git_ok(&b, &["status", "--porcelain"]);
    let merged = run(
        &b,
        &bin,
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

    let parents = parents_of(&b, &merge_commit);
    assert_eq!(
        parents,
        vec![b1_commit, a2_commit],
        "merge commit must have [local tip, incoming] parents"
    );
    let trailers = commit_trailers(&b, &merge_commit);
    assert_eq!(
        trailers.parents,
        vec![b1_export, a2_export.clone()],
        "trailers must list both export ids"
    );
    let committed_manifest: serde_json::Value = serde_json::from_str(&git_ok(
        &b,
        &["cat-file", "-p", &format!("{merge_commit}:manifest.json")],
    ))
    .unwrap();
    assert_eq!(
        committed_manifest["parents"],
        serde_json::json!([trailers.parents[0], a2_export.clone()])
    );
    let committed_tasks = git_ok(
        &b,
        &["cat-file", "-p", &format!("{merge_commit}:tasks.jsonl")],
    );
    assert!(committed_tasks.contains("b-edit"), "local edit missing");
    assert!(committed_tasks.contains("a-edit"), "incoming edit missing");

    assert_eq!(
        state_value(&b, "last_snapshot_commit").as_deref(),
        Some(merge_commit.as_str())
    );
    assert_eq!(
        state_value(&b, "last_export_id").as_deref(),
        trailers.export_id.as_deref()
    );
    assert_eq!(git_ok(&b, &["rev-parse", LOCAL_SNAP_REF]), merge_commit);
    assert_eq!(git_ok(&b, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git_ok(&b, &["status", "--porcelain"]), status_before);

    // 5. A diverges again; B's next merge resolves the common ancestor A2
    //    through the merge commit's DAG.
    let (a3_export, _a3_commit) = diverge(&a, &bin, &remote, &out.join("a3"), "a2-edit");
    git_ok(
        &b,
        &[
            "fetch",
            "--quiet",
            "origin",
            "refs/heads/state:refs/remotes/origin/state",
        ],
    );
    let second = run(
        &b,
        &bin,
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
    let second_data = json(&second)["data"].clone();
    assert_eq!(second_data["baseSource"], "ancestor");
    assert_eq!(
        second_data["baseExportId"].as_str(),
        Some(a2_export.as_str())
    );
    assert_eq!(second_data["degraded"], false);
    assert_ne!(a3_export, a2_export);
}

/// Guards the offline contract directly: the fixture remote is a filesystem
/// path, never a network URL.
#[test]
fn git_e2e_remote_is_a_local_bare_path() {
    let out = scratch("offline_probe");
    let remote = bare_remote(&out);
    assert!(remote.is_dir(), "remote must be a local bare repo");
    let remote_str = remote.to_string_lossy().into_owned();
    assert!(
        !remote_str.contains("://") && remote_str.starts_with('/'),
        "fixture remote must be a filesystem path: {remote_str}"
    );
}
