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
            turn_id: Some("turn-a"),
            native_read_only: false,
            tool_name: name,
            tool_input: None,
        },
        20,
    )
}

fn tool_with_native_read_only(
    root: &Path,
    session: &str,
    task: &str,
    name: &str,
    turn_id: &str,
    native_read_only: bool,
) -> Result<Option<String>> {
    authorize_child_tool_with_context(
        root,
        "runtime",
        session,
        ChildToolContext {
            agent_id: task,
            agent_type: None,
            transcript_path: None,
            turn_id: Some(turn_id),
            native_read_only,
            tool_name: name,
            tool_input: None,
        },
        20,
    )
}

fn set_explorer_native_read_only(root: &Path, session: &str, task: &str, turn_id: &str) {
    let store = LedgerStore::open(root, session).unwrap();
    let mut ledger = store.load("runtime", session, 20).unwrap().unwrap();
    let reservation = ledger.reservations.get_mut(task).unwrap();
    let turn_hash = hash_component(turn_id);
    reservation.current_turn_id_hash = Some(turn_hash.clone());
    reservation.native_read_only_turn_hash = Some(turn_hash);
    store.save(&mut ledger, 20).unwrap();
}

fn active_marker_hashes(task: &str) -> BTreeSet<String> {
    BTreeSet::from([hash_component(task)])
}

#[test]
fn stopped_turn_final_usage_is_recorded_before_completion_trace() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let session = "stopped-final-usage";
    spawn(root, session, "worker", "codey_worker", "/repo");
    for (status, input_tokens, at) in [("running", 10, 20), ("completed", 25, 40)] {
        if status == "completed" {
            subagent_stopped_with_context(
                root,
                "runtime",
                session,
                "worker",
                Some("codey_worker"),
                None,
                Some("turn-a"),
                30,
            )
            .unwrap();
        }
        observe_status_response(
            root,
            "runtime",
            session,
            Some(&json!({"agents":[{
                "agent_id":"worker", "turn_id":"turn-a", "status":status,
                "usage":{"input_tokens":input_tokens,"output_tokens":7}
            }]})),
            StatusResponseSource::Snapshot(false),
            at,
        )
        .unwrap();
    }
    let trace = std::fs::read_to_string(crate::subagent::telemetry::trace_file(root)).unwrap();
    let completed = trace
        .lines()
        .map(|line| serde_json::from_str::<SubagentTraceEvent>(line).unwrap())
        .find(|event| event.event == TraceEventKind::Completed)
        .unwrap();
    assert_eq!(completed.usage.unwrap().input_tokens, 25);
}

