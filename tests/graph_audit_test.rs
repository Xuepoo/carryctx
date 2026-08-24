//! CTX-0076 / issue #99: graph add-node/link/extract-deps/scan mutations wrote
//! via the GraphRepository directly — no UnitOfWork, no audit events — so team
//! projections and the event-log audit trail never saw graph changes. Every
//! graph mutation must commit its rows together with audit events in the same
//! transaction, using the dotted past-tense event taxonomy.

mod common;

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

fn events(dir: &std::path::Path, bin: &std::path::Path) -> serde_json::Value {
    let out = std::process::Command::new(bin)
        .args(["event", "list", "--limit", "1000", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .current_dir(dir)
        .output()
        .expect("event list should execute");
    assert!(
        out.status.success(),
        "event list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("event list envelope")
}

fn events_of_type<'a>(
    value: &'a serde_json::Value,
    event_type: &str,
) -> Vec<&'a serde_json::Value> {
    value["data"]["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["event_type"] == event_type)
        .collect()
}

#[test]
fn test_graph_add_node_records_node_added_event() {
    let (dir, bin) = common::setup_test_project("graph_add_node_event");
    setup(&dir, &bin);

    let out = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "graph",
            "add-node",
            "--node-type",
            "module",
            "--name",
            "src/domain",
            "--description",
            "pure domain layer",
        ],
    );
    assert!(
        out.status.success(),
        "add-node must succeed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let node: serde_json::Value = serde_json::from_slice(&out.stdout).expect("envelope");
    let node_id = node["data"]["id"].as_str().expect("node id");

    let value = events(&dir, &bin);
    let added = events_of_type(&value, "graph.node_added");
    assert_eq!(
        added.len(),
        1,
        "exactly one graph.node_added event: {value}"
    );
    assert_eq!(added[0]["payload"]["nodeId"], node_id);
    assert_eq!(added[0]["payload"]["name"], "src/domain");
    assert_eq!(added[0]["payload"]["nodeType"], "module");
}

#[test]
fn test_graph_link_records_edge_added_event() {
    let (dir, bin) = common::setup_test_project("graph_link_event");
    setup(&dir, &bin);

    let mut ids = Vec::new();
    for name in ["src/a.rs", "src/b.rs"] {
        let out = common::run_cmd(
            &dir,
            &bin,
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
        assert!(out.status.success(), "add-node {name} must succeed");
        let envelope: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("add-node envelope");
        ids.push(
            envelope["data"]["id"]
                .as_str()
                .expect("node id")
                .to_string(),
        );
    }
    let (a, b) = (ids[0].clone(), ids[1].clone());

    let out = common::run_cmd(
        &dir,
        &bin,
        &["--json", "graph", "link", &a, &b, "depends_on"],
    );
    assert!(
        out.status.success(),
        "link must succeed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let value = events(&dir, &bin);
    let linked = events_of_type(&value, "graph.edge_added");
    assert_eq!(
        linked.len(),
        1,
        "exactly one graph.edge_added event: {value}"
    );
    assert_eq!(linked[0]["payload"]["sourceId"], a);
    assert_eq!(linked[0]["payload"]["targetId"], b);
    assert_eq!(linked[0]["payload"]["relation"], "depends_on");
}

/// Scan and extract-deps are import-style mutations: their created nodes and
/// edges must be covered by an audit event committed in the same transaction.
#[test]
fn test_graph_scan_and_extract_deps_record_audit_events() {
    let (dir, bin) = common::setup_test_project("graph_scan_event");
    setup(&dir, &bin);
    std::fs::write(dir.join("lib.rs"), "pub mod alpha;\n").unwrap();
    std::fs::write(dir.join("alpha.rs"), "pub fn f() {}\n").unwrap();

    let scan = common::run_cmd(&dir, &bin, &["--json", "graph", "scan"]);
    assert!(
        scan.status.success(),
        "scan must succeed: {} {}",
        String::from_utf8_lossy(&scan.stdout),
        String::from_utf8_lossy(&scan.stderr)
    );

    let value = events(&dir, &bin);
    let scanned = events_of_type(&value, "graph.scanned");
    assert_eq!(scanned.len(), 1, "exactly one graph.scanned event: {value}");
    let payload_edges = scanned[0]["payload"]["edgesCreated"]
        .as_i64()
        .expect("edgesCreated count");
    let payload_nodes = scanned[0]["payload"]["nodesCreated"]
        .as_i64()
        .expect("nodesCreated count");
    assert!(
        payload_edges >= 1,
        "the scan must have produced an edge: {value}"
    );
    assert!(
        payload_nodes >= 2,
        "the scan must have produced nodes: {value}"
    );

    // extract-deps on another file adds its own audit event.
    std::fs::write(dir.join("beta.rs"), "pub fn g() {}\n").unwrap();
    let extract = common::run_cmd(&dir, &bin, &["--json", "graph", "extract-deps", "beta.rs"]);
    assert!(
        extract.status.success(),
        "extract-deps must succeed: {} {}",
        String::from_utf8_lossy(&extract.stdout),
        String::from_utf8_lossy(&extract.stderr)
    );
    let value = events(&dir, &bin);
    let extracted = events_of_type(&value, "graph.deps_extracted");
    assert_eq!(
        extracted.len(),
        1,
        "exactly one graph.deps_extracted event: {value}"
    );
    assert_eq!(extracted[0]["payload"]["file"], "beta.rs");
}
