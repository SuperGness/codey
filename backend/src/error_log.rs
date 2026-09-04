use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::Context;
use chrono::{DateTime, FixedOffset, NaiveDate, SecondsFormat, Utc};
use codey_runtime_core::app_paths::{codex_app_version, resolve_codex_app_dir_with_saved};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const ERROR_LOG_FILE: &str = "codey-errors.log";
const ERROR_LOG_HELPER_ARGUMENT: &str = "--codey-record-error";
const MAX_HELPER_INPUT_BYTES: u64 = 1024 * 1024;
const MAX_HELPER_RECORDS: usize = 64;
const MAX_LOG_FIELD_BYTES: usize = 8 * 1024;
const MAX_LOG_ERROR_BYTES: usize = 16 * 1024;
const MAX_LOG_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_LOG_CONTEXT_DEPTH: usize = 12;
const MAX_LOG_COLLECTION_ITEMS: usize = 128;
const MAX_LOG_FILE_BYTES: u64 = 16 * 1024 * 1024;
const REDACTED_VALUE: &str = "[REDACTED]";
const TRUNCATED_VALUE: &str = "[TRUNCATED]";
const BEIJING_OFFSET_SECONDS: i32 = 8 * 60 * 60;
const FAILURE_DEDUP_WINDOW: Duration = Duration::from_secs(600);
const FAILURE_DEDUP_MAX_KEYS: usize = 64;
static ERROR_LOG_WRITER: OnceLock<Mutex<ErrorLogWriter>> = OnceLock::new();
static PANIC_LOG_HOOK: OnceLock<()> = OnceLock::new();
static FAILURE_DEDUP: OnceLock<Mutex<FailureDedupCache>> = OnceLock::new();
static CODEX_APP_VERSION: OnceLock<RwLock<Option<String>>> = OnceLock::new();

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    electron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chrome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<String>,
}

impl ErrorVersions {
    fn current() -> Self {
        Self {
            codey: Some(env!("CARGO_PKG_VERSION").to_string()),
            codex: cached_codex_app_version(),
            ..Self::default()
        }
    }
}

fn cached_codex_app_version() -> Option<String> {
    CODEX_APP_VERSION
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()?
        .clone()
}