#[test]
fn followup_snapshot_confirms_plain_text_without_another_start_hook() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let session = "snapshot-text-followup";
    spawn(root, session, "worker", "codey_worker", "/repo");
    observe_status_response(
        root,
        "runtime",
        session,
        Some(&json!({"agents":[{
            "agent_id":"worker", "turn_id":"turn-a", "status":"completed",
            "usage":{"input_tokens":21,"output_tokens":4}
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let followup = json!({"target":"worker","message":"Return plain text"});
    pre_followup_task(root, "runtime", session, Some(&followup), 0, 30, |_| {
        Ok(None)
    })
    .unwrap();
    post_followup_task(
        root,
        "runtime",
        session,
        Some(&followup),
        Some(&json!({})),
        None,
        31,
    )
    .unwrap();
    let final_status = json!({"agents":[{"agent_id":"worker","status":"completed"}]});
    let notification = observe_status_response(
        root,
        "runtime",
        session,
        Some(&final_status),
        StatusResponseSource::Notification,
        32,
    )
    .unwrap();
    assert!(notification.needs_snapshot);
    assert!(notification.settled_agent_hashes.is_empty());
    observe_status_response(
        root,
        "runtime",
        session,
        Some(&final_status),
        StatusResponseSource::Snapshot(false),
        33,
    )
    .unwrap();
    let ledger = LedgerStore::open(root, session)
        .unwrap()
        .load("runtime", session, 34)
        .unwrap()
        .unwrap();
    let reservation = &ledger.reservations["worker"];
    assert_eq!(reservation.outcome, ExecutionOutcome::Succeeded);
    assert_eq!(reservation.state, ReservationState::Terminal);
    assert!(reservation.current_turn_id_hash.is_none());
    assert!(reservation.token_usage.is_none());
    assert_eq!(
        reservation.usage_by_turn[&hash_component("turn-a")].input_tokens,
        21
    );
}

#[test]
fn followup_snapshot_cannot_supply_new_completion_with_old_usage() {
    for source in [
        StatusResponseSource::Notification,
        StatusResponseSource::Snapshot(false),
    ] {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let session = "snapshot-old-usage";
        spawn(root, session, "worker", "codey_worker", "/repo");
        observe_status_response(
            root,
            "runtime",
            session,
            Some(&json!({"agents":[{
                "agent_id":"worker", "turn_id":"turn-a", "status":"completed",
                "usage":{"input_tokens":21,"output_tokens":4}
            }]})),
            StatusResponseSource::Snapshot(false),
            20,
        )
        .unwrap();
        let followup = json!({"target":"worker","message":"Return plain text"});
        pre_followup_task(root, "runtime", session, Some(&followup), 0, 30, |_| {
            Ok(None)
        })
        .unwrap();
        post_followup_task(
            root,
            "runtime",
            session,
            Some(&followup),
            Some(&json!({})),
            None,
            31,
        )
        .unwrap();
        observe_status_response(
            root,
            "runtime",
            session,
            Some(&json!({"agents":[{
            "agent_id":"worker", "turn_id":"turn-a", "status":"completed",
            "usage":{"input_tokens":21,"output_tokens":4}
        }, {"agent_id":"worker","turn_id":"turn-b","status":"completed"}]})),
            source,
            32,
        )
        .unwrap();
        let ledger = LedgerStore::open(root, session)
            .unwrap()
            .load("runtime", session, 33)
            .unwrap()
            .unwrap();
        let reservation = &ledger.reservations["worker"];
        assert_eq!(reservation.outcome, ExecutionOutcome::Succeeded);
        assert_eq!(
            reservation.current_turn_id_hash,
            Some(hash_component("turn-b"))
        );
        assert!(reservation.token_usage.is_none());
        assert_eq!(
            reservation.usage_by_turn[&hash_component("turn-a")].input_tokens,
            21
        );
    }
}

#[test]
fn scoped_pure_text_stop_settles_followup_before_or_after_ack() {
    for ack_first in [false, true] {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let session = "fast-followup";
        spawn(root, session, "worker", "codey_worker", "/repo");
        assert_eq!(
            tool_with_native_read_only(root, session, "worker", "grep", "turn-a", false).unwrap(),
            None
        );
        observe_status_response(
            root,
            "runtime",
            session,
            Some(
                &json!({"agents":[{"agent_id":"worker","turn_id":"turn-a","status":"completed"}]}),
            ),
            StatusResponseSource::Snapshot(false),
            21,
        )
        .unwrap();
        let followup = json!({"target":"worker","message":"Return a short explanation"});
        assert_eq!(
            pre_followup_task(root, "runtime", session, Some(&followup), 0, 30, |_| Ok(
                None
            ))
            .unwrap(),
            FollowupAdmission::ResumeRequested
        );
        if ack_first {
            post_followup_task(
                root,
                "runtime",
                session,
                Some(&followup),
                Some(&json!({})),
                Some("root-turn-b"),
                31,
            )
            .unwrap();
        }
        assert!(
            !subagent_stopped_with_context(
                root,
                "runtime",
                session,
                "worker",
                Some("codey_worker"),
                None,
                Some("turn-a"),
                32
            )
            .unwrap()
        );
        assert!(
            subagent_stopped_with_context(
                root,
                "runtime",
                session,
                "worker",
                Some("codey_worker"),
                None,
                Some("turn-b"),
                33
            )
            .unwrap()
        );
        if !ack_first {
            post_followup_task(
                root,
                "runtime",
                session,
                Some(&followup),
                Some(&json!({})),
                Some("root-turn-b"),
                34,
            )
            .unwrap();
        }
        observe_status_response(
            root,
            "runtime",
            session,
            Some(&json!({"agents":[{"agent_id":"worker","status":"completed"}]})),
            StatusResponseSource::Snapshot(false),
            35,
        )
        .unwrap();
        let ledger = LedgerStore::open(root, session)
            .unwrap()
            .load("runtime", session, 36)
            .unwrap()
            .unwrap();
        let reservation = &ledger.reservations["worker"];
        assert_eq!(reservation.state, ReservationState::Terminal);
        assert_eq!(reservation.outcome, ExecutionOutcome::Succeeded);
        assert_eq!(
            reservation.current_turn_id_hash,
            Some(hash_component("turn-b"))
        );
        assert!(reservation.token_usage.is_none());
        assert_eq!(
            active_reservation_count(root, "runtime", session, 36).unwrap(),
            Some(0)
        );
    }
}

#[test]
fn live_usage_updates_the_same_turn_without_duplicating_it() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "usage-session",
        "worker",
        "codey_worker",
        "/repo",
    );
    for (timestamp, input_tokens, state) in [(20, 10, "running"), (30, 25, "completed")] {
        observe_status_response(
            temp.path(),
            "runtime",
            "usage-session",
            Some(&json!({"agents": [{
                "agent_id": "worker", "turn_id": "turn-a", "status": state,
                "usage": {"input_tokens": input_tokens, "output_tokens": 3}
            }]})),
            StatusResponseSource::Snapshot(false),
            timestamp,
        )
        .unwrap();
    }
    let ledger = LedgerStore::open(temp.path(), "usage-session")
        .unwrap()
        .load("runtime", "usage-session", 31)
        .unwrap()
        .unwrap();
    let reservation = &ledger.reservations["worker"];
    assert_eq!(reservation.usage_by_turn.len(), 1);
    assert_eq!(
        reservation.usage_by_turn[&hash_component("turn-a")].input_tokens,
        25
    );
    assert_eq!(reservation.token_usage.as_ref().unwrap().input_tokens, 25);
}

#[test]
fn followup_ignores_late_usage_from_the_previous_turn() {
    let temp = tempdir().unwrap();
    let session = "followup-usage-boundary";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "running",
            "usage": {"input_tokens": 10, "output_tokens": 4}
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed",
            "usage": {"input_tokens": 25, "output_tokens": 7}
        }]})),
        StatusResponseSource::Snapshot(false),
        30,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "running",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        }]})),
        StatusResponseSource::Snapshot(false),
        31,
    )
    .unwrap();
    let completed_a = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 32)
        .unwrap()
        .unwrap();
    let completed_a = &completed_a.reservations["worker"];
    assert_eq!(completed_a.state, ReservationState::Terminal);
    assert_eq!(
        completed_a.current_turn_id_hash.as_deref(),
        Some(hash_component("turn-a").as_str())
    );
    assert_eq!(
        (
            completed_a.usage_by_turn[&hash_component("turn-a")].input_tokens,
            completed_a.usage_by_turn[&hash_component("turn-a")].output_tokens
        ),
        (25, 7)
    );
    let followup = json!({"target":"worker", "message":"Continue with turn B"});
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            session,
            Some(&followup),
            0,
            40,
            |_| Ok(None)
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    post_followup_task(
        temp.path(),
        "runtime",
        session,
        Some(&followup),
        Some(&json!({})),
        None,
        41,
    )
    .unwrap();

    for (timestamp, state, input_tokens) in [(42, "running", 10), (43, "completed", 11)] {
        observe_status_response(
            temp.path(),
            "runtime",
            session,
            Some(&json!({"agents": [{
                "agent_id": "worker", "turn_id": "turn-a", "status": state,
                "usage": {"input_tokens": input_tokens, "output_tokens": 3}
            }]})),
            StatusResponseSource::Snapshot(false),
            timestamp,
        )
        .unwrap();
    }
    let queued = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 44)
        .unwrap()
        .unwrap();
    let reservation = &queued.reservations["worker"];
    assert_eq!(reservation.state, ReservationState::Running);
    assert_eq!(
        reservation.current_turn_id_hash.as_deref(),
        Some(hash_component("turn-a").as_str())
    );
    assert!(
        reservation
            .retired_turn_id_hashes
            .contains(hash_component("turn-a").as_str())
    );
    assert_eq!(reservation.token_usage, None);
    assert_eq!(
        (
            reservation.usage_by_turn[&hash_component("turn-a")].input_tokens,
            reservation.usage_by_turn[&hash_component("turn-a")].output_tokens
        ),
        (25, 7)
    );

    assert_eq!(
        authorize_child_tool_with_context(
            temp.path(),
            "runtime",
            session,
            ChildToolContext {
                agent_id: "worker",
                agent_type: Some("codey_worker"),
                transcript_path: None,
                turn_id: Some("turn-b"),
                native_read_only: false,
                tool_name: "grep",
                tool_input: None,
            },
            50,
        )
        .unwrap(),
        None
    );
    for (timestamp, input_tokens, state) in [(51, 30, "running"), (52, 34, "completed")] {
        observe_status_response(
            temp.path(),
            "runtime",
            session,
            Some(&json!({"agents": [{
                "agent_id": "worker", "turn_id": "turn-b", "status": state,
                "usage": {"input_tokens": input_tokens, "output_tokens": 8}
            }]})),
            StatusResponseSource::Snapshot(false),
            timestamp,
        )
        .unwrap();
    }
    let completed = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 53)
        .unwrap()
        .unwrap();
    let reservation = &completed.reservations["worker"];
    assert_eq!(reservation.usage_by_turn.len(), 2);
    assert!(
        reservation
            .retired_turn_id_hashes
            .contains(hash_component("turn-a").as_str())
    );
    assert_eq!(
        (
            reservation.usage_by_turn[&hash_component("turn-a")].input_tokens,
            reservation.usage_by_turn[&hash_component("turn-a")].output_tokens
        ),
        (25, 7)
    );
    assert_eq!(
        reservation.usage_by_turn[&hash_component("turn-b")].input_tokens,
        34
    );
    assert_eq!(reservation.token_usage.as_ref().unwrap().input_tokens, 34);
}

