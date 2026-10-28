//! 原生事件中的轮次、生成步骤和工具计时。按追加位置读取，避免轮询重复扫描历史。
use super::{ConversationStatsSnapshot, LastTurnStatsSnapshot, normalize_session_id};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

const MAX_TURN_ITEMS: usize = 2048;
const MAX_RECENT_TURNS: usize = 64;
const MAX_ID_BYTES: usize = 512;
type IdDigest = [u8; 32];
type TurnStartTime = (u64, u32);

#[derive(Default)]
struct SessionTurnCount {
    count: u64,
    uncertain: bool,
    recent: VecDeque<(IdDigest, Option<TurnStartTime>)>,
    retired_through: Option<TurnStartTime>,
    retired_without_time: bool,
}

impl SessionTurnCount {
    fn observe(&mut self, id: IdDigest, started: Option<TurnStartTime>) {
        if self.recent.iter().any(|(known, _)| *known == id) {
            return;
        }
        if let (Some(started), Some(retired)) = (started, self.retired_through)
            && started <= retired
        {
            // 已淘汰的 ID 无法区分旧事件重放、相同时间的新轮次或时钟
            // 回退后的新事件，不重复增加，也不将推断当作精确总数。
            self.uncertain = true;
            return;
        }
        if started.is_none() && (self.retired_through.is_some() || self.retired_without_time) {
            self.uncertain = true;
        }
        if let Some(count) = self.count.checked_add(1) {
            self.count = count;
        } else {
            self.uncertain = true;
        }
        self.recent.push_back((id, started));
        if self.recent.len() > MAX_RECENT_TURNS {
            let (_, started) = self.recent.pop_front().expect("recent turn exists");
            if let Some(started) = started {
                self.retired_through =
                    Some(self.retired_through.map_or(started, |old| old.max(started)));
            } else {
                self.retired_without_time = true;
            }
        }
    }

    fn value(&self) -> Option<u64> {
        (!self.uncertain && self.count > 0).then_some(self.count)
    }
}

#[derive(Default)]
struct TurnActivity {
    snapshot: LastTurnStatsSnapshot,
    responses: HashSet<IdDigest>,
    tools: HashSet<IdDigest>,
}

fn record_timestamp(record: &Value) -> Option<u64> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(record["timestamp"].as_str()?).ok()?;
    u64::try_from(timestamp.timestamp_millis()).ok()
}

fn record_turn_time(record: &Value) -> Option<TurnStartTime> {
    let timestamp = chrono::DateTime::parse_from_rfc3339(record["timestamp"].as_str()?).ok()?;
    Some((
        u64::try_from(timestamp.timestamp()).ok()?,
        timestamp.timestamp_subsec_nanos(),
    ))
}

fn bounded_id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)
}

fn id_digest(id: &str) -> IdDigest {
    Sha256::digest(id.as_bytes()).into()
}

fn remember_id(ids: &mut HashSet<IdDigest>, id: &str, partial: &mut bool) -> bool {
    let digest = id_digest(id);
    if ids.contains(&digest) {
        return false;
    }
    if ids.len() >= MAX_TURN_ITEMS {
        *partial = true;
        return false;
    }
    ids.insert(digest)
}

#[derive(Default)]
pub(super) struct Activity {
    pub offset: u64,
    #[cfg(test)]
    pub(super) last_read_start: u64,
    #[cfg(test)]
    pub(super) last_read_bytes: u64,
    last_turn: Option<TurnActivity>,
    recent_turn_ids: VecDeque<IdDigest>,
    turn_count: SessionTurnCount,
    own_segment: Option<bool>,
    pub(super) usage_snapshot: Option<ConversationStatsSnapshot>,
}

