//! CTX-0155 integration tests: the redacted publication writer on the
//! distinct public ref `refs/heads/carryctx-snapshots` (DEC-0052, issue #138;
//! design `2026-09-10-mergeable-git-managed-state.md` §3.1/§3.6).
//!
//! Guards enforced here:
//! - the `***REDACTED***` pass runs on snapshot rows before the commit, and
//!   the public ref tree never carries the raw secret values;
//! - `refs/heads/carryctx-snapshots` is written only by `--publication` and is
//!   never a target an unredacted export can reach;
//! - `refs/carryctx/local` stays local-only (a bare origin never receives it),
//!   and CarryCtx never pushes any ref itself;
//! - the public ref uses the same compare-and-swap discipline as CTX-0144.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Default local-only unredacted snapshot ref (never pushed by the binary).
const LOCAL_SNAP_REF: &str = "refs/carryctx/local";
/// Public redacted publication ref (DEC-0052, issue #138).
const PUBLIC_SNAP_REF: &str = "refs/heads/carryctx-snapshots";
/// Secret-shaped values seeded into the local database.
const ENV_SECRET: &str = "sk-live-abcdefghijklmnopqrstuvwxyz123456";
const ASSIGNMENT: &str = "OPENAI_API_KEY=sk-live-abcdefghijklmnopqrstuvwxyz123456";
const PAT_SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz012345";
const GH_ASSIGNMENT: &str = "GH_PAT=ghp_abcdefghijklmnopqrstuvwxyz012345";
const RUN_SECRET: &str = "Abcdeg0123456789Abcdef0123456789Abcdef01";

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

/// Run a redacted publication export into `out`.
fn publication(dir: &Path, bin: &Path, out: &Path) -> serde_json::Value {
    let output = run(
        dir,
        bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            out.to_str().unwrap(),
            "--publication",
            "--json",
        ],
    );
    assert!(output.status.success(), "publication failed: {output:?}");
    json(&output)["data"].clone()
}

fn pack_manifest(out: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap()
}

/// Read one file from the public ref's tip tree via Git plumbing.
fn public_file(repo: &Path, name: &str) -> String {
    git_ok(repo, &["show", &format!("{PUBLIC_SNAP_REF}:{name}")])
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

fn empty_repo(name: &str) -> (PathBuf, PathBuf) {
    common::setup_test_project(name)
}

/// Core guard (DEC-0052 / issue #138): the public ref receives a redacted
/// artifact; the local-only ref and `snapshot_state` are untouched.
#[test]
fn publication_commits_redacted_bundle_to_the_distinct_public_ref() {
    let (dir, bin) = empty_repo("publication_guard");
    common::init_and_agent(&dir, &bin);
    let created = run(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "deploy",
            "--description",
            ASSIGNMENT,
            "--json",
        ],
    );
    assert!(created.status.success(), "task create failed: {created:?}");

    let out = dir.join("pub");
    let data = publication(&dir, &bin, &out);

    assert_eq!(data["publication"]["ref"], PUBLIC_SNAP_REF);
    assert_eq!(data["publication"]["redacted"], true);
    assert!(data["publication"]["commit"].as_str().is_some());
    assert!(data["publication"]["redactions"].as_u64().unwrap() >= 1);
    assert!(
        data["snapshot"].is_null(),
        "publication must not fake a local snapshot"
    );

    // The on-disk artifact and the commit tree are the redacted publication.
    let manifest = pack_manifest(&out);
    assert_eq!(manifest["redacted"], true);
    for table in [
        "tasks.jsonl",
        "progress_items.jsonl",
        "events.jsonl",
        "project.json",
    ] {
        for text in [
            fs::read_to_string(out.join(table)).unwrap_or_default(),
            public_file(&dir, table),
        ] {
            assert!(
                !text.contains(ENV_SECRET),
                "raw secret leaked in {table}: {text}"
            );
        }
    }
    let committed_manifest: serde_json::Value =
        serde_json::from_str(&public_file(&dir, "manifest.json")).unwrap();
    assert_eq!(committed_manifest["redacted"], true);
    assert!(
        public_file(&dir, "tasks.jsonl").contains("***REDACTED***"),
        "redaction marker missing from the public tree"
    );

    // Distinct refs: the local-only unredacted ref was never created and
    // `snapshot_state` still tracks no snapshot.
    assert!(
        !git_out(&dir, &["rev-parse", "--verify", "--quiet", LOCAL_SNAP_REF])
            .status
            .success(),
        "publication touched the local-only ref"
    );
    assert_eq!(state_value(&dir, "last_export_id"), None);
    assert_eq!(state_value(&dir, "last_snapshot_commit"), None);
}