#[test]
fn repeated_status_keeps_latest_complete_usage_fields() {
    let temp = tempdir().unwrap();
    let session = "usage-all-fields";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    for (timestamp, cached_tokens, reasoning_tokens) in [(20, 1, 0), (21, 2, 3)] {
        observe_status_response(
            temp.path(),
            "runtime",
            session,
            Some(&json!({"agents": [{
                "agent_id": "worker", "turn_id": "turn-a", "status": "running",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 4,
                    "cached_tokens": cached_tokens,
                    "reasoning_tokens": reasoning_tokens
                }
            }]})),
            StatusResponseSource::Snapshot(false),
            timestamp,
        )
        .unwrap();
    }
    let ledger = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 22)
        .unwrap()
        .unwrap();
    let usage = &ledger.reservations["worker"].usage_by_turn[&hash_component("turn-a")];
    assert_eq!((usage.cached_tokens, usage.reasoning_tokens), (2, 3));
}

#[test]
fn live_usage_can_refine_a_first_turn_that_stopped_before_turn_context() {
    let temp = tempdir().unwrap();
    let session = "first-turn-stop-refinement";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    subagent_stopped_with_context(
        temp.path(),
        "runtime",
        session,
        "worker",
        Some("codey_worker"),
        None,
        None,
        12,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "running",
            "usage": {"input_tokens": 21, "output_tokens": 4}
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let ledger = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 21)
        .unwrap()
        .unwrap();
    let reservation = &ledger.reservations["worker"];
    assert_eq!(
        reservation.current_turn_id_hash.as_deref(),
        Some(hash_component("turn-a").as_str())
    );
    assert_eq!(reservation.token_usage.as_ref().unwrap().input_tokens, 21);
}

