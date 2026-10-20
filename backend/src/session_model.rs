use std::collections::HashSet;
use std::fs::File;
#[cfg(any(windows, target_os = "macos", test))]
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// Only model metadata leaves this module; conversation items are never returned.
fn validate_thread_id(thread_id: &str) -> Result<()> {
    if thread_id.is_empty()
        || thread_id.len() > 128
        || !thread_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        bail!("无效的会话标识");
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfirmedModel {
    thread_id: String,
    model: String,
    provider: Option<String>,
    selected_at_ms: i64,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    reasoning_effort: Option<Value>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

pub(crate) fn valid_reasoning_effort(value: &Value) -> bool {
    value.is_null()
        || value.as_str().is_some_and(|v| {
            matches!(
                v,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
            )
        })
}

fn model_response(
    thread_id: &str,
    model: &str,
    provider: &Option<String>,
    effort: Option<&Value>,
) -> Value {
    let mut result =
        json!({"status":"ok", "threadId":thread_id, "model":model, "modelProvider":provider});
    if let Some(effort) = effort.filter(|v| valid_reasoning_effort(v)) {
        result["reasoningEffort"] = effort.clone();
    }
    result
}

fn valid_model_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value == value.trim()
        && !value.chars().any(char::is_control)
}

fn confirmed_model(home: &Path, thread_id: &str) -> Result<Option<ConfirmedModel>> {
    let store = home.join("codey-session-models");
    match std::fs::symlink_metadata(&store) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {
            checked_path(home, &store)?;
        }
    }
    let path = store.join(format!("{thread_id}.json"));
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() => bail!("会话模型记录不能是符号链接"),
        Ok(_) => {}
    }
    let record: ConfirmedModel = serde_json::from_slice(&crate::fs_util::read_bounded(
        &checked_path(home, &path)?,
        16 * 1024,
    )?)?;
    if record.thread_id != thread_id
        || !valid_model_field(&record.model)
        || record
            .provider
            .as_deref()
            .is_some_and(|v| !valid_model_field(v))
        || record.selected_at_ms <= 0
        || record
            .reasoning_effort
            .as_ref()
            .is_some_and(|v| !valid_reasoning_effort(v))
        || record.selected_at_ms
            > chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(300_000)
    {
        bail!("无效的已确认会话模型记录");
    }
    Ok(Some(record))
}