pub fn refresh_codex_app_version(
    app_dir: Option<&Path>,
    saved_app_path: Option<&str>,
) -> Option<String> {
    let version = resolve_codex_app_dir_with_saved(app_dir, saved_app_path)
        .as_deref()
        .and_then(codex_app_version);
    if let Ok(mut cached) = CODEX_APP_VERSION.get_or_init(|| RwLock::new(None)).write() {
        *cached = version.clone();
    }
    version
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorRecord {
    timestamp: String,
    platform: String,
    #[serde(default)]
    versions: ErrorVersions,
    event: String,
    operation: String,
    error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recoverable: Option<bool>,
    #[serde(default, skip_serializing_if = "is_empty_context")]
    context: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ErrorHelperInput {
    Record(Box<ErrorRecord>),
    Batch(Vec<ErrorRecord>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailureMetadata {
    pub stage: Option<String>,
    pub recoverable: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FailureDedupKey {
    event: String,
    operation: String,
    error: String,
}

#[derive(Debug)]
struct FailureDedupEntry {
    last_emitted: Instant,
    suppressed: u64,
}

#[derive(Debug, Default)]
struct FailureDedupCache {
    entries: HashMap<FailureDedupKey, FailureDedupEntry>,
}

#[derive(Debug, PartialEq, Eq)]
enum FailureDedupDecision {
    Emit { suppressed: u64 },
    Suppress,
}

impl FailureDedupCache {
    // A watchdog on a stuck renderer otherwise appends the identical record
    // every cycle for the whole session. Repeats inside the window are counted
    // and folded into the next emitted record as `suppressedRepeats`.
    fn decide(&mut self, key: FailureDedupKey, now: Instant) -> FailureDedupDecision {
        if let Some(entry) = self.entries.get_mut(&key) {
            if now.duration_since(entry.last_emitted) < FAILURE_DEDUP_WINDOW {
                entry.suppressed = entry.suppressed.saturating_add(1);
                return FailureDedupDecision::Suppress;
            }
            let suppressed = entry.suppressed;
            entry.last_emitted = now;
            entry.suppressed = 0;
            return FailureDedupDecision::Emit { suppressed };
        }
        if self.entries.len() >= FAILURE_DEDUP_MAX_KEYS {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_emitted)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            FailureDedupEntry {
                last_emitted: now,
                suppressed: 0,
            },
        );
        FailureDedupDecision::Emit { suppressed: 0 }
    }
}

fn failure_dedup_decide(key: FailureDedupKey) -> FailureDedupDecision {
    FAILURE_DEDUP
        .get_or_init(|| Mutex::new(FailureDedupCache::default()))
        .lock()
        .map(|mut cache| cache.decide(key, Instant::now()))
        // Logging must never be lost just because the dedup lock is poisoned.
        .unwrap_or(FailureDedupDecision::Emit { suppressed: 0 })
}

fn beijing_offset() -> FixedOffset {
    FixedOffset::east_opt(BEIJING_OFFSET_SECONDS).expect("valid Beijing UTC offset")
}

fn beijing_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&beijing_offset())
}

fn format_beijing_timestamp(timestamp: DateTime<FixedOffset>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn normalize_beijing_timestamp(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|timestamp| format_beijing_timestamp(timestamp.with_timezone(&beijing_offset())))
        .unwrap_or_else(|_| format_beijing_timestamp(beijing_now()))
}

fn is_empty_context(context: &Value) -> bool {
    match context {
        Value::Null => true,
        Value::Object(values) => values.is_empty(),
        Value::Array(values) => values.is_empty(),
        _ => false,
    }
}

fn take_context_version(context: &mut Value, key: &str) -> Option<String> {
    let values = context.as_object_mut()?;
    let version = values
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty())?
        .to_string();
    values.remove(key);
    Some(version)
}

fn normalize_record(record: &mut ErrorRecord) {
    record.timestamp = normalize_beijing_timestamp(&record.timestamp);
    if record.platform.trim().is_empty() {
        record.platform = std::env::consts::OS.to_string();
    }
    record.versions.codey = Some(env!("CARGO_PKG_VERSION").to_string());
    record.versions.codex = record
        .versions
        .codex
        .take()
        .or_else(|| take_context_version(&mut record.context, "codexVersion"))
        .or_else(cached_codex_app_version);
    record.versions.electron = record
        .versions
        .electron
        .take()
        .or_else(|| take_context_version(&mut record.context, "electronVersion"));
    record.versions.chrome = record
        .versions
        .chrome
        .take()
        .or_else(|| take_context_version(&mut record.context, "chromeVersion"));
    record.versions.node = record
        .versions
        .node
        .take()
        .or_else(|| take_context_version(&mut record.context, "nodeVersion"));
    sanitize_record(record);
}

fn sanitize_record(record: &mut ErrorRecord) {
    sanitize_log_text(&mut record.event, MAX_LOG_FIELD_BYTES);
    sanitize_log_text(&mut record.operation, MAX_LOG_FIELD_BYTES);
    sanitize_log_text(&mut record.error, MAX_LOG_ERROR_BYTES);
    if let Some(stage) = &mut record.stage {
        sanitize_log_text(stage, MAX_LOG_FIELD_BYTES);
    }
    for version in [
        &mut record.versions.codey,
        &mut record.versions.codex,
        &mut record.versions.electron,
        &mut record.versions.chrome,
        &mut record.versions.node,
    ]
    .into_iter()
    .flatten()
    {
        sanitize_log_text(version, 256);
    }
    sanitize_context(&mut record.context);
}

fn sanitize_context(context: &mut Value) {
    sanitize_json_value(context, 0);
    let serialized_bytes = serde_json::to_vec(context)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if serialized_bytes > MAX_LOG_CONTEXT_BYTES {
        *context = serde_json::json!({
            "contextTruncated": true,
            "originalBytes": serialized_bytes,
        });
    }
}

fn sanitize_json_value(value: &mut Value, depth: usize) {
    if depth >= MAX_LOG_CONTEXT_DEPTH {
        *value = Value::String(TRUNCATED_VALUE.to_string());
        return;
    }
    match value {
        Value::Object(values) => {
            if values.len() > MAX_LOG_COLLECTION_ITEMS {
                let original_len = values.len();
                let retained = std::mem::take(values)
                    .into_iter()
                    .take(MAX_LOG_COLLECTION_ITEMS)
                    .collect();
                *values = retained;
                values.insert(
                    "__codeyTruncatedItems".to_string(),
                    Value::from(original_len - MAX_LOG_COLLECTION_ITEMS),
                );
            }
            for (key, value) in values.iter_mut() {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED_VALUE.to_string());
                } else {
                    sanitize_json_value(value, depth + 1);
                }
            }
        }
        Value::Array(values) => {
            if values.len() > MAX_LOG_COLLECTION_ITEMS {
                values.truncate(MAX_LOG_COLLECTION_ITEMS);
                values.push(serde_json::json!({ "__codeyTruncatedItems": true }));
            }
            for value in values {
                sanitize_json_value(value, depth + 1);
            }
        }
        Value::String(value) => sanitize_log_text(value, MAX_LOG_FIELD_BYTES),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "apikey"
            | "xapikey"
            | "key"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "bottoken"
            | "secret"
            | "clientsecret"
            | "password"
            | "passwd"
            | "cookie"
            | "setcookie"
            | "credential"
            | "credentials"
            | "webhook"
            | "webhookurl"
    ) || normalized.contains("authorization")
        || normalized.ends_with("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
        || normalized.ends_with("cookie")
        || normalized.ends_with("credential")
        || normalized.ends_with("webhookurl")
}