#[test]
fn first_turn_terminal_observation_can_establish_its_turn() {
    let temp = tempdir().unwrap();
    let session = "atomic-first-turn-terminal";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    subagent_stopped_with_context(
        temp.path(),
        "runtime",
        session,
        "worker",
        Some("codey_worker"),
        None,
        None,
        12,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let ledger = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 21)
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.reservations["worker"]
            .current_turn_id_hash
            .as_deref(),
        Some(hash_component("turn-a").as_str())
    );
}

#[test]
fn followup_rejects_prior_terminal_until_the_new_turn_is_established() {
    let temp = tempdir().unwrap();
    let session = "followup-terminal-boundary";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    subagent_stopped_with_context(
        temp.path(),
        "runtime",
        session,
        "worker",
        Some("codey_worker"),
        None,
        None,
        12,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed",
            "usage": {"input_tokens": 9, "output_tokens": 2}
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let followup = json!({"target":"worker", "message":"Continue with turn B"});
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            session,
            Some(&followup),
            0,
            30,
            |_| Ok(None)
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    post_followup_task(
        temp.path(),
        "runtime",
        session,
        Some(&followup),
        Some(&json!({})),
        None,
        31,
    )
    .unwrap();

    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        32,
    )
    .unwrap();
    let still_running = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 33)
        .unwrap()
        .unwrap();
    assert_eq!(
        still_running.reservations["worker"].state,
        ReservationState::Running
    );

    assert_eq!(
        authorize_child_tool_with_context(
            temp.path(),
            "runtime",
            session,
            ChildToolContext {
                agent_id: "worker",
                agent_type: Some("codey_worker"),
                transcript_path: None,
                turn_id: Some("turn-b"),
                native_read_only: false,
                tool_name: "grep",
                tool_input: None,
            },
            40,
        )
        .unwrap(),
        None
    );
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "status": "completed", "message": "turn B completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        41,
    )
    .unwrap();
    let completed = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 42)
        .unwrap()
        .unwrap();
    assert_eq!(
        completed.reservations["worker"].state,
        ReservationState::Terminal
    );
    assert_eq!(
        completed.reservations["worker"].outcome,
        ExecutionOutcome::Succeeded
    );
    assert_eq!(
        completed.reservations["worker"]
            .current_turn_id_hash
            .as_deref(),
        Some(hash_component("turn-b").as_str())
    );
}

