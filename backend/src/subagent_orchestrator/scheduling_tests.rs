use super::*;
use serde_json::json;
use std::sync::{Arc, Barrier};
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

fn claimed(root: &Path, session: &str, task: &str) -> bool {
    LedgerStore::open(root, session)
        .unwrap()
        .load("runtime", session, 21)
        .unwrap()
        .unwrap()
        .reservations[task]
        .workspace_write_claimed
}

#[test]
fn every_role_can_read_in_the_same_workspace_without_claiming_writing() {
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
        assert!(!claimed(temp.path(), &session, &task));
    }
}

#[test]
fn review_starts_and_reads_while_another_task_owns_writing() {
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
    ] {
        assert_eq!(
            tool(temp.path(), "review-session", "review", name).unwrap(),
            None
        );
    }
    for name in [
        "apply_patch",
        "functions.exec",
        "functions.write_stdin",
        "mcp__unknown__read",
        "mcp__cua_repl__js",
    ] {
        assert!(
            tool(temp.path(), "review-session", "review", name)
                .unwrap()
                .unwrap()
                .contains("CODEY_SUBAGENT_WORKSPACE_BUSY"),
            "{name}"
        );
    }
    assert!(!claimed(temp.path(), "review-session", "review"));
    assert_eq!(
        tool(temp.path(), "review-session", "review", "grep").unwrap(),
        None
    );
}

#[test]
fn simultaneous_first_writes_are_atomic_across_sessions() {
    for round in 0..12 {
        let temp = tempdir().unwrap();
        for (session, task, workspace) in
            [("a", "writer_a", "/repo"), ("b", "writer_b", "/repo/src")]
        {
            spawn(temp.path(), session, task, "codey_worker", workspace);
        }
        let barrier = Arc::new(Barrier::new(2));
        let handles = [("a", "writer_a"), ("b", "writer_b")]
            .into_iter()
            .map(|(session, task)| {
                let root = temp.path().to_path_buf();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    tool(&root, session, task, "apply_patch").unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results.iter().filter(|result| result.is_none()).count(),
            1,
            "round {round}: {results:?}"
        );
        assert_eq!(
            usize::from(claimed(temp.path(), "a", "writer_a"))
                + usize::from(claimed(temp.path(), "b", "writer_b")),
            1
        );
    }
}

#[test]
fn same_session_review_and_writer_obey_concurrency_and_write_exclusivity() {
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
        tool(temp.path(), "session", "writer", "functions.exec").unwrap(),
        None
    );
    assert_eq!(
        tool(temp.path(), "session", "review", "grep").unwrap(),
        None
    );
    assert!(
        tool(temp.path(), "session", "review", "apply_patch")
            .unwrap()
            .is_some()
    );
    subagent_stopped(temp.path(), "runtime", "session", "writer", 22).unwrap();
    assert_eq!(
        tool(temp.path(), "session", "review", "apply_patch").unwrap(),
        None
    );
}

#[test]
fn write_claim_survives_interrupt_until_acknowledgement() {
    let temp = tempdir().unwrap();
    spawn(temp.path(), "a", "writer_a", "codey_worker", "/repo");
    spawn(
        temp.path(),
        "b",
        "writer_b",
        "codey_visual_analysis",
        "/repo",
    );
    assert_eq!(
        tool(temp.path(), "a", "writer_a", "apply_patch").unwrap(),
        None
    );
    let target = json!({"target":"writer_a"});
    pre_interrupt_agent(temp.path(), "runtime", "a", Some(&target), 22).unwrap();
    assert!(
        tool(temp.path(), "b", "writer_b", "apply_patch")
            .unwrap()
            .is_some()
    );
    assert!(
        tool(temp.path(), "a", "writer_a", "apply_patch")
            .unwrap()
            .is_some()
    );
    settle_interrupt_acknowledgement(
        temp.path(),
        "runtime",
        "a",
        Some(&target),
        &InterruptAcknowledgement {
            prior_outcome: None,
            identifiers: vec![],
        },
        23,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        tool(temp.path(), "b", "writer_b", "apply_patch").unwrap(),
        None
    );
}

#[test]
fn coordination_lock_does_not_block_spawn_or_reads_and_timeout_does_not_claim_writing() {
    let temp = tempdir().unwrap();
    let guard = AdmissionGuard::acquire(temp.path()).unwrap();
    spawn(
        temp.path(),
        "session",
        "review",
        "codey_quick_scan",
        "/repo",
    );
    assert_eq!(
        tool(temp.path(), "session", "review", "grep").unwrap(),
        None
    );
    assert!(
        tool(temp.path(), "session", "review", "apply_patch")
            .unwrap_err()
            .to_string()
            .contains("CODEY_SUBAGENT_ADMISSION_BUSY")
    );
    assert!(!claimed(temp.path(), "session", "review"));
    drop(guard);
    assert_eq!(
        tool(temp.path(), "session", "review", "apply_patch").unwrap(),
        None
    );
}

#[test]
fn independent_workspaces_can_write_concurrently() {
    let temp = tempdir().unwrap();
    for (session, task, workspace) in [("a", "task_a", "/repo-a"), ("b", "task_b", "/repo-b")] {
        spawn(temp.path(), session, task, "codey_worker", workspace);
        assert_eq!(
            tool(temp.path(), session, task, "apply_patch").unwrap(),
            None
        );
    }
}

#[test]
fn fenced_or_unbound_attempts_cannot_acquire_a_write_claim() {
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
    assert!(!claimed(temp.path(), "session", "review"));
}
