use codey_runtime_core::models::{DeleteStatus, SessionRef};
use codey_runtime_data::delete_local_from_paths;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn session(id: &str, title: &str) -> SessionRef {
    SessionRef::new(id, title).unwrap()
}

fn create_supported_db(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, body TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sessions (id, title) VALUES ('s1', 'First')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO messages (session_id, body) VALUES ('s1', 'hello')",
        [],
    )
    .unwrap();
}

fn create_codex_thread_db(path: &Path, rollout_path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, cwd TEXT, archived INTEGER, archived_at INTEGER, updated_at INTEGER, updated_at_ms INTEGER)", []).unwrap();
    db.execute(
        "CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL, tool_name TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE thread_goals (thread_id TEXT NOT NULL, goal TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute("CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL, status TEXT NOT NULL)", []).unwrap();
    db.execute(
        "CREATE TABLE stage1_outputs (thread_id TEXT NOT NULL, output TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE agent_job_items (id TEXT PRIMARY KEY, assigned_thread_id TEXT)",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO threads (id, rollout_path, title, cwd, archived, archived_at, updated_at, updated_at_ms) VALUES ('t1', ?1, 'Codex Thread', '/old/project', 0, NULL, 100, 100000)", [rollout_path.to_string_lossy().to_string()]).unwrap();
    db.execute(
        "INSERT INTO thread_dynamic_tools (thread_id, tool_name) VALUES ('t1', 'Read')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_goals (thread_id, goal) VALUES ('t1', 'delete me')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id, status) VALUES ('t1', 'child', 'running')", []).unwrap();
    db.execute("INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id, status) VALUES ('parent', 't1', 'done')", []).unwrap();
    db.execute(
        "INSERT INTO stage1_outputs (thread_id, output) VALUES ('t1', 'cached')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO agent_job_items (id, assigned_thread_id) VALUES ('job1', 't1')",
        [],
    )
    .unwrap();
}

fn managed_rollout_path(home: &Path, name: &str) -> std::path::PathBuf {
    let path = home.join("sessions").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

fn thread_count(path: &Path, id: &str) -> i64 {
    let db = Connection::open(path).unwrap();
    db.query_row("SELECT COUNT(*) FROM threads WHERE id = ?1", [id], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap()
}

#[test]
fn generic_delete_rolls_back_when_later_delete_fails() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex.sqlite");
    create_supported_db(&db_path);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TRIGGER fail_session_delete BEFORE DELETE ON sessions BEGIN SELECT RAISE(ABORT, 'boom'); END",
        [],
    )
    .unwrap();
    drop(db);

    let result = delete_local_from_paths([db_path.clone()], &session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.undo_token.is_none());
    assert!(result.backup_path.is_none());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions WHERE id = 's1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 's1'",
            [],
            |row| { row.get::<_, i64>(0) }
        )
        .unwrap(),
        1
    );
}

#[test]
fn delete_codex_thread_schema_removes_related_rows_and_file() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = managed_rollout_path(tmp.path(), "rollout.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);

    let deleted = delete_local_from_paths([db_path.clone()], &session("local:t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert!(!rollout_path.exists());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT assigned_thread_id FROM agent_job_items WHERE id = 'job1'",
            [],
            |row| row.get::<_, Option<String>>(0)
        )
        .unwrap(),
        None
    );
    drop(db);
}