impl Activity {
    pub fn read_append(&mut self, path: &Path, session_id: &str) -> std::io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        #[cfg(test)]
        {
            self.last_read_start = self.offset;
            self.last_read_bytes = 0;
        }
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            // 单行保留上限 8 MiB，超大工具输出按块跳过。未写完的末行下次再读。
            let start = self.offset;
            line.clear();
            let mut oversized = false;
            let mut consumed = 0u64;
            loop {
                let buf = reader.fill_buf()?;
                if buf.is_empty() {
                    // 完整 JSON 可以暂时没有换行；不推进偏移，追加后仍重新校验。
                    if !oversized && let Ok(record) = serde_json::from_slice::<Value>(&line) {
                        self.record(&record, session_id);
                    }
                    return Ok(());
                }
                let end = memchr::memchr(b'\n', buf);
                let size = end.map_or(buf.len(), |v| v + 1);
                consumed += size as u64;
                #[cfg(test)]
                {
                    self.last_read_bytes += size as u64;
                }
                if !oversized && line.len() + size <= 8 * 1024 * 1024 {
                    line.extend_from_slice(&buf[..size]);
                } else {
                    oversized = true;
                    line.clear();
                }
                reader.consume(size);
                if end.is_some() {
                    break;
                }
            }
            self.offset = start + consumed;
            if !oversized && let Ok(record) = serde_json::from_slice::<Value>(&line) {
                self.record(&record, session_id);
            }
        }
    }

    pub(super) fn record(&mut self, record: &Value, session_id: &str) {
        let payload = &record["payload"];
        if record["type"] == "session_meta" {
            if let Some(id) = payload["session_id"]
                .as_str()
                .or_else(|| payload["id"].as_str())
            {
                self.own_segment = Some(normalize_session_id(id).eq_ignore_ascii_case(session_id));
            }
            return;
        }
        if self.own_segment == Some(false) {
            return;
        }
        if ["thread_id", "session_id"].iter().any(|key| {
            payload[*key]
                .as_str()
                .is_some_and(|id| !normalize_session_id(id).eq_ignore_ascii_case(session_id))
        }) {
            return;
        }
        if let Some(usage) = super::history::usage_from_record(record, session_id) {
            self.usage_snapshot = Some(usage);
        }
        match (record["type"].as_str(), payload["type"].as_str()) {
            (Some("event_msg"), Some("task_started")) => {
                if let Some(id) = bounded_id(&payload["turn_id"]) {
                    let digest = id_digest(id);
                    self.turn_count.observe(digest, record_turn_time(record));
                    let started = record_timestamp(record);
                    if let Some(turn) = &mut self.last_turn
                        && turn.snapshot.id == id
                    {
                        if turn.snapshot.started_at_unix_ms.is_none() {
                            turn.snapshot.started_at_unix_ms = started;
                        }
                        return;
                    }
                    if self.last_turn.as_ref().is_some_and(|turn| {
                        started
                            .zip(turn.snapshot.started_at_unix_ms)
                            .is_some_and(|(new, old)| new <= old)
                    }) || self.recent_turn_ids.contains(&digest)
                    {
                        return;
                    }
                    self.recent_turn_ids.push_back(digest);
                    if self.recent_turn_ids.len() > MAX_RECENT_TURNS {
                        self.recent_turn_ids.pop_front();
                    }
                    self.last_turn = Some(TurnActivity {
                        snapshot: LastTurnStatsSnapshot {
                            id: id.to_owned(),
                            started_at_unix_ms: started,
                            status: "running",
                            coverage: "unknown",
                            step_count: Some(0),
                            in_flight: Some(0),
                            speed_state: Some("waiting"),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                } else {
                    self.turn_count.uncertain = true;
                }
            }
            (Some("event_msg"), Some("task_complete" | "turn_aborted")) => {
                if let Some(turn) = &mut self.last_turn
                    && payload["turn_id"].as_str() == Some(turn.snapshot.id.as_str())
                {
                    let completed = record_timestamp(record).filter(|end| {
                        turn.snapshot
                            .started_at_unix_ms
                            .is_none_or(|start| *end >= start)
                    });
                    turn.snapshot.completed_at_unix_ms =
                        turn.snapshot.completed_at_unix_ms.or(completed);
                    if turn.snapshot.status == "running" {
                        turn.snapshot.status = if payload["type"] == "turn_aborted" {
                            "aborted"
                        } else {
                            "completed"
                        };
                    }
                    turn.snapshot.speed_state = None;
                }
            }
            (Some("token_usage_record"), _) => {
                if payload["turn_id"].as_str().is_none() {
                    if let Some(turn) = &mut self.last_turn
                        && event_belongs_to_turn(record, turn)
                    {
                        // 事件时间只是写入时刻，不能拿它给迟到 response 推断轮次。
                        if turn.responses.is_empty() {
                            turn.snapshot.step_count = None;
                        }
                    }
                    return;
                }
                if payload["session_id"]
                    .as_str()
                    .is_some_and(|id| normalize_session_id(id).eq_ignore_ascii_case(session_id))
                    && let Some(id) = bounded_id(&payload["response_id"])
                    && let Some(turn) = &mut self.last_turn
                    && event_belongs_to_turn(record, turn)
                    && remember_id(&mut turn.responses, id, &mut turn.snapshot.activity_partial)
                {
                    turn.snapshot.step_count = Some(turn.responses.len() as u64);
                }
            }
            (Some("event_msg"), Some("item_completed")) => {
                let item = &payload["item"];
                if !matches!(
                    item["type"].as_str(),
                    Some(
                        "McpToolCall"
                            | "CommandExecution"
                            | "CollabAgentToolCall"
                            | "FileChange"
                            | "WebSearch"
                            | "ImageGeneration"
                            | "DynamicToolCall"
                    )
                ) {
                    return;
                }
                let duration = item["duration"]["secs"]
                    .as_u64()
                    .and_then(|secs| {
                        let nanos = item["duration"]["nanos"].as_u64()?;
                        (nanos < 1_000_000_000)
                            .then(|| secs.saturating_mul(1000).saturating_add(nanos / 1_000_000))
                    })
                    .or_else(|| {
                        payload["completed_at_ms"]
                            .as_u64()?
                            .checked_sub(payload["started_at_ms"].as_u64()?)
                    });
                if let (Some(id), Some(duration)) = (bounded_id(&item["id"]), duration)
                    && let Some(turn) = &mut self.last_turn
                    && event_belongs_to_turn(record, turn)
                    // 无显式 turn ID 时也排除新轮开始前已运行的迟到工具。
                    && (payload["turn_id"].as_str().is_some()
                        || record_timestamp(record).zip(turn.snapshot.started_at_unix_ms)
                            .is_some_and(|(end, start)| end.saturating_sub(duration) >= start))
                    && remember_id(&mut turn.tools, id, &mut turn.snapshot.activity_partial)
                {
                    turn.snapshot.tool_duration_ms = Some(
                        turn.snapshot
                            .tool_duration_ms
                            .unwrap_or(0)
                            .saturating_add(duration),
                    );
                }
            }
            _ => {}
        }
    }

    pub fn apply(&self, snapshot: &mut ConversationStatsSnapshot) {
        snapshot.turn_count = self.turn_count.value();
        snapshot.activity_source = snapshot.turn_count.map(|_| "rollout");
        snapshot.activity_scope = snapshot.turn_count.map(|_| "session");
        snapshot.last_turn = self.last_turn.as_ref().map(|turn| turn.snapshot.clone());
    }
}

fn event_belongs_to_turn(record: &Value, turn: &TurnActivity) -> bool {
    let snapshot = &turn.snapshot;
    if let Some(id) = record["payload"]["turn_id"].as_str() {
        return id == snapshot.id;
    }
    record_timestamp(record)
        .zip(snapshot.started_at_unix_ms)
        .is_some_and(|(timestamp, start)| {
            timestamp >= start
                && snapshot
                    .completed_at_unix_ms
                    .is_none_or(|end| timestamp <= end)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn start(id: &str, second: u32) -> Value {
        json!({"type":"event_msg","timestamp":format!("2026-10-18T00:00:{second:02}Z"),"payload":{"type":"task_started","turn_id":id}})
    }

    #[test]
    fn append_reader_preserves_stats_across_large_buffered_content() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", start("large", 0)).unwrap();
        let content = json!({"type":"response_item","payload":{"type":"function_call_output","output":"tool output 中文 ".repeat(8192)}});
        for _ in 0..100 {
            writeln!(file, "{content}").unwrap();
        }
        writeln!(file, "{}", json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":123}}}})).unwrap();

        let mut activity = Activity::default();
        activity.read_append(file.path(), "own").unwrap();
        let mut snapshot = activity.usage_snapshot.clone().unwrap();
        activity.apply(&mut snapshot);
        assert_eq!(snapshot.total_tokens, Some(123));
        assert_eq!(snapshot.turn_count, Some(1));
        assert_eq!(snapshot.last_turn.unwrap().id, "large");
        assert_eq!(activity.offset, file.as_file().metadata().unwrap().len());

        // 大行后的短记录和后续追加都必须独立解析，重复读取不能增加轮数。
        writeln!(file, "{}", start("next", 1)).unwrap();
        activity.read_append(file.path(), "own").unwrap();
        activity.read_append(file.path(), "own").unwrap();
        let mut snapshot = activity.usage_snapshot.clone().unwrap();
        activity.apply(&mut snapshot);
        assert_eq!(snapshot.total_tokens, Some(123));
        assert_eq!(snapshot.turn_count, Some(2));
        assert_eq!(snapshot.last_turn.unwrap().id, "next");
        assert_eq!(activity.offset, file.as_file().metadata().unwrap().len());
    }

    #[test]
    fn total_turn_count_is_independent_of_last_turn_and_excludes_foreign_events() {
        let mut activity = Activity::default();
        let mut snapshot = ConversationStatsSnapshot::default();
        activity.apply(&mut snapshot);
        assert_eq!(snapshot.turn_count, None);
        activity.record(
            &json!({"type":"session_meta","payload":{"id":"parent"}}),
            "own",
        );
        activity.record(&start("inherited", 0), "own");
        activity.record(
            &json!({"type":"session_meta","payload":{"id":"local:own"}}),
            "own",
        );
        activity.record(&start("first", 1), "own");
        let second = json!({"type":"event_msg","timestamp":"2026-10-18T00:00:01.000001Z","payload":{"type":"task_started","turn_id":"second"}});
        let third = json!({"type":"event_msg","timestamp":"2026-10-18T00:00:01.000001Z","payload":{"type":"task_started","turn_id":"third"}});
        activity.record(&second, "own");
        activity.record(&third, "own");
        activity.record(&second, "own");
        let mut foreign = start("foreign", 5);
        foreign["payload"]["thread_id"] = json!("foreign");
        activity.record(&foreign, "own");
        activity.record(&start("last", 10), "own");
        activity.record(&start("first", 1), "own");
        activity.apply(&mut snapshot);
        assert_eq!(snapshot.turn_count, Some(4));
        assert_eq!(snapshot.activity_source, Some("rollout"));
        assert_eq!(snapshot.activity_scope, Some("session"));
        assert_eq!(snapshot.last_turn.unwrap().id, "last");
    }

    #[test]
    fn total_turn_count_stays_bounded_and_rejects_old_replayed_events() {
        let mut activity = Activity::default();
        for index in 0..10_000 {
            let timestamp = chrono::DateTime::from_timestamp(1_792_281_600 + index, 0)
                .unwrap()
                .to_rfc3339();
            activity.record(&json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"task_started","turn_id":format!("turn-{index}")}}), "own");
        }
        assert_eq!(activity.turn_count.value(), Some(10_000));
        assert_eq!(activity.turn_count.recent.len(), MAX_RECENT_TURNS);
        assert_eq!(activity.recent_turn_ids.len(), MAX_RECENT_TURNS);
        let timestamp = chrono::DateTime::from_timestamp(1_792_281_600, 0)
            .unwrap()
            .to_rfc3339();
        activity.record(&json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"task_started","turn_id":"turn-0"}}), "own");
        assert_eq!(activity.turn_count.value(), None);
        assert_eq!(activity.turn_count.count, 10_000);
        assert_eq!(
            activity.last_turn.as_ref().unwrap().snapshot.id,
            "turn-9999"
        );
    }

    #[test]
    fn total_turn_count_keeps_ambiguous_or_missing_records_unknown() {
        let mut missing = Activity::default();
        let mut tied = Activity::default();
        for index in 0..MAX_RECENT_TURNS + 2 {
            missing.record(&json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("turn-{index}")}}), "own");
            tied.record(&start(&format!("turn-{index}"), 0), "own");
        }
        assert_eq!(missing.turn_count.value(), None);
        assert_eq!(tied.turn_count.value(), None);
        assert_eq!(missing.turn_count.recent.len(), MAX_RECENT_TURNS);
        let mut invalid = Activity::default();
        invalid.record(&start("valid", 0), "own");
        invalid.record(
            &json!({"type":"event_msg","payload":{"type":"task_started"}}),
            "own",
        );
        assert_eq!(invalid.turn_count.value(), None);
        invalid.record(&start("next", 1), "own");
        assert_eq!(invalid.turn_count.value(), None);
    }

    #[test]
    fn last_turn_events_reset_only_at_new_turn_and_reject_late_old_items() {
        let mut activity = Activity::default();
        activity.record(&start("old", 0), "own");
        activity.record(&json!({"type":"token_usage_record","timestamp":"2026-10-18T00:00:01Z","payload":{"session_id":"own","turn_id":"old","response_id":"old-response"}}), "own");
        activity.record(&start("new", 10), "own");
        let tool = json!({"type":"event_msg","timestamp":"2026-10-18T00:00:12Z","payload":{"type":"item_completed","turn_id":"new","item":{"type":"McpToolCall","id":"new-tool","duration":{"secs":1,"nanos":0}}}});
        activity.record(&tool, "own");
        activity.record(&start("new", 10), "own");
        activity.record(&tool, "own");
        activity.record(&start("old", 0), "own");
        activity.record(&json!({"type":"event_msg","timestamp":"2026-10-18T00:00:13Z","payload":{"type":"item_completed","turn_id":"old","item":{"type":"McpToolCall","id":"old-tool","duration":{"secs":1,"nanos":0}}}}), "own");
        // 没有 turn ID 的迟到工具按真正开始时间排除。
        activity.record(&json!({"type":"event_msg","timestamp":"2026-10-18T00:00:13Z","payload":{"type":"item_completed","item":{"type":"McpToolCall","id":"late-tool","duration":{"secs":5,"nanos":0}}}}), "own");
        activity.record(&json!({"type":"event_msg","timestamp":"2026-10-18T00:00:14Z","payload":{"type":"task_complete","turn_id":"old"}}), "own");
        let current = &activity.last_turn.as_ref().unwrap().snapshot;
        assert_eq!(current.id, "new");
        assert_eq!(current.step_count, Some(0));
        assert_eq!(current.tool_duration_ms, Some(1000));
        assert_eq!(current.status, "running");
        activity.record(&json!({"type":"event_msg","timestamp":"2026-10-18T00:00:15Z","payload":{"type":"turn_aborted","turn_id":"new","completed_at_ms":300}}), "own");
        let current = &activity.last_turn.as_ref().unwrap().snapshot;
        assert_eq!(current.status, "aborted");
        assert_eq!(
            current.completed_at_unix_ms.unwrap() - current.started_at_unix_ms.unwrap(),
            5000
        );
        assert_eq!(current.model_duration_ms, None);
    }

    #[test]
    fn last_turn_never_treats_relative_timestamps_or_total_duration_as_model_timing() {
        let mut activity = Activity::default();
        activity.record(&json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"missing","started_at_ms":100}}), "own");
        activity.record(&json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"missing","completed_at_ms":300,"duration_ms":200}}), "own");
        let turn = &activity.last_turn.as_ref().unwrap().snapshot;
        assert_eq!(turn.started_at_unix_ms, None);
        assert_eq!(turn.completed_at_unix_ms, None);
        assert_eq!(turn.model_duration_ms, None);
        assert!(super::super::TurnWindow::from_snapshot(turn).is_none());
    }

    #[test]
    fn last_turn_activity_memory_is_bounded_and_overflow_is_explicit() {
        let mut activity = Activity::default();
        activity.record(&start("long", 0), "own");
        for index in 0..MAX_TURN_ITEMS + 100 {
            let id = format!("{index:0>512}");
            activity.record(&json!({"type":"token_usage_record","payload":{"session_id":"own","turn_id":"long","response_id":id}}), "own");
            activity.record(&json!({"type":"event_msg","payload":{"type":"item_completed","turn_id":"long","item":{"type":"McpToolCall","id":id,"duration":{"secs":1,"nanos":0}}}}), "own");
        }
        let turn = activity.last_turn.as_ref().unwrap();
        assert_eq!(turn.responses.len(), MAX_TURN_ITEMS);
        assert_eq!(turn.tools.len(), MAX_TURN_ITEMS);
        assert_eq!(
            turn.snapshot.tool_duration_ms,
            Some(MAX_TURN_ITEMS as u64 * 1000)
        );
        assert_eq!(
            std::mem::size_of_val(turn.responses.iter().next().unwrap()),
            32
        );
        assert_eq!(std::mem::size_of_val(turn.tools.iter().next().unwrap()), 32);
        assert!(turn.snapshot.activity_partial);
        activity.record(&start("next", 10), "own");
        assert_eq!(activity.last_turn.as_ref().unwrap().responses.len(), 0);
        assert!(
            !activity
                .last_turn
                .as_ref()
                .unwrap()
                .snapshot
                .activity_partial
        );
    }

    #[test]
    fn digest_deduplication_preserves_duplicates_at_capacity_and_bounds_recent_turns() {
        let mut ids = HashSet::new();
        let mut partial = false;
        for index in 0..MAX_TURN_ITEMS {
            assert!(remember_id(
                &mut ids,
                &format!("{index:0>512}"),
                &mut partial
            ));
        }
        assert!(!remember_id(
            &mut ids,
            &format!("{:0>512}", 0),
            &mut partial
        ));
        assert!(!partial, "重复记录不应被视为超过容量");
        assert!(!remember_id(&mut ids, "new", &mut partial));
        assert!(partial);
        assert_eq!(ids.len(), MAX_TURN_ITEMS);

        let mut activity = Activity::default();
        for index in 0..100 {
            activity.record(&json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("{index:0>512}")}}), "own");
        }
        assert_eq!(activity.turn_count.recent.len(), MAX_RECENT_TURNS);
        assert_eq!(
            std::mem::size_of_val(activity.recent_turn_ids.front().unwrap()),
            32
        );
        activity.record(&json!({"type":"event_msg","payload":{"type":"task_started","turn_id":format!("{:0>512}", 98)}}), "own");
        assert_eq!(
            activity.last_turn.as_ref().unwrap().snapshot.id,
            format!("{:0>512}", 99)
        );
    }
    #[test]
    fn activity_deduplicates_and_requires_own_session_and_actual_tool_timing() {
        let mut stats = Activity::default();
        let turn = json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn"}});
        let response = json!({"type":"token_usage_record","payload":{"session_id":"own","turn_id":"turn","response_id":"response"}});
        let tool = json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"own","turn_id":"turn","item":{"type":"McpToolCall","id":"tool","duration":{"secs":2,"nanos":500000000}},"started_at_ms":1,"completed_at_ms":9}});
        for item in [&turn, &turn, &response, &response, &tool, &tool] {
            stats.record(item, "own");
        }
        stats.record(&json!({"type":"token_usage_record","payload":{"session_id":"foreign","response_id":"foreign"}}), "own");
        stats.record(&json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":10}}}}), "own");
        stats.record(&json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","id":"reason"},"started_at_ms":1,"completed_at_ms":10000}}), "own");
        let mut snapshot = ConversationStatsSnapshot::default();
        stats.apply(&mut snapshot);
        let turn = snapshot.last_turn.unwrap();
        assert_eq!(turn.id, "turn");
        assert_eq!(turn.step_count, Some(1));
        assert_eq!(turn.tool_duration_ms, Some(2500));
        assert_eq!(turn.model_duration_ms, None);
    }

    #[test]
    fn append_reader_retries_incomplete_line_and_ignores_inherited_segment() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"session_meta","payload":{"id":"parent"}})
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"foreign"}})
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            json!({"type":"session_meta","payload":{"id":"own"}})
        )
        .unwrap();
        write!(file, "{{\"type\":\"event_msg\",\"payload\":").unwrap();
        let mut activity = Activity::default();
        activity.read_append(file.path(), "own").unwrap();
        assert!(activity.last_turn.is_none());
        write!(
            file,
            "{{\"type\":\"task_started\",\"turn_id\":\"own-turn\"}}}}"
        )
        .unwrap();
        activity.read_append(file.path(), "own").unwrap();
        assert_eq!(activity.last_turn.as_ref().unwrap().snapshot.id, "own-turn");
        assert_eq!(activity.turn_count.value(), Some(1));
        writeln!(file).unwrap();
        activity.read_append(file.path(), "own").unwrap();
        activity.read_append(file.path(), "own").unwrap();
        assert_eq!(activity.last_turn.as_ref().unwrap().snapshot.id, "own-turn");
    }
}
