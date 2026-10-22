mod common;

use common::{init_and_agent, run_cmd, setup_test_project};
use serde_json::Value;

fn parse_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn test_context_include_related_tasks_json_and_markdown() {
    let (dir, bin) = setup_test_project("context_related_tasks");
    init_and_agent(&dir, &bin);

    // Create tasks A, B, C
    let out_b = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "Task B (Prereq)"],
    );
    assert!(out_b.status.success());
    let id_b = parse_json(&out_b)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_string();

    let out_a = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "Task A (Target)"],
    );
    assert!(out_a.status.success());
    let id_a = parse_json(&out_a)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_string();

    let out_c = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "Task C (Dependent)"],
    );
    assert!(out_c.status.success());
    let id_c = parse_json(&out_c)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Link dependencies: A depends on B; C depends on A
    let dep_ab = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "depend", &id_a, "--on", &id_b],
    );
    assert!(dep_ab.status.success());

    let dep_ca = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "depend", &id_c, "--on", &id_a],
    );
    assert!(dep_ca.status.success());

    // 1. Without --include-related-tasks, related_tasks is not populated
    let out_default = run_cmd(&dir, &bin, &["--json", "context", "--task", &id_a]);
    assert!(out_default.status.success());
    let data_default = &parse_json(&out_default)["data"];
    assert!(
        data_default.get("related_tasks").is_none() || data_default["related_tasks"].is_null(),
        "related_tasks must not be populated without flag"
    );

    // 2. With --include-related-tasks, related_tasks has depends_on and blocks
    let out_flag = run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "context",
            "--task",
            &id_a,
            "--include-related-tasks",
        ],
    );
    assert!(out_flag.status.success());
    let data_flag = &parse_json(&out_flag)["data"];
    let rt = &data_flag["related_tasks"];
    assert!(rt.is_object(), "related_tasks must be an object");

    let depends = rt["depends_on"].as_array().expect("depends_on array");
    assert_eq!(depends.len(), 1);
    assert_eq!(depends[0]["display_id"], id_b);
    assert!(depends[0]["title"].as_str().unwrap().contains("Task B"));

    let blocks = rt["blocks"].as_array().expect("blocks array");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["display_id"], id_c);
    assert!(blocks[0]["title"].as_str().unwrap().contains("Task C"));

    // Also check camelCase alias
    assert_eq!(data_flag["relatedTasks"], data_flag["related_tasks"]);

    // 3. With --full, related_tasks is included automatically with full fields (id, kind)
    let out_full = run_cmd(
        &dir,
        &bin,
        &["--json", "context", "--task", &id_a, "--full"],
    );
    assert!(out_full.status.success());
    let data_full = &parse_json(&out_full)["data"];
    let rt_full = &data_full["related_tasks"];
    assert!(rt_full["depends_on"][0].get("kind").is_some());
    assert!(rt_full["depends_on"][0].get("id").is_some());

    // 4. In --compact mode, internal id and kind are stripped
    let out_compact = run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "context",
            "--task",
            &id_a,
            "--include-related-tasks",
            "--compact",
        ],
    );
    assert!(out_compact.status.success());
    let data_compact = &parse_json(&out_compact)["data"];
    let rt_compact = &data_compact["related_tasks"];
    assert!(rt_compact["depends_on"][0].get("id").is_none());
    assert!(rt_compact["depends_on"][0].get("kind").is_none());
    assert_eq!(rt_compact["depends_on"][0]["display_id"], id_b);

    // 5. In markdown mode, related tasks sections are rendered
    let out_md = run_cmd(
        &dir,
        &bin,
        &[
            "context",
            "--task",
            &id_a,
            "--include-related-tasks",
            "--format",
            "markdown",
        ],
    );
    assert!(out_md.status.success());
    let md = String::from_utf8_lossy(&out_md.stdout);
    assert!(md.contains("## Related Tasks"));
    assert!(md.contains("### Depends On (Prerequisites)"));
    assert!(md.contains(&format!("- **{id_b}**:")));
    assert!(md.contains("### Blocks (Dependents)"));
    assert!(md.contains(&format!("- **{id_c}**:")));
}
