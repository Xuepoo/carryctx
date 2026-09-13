//! CTX-0173: a Git superproject and each of its git submodules are independent
//! CarryCtx projects.
//!
//! CarryCtx resolves the project root with `git rev-parse --show-toplevel` and
//! stores state under `<git-common-dir>/carryctx/state.sqlite`. For a submodule
//! the common dir is `<super>/.git/modules/<name>`, so the submodule must get
//! its own project identity, task namespace, worktree registry, graph, and
//! detached-HEAD resolution — without leaking into the superproject and
//! vice-versa.
//!
//! Every fixture is a disposable pair of Git repositories under
//! `std::env::temp_dir()`; direct `git` calls scrub ambient `GIT_*` state the
//! same way `common::fixture_git` and `git_e2e_test.rs` do, so hook runners
//! cannot redirect fixture commands into the repository under test.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU16, Ordering};

fn bin() -> PathBuf {
    common::test_binary()
}

fn run(dir: &Path, args: &[&str]) -> Output {
    common::run_cmd(dir, &bin(), args)
}

fn run_json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = run(dir, args);
    let stream = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(stream).unwrap_or_else(|error| {
        panic!(
            "expected JSON from {args:?}, got {error}: {}",
            String::from_utf8_lossy(stream)
        )
    })
}

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