// 仅接收 app-server 已确认的设置通知，未发送消息的模型选择也能跨重启保留。
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn remember_settings(
    home: &Path,
    thread_id: &str,
    model: &str,
    provider: Option<&str>,
    selected_at_ms: Option<i64>,
    reasoning_effort: Option<&Value>,
) -> Result<()> {
    validate_thread_id(thread_id)?;
    let model = model.trim();
    let provider = provider.map(str::trim);
    if !valid_model_field(model) || provider.is_some_and(|v| !valid_model_field(v)) {
        bail!("无效的会话模型设置");
    }
    if reasoning_effort.is_some_and(|v| !valid_reasoning_effort(v)) {
        bail!("无效的推理强度设置");
    }
    let home = home.canonicalize().context("找不到 Codex 数据目录")?;
    let now = chrono::Utc::now().timestamp_millis();
    let selected_at_ms = selected_at_ms
        .filter(|v| *v > 0 && *v <= now.saturating_add(300_000))
        .unwrap_or(now);
    if confirmed_model(&home, thread_id)?.is_some_and(|old| old.selected_at_ms > selected_at_ms) {
        return Ok(());
    }
    let store = home.join("codey-session-models");
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(&store) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let store = checked_path(&home, &store)?;
    let lock_path = store.join(format!("{thread_id}.lock"));
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            bail!("无效的会话模型锁文件")
        }
        Ok(_) => {
            checked_path(&home, &lock_path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path)?;
    crate::fs_util::acquire_exclusive_lock_with_timeout(
        &lock,
        &lock_path,
        std::time::Duration::from_secs(2),
        std::time::Duration::from_millis(10),
        || "等待会话模型写入超时".to_owned(),
        || "无法锁定会话模型记录".to_owned(),
    )?;
    if confirmed_model(&home, thread_id)?.is_some_and(|old| old.selected_at_ms > selected_at_ms) {
        return Ok(());
    }
    let record = ConfirmedModel {
        thread_id: thread_id.to_owned(),
        model: model.to_owned(),
        provider: provider.map(str::to_owned),
        selected_at_ms,
        reasoning_effort: reasoning_effort.cloned(),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(&store)?;
    serde_json::to_writer(&mut temporary, &record)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(store.join(format!("{thread_id}.json")))?;
    let _ = File::open(&store).and_then(|directory| directory.sync_all());
    Ok(())
}

pub(crate) fn read(home: &Path, thread_id: &str) -> Result<Value> {
    validate_thread_id(thread_id)?;
    let home = home.canonicalize().context("找不到 Codex 数据目录")?;
    let confirmed = confirmed_model(&home, thread_id)?;
    let mut rollout = None;
    let mut saved_model = None;
    for path in codex_session_db_paths_from_home(&home) {
        if !path.is_file() {
            continue;
        }
        let path = checked_path(&home, &path)?;
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let columns: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info('threads')")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !columns.iter().any(|c| c == "id") {
            continue;
        }
        let column = |name: &str| {
            if columns.iter().any(|c| c == name) {
                name.to_owned()
            } else {
                "NULL".to_owned()
            }
        };
        let sql = format!(
            "SELECT {}, {}, {} FROM threads WHERE id=?1 LIMIT 1",
            column("model"),
            column("model_provider"),
            column("rollout_path")
        );
        let row = db
            .query_row(&sql, [thread_id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .optional()?;
        if let Some((model, saved_provider, saved_rollout)) = row {
            if saved_model.is_none() {
                saved_model = model
                    .filter(|m| !m.trim().is_empty())
                    .map(|model| (model, saved_provider));
            }
            rollout = saved_rollout.map(|p| home.join(p)).or(rollout);
        }
    }
    // 恢复会话会刷新数据库中的模型；最后一轮记录才是实际使用的模型。
    // 只有文件确实不存在才允许回退，权限或其他读取错误必须返回。
    let rollout = match rollout {
        Some(path) => match std::fs::metadata(&path) {
            Ok(_) => Some(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        },
        None => None,
    };
    let fallback = || match confirmed
        .as_ref()
        .map(|c| (&c.model, &c.provider, c.reasoning_effort.as_ref()))
        .or_else(|| saved_model.as_ref().map(|(m, p)| (m, p, None)))
    {
        Some((model, provider, effort)) => model_response(thread_id, model, provider, effort),
        None => json!({"status":"missing", "threadId":thread_id}),
    };
    let Some(rollout) = rollout
        .or_else(|| crate::message_delete::find_rollout_file_by_session_id(&home, thread_id))
    else {
        return Ok(fallback());
    };
    let path = checked_path(&home, &rollout)?;
    let mut model = None;
    let mut session_provider = None;
    let mut identity = RolloutIdentity::default();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            identity.require_owner()?;
            continue;
        };
        if identity.observe(&record, thread_id)? {
            session_provider = record["payload"]["model_provider"]
                .as_str()
                .map(str::to_owned);
        }
        let applied_settings = own_settings(&record, thread_id);
        let payload = match record["type"].as_str() {
            Some("turn_context") if identity.owns_turn(&record, thread_id) => &record["payload"],
            Some("event_msg") if applied_settings => &record["payload"]["thread_settings"],
            _ => continue,
        };
        let Ok(payload) = serde_json::from_value::<MetadataPayload>(payload.clone()) else {
            continue;
        };
        if let Some(selected) = payload.model.filter(|m| !m.trim().is_empty()) {
            if applied_settings && payload.model_provider.is_some() {
                session_provider = payload.model_provider.clone();
            }
            model = Some((
                selected,
                payload.model_provider.or_else(|| session_provider.clone()),
                record["timestamp"]
                    .as_str()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.timestamp_millis())
                    .filter(|value| {
                        *value > 0
                            && *value
                                <= chrono::Utc::now()
                                    .timestamp_millis()
                                    .saturating_add(300_000)
                    }),
                payload
                    .effort
                    .or_else(|| {
                        payload
                            .collaboration_mode
                            .and_then(|mode| mode.pointer("/settings/reasoning_effort").cloned())
                    })
                    .filter(valid_reasoning_effort),
            ));
        }
    }
    identity.require_owner()?;
    let Some((model, provider, timestamp, effort)) = model else {
        return Ok(fallback());
    };
    // 缺少可信时间的历史记录不能覆盖明确确认过的选择。
    if confirmed
        .as_ref()
        .is_some_and(|c| timestamp.is_none_or(|time| time <= c.selected_at_ms))
    {
        if let Some(confirmed) = confirmed.as_ref().filter(|c| {
            c.reasoning_effort.is_none()
                && crate::model_id::equal(&c.model, &model)
                && c.provider == provider
        }) {
            return Ok(model_response(
                thread_id,
                &confirmed.model,
                &confirmed.provider,
                effort.as_ref(),
            ));
        }
        return Ok(fallback());
    }
    Ok(model_response(
        thread_id,
        &model,
        &provider,
        effort.as_ref(),
    ))
}

fn checked_path(home: &Path, path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    if !path.starts_with(home) {
        bail!("会话元数据不在 Codex 数据目录内");
    }
    Ok(path)
}

#[derive(Default)]
struct RolloutIdentity {
    seen: bool,
    ancestors: HashSet<String>,
    current_session: bool,
}

fn own_settings(record: &Value, thread_id: &str) -> bool {
    record["type"] == "event_msg"
        && record["payload"]["type"] == "thread_settings_applied"
        && record["payload"]["thread_id"].as_str() == Some(thread_id)
}

