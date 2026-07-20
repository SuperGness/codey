use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::Engine;
use codex_plus_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::codex_config::ensure_global_model_provider;

const SESSION_BUNDLE_FORMAT: &str = "codey.session";
const SESSION_BUNDLE_VERSION: u32 = 1;
const BINARY_VALUE_KEY: &str = "$codeyBase64";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionBundle {
    format: String,
    version: u32,
    exported_at_ms: u128,
    thread: Map<String, Value>,
    rollout: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExportResult {
    pub status: &'static str,
    pub session_id: String,
    pub filename: String,
    pub data: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionImportResult {
    pub status: &'static str,
    pub session_id: String,
    pub title: String,
    pub project_path: String,
    pub duplicated: bool,
    pub message: String,
}

pub fn export_session(home: &Path, session_id: &str) -> Result<SessionExportResult> {
    let session_id = normalize_session_id(session_id);
    if session_id.is_empty() {
        anyhow::bail!("无法识别要导出的会话");
    }
    let (thread, _) = find_thread(home, session_id)?
        .ok_or_else(|| anyhow::anyhow!("未找到会话：{session_id}"))?;
    let rollout_path = thread
        .get("rollout_path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("会话缺少 rollout 文件路径"))?;
    let rollout_path = if rollout_path.is_absolute() {
        rollout_path
    } else {
        home.join(rollout_path)
    };
    let rollout_path = checked_rollout_path(home, &rollout_path)?;
    let rollout = fs::read_to_string(&rollout_path)
        .with_context(|| format!("读取会话数据失败：{}", rollout_path.display()))?;
    validate_rollout(&rollout)?;

    let title = thread
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("未命名会话");
    let filename = format!(
        "Codey会话-{}-{}.codey-session.json",
        safe_filename(title),
        short_session_id(session_id)
    );
    let bundle = SessionBundle {
        format: SESSION_BUNDLE_FORMAT.to_string(),
        version: SESSION_BUNDLE_VERSION,
        exported_at_ms: timestamp_millis(),
        thread,
        rollout,
    };
    let data = serde_json::to_string_pretty(&bundle).context("序列化会话数据失败")?;
    Ok(SessionExportResult {
        status: "exported",
        session_id: session_id.to_string(),
        filename: filename.clone(),
        data,
        message: format!("已导出会话：{filename}"),
    })
}

pub fn import_session(home: &Path, project_path: &str, data: &str) -> Result<SessionImportResult> {
    let bundle: SessionBundle =
        serde_json::from_str(data).context("数据文件不是有效的 Codey 会话 JSON")?;
    if bundle.format != SESSION_BUNDLE_FORMAT {
        anyhow::bail!("不支持的数据文件：缺少 Codey 会话格式标记");
    }
    if bundle.version != SESSION_BUNDLE_VERSION {
        anyhow::bail!(
            "不支持的会话数据版本：{}（当前支持版本 {}）",
            bundle.version,
            SESSION_BUNDLE_VERSION
        );
    }
    validate_rollout(&bundle.rollout)?;
    let project_path = if project_path.trim().is_empty() {
        bundle
            .thread
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("数据文件缺少原项目目录，请在项目行使用导入"))?
    } else {
        project_path
    };
    let project = canonical_project_path(project_path)?;
    let provider_id = ensure_global_model_provider(home)?;

    let original_id = bundle
        .thread
        .get("id")
        .and_then(Value::as_str)
        .map(normalize_session_id)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("数据文件缺少会话 ID"))?;
    let db_path = active_thread_database(home)?
        .ok_or_else(|| anyhow::anyhow!("未找到可写入的 Codex 会话数据库"))?;
    let duplicated = thread_exists(home, original_id)?;
    let session_id = if duplicated {
        Uuid::new_v4().to_string()
    } else {
        original_id.to_string()
    };
    let rollout = rewrite_rollout(
        &bundle.rollout,
        original_id,
        &session_id,
        &project.to_string_lossy(),
        &provider_id,
    )?;
    let rollout_path = imported_rollout_path(home, &session_id);
    let title = bundle
        .thread
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("导入的会话")
        .to_string();

    if let Some(parent) = rollout_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建导入会话目录失败：{}", parent.display()))?;
    }
    let mut rollout_file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&rollout_path)
        .with_context(|| format!("创建导入会话文件失败：{}", rollout_path.display()))?;
    rollout_file
        .write_all(rollout.as_bytes())
        .with_context(|| format!("写入导入会话文件失败：{}", rollout_path.display()))?;
    rollout_file
        .sync_all()
        .with_context(|| format!("保存导入会话文件失败：{}", rollout_path.display()))?;

    let insert_result = insert_thread(
        &db_path,
        &bundle.thread,
        &session_id,
        &project.to_string_lossy(),
        &rollout_path.to_string_lossy(),
        &title,
        &provider_id,
    );
    if let Err(error) = insert_result {
        let _ = fs::remove_file(&rollout_path);
        return Err(error);
    }

    Ok(SessionImportResult {
        status: "imported",
        session_id: session_id.clone(),
        title: title.clone(),
        project_path: project.to_string_lossy().to_string(),
        duplicated,
        message: if duplicated {
            format!("已导入“{title}”；原会话已存在，已创建副本")
        } else {
            format!("已导入会话“{title}”")
        },
    })
}