fn sanitize_log_text(value: &mut String, max_bytes: usize) {
    *value = redact_embedded_urls(&redact_labeled_secrets(value));
    truncate_utf8(value, max_bytes);
}

fn redact_labeled_secrets(input: &str) -> String {
    let mut value = input.to_string();
    for label in [
        "authorization:",
        "proxy-authorization:",
        "x-api-key:",
        "api-key:",
        "cookie:",
        "set-cookie:",
    ] {
        value = redact_after_label(&value, label, true);
    }
    for label in ["bearer ", "basic "] {
        value = redact_after_label(&value, label, false);
    }
    value
}

fn redact_after_label(input: &str, label: &str, until_line_end: bool) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = find_ascii_case_insensitive(&input[cursor..], label) {
        let start = cursor + relative;
        let value_start = start + label.len();
        output.push_str(&input[cursor..value_start]);
        output.push_str(REDACTED_VALUE);
        let mut end = value_start;
        for (offset, character) in input[value_start..].char_indices() {
            let stop = if until_line_end {
                matches!(character, '\n' | '\r')
            } else {
                character.is_whitespace()
                    || matches!(
                        character,
                        ',' | ';' | '"' | '\'' | '<' | '>' | ')' | ']' | '}'
                    )
            };
            if stop {
                end = value_start + offset;
                break;
            }
            end = value_start + offset + character.len_utf8();
        }
        cursor = end;
        if cursor == value_start && value_start == input.len() {
            break;
        }
    }
    output.push_str(&input[cursor..]);
    output
}

fn find_ascii_case_insensitive(input: &str, needle: &str) -> Option<usize> {
    let input = input.as_bytes();
    let needle = needle.as_bytes();
    input
        .windows(needle.len())
        .position(|candidate| candidate.eq_ignore_ascii_case(needle))
}

fn redact_embedded_urls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    loop {
        let http = find_ascii_case_insensitive(&input[cursor..], "http://");
        let https = find_ascii_case_insensitive(&input[cursor..], "https://");
        let Some(relative) = [http, https].into_iter().flatten().min() else {
            output.push_str(&input[cursor..]);
            break;
        };
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let mut end = input.len();
        for (offset, character) in input[start..].char_indices() {
            if character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | '`') {
                end = start + offset;
                break;
            }
        }
        let mut candidate_end = end;
        while candidate_end > start {
            let Some(character) = input[start..candidate_end].chars().next_back() else {
                break;
            };
            if !matches!(character, ',' | ';' | '.' | ')' | ']' | '}') {
                break;
            }
            candidate_end -= character.len_utf8();
        }
        let candidate = &input[start..candidate_end];
        output.push_str(sanitize_url(candidate).as_deref().unwrap_or(candidate));
        output.push_str(&input[candidate_end..end]);
        cursor = end;
    }
    output
}

fn sanitize_url(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    let mut changed = false;
    if !url.username().is_empty() || url.password().is_some() {
        let _ = url.set_password(None);
        let _ = url.set_username("");
        changed = true;
    }
    let query = url.query_pairs().into_owned().collect::<Vec<_>>();
    if query.iter().any(|(key, _)| is_sensitive_query_key(key)) {
        let sanitized = query
            .into_iter()
            .map(|(key, value)| {
                if is_sensitive_query_key(&key) {
                    (key, "REDACTED".to_string())
                } else {
                    (key, value)
                }
            })
            .collect::<Vec<_>>();
        url.query_pairs_mut().clear().extend_pairs(sanitized);
        changed = true;
    }
    if url.fragment().is_some() {
        url.set_fragment(Some("REDACTED"));
        changed = true;
    }
    let lower_path = url.path().to_ascii_lowercase();
    if let Some((index, marker_len)) = lower_path
        .find("/hook/")
        .map(|index| (index, "/hook/".len()))
        .or_else(|| {
            lower_path
                .find("/webhook/")
                .map(|index| (index, "/webhook/".len()))
        })
    {
        let marker_end = index + marker_len;
        let mut prefix = url.path()[..marker_end].to_string();
        prefix.push_str("REDACTED");
        url.set_path(&prefix);
        changed = true;
    } else if lower_path.starts_with("/bot") && lower_path.len() > 4 {
        url.set_path("/botREDACTED");
        changed = true;
    }
    changed.then(|| url.to_string())
}

