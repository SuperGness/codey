use super::*;

#[test]
fn maintenance_status_exposes_structured_session_metrics() {
    let cleanup = Ok(SessionIndexCleanupReport {
        scanned_entries: 5,
        live_threads: 3,
        pruned_entries: 2,
        backup_dir: None,
    });

    let summary = session_maintenance_summary(&cleanup);
    let status = MaintenanceStatus {
        session_status: summary.status,
        session_files_fixed: summary.files_fixed,
        sqlite_rows_updated: summary.sqlite_rows_updated,
        ghost_tasks_pruned: summary.ghost_tasks_pruned,
        performance_status: "ready".to_string(),
        performance_detail: String::new(),
    };
    let value = serde_json::to_value(status).unwrap();

    assert_eq!(value["sessionFilesFixed"], 0);
    assert_eq!(value["sqliteRowsUpdated"], 0);
    assert_eq!(value["ghostTasksPruned"], 2);
}

#[test]
fn startup_without_provider_rewrites_is_still_ready() {
    let cleanup = Ok(SessionIndexCleanupReport {
        scanned_entries: 0,
        live_threads: 0,
        pruned_entries: 0,
        backup_dir: None,
    });

    let summary = session_maintenance_summary(&cleanup);

    assert_eq!(summary.status, "ready");
    assert_eq!(summary.files_fixed, 0);
    assert_eq!(summary.sqlite_rows_updated, 0);
}
