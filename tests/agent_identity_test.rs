mod common;

/// CTX-0071 / issue #104: agent identity integrity.
///
/// - Duplicate names must be rejected with a helpful error (the schema has a
///   UNIQUE(project_id, name) constraint; the use case must pre-check and map
///   violations to an actionable message instead of a raw database error).
/// - Renaming into a taken name must be rejected.
/// - Deactivated agents must not resolve: they cannot act.
#[test]
fn test_duplicate_agent_name_rejected_with_helpful_error() {
    let (dir, bin) = common::setup_test_project("agent_dup_name");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    let first = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "alice", "--provider", "test"],
    );
    assert!(first.status.success(), "first register should succeed");

    let second = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "alice", "--provider", "test"],
    );
    assert!(!second.status.success(), "duplicate register must fail");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("STATE_CONFLICT"),
        "duplicate register must report STATE_CONFLICT: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("rename") || stderr.to_lowercase().contains("already"),
        "duplicate register error must be actionable: {stderr}"
    );

    // Renaming bob onto alice's taken name must fail too.
    let bob = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "bob", "--provider", "test"],
    );
    assert!(bob.status.success(), "bob register should succeed");
    let rename = common::run_cmd(
        &dir,
        &bin,
        &["agent", "rename", "bob", "--name", "alice", "--json"],
    );
    assert!(
        !rename.status.success(),
        "rename onto a taken name must fail"
    );
    assert!(
        String::from_utf8_lossy(&rename.stderr).contains("STATE_CONFLICT"),
        "rename conflict must report STATE_CONFLICT: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
}

#[test]
fn test_deactivated_agent_cannot_resolve() {
    let (dir, bin) = common::setup_test_project("agent_deactivated");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    let reg = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "ghost", "--provider", "test"],
    );
    assert!(reg.status.success(), "ghost register should succeed");

    let deact = common::run_cmd(&dir, &bin, &["agent", "deactivate", "ghost", "--json"]);
    assert!(deact.status.success(), "deactivate should succeed");

    // A deactivated agent must not act: resolver rejects it instead of
    // silently resolving the deactivated row.
    let out = common::run_cmd_as(
        &dir,
        &bin,
        "ghost",
        &["task", "create", "--title", "ghost work", "--json"],
    );
    assert!(
        !out.status.success(),
        "deactivated agent must not be able to act"
    );
}
