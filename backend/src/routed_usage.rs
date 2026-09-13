use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use codey_runtime_core::paths::default_app_state_dir;

use crate::codex_config::codex_home;
use crate::commands::AppState;

const SCAN_STATE_FILE: &str = "routed-usage-scan-state-v1.json";
const RECORDS_FILE: &str = "routed-usage-records-v1.jsonl";
const STATE_VERSION: u64 = 1;
/// Upper bound of rollout files scanned per bridge call so the initial
/// catch-up over an existing `sessions/` tree stays resumable instead of
/// blocking one request for seconds.
const MAX_NEW_FILES_PER_TICK: usize = 24;
/// Per-call byte budget across all scanned files; the scan offset only ever
/// advances past fully processed lines, so the rest is picked up next tick.
const MAX_NEW_BYTES_PER_TICK: u64 = 192 * 1024 * 1024;
/// Keep the JSONL append log bounded; oversized logs are compacted to the
/// deduplicated record set.
const COMPACT_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;
/// UUIDv7 turn ids sort chronologically, so pruning the smallest ids from a
/// file's in-memory turn→model map drops the stalest history first.
const TURN_MODEL_MAP_CAP: usize = 64;

static SCAN_LOCK: Mutex<()> = Mutex::new(());
static AGGREGATE_CACHE_RECORDS_LEN: AtomicU64 = AtomicU64::new(0);
static AGGREGATE_CACHE: Mutex<Option<Value>> = Mutex::new(None);
static STATE_DIR_FOR_TESTS: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
pub fn set_state_dir_for_tests(path: Option<PathBuf>) {
    if let Ok(mut guard) = STATE_DIR_FOR_TESTS.lock() {
        *guard = path;
    }
}

fn resolve_state_dir() -> PathBuf {
    if let Ok(guard) = STATE_DIR_FOR_TESTS.lock()
        && let Some(path) = guard.as_ref()
    {
        return path.clone();
    }
    default_app_state_dir()
}

/// Bridge snapshot for the renderer merge script. Token counts consumed on
/// Codey-routed models never reach the official profile stats backend, so the
/// page script polls here and patches them into `GET /wham/profiles/me`.
pub async fn routed_usage_snapshot(state: &Arc<AppState>) -> Value {
    if !state.config.read().await.merge_routed_usage_into_profile {
        return json!({"status": "disabled"});
    }
    let home = codex_home();
    crate::commands::blocking_value("合并路由 token 用量", move || scan_and_snapshot(home)).await}

fn scan_and_snapshot(home: &Path) -> anyhow::Result<Value> {
    let _guard =
        scan_lock_guard().ok_or_else(|| anyhow::anyhow!("路由用量扫描锁不可用（可能已被毒化）"))?;
    let state_dir = resolve_state_dir();
    fs::create_dir_all(&state_dir)?;
    let records_path = state_dir.join(RECORDS_FILE);
    let dirty = scan_once(home, &state_dir, &records_path)?;
    let records_len = fs::metadata(&records_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    if !dirty
        && AGGREGATE_CACHE_RECORDS_LEN.load(Ordering::Acquire) == records_len
        && let Ok(cached) = AGGREGATE_CACHE.lock()
        && let Some(cached) = cached.as_ref()
    {
        return Ok(cached.clone());
    }
    let days = aggregate_records(&records_path)?;
    let snapshot = json!({
        "status": "ok",
        "version": STATE_VERSION,
        "days": days,
    });
    AGGREGATE_CACHE_RECORDS_LEN.store(records_len, Ordering::Release);
    if let Ok(mut cache) = AGGREGATE_CACHE.lock() {
        *cache = Some(snapshot.clone());
    }
    Ok(snapshot)
}

fn scan_lock_guard() -> Option<MutexGuard<'static, ()>> {
    SCAN_LOCK.lock().ok()
}

/// Incremental scan pass. Returns `true` when new routed usage was appended.
fn scan_once(home: &Path, state_dir: &Path, records_path: &Path) -> anyhow::Result<bool> {
    let state_path = state_dir.join(SCAN_STATE_FILE);
    let mut state = load_scan_state(&state_path);
    let sessions_dir = home.join("sessions");
    let mut files = Vec::new();
    collect_rollout_files(&sessions_dir, &mut files);

    let mut dirty = false;
    let mut budget = MAX_NEW_BYTES_PER_TICK;
    let mut processed = 0usize;
    for path in &files {
        let key = path.to_string_lossy().into_owned();
        let file_state = state.files.get(&key);
        let known_offset = file_state.map_or(0, |entry| entry.offset);
        let len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        // offset > len means the rollout was truncated/rewritten; rescan it
        // from the start — record-level dedup keeps the totals stable.
        if known_offset == len || (known_offset > 0 && known_offset < len && budget == 0) {
            continue;
        }
        if processed >= MAX_NEW_FILES_PER_TICK || budget == 0 {
            break;
        }
        let consumed =
            match append_new_records(path, known_offset, budget, &mut state, records_path) {
                Ok(consumed) => consumed,
                Err(error) => {
                    crate::error_log::record_failure(
                        "scan_failed",
                        "routed_usage_scan",
                        format!("{error:#}"),
                        json!({ "path": key }),
                    );
                    continue;
                }
            };
        if consumed > 0 {
            dirty = true;
        }
        budget = budget.saturating_sub(consumed);
        processed += 1;
    }

    state.files.retain(|key, _| {
        files
            .iter()
            .any(|path| path.to_string_lossy() == key.as_str())
    });
    save_scan_state(&state_path, &state)?;
    compact_records_if_needed(records_path)?;
    Ok(dirty)
}

