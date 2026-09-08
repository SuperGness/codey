use codey_runtime_core::models::{DeleteResult, DeleteStatus, SessionRef};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, ToSql};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub fn delete_local_from_paths(
    db_paths: impl IntoIterator<Item = PathBuf>,
    session: &SessionRef,
) -> DeleteResult {
    let mut result = failed(
        &session.session_id,
        "Thread not found in local storage".to_string(),
    );
    let mut deleted_count = 0usize;
    let mut hard_failures = Vec::new();
    let mut rollout_cache = HashMap::new();
    for db_path in db_paths {
        let candidate_result = delete_local_permanently(&db_path, session, &mut rollout_cache);
        if matches!(candidate_result.status, DeleteStatus::LocalDeleted) {
            deleted_count += 1;
            result = candidate_result;
        } else if is_missing_local_session(&candidate_result) {
            if deleted_count == 0 && hard_failures.is_empty() {
                result = candidate_result;
            }
        } else {
            hard_failures.push(format!(
                "{}: {}",
                db_path.to_string_lossy(),
                candidate_result.message
            ));
            if deleted_count == 0 {
                result = candidate_result;
            }
        }
    }
    if !hard_failures.is_empty() {
        result.status = if deleted_count == 0 {
            DeleteStatus::Failed
        } else {
            DeleteStatus::Partial
        };
        result.message = if deleted_count == 0 {
            format!("删除本地会话失败：{}", hard_failures.join("；"))
        } else {
            format!(
                "已从 {deleted_count} 个本地存储删除，但其余存储清理失败：{}",
                hard_failures.join("；")
            )
        };
    } else if deleted_count > 1 {
        result.message = format!("已从 {deleted_count} 个本地存储删除");
    }
    result
}

fn is_missing_local_session(result: &DeleteResult) -> bool {
    matches!(result.status, DeleteStatus::Failed)
        && (matches!(
            result.message.as_str(),
            "Thread not found in local storage" | "Session not found in local storage"
        ) || result.message.starts_with("Database not found: "))
}

fn delete_local_permanently(
    db_path: &Path,
    session: &SessionRef,
    rollout_cache: &mut HashMap<PathBuf, Vec<PathBuf>>,
) -> DeleteResult {
    if matches!(db_path.try_exists(), Ok(false)) {
        return failed(
            &session.session_id,
            format!("Database not found: {}", db_path.to_string_lossy()),
        );
    }
    let result = (|| -> anyhow::Result<DeleteResult> {
        let mut db =
            Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        match schema_kind(&db)? {
            Some(SchemaKind::GenericSessions) => {
                permanently_delete_generic_session(&mut db, session)
            }
            Some(SchemaKind::CodexThreads) => {
                permanently_delete_codex_thread(db_path, &mut db, session, rollout_cache)
            }
            Some(SchemaKind::CodexAutomationRuns) => {
                permanently_delete_codex_automation_run(&mut db, session)
            }
            Some(SchemaKind::CodexThreadCatalog) => {
                permanently_delete_codex_thread_catalog(&mut db, session)
            }
            None => Ok(failed(
                &session.session_id,
                "Unsupported local storage schema".to_string(),
            )),
        }
    })();
    result.unwrap_or_else(|error| failed(&session.session_id, error.to_string()))
}

fn permanently_delete_generic_session(
    db: &mut Connection,
    session: &SessionRef,
) -> anyhow::Result<DeleteResult> {
    if !matching_rows_exist(db, "sessions", "id = ?1", &[&session.session_id])? {
        return Ok(failed(
            &session.session_id,
            "Session not found in local storage".to_string(),
        ));
    }
    let tx = db.transaction()?;
    if has_table(&tx, "messages")? {
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            [&session.session_id],
        )?;
    }
    tx.execute("DELETE FROM sessions WHERE id = ?1", [&session.session_id])?;
    tx.commit()?;
    Ok(permanently_deleted(&session.session_id))
}