fn is_sensitive_query_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "key"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "apikey"
            | "secret"
            | "password"
            | "passwd"
            | "auth"
            | "authorization"
            | "signature"
            | "sig"
            | "credential"
            | "code"
    )
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    const SUFFIX: &str = "…[truncated]";
    let mut end = max_bytes.saturating_sub(SUFFIX.len()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str(SUFFIX);
}

#[derive(Default)]
struct ErrorLogWriter;

impl ErrorLogWriter {
    fn clear_if_stale(&mut self, path: &Path, today: NaiveDate) -> std::io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        with_log_file_lock(path, || {
            if !path.exists() || !file_is_from_different_day(path, today)? {
                return Ok(());
            }
            OpenOptions::new().write(true).truncate(true).open(path)?;
            Ok(())
        })
    }

    fn append(&mut self, path: &Path, today: NaiveDate, line: &str) -> std::io::Result<()> {
        with_log_file_lock(path, || {
            let incoming_bytes = u64::try_from(line.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let size_limit_reached = fs::metadata(path)
                .map(|metadata| metadata.len().saturating_add(incoming_bytes) > MAX_LOG_FILE_BYTES)
                .unwrap_or(false);
            let truncate = file_is_from_different_day(path, today)? || size_limit_reached;
            if !truncate {
                repair_incomplete_tail(path)?;
            }
            let mut options = OpenOptions::new();
            options.create(true);
            #[cfg(unix)]
            options.mode(0o600);
            if truncate {
                options.write(true).truncate(true);
            } else {
                options.append(true);
            }
            let mut file = options.open(path)?;
            #[cfg(unix)]
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            let mut complete_line = String::with_capacity(line.len().saturating_add(1));
            complete_line.push_str(line);
            complete_line.push('\n');
            file.write_all(complete_line.as_bytes())?;
            file.flush()
        })
    }
}

fn with_log_file_lock<T>(
    log_path: &Path,
    action: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = log_path.with_extension("lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let lock_file = options.open(lock_path)?;
    fs2::FileExt::lock_exclusive(&lock_file)?;
    let result = action();
    let unlock_result = fs2::FileExt::unlock(&lock_file);
    match (result, unlock_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn repair_incomplete_tail(path: &Path) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    // 只探测文件尾部：修复目标只有最后一行，整文件读取会让持续失败的记录
    // 路径退化为 O(日志大小)。窗口内没有换行时按倍扩大，语义与全量读一致。
    const TAIL_PROBE_BYTES: u64 = 64 * 1024;
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    let mut probe = TAIL_PROBE_BYTES;
    let (line_start, tail) = loop {
        let start = len.saturating_sub(probe);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity(usize::try_from(len - start).unwrap_or_default());
        file.read_to_end(&mut bytes)?;
        if bytes.last() == Some(&b'\n') {
            return Ok(());
        }
        if let Some(index) = bytes.iter().rposition(|byte| *byte == b'\n') {
            let line_start = start.saturating_add(index as u64).saturating_add(1);
            break (line_start, bytes.split_off(index.saturating_add(1)));
        }
        if start == 0 {
            break (0, bytes);
        }
        probe = probe.saturating_mul(2);
    };
    if serde_json::from_slice::<Value>(&tail).is_ok() {
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
    } else {
        file.set_len(line_start)?;
    }
    file.flush()
}

pub fn initialize() {
    let path = error_log_path();
    let today = beijing_now().date_naive();
    let writer = ERROR_LOG_WRITER.get_or_init(|| Mutex::new(ErrorLogWriter));
    let result = writer
        .lock()
        .map_err(|_| std::io::Error::other("Codey error log lock is poisoned"))
        .and_then(|mut writer| writer.clear_if_stale(&path, today));
    if let Err(error) = result {
        eprintln!("清理过期 Codey 错误日志失败：{error}");
    }
}

pub fn record_failure(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    context: impl Serialize,
) {
    record_failure_with_metadata(event, operation, error, FailureMetadata::default(), context);
}

pub fn record_failure_with_metadata(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    metadata: FailureMetadata,
    context: impl Serialize,
) {
    let now = beijing_now();
    let mut event = event.into();
    let mut operation = operation.into();
    let mut error = error.into();
    let mut context = serde_json::to_value(context).unwrap_or_else(|serialization_error| {
        serde_json::json!({
            "contextSerializationError": serialization_error.to_string(),
        })
    });
    sanitize_log_text(&mut event, MAX_LOG_FIELD_BYTES);
    sanitize_log_text(&mut operation, MAX_LOG_FIELD_BYTES);
    sanitize_log_text(&mut error, MAX_LOG_ERROR_BYTES);
    sanitize_context(&mut context);
    match failure_dedup_decide(FailureDedupKey {
        event: event.clone(),
        operation: operation.clone(),
        error: error.clone(),
    }) {
        FailureDedupDecision::Suppress => return,
        FailureDedupDecision::Emit { suppressed } => {
            if suppressed > 0
                && let Some(values) = context.as_object_mut()
            {
                values.insert("suppressedRepeats".to_string(), Value::from(suppressed));
            }
        }
    }
    let mut record = ErrorRecord {
        timestamp: format_beijing_timestamp(now),
        platform: std::env::consts::OS.to_string(),
        versions: ErrorVersions::current(),
        event,
        operation,
        error,
        stage: metadata.stage,
        recoverable: metadata.recoverable,
        context,
    };
    sanitize_record(&mut record);
    if let Err(error) = append_record(&record, now.date_naive()) {
        eprintln!("写入 Codey 错误日志失败：{error}");
    }
}

pub fn record_process_failure(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    stage: impl Into<String>,
) {
    record_process_failure_with_recoverability(event, operation, error, stage, false);
}

pub fn record_process_failure_with_recoverability(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    stage: impl Into<String>,
    recoverable: bool,
) {
    record_failure_with_metadata(
        event,
        operation,
        error,
        FailureMetadata {
            stage: Some(stage.into()),
            recoverable: Some(recoverable),
        },
        serde_json::json!({}),
    );
}

pub fn install_panic_hook(component: &'static str, stage: &'static str) {
    PANIC_LOG_HOOK.get_or_init(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let message = panic_info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| {
                    panic_info
                        .payload()
                        .downcast_ref::<String>()
                        .map(String::as_str)
                })
                .unwrap_or("unknown panic");
            let location = panic_info
                .location()
                .map(|location| {
                    format!(
                        "{}:{}:{}",
                        location.file(),
                        location.line(),
                        location.column()
                    )
                })
                .unwrap_or_else(|| "unknown".to_string());
            record_failure_with_metadata(
                "process_panicked",
                "uncaught_panic",
                message,
                FailureMetadata {
                    stage: Some(stage.to_string()),
                    recoverable: Some(false),
                },
                serde_json::json!({
                    "component": component,
                    "location": location,
                }),
            );
            previous_hook(panic_info);
        }));
    });
}

