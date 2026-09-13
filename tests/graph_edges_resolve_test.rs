//! CTX-0168 / issue #191: `graph edges` only accepted a raw node ULID —
//! passing a file path or node name failed with "not a Context Graph node
//! ID" even though `graph export --focus` already resolves by ULID, exact
//! name, and name suffix (`filter_subgraph_by_focus`). Edges must resolve
//! the same way: exact ULID, then exact name, then unambiguous name suffix.
//! Unknown targets fail with RESOURCE_NOT_FOUND naming the target;
//! ambiguous suffixes fail descriptively listing the candidates.

mod common;

use serde_json::Value;

fn setup(dir: &std::path::Path, bin: &std::path::Path) {
    common::run_cmd(dir, bin, &["init", "--force"]);
    common::run_cmd(
        dir,
        bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );
}

fn add_node(dir: &std::path::Path, bin: &std::path::Path, name: &str) -> String {
    let out = common::run_cmd(
        dir,
        bin,
        &[
            "--json",
            "graph",
            "add-node",
            "--node-type",
            "file",
            "--name",
            name,
        ],
    );
    assert!(
        out.status.success(),
        "add-node {name} must succeed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).expect("add-node envelope");
    envelope["data"]["id"]
        .as_str()
        .expect("node id")
        .to_string()
}

fn link(dir: &std::path::Path, bin: &std::path::Path, source: &str, target: &str) {
    let out = common::run_cmd(
        dir,
        bin,
        &["--json", "graph", "link", source, target, "depends_on"],
    );
    assert!(
        out.status.success(),
        "link must succeed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn edges_envelope(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    serde_json::from_str(stdout.trim())
        .or_else(|_| serde_json::from_str(stderr.trim()))
        .unwrap_or_else(|e| panic!("expected a JSON envelope ({e}): {stdout} | {stderr}"))
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn test_graph_edges_by_ulid_still_works() {
    let (dir, bin) = common::setup_test_project("graph_edges_ulid");
    setup(&dir, &bin);
    let a = add_node(&dir, &bin, "src/a.rs");
    let b = add_node(&dir, &bin, "src/b.rs");
    link(&dir, &bin, &a, &b);

    let out = common::run_cmd(&dir, &bin, &["--json", "graph", "edges", &a]);
    assert!(
        out.status.success(),
        "edges by ULID must succeed: {}",
        combined(&out)
    );
    let value = edges_envelope(&out);
    assert_eq!(value["success"], true);
    let edges = value["data"].as_array().expect("edges array");
    assert_eq!(edges.len(), 1, "exactly the linked edge: {value}");
}

#[test]
fn test_graph_edges_resolves_exact_name() {
    let (dir, bin) = common::setup_test_project("graph_edges_name");
    setup(&dir, &bin);
    let a = add_node(&dir, &bin, "src/a.rs");
    let b = add_node(&dir, &bin, "src/b.rs");
    link(&dir, &bin, &a, &b);

    let out = common::run_cmd(&dir, &bin, &["--json", "graph", "edges", "src/a.rs"]);
    assert!(
        out.status.success(),
        "edges by exact name must succeed: {}",
        combined(&out)
    );
    let value = edges_envelope(&out);
    let edges = value["data"].as_array().expect("edges array");
    assert_eq!(edges.len(), 1, "same edge as ULID lookup: {value}");
    assert_eq!(edges[0]["source_id"], a);
    assert_eq!(edges[0]["target_id"], b);
}

#[test]
fn test_graph_edges_resolves_unambiguous_suffix() {
    let (dir, bin) = common::setup_test_project("graph_edges_suffix");
    setup(&dir, &bin);
    let a = add_node(&dir, &bin, "src/alpha.rs");
    let b = add_node(&dir, &bin, "src/beta.rs");
    link(&dir, &bin, &a, &b);

    // "alpha.rs" is a suffix of exactly one node name.
    let out = common::run_cmd(&dir, &bin, &["--json", "graph", "edges", "alpha.rs"]);
    assert!(
        out.status.success(),
        "edges by unambiguous suffix must succeed: {}",
        combined(&out)
    );
    let value = edges_envelope(&out);
    let edges = value["data"].as_array().expect("edges array");
    assert_eq!(edges.len(), 1, "edges of src/alpha.rs: {value}");
    assert_eq!(edges[0]["source_id"], a);
}

#[test]
fn test_graph_edges_unknown_target_is_descriptive_not_found() {
    let (dir, bin) = common::setup_test_project("graph_edges_missing");
    setup(&dir, &bin);
    add_node(&dir, &bin, "src/a.rs");

    let out = common::run_cmd(&dir, &bin, &["--json", "graph", "edges", "no-such-node.rs"]);
    assert!(!out.status.success(), "unknown target must fail");
    assert_eq!(out.status.code(), Some(7), "exit 7: {}", combined(&out));
    let value = edges_envelope(&out);
    assert_eq!(value["success"], false);
    assert_eq!(value["error"]["code"], "RESOURCE_NOT_FOUND");
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("no-such-node.rs"),
        "error must name the tried target: {message}"
    );
    assert!(
        message.contains("suffix") || message.contains("name"),
        "error must explain ID/name/suffix resolution was tried: {message}"
    );
}

#[test]
fn test_graph_edges_ambiguous_suffix_lists_candidates() {
    let (dir, bin) = common::setup_test_project("graph_edges_ambiguous");
    setup(&dir, &bin);
    let a = add_node(&dir, &bin, "src/a/foo.rs");
    let b = add_node(&dir, &bin, "src/b/foo.rs");
    link(&dir, &bin, &a, &b);

    let out = common::run_cmd(&dir, &bin, &["--json", "graph", "edges", "foo.rs"]);
    assert!(
        !out.status.success(),
        "ambiguous suffix must fail: {}",
        combined(&out)
    );
    let value = edges_envelope(&out);
    assert_eq!(value["success"], false);
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.to_lowercase().contains("ambiguous"),
        "error must say the reference is ambiguous: {message}"
    );
    assert!(
        message.contains("foo.rs"),
        "error must name the tried target: {message}"
    );
    // Both candidates must be identifiable so the caller can retry by ULID.
    for id in [&a, &b] {
        assert!(
            message.contains(id),
            "error must list candidate ULID {id}: {message}"
        );
    }
}