#[test]
fn permanent_delete_rejects_rollouts_outside_managed_directories() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let outside_path = tmp.path().join("outside.jsonl");
    fs::write(&outside_path, "do not delete\n").unwrap();
    create_codex_thread_db(&db_path, &outside_path);

    let result = delete_local_from_paths(vec![db_path.clone()], &session("t1", "Codex Thread"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(
        result
            .message
            .contains("outside managed session directories")
    );
    assert!(outside_path.exists());
    assert_eq!(thread_count(&db_path, "t1"), 1);
}

#[test]
fn delete_local_from_paths_permanently_deletes_generic_session_without_backup() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("generic.sqlite");
    create_supported_db(&db_path);

    let result = delete_local_from_paths(vec![db_path.clone()], &session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert!(result.undo_token.is_none());
    assert!(result.backup_path.is_none());
    assert!(!tmp.path().join("backups").exists());
    let db = Connection::open(db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM messages", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn delete_local_from_paths_permanently_deletes_automation_rows_without_backup() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("automation.db");
    let db = Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE automation_runs (
            thread_id TEXT PRIMARY KEY,
            thread_title TEXT
        );
        CREATE TABLE inbox_items (
            id TEXT PRIMARY KEY,
            thread_id TEXT,
            title TEXT
        );
        INSERT INTO automation_runs VALUES ('t1', 'First');
        INSERT INTO inbox_items VALUES ('i1', 't1', 'Inbox');",
    )
    .unwrap();
    drop(db);

    let result = delete_local_from_paths(vec![db_path.clone()], &session("local:t1", "First"));

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert!(result.undo_token.is_none());
    assert!(result.backup_path.is_none());
    assert!(!tmp.path().join("backups").exists());
    let db = Connection::open(db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM automation_runs", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM inbox_items", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn delete_local_from_paths_removes_duplicate_threads_from_all_databases() {
    let tmp = tempdir().unwrap();
    let first_db = tmp.path().join("first.sqlite");
    let second_db = tmp.path().join("second.sqlite");
    let first_rollout = managed_rollout_path(tmp.path(), "first.jsonl");
    let second_rollout = managed_rollout_path(tmp.path(), "second.jsonl");
    fs::write(&first_rollout, "{\"type\":\"message\"}\n").unwrap();
    fs::write(&second_rollout, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&first_db, &first_rollout);
    create_codex_thread_db(&second_db, &second_rollout);

    let result = delete_local_from_paths(
        vec![first_db.clone(), second_db.clone()],
        &session("t1", "Codex Thread"),
    );

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert_eq!(result.message, "已从 2 个本地存储删除");
    assert!(result.undo_token.is_none());
    assert!(result.backup_path.is_none());
    assert!(!tmp.path().join("backups").exists());
    assert_eq!(thread_count(&first_db, "t1"), 0);
    assert_eq!(thread_count(&second_db, "t1"), 0);
    assert!(!first_rollout.exists());
    assert!(!second_rollout.exists());
}

#[test]
fn delete_local_from_paths_removes_all_rollouts_with_the_deleted_thread_id() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let live_dir = tmp.path().join("sessions/2026/07/18");
    let archived_dir = tmp.path().join("archived_sessions/2026/07/17");
    fs::create_dir_all(&live_dir).unwrap();
    fs::create_dir_all(&archived_dir).unwrap();
    let live_rollout = live_dir.join("current.jsonl");
    let stale_live_rollout = live_dir.join("previous.jsonl");
    let stale_archived_rollout = archived_dir.join("archived.jsonl");
    let other_rollout = archived_dir.join("unrelated.jsonl");
    fs::write(
        &live_rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\"}}\n",
    )
    .unwrap();
    fs::write(
        &stale_live_rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"t1\"}}\n",
    )
    .unwrap();
    fs::write(
        &stale_archived_rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\"}}\n",
    )
    .unwrap();
    fs::write(
        &other_rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t2\"}}\n",
    )
    .unwrap();
    create_codex_thread_db(&db_path, &live_rollout);

    let result =
        delete_local_from_paths(vec![db_path.clone()], &session("local:t1", "Codex Thread"));

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert_eq!(thread_count(&db_path, "t1"), 0);
    assert!(!live_rollout.exists());
    assert!(!stale_live_rollout.exists());
    assert!(!stale_archived_rollout.exists());
    assert!(other_rollout.exists());
}

#[test]
fn delete_local_from_paths_removes_an_orphaned_rollout_without_a_thread_row() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_dir = tmp.path().join("sessions/2026/07/18");
    fs::create_dir_all(&rollout_dir).unwrap();
    let rollout = rollout_dir.join("orphaned.jsonl");
    fs::write(
        &rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\"}}\n",
    )
    .unwrap();
    create_codex_thread_db(&db_path, &rollout);
    Connection::open(&db_path)
        .unwrap()
        .execute("DELETE FROM threads WHERE id = 't1'", [])
        .unwrap();

    let result = delete_local_from_paths(vec![db_path], &session("local:t1", "Codex Thread"));

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert!(!rollout.exists());
}

#[test]
fn delete_local_from_paths_surfaces_catalog_failure_after_thread_deletion() {
    let tmp = tempdir().unwrap();
    let thread_db_path = tmp.path().join("threads.db");
    let rollout_path = managed_rollout_path(tmp.path(), "t1.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&thread_db_path, &rollout_path);

    let catalog_db_path = tmp.path().join("catalog.db");
    let catalog = Connection::open(&catalog_db_path).unwrap();
    catalog
        .execute_batch(
            "CREATE TABLE local_thread_catalog (
                host_id TEXT NOT NULL,
                thread_id TEXT NOT NULL,
                display_title TEXT NOT NULL,
                PRIMARY KEY (host_id, thread_id)
            );
            INSERT INTO local_thread_catalog
                (host_id, thread_id, display_title)
            VALUES ('local', 't1', 'Codex Thread');
            CREATE TRIGGER prevent_catalog_delete
            BEFORE DELETE ON local_thread_catalog
            BEGIN
                SELECT RAISE(ABORT, 'catalog delete blocked');
            END;",
        )
        .unwrap();
    drop(catalog);

    let result = delete_local_from_paths(
        vec![thread_db_path.clone(), catalog_db_path.clone()],
        &session("local:t1", "Codex Thread"),
    );

    assert_eq!(result.status, DeleteStatus::Partial);
    assert!(result.message.contains("catalog delete blocked"));
    assert!(result.undo_token.is_none());
    assert!(result.backup_path.is_none());
    assert!(!tmp.path().join("backups").exists());
    assert_eq!(
        Connection::open(thread_db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM threads WHERE id='t1'", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        Connection::open(catalog_db_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM local_thread_catalog WHERE thread_id='t1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn delete_catalog_only_thread_removes_its_sidebar_record() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let db = Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE local_thread_catalog (
            host_id TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            display_title TEXT NOT NULL,
            PRIMARY KEY (host_id, thread_id)
        );
        INSERT INTO local_thread_catalog
            (host_id, thread_id, display_title)
        VALUES ('local', 't1', 'Codex Thread');",
    )
    .unwrap();
    drop(db);

    let deleted = delete_local_from_paths([db_path.clone()], &session("local:t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert_eq!(
        Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM local_thread_catalog WHERE thread_id='t1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn delete_catalog_record_when_the_thread_table_is_already_missing_it() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mixed.db");
    let db = Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            rollout_path TEXT NOT NULL
        );
        CREATE TABLE local_thread_catalog (
            host_id TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            display_title TEXT NOT NULL,
            PRIMARY KEY (host_id, thread_id)
        );
        INSERT INTO local_thread_catalog
            (host_id, thread_id, display_title)
        VALUES ('local', 't1', 'Codex Thread');",
    )
    .unwrap();
    drop(db);

    let deleted = delete_local_from_paths([db_path.clone()], &session("local:t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert_eq!(
        Connection::open(db_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM local_thread_catalog WHERE thread_id='t1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn codex_delete_rolls_back_when_related_delete_fails() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = managed_rollout_path(tmp.path(), "rollout.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TRIGGER fail_goals_delete BEFORE DELETE ON thread_goals BEGIN SELECT RAISE(ABORT, 'boom'); END",
        [],
    )
    .unwrap();
    drop(db);

    let result = delete_local_from_paths([db_path.clone()], &session("t1", "Codex Thread"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.undo_token.is_none());
    assert!(rollout_path.exists());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_dynamic_tools WHERE thread_id = 't1'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_goals WHERE thread_id = 't1'",
            [],
            |row| { row.get::<_, i64>(0) }
        )
        .unwrap(),
        1
    );
}

#[test]
fn missing_candidate_does_not_turn_successful_deletion_into_failure() {
    for missing_first in [false, true] {
        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join("current.sqlite");
        create_supported_db(&db_path);
        let missing = tmp.path().join("state_5.sqlite");
        let paths = if missing_first {
            [missing.clone(), db_path.clone()]
        } else {
            [db_path.clone(), missing.clone()]
        };
        let result = delete_local_from_paths(paths, &session("s1", "First"));
        assert_eq!(result.status, DeleteStatus::LocalDeleted);
        assert!(!missing.exists());
        assert_eq!(
            Connection::open(db_path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn schema_read_errors_are_reported_without_being_treated_as_missing_tables() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("corrupt.sqlite");
    fs::write(&path, "not a SQLite database").unwrap();
    let result = delete_local_from_paths([path], &session("s1", "First"));
    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(
        result.message.contains("not a database"),
        "{}",
        result.message
    );
}

#[test]
fn missing_db_and_unsupported_schema_return_failed_results() {
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("missing.sqlite");
    let result = delete_local_from_paths([missing], &session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.message.contains("Database not found"));

    let db_path = tmp.path().join("unknown.sqlite");
    let db = Connection::open(&db_path).unwrap();
    db.execute("CREATE TABLE unrelated (id TEXT PRIMARY KEY)", [])
        .unwrap();
    drop(db);
    let result = delete_local_from_paths([db_path.clone()], &session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.message.contains("Unsupported"));
}