fn collect_rollout_files(sessions_dir: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![(sessions_dir.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 {
                    stack.push((path, depth + 1));
                }
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
    }
    out.sort();
}

/// Reads a rollout file from `offset`, updates the in-file turn→model map and
/// appends routed usage records. Returns the number of bytes consumed.
fn append_new_records(
    path: &Path,
    offset: u64,
    byte_budget: u64,
    state: &mut ScanState,
    records_path: &Path,
) -> anyhow::Result<u64> {
    let key = path.to_string_lossy().into_owned();
    let entry = state.files.entry(key).or_default();
    let len = fs::metadata(path)?.len();
    if offset > len {
        entry.offset = 0;
        entry.models.clear();
    }
    let start = entry.offset;
    let read_len = len.saturating_sub(start).min(byte_budget);
    if read_len == 0 {
        return Ok(0);
    }

    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = vec![0u8; read_len as usize];
    file.read_exact(&mut buffer)?;

    // Only hand complete lines to the parser; the tail after the last newline
    // is still being written by Codex and is re-read next tick.
    let complete_end = match buffer.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => index + 1,
        None => return Ok(0),
    };
    let complete = &buffer[..complete_end];

    let mut records = String::new();
    for line in complete.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if memchr::memmem::find(line, b"\"turn_context\"").is_some() {
            record_turn_model(line, entry);
        } else if memchr::memmem::find(line, b"\"token_usage_record\"").is_some() {
            let record = routed_record_line(line, &entry.models);
            if let Some(record) = record {
                records.push_str(&record);
                records.push('\n');
            }
        }
    }
    if !records.is_empty() {
        let mut target = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(records_path)?;
        target.write_all(records.as_bytes())?;
    }
    entry.offset = start + complete_end as u64;
    Ok(complete_end as u64)
}

/// Records `turn_context` payload models so later usage lines can be matched
/// to the model that served the turn.
fn record_turn_model(line: &[u8], entry: &mut FileScanState) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    if value.get("type").and_then(Value::as_str) != Some("turn_context") {
        return;
    }
    let Some(payload) = value.get("payload") else {
        return;
    };
    let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) else {
        return;
    };
    let Some(model) = payload.get("model").and_then(Value::as_str) else {
        return;
    };
    if model.trim().is_empty() {
        return;
    }
    entry.models.insert(turn_id.to_string(), model.to_string());
    while entry.models.len() > TURN_MODEL_MAP_CAP
        && let Some(stalest) = entry.models.keys().next().cloned()
    {
        entry.models.remove(&stalest);
    }
}

/// Extracts an append-ready JSONL line for a routed usage record, or `None`
/// when the record does not belong to a routed turn.
fn routed_record_line(line: &[u8], models: &BTreeMap<String, String>) -> Option<String> {
    let value = serde_json::from_slice::<Value>(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("token_usage_record") {
        return None;
    }
    let payload = value.get("payload")?;
    let turn_id = payload.get("turn_id").and_then(Value::as_str)?;
    let model = models.get(turn_id)?;
    if !is_routed_model(model) {
        return None;
    }
    let usage = payload.get("usage")?;
    let total = usage.get("total_tokens").and_then(Value::as_u64)?;
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .get("cached_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let date = record_date(&value)?;
    let response_id = payload
        .get("response_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}:{}:{}",
                payload
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                turn_id,
                value.get("timestamp").and_then(Value::as_str).unwrap_or(""),
            )
        });
    Some(
        json!({
            "rid": response_id,
            "d": date,
            "i": input,
            "c": cached,
            "o": output,
            "t": total,
        })
        .to_string(),
    )
}

/// Routed models either carry the CC Switch takeover prefix
/// (`route-<provider>/<model>`) or Codey's legacy trailing bracket suffix.
fn is_routed_model(model: &str) -> bool {
    let model = model.trim();
    if model.starts_with("route-") {
        return true;
    }
    match model.rfind('[') {
        Some(index) => index > 0 && model.ends_with(']'),
        None => false,
    }
}