fn permanently_delete_codex_thread(
    db_path: &Path,
    db: &mut Connection,
    session: &SessionRef,
    rollout_cache: &mut HashMap<PathBuf, Vec<PathBuf>>,
) -> anyhow::Result<DeleteResult> {
    let thread_id = normalize_codex_thread_id(&session.session_id);
    let thread_rows = select_dicts(db, "SELECT * FROM threads WHERE id = ?1", &[&thread_id])?;
    let rollout_paths =
        codex_thread_rollout_paths(db_path, &thread_rows, &thread_id, rollout_cache)?;
    let mut found = !thread_rows.is_empty() || !rollout_paths.is_empty();
    for (table, where_clause) in [
        ("thread_dynamic_tools", "thread_id = ?1"),
        ("thread_goals", "thread_id = ?1"),
        (
            "thread_spawn_edges",
            "parent_thread_id = ?1 OR child_thread_id = ?1",
        ),
        ("stage1_outputs", "thread_id = ?1"),
        ("agent_job_items", "assigned_thread_id = ?1"),
        ("local_thread_catalog", "thread_id = ?1"),
    ] {
        if matching_rows_exist(db, table, where_clause, &[&thread_id])? {
            found = true;
        }
    }
    if !found {
        return Ok(failed(
            &session.session_id,
            "Thread not found in local storage".to_string(),
        ));
    }

    let tx = db.transaction()?;
    delete_related_rows(&tx, "thread_dynamic_tools", "thread_id = ?1", &[&thread_id])?;
    delete_related_rows(&tx, "thread_goals", "thread_id = ?1", &[&thread_id])?;
    delete_related_rows(
        &tx,
        "thread_spawn_edges",
        "parent_thread_id = ?1 OR child_thread_id = ?1",
        &[&thread_id],
    )?;
    delete_related_rows(&tx, "stage1_outputs", "thread_id = ?1", &[&thread_id])?;
    if has_table(&tx, "agent_job_items")?
        && has_columns(&tx, "agent_job_items", &["assigned_thread_id"])?
    {
        tx.execute(
            "UPDATE agent_job_items SET assigned_thread_id = NULL WHERE assigned_thread_id = ?1",
            [&thread_id],
        )?;
    }
    delete_related_rows(&tx, "local_thread_catalog", "thread_id = ?1", &[&thread_id])?;
    tx.execute("DELETE FROM threads WHERE id = ?1", [&thread_id])?;
    tx.commit()?;

    let mut file_errors = Vec::new();
    for path in rollout_paths {
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            file_errors.push(format!("{}: {error}", path.display()));
        } else if let Some(home) = codex_home_from_db_path(db_path)
            && let Some(paths) = rollout_cache.get_mut(home)
        {
            // Later databases must not count an already deleted orphan as present.
            paths.retain(|candidate| candidate != &path);
        }
    }
    if !file_errors.is_empty() {
        return Ok(failed(
            &thread_id,
            format!(
                "本地数据库已永久删除，但文件删除失败：{}",
                file_errors.join("; ")
            ),
        ));
    }
    Ok(permanently_deleted(&thread_id))
}

fn permanently_delete_codex_automation_run(
    db: &mut Connection,
    session: &SessionRef,
) -> anyhow::Result<DeleteResult> {
    let thread_id = normalize_codex_thread_id(&session.session_id);
    let mut found = false;
    for table in ["automation_runs", "inbox_items", "local_thread_catalog"] {
        if matching_rows_exist(db, table, "thread_id = ?1", &[&thread_id])? {
            found = true;
        }
    }
    if !found {
        return Ok(failed(
            &session.session_id,
            "Thread not found in local storage".to_string(),
        ));
    }
    let tx = db.transaction()?;
    delete_related_rows(&tx, "automation_runs", "thread_id = ?1", &[&thread_id])?;
    delete_related_rows(&tx, "inbox_items", "thread_id = ?1", &[&thread_id])?;
    delete_related_rows(&tx, "local_thread_catalog", "thread_id = ?1", &[&thread_id])?;
    tx.commit()?;
    Ok(permanently_deleted(&thread_id))
}