pub async fn record_failure_async<C>(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    context: C,
) where
    C: Serialize + Send + 'static,
{
    record_failure_with_metadata_async(
        event,
        operation,
        error,
        FailureMetadata::default(),
        context,
    )
    .await;
}

pub async fn record_failure_with_metadata_async<C>(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    metadata: FailureMetadata,
    context: C,
) where
    C: Serialize + Send + 'static,
{
    let event = event.into();
    let operation = operation.into();
    let error = error.into();
    if let Err(join_error) = tokio::task::spawn_blocking(move || {
        record_failure_with_metadata(event, operation, error, metadata, context);
    })
    .await
    {
        eprintln!("Codey 错误日志写入任务异常退出：{join_error}");
    }
}

pub fn run_helper_if_requested() -> anyhow::Result<bool> {
    if std::env::args_os()
        .nth(1)
        .is_none_or(|argument| argument != ERROR_LOG_HELPER_ARGUMENT)
    {
        return Ok(false);
    }

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_HELPER_INPUT_BYTES.saturating_add(1))
        .read_to_string(&mut input)
        .context("读取 Codey 错误日志 helper 输入失败")?;
    anyhow::ensure!(
        u64::try_from(input.len()).unwrap_or(u64::MAX) <= MAX_HELPER_INPUT_BYTES,
        "Codey 错误日志 helper 输入过大"
    );
    let records = parse_helper_records(&input)?;
    append_records(&records, beijing_now().date_naive())
        .context("Codey 错误日志 helper 写入失败")?;
    Ok(true)
}

fn parse_helper_records(input: &str) -> anyhow::Result<Vec<ErrorRecord>> {
    let input: ErrorHelperInput =
        serde_json::from_str(input).context("解析 Codey 错误日志 helper 输入失败")?;
    let mut records = match input {
        ErrorHelperInput::Record(record) => vec![*record],
        ErrorHelperInput::Batch(records) => records,
    };
    anyhow::ensure!(!records.is_empty(), "Codey 错误日志 helper 批次为空");
    anyhow::ensure!(
        records.len() <= MAX_HELPER_RECORDS,
        "Codey 错误日志 helper 批次过大"
    );
    for (index, record) in records.iter_mut().enumerate() {
        anyhow::ensure!(
            !record.event.trim().is_empty()
                && !record.operation.trim().is_empty()
                && !record.error.trim().is_empty(),
            "Codey 错误日志 helper 第 {} 条记录缺少失败信息",
            index + 1
        );
        normalize_record(record);
    }
    Ok(records)
}