impl RolloutIdentity {
    // 首条元数据确定文件归属；分叉历史随后可以包含已声明的父会话和祖先。
    fn observe(&mut self, record: &Value, thread_id: &str) -> Result<bool> {
        if record["type"] != "session_meta" {
            self.require_owner()?;
            if own_settings(record, thread_id) {
                self.current_session = true;
            }
            return Ok(false);
        }
        let payload = &record["payload"];
        let id = payload["id"].as_str().context("无法确认会话记录标识")?;
        if id != thread_id && (!self.seen || !self.ancestors.contains(id)) {
            bail!("会话记录标识不匹配");
        }
        let first = !self.seen;
        self.seen = true;
        for parent in [
            payload.get("forked_from_id"),
            payload.pointer("/source/subagent/thread_spawn/parent_thread_id"),
        ]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_null())
        {
            let parent = parent.as_str().context("无效的父会话标识")?;
            validate_thread_id(parent)?;
            if parent != thread_id {
                self.ancestors.insert(parent.to_owned());
            }
        }
        // 继承回合的时间会被重写，只有明确属于本会话的元数据或设置事件能结束继承段。
        self.current_session =
            id == thread_id && (!first || payload.get("forked_from_id").is_none_or(Value::is_null));
        Ok(first)
    }

    fn owns_turn(&self, record: &Value, thread_id: &str) -> bool {
        match record["payload"].get("thread_id") {
            Some(id) => id.as_str() == Some(thread_id),
            None => self.current_session,
        }
    }

    fn require_owner(&self) -> Result<()> {
        if !self.seen {
            bail!("无法确认会话记录标识");
        }
        Ok(())
    }
}

// 压缩后的 turn_context 可能已记录失败的目标模型，源身份只取压缩记录自身。
pub(crate) fn native_compaction_source(home: &Path, thread_id: &str) -> Result<Option<String>> {
    validate_thread_id(thread_id)?;
    let home = home.canonicalize().context("找不到 Codex 数据目录")?;
    let mut rollout = None;
    for path in codex_session_db_paths_from_home(&home) {
        if !path.is_file() {
            continue;
        }
        let db = Connection::open_with_flags(
            checked_path(&home, &path)?,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let columns: Vec<String> = db
            .prepare("SELECT name FROM pragma_table_info('threads')")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !columns.iter().any(|c| c == "id") || !columns.iter().any(|c| c == "rollout_path") {
            continue;
        }
        let saved: Option<Option<String>> = db
            .query_row(
                "SELECT rollout_path FROM threads WHERE id=?1 LIMIT 1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(Some(path)) = saved {
            rollout = Some(home.join(path));
            break;
        }
    }
    let Some(rollout) = rollout
        .filter(|p| p.is_file())
        .or_else(|| crate::message_delete::find_rollout_file_by_session_id(&home, thread_id))
    else {
        return Ok(None);
    };
    let mut identity = RolloutIdentity::default();
    let mut source = None;
    let mut native_history = false;
    for line in BufReader::new(File::open(checked_path(&home, &rollout)?)?).lines() {
        let line = line?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            identity.require_owner()?;
            continue;
        };
        identity.observe(&record, thread_id)?;
        let payload = &record["payload"];
        if let Some("compacted") = record["type"].as_str() {
            source = None;
            let has_native = payload["replacement_history"]
                .as_array()
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item["type"] == "compaction"
                            && item["encrypted_content"].as_str().is_some_and(|value| {
                                !value.is_empty() && !value.starts_with("codey-summary-v1:")
                            })
                    })
                });
            native_history = has_native;
            if has_native {
                source = payload["resume_metadata"]["previous_turn_settings"]["model"]
                    .as_str()
                    .filter(|model| {
                        model.split_once('/').is_some_and(|(provider, model)| {
                            !provider.is_empty() && !model.is_empty()
                        })
                    })
                    .map(str::to_owned);
            }
        }
    }
    identity.require_owner()?;
    if native_history && source.is_none() {
        bail!("压缩历史缺少明确的源线路模型，请恢复原线路后重试");
    }
    Ok(source)
}