/// Every table in the public tree is scanned: no seeded secret survives, and
/// the local unredacted snapshot keeps the originals (DB untouched).
#[test]
fn public_ref_never_carries_unredacted_rows_across_tables() {
    let (dir, bin) = empty_repo("publication_multitable");
    common::init_and_agent(&dir, &bin);
    let created = run(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "ship",
            "--description",
            ENV_SECRET,
            "--json",
        ],
    );
    assert!(created.status.success(), "task create failed: {created:?}");
    let task_id = json(&created)["data"]["id"].as_str().unwrap().to_string();
    let noted = run(
        &dir,
        &bin,
        &[
            "progress",
            "note",
            GH_ASSIGNMENT,
            "--task",
            &task_id,
            "--json",
        ],
    );
    assert!(noted.status.success(), "progress note failed: {noted:?}");
    let decided = run(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "rotate",
            "--decision",
            RUN_SECRET,
            "--task",
            &task_id,
            "--json",
        ],
    );
    assert!(decided.status.success(), "decision add failed: {decided:?}");

    let out = dir.join("pub");
    publication(&dir, &bin, &out);

    let files = git_ok(&dir, &["ls-tree", "-r", "--name-only", PUBLIC_SNAP_REF]);
    let mut jsonl_seen = 0;
    for name in files.lines().filter(|name| name.ends_with(".jsonl")) {
        jsonl_seen += 1;
        let text = public_file(&dir, name);
        for secret in [ENV_SECRET, PAT_SECRET, RUN_SECRET] {
            assert!(
                !text.contains(secret),
                "raw secret {secret} leaked into {name}: {text}"
            );
        }
    }
    assert!(
        jsonl_seen >= 10,
        "expected a full bundle, saw {jsonl_seen} files"
    );

    // The project row is published too; it must not carry a raw secret and
    // must keep the identity field intact.
    let project = public_file(&dir, "project.json");
    for secret in [ENV_SECRET, PAT_SECRET, RUN_SECRET] {
        assert!(
            !project.contains(secret),
            "raw secret leaked into project.json: {project}"
        );
    }
    let project_value: serde_json::Value = serde_json::from_str(&project).unwrap();
    assert_eq!(project_value["id"].as_str().unwrap().len(), 26);

    // The local-only unredacted artifact still carries the originals; the
    // local database was never rewritten by the redaction pass.
    let local = run(
        &dir,
        &bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            dir.join("local").to_str().unwrap(),
            "--snapshot",
            "--json",
        ],
    );
    assert!(local.status.success(), "local snapshot failed: {local:?}");
    assert_eq!(json(&local)["data"]["snapshot"]["ref"], LOCAL_SNAP_REF);
    let local_tasks = git_ok(&dir, &["show", &format!("{LOCAL_SNAP_REF}:tasks.jsonl")]);
    assert!(
        local_tasks.contains(ENV_SECRET),
        "local unredacted snapshot must keep the original value"
    );
}