#[test]
fn anonymous_stop_and_completion_cannot_finish_an_unresolved_followup() {
    let temp = tempdir().unwrap();
    let session = "followup-anonymous-terminal";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    subagent_stopped_with_context(
        temp.path(),
        "runtime",
        session,
        "worker",
        Some("codey_worker"),
        None,
        None,
        12,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let followup = json!({"target":"worker", "message":"Start turn B"});
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            session,
            Some(&followup),
            0,
            30,
            |_| Ok(None)
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    post_followup_task(
        temp.path(),
        "runtime",
        session,
        Some(&followup),
        Some(&json!({})),
        None,
        31,
    )
    .unwrap();

    assert!(
        settle_unique_anonymous_stop(temp.path(), "runtime", session, None, 32)
            .unwrap()
            .is_none()
    );
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "status": "completed"
        }]})),
        StatusResponseSource::Notification,
        33,
    )
    .unwrap();
    let pending = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 34)
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.reservations["worker"].state,
        ReservationState::Running
    );

    assert!(
        settle_unique_anonymous_stop(temp.path(), "runtime", session, None, 35)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        subagent_started_with_context(
            temp.path(),
            "runtime",
            session,
            "worker",
            Some("codey_worker"),
            None,
            Some("turn-b"),
            36,
        )
        .unwrap(),
        true
    );
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "status": "completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        37,
    )
    .unwrap();
    let completed = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 38)
        .unwrap()
        .unwrap();
    assert_eq!(
        completed.reservations["worker"].state,
        ReservationState::Terminal
    );
    assert_eq!(
        completed.reservations["worker"]
            .current_turn_id_hash
            .as_deref(),
        Some(hash_component("turn-b").as_str())
    );
}