#[derive(Deserialize)]
struct MetadataPayload {
    model: Option<String>,
    #[serde(alias = "model_provider_id")]
    model_provider: Option<String>,
    #[serde(
        default,
        alias = "reasoning_effort",
        deserialize_with = "present_value"
    )]
    effort: Option<Value>,
    collaboration_mode: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remember_settings(
        home: &Path,
        thread_id: &str,
        model: &str,
        provider: Option<&str>,
        timestamp: Option<i64>,
    ) -> Result<()> {
        super::remember_settings(home, thread_id, model, provider, timestamp, None)
    }

    fn rollout_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('saved',NULL,NULL,'session.jsonl');",
        )
        .unwrap();
        home
    }

    fn write_rollout(home: &Path, records: &[Value]) {
        std::fs::write(
            home.join("session.jsonl"),
            records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    }

    fn inherited_metadata() -> Vec<Value> {
        vec![
            json!({"type":"session_meta","payload":{
                "id":"saved","model_provider":"child-provider","forked_from_id":"parent"
            }}),
            json!({"type":"session_meta","payload":{
                "id":"parent","model_provider":"parent-provider",
                "source":{"subagent":{"thread_spawn":{"parent_thread_id":"ancestor"}}}
            }}),
            json!({"type":"session_meta","payload":{
                "id":"ancestor","model_provider":"ancestor-provider"
            }}),
        ]
    }

    #[test]
    fn inherited_models_wait_for_settings_explicitly_applied_to_current_thread() {
        let home = rollout_home();
        let mut records = inherited_metadata();
        records.extend([
            json!({"type":"event_msg","payload":{"type":"thread_settings_applied",
                "thread_id":"parent","thread_settings":{
                    "model":"parent/model","model_provider_id":"parent-applied","reasoning_effort":"low"
                }
            }}),
            json!({"type":"turn_context","payload":{"model":"parent/latest","effort":"medium"}}),
        ]);
        write_rollout(home.path(), &records);
        assert_eq!(read(home.path(), "saved").unwrap()["status"], "missing");

        records.push(json!({"type":"event_msg","payload":{"type":"thread_settings_applied",
            "thread_id":"saved","thread_settings":{
                "model":"child/selected","model_provider_id":"child-applied","reasoning_effort":"high"
            }
        }}));
        write_rollout(home.path(), &records);
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({
                "status":"ok","threadId":"saved","model":"child/selected",
                "modelProvider":"child-applied","reasoningEffort":"high"
            })
        );

        // 原生子代理在设置事件后直接写入自己的回合，不会再写一条 session_meta。
        records.push(
            json!({"type":"turn_context","payload":{"model":"child/first-turn","effort":"max"}}),
        );
        write_rollout(home.path(), &records);
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({
                "status":"ok","threadId":"saved","model":"child/first-turn",
                "modelProvider":"child-applied","reasoningEffort":"max"
            })
        );

        records.extend([
            json!({"type":"session_meta","payload":{"id":"saved","model_provider":"stale-provider"}}),
            json!({"type":"turn_context","payload":{"model":"child/latest","effort":null}}),
        ]);
        write_rollout(home.path(), &records);
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({
                "status":"ok","threadId":"saved","model":"child/latest",
                "modelProvider":"child-applied","reasoningEffort":null
            })
        );
    }

    #[test]
    fn repeated_owner_metadata_and_explicit_turn_identity_preserve_child_provider() {
        let home = rollout_home();
        for own_record in [
            json!({"type":"session_meta","payload":{"id":"saved","model_provider":"ignored-provider"}}),
            json!({"type":"turn_context","payload":{"thread_id":"saved","model":"child/model"}}),
        ] {
            let mut records = inherited_metadata();
            records.push(json!({"type":"turn_context","payload":{"model":"ancestor/model"}}));
            records.push(own_record.clone());
            if own_record["type"] == "session_meta" {
                records.push(json!({"type":"turn_context","payload":{"model":"child/model"}}));
            }
            records.push(json!({"type":"turn_context","payload":{"thread_id":"parent","model":"parent/model"}}));
            write_rollout(home.path(), &records);
            assert_eq!(
                read(home.path(), "saved").unwrap(),
                json!({
                    "status":"ok","threadId":"saved","model":"child/model","modelProvider":"child-provider"
                })
            );
        }
        // 只有来源关系、没有分叉历史的子会话可以直接读取自己的回合。
        write_rollout(
            home.path(),
            &[
                json!({"type":"session_meta","payload":{
                    "id":"saved","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}
                }}),
                json!({"type":"turn_context","payload":{"model":"child/model"}}),
            ],
        );
        assert_eq!(read(home.path(), "saved").unwrap()["model"], "child/model");
    }

    #[test]
    fn inherited_only_history_uses_current_thread_fallbacks_without_parent_effort() {
        let home = rollout_home();
        let mut records = inherited_metadata();
        records.push(json!({"type":"turn_context","timestamp":"2026-01-03T00:00:00Z",
            "payload":{"model":"parent/model","model_provider":"parent-provider","effort":"high"}
        }));
        write_rollout(home.path(), &records);
        assert_eq!(read(home.path(), "saved").unwrap()["status"], "missing");
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute(
            "UPDATE threads SET model='child/database',model_provider='child-database'",
            [],
        )
        .unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({
                "status":"ok","threadId":"saved","model":"child/database","modelProvider":"child-database"
            })
        );
        remember_settings(
            home.path(),
            "saved",
            "child/confirmed",
            Some("child-confirmed"),
            None,
        )
        .unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({
                "status":"ok","threadId":"saved","model":"child/confirmed","modelProvider":"child-confirmed"
            })
        );
    }

    #[test]
    fn applied_settings_respect_confirmed_time_and_effort_presence() {
        let home = rollout_home();
        let confirmed_at = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        remember_settings(
            home.path(),
            "saved",
            "child/confirmed",
            None,
            Some(confirmed_at),
        )
        .unwrap();
        for (timestamp, effort, expected_model) in [
            (
                Some("2026-01-03T00:00:00Z"),
                Some(Value::Null),
                "child/applied",
            ),
            (Some("2026-01-03T00:00:00Z"), None, "child/applied"),
            (
                Some("2026-01-01T00:00:00Z"),
                Some(json!("high")),
                "child/confirmed",
            ),
            (None, Some(json!("high")), "child/confirmed"),
        ] {
            let mut records = inherited_metadata();
            let mut settings = json!({"type":"event_msg","timestamp":timestamp,"payload":{
                "type":"thread_settings_applied","thread_id":"saved","thread_settings":{"model":"child/applied"}
            }});
            if let Some(effort) = effort.clone() {
                settings["payload"]["thread_settings"]["reasoning_effort"] = effort;
            }
            records.push(settings);
            write_rollout(home.path(), &records);
            let result = read(home.path(), "saved").unwrap();
            assert_eq!(result["model"], expected_model);
            assert_eq!(
                result.get("reasoningEffort").cloned(),
                if expected_model == "child/applied" {
                    effort
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn both_readers_reject_wrong_or_missing_initial_identity_and_undeclared_ancestors() {
        let home = rollout_home();
        remember_settings(home.path(), "saved", "child/confirmed", None, None).unwrap();
        let owner = json!({"type":"session_meta","payload":{"id":"saved"}});
        let turn = json!({"type":"turn_context","payload":{"model":"child/model"}});
        for first in [
            json!({"type":"session_meta","payload":{"id":"other"}}),
            json!({"type":"session_meta","payload":{}}),
            json!({"type":"session_meta","payload":{"id":7}}),
            turn.clone(),
        ] {
            write_rollout(home.path(), &[first, owner.clone(), turn.clone()]);
            assert!(read(home.path(), "saved").is_err());
            assert!(native_compaction_source(home.path(), "saved").is_err());
        }
        for prefix in ["not-json", ""] {
            std::fs::write(
                home.path().join("session.jsonl"),
                format!("{prefix}\n{owner}\n{turn}"),
            )
            .unwrap();
            assert!(read(home.path(), "saved").is_err());
            assert!(native_compaction_source(home.path(), "saved").is_err());
        }
        for records in [
            vec![turn],
            vec![
                owner.clone(),
                json!({"type":"session_meta","payload":{"id":"other"}}),
            ],
            vec![
                inherited_metadata()[0].clone(),
                json!({"type":"session_meta","payload":{"id":"ancestor"}}),
            ],
            vec![owner, json!({"type":"session_meta","payload":{"id":null}})],
        ] {
            write_rollout(home.path(), &records);
            assert!(read(home.path(), "saved").is_err());
            assert!(native_compaction_source(home.path(), "saved").is_err());
        }
    }

    #[test]
    fn inherited_native_compaction_keeps_its_explicit_source_until_replaced() {
        let home = rollout_home();
        let mut records = inherited_metadata();
        records.extend([
            json!({"type":"compacted","payload":{
                "replacement_history":[{"type":"compaction","encrypted_content":"opaque"}],
                "resume_metadata":{"previous_turn_settings":{"model":"ancestor/source"}}
            }}),
            json!({"type":"turn_context","payload":{"model":"parent/model"}}),
        ]);
        write_rollout(home.path(), &records);
        assert_eq!(
            native_compaction_source(home.path(), "saved")
                .unwrap()
                .as_deref(),
            Some("ancestor/source")
        );
        assert_eq!(read(home.path(), "saved").unwrap()["status"], "missing");
        records.push(json!({"type":"compacted","payload":{"replacement_history":[
            {"type":"compaction","encrypted_content":"codey-summary-v1:portable"}
        ]}}));
        write_rollout(home.path(), &records);
        assert_eq!(
            native_compaction_source(home.path(), "saved").unwrap(),
            None
        );
    }

    #[test]
    fn reasoning_effort_tracks_the_selected_record_and_preserves_legacy_absence() {
        let home = tempfile::tempdir().unwrap();
        remember_settings(home.path(), "saved", "legacy", None, None).unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap()
                .get("reasoningEffort")
                .is_none()
        );
        for effort in [json!("high"), Value::Null] {
            super::remember_settings(home.path(), "saved", "selected", None, None, Some(&effort))
                .unwrap();
            assert_eq!(
                read(home.path(), "saved").unwrap()["reasoningEffort"],
                effort
            );
        }
        for effort in [json!("invalid"), json!(3), json!({}), json!(" high ")] {
            assert!(
                super::remember_settings(
                    home.path(),
                    "saved",
                    "selected",
                    None,
                    None,
                    Some(&effort)
                )
                .is_err()
            );
        }
        remember_settings(home.path(), "saved", "different-model", None, None).unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap()
                .get("reasoningEffort")
                .is_none()
        );
    }

    #[test]
    fn history_effort_comes_from_same_turn_and_top_level_null_wins() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT); INSERT INTO threads VALUES ('saved','session.jsonl');").unwrap();
        for (payload, expected) in [
            (
                json!({"model":"latest","effort":"high"}),
                Some(json!("high")),
            ),
            (json!({"model":"latest"}), None),
            (
                json!({"model":"latest","effort":null,"collaboration_mode":{"settings":{"reasoning_effort":"low"}}}),
                Some(Value::Null),
            ),
            (
                json!({"model":"latest","collaboration_mode":{"settings":{"reasoning_effort":"low"}}}),
                Some(json!("low")),
            ),
            (json!({"model":"latest","effort":"invalid"}), None),
        ] {
            std::fs::write(
                home.path().join("session.jsonl"),
                format!(
                    "{}\n{}\n{}\n",
                    json!({"type":"session_meta","payload":{"id":"saved"}}),
                    json!({"type":"turn_context","payload":{"model":"old","effort":"medium"}}),
                    json!({"type":"turn_context","payload":payload})
                ),
            )
            .unwrap();
            let saved = read(home.path(), "saved").unwrap();
            assert_eq!(saved["model"], "latest");
            assert_eq!(saved.get("reasoningEffort").cloned(), expected);
        }
    }

    #[test]
    fn legacy_confirmation_inherits_effort_only_for_same_model() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT); INSERT INTO threads VALUES ('saved','session.jsonl');").unwrap();
        std::fs::write(home.path().join("session.jsonl"), format!("{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":"saved"}}),
            json!({"type":"turn_context","timestamp":"2026-01-01T00:00:00Z","payload":{"model":"same","effort":"high"}}))).unwrap();
        remember_settings(home.path(), "saved", "same", None, None).unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap()["reasoningEffort"],
            "high"
        );
        super::remember_settings(home.path(), "saved", "same", None, None, Some(&Value::Null))
            .unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap().get("reasoningEffort"),
            Some(&Value::Null)
        );
        remember_settings(home.path(), "saved", "different", None, None).unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap()
                .get("reasoningEffort")
                .is_none()
        );
    }

    fn model_history(home: &Path, timestamp: Option<&str>, model: &str) {
        let path = home.join("session.jsonl");
        std::fs::write(path, format!("{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":"saved","model_provider":"session-provider"}}),
            json!({"type":"turn_context","timestamp":timestamp,"payload":{"model":model,"model_provider":"turn-provider"}}),
        )).unwrap();
    }

    #[test]
    fn confirmed_unsent_selection_survives_reopening_and_is_isolated() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('saved','default','session.jsonl');
            INSERT INTO threads VALUES ('other','other-model',NULL);",
        )
        .unwrap();
        model_history(home.path(), Some("2026-01-01T00:00:00Z"), "old-model");
        let selected = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        remember_settings(
            home.path(),
            "saved",
            " route/new ",
            Some(" codey_router "),
            Some(selected),
        )
        .unwrap();
        let expected = json!({"status":"ok","threadId":"saved","model":"route/new","modelProvider":"codey_router"});
        assert_eq!(read(home.path(), "saved").unwrap(), expected);
        drop(db);
        // 没有进程内缓存，重新读取磁盘仍得到已经确认但尚未用于新回合的选择。
        assert_eq!(read(home.path(), "saved").unwrap(), expected);
        assert_eq!(read(home.path(), "other").unwrap()["model"], "other-model");
        assert_eq!(read(home.path(), "new").unwrap()["status"], "missing");
        remember_settings(home.path(), "saved", "outdated", None, Some(selected - 1)).unwrap();
        assert_eq!(read(home.path(), "saved").unwrap(), expected);
        model_history(home.path(), Some("2026-01-03T00:00:00Z"), "external-model");
        assert_eq!(
            read(home.path(), "saved").unwrap()["model"],
            "external-model"
        );
        assert_eq!(
            read(home.path(), "saved").unwrap()["modelProvider"],
            "turn-provider"
        );
        for timestamp in [None, Some("not-a-date"), Some("2026-01-02T00:00:00Z")] {
            model_history(home.path(), timestamp, "legacy-model");
            assert_eq!(read(home.path(), "saved").unwrap(), expected);
        }
    }

    #[test]
    fn confirmation_without_history_uses_receive_time_for_invalid_timestamp() {
        let home = tempfile::tempdir().unwrap();
        for (id, time) in [
            ("absent", None),
            ("negative", Some(-1)),
            ("future", Some(i64::MAX)),
        ] {
            let before = chrono::Utc::now().timestamp_millis();
            remember_settings(home.path(), id, "route/model", None, time).unwrap();
            let after = chrono::Utc::now().timestamp_millis();
            let record = confirmed_model(&home.path().canonicalize().unwrap(), id)
                .unwrap()
                .unwrap();
            assert!((before..=after).contains(&record.selected_at_ms));
            assert_eq!(read(home.path(), id).unwrap()["model"], "route/model");
        }
    }

    #[test]
    fn confirmed_selection_does_not_bypass_rollout_identity_errors() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('saved','session.jsonl');",
        )
        .unwrap();
        remember_settings(home.path(), "saved", "route/model", None, None).unwrap();
        for body in [
            "invalid",
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"other\"}}",
        ] {
            std::fs::write(home.path().join("session.jsonl"), body).unwrap();
            assert!(read(home.path(), "saved").is_err());
        }
    }

    #[test]
    fn confirmed_model_preserves_catalog_names_with_internal_spaces() {
        let home = tempfile::tempdir().unwrap();
        remember_settings(home.path(), "saved", " route/model name ", None, None).unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap()["model"],
            "route/model name"
        );
    }

    #[test]
    fn rejects_invalid_confirmation_and_store_without_mutation() {
        let home = tempfile::tempdir().unwrap();
        for id in ["", "../saved", "with/slash"] {
            assert!(remember_settings(home.path(), id, "model", None, None).is_err());
        }
        for model in ["", "  ", "with\nnewline", "with\0null"] {
            assert!(remember_settings(home.path(), "saved", model, None, None).is_err());
        }
        assert!(remember_settings(home.path(), "saved", "model", Some(" "), None).is_err());
        let store = home.path().join("codey-session-models");
        assert!(!store.exists());
        std::fs::create_dir(&store).unwrap();
        let path = store.join("saved.json");
        for content in [
            "invalid".to_owned(),
            json!({"thread_id":"other","model":"model","provider":null,"selected_at_ms":1})
                .to_string(),
            json!({"thread_id":"saved","model":"model","provider":null,"selected_at_ms":0})
                .to_string(),
        ] {
            std::fs::write(&path, &content).unwrap();
            assert!(read(home.path(), "saved").is_err());
            assert!(remember_settings(home.path(), "saved", "new", None, None).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
            assert_eq!(std::fs::read_dir(&store).unwrap().count(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_store_escape_and_uses_private_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = home.path().join("codey-session-models");
        symlink(outside.path(), &store).unwrap();
        assert!(remember_settings(home.path(), "saved", "model", None, None).is_err());
        assert!(read(home.path(), "saved").is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        std::fs::remove_file(&store).unwrap();
        remember_settings(home.path(), "saved", "model", None, None).unwrap();
        assert_eq!(
            std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(store.join("saved.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let bytes = std::fs::read(store.join("saved.json")).unwrap();
        symlink(store.join("saved.json"), store.join("other.json")).unwrap();
        assert!(remember_settings(home.path(), "other", "new", None, None).is_err());
        assert!(read(home.path(), "other").is_err());
        assert_eq!(std::fs::read(store.join("saved.json")).unwrap(), bytes);
    }

    #[test]
    fn concurrent_notifications_keep_latest_selection() {
        let home = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for timestamp in 1..=12 {
                let home = home.path();
                scope.spawn(move || {
                    remember_settings(
                        home,
                        "saved",
                        &format!("model-{timestamp}"),
                        None,
                        Some(timestamp),
                    )
                    .unwrap()
                });
            }
        });
        assert_eq!(read(home.path(), "saved").unwrap()["model"], "model-12");
    }

    #[test]
    fn native_compaction_source_uses_compacted_origin_and_expires_after_portable_summary() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("session.jsonl");
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT, model TEXT);
            INSERT INTO threads VALUES ('saved','session.jsonl','target/new');",
        )
        .unwrap();
        let identity =
            json!({"type":"session_meta","payload":{"id":"saved","model_provider":"codey_router"}});
        let native = json!({"type":"compacted","payload":{
            "replacement_history":[{"type":"compaction","encrypted_content":"opaque"}],
            "resume_metadata":{"previous_turn_settings":{"model":"source/old"}}
        }});
        let target = json!({"type":"turn_context","payload":{"model":"target/new"}});
        let write = |records: &[Value]| {
            std::fs::write(
                &path,
                records
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap()
        };
        write(&[identity.clone(), native.clone(), target.clone()]);
        assert_eq!(
            native_compaction_source(home.path(), "saved")
                .unwrap()
                .as_deref(),
            Some("source/old")
        );
        assert_eq!(read(home.path(), "saved").unwrap()["model"], "target/new");
        // 旧会话的创建时 provider 不随恢复更新；压缩来源以该记录自身的模型为准。
        let original_provider =
            json!({"type":"session_meta","payload":{"id":"saved","model_provider":"openai"}});
        write(&[original_provider.clone(), target.clone()]);
        assert_eq!(
            native_compaction_source(home.path(), "saved").unwrap(),
            None
        );
        write(&[original_provider, native.clone(), target.clone()]);
        assert_eq!(
            native_compaction_source(home.path(), "saved")
                .unwrap()
                .as_deref(),
            Some("source/old")
        );
        let portable = json!({"type":"compacted","payload":{"replacement_history":[
            {"type":"compaction","encrypted_content":"codey-summary-v1:portable"}
        ]}});
        write(&[identity.clone(), native.clone(), target, portable]);
        assert_eq!(
            native_compaction_source(home.path(), "saved").unwrap(),
            None
        );
        let mut unknown = native.clone();
        unknown["payload"]["resume_metadata"]["previous_turn_settings"]["model"] =
            json!("raw-model");
        write(&[identity.clone(), unknown]);
        assert!(native_compaction_source(home.path(), "saved").is_err());
        write(&[
            json!({"type":"session_meta","payload":{"id":"other","model_provider":"codey_router"}}),
            native,
        ]);
        assert!(native_compaction_source(home.path(), "saved").is_err());
        let outside = tempfile::NamedTempFile::new().unwrap();
        db.execute(
            "UPDATE threads SET rollout_path=?1",
            [outside.path().to_str().unwrap()],
        )
        .unwrap();
        assert!(
            native_compaction_source(home.path(), "saved")
                .unwrap_err()
                .to_string()
                .contains("不在")
        );
    }

    #[test]
    fn latest_valid_rollout_model_takes_priority_over_database_default() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join("state_5.sqlite");
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, rollout_path TEXT);
             INSERT INTO threads VALUES ('saved', 'default', 'database-provider', 'session.jsonl');",
        )
        .unwrap();
        let path = home.path().join("session.jsonl");
        let records = [
            json!({"type":"session_meta","payload":{"id":"saved","model_provider":"session-provider"}}),
            json!({"type":"turn_context","payload":{"model":"old","model_provider":"old-provider"}}),
            json!({"type":"turn_context","payload":{"model":"latest","model_provider":"latest-provider"}}),
            json!({"type":"turn_context","payload":{"model":"  ","model_provider":"ignored-provider"}}),
            json!({"type":"turn_context","payload":{"model_provider":"also-ignored"}}),
        ];
        let content = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, &content).unwrap();
        let db_before = std::fs::read(&db_path).unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({"status":"ok","threadId":"saved","model":"latest","modelProvider":"latest-provider"})
        );
        assert_eq!(std::fs::read(&db_path).unwrap(), db_before);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);

        // 无 provider 的新回合不继承上一模型或数据库模型的 provider。
        std::fs::write(
            &path,
            format!(
                "{content}\n{}",
                json!({"type":"turn_context","payload":{"model":"newest"}})
            ),
        )
        .unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({"status":"ok","threadId":"saved","model":"newest","modelProvider":"session-provider"})
        );
    }

    #[test]
    fn database_model_is_fallback_only_for_missing_rollout_or_model() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, rollout_path TEXT);
             INSERT INTO threads VALUES ('saved', 'default', 'database-provider', 'session.jsonl');",
        ).unwrap();
        let expected = json!({"status":"ok","threadId":"saved","model":"default","modelProvider":"database-provider"});
        assert_eq!(read(home.path(), "saved").unwrap(), expected);
        let path = home.path().join("session.jsonl");
        std::fs::write(&path, json!({"type":"session_meta","payload":{"id":"saved","model_provider":"other-provider"}}).to_string()).unwrap();
        assert_eq!(read(home.path(), "saved").unwrap(), expected);

        std::fs::write(
            &path,
            json!({"type":"session_meta","payload":{"id":"other"}}).to_string(),
        )
        .unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap_err()
                .to_string()
                .contains("不匹配")
        );
        std::fs::write(&path, "invalid metadata").unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap_err()
                .to_string()
                .contains("无法确认")
        );
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read(home.path(), "saved").is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read(home.path(), "saved").is_err());
        let outside = tempfile::NamedTempFile::new().unwrap();
        db.execute(
            "UPDATE threads SET rollout_path=?1",
            [outside.path().to_str().unwrap()],
        )
        .unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap_err()
                .to_string()
                .contains("不在")
        );
    }

    #[test]
    fn missing_model_is_distinct_from_corrupt_metadata() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            read(home.path(), "empty").unwrap(),
            json!({"status":"missing", "threadId":"empty"})
        );
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT); INSERT INTO threads VALUES ('legacy','session.jsonl');").unwrap();
        std::fs::write(
            home.path().join("session.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"legacy\"}}\n",
        )
        .unwrap();
        assert_eq!(read(home.path(), "legacy").unwrap()["status"], "missing");
        drop(db);
        std::fs::write(home.path().join("state_5.sqlite"), "invalid sqlite").unwrap();
        assert!(read(home.path(), "legacy").is_err());
    }

    #[test]
    fn reads_saved_model_without_modifying_database_or_returning_body() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, body TEXT);
            INSERT INTO threads VALUES ('saved', 'account/astra', 'codey_router', 'private conversation');").unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap(),
            json!({
                "status":"ok", "threadId":"saved", "model":"account/astra", "modelProvider":"codey_router"
            })
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!home.path().join("state_5.sqlite-wal").exists());
    }

    #[test]
    fn falls_back_to_last_turn_metadata_for_legacy_catalogs() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("session.jsonl");
        std::fs::write(&path, concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"saved\",\"model_provider\":\"codey_router\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"account/old\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"content\":\"private conversation\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"account/astra\"}}\n"
        )).unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('saved', 'session.jsonl');",
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            read(home.path(), "saved").unwrap()["model"],
            "account/astra"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(read(home.path(), "../saved").is_err());
        assert_eq!(read(home.path(), "missing").unwrap()["status"], "missing");
    }

    #[test]
    fn rejects_outside_rollouts_and_mismatched_identity() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, rollout_path TEXT);")
            .unwrap();
        db.execute(
            "INSERT INTO threads VALUES ('saved', ?1)",
            [outside.path().to_str().unwrap()],
        )
        .unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap_err()
                .to_string()
                .contains("不在")
        );
        std::fs::write(
            home.path().join("session.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"different\"}}\n",
        )
        .unwrap();
        db.execute("UPDATE threads SET rollout_path='session.jsonl'", [])
            .unwrap();
        assert!(
            read(home.path(), "saved")
                .unwrap_err()
                .to_string()
                .contains("不匹配")
        );
    }
}