fn find_thread(home: &Path, session_id: &str) -> Result<Option<(Map<String, Value>, PathBuf)>> {
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let db = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !has_table(&db, "threads")? {
            continue;
        }
        if let Some(thread) = read_thread_row(&db, session_id)? {
            return Ok(Some((thread, db_path)));
        }
    }
    Ok(None)
}

fn read_thread_row(db: &Connection, session_id: &str) -> Result<Option<Map<String, Value>>> {
    let mut statement = db.prepare("SELECT * FROM threads WHERE id=?1 LIMIT 1")?;
    let columns = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let mut rows = statement.query(params![session_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let mut values = Map::with_capacity(columns.len());
    for (index, column) in columns.into_iter().enumerate() {
        values.insert(column, json_from_sql(row.get_ref(index)?));
    }
    Ok(Some(values))
}

fn json_from_sql(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::from(value),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(value) => Value::String(String::from_utf8_lossy(value).to_string()),
        ValueRef::Blob(value) => json!({
            BINARY_VALUE_KEY: base64::engine::general_purpose::STANDARD.encode(value),
        }),
    }
}

fn sql_from_json(value: &Value) -> Result<SqlValue> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| value.as_f64().map(SqlValue::Real))
            .ok_or_else(|| anyhow::anyhow!("会话元数据包含无法写入的数字")),
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Object(object) if object.len() == 1 && object.contains_key(BINARY_VALUE_KEY) => {
            let encoded = object
                .get(BINARY_VALUE_KEY)
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("会话元数据包含无效的二进制值"))?;
            Ok(SqlValue::Blob(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .context("会话元数据包含无效的 Base64 值")?,
            ))
        }
        Value::Array(_) | Value::Object(_) => Ok(SqlValue::Text(value.to_string())),
    }
}

fn active_thread_database(home: &Path) -> Result<Option<PathBuf>> {
    let mut best: Option<(i64, PathBuf)> = None;
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let db =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        if !has_table(&db, "threads")? {
            continue;
        }
        let columns = table_columns(&db, "threads")?;
        if !columns.iter().any(|column| column == "id")
            || !columns.iter().any(|column| column == "rollout_path")
            || !columns.iter().any(|column| column == "cwd")
        {
            continue;
        }
        let score_expression = if columns.iter().any(|column| column == "recency_at_ms") {
            "COALESCE(MAX(recency_at_ms), 0)"
        } else if columns.iter().any(|column| column == "updated_at_ms") {
            "COALESCE(MAX(updated_at_ms), 0)"
        } else if columns.iter().any(|column| column == "updated_at") {
            "COALESCE(MAX(updated_at), 0) * 1000"
        } else {
            "COUNT(*)"
        };
        let score = db
            .query_row(
                &format!("SELECT {score_expression} FROM threads"),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_default();
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, db_path));
        }
    }
    Ok(best.map(|(_, path)| path))
}