#[test]
fn first_turn_text_completion_still_settles_after_a_followup() {
    let temp = tempdir().unwrap();
    let session = "followup-text-completion";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let followup = json!({"target":"worker", "message":"Continue with turn B"});
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            session,
            Some(&followup),
            0,
            30,
            |_| Ok(None)
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    post_followup_task(
        temp.path(),
        "runtime",
        session,
        Some(&followup),
        Some(&json!({})),
        None,
        31,
    )
    .unwrap();
    assert_eq!(
        subagent_started_with_context(
            temp.path(),
            "runtime",
            session,
            "worker",
            Some("codey_worker"),
            None,
            Some("turn-b"),
            32,
        )
        .unwrap(),
        true
    );
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "status": "completed", "message": "plain text result"
        }]})),
        StatusResponseSource::Snapshot(false),
        33,
    )
    .unwrap();
    let ledger = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 34)
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.reservations["worker"].state,
        ReservationState::Terminal
    );
    assert_eq!(
        ledger.reservations["worker"]
            .current_turn_id_hash
            .as_deref(),
        Some(hash_component("turn-b").as_str())
    );
}

#[test]
fn late_explicit_stop_from_the_old_turn_cannot_finish_followup() {
    let temp = tempdir().unwrap();
    let session = "followup-stop-boundary";
    spawn(temp.path(), session, "worker", "codey_worker", "/repo");
    subagent_stopped_with_context(
        temp.path(),
        "runtime",
        session,
        "worker",
        Some("codey_worker"),
        None,
        None,
        12,
    )
    .unwrap();
    observe_status_response(
        temp.path(),
        "runtime",
        session,
        Some(&json!({"agents": [{
            "agent_id": "worker", "turn_id": "turn-a", "status": "completed"
        }]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let followup = json!({"target":"worker", "message":"Start turn B"});
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            session,
            Some(&followup),
            0,
            30,
            |_| Ok(None)
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    post_followup_task(
        temp.path(),
        "runtime",
        session,
        Some(&followup),
        Some(&json!({})),
        None,
        31,
    )
    .unwrap();

    assert_eq!(
        subagent_stopped_with_context(
            temp.path(),
            "runtime",
            session,
            "worker",
            Some("codey_worker"),
            None,
            Some("turn-a"),
            32,
        )
        .unwrap(),
        false
    );
    assert_eq!(
        subagent_started_with_context(
            temp.path(),
            "runtime",
            session,
            "worker",
            Some("codey_worker"),
            None,
            Some("turn-b"),
            33,
        )
        .unwrap(),
        true
    );
    assert_eq!(
        subagent_stopped_with_context(
            temp.path(),
            "runtime",
            session,
            "worker",
            Some("codey_worker"),
            None,
            Some("turn-a"),
            34,
        )
        .unwrap(),
        false
    );
    let running = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 35)
        .unwrap()
        .unwrap();
    assert_eq!(
        running.reservations["worker"].state,
        ReservationState::Running
    );
    assert_eq!(
        subagent_stopped_with_context(
            temp.path(),
            "runtime",
            session,
            "worker",
            Some("codey_worker"),
            None,
            Some("turn-b"),
            36,
        )
        .unwrap(),
        true
    );
    let terminal = LedgerStore::open(temp.path(), session)
        .unwrap()
        .load("runtime", session, 37)
        .unwrap()
        .unwrap();
    assert_eq!(
        terminal.reservations["worker"].state,
        ReservationState::Terminal
    );
}

#[test]
fn child_tool_calls_do_not_repeat_resume_events_or_reset_start_time() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "resume-session",
        "worker",
        "codey_worker",
        "/repo",
    );
    let invoke = |turn_id, now_ms| {
        authorize_child_tool_with_context(
            temp.path(),
            "runtime",
            "resume-session",
            ChildToolContext {
                agent_id: "worker",
                agent_type: Some("codey_worker"),
                transcript_path: None,
                turn_id: Some(turn_id),
                native_read_only: false,
                tool_name: "grep",
                tool_input: None,
            },
            now_ms,
        )
        .unwrap()
    };
    assert_eq!(invoke("turn-a", 12), None);
    assert_eq!(invoke("turn-a", 13), None);
    let events = || {
        std::fs::read_to_string(crate::subagent::telemetry::trace_file(temp.path()))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<SubagentTraceEvent>(line).unwrap())
            .collect::<Vec<_>>()
    };
    assert!(
        !events()
            .iter()
            .any(|event| event.event == TraceEventKind::Resumed)
    );
    observe_status_response(
        temp.path(),
        "runtime",
        "resume-session",
        Some(&json!({"agents": [{"agent_id":"worker", "status":"completed"}]})),
        StatusResponseSource::Snapshot(false),
        20,
    )
    .unwrap();
    let followup = json!({"target":"worker", "message":"Continue the same task"});
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            "resume-session",
            Some(&followup),
            0,
            30,
            |_| Ok(None)
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    post_followup_task(
        temp.path(),
        "runtime",
        "resume-session",
        Some(&followup),
        Some(&json!({})),
        Some("root-turn-b"),
        31,
    )
    .unwrap();
    let queued = events()
        .into_iter()
        .find(|event| event.attributes.get("followup.admitted") == Some(&json!(true)))
        .unwrap();
    assert!(!queued.attributes.contains_key("duration.execution_ms"));
    assert_eq!(queued.attributes.get("duration.queue_ms"), Some(&json!(0)));
    assert_eq!(invoke("turn-b", 40), None);
    let count = events()
        .iter()
        .filter(|event| event.event == TraceEventKind::Resumed)
        .count();
    assert_eq!(invoke("turn-b", 45), None);
    assert_eq!(
        events()
            .iter()
            .filter(|event| event.event == TraceEventKind::Resumed)
            .count(),
        count
    );
    let ledger = LedgerStore::open(temp.path(), "resume-session")
        .unwrap()
        .load("runtime", "resume-session", 46)
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.reservations["worker"].current_turn_started_at_ms,
        Some(40)
    );
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
        "codey_reviewer",
        "/repo/src",
    );
    for name in [
        "mcp__codey_fastctx__inspect_local_file",
        "mcp__codey_fastctx__glob",
        "view_image",
        "functions.exec",
        "functions.exec_command",
        "functions.write_stdin",
        "grep",
    ] {
        assert_eq!(
            tool(temp.path(), "review-session", "review", name).unwrap(),
            None,
            "{name}"
        );
    }
    for name in ["apply_patch"] {
        assert!(
            tool(temp.path(), "review-session", "review", name)
                .unwrap()
                .is_some(),
            "{name}"
        );
    }
    assert_eq!(
        tool(temp.path(), "review-session", "review", "mcp__cua_repl__js").unwrap(),
        None
    );
    assert_eq!(
        tool(
            temp.path(),
            "review-session",
            "review",
            "mcp__unknown__read"
        )
        .unwrap(),
        None
    );
}