fn permanently_delete_codex_thread_catalog(
    db: &mut Connection,
    session: &SessionRef,
) -> anyhow::Result<DeleteResult> {
    let thread_id = normalize_codex_thread_id(&session.session_id);
    if !matching_rows_exist(db, "local_thread_catalog", "thread_id = ?1", &[&thread_id])? {
        return Ok(failed(
            &session.session_id,
            "Thread not found in local storage".to_string(),
        ));
    }
    db.execute(
        "DELETE FROM local_thread_catalog WHERE thread_id = ?1",
        [&thread_id],
    )?;
    Ok(permanently_deleted(&thread_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaKind {
    GenericSessions,
    CodexThreads,
    CodexAutomationRuns,
    CodexThreadCatalog,
}

fn failed(session_id: &str, message: String) -> DeleteResult {
    DeleteResult {
        status: DeleteStatus::Failed,
        session_id: session_id.to_string(),
        message,
        undo_token: None,
        backup_path: None,
    }
}

fn permanently_deleted(session_id: &str) -> DeleteResult {
    DeleteResult {
        status: DeleteStatus::LocalDeleted,
        session_id: session_id.to_string(),
        message: "已从本地存储永久删除".to_string(),
        undo_token: None,
        backup_path: None,
    }
}

fn normalize_codex_thread_id(session_id: &str) -> String {
    session_id
        .strip_prefix("local:")
        .unwrap_or(session_id)
        .to_string()
}

fn schema_kind(db: &Connection) -> anyhow::Result<Option<SchemaKind>> {
    if has_table(db, "sessions")? && has_columns(db, "sessions", &["id", "title"])? {
        if has_table(db, "messages")? && !has_columns(db, "messages", &["session_id"])? {
            return Ok(None);
        }
        return Ok(Some(SchemaKind::GenericSessions));
    }
    if has_table(db, "threads")? && has_columns(db, "threads", &["id", "title", "rollout_path"])? {
        return Ok(Some(SchemaKind::CodexThreads));
    }
    if has_table(db, "automation_runs")? && has_columns(db, "automation_runs", &["thread_id"])? {
        return Ok(Some(SchemaKind::CodexAutomationRuns));
    }
    if has_table(db, "local_thread_catalog")?
        && has_columns(db, "local_thread_catalog", &["thread_id"])?
    {
        return Ok(Some(SchemaKind::CodexThreadCatalog));
    }
    Ok(None)
}

fn has_table(db: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn has_columns(db: &Connection, table: &str, columns: &[&str]) -> anyhow::Result<bool> {
    let existing: HashSet<String> = table_columns(db, table)?.into_iter().collect();
    Ok(columns.iter().all(|column| existing.contains(*column)))
}

fn table_columns(db: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = db.prepare(&format!(
        "PRAGMA table_info(\"{}\")",
        table.replace('"', "\"\"")
    ))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn select_dicts(db: &Connection, sql: &str, params: &[&dyn ToSql]) -> anyhow::Result<Vec<Value>> {
    let mut stmt = db.prepare(sql)?;
    let columns: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let rows = stmt.query_map(params, |row| {
        let mut data = Map::new();
        for (index, column) in columns.iter().enumerate() {
            data.insert(column.clone(), sql_value_to_json(row.get_ref(index)?));
        }
        Ok(Value::Object(data))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn delete_related_rows(
    db: &Connection,
    table: &str,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> anyhow::Result<()> {
    if has_table(db, table)? {
        db.execute(
            &format!("DELETE FROM \"{table}\" WHERE {where_clause}"),
            params,
        )?;
    }
    Ok(())
}

fn matching_rows_exist(
    db: &Connection,
    table: &str,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> anyhow::Result<bool> {
    if !has_table(db, table)? {
        return Ok(false);
    }
    let exists = db.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM \"{table}\" WHERE {where_clause} LIMIT 1)"),
        params,
        |row| row.get::<_, bool>(0),
    )?;
    Ok(exists)
}

fn codex_home_from_db_path(db_path: &Path) -> Option<&Path> {
    let db_dir = db_path.parent()?;
    if db_dir.ends_with("sqlite") {
        db_dir.parent()
    } else {
        Some(db_dir)
    }
}

fn checked_codex_rollout_path(db_path: &Path, value: &str) -> anyhow::Result<Option<PathBuf>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        anyhow::bail!("rollout path must be absolute: {value}");
    }
    let home = codex_home_from_db_path(db_path)
        .ok_or_else(|| anyhow::anyhow!("database path has no Codex home: {}", db_path.display()))?
        .canonicalize()?;
    let canonical_path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("rollout path has no parent: {value}"))?
                .canonicalize()?;
            let file_name = path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("rollout path has no file name: {value}"))?;
            parent.join(file_name)
        }
        Err(error) => return Err(error.into()),
    };
    if !["sessions", "archived_sessions"]
        .into_iter()
        .any(|directory| canonical_path.starts_with(home.join(directory)))
    {
        anyhow::bail!("rollout path is outside managed session directories: {value}");
    }
    Ok(Some(path))
}

fn codex_thread_rollout_paths(
    db_path: &Path,
    thread_rows: &[Value],
    thread_id: &str,
    rollout_cache: &mut HashMap<PathBuf, Vec<PathBuf>>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for path in thread_rows
        .iter()
        .filter_map(|row| row.get("rollout_path").and_then(Value::as_str))
    {
        if let Some(path) = checked_codex_rollout_path(db_path, path)? {
            add_rollout_path(&mut paths, &mut seen, path);
        }
    }

    let Some(home) = codex_home_from_db_path(db_path) else {
        return Ok(paths);
    };
    // This cache lives only for one deletion. Indexed paths are checked for each
    // database; the expensive orphan discovery is shared within the same home.
    if let std::collections::hash_map::Entry::Vacant(entry) =
        rollout_cache.entry(home.to_path_buf())
    {
        let mut discovered = Vec::new();
        let mut discovered_set = HashSet::new();
        for root in [home.join("sessions"), home.join("archived_sessions")] {
            collect_matching_rollout_paths(&root, thread_id, &mut discovered, &mut discovered_set)?;
        }
        entry.insert(discovered);
    }
    for path in &rollout_cache[home] {
        add_rollout_path(&mut paths, &mut seen, path.clone());
    }
    Ok(paths)
}

fn add_rollout_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn collect_matching_rollout_paths(
    root: &Path,
    thread_id: &str,
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    let entries = fs::read_dir(root)?;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_matching_rollout_paths(&path, thread_id, paths, seen)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            && rollout_matches_thread_id(&path, thread_id)?
        {
            add_rollout_path(paths, seen, path);
        }
    }
    Ok(())
}

fn rollout_matches_thread_id(path: &Path, thread_id: &str) -> anyhow::Result<bool> {
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(metadata) = event
            .get("payload")
            .filter(|_| event.get("type").and_then(Value::as_str) == Some("session_meta"))
        else {
            continue;
        };
        return Ok(["id", "session_id"]
            .iter()
            .any(|key| metadata.get(*key).and_then(Value::as_str) == Some(thread_id)));
    }
    Ok(false)
}

