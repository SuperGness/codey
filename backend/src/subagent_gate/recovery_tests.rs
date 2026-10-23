use super::*;

fn start_recovery_child(root: &Path, session: &str) {
    start_recovery_child_named(root, session, "reader");
}

fn start_recovery_child_named(root: &Path, session: &str, task_name: &str) {
    write_test_runtime_policy(root);
    let mut spawn = input("PreToolUse", session);
    spawn.cwd = Some(format!("/repo-{task_name}"));
    spawn.turn_id = Some("root-turn-a".into());
    spawn.tool_name = Some("agents.spawn_agent".into());
    spawn.tool_input = Some(json!({
        "task_name": task_name, "agent_type": "codey_quick_scan", "message": "Read the source"
    }));
    assert_eq!(
        handle_hook_for_runtime_at(&spawn, root, "runtime-a", 10).unwrap(),
        json!({})
    );
    spawn.hook_event_name = "PostToolUse".into();
    let target = format!("/root/{task_name}");
    spawn.tool_response = Some(json!({"agent_id": target}));
    handle_hook_for_runtime_at(&spawn, root, "runtime-a", 11).unwrap();
    let mut start = input("SubagentStart", session);
    start.agent_id = Some(target);
    start.agent_type = Some("codey_quick_scan".into());
    handle_hook_for_runtime_at(&start, root, "runtime-a", 12).unwrap();
}

fn root_command(session: &str) -> HookInput {
    let mut command = input("PreToolUse", session);
    command.tool_name = Some("functions.exec_command".into());
    command.tool_input = Some(json!({"cmd": "git status --short"}));
    command
}

fn unavailable_wait(session: &str) -> HookInput {
    let mut wait = input("PostToolUse", session);
    wait.tool_name = Some("agents.wait_agent".into());
    wait.tool_response = Some(json!("该工具未在当前线程注册，无法执行 agents.wait_agent"));
    wait
}

fn write_resumed_root_rollout(path: &Path, session: &str) {
    let records = [
        json!({"type": "session_meta", "payload": {"id": session, "source": "vscode"}}),
        json!({"type": "event_msg", "payload": {"type": "turn_aborted", "turn_id": "root-turn-a"}}),
        json!({"type": "event_msg", "payload": {"type": "task_started", "turn_id": "root-turn-b"}}),
    ];
    fs::write(
        path,
        records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    )
    .unwrap();
}

#[test]
fn resumed_root_with_rollout_suffix_can_orchestrate_before_and_after_status_read() {
    for suffix in ["", "_00000000-0000-4000-8000-000000000002"] {
        for read_status_first in [false, true] {
            for tool in [
                "agents.send_message",
                "agents.followup_task",
                "agents.interrupt_agent",
            ] {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join(STATE_DIRECTORY);
                let sessions = temp.path().join("sessions/2026/10/22");
                fs::create_dir_all(&sessions).unwrap();
                let session = "00000000-0000-4000-8000-000000000001";
                start_recovery_child(&root, session);
                let transcript = sessions.join(format!(
                    "rollout-2026-10-22T16-25-46-{session}{suffix}.jsonl"
                ));
                write_resumed_root_rollout(&transcript, session);
                let mut request = input("PreToolUse", session);
                request.turn_id = Some("root-turn-b".into());
                request.transcript_path = Some(transcript.to_string_lossy().into_owned());
                if read_status_first {
                    request.tool_name = Some("agents.list_agents".into());
                    request.tool_input = Some(json!({}));
                    assert_eq!(
                        handle_hook_for_runtime_at(&request, &root, "runtime-a", 20).unwrap(),
                        json!({})
                    );
                    request.hook_event_name = "PostToolUse".into();
                    request.tool_response = Some(json!({
                        "agents": [{"agent_id": "/root/reader", "status": "interrupted"}]
                    }));
                    assert_eq!(
                        handle_hook_for_runtime_at(&request, &root, "runtime-a", 21).unwrap()["decision"],
                        "block"
                    );
                    request.hook_event_name = "PreToolUse".into();
                    request.tool_response = None;
                }
                request.tool_name = Some(tool.into());
                request.tool_input = Some(if tool == "agents.interrupt_agent" {
                    json!({"target": "/root/reader"})
                } else {
                    json!({"target": "/root/reader", "message": "Continue the assigned task"})
                });
                assert_eq!(
                    handle_hook_for_runtime_at(&request, &root, "runtime-a", 30).unwrap(),
                    json!({}),
                    "{suffix:?}, read_status_first={read_status_first}, {tool}"
                );
                assert!(
                    root_turn_matches(&root, "runtime-a", session, Some("root-turn-b")).unwrap()
                );
                assert_eq!(
                    active_agent_count_for_runtime(&root, "runtime-a", session).unwrap(),
                    1
                );
            }
        }
    }
}