/// Rollout timestamps are RFC3339 UTC, so the bucket day is the first ten
/// characters (`YYYY-MM-DD`).
fn record_date(value: &Value) -> Option<String> {
    let timestamp = value.get("timestamp").and_then(Value::as_str)?;
    let date = timestamp.get(..10)?;
    let bytes = date.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4] == b'-'
        && bytes[5].is_ascii_digit()
        && bytes[6].is_ascii_digit()
        && bytes[7] == b'-'
        && bytes[8].is_ascii_digit()
        && bytes[9].is_ascii_digit();
    shaped.then(|| date.to_string())
}

fn aggregate_records(records_path: &Path) -> anyhow::Result<BTreeMap<String, u64>> {
    let Ok(contents) = fs::read(records_path) else {
        return Ok(BTreeMap::new());
    };
    let mut days: BTreeMap<String, u64> = BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in contents.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(rid) = value.get("rid").and_then(Value::as_str) else {
            continue;
        };
        if seen.len() < 1_000_000 && !seen.insert(rid.to_string()) {
            continue;
        }
        let Some(date) = value.get("d").and_then(Value::as_str) else {
            continue;
        };
        let total = value.get("t").and_then(Value::as_u64).unwrap_or(0);
        *days.entry(date.to_string()).or_insert(0) += total;
    }
    Ok(days)
}