fn thread_exists(home: &Path, session_id: &str) -> Result<bool> {
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let db = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !has_table(&db, "threads")? {
            continue;
        }
        let exists = db
            .query_row(
                "SELECT 1 FROM threads WHERE id=?1 LIMIT 1",
                params![session_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            return Ok(true);
        }
    }
    Ok(false)
}

fn insert_thread(
    db_path: &Path,
    exported: &Map<String, Value>,
    session_id: &str,
    project_path: &str,
    rollout_path: &str,
    title: &str,
    provider_id: &str,
) -> Result<()> {
    let mut db = Connection::open(db_path)
        .with_context(|| format!("打开 Codex 会话数据库失败：{}", db_path.display()))?;
    let target_columns = table_columns(&db, "threads")?;
    let target_set = target_columns
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let now_ms = i64::try_from(timestamp_millis()).unwrap_or(i64::MAX);
    let now_seconds = now_ms / 1000;
    let mut overrides = HashMap::<&str, Value>::from([
        ("id", Value::String(session_id.to_string())),
        ("rollout_path", Value::String(rollout_path.to_string())),
        ("cwd", Value::String(project_path.to_string())),
        ("model_provider", Value::String(provider_id.to_string())),
        ("title", Value::String(title.to_string())),
        ("archived", Value::from(0)),
        ("archived_at", Value::Null),
        ("created_at", Value::from(now_seconds)),
        ("created_at_ms", Value::from(now_ms)),
        ("updated_at", Value::from(now_seconds)),
        ("updated_at_ms", Value::from(now_ms)),
        ("recency_at", Value::from(now_seconds)),
        ("recency_at_ms", Value::from(now_ms)),
    ]);
    if target_set.contains("preview")
        && exported
            .get("preview")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        let preview = exported
            .get("first_user_message")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(title);
        overrides.insert("preview", Value::String(preview.to_string()));
    }

    let mut insert_columns = Vec::new();
    let mut insert_values = Vec::new();
    for column in target_columns {
        let value = overrides
            .get(column.as_str())
            .or_else(|| exported.get(&column));
        let Some(value) = value else {
            continue;
        };
        insert_columns.push(column);
        insert_values.push(sql_from_json(value)?);
    }
    for required in ["id", "rollout_path", "cwd"] {
        if !insert_columns.iter().any(|column| column == required) {
            anyhow::bail!("当前 Codex 会话数据库缺少必要字段：{required}");
        }
    }
    let quoted_columns = insert_columns
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = std::iter::repeat_n("?", insert_values.len())
        .collect::<Vec<_>>()
        .join(",");
    let transaction = db.transaction()?;
    transaction
        .execute(
            &format!("INSERT INTO threads ({quoted_columns}) VALUES ({placeholders})"),
            params_from_iter(insert_values.iter()),
        )
        .context("写入导入会话元数据失败")?;
    transaction.commit()?;
    Ok(())
}

fn rewrite_rollout(
    rollout: &str,
    original_id: &str,
    session_id: &str,
    project_path: &str,
    provider_id: &str,
) -> Result<String> {
    let mut output = String::with_capacity(rollout.len());
    for line in rollout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut value: Value =
            serde_json::from_str(line).context("会话 rollout 包含无效的 JSONL 记录")?;
        replace_exact_string(&mut value, original_id, session_id);
        if value.get("type").and_then(Value::as_str) == Some("session_meta")
            && let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut)
        {
            payload.insert("cwd".to_string(), Value::String(project_path.to_string()));
            payload.insert(
                "model_provider".to_string(),
                Value::String(provider_id.to_string()),
            );
        }
        output.push_str(&serde_json::to_string(&value)?);
        output.push('\n');
    }
    Ok(output)
}