fn git_out(repo: &Path, args: &[&str]) -> Output {
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
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU16 = AtomicU16::new(0);
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ctx0173_{name}_{}_{count}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn init_repo(dir: &Path) {
    git_ok(dir, &["init", "-b", "main"]);
    git_ok(dir, &["config", "user.email", "test@carryctx.dev"]);
    git_ok(dir, &["config", "user.name", "Test"]);
    git_ok(dir, &["commit", "--allow-empty", "-m", "init"]);
}

/// A disposable superproject with a local-path submodule at `super/sub`.
///
/// The temp root is removed on drop, so cleanup happens even when an assertion
/// panics mid-test.
struct Fixture {
    root: PathBuf,
    super_dir: PathBuf,
    sub_dir: PathBuf,
    bin: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture(name: &str) -> Fixture {
    let root = scratch(name);
    let child = root.join("child");
    let super_dir = root.join("super");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir_all(&super_dir).unwrap();
    init_repo(&child);
    init_repo(&super_dir);

    // A tracked file makes the child a meaningful commit to check out.
    std::fs::write(child.join("child.txt"), "child\n").unwrap();
    git_ok(&child, &["add", "child.txt"]);
    git_ok(&child, &["commit", "-m", "child content"]);

    // Local-path submodules are refused unless the file protocol is allowed.
    git_ok(
        &super_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            child.to_str().unwrap(),
            "sub",
        ],
    );
    let sub_dir = super_dir.join("sub");
    assert!(
        sub_dir.join(".git").exists(),
        "submodule checkout must exist at {}",
        sub_dir.display()
    );

    Fixture {
        root,
        super_dir,
        sub_dir,
        bin: bin(),
    }
}

impl Fixture {
    fn init_both(&self) -> (String, String) {
        let super_id = self.init(&self.super_dir);
        let sub_id = self.init(&self.sub_dir);
        (super_id, sub_id)
    }

    fn init(&self, dir: &Path) -> String {
        let value = run_json(dir, &["init", "--force", "--json"]);
        assert_eq!(value["success"], true, "init in {} failed", dir.display());
        let registered = common::run_cmd(
            dir,
            &self.bin,
            &[
                "agent",
                "register",
                "--name",
                "tester",
                "--provider",
                "test",
            ],
        );
        assert!(
            registered.status.success(),
            "agent register in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&registered.stderr)
        );
        value["data"]["project_id"].as_str().unwrap().to_string()
    }

    fn task_titles(&self, dir: &Path) -> Vec<String> {
        run_json(dir, &["task", "list", "--json"])["data"]
            .as_array()
            .expect("task list data array")
            .iter()
            .filter_map(|task| task["title"].as_str().map(str::to_string))
            .collect()
    }

    fn create_task(&self, dir: &Path, title: &str) -> String {
        let value = run_json(dir, &["task", "create", "--title", title, "--json"]);
        assert_eq!(value["success"], true, "task create '{title}' failed");
        value["data"]["display_id"].as_str().unwrap().to_string()
    }

    fn graph_node_names(&self, dir: &Path) -> Vec<String> {
        // `graph export -t json` writes the raw graph JSON to stdout (the
        // envelope is only added for the global `--json` flag).
        let output = run(dir, &["graph", "export", "-t", "json"]);
        assert!(
            output.status.success(),
            "graph export failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        let graph: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("raw graph JSON");
        graph["nodes"]
            .as_array()
            .expect("nodes array")
            .iter()
            .filter_map(|node| node["name"].as_str().map(str::to_string))
            .collect()
    }
}

/// Case 1: identity, task namespace, and listing are independent per project.
#[test]
fn superproject_and_submodule_are_independent_projects() {
    let fixture = fixture("independence");
    let (super_id, sub_id) = fixture.init_both();
    assert_ne!(
        super_id, sub_id,
        "superproject and submodule must have distinct project ids"
    );

    fixture.create_task(&fixture.sub_dir, "SUB-ONLY-TASK");
    fixture.create_task(&fixture.super_dir, "SUPER-ONLY-TASK");

    let super_titles = fixture.task_titles(&fixture.super_dir);
    assert!(
        super_titles.contains(&"SUPER-ONLY-TASK".to_string()),
        "superproject must list its own task: {super_titles:?}"
    );
    assert!(
        !super_titles.contains(&"SUB-ONLY-TASK".to_string()),
        "superproject must not list the submodule task: {super_titles:?}"
    );

    let sub_titles = fixture.task_titles(&fixture.sub_dir);
    assert!(
        sub_titles.contains(&"SUB-ONLY-TASK".to_string()),
        "submodule must list its own task: {sub_titles:?}"
    );
    assert!(
        !sub_titles.contains(&"SUPER-ONLY-TASK".to_string()),
        "submodule must not list the superproject task: {sub_titles:?}"
    );
}

/// Case 2: state lands under each project's own git-common-dir.
#[test]
fn state_databases_live_in_each_projects_common_dir() {
    let fixture = fixture("state_location");
    let (super_id, sub_id) = fixture.init_both();
    assert_ne!(super_id, sub_id);

    let super_db = fixture.super_dir.join(".git/carryctx/state.sqlite");
    let sub_db = fixture
        .root
        .join("super/.git/modules/sub/carryctx/state.sqlite");
    assert!(super_db.is_file(), "super state db missing: {super_db:?}");
    assert!(sub_db.is_file(), "submodule state db missing: {sub_db:?}");

    // The init envelope must report exactly those paths.
    let super_init = run_json(&fixture.super_dir, &["init", "--force", "--json"]);
    let sub_init = run_json(&fixture.sub_dir, &["init", "--force", "--json"]);
    assert_eq!(
        super_init["data"]["state_path"].as_str(),
        Some(super_db.to_str().unwrap())
    );
    assert_eq!(
        sub_init["data"]["state_path"].as_str(),
        Some(sub_db.to_str().unwrap())
    );
    assert_ne!(
        super_init["data"]["state_path"], sub_init["data"]["state_path"],
        "the two projects must not share one state database"
    );
}

/// Case 3: `worktree create` inside the submodule stays inside the submodule.
#[test]
fn worktree_create_in_submodule_is_isolated_from_superproject() {
    let fixture = fixture("worktree_isolation");
    fixture.init_both();
    let task = fixture.create_task(&fixture.sub_dir, "worktree task");

    let created = run_json(&fixture.sub_dir, &["worktree", "create", &task, "--json"]);
    assert_eq!(
        created["success"], true,
        "worktree create failed: {created}"
    );
    let branch = created["data"]["branch"].as_str().unwrap().to_string();
    assert_eq!(branch, format!("carryctx/{}", task.to_lowercase()));

    let worktree_dir = fixture.sub_dir.join(".worktrees").join(task.to_lowercase());
    assert!(
        worktree_dir.is_dir(),
        "submodule worktree must be under super/sub/.worktrees: {worktree_dir:?}"
    );

    let sub_worktrees = git_ok(&fixture.sub_dir, &["worktree", "list", "--porcelain"]);
    assert!(
        sub_worktrees.contains(&branch),
        "submodule worktree list must show branch {branch}: {sub_worktrees}"
    );
    assert!(
        sub_worktrees.contains(worktree_dir.to_str().unwrap()),
        "submodule worktree list must show its path: {sub_worktrees}"
    );

    // The superproject neither registers the submodule worktree nor sees a task.
    let super_worktrees = git_ok(&fixture.super_dir, &["worktree", "list", "--porcelain"]);
    assert!(
        !super_worktrees.contains(".worktrees"),
        "superproject must not see the submodule worktree: {super_worktrees}"
    );
    assert!(
        !fixture.super_dir.join(".worktrees").exists(),
        "no worktree directory may be created in the superproject"
    );
    assert_eq!(
        fixture.task_titles(&fixture.super_dir),
        Vec::<String>::new(),
        "the superproject must not gain a task from submodule worktree creation"
    );

    // Cleanup: remove through carryctx so no worktree registration lingers in
    // the shared fixture before the temp root is dropped.
    let removed = run_json(&fixture.sub_dir, &["worktree", "remove", &task, "--json"]);
    assert_eq!(
        removed["success"], true,
        "worktree remove failed: {removed}"
    );
    assert!(
        !worktree_dir.exists(),
        "removed worktree directory must be gone: {worktree_dir:?}"
    );
}

/// Case 4: a detached HEAD (typical fresh submodule clone) still resolves to
/// the submodule project.
#[test]
fn detached_head_submodule_still_resolves_its_project() {
    let fixture = fixture("detached_head");
    let sub_id = fixture.init(&fixture.sub_dir);
    let task = fixture.create_task(&fixture.sub_dir, "detached task");

    git_ok(&fixture.sub_dir, &["checkout", "--detach"]);

    let resume = run_json(&fixture.sub_dir, &["resume", "--compact", "--json"]);
    assert_eq!(resume["success"], true, "resume failed: {resume}");
    assert_eq!(
        resume["data"]["projectId"].as_str(),
        Some(sub_id.as_str()),
        "resume must report the submodule project id"
    );
    assert!(
        resume["data"]["branch"].is_null(),
        "detached HEAD must report a null branch: {}",
        resume["data"]["branch"]
    );

    let titles = fixture.task_titles(&fixture.sub_dir);
    assert_eq!(
        titles,
        vec!["detached task".to_string()],
        "task list must still show the submodule task after detach: {task}"
    );
}

/// Case 5: `graph scan` is scoped to the repository it runs in and never
/// creates nodes for the other project's files.
#[test]
fn graph_scan_is_scoped_per_project() {
    let fixture = fixture("graph_scope");
    fixture.init_both();

    std::fs::create_dir_all(fixture.super_dir.join("src")).unwrap();
    std::fs::create_dir_all(fixture.sub_dir.join("src")).unwrap();
    std::fs::write(
        fixture.super_dir.join("src/super_dep.rs"),
        "pub fn dep() {}\n",
    )
    .unwrap();
    std::fs::write(
        fixture.super_dir.join("src/super_main.rs"),
        "use crate::super_dep;\npub fn main() {}\n",
    )
    .unwrap();
    std::fs::write(fixture.sub_dir.join("src/sub_dep.rs"), "pub fn dep() {}\n").unwrap();
    std::fs::write(
        fixture.sub_dir.join("src/sub_main.rs"),
        "use crate::sub_dep;\npub fn main() {}\n",
    )
    .unwrap();

    let super_scan = run_json(&fixture.super_dir, &["graph", "scan", "--json"]);
    let sub_scan = run_json(&fixture.sub_dir, &["graph", "scan", "--json"]);
    assert_eq!(super_scan["success"], true, "super graph scan failed");
    assert_eq!(sub_scan["success"], true, "sub graph scan failed");

    let super_nodes = fixture.graph_node_names(&fixture.super_dir);
    assert!(
        super_nodes.contains(&"src/super_main.rs".to_string()),
        "superproject graph must contain its own file: {super_nodes:?}"
    );
    assert!(
        !super_nodes.contains(&"src/sub_main.rs".to_string()),
        "superproject graph must not contain the submodule file: {super_nodes:?}"
    );

    let sub_nodes = fixture.graph_node_names(&fixture.sub_dir);
    assert!(
        sub_nodes.contains(&"src/sub_main.rs".to_string()),
        "submodule graph must contain its own file: {sub_nodes:?}"
    );
    assert!(
        !sub_nodes.contains(&"src/super_main.rs".to_string()),
        "scanning the submodule must never create a node for a super-only file: {sub_nodes:?}"
    );
}