#[test]
fn resumed_root_rejects_non_rollout_paths_and_paths_outside_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join(STATE_DIRECTORY);
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let session = "resume-path-session";
    start_recovery_child(&root, session);
    let mut transcripts = vec![
        temp.path().join(format!("rollout-{session}_suffix.jsonl")),
        sessions.join(format!("events-{session}.jsonl")),
        sessions.join(format!("rollout-{session}.txt")),
    ];
    for transcript in &transcripts {
        write_resumed_root_rollout(transcript, session);
    }
    let directory = sessions.join(format!("rollout-{session}_directory.jsonl"));
    fs::create_dir(&directory).unwrap();
    transcripts.push(directory);
    #[cfg(unix)]
    {
        let target = sessions.join(format!("rollout-{session}_target.jsonl"));
        write_resumed_root_rollout(&target, session);
        let link = sessions.join(format!("rollout-{session}_link.jsonl"));
        std::os::unix::fs::symlink(&target, &link).unwrap();
        transcripts.push(link);
        let linked_directory = sessions.join("outside");
        std::os::unix::fs::symlink(temp.path(), &linked_directory).unwrap();
        transcripts.push(linked_directory.join(format!("rollout-{session}_suffix.jsonl")));
    }
    for transcript in transcripts {
        let mut request = input("PreToolUse", session);
        request.turn_id = Some("root-turn-b".into());
        request.transcript_path = Some(transcript.to_string_lossy().into_owned());
        request.tool_name = Some("agents.send_message".into());
        request.tool_input = Some(json!({"target": "/root/reader", "message": "Continue"}));
        assert_eq!(
            handle_hook_for_runtime_at(&request, &root, "runtime-a", 30).unwrap()["hookSpecificOutput"]
                ["permissionDecision"],
            "deny",
            "{}",
            transcript.display()
        );
        assert!(!root_turn_matches(&root, "runtime-a", session, Some("root-turn-b")).unwrap());
        assert_eq!(
            active_agent_count_for_runtime(&root, "runtime-a", session).unwrap(),
            1
        );
    }
}