fn replace_exact_string(value: &mut Value, old: &str, new: &str) {
    match value {
        Value::String(current) if current == old => *current = new.to_string(),
        Value::Array(items) => {
            for item in items {
                replace_exact_string(item, old, new);
            }
        }
        Value::Object(object) => {
            for child in object.values_mut() {
                replace_exact_string(child, old, new);
            }
        }
        _ => {}
    }
}

fn validate_rollout(rollout: &str) -> Result<()> {
    let mut found_session_meta = false;
    let mut records = 0usize;
    for line in rollout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).context("会话 rollout 包含无效的 JSONL 记录")?;
        found_session_meta |= value.get("type").and_then(Value::as_str) == Some("session_meta");
        records += 1;
    }
    if records == 0 {
        anyhow::bail!("会话数据为空");
    }
    if !found_session_meta {
        anyhow::bail!("会话数据缺少 session_meta 记录");
    }
    Ok(())
}

fn checked_rollout_path(home: &Path, rollout_path: &Path) -> Result<PathBuf> {
    let canonical_home = home
        .canonicalize()
        .with_context(|| format!("找不到 Codex 数据目录：{}", home.display()))?;
    let canonical_rollout = rollout_path
        .canonicalize()
        .with_context(|| format!("找不到会话数据文件：{}", rollout_path.display()))?;
    if !canonical_rollout.starts_with(&canonical_home) {
        anyhow::bail!("会话数据文件不在 Codex 数据目录内，已拒绝导出");
    }
    Ok(canonical_rollout)
}

fn canonical_project_path(project_path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(project_path.trim());
    if project_path.trim().is_empty() || !path.is_absolute() {
        anyhow::bail!("只能将会话导入本地项目目录");
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("找不到项目目录：{}", path.display()))?;
    if !canonical.is_dir() {
        anyhow::bail!("导入目标不是目录：{}", canonical.display());
    }
    Ok(canonical)
}

fn imported_rollout_path(home: &Path, session_id: &str) -> PathBuf {
    home.join("sessions")
        .join("imported")
        .join(format!("rollout-{}-{session_id}.jsonl", timestamp_millis()))
}

fn has_table(db: &Connection, table: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 LIMIT 1",
            params![table],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(&format!(
        "PRAGMA table_info(\"{}\")",
        table.replace('"', "\"\"")
    ))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns)
}

fn normalize_session_id(value: &str) -> &str {
    value.strip_prefix("local:").unwrap_or(value).trim()
}

fn short_session_id(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '-')
        .take(8)
        .collect()
}

