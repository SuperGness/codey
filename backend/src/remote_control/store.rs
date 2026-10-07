use std::{collections::HashMap, fs, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};

pub(super) fn projects(home: &Path) -> Result<Value, String> {
    let path = home.join(".codex-global-state.json");
    if !path.exists() {
        return Ok(json!([]));
    }
    if fs::metadata(&path)
        .map_err(|_| "无法读取 Codex 项目状态")?
        .len()
        > 16 * 1024 * 1024
    {
        return Err("Codex 项目状态文件过大".into());
    }
    let state: Value =
        serde_json::from_slice(&fs::read(path).map_err(|_| "无法读取 Codex 项目状态")?)
            .map_err(|_| "Codex 项目状态格式无效")?;
    let rows: Vec<Value> = state["local-projects"]
        .as_object()
        .into_iter()
        .flat_map(|p| p.values())
        .filter_map(|project| {
            let id = project["id"].as_str()?;
            let cwd = project["rootPaths"].as_array()?.first()?.as_str()?;
            Some(json!({"id":id,"name":project["name"].as_str().unwrap_or(cwd),"cwd":cwd,"rootPaths":project["rootPaths"]}))
        })
        .collect();
    Ok(json!(rows))
}

pub(super) fn threads(home: &Path, search: &str, archived: bool) -> Result<Value, String> {
    if search.len() > 200 {
        return Err("会话搜索内容过长".into());
    }
    let paths = codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home(home);
    let mut rows = HashMap::<String, Value>::new();
    let mut read_database = false;
    for path in &paths {
        let Ok(connection) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        let _ = connection.busy_timeout(Duration::from_millis(250));
        let Ok(columns) = crate::sqlite_util::table_columns(&connection, "threads") else {
            continue;
        };
        if !columns.contains("id") || !columns.contains("cwd") {
            continue;
        }
        let field = |name: &str, fallback: &str| {
            if columns.contains(name) {
                name.to_string()
            } else {
                fallback.to_string()
            }
        };
        let title = format!(
            "COALESCE(NULLIF({}, ''), {}, '')",
            field("name", "NULL"),
            field("title", "NULL")
        );
        let updated = field(
            "updated_at_ms",
            &format!("{} * 1000", field("updated_at", "0")),
        );
        let archived_column = field("archived", "0");
        let query = format!(
            "SELECT id, COALESCE({title}, ''), cwd, COALESCE({updated}, 0) FROM threads WHERE {archived_column} = ?1 AND (instr(lower(COALESCE({title}, '')), lower(?2)) > 0 OR instr(lower(cwd), lower(?2)) > 0) ORDER BY {updated} DESC LIMIT 200"
        );
        let mut statement = connection
            .prepare(&query)
            .map_err(|_| "Codex 会话数据库结构不兼容")?;
        let records = statement
            .query_map(params![archived, search], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|_| "读取 Codex 会话列表失败")?;
        for record in records {
            let (id, title, cwd, updated) = record.map_err(|_| "读取 Codex 会话记录失败")?;
            if super::desktop::validate_thread(&id).is_err() {
                continue;
            }
            let entry = json!({"id":id,"title":if title.trim().is_empty() { "未命名会话" } else { &title },"cwd":cwd,"updatedAt":updated});
            if rows
                .get(&id)
                .is_none_or(|old| old["updatedAt"].as_i64().unwrap_or(0) < updated)
            {
                rows.insert(id, entry);
            }
        }
        read_database = true;
    }
    if !paths.is_empty() && !read_database {
        return Err("无法读取 Codex 会话数据库，请在电脑端检查 Codex 状态".into());
    }
    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row["updatedAt"].as_i64().unwrap_or(0)));
    rows.truncate(200);
    Ok(json!(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_saved_projects_and_parameterized_thread_search() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join(".codex-global-state.json"),
            r#"{"local-projects":{"p":{"id":"p","name":"Codey","rootPaths":["E:/code/codey"]}}}"#,
        )
        .unwrap();
        assert_eq!(projects(home.path()).unwrap()[0]["id"], "p");
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, title TEXT, cwd TEXT, updated_at INTEGER, archived INTEGER)").unwrap();
        for (title, archived) in [("当前任务", false), ("归档任务", true)] {
            db.execute(
                "INSERT INTO threads VALUES (?1,?2,?3,10,?4)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    title,
                    "E:/code/codey",
                    archived
                ],
            )
            .unwrap();
        }
        assert_eq!(
            threads(home.path(), "", false)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            threads(home.path(), "", true).unwrap()[0]["title"],
            "归档任务"
        );
        assert!(
            threads(home.path(), "' OR 1=1 --", false)
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            threads(home.path(), "当前", false).unwrap()[0]["updatedAt"],
            10_000
        );
    }
}