fn compact_records_if_needed(records_path: &Path) -> anyhow::Result<()> {
    let len = fs::metadata(records_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    if len < COMPACT_THRESHOLD_BYTES {
        return Ok(());
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut compacted = String::new();
    if let Ok(contents) = fs::read(records_path) {
        for line in contents.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let Some(rid) = value.get("rid").and_then(Value::as_str) else {
                continue;
            };
            if !seen.insert(rid.to_string()) {
                continue;
            }
            compacted.push_str(&value.to_string());
            compacted.push('\n');
        }
    }
    let tmp_path = records_path.with_extension("jsonl.tmp");
    fs::write(&tmp_path, compacted)?;
    fs::rename(&tmp_path, records_path)?;
    Ok(())
}

#[derive(Default)]
struct ScanState {
    files: BTreeMap<String, FileScanState>,
}

#[derive(Default)]
struct FileScanState {
    offset: u64,
    models: BTreeMap<String, String>,
}

fn load_scan_state(path: &Path) -> ScanState {
    let mut state = ScanState::default();
    let Ok(contents) = fs::read(path) else {
        return state;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&contents) else {
        return state;
    };
    if value.get("version").and_then(Value::as_u64) != Some(STATE_VERSION) {
        return state;
    }
    let Some(files) = value.get("files").and_then(Value::as_object) else {
        return state;
    };
    for (key, entry) in files {
        let offset = entry.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let mut models = BTreeMap::new();
        if let Some(map) = entry.get("models").and_then(Value::as_object) {
            for (turn_id, model) in map {
                if let Some(model) = model.as_str() {
                    models.insert(turn_id.clone(), model.to_string());
                }
            }
        }
        state
            .files
            .insert(key.clone(), FileScanState { offset, models });
    }
    state
}

fn save_scan_state(path: &Path, state: &ScanState) -> anyhow::Result<()> {
    let mut files = serde_json::Map::new();
    for (key, entry) in &state.files {
        files.insert(
            key.clone(),
            json!({
                "offset": entry.offset,
                "models": entry.models,
            }),
        );
    }
    let payload = json!({
        "version": STATE_VERSION,
        "files": files,
    });
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, payload.to_string())?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollout(dir: &Path, relative: &str, lines: &[Value]) -> PathBuf {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut contents = String::new();
        for line in lines {
            contents.push_str(&line.to_string());
            contents.push('\n');
        }
        fs::write(&path, contents).unwrap();
        path
    }

    fn turn_context(turn_id: &str, model: &str) -> Value {
        json!({
            "timestamp": "2026-09-12T16:00:00.000Z",
            "type": "turn_context",
            "payload": {"turn_id": turn_id, "model": model}
        })
    }

    fn usage_record(timestamp: &str, response_id: &str, turn_id: &str, total: u64) -> Value {
        json!({
            "timestamp": timestamp,
            "type": "token_usage_record",
            "payload": {
                "turn_id": turn_id,
                "response_id": response_id,
                "usage": {"input_tokens": total - 10, "cached_input_tokens": 0, "output_tokens": 10, "total_tokens": total}
            }
        })
    }

    #[test]
    fn routed_model_classification_matches_prefix_and_suffix() {
        assert!(is_routed_model("route-mtwrmp6l-exkax4/gemini-3.8-flash"));
        assert!(is_routed_model("deepseek-v3[codey]"));
        assert!(!is_routed_model("gpt-5.6-luna"));
        assert!(!is_routed_model("[codey]"));
        assert!(!is_routed_model("gpt-5.6["))
    }

    #[test]
    fn scan_and_aggregate_round_trips_routed_usage_only() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        write_rollout(
            &home,
            "sessions/2026/09/12/rollout-a.jsonl",
            &[
                json!({"type": "session_meta", "payload": {}}),
                turn_context("turn-official", "gpt-5.6-luna"),
                usage_record(
                    "2026-09-12T10:00:00.000Z",
                    "resp-official",
                    "turn-official",
                    1000,
                ),
                turn_context("turn-routed", "route-abc/deepseek-v3"),
                usage_record("2026-09-12T11:00:00.000Z", "resp-1", "turn-routed", 500),
                usage_record("2026-09-12T11:01:00.000Z", "resp-2", "turn-routed", 250),
            ],
        );
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let records_path = state_dir.join(RECORDS_FILE);

        scan_once(&home, &state_dir, &records_path).unwrap();
        let days = aggregate_records(&records_path).unwrap();
        assert_eq!(days.get("2026-09-12"), Some(&750));

        // A second pass must not double count.
        scan_once(&home, &state_dir, &records_path).unwrap();
        let days = aggregate_records(&records_path).unwrap();
        assert_eq!(days.get("2026-09-12"), Some(&750));
    }

    #[test]
    fn scan_resumes_after_partial_line_and_survives_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let path = home.join("sessions/2026/09/12/rollout-b.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut contents = String::new();
        contents.push_str(&turn_context("turn-routed", "route-abc/model").to_string());
        contents.push('\n');
        contents.push_str(
            &usage_record("2026-09-12T09:00:00.000Z", "resp-1", "turn-routed", 300).to_string(),
        );
        contents.push('\n');
        fs::write(&path, &contents).unwrap();

        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let records_path = state_dir.join(RECORDS_FILE);
        scan_once(&home, &state_dir, &records_path).unwrap();
        assert_eq!(
            aggregate_records(&records_path).unwrap().get("2026-09-12"),
            Some(&300)
        );

        // Append a partial line: it must stay unread until completed.
        let full_line =
            usage_record("2026-09-12T09:05:00.000Z", "resp-2", "turn-routed", 700).to_string();
        let partial_line: String = full_line.chars().take(40).collect();
        let mut partial = contents.clone();
        partial.push_str(&partial_line);
        fs::write(&path, &partial).unwrap();
        scan_once(&home, &state_dir, &records_path).unwrap();
        assert_eq!(
            aggregate_records(&records_path).unwrap().get("2026-09-12"),
            Some(&300)
        );

        // Completing the line makes the record visible exactly once.
        let mut complete = contents.clone();
        complete.push_str(
            &usage_record("2026-09-12T09:05:00.000Z", "resp-2", "turn-routed", 700).to_string(),
        );
        complete.push('\n');
        fs::write(&path, &complete).unwrap();
        scan_once(&home, &state_dir, &records_path).unwrap();
        assert_eq!(
            aggregate_records(&records_path).unwrap().get("2026-09-12"),
            Some(&1000)
        );

        // Truncating below the stored offset re-reads the file, and record
        // dedup keeps the aggregate stable.
        fs::write(&path, &contents).unwrap();
        scan_once(&home, &state_dir, &records_path).unwrap();
        assert_eq!(
            aggregate_records(&records_path).unwrap().get("2026-09-12"),
            Some(&1000)
        );
    }

    #[test]
    fn snapshot_reports_days_and_skips_unknown_turn_models() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        set_state_dir_for_tests(Some(state_dir.clone()));
        AGGREGATE_CACHE_RECORDS_LEN.store(0, Ordering::Release);
        if let Ok(mut cache) = AGGREGATE_CACHE.lock() {
            *cache = None;
        }
        write_rollout(
            &home,
            "sessions/2026/09/13/rollout-c.jsonl",
            &[
                // Usage for a turn whose context line lives in an earlier
                // file must be skipped, never guessed.
                usage_record(
                    "2026-09-13T08:00:00.000Z",
                    "resp-orphan",
                    "turn-unknown",
                    999,
                ),
                turn_context("turn-known", "route-abc/gemini"),
                usage_record("2026-09-13T08:05:00.000Z", "resp-known", "turn-known", 4200),
            ],
        );
        let snapshot = scan_and_snapshot(&home).unwrap();
        assert_eq!(snapshot["status"], "ok");
        assert_eq!(snapshot["days"]["2026-09-13"], 4200);
        set_state_dir_for_tests(None);
    }
}