fn safe_filename(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.').trim();
    if sanitized.is_empty() {
        "未命名会话".to_string()
    } else {
        sanitized.chars().take(80).collect()
    }
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::tempdir;

    fn create_thread_db(home: &Path, session_id: &str, cwd: &Path, title: &str) -> PathBuf {
        fs::create_dir_all(home).unwrap();
        let db_path = home.join("state_5.sqlite");
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                source TEXT NOT NULL,
                model_provider TEXT NOT NULL,
                cwd TEXT NOT NULL,
                title TEXT NOT NULL,
                sandbox_policy TEXT NOT NULL,
                approval_mode TEXT NOT NULL,
                tokens_used INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                archived_at INTEGER,
                preview TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();
        let rollout_path = home
            .join("sessions")
            .join(format!("rollout-{session_id}.jsonl"));
        fs::create_dir_all(rollout_path.parent().unwrap()).unwrap();
        fs::write(
            &rollout_path,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session_id}\",\"session_id\":\"{session_id}\",\"cwd\":{}}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[]}}}}\n",
                serde_json::to_string(&cwd.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        db.execute(
            "INSERT INTO threads (
                id, rollout_path, created_at, updated_at, source, model_provider, cwd,
                title, sandbox_policy, approval_mode, tokens_used, archived, preview
             ) VALUES (?1, ?2, 10, 20, 'vscode', 'openai', ?3, ?4, '{}', 'never', 7, 0, ?4)",
            params![
                session_id,
                rollout_path.to_string_lossy(),
                cwd.to_string_lossy(),
                title
            ],
        )
        .unwrap();
        db_path
    }

    #[test]
    fn exports_and_imports_a_portable_session_bundle() {
        let source = tempdir().unwrap();
        let source_project = tempdir().unwrap();
        let source_id = "01900000-0000-7000-8000-000000000001";
        create_thread_db(
            source.path(),
            source_id,
            source_project.path(),
            "可移植会话",
        );
        let exported = export_session(source.path(), source_id).unwrap();
        assert_eq!(exported.status, "exported");
        assert!(exported.filename.starts_with("Codey会话-"));
        assert!(exported.filename.ends_with(".codey-session.json"));

        let target = tempdir().unwrap();
        let seed_project = tempdir().unwrap();
        create_thread_db(
            target.path(),
            "01900000-0000-7000-8000-000000000002",
            seed_project.path(),
            "已有会话",
        );
        let imported_project = tempdir().unwrap();
        let imported = import_session(
            target.path(),
            imported_project.path().to_str().unwrap(),
            &exported.data,
        )
        .unwrap();
        assert_eq!(imported.session_id, source_id);
        assert!(!imported.duplicated);

        let db = Connection::open(target.path().join("state_5.sqlite")).unwrap();
        let row = db
            .query_row(
                "SELECT cwd, title, model_provider, rollout_path, created_at FROM threads WHERE id=?1",
                params![source_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            PathBuf::from(&row.0),
            imported_project.path().canonicalize().unwrap()
        );
        assert_eq!(row.1, "可移植会话");
        assert_eq!(row.2, crate::codex_config::GLOBAL_PROVIDER_ID);
        assert!(row.4 > 20);
        let rollout = fs::read_to_string(row.3).unwrap();
        assert!(rollout.contains(imported_project.path().to_str().unwrap()));
        assert!(rollout.contains(source_id));
    }

    #[test]
    fn imports_a_duplicate_as_a_new_session() {
        let home = tempdir().unwrap();
        let project = tempdir().unwrap();
        let session_id = "01900000-0000-7000-8000-000000000003";
        create_thread_db(home.path(), session_id, project.path(), "重复会话");
        let exported = export_session(home.path(), session_id).unwrap();
        let imported = import_session(
            home.path(),
            project.path().to_str().unwrap(),
            &exported.data,
        )
        .unwrap();
        assert!(imported.duplicated);
        assert_ne!(imported.session_id, session_id);
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM threads", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let rollout_path = db
            .query_row(
                "SELECT rollout_path FROM threads WHERE id=?1",
                params![imported.session_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        let rollout = fs::read_to_string(rollout_path).unwrap();
        assert!(rollout.contains(&imported.session_id));
        assert!(!rollout.contains(session_id));
    }

    #[test]
    fn imports_from_the_tasks_header_using_the_exported_project() {
        let source = tempdir().unwrap();
        let source_project = tempdir().unwrap();
        let source_id = "01900000-0000-7000-8000-000000000004";
        create_thread_db(
            source.path(),
            source_id,
            source_project.path(),
            "待导入会话",
        );
        let exported = export_session(source.path(), source_id).unwrap();

        let target = tempdir().unwrap();
        let seed_project = tempdir().unwrap();
        let target_id = "01900000-0000-7000-8000-000000000005";
        create_thread_db(target.path(), target_id, seed_project.path(), "已有任务");

        let imported = import_session(target.path(), "", &exported.data).unwrap();
        assert_eq!(
            PathBuf::from(&imported.project_path),
            source_project.path().canonicalize().unwrap()
        );
        let db = Connection::open(target.path().join("state_5.sqlite")).unwrap();
        let cwd = db
            .query_row(
                "SELECT cwd FROM threads WHERE id=?1",
                params![imported.session_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(
            PathBuf::from(cwd),
            source_project.path().canonicalize().unwrap()
        );
    }
}