#[test]
fn explorer_commands_require_current_native_read_only_and_wrappers_stay_denied() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "explorer-session",
        "explorer",
        "codey_explorer",
        "/repo",
    );
    let denied = tool_with_native_read_only(
        temp.path(),
        "explorer-session",
        "explorer",
        "exec_command",
        "child-turn-a",
        false,
    )
    .unwrap()
    .unwrap();
    assert!(denied.contains("原生 rollout 证明只读权限"));

    for name in [
        "functions.exec",
        "mcp__cua_repl__js",
        "mcp__cua_repl__js_reset",
        "mcp__codex_app__open_in_codex",
        "mcp__unknown__query",
    ] {
        assert!(
            tool_with_native_read_only(
                temp.path(),
                "explorer-session",
                "explorer",
                name,
                "child-turn-a",
                true,
            )
            .unwrap()
            .is_some(),
            "{name}"
        );
    }
    for name in [
        "inspect_local_file",
        "grep",
        "glob",
        "view_image",
        "web.run",
    ] {
        assert_eq!(
            tool_with_native_read_only(
                temp.path(),
                "explorer-session",
                "explorer",
                name,
                "child-turn-a",
                true,
            )
            .unwrap(),
            None,
            "{name}"
        );
    }
    assert_eq!(
        tool_with_native_read_only(
            temp.path(),
            "explorer-session",
            "explorer",
            "exec_command",
            "child-turn-a",
            true,
        )
        .unwrap(),
        None
    );
    assert!(
        tool_with_native_read_only(
            temp.path(),
            "explorer-session",
            "explorer",
            "exec_command",
            "child-turn-b",
            false,
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn root_write_requires_every_explorer_to_prove_the_current_read_only_turn() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "explorer-session",
        "explorer_a",
        "codey_explorer",
        "/repo",
    );
    let markers = active_marker_hashes("explorer_a");
    assert!(
        !active_attempts_are_read_only(temp.path(), "runtime", "explorer-session", &markers, 20,)
            .unwrap()
    );

    set_explorer_native_read_only(
        temp.path(),
        "explorer-session",
        "explorer_a",
        "child-turn-a",
    );
    assert!(
        active_attempts_are_read_only(temp.path(), "runtime", "explorer-session", &markers, 21,)
            .unwrap()
    );
    assert!(
        !active_attempts_are_read_only(
            temp.path(),
            "runtime",
            "explorer-session",
            &BTreeSet::new(),
            22,
        )
        .unwrap()
    );

    tool_with_native_read_only(
        temp.path(),
        "explorer-session",
        "explorer_a",
        "exec_command",
        "child-turn-b",
        false,
    )
    .unwrap();
    assert!(
        !active_attempts_are_read_only(temp.path(), "runtime", "explorer-session", &markers, 23,)
            .unwrap()
    );
}