#[test]
fn root_entry_points_recover_after_status_tool_failure_without_stop() {
    for event in ["PreToolUse", "UserPromptSubmit", "PostToolUse"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session = "no-stop";
        start_recovery_child(root, session);
        let command = root_command(session);
        assert_eq!(
            handle_hook_for_runtime_at(&command, root, "runtime-a", 1_000).unwrap()["hookSpecificOutput"]
                ["permissionDecision"],
            "deny"
        );
        handle_hook_for_runtime_at(&unavailable_wait(session), root, "runtime-a", 1_000).unwrap();
        let mut trigger = input(event, session);
        if event == "PreToolUse" {
            trigger = root_command(session);
        }
        if event == "PostToolUse" {
            trigger = unavailable_wait(session);
        }
        handle_hook_for_runtime_at(
            &trigger,
            root,
            "runtime-a",
            1_000 + UNAVAILABLE_STATUS_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
            0,
            "{event}"
        );
        assert_eq!(
            handle_hook_for_runtime_at(
                &command,
                root,
                "runtime-a",
                1_001 + UNAVAILABLE_STATUS_GRACE_MILLIS
            )
            .unwrap(),
            json!({}),
            "{event}"
        );
        let ledger: Value = serde_json::from_slice(
            &fs::read(session_state_dir(root, session).join("orchestrator-ledger-v1.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(ledger["reservations"]["reader"]["outcome"], "lost");
        assert!(ledger["reservations"]["reader"]["fenced_at_ms"].is_number());
    }
}

#[test]
fn unavailable_status_tools_recover_after_short_grace_and_reject_late_child_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let session = "unavailable";
    start_recovery_child(root, session);
    let wait = unavailable_wait(session);
    assert_eq!(
        handle_hook_for_runtime_at(&wait, root, "runtime-a", 1_000).unwrap()["decision"],
        "block"
    );
    let command = root_command(session);
    assert_eq!(
        handle_hook_for_runtime_at(
            &command,
            root,
            "runtime-a",
            1_000 + UNAVAILABLE_STATUS_GRACE_MILLIS - 1
        )
        .unwrap()["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
    assert_eq!(
        handle_hook_for_runtime_at(
            &command,
            root,
            "runtime-a",
            1_000 + UNAVAILABLE_STATUS_GRACE_MILLIS
        )
        .unwrap(),
        json!({})
    );

    let mut child = root_command(session);
    child.agent_id = Some("/root/reader".into());
    child.agent_type = Some("codey_quick_scan".into());
    child.tool_name = Some("mcp__codey_fastctx__inspect_local_file".into());
    attest_test_child(&child, root, "runtime-a");
    assert_eq!(
        handle_hook_for_runtime_at(&child, root, "runtime-a", 32_000).unwrap()["hookSpecificOutput"]
            ["permissionDecision"],
        "deny"
    );

    let mut late = input("PostToolUse", session);
    late.tool_name = Some("agents.agent_status".into());
    late.tool_input = Some(json!({"target": "/root/reader"}));
    late.tool_response = Some(json!({"agent_id": "/root/reader", "status": "running"}));
    assert_eq!(
        handle_hook_for_runtime_at(&late, root, "runtime-a", 33_000).unwrap(),
        json!({})
    );
    assert_eq!(
        active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
        0
    );
}

#[test]
fn working_status_tool_cancels_unavailability_recovery() {
    for tool in [
        "agents.wait_agent",
        "agents.list_agents",
        "agents.agent_status",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session = "working-fallback";
        start_recovery_child(root, session);
        handle_hook_for_runtime_at(&unavailable_wait(session), root, "runtime-a", 1_000).unwrap();
        let mut status = input("PostToolUse", session);
        status.tool_name = Some(tool.into());
        status.tool_input = Some(if tool == "agents.agent_status" {
            json!({"target": "/root/reader"})
        } else {
            json!({})
        });
        status.tool_response = Some(if tool == "agents.list_agents" {
            json!({"agents": [{"agent_id": "/root/reader", "agent_status": "running"}]})
        } else {
            json!({"agent_id": "/root/reader", "status": "running"})
        });
        handle_hook_for_runtime_at(&status, root, "runtime-a", 2_000).unwrap();
        assert_eq!(
            handle_hook_for_runtime_at(&root_command(session), root, "runtime-a", 31_000).unwrap()
                ["hookSpecificOutput"]["permissionDecision"],
            "deny",
            "{tool}"
        );
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
            1
        );
    }
}

#[test]
fn single_agent_status_is_anonymous_reconciliation_and_settles_only_its_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let session = "single-status";
    start_recovery_child(root, session);
    start_recovery_child_named(root, session, "sibling");
    let mut status = input("PreToolUse", session);
    status.tool_name = Some("agents.agent_status".into());
    status.tool_input = Some(json!({"target": "/root/reader"}));
    assert_eq!(
        handle_hook_for_runtime_at(&status, root, "runtime-a", 1_000).unwrap(),
        json!({})
    );
    status.hook_event_name = "PostToolUse".into();
    status.tool_response = Some(json!({"agent_id": "/root/reader", "agent_status": "completed"}));
    assert_eq!(
        handle_hook_for_runtime_at(&status, root, "runtime-a", 1_001).unwrap()["decision"],
        "block"
    );
    assert_eq!(
        active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
        1
    );
    assert!(
        agent_marker_path(
            &session_state_dir(root, session),
            "runtime-a",
            "/root/sibling"
        )
        .exists()
    );
}

#[test]
fn status_tool_unavailability_does_not_come_from_child_payloads() {
    for response in [
        json!({"message": "Please retry later"}),
        json!({"agent_id": "/root/a", "error": "unknown tool agents.wait_agent"}),
        json!({"updates": [{"message": "unknown tool agents.wait_agent"}]}),
        json!({"output": {"error": {"code": "tool_not_found"}}}),
        json!("unknown tool agents.spawn_agent"),
    ] {
        assert!(
            !status_tool_is_unavailable("agents.wait_agent", Some(&response)),
            "{response}"
        );
    }
    for response in [
        json!("unknown tool agents.wait_agent"),
        json!({"error": {"code": "tool_not_registered"}}),
        json!({"message": "agents.wait_agent is not registered"}),
    ] {
        assert!(
            status_tool_is_unavailable("agents.wait_agent", Some(&response)),
            "{response}"
        );
    }
}

#[test]
fn corrupt_ledger_and_marker_recovery_persists_across_later_hooks() {
    for corruption in ["ledger", "marker"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session = "corrupt-recovery";
        start_recovery_child(root, session);
        let dir = session_state_dir(root, session);
        let path = if corruption == "ledger" {
            dir.join("orchestrator-ledger-v1.json")
        } else {
            agent_marker_path(&dir, "runtime-a", "/root/reader")
        };
        fs::write(&path, b"{").unwrap();
        let command = root_command(session);
        assert!(handle_hook_for_runtime_at(&command, root, "runtime-a", 1_000).is_err());
        assert_eq!(
            handle_hook_for_runtime_at(
                &command,
                root,
                "runtime-a",
                1_000 + STATE_ERROR_GRACE_MILLIS
            )
            .unwrap(),
            json!({})
        );
        assert_eq!(
            handle_hook_for_runtime_at(
                &command,
                root,
                "runtime-a",
                1_001 + STATE_ERROR_GRACE_MILLIS
            )
            .unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
            0
        );
        if corruption == "ledger" {
            let quarantines = fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("orchestrator-ledger-v1.corrupt-")
                })
                .count();
            assert_eq!(quarantines, 1);
        }
    }
}

