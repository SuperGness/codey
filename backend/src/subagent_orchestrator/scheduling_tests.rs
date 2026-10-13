use super::*;
use serde_json::json;
use tempfile::tempdir;

fn spawn(root: &Path, session: &str, task: &str, role: &str, workspace: &str) {
    let input =
        json!({"task_name": task, "agent_type": role, "message": "Complete the assigned task"});
    assert_eq!(
        pre_spawn_with_workspace_and_turn(
            root,
            "runtime",
            session,
            Some(&input),
            RootHookContext::new(Some(workspace), 0, 10),
        )
        .unwrap(),
        None
    );
    post_spawn(
        root,
        "runtime",
        session,
        Some(&input),
        Some(&json!({"agent_id": task})),
        11,
    )
    .unwrap();
}

fn tool(root: &Path, session: &str, task: &str, name: &str) -> Result<Option<String>> {
    authorize_child_tool_with_context(
        root,
        "runtime",
        session,
        ChildToolContext {
            agent_id: task,
            agent_type: None,
            transcript_path: None,
            tool_name: name,
            tool_input: None,
        },
        20,
    )
}

#[test]
fn every_role_can_read_in_the_same_workspace() {
    let temp = tempdir().unwrap();
    for (index, role) in crate::config::SUBAGENT_ROLE_IDS.into_iter().enumerate() {
        let session = format!("session-{index}");
        let task = format!("task_{index}");
        spawn(temp.path(), &session, &task, role, "/repo");
        for name in [
            "mcp__codey_fastctx__grep",
            "functions.view_image",
            "web.run",
            "read_mcp_resource",
        ] {
            assert_eq!(
                tool(temp.path(), &session, &task, name).unwrap(),
                None,
                "{role} {name}"
            );
        }
    }
}

#[test]
fn review_and_writer_run_concurrently_in_the_same_workspace() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "writer-session",
        "writer",
        "codey_worker",
        "/repo",
    );
    assert_eq!(
        tool(temp.path(), "writer-session", "writer", "apply_patch").unwrap(),
        None
    );
    spawn(
        temp.path(),
        "review-session",
        "review",
        "codey_deep_research",
        "/repo/src",
    );
    for name in [
        "mcp__codey_fastctx__inspect_local_file",
        "mcp__codey_fastctx__glob",
        "view_image",
        "functions.exec",
        "functions.exec_command",
        "functions.write_stdin",
        "apply_patch",
        "mcp__unknown__read",
        "mcp__cua_repl__js",
        "grep",
    ] {
        assert_eq!(
            tool(temp.path(), "review-session", "review", name).unwrap(),
            None,
            "{name}"
        );
    }
}

#[test]
fn same_session_review_and_writer_obey_concurrency() {
    let temp = tempdir().unwrap();
    spawn(temp.path(), "session", "writer", "codey_worker", "/repo");
    spawn(
        temp.path(),
        "session",
        "review",
        "codey_quick_scan",
        "/repo",
    );
    let third = json!({"task_name":"third", "agent_type":"codey_worker", "message":"Work"});
    assert!(
        pre_spawn(temp.path(), "runtime", "session", Some(&third), 2, 12)
            .unwrap()
            .unwrap()
            .contains(CONCURRENCY_LIMIT_ERROR_CODE)
    );
    assert_eq!(
        tool(temp.path(), "session", "writer", "apply_patch").unwrap(),
        None
    );
    assert_eq!(
        tool(temp.path(), "session", "review", "apply_patch").unwrap(),
        None
    );
}

#[test]
fn writers_in_different_sessions_share_the_workspace_without_exclusion() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "writer-a-session",
        "writer_a",
        "codey_worker",
        "/repo",
    );
    spawn(
        temp.path(),
        "writer-b-session",
        "writer_b",
        "codey_worker",
        "/repo",
    );
    for (session, task) in [
        ("writer-a-session", "writer_a"),
        ("writer-b-session", "writer_b"),
    ] {
        for name in [
            "apply_patch",
            "functions.exec",
            "exec_command",
            "mcp__codey_fastctx__replace",
        ] {
            assert_eq!(
                tool(temp.path(), session, task, name).unwrap(),
                None,
                "{session} {task} {name}"
            );
        }
    }
}

#[test]
fn unbound_or_fenced_attempts_cannot_use_tools() {
    let temp = tempdir().unwrap();
    assert!(
        tool(temp.path(), "session", "unbound", "apply_patch")
            .unwrap()
            .is_some()
    );
    spawn(
        temp.path(),
        "session",
        "review",
        "codey_quick_scan",
        "/repo",
    );
    pre_interrupt_agent(
        temp.path(),
        "runtime",
        "session",
        Some(&json!({"target":"review"})),
        22,
    )
    .unwrap();
    assert!(
        tool(temp.path(), "session", "review", "apply_patch")
            .unwrap()
            .is_some()
    );
}