/// The publication DAG lives on the public ref: parents chain to prior
/// publications, and a concurrent ref move fails closed (CAS).
#[test]
fn publication_chain_and_concurrent_move_fail_closed() {
    let (dir, bin) = empty_repo("publication_chain");
    init_project(&dir, &bin, &["one"]);

    let first = publication(&dir, &bin, &dir.join("pub1"));
    let second = publication(&dir, &bin, &dir.join("pub2"));
    assert_eq!(
        second["manifest"]["parents"][0], first["manifest"]["export_id"],
        "second publication must chain to the first on the public ref"
    );
    assert_eq!(
        second["publication"]["previousCommit"],
        first["publication"]["commit"]
    );
    let commits = git_ok(&dir, &["rev-list", PUBLIC_SNAP_REF]);
    assert_eq!(commits.lines().count(), 2);

    // Simulate another clone moving the public ref between read and write.
    let head = git_ok(&dir, &["rev-parse", "HEAD"]);
    git_ok(&dir, &["update-ref", PUBLIC_SNAP_REF, &head]);
    let raced = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            dir.join("pub3").to_str().unwrap(),
            "--publication",
            "--json",
        ],
    );
    assert_eq!(raced.status.code(), Some(4), "raced publication: {raced:?}");
    assert_eq!(json(&raced)["error"]["code"], "GIT_ERROR");
    assert_eq!(git_ok(&dir, &["rev-parse", PUBLIC_SNAP_REF]), head);
}

/// CarryCtx never pushes: publication leaves a configured bare origin empty,
/// the local-only ref cannot be moved by `git push --all`, and an explicit
/// user push of the public ref transports only redacted rows.
#[test]
fn publication_is_offline_and_only_the_user_pushes() {
    let (dir, bin) = empty_repo("publication_offline");
    common::init_and_agent(&dir, &bin);
    let created = run(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "deploy",
            "--description",
            ASSIGNMENT,
            "--json",
        ],
    );
    assert!(created.status.success(), "task create failed: {created:?}");

    let origin = dir.join("origin.git");
    git_ok(&dir, &["init", "--bare", origin.to_str().unwrap()]);
    git_ok(&dir, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git_ok(&dir, &["push", "--quiet", "origin", "main"]);

    publication(&dir, &bin, &dir.join("pub"));

    // The binary wrote local objects only; origin still has just the branch.
    let advertised = git_ok(&dir, &["ls-remote", origin.to_str().unwrap()]);
    assert!(
        !advertised.contains("carryctx"),
        "publication pushed to origin: {advertised}"
    );

    // `git push --all` moves only refs/heads/*; the local unredacted ref is
    // outside that namespace and never leaves the machine.
    git_ok(&dir, &["push", "--quiet", "--all", "origin"]);
    let after_all = git_ok(&dir, &["ls-remote", origin.to_str().unwrap()]);
    assert!(
        !after_all.contains("refs/carryctx"),
        "local-only ref leaked via push --all: {after_all}"
    );

    // User transport of the public branch carries the redacted tree.
    git_ok(
        &dir,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{PUBLIC_SNAP_REF}:refs/heads/carryctx-snapshots"),
        ],
    );
    let remote_readme = git_ok(
        &dir,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("{PUBLIC_SNAP_REF}:refs/remotes/origin/carryctx-snapshots"),
        ],
    );
    let _ = remote_readme;
    let pushed = git_ok(
        &dir,
        &["show", "refs/remotes/origin/carryctx-snapshots:tasks.jsonl"],
    );
    assert!(
        !pushed.contains(ENV_SECRET),
        "pushed tree leaked the secret"
    );
    // The local unredacted ref still has no remote-tracking counterpart.
    let remotes = git_ok(&dir, &["for-each-ref", "refs/remotes"]);
    assert!(
        !remotes.contains("carryctx-local"),
        "local-only ref gained a remote: {remotes}"
    );
}