#[test]
fn child_hooks_cannot_release_root_gate_after_status_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let session = "child-recovery";
    start_recovery_child(root, session);
    handle_hook_for_runtime_at(&root_command(session), root, "runtime-a", 1_000).unwrap();
    handle_hook_for_runtime_at(&unavailable_wait(session), root, "runtime-a", 1_000).unwrap();
    for event in ["PreToolUse", "PostToolUse", "UserPromptSubmit", "Stop"] {
        let mut child = input(event, session);
        child.agent_id = Some("/root/reader".into());
        child.agent_type = Some("codey_quick_scan".into());
        handle_hook_for_runtime_at(
            &child,
            root,
            "runtime-a",
            1_000 + UNAVAILABLE_STATUS_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
            1,
            "{event}"
        );
    }
}

#[test]
fn corruption_recovery_cannot_remove_a_newer_runtimes_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let session = "retired-runtime-recovery";
    start_recovery_child(root, session);
    crate::subagent_orchestrator::active_reservation_count(root, "runtime-b", session, 100)
        .unwrap();
    let ledger_path = session_state_dir(root, session).join("orchestrator-ledger-v1.json");
    let expected = fs::read(&ledger_path).unwrap();
    let command = root_command(session);
    for now_ms in [1_000, 1_000 + STATE_ERROR_GRACE_MILLIS] {
        let error = handle_hook_for_runtime_at(&command, root, "runtime-a", now_ms).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("CODEY_SUBAGENT_STALE_RUNTIME_EVENT")
        );
        assert_eq!(fs::read(&ledger_path).unwrap(), expected);
    }
}

#[test]
fn single_target_status_rejects_mismatched_or_mixed_response_identities() {
    for response in [
        json!({"agent_id": "/root/sibling", "status": "completed"}),
        json!({"agents": [
            {"agent_id": "/root/reader", "status": "completed"},
            {"agent_id": "/root/sibling", "status": "completed"}
        ]}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session = "mismatched-status";
        start_recovery_child(root, session);
        start_recovery_child_named(root, session, "sibling");
        let mut status = input("PostToolUse", session);
        status.tool_name = Some("agents.agent_status".into());
        status.tool_input = Some(json!({"target": "/root/reader"}));
        status.tool_response = Some(response);
        assert_eq!(
            handle_hook_for_runtime_at(&status, root, "runtime-a", 1_000).unwrap()["decision"],
            "block"
        );
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
            2
        );
    }
}

fn reader_tool(session: &str, event: &str) -> HookInput {
    let mut child = input(event, session);
    child.agent_id = Some("/root/reader".into());
    child.agent_type = Some("codey_quick_scan".into());
    child.tool_name = Some("mcp__codey_fastctx__inspect_local_file".into());
    child
}

fn child_tool_in_flight_exists(root: &Path, session: &str) -> bool {
    fs::read_dir(session_state_dir(root, session))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("child-tool-in-flight-")
        })
}