#[test]
fn reviewer_can_run_commands_but_cannot_edit_source() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "review-session",
        "review",
        "codey_reviewer",
        "/repo",
    );
    for name in ["exec_command", "functions.exec_command"] {
        assert_eq!(
            tool(temp.path(), "review-session", "review", name).unwrap(),
            None,
            "{name}"
        );
    }
    for name in ["apply_patch", "mcp__codey_fastctx__replace"] {
        assert!(
            tool(temp.path(), "review-session", "review", name)
                .unwrap()
                .is_some_and(|reason| reason.contains("Reviewer 不得直接修改源码")),
            "{name}"
        );
    }
}

#[test]
fn interrupted_explorer_and_followup_require_a_new_native_proof() {
    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "explorer-session",
        "explorer",
        "codey_explorer",
        "/repo",
    );
    set_explorer_native_read_only(temp.path(), "explorer-session", "explorer", "child-turn-a");
    pre_interrupt_agent(
        temp.path(),
        "runtime",
        "explorer-session",
        Some(&json!({"target":"explorer"})),
        21,
    )
    .unwrap();
    assert!(
        tool_with_native_read_only(
            temp.path(),
            "explorer-session",
            "explorer",
            "exec_command",
            "child-turn-a",
            true,
        )
        .unwrap()
        .is_some()
    );

    let temp = tempdir().unwrap();
    spawn(
        temp.path(),
        "followup-session",
        "explorer",
        "codey_explorer",
        "/repo",
    );
    set_explorer_native_read_only(temp.path(), "followup-session", "explorer", "child-turn-a");
    let wait = json!({
        "agents": [{
            "agent_id": "explorer",
            "status": "completed",
            "turn_id": "child-turn-a",
            "usage": {"input_tokens": 2, "output_tokens": 1}
        }]
    });
    observe_status_response(
        temp.path(),
        "runtime",
        "followup-session",
        Some(&wait),
        StatusResponseSource::Snapshot(true),
        22,
    )
    .unwrap();
    assert_eq!(
        pre_followup_task(
            temp.path(),
            "runtime",
            "followup-session",
            Some(&json!({"target":"explorer","message":"continue"})),
            0,
            23,
            |_| Ok(None),
        )
        .unwrap(),
        FollowupAdmission::ResumeRequested
    );
    assert!(
        tool_with_native_read_only(
            temp.path(),
            "followup-session",
            "explorer",
            "exec_command",
            "child-turn-b",
            false,
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn same_session_review_and_writer_obey_concurrency() {
    let temp = tempdir().unwrap();
    spawn(temp.path(), "session", "writer", "codey_worker", "/repo");
    spawn(temp.path(), "session", "review", "codey_explorer", "/repo");
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
    assert!(
        tool(temp.path(), "session", "review", "apply_patch")
            .unwrap()
            .is_some_and(|reason| reason.contains("不具备写入权限"))
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
    spawn(temp.path(), "session", "review", "codey_explorer", "/repo");
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