fn sql_value_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => json!(String::from_utf8_lossy(value).to_string()),
        ValueRef::Blob(value) => json!(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            value
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollout(home: &Path, name: &str, id: &str) -> PathBuf {
        let path = home.join("sessions").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("{}\n", json!({"type":"session_meta","payload":{"id":id}})),
        )
        .unwrap();
        path
    }

    #[test]
    fn orphan_deletion_is_not_counted_again_for_later_databases() {
        let home = tempfile::tempdir().unwrap();
        let path = rollout(home.path(), "orphan.jsonl", "thread");
        let databases: Vec<_> = ["first.db", "second.db"]
            .map(|name| {
                let db_path = home.path().join(name);
                Connection::open(&db_path).unwrap().execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT, rollout_path TEXT);"
            ).unwrap();
                db_path
            })
            .into();
        let result =
            delete_local_from_paths(databases, &SessionRef::new("thread", "Thread").unwrap());
        assert_eq!(result.status, DeleteStatus::LocalDeleted);
        assert_eq!(result.message, "已从本地存储永久删除");
        assert!(!path.exists());
    }

    #[test]
    fn rollout_discovery_keeps_homes_separate_and_does_not_cache_failures() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_path = rollout(first.path(), "first.jsonl", "thread");
        let second_path = rollout(second.path(), "second.jsonl", "thread");
        let mut cache = HashMap::new();
        for (home, expected) in [(first.path(), first_path), (second.path(), second_path)] {
            assert_eq!(
                codex_thread_rollout_paths(&home.join("state.db"), &[], "thread", &mut cache)
                    .unwrap(),
                [expected]
            );
        }
        assert_eq!(cache.len(), 2);

        let failed = tempfile::tempdir().unwrap();
        fs::write(failed.path().join("sessions"), "not a directory").unwrap();
        let db = failed.path().join("state.db");
        assert!(codex_thread_rollout_paths(&db, &[], "thread", &mut cache).is_err());
        assert!(!cache.contains_key(failed.path()));
        fs::remove_file(failed.path().join("sessions")).unwrap();
        let recovered = rollout(failed.path(), "recovered.jsonl", "thread");
        assert_eq!(
            codex_thread_rollout_paths(&db, &[], "thread", &mut cache).unwrap(),
            [recovered]
        );
    }

    #[test]
    #[ignore = "opt-in rollout discovery benchmark"]
    fn rollout_discovery_benchmark() {
        use std::time::Instant;
        let home = tempfile::tempdir().unwrap();
        for index in 0..2_000 {
            rollout(
                home.path(),
                &format!("{index}.jsonl"),
                if index == 0 { "target" } else { "other" },
            );
        }
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..9 {
            for shared in if sample % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let mut cache = HashMap::new();
                let start = Instant::now();
                for database in ["first.db", "second.db", "third.db"] {
                    if !shared {
                        cache.clear();
                    }
                    assert_eq!(
                        codex_thread_rollout_paths(
                            &home.path().join(database),
                            &[],
                            "target",
                            &mut cache
                        )
                        .unwrap()
                        .len(),
                        1
                    );
                }
                samples[usize::from(shared)].push(start.elapsed().as_secs_f64() * 1_000.0);
            }
        }
        for sample in &mut samples {
            sample.sort_by(f64::total_cmp);
        }
        eprintln!(
            "rollout discovery: 2000 files, 3 databases, median of 9; repeated={:.3}ms shared={:.3}ms speedup={:.2}x",
            samples[0][4],
            samples[1][4],
            samples[0][4] / samples[1][4]
        );
    }
}
