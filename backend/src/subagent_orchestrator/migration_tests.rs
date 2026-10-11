use super::*;
use serde_json::json;
use tempfile::tempdir;

fn legacy_ledger(root: &Path, version: u32, writable: bool) -> PathBuf {
    let spawn = json!({"task_name": "task_a", "agent_type": "codey_worker", "message": "Work"});
    assert_eq!(
        pre_spawn(root, "runtime", "session", Some(&spawn), 0, 10).unwrap(),
        None
    );
    post_spawn(
        root,
        "runtime",
        "session",
        Some(&spawn),
        Some(&json!({"agent_id": "agent-a"})),
        11,
    )
    .unwrap();
    subagent_started_with_context(
        root,
        "runtime",
        "session",
        "agent-a",
        Some("codey_worker"),
        None,
        12,
    )
    .unwrap();
    let path = root.join(hash_component("session")).join(LEDGER_FILE);
    let mut ledger: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ledger["schema_version"] = json!(version);
    ledger["reservations"]["task_a"]["write_capable"] = json!(writable);
    ledger["reservations"]["task_a"]
        .as_object_mut()
        .unwrap()
        .remove("workspace_write_claimed");
    // v14 fields removed by v15 must not prevent migration of live attempts.
    ledger["batch_number"] = json!(1);
    ledger["reservations"]["task_a"]["acceptance"] = json!([]);
    fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
    path
}

#[test]
fn supported_legacy_ledgers_preserve_active_identity_and_workspace_reservation() {
    for version in [14, 15, 16] {
        let temp = tempdir().unwrap();
        let path = legacy_ledger(temp.path(), version, true);
        let before: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            active_reservation_count(temp.path(), "runtime", "session", 20).unwrap(),
            Some(1)
        );
        let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["schema_version"], json!(LEDGER_SCHEMA_VERSION));
        assert_eq!(
            after["reservations"]["task_a"]["workspace_write_claimed"],
            json!(true)
        );
        for field in [
            "attempt_id",
            "fencing_token",
            "origin_runtime_id_hash",
            "runtime_generation",
            "agent_id_hash",
            "state",
            "capabilities",
        ] {
            assert_eq!(
                after["reservations"]["task_a"][field], before["reservations"]["task_a"][field],
                "{field}"
            );
        }
        assert!(
            after["reservations"]["task_a"]
                .get("write_capable")
                .is_none()
        );
        let second = json!({"task_name":"task_b","agent_type":"codey_worker","message":"Work"});
        assert_eq!(
            pre_spawn(temp.path(), "runtime", "other", Some(&second), 0, 21).unwrap(),
            None
        );
        post_spawn(
            temp.path(),
            "runtime",
            "other",
            Some(&second),
            Some(&json!({"agent_id":"agent-b"})),
            21,
        )
        .unwrap();
        let attempt = || ChildToolContext {
            agent_id: "agent-b",
            agent_type: None,
            transcript_path: None,
            tool_name: "apply_patch",
            tool_input: None,
        };
        assert!(
            authorize_child_tool_with_context(temp.path(), "runtime", "other", attempt(), 22)
                .unwrap()
                .unwrap()
                .contains("CODEY_SUBAGENT_WORKSPACE_BUSY")
        );
        subagent_stopped(temp.path(), "runtime", "session", "agent-a", 22).unwrap();
        assert_eq!(
            authorize_child_tool_with_context(temp.path(), "runtime", "other", attempt(), 23)
                .unwrap(),
            None
        );
    }
}

#[test]
fn migration_never_expands_legacy_permissions() {
    let temp = tempdir().unwrap();
    let path = legacy_ledger(temp.path(), 15, false);
    assert_eq!(
        active_reservation_count(temp.path(), "runtime", "session", 20).unwrap(),
        Some(1)
    );
    let ledger: SessionLedger = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(!reservation_declares_write(&ledger.reservations["task_a"]));
    let denied = authorize_child_tool_with_context(
        temp.path(),
        "runtime",
        "session",
        ChildToolContext {
            agent_id: "agent-a",
            agent_type: Some("codey_worker"),
            transcript_path: None,
            tool_name: "apply_patch",
            tool_input: None,
        },
        21,
    )
    .unwrap();
    assert!(denied.is_some());
}

#[test]
fn v17_migration_preserves_both_unclaimed_readers_and_workspace_writers() {
    for writable in [false, true] {
        let temp = tempdir().unwrap();
        let path = legacy_ledger(temp.path(), 17, true);
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let reservation = value["reservations"]["task_a"].as_object_mut().unwrap();
        reservation.insert("workspace_write_claimed".into(), json!(writable));
        reservation.remove("file_write_claims");
        reservation.remove("unbounded_write_claimed");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        active_reservation_count(temp.path(), "runtime", "session", 20).unwrap();
        let after: SessionLedger = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            after.reservations["task_a"].workspace_write_claimed,
            writable
        );
        assert!(!after.reservations["task_a"].unbounded_write_claimed);
        assert!(after.reservations["task_a"].file_write_claims.is_empty());
        assert_eq!(after.schema_version, LEDGER_SCHEMA_VERSION);
    }
}

#[test]
fn v18_missing_or_invalid_file_claims_are_rejected_without_overwriting_the_ledger() {
    for invalid in [
        None,
        Some(json!(["relative.rs"])),
        Some(json!(["/root/../file"])),
    ] {
        let temp = tempdir().unwrap();
        let path = legacy_ledger(temp.path(), LEDGER_SCHEMA_VERSION, true);
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let reservation = value["reservations"]["task_a"].as_object_mut().unwrap();
        if let Some(invalid) = invalid {
            reservation.insert("file_write_claims".into(), invalid);
        } else {
            reservation.remove("file_write_claims");
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(active_reservation_count(temp.path(), "runtime", "session", 20).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn unsupported_or_incomplete_legacy_ledger_is_not_silently_rewritten() {
    for version in [13, LEDGER_SCHEMA_VERSION + 1] {
        let temp = tempdir().unwrap();
        let path = legacy_ledger(temp.path(), version, true);
        let before = fs::read(&path).unwrap();
        assert!(active_reservation_count(temp.path(), "runtime", "session", 20).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    let temp = tempdir().unwrap();
    let path = legacy_ledger(temp.path(), 14, true);
    let mut ledger: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ledger["reservations"]["task_a"]
        .as_object_mut()
        .unwrap()
        .remove("origin_runtime_id_hash");
    let bytes = serde_json::to_vec(&ledger).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(active_reservation_count(temp.path(), "runtime", "session", 20).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn migration_preserves_fencing_and_does_not_quarantine_supported_ledgers() {
    let temp = tempdir().unwrap();
    let path = legacy_ledger(temp.path(), 14, true);
    pre_interrupt_agent(
        temp.path(),
        "runtime",
        "session",
        Some(&json!({"target":"agent-a"})),
        20,
    )
    .unwrap();
    assert_eq!(
        active_reservation_count(temp.path(), "runtime", "session", 21).unwrap(),
        Some(1)
    );
    recover_corrupt_gate_state(temp.path(), "runtime", "session", 22).unwrap();
    assert!(path.exists());
    let ledger: SessionLedger = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(ledger.reservations["task_a"].fenced_at_ms.is_some());
    assert_eq!(
        ledger.reservations["task_a"].outcome,
        ExecutionOutcome::Lost
    );
}