#[test]
fn publication_dry_run_writes_nothing() {
    let (dir, bin) = empty_repo("publication_dry_run");
    common::init_and_agent(&dir, &bin);
    let created = run(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "deploy",
            "--description",
            ASSIGNMENT,
            "--json",
        ],
    );
    assert!(created.status.success(), "task create failed: {created:?}");

    let exports_before = event_count(&dir, "project.exported");
    let out = dir.join("pub");
    let planned = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            out.to_str().unwrap(),
            "--publication",
            "--dry-run",
            "--json",
        ],
    );
    assert!(planned.status.success(), "dry-run failed: {planned:?}");
    let body = json(&planned);
    assert_eq!(body["data"]["publication"]["ref"], PUBLIC_SNAP_REF);
    assert_eq!(body["data"]["publication"]["redacted"], true);
    assert!(
        body["data"]["publication"]["wouldCommit"]
            .as_bool()
            .unwrap()
    );
    assert!(body["data"]["operation"]["applied"] == false);

    assert!(!out.exists(), "dry-run must not create the bundle dir");
    assert!(
        !git_out(&dir, &["rev-parse", "--verify", "--quiet", PUBLIC_SNAP_REF])
            .status
            .success(),
        "dry-run wrote the public ref"
    );
    assert_eq!(state_value(&dir, "last_export_id"), None);
    assert_eq!(event_count(&dir, "project.exported"), exports_before);
}

#[test]
fn publication_ref_is_not_redirectable() {
    let (dir, bin) = empty_repo("publication_redirect");
    init_project(&dir, &bin, &["one"]);

    // The publication target is fixed; a `--snapshot-ref` redirect is refused.
    let redirected = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            dir.join("pub").to_str().unwrap(),
            "--publication",
            "--snapshot-ref",
            "refs/heads/carryctx-custom",
            "--json",
        ],
    );
    assert_eq!(redirected.status.code(), Some(2), "{redirected:?}");
    assert_eq!(json(&redirected)["error"]["code"], "INVALID_ARGUMENTS");

    // `--snapshot` and `--publication` are separate artifacts; clap refuses
    // the combination before any handler runs.
    let both = run(
        &dir,
        &bin,
        &[
            "export",
            "-o",
            dir.join("pub").to_str().unwrap(),
            "--snapshot",
            "--publication",
            "--json",
        ],
    );
    assert_eq!(both.status.code(), Some(2), "{both:?}");
    assert!(!dir.join("pub").exists());
}

#[test]
fn publication_preserves_counts_and_imports_but_is_not_a_merge_source() {
    let (dir, bin) = empty_repo("publication_import");
    init_project(&dir, &bin, &["one", "two"]);
    let out = dir.join("pub");
    publication(&dir, &bin, &out);

    // Redaction is row-count preserving: every JSONL file on disk holds
    // exactly the manifest's recorded rows, so the artifact stays valid.
    let manifest = pack_manifest(&out);
    let counts = manifest["counts"].as_object().unwrap();
    for (table, expected) in counts {
        let text = fs::read_to_string(out.join(format!("{table}.jsonl"))).unwrap();
        let actual = text.lines().filter(|line| !line.is_empty()).count() as u64;
        assert_eq!(
            actual,
            expected.as_u64().unwrap(),
            "redaction changed the row count of {table}"
        );
    }
    assert_eq!(manifest["redacted"], true);

    // A redacted publication is a fresh-import artifact...
    let (fresh, fresh_bin) = empty_repo("publication_import_fresh");
    let imported = run(
        &fresh,
        &fresh_bin,
        &["import", out.to_str().unwrap(), "--json"],
    );
    assert!(
        imported.status.success(),
        "fresh import failed: {imported:?}"
    );

    // ...but never a merge source (publication artifacts lose merge fidelity).
    let merged = run(
        &dir,
        &bin,
        &["import", out.to_str().unwrap(), "--mode", "merge", "--json"],
    );
    assert_eq!(merged.status.code(), Some(10), "merge import: {merged:?}");
    assert_eq!(json(&merged)["error"]["code"], "UNSUPPORTED_OPERATION");
}