fn append_record(record: &ErrorRecord, today: NaiveDate) -> anyhow::Result<()> {
    append_records(std::slice::from_ref(record), today)
}

fn append_records(records: &[ErrorRecord], today: NaiveDate) -> anyhow::Result<()> {
    anyhow::ensure!(!records.is_empty(), "Codey 错误日志批次为空");
    let lines = records
        .iter()
        .cloned()
        .map(|mut record| {
            sanitize_record(&mut record);
            serde_json::to_string(&record)
        })
        .collect::<Result<Vec<_>, _>>()
        .context("序列化 Codey 错误日志失败")?
        .join("\n");
    let path = error_log_path();
    let writer = ERROR_LOG_WRITER.get_or_init(|| Mutex::new(ErrorLogWriter));
    writer
        .lock()
        .map_err(|_| anyhow::anyhow!("Codey error log lock is poisoned"))?
        .append(&path, today, &lines)
        .map_err(anyhow::Error::from)
}

pub fn error_log_path() -> PathBuf {
    codey_runtime_core::paths::default_app_state_dir().join(ERROR_LOG_FILE)
}

fn file_is_from_different_day(path: &Path, today: NaiveDate) -> std::io::Result<bool> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let modified = metadata.modified()?;
    Ok(DateTime::<Utc>::from(modified)
        .with_timezone(&beijing_offset())
        .date_naive()
        != today)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_log_sanitization_removes_credentials_and_url_secrets() {
        let mut context = serde_json::json!({
            "authorization": "Bearer authorization-secret",
            "nested": {
                "apiKey": "api-key-secret",
                "tokenCount": 42,
                "url": "https://user:password@example.com/open-apis/bot/v2/hook/hook-secret?token=query-secret&mode=safe#fragment-secret",
                "message": "request failed with Bearer bearer-secret",
                "webhookUrl": "https://example.com/hook/field-secret"
            }
        });

        sanitize_context(&mut context);
        let serialized = serde_json::to_string(&context).unwrap();

        for secret in [
            "authorization-secret",
            "api-key-secret",
            "user",
            "password",
            "hook-secret",
            "query-secret",
            "fragment-secret",
            "bearer-secret",
            "field-secret",
        ] {
            assert!(!serialized.contains(secret), "secret leaked: {secret}");
        }
        assert_eq!(context["nested"]["tokenCount"], 42);
        assert!(serialized.contains("mode=safe"));
        assert!(serialized.contains("REDACTED"));
    }

    #[test]
    fn log_text_sanitization_handles_headers_and_embedded_urls() {
        let mut message = "Authorization: Bearer header-secret\nrequest https://name:pass@example.com/v1?api_key=url-secret&debug=1 failed"
            .to_string();

        sanitize_log_text(&mut message, MAX_LOG_ERROR_BYTES);

        assert!(!message.contains("header-secret"));
        assert!(!message.contains("name:pass"));
        assert!(!message.contains("url-secret"));
        assert!(message.contains("debug=1"));
    }

    #[test]
    fn log_fields_and_context_have_bounded_sizes() {
        let mut field = "界".repeat(MAX_LOG_FIELD_BYTES);
        sanitize_log_text(&mut field, MAX_LOG_FIELD_BYTES);
        assert!(field.len() <= MAX_LOG_FIELD_BYTES);
        assert!(field.ends_with("[truncated]"));

        let mut context = serde_json::json!({
            "items": (0..32)
                .map(|_| "x".repeat(MAX_LOG_FIELD_BYTES))
                .collect::<Vec<_>>()
        });
        sanitize_context(&mut context);
        assert_eq!(context["contextTruncated"], true);
        assert!(serde_json::to_vec(&context).unwrap().len() < MAX_LOG_CONTEXT_BYTES);
    }

    #[test]
    fn repeated_failures_are_suppressed_within_the_dedup_window() {
        let mut cache = FailureDedupCache::default();
        let key = || FailureDedupKey {
            event: "injection_health_check_failed".to_string(),
            operation: "check_cdp_bridge_health".to_string(),
            error: "timed out waiting for CDP command".to_string(),
        };
        let start = Instant::now();

        assert_eq!(
            cache.decide(key(), start),
            FailureDedupDecision::Emit { suppressed: 0 }
        );
        for step in 1..=3_u64 {
            assert_eq!(
                cache.decide(key(), start + Duration::from_secs(35 * step)),
                FailureDedupDecision::Suppress
            );
        }
        assert_eq!(
            cache.decide(key(), start + FAILURE_DEDUP_WINDOW + Duration::from_secs(1)),
            FailureDedupDecision::Emit { suppressed: 3 }
        );
        // The counter resets after an emit, so the cadence stays one record
        // per window no matter how long the renderer stays stuck.
        assert_eq!(
            cache.decide(key(), start + FAILURE_DEDUP_WINDOW + Duration::from_secs(2)),
            FailureDedupDecision::Suppress
        );
    }

    #[test]
    fn dedup_cache_evicts_the_oldest_key_when_full() {
        let mut cache = FailureDedupCache::default();
        let start = Instant::now();
        for index in 0..FAILURE_DEDUP_MAX_KEYS {
            let key = FailureDedupKey {
                event: format!("event-{index}"),
                operation: "op".to_string(),
                error: "err".to_string(),
            };
            assert_eq!(
                cache.decide(key, start + Duration::from_secs(index as u64)),
                FailureDedupDecision::Emit { suppressed: 0 }
            );
        }

        let extra = FailureDedupKey {
            event: "extra".to_string(),
            operation: "op".to_string(),
            error: "err".to_string(),
        };
        assert_eq!(
            cache.decide(extra, start + Duration::from_secs(999)),
            FailureDedupDecision::Emit { suppressed: 0 }
        );
        assert_eq!(cache.entries.len(), FAILURE_DEDUP_MAX_KEYS);
        let evicted = FailureDedupKey {
            event: "event-0".to_string(),
            operation: "op".to_string(),
            error: "err".to_string(),
        };
        assert!(!cache.entries.contains_key(&evicted));
    }

    #[test]
    fn path_uses_codey_state_directory() {
        assert!(
            error_log_path().ends_with(".codex-session-delete/codey-errors.log"),
            "{}",
            error_log_path().display()
        );
    }

    #[test]
    fn current_versions_include_detected_codex_version() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("Codex.app");
        let contents = app_dir.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<key>CFBundleShortVersionString</key><string>26.826.1724</string>",
        )
        .unwrap();

        assert_eq!(
            refresh_codex_app_version(Some(&app_dir), None).as_deref(),
            Some("26.826.1724")
        );
        assert_eq!(
            ErrorVersions::current().codex.as_deref(),
            Some("26.826.1724")
        );
    }

    #[test]
    fn record_contains_minimal_diagnostic_fields() {
        let now = beijing_now();
        let record = ErrorRecord {
            timestamp: format_beijing_timestamp(now),
            platform: "windows".to_string(),
            versions: ErrorVersions {
                codey: Some("0.7.3".to_string()),
                electron: Some("151.0.0".to_string()),
                ..ErrorVersions::default()
            },
            event: "injection_failed".to_string(),
            operation: "inject_cdp_bridge".to_string(),
            error: "renderer unavailable".to_string(),
            stage: Some("startup.renderer_injection".to_string()),
            recoverable: Some(false),
            context: serde_json::json!({"debugPort": 9229}),
        };

        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value["event"], "injection_failed");
        assert_eq!(value["operation"], "inject_cdp_bridge");
        assert_eq!(value["error"], "renderer unavailable");
        assert_eq!(value["platform"], "windows");
        assert_eq!(value["versions"]["codey"], "0.7.3");
        assert_eq!(value["versions"]["electron"], "151.0.0");
        assert_eq!(value["stage"], "startup.renderer_injection");
        assert_eq!(value["recoverable"], false);
        assert_eq!(value["context"]["debugPort"], 9229);
        assert_eq!(value["timestamp"].as_str().unwrap().len(), 25);
        assert!(value.get("timestampMs").is_none());
        assert!(value.get("pid").is_none());
        assert!(value.get("durationMs").is_none());
        assert!(value.get("attempts").is_none());
        assert!(value.get("timeoutMs").is_none());
    }

    #[test]
    fn helper_accepts_and_normalizes_a_failure_batch() {
        let records = parse_helper_records(
            &serde_json::json!([
                {
                    "timestamp": "2026-08-18T09:36:53Z",
                    "platform": "windows",
                    "event": "patch_failed",
                    "operation": "renderer_patch:model allowlist",
                    "error": "gate matched 0 times"
                },
                {
                    "timestamp": "2026-08-18T09:36:54Z",
                    "platform": "windows",
                    "event": "patch_failed",
                    "operation": "renderer_patch:model visibility",
                    "error": "gate matched 0 times"
                }
            ])
            .to_string(),
        )
        .unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].timestamp, "2026-08-18T17:36:53+08:00");
        assert_eq!(
            records[1].versions.codey.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn helper_rejects_empty_and_oversized_batches() {
        assert!(parse_helper_records("[]").is_err());
        let record = serde_json::json!({
            "timestamp": "2026-08-18T09:36:53Z",
            "platform": "windows",
            "event": "patch_failed",
            "operation": "renderer_patch:test",
            "error": "gate matched 0 times"
        });
        let oversized = Value::Array(vec![record; MAX_HELPER_RECORDS + 1]).to_string();
        assert!(parse_helper_records(&oversized).is_err());
    }

    #[test]
    fn legacy_helper_records_remain_compatible() {
        let mut record = serde_json::from_value::<ErrorRecord>(serde_json::json!({
            "timestamp": "2026-08-02T11:21:24.543+08:00",
            "timestampMs": 1_785_640_884_543_i64,
            "pid": 4255,
            "platform": "macos",
            "event": "patch_failed",
            "operation": "renderer_patch:model visibility",
            "error": "gate matched 0 times",
            "context": {"matchCount": 0}
        }))
        .unwrap();

        normalize_record(&mut record);
        assert_eq!(record.stage, None);
        assert_eq!(record.recoverable, None);
        assert_eq!(record.timestamp, "2026-08-02T11:21:24+08:00");
        assert_eq!(
            record.versions.codey.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(record.context["matchCount"], 0);
    }

    #[test]
    fn legacy_runtime_versions_move_out_of_context() {
        let mut record = serde_json::from_value::<ErrorRecord>(serde_json::json!({
            "timestamp": "2026-08-15T08:28:19.264Z",
            "platform": "windows",
            "event": "patch_failed",
            "operation": "renderer_patch:model visibility",
            "error": "gate matched 0 times",
            "context": {
                "matchCount": 0,
                "electronVersion": "151.0.0",
                "chromeVersion": "151.0.0",
                "nodeVersion": "24.14.0"
            }
        }))
        .unwrap();

        normalize_record(&mut record);

        assert_eq!(record.timestamp, "2026-08-15T16:28:19+08:00");
        assert_eq!(record.versions.electron.as_deref(), Some("151.0.0"));
        assert_eq!(record.versions.chrome.as_deref(), Some("151.0.0"));
        assert_eq!(record.versions.node.as_deref(), Some("24.14.0"));
        assert_eq!(record.context, serde_json::json!({"matchCount": 0}));
    }

    #[test]
    fn same_day_failures_are_appended() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(ERROR_LOG_FILE);
        let today = beijing_now().date_naive();
        let mut writer = ErrorLogWriter;

        writer.append(&path, today, r#"{"error":"first"}"#).unwrap();
        writer
            .append(&path, today, r#"{"error":"second"}"#)
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "{\"error\":\"first\"}\n{\"error\":\"second\"}\n"
        );
    }

    #[test]
    fn crossing_into_a_new_day_clears_old_failures() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(ERROR_LOG_FILE);
        let first_day = beijing_now().date_naive();
        let next_day = first_day.succ_opt().unwrap();
        let mut writer = ErrorLogWriter;

        writer
            .append(&path, first_day, r#"{"error":"old"}"#)
            .unwrap();
        writer
            .append(&path, next_day, r#"{"error":"new"}"#)
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "{\"error\":\"new\"}\n"
        );
    }

    #[test]
    fn same_day_log_restarts_when_the_file_size_cap_is_reached() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(ERROR_LOG_FILE);
        std::fs::write(&path, vec![b'x'; MAX_LOG_FILE_BYTES as usize]).unwrap();
        let today = beijing_now().date_naive();
        let mut writer = ErrorLogWriter;

        writer.append(&path, today, r#"{"error":"new"}"#).unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "{\"error\":\"new\"}\n"
        );
    }

    #[test]
    fn incomplete_tail_is_repaired_before_the_next_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(ERROR_LOG_FILE);
        std::fs::write(&path, b"{\"error\":\"complete\"}\n{\"error\":").unwrap();
        let today = beijing_now().date_naive();
        let mut writer = ErrorLogWriter;

        writer.append(&path, today, r#"{"error":"next"}"#).unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "{\"error\":\"complete\"}\n{\"error\":\"next\"}\n"
        );
    }

    #[test]
    fn concurrent_writers_keep_each_json_line_intact() {
        let temp = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(temp.path().join(ERROR_LOG_FILE));
        let today = beijing_now().date_naive();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|thread_id| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut writer = ErrorLogWriter;
                    barrier.wait();
                    for entry_id in 0..20 {
                        writer
                            .append(
                                &path,
                                today,
                                &serde_json::json!({
                                    "thread": thread_id,
                                    "entry": entry_id,
                                })
                                .to_string(),
                            )
                            .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        let contents = std::fs::read_to_string(path.as_ref()).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 160);
        for line in lines {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }
}