#[test]
fn stopping_a_child_clears_its_in_flight_tool_before_status_failure_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let session = "stop-clears-in-flight";
    start_recovery_child(root, session);
    start_recovery_child_named(root, session, "sibling");
    handle_hook_for_runtime_at(&root_command(session), root, "runtime-a", 1_000).unwrap();

    let child = reader_tool(session, "PreToolUse");
    attest_test_child(&child, root, "runtime-a");
    handle_hook_for_runtime_at(&child, root, "runtime-a", 2_000).unwrap();
    assert!(child_tool_in_flight_exists(root, session));
    handle_hook_for_runtime_at(&unavailable_wait(session), root, "runtime-a", 2_000).unwrap();

    let mut stopped = input("SubagentStop", session);
    stopped.agent_id = Some("/root/reader".into());
    stopped.agent_type = Some("codey_quick_scan".into());
    handle_hook_for_runtime_at(&stopped, root, "runtime-a", 3_000).unwrap();
    assert!(!child_tool_in_flight_exists(root, session));
    assert_eq!(
        active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
        1
    );
    assert_eq!(
        handle_hook_for_runtime_at(
            &root_command(session),
            root,
            "runtime-a",
            2_000 + UNAVAILABLE_STATUS_GRACE_MILLIS - 1
        )
        .unwrap()["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
    assert_eq!(
        handle_hook_for_runtime_at(
            &root_command(session),
            root,
            "runtime-a",
            2_000 + UNAVAILABLE_STATUS_GRACE_MILLIS
        )
        .unwrap(),
        json!({})
    );
    assert_eq!(
        active_agent_count_for_runtime(root, "runtime-a", session).unwrap(),
        0
    );
}

#[test]
fn unverifiable_single_status_does_not_open_the_protocol_circuit() {
    for response in [
        json!({"agent_id": "/root/sibling", "status": "completed"}),
        json!({"status": "running"}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session = "unverifiable-status";
        start_recovery_child(root, session);
        start_recovery_child_named(root, session, "sibling");
        let mut status = input("PostToolUse", session);
        status.tool_name = Some("agents.agent_status".into());
        status.tool_input = Some(json!({"target": "/root/reader"}));
        status.tool_response = Some(response.clone());
        handle_hook_for_runtime_at(&status, root, "runtime-a", 1_000).unwrap();
        assert_eq!(
            protocol_issue_reason(root, "runtime-a", session).unwrap(),
            None,
            "{response}"
        );
    }
}

#[test]
fn timed_out_wait_returns_a_compact_continuation_naming_active_tasks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let session = "compact-timeout";
    start_recovery_child(root, session);
    start_recovery_child_named(root, session, "sibling");
    let mut wait = input("PostToolUse", session);
    wait.tool_name = Some("agents.wait_agent".into());
    for response in [
        json!({"message": "Wait timed out.", "timed_out": true}),
        json!("{\"message\":\"Wait timed out.\",\"timed_out\":true}"),
    ] {
        wait.tool_response = Some(response);
        let output = handle_hook_for_runtime_at(&wait, root, "runtime-a", 1_000).unwrap();
        assert_eq!(output["decision"], "block");
        let reason = output["reason"].as_str().unwrap();
        assert!(reason.contains("仍有 2 个子代理"), "{reason}");
        assert!(
            reason.contains("`reader`") && reason.contains("`sibling`"),
            "{reason}"
        );
        assert!(
            reason.contains("直接再次调用 agents.wait_agent"),
            "{reason}"
        );
        assert!(reason.chars().count() < 300, "{}", reason.chars().count());
    }

    let mut stopped = input("SubagentStop", session);
    stopped.agent_id = Some("/root/sibling".into());
    stopped.agent_type = Some("codey_quick_scan".into());
    handle_hook_for_runtime_at(&stopped, root, "runtime-a", 1_100).unwrap();
    wait.tool_response = Some(json!({"message": "Wait timed out.", "timed_out": true}));
    let reason = handle_hook_for_runtime_at(&wait, root, "runtime-a", 1_200).unwrap()["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.contains("仍有 1 个子代理") && reason.contains("`reader`"));
    assert!(!reason.contains("`sibling`"));

    // A completed wait without per-agent detail still gets the full guidance.
    wait.tool_response = Some(json!({"message": "Wait completed.", "timed_out": false}));
    let reason = handle_hook_for_runtime_at(&wait, root, "runtime-a", 1_300).unwrap()["reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(reason.contains("agents.list_agents") && reason.contains("门禁指令到此结束"));
}
