use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use codey_runtime_core::codex_sqlite::CodexSessionDbDiscoveryCache;
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use serde_json::Value;

const RECENT_SESSION_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const MAX_RECENT_SESSIONS: usize = 64;
pub(crate) const MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT: usize = 256;
const MAX_CACHED_TURN_CONFIGURATIONS_PER_ROLLOUT: usize = MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT * 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub session_id: String,
    pub turn_id: String,
    pub waiting_id: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedTurn {
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortedTurn {
    pub session_id: String,
    pub turn_id: String,
    pub is_snapshot_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnConfiguration {
    pub model: String,
    pub reasoning_effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedTurn {
    pub session_id: String,
    pub turn_id: String,
    pub duration_ms: u128,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
    pub is_snapshot_replay: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLifecycleStatus {
    #[default]
    Idle,
    Running,
    Error,
    Waiting,
}

#[derive(Debug, Clone, Default)]
pub struct RecentSessionEvents {
    pub pending_approvals: Vec<PendingApproval>,
    pub started_turns: Arc<Vec<StartedTurn>>,
    pub aborted_turns: Arc<Vec<AbortedTurn>>,
    pub completed_turns: Arc<Vec<CompletedTurn>>,
    pub session_statuses: Arc<HashMap<String, SessionLifecycleStatus>>,
    pub turn_configurations: Arc<HashMap<String, HashMap<String, TurnConfiguration>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RolloutSignature {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl RolloutSignature {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ParsedRolloutEvents {
    #[cfg(test)]
    pending_approvals: Vec<(String, String)>,
    started_turns: Vec<String>,
    aborted_turns: Vec<String>,
    completed_turns: Vec<(String, u128, Option<i64>, Option<String>)>,
    snapshot_replay_turns: HashSet<String>,
    turn_configurations: HashMap<String, TurnConfiguration>,
}

/// Resumable rollout parser. Rollout JSONL is append-only in normal operation,
/// so a poll that sees a grown file only has to parse the appended bytes
/// instead of the whole history.
const ROLLOUT_FINGERPRINT_LEN: usize = 64;

#[derive(Debug, Clone)]
struct RolloutFingerprint {
    bytes: [u8; ROLLOUT_FINGERPRINT_LEN],
    len: usize,
}

impl Default for RolloutFingerprint {
    fn default() -> Self {
        Self {
            bytes: [0; ROLLOUT_FINGERPRINT_LEN],
            len: 0,
        }
    }
}

impl RolloutFingerprint {
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }

    fn set_prefix(&mut self, source: &[u8]) {
        self.len = source.len().min(ROLLOUT_FINGERPRINT_LEN);
        self.bytes[..self.len].copy_from_slice(&source[..self.len]);
    }

    fn push_tail(&mut self, source: &[u8]) {
        if source.len() >= ROLLOUT_FINGERPRINT_LEN {
            self.bytes
                .copy_from_slice(&source[source.len() - ROLLOUT_FINGERPRINT_LEN..]);
            self.len = ROLLOUT_FINGERPRINT_LEN;
            return;
        }
        if self.len + source.len() <= ROLLOUT_FINGERPRINT_LEN {
            let next_len = self.len + source.len();
            self.bytes[self.len..next_len].copy_from_slice(source);
            self.len = next_len;
            return;
        }

        let retained = ROLLOUT_FINGERPRINT_LEN - source.len();
        self.bytes.copy_within(self.len - retained..self.len, 0);
        self.bytes[retained..].copy_from_slice(source);
        self.len = ROLLOUT_FINGERPRINT_LEN;
    }
}

#[derive(Debug, Clone, Default)]
struct RolloutParseState {
    consumed_bytes: u64,
    /// Leading bytes of the file. Provider normalisation rewrites the
    /// `session_meta` header in place, which leaves the tail untouched.
    head_fingerprint: RolloutFingerprint,
    /// Trailing bytes of the region already consumed. Re-reading just these
    /// detects an in-place rewrite (Codey itself rewrites rollouts when
    /// deleting turns or normalising providers), which must force a full
    /// re-parse instead of appending to stale state.
    tail_fingerprint: RolloutFingerprint,
    is_subagent: bool,
    forked_session_id: Option<String>,
    replaying_fork_history: bool,
    current_turn_id: String,
    waiting_calls: HashMap<String, String>,
    terminal_turns: HashSet<String>,
    terminal_turn_order: VecDeque<String>,
    active_turns: HashSet<String>,
    turn_configuration_order: VecDeque<String>,
    latest_terminal: SessionLifecycleStatus,
    events: ParsedRolloutEvents,
}

impl RolloutParseState {
    /// Consumes whole lines from `chunk`, returning how many bytes were taken.
    /// A trailing partial line is only consumed once it parses as a complete
    /// JSON record, so a rollout caught mid-write resumes cleanly.
    fn consume(&mut self, chunk: &str) -> usize {
        if self.is_subagent {
            return chunk.len();
        }
        let mut consumed = 0usize;
        for segment in chunk.split_inclusive('\n') {
            let is_complete_line = segment.ends_with('\n');
            let line = segment.trim_end_matches(['\n', '\r']);
            if !is_complete_line {
                if line.trim().is_empty() {
                    break;
                }
                let Ok(record) = serde_json::from_str::<Value>(line) else {
                    break;
                };
                consumed += segment.len();
                self.apply(&record);
                break;
            }
            consumed += segment.len();
            if let Ok(record) = serde_json::from_str::<Value>(line) {
                self.apply(&record);
            }
            if self.is_subagent {
                return chunk.len();
            }
        }
        consumed
    }

    fn advance(&mut self, chunk: &str, consumed: usize) {
        if consumed == 0 {
            return;
        }
        let taken = &chunk.as_bytes()[..consumed];
        if self.consumed_bytes == 0 {
            self.head_fingerprint.set_prefix(taken);
        }
        self.consumed_bytes += consumed as u64;
        self.tail_fingerprint.push_tail(taken);
    }

    fn apply(&mut self, record: &Value) {
        let Some(payload) = record.get("payload") else {
            return;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if is_subagent_payload(payload) {
                    self.is_subagent = true;
                }
                self.observe_session_meta(payload);
            }
            Some("turn_context") => {
                if let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) {
                    self.current_turn_id = turn_id.to_string();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let reasoning_effort = payload
                        .get("effort")
                        .or_else(|| payload.get("reasoning_effort"))
                        .or_else(|| payload.get("reasoningEffort"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if !model.is_empty() || !reasoning_effort.is_empty() {
                        self.remember_turn_configuration(
                            turn_id.to_string(),
                            TurnConfiguration {
                                model,
                                reasoning_effort,
                            },
                        );
                    }
                }
            }
            Some("event_msg") => {
                let Some(turn_id) = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|turn_id| !turn_id.is_empty())
                else {
                    return;
                };
                match payload.get("type").and_then(Value::as_str) {
                    Some("task_started") => {
                        if self.active_turns.insert(turn_id.to_string()) {
                            self.events.started_turns.push(turn_id.to_string());
                        }
                    }
                    Some("task_complete") => {
                        let error = task_completion_error(payload);
                        self.latest_terminal = if error.is_some() {
                            SessionLifecycleStatus::Error
                        } else {
                            SessionLifecycleStatus::Idle
                        };
                        if self.finish_turn(turn_id) {
                            if self.replaying_fork_history {
                                self.events
                                    .snapshot_replay_turns
                                    .insert(turn_id.to_string());
                            }
                            self.events.completed_turns.push((
                                turn_id.to_string(),
                                payload
                                    .get("duration_ms")
                                    .and_then(Value::as_u64)
                                    .unwrap_or_default() as u128,
                                payload.get("completed_at").and_then(Value::as_i64),
                                error,
                            ));
                            self.remember_terminal_turn(turn_id);
                        }
                    }
                    Some("turn_aborted") => {
                        self.latest_terminal = SessionLifecycleStatus::Idle;
                        if self.finish_turn(turn_id) {
                            if self.replaying_fork_history {
                                self.events
                                    .snapshot_replay_turns
                                    .insert(turn_id.to_string());
                            }
                            self.events.aborted_turns.push(turn_id.to_string());
                            self.remember_terminal_turn(turn_id);
                        }
                    }
                    _ => {}
                }
            }
            Some("response_item") => match payload.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    if !function_call_requires_approval(payload) {
                        return;
                    }
                    let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                        return;
                    };
                    let turn_id = payload
                        .get("internal_chat_message_metadata_passthrough")
                        .and_then(|metadata| metadata.get("turn_id"))
                        .and_then(Value::as_str)
                        .unwrap_or(&self.current_turn_id);
                    self.waiting_calls
                        .insert(call_id.to_string(), turn_id.to_string());
                }
                Some("function_call_output") => {
                    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                        self.waiting_calls.remove(call_id);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn finish_turn(&mut self, turn_id: &str) -> bool {
        self.active_turns.remove(turn_id);
        self.waiting_calls
            .retain(|_, waiting_turn_id| waiting_turn_id != turn_id);
        self.terminal_turns.insert(turn_id.to_string())
    }

    fn remember_terminal_turn(&mut self, turn_id: &str) {
        self.terminal_turn_order.push_back(turn_id.to_string());
        while self.terminal_turn_order.len() > MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT {
            let Some(oldest) = self.terminal_turn_order.pop_front() else {
                break;
            };
            self.terminal_turns.remove(&oldest);
            self.events
                .started_turns
                .retain(|candidate| candidate != &oldest);
            self.events
                .completed_turns
                .retain(|(candidate, _, _, _)| candidate != &oldest);
            self.events
                .aborted_turns
                .retain(|candidate| candidate != &oldest);
            self.events.snapshot_replay_turns.remove(&oldest);
            self.events.turn_configurations.remove(&oldest);
            self.turn_configuration_order
                .retain(|candidate| candidate != &oldest);
        }
    }

    fn remember_turn_configuration(&mut self, turn_id: String, configuration: TurnConfiguration) {
        let is_new = !self.events.turn_configurations.contains_key(&turn_id);
        self.events
            .turn_configurations
            .insert(turn_id.clone(), configuration);
        if is_new {
            self.turn_configuration_order.push_back(turn_id);
        }
        while self.turn_configuration_order.len() > MAX_CACHED_TURN_CONFIGURATIONS_PER_ROLLOUT {
            let removable_index = self
                .turn_configuration_order
                .iter()
                .position(|candidate| {
                    !self.active_turns.contains(candidate)
                        && !self
                            .waiting_calls
                            .values()
                            .any(|waiting_turn_id| waiting_turn_id == candidate)
                })
                .unwrap_or(0);
            if let Some(oldest) = self.turn_configuration_order.remove(removable_index) {
                self.events.turn_configurations.remove(&oldest);
            }
        }
    }

    fn observe_session_meta(&mut self, payload: &Value) {
        let session_id = payload
            .get("id")
            .or_else(|| payload.get("session_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty());
        let is_fork_header = payload
            .get("forked_from_id")
            .and_then(Value::as_str)
            .is_some_and(|parent_id| !parent_id.trim().is_empty());

        if self.forked_session_id.is_none() && is_fork_header {
            self.forked_session_id = session_id.map(ToString::to_string);
        }
        if let (Some(forked_session_id), Some(session_id)) =
            (self.forked_session_id.as_deref(), session_id)
        {
            // A fork rollout starts with its new header, replays one or more
            // ancestor session snapshots, then writes the new header again
            // before live events continue.
            self.replaying_fork_history = session_id != forked_session_id;
        }
    }

    fn pending_approvals(&self) -> Vec<(String, String)> {
        let mut pending_approvals = self
            .waiting_calls
            .iter()
            .filter(|(_, turn_id)| {
                !turn_id.is_empty() && !self.terminal_turns.contains(turn_id.as_str())
            })
            .map(|(call_id, turn_id)| (turn_id.clone(), call_id.clone()))
            .collect::<Vec<_>>();
        pending_approvals.sort();
        pending_approvals
    }

    fn lifecycle_status(&self, pending_approvals: &[(String, String)]) -> SessionLifecycleStatus {
        if !pending_approvals.is_empty() {
            SessionLifecycleStatus::Waiting
        } else if !self.active_turns.is_empty() {
            SessionLifecycleStatus::Running
        } else {
            self.latest_terminal
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> Option<ParsedRolloutEvents> {
        if self.is_subagent {
            return None;
        }
        let mut parsed = self.events.clone();
        parsed.pending_approvals = self.pending_approvals();
        Some(parsed)
    }
}

#[derive(Debug, Clone)]
struct CachedRolloutEvents {
    session_id: String,
    signature: RolloutSignature,
    state: RolloutParseState,
}

#[derive(Debug)]
struct CachedDatabaseConnection {
    signature: RolloutSignature,
    connection: Connection,
}

#[derive(Debug, Default)]
pub struct RecentSessionEventCache {
    rollouts: HashMap<PathBuf, CachedRolloutEvents>,
    database_discovery: CodexSessionDbDiscoveryCache,
    database_connections: HashMap<PathBuf, CachedDatabaseConnection>,
    last_snapshot: Option<Arc<RecentSessionEvents>>,
    #[cfg(test)]
    parse_count: usize,
    #[cfg(test)]
    incremental_parse_count: usize,
    #[cfg(test)]
    database_open_count: usize,
}

impl RecentSessionEventCache {
    /// Finds recent session lifecycle events from rollout data. Unchanged
    /// rollouts reuse their compact parsed event set across polling cycles.
    pub fn refresh(&mut self, home: &Path) -> Arc<RecentSessionEvents> {
        let recent_after = SystemTime::now()
            .checked_sub(RECENT_SESSION_WINDOW)
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default();
        let rollouts = self.recent_codey_rollouts(home, recent_after);
        self.refresh_rollouts(rollouts)
    }

    pub(crate) fn release_database_connections(&mut self) {
        self.database_connections.clear();
    }

    fn recent_codey_rollouts(&mut self, home: &Path, recent_after: i64) -> Vec<(String, PathBuf)> {
        let database_paths = self.session_database_paths(home);
        let mut rollouts = Vec::new();
        for database_path in database_paths {
            if !self.ensure_database_connection(&database_path) {
                continue;
            }
            let query_result = self
                .database_connections
                .get(&database_path)
                .map(|cached| query_recent_codey_rollouts(&cached.connection, home, recent_after));
            match query_result {
                Some(Ok(rows)) => rollouts.extend(rows),
                Some(Err(_)) => {
                    // A replaced database or schema migration can leave a cached
                    // read handle stale. Reopen it on the next polling cycle.
                    self.database_connections.remove(&database_path);
                }
                None => {}
            }
        }
        rollouts.sort();
        rollouts.dedup();
        rollouts
    }

    fn session_database_paths(&mut self, home: &Path) -> Vec<PathBuf> {
        let database_paths = self.database_discovery.session_db_paths_from_home(home);
        let active_database_paths = database_paths.iter().cloned().collect::<HashSet<_>>();
        self.database_connections
            .retain(|path, _| active_database_paths.contains(path));
        database_paths
    }

    fn ensure_database_connection(&mut self, database_path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(database_path) else {
            self.database_connections.remove(database_path);
            return false;
        };
        let signature = RolloutSignature::from_metadata(&metadata);
        let connection_is_current = self
            .database_connections
            .get(database_path)
            .is_some_and(|cached| cached.signature == signature);
        if !connection_is_current {
            self.database_connections.remove(database_path);
        }
        if !self.database_connections.contains_key(database_path) {
            let Ok(connection) = Connection::open_with_flags(
                database_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) else {
                return false;
            };
            #[cfg(test)]
            {
                self.database_open_count += 1;
            }
            self.database_connections.insert(
                database_path.to_path_buf(),
                CachedDatabaseConnection {
                    signature,
                    connection,
                },
            );
        }
        true
    }

    fn refresh_rollouts(&mut self, rollouts: Vec<(String, PathBuf)>) -> Arc<RecentSessionEvents> {
        let active_paths = rollouts
            .iter()
            .map(|(_, path)| path.clone())
            .collect::<HashSet<_>>();
        let mut snapshot_changed = self.last_snapshot.is_none()
            || self.rollouts.len() != active_paths.len()
            || self
                .rollouts
                .keys()
                .any(|path| !active_paths.contains(path));
        self.rollouts.retain(|path, _| active_paths.contains(path));

        for (session_id, rollout_path) in &rollouts {
            let Ok(metadata) = fs::metadata(rollout_path) else {
                snapshot_changed = true;
                self.rollouts.remove(rollout_path);
                continue;
            };
            let signature = RolloutSignature::from_metadata(&metadata);
            let cache_is_current = self.rollouts.get(rollout_path).is_some_and(|cached| {
                cached.session_id == *session_id && cached.signature == signature
            });
            if !cache_is_current {
                snapshot_changed = true;
                // Reuse the resumable parser state when the file only grew and
                // its consumed prefix is intact; otherwise start from scratch.
                let resumable = self
                    .rollouts
                    .remove(rollout_path)
                    .filter(|cached| cached.session_id == *session_id)
                    .map(|cached| cached.state)
                    .filter(|state| state.consumed_bytes > 0);
                let Some((state, _was_incremental)) = read_rollout_update(rollout_path, resumable)
                else {
                    continue;
                };
                #[cfg(test)]
                {
                    self.parse_count += 1;
                    if _was_incremental {
                        self.incremental_parse_count += 1;
                    }
                }
                self.rollouts.insert(
                    rollout_path.clone(),
                    CachedRolloutEvents {
                        session_id: session_id.clone(),
                        signature: signature.clone(),
                        state,
                    },
                );
            }
        }

        if !snapshot_changed && let Some(snapshot) = self.last_snapshot.as_ref() {
            if snapshot.pending_approvals.is_empty() {
                return Arc::clone(snapshot);
            }
            let snapshot = Arc::new(RecentSessionEvents {
                pending_approvals: self.refresh_pending_approvals(&rollouts),
                started_turns: Arc::clone(&snapshot.started_turns),
                aborted_turns: Arc::clone(&snapshot.aborted_turns),
                completed_turns: Arc::clone(&snapshot.completed_turns),
                session_statuses: Arc::clone(&snapshot.session_statuses),
                turn_configurations: Arc::clone(&snapshot.turn_configurations),
            });
            self.last_snapshot = Some(Arc::clone(&snapshot));
            return snapshot;
        }

        let mut pending_approvals = Vec::new();
        let mut started_turns = Vec::new();
        let mut aborted_turns = Vec::new();
        let mut completed_turns = Vec::new();
        let mut session_statuses = HashMap::new();
        let mut turn_configurations = HashMap::new();
        for (session_id, rollout_path) in rollouts {
            let Some(cached) = self
                .rollouts
                .get(&rollout_path)
                .filter(|cached| cached.session_id == session_id && !cached.state.is_subagent)
            else {
                continue;
            };
            let state = &cached.state;
            let signature = &cached.signature;
            let waiting_approvals = state.pending_approvals();
            let status = state.lifecycle_status(&waiting_approvals);
            let parsed = &state.events;
            let rollout_is_snapshot_replay = rollout_path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|directory| directory == "imported");
            let duration_ms = signature
                .modified
                .and_then(|modified| modified.elapsed().ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            session_statuses.insert(session_id.clone(), status);
            turn_configurations.insert(session_id.clone(), parsed.turn_configurations.clone());
            pending_approvals.extend(waiting_approvals.into_iter().map(|(turn_id, waiting_id)| {
                PendingApproval {
                    session_id: session_id.clone(),
                    turn_id,
                    waiting_id,
                    duration_ms,
                }
            }));
            started_turns.extend(
                parsed
                    .started_turns
                    .iter()
                    .cloned()
                    .map(|turn_id| StartedTurn {
                        session_id: session_id.clone(),
                        turn_id,
                    }),
            );
            completed_turns.extend(parsed.completed_turns.iter().cloned().map(
                |(turn_id, duration_ms, completed_at, error)| CompletedTurn {
                    is_snapshot_replay: rollout_is_snapshot_replay
                        || parsed.snapshot_replay_turns.contains(&turn_id),
                    session_id: session_id.clone(),
                    turn_id,
                    duration_ms,
                    completed_at,
                    error,
                },
            ));
            aborted_turns.extend(
                parsed
                    .aborted_turns
                    .iter()
                    .cloned()
                    .map(|turn_id| AbortedTurn {
                        is_snapshot_replay: rollout_is_snapshot_replay
                            || parsed.snapshot_replay_turns.contains(&turn_id),
                        session_id: session_id.clone(),
                        turn_id,
                    }),
            );
        }

        let snapshot = Arc::new(RecentSessionEvents {
            pending_approvals,
            started_turns: Arc::new(started_turns),
            aborted_turns: Arc::new(aborted_turns),
            completed_turns: Arc::new(completed_turns),
            session_statuses: Arc::new(session_statuses),
            turn_configurations: Arc::new(turn_configurations),
        });
        self.last_snapshot = Some(Arc::clone(&snapshot));
        snapshot
    }

    fn refresh_pending_approvals(&self, rollouts: &[(String, PathBuf)]) -> Vec<PendingApproval> {
        let mut pending_approvals = Vec::new();
        for (session_id, rollout_path) in rollouts {
            let Some(cached) = self
                .rollouts
                .get(rollout_path)
                .filter(|cached| cached.session_id == *session_id && !cached.state.is_subagent)
            else {
                continue;
            };
            let duration_ms = cached
                .signature
                .modified
                .and_then(|modified| modified.elapsed().ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            pending_approvals.extend(cached.state.pending_approvals().into_iter().map(
                |(turn_id, waiting_id)| PendingApproval {
                    session_id: session_id.clone(),
                    turn_id,
                    waiting_id,
                    duration_ms,
                },
            ));
        }
        pending_approvals
    }
}

/// Returns an updated parser state plus whether the previous state was resumed.
/// Falls back to a fresh state and the whole file whenever the previously
/// consumed prefix cannot be confirmed byte-for-byte.
fn read_rollout_update(
    path: &Path,
    resumable: Option<RolloutParseState>,
) -> Option<(RolloutParseState, bool)> {
    let full = || {
        let file = fs::File::open(path).ok()?;
        let mut state = RolloutParseState::default();
        consume_rollout_reader(BufReader::new(file), &mut state)?;
        Some((state, false))
    };
    let Some(mut state) = resumable else {
        return full();
    };
    if state.is_subagent {
        return Some((state, true));
    }
    let fingerprint_len = state.tail_fingerprint.len() as u64;
    if fingerprint_len == 0 || fingerprint_len > state.consumed_bytes {
        return full();
    }
    let Ok(mut file) = fs::File::open(path) else {
        return full();
    };
    // The caller only reaches this path because the file changed. Without new
    // bytes the change must have been an in-place rewrite, which invalidates
    // the accumulated state even though the tail may look untouched.
    match file.metadata() {
        Ok(metadata) if metadata.len() > state.consumed_bytes => {}
        _ => return full(),
    }
    // Rollout rewrites (provider normalisation) edit the `session_meta` header
    // and leave the tail byte-identical, so the head has to be checked too.
    if !state.head_fingerprint.is_empty() {
        let head_len = state.head_fingerprint.len();
        let mut head = [0u8; ROLLOUT_FINGERPRINT_LEN];
        if file.read_exact(&mut head[..head_len]).is_err()
            || head[..head_len] != *state.head_fingerprint.as_slice()
        {
            return full();
        }
    }
    if file
        .seek(SeekFrom::Start(state.consumed_bytes - fingerprint_len))
        .is_err()
    {
        return full();
    }
    let tail_len = state.tail_fingerprint.len();
    let mut fingerprint = [0u8; ROLLOUT_FINGERPRINT_LEN];
    if file.read_exact(&mut fingerprint[..tail_len]).is_err()
        || fingerprint[..tail_len] != *state.tail_fingerprint.as_slice()
    {
        return full();
    }
    consume_rollout_reader(BufReader::new(file), &mut state)?;
    Some((state, true))
}

fn consume_rollout_reader(mut reader: impl BufRead, state: &mut RolloutParseState) -> Option<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).ok()?;
        if read == 0 {
            return Some(());
        }
        let consumed = state.consume(&line);
        state.advance(&line, consumed);
        if state.is_subagent {
            return Some(());
        }
    }
}

#[cfg(test)]
fn parse_rollout_events(contents: &str) -> Option<ParsedRolloutEvents> {
    let mut state = RolloutParseState::default();
    state.consume(contents);
    state.snapshot()
}

fn is_subagent_payload(payload: &Value) -> bool {
    payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
        || payload.get("parent_thread_id").is_some_and(|parent| {
            parent
                .as_str()
                .is_some_and(|value| !value.trim().is_empty())
        })
        || payload
            .get("source")
            .and_then(Value::as_object)
            .is_some_and(|source| {
                source.contains_key("subagent") || source.contains_key("sub_agent")
            })
}

#[cfg(test)]
fn rollout_is_subagent(contents: &str) -> bool {
    parse_rollout_events(contents).is_none()
}

fn query_recent_codey_rollouts(
    connection: &Connection,
    home: &Path,
    recent_after: i64,
) -> rusqlite::Result<Vec<(String, PathBuf)>> {
    let mut rollouts = Vec::new();
    // SQLite columns can contain values that violate their declared affinity.
    // Ignore only malformed rows here; prepare/query failures still invalidate
    // the cached connection so a replaced database is reopened next poll.
    let mut statement = connection.prepare_cached(
        "SELECT id, rollout_path FROM threads \
         WHERE archived=0 AND updated_at >= ?1 \
           AND typeof(id)='text' AND typeof(rollout_path)='text' \
         ORDER BY updated_at DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![recent_after, MAX_RECENT_SESSIONS], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (session_id, rollout_path) = row?;
        let path = PathBuf::from(rollout_path);
        rollouts.push((
            session_id,
            if path.is_absolute() {
                path
            } else {
                home.join(path)
            },
        ));
    }
    Ok(rollouts)
}

#[cfg(test)]
fn pending_approvals_in_rollout(contents: &str) -> Vec<(String, String)> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.pending_approvals)
        .unwrap_or_default()
}

#[cfg(test)]
fn session_lifecycle_status_in_rollout(
    contents: &str,
    _pending_approvals: &[(String, String)],
) -> SessionLifecycleStatus {
    let mut state = RolloutParseState::default();
    state.consume(contents);
    if state.is_subagent {
        return SessionLifecycleStatus::Idle;
    }
    let pending_approvals = state.pending_approvals();
    state.lifecycle_status(&pending_approvals)
}

fn task_completion_error(payload: &Value) -> Option<String> {
    payload.get("error").and_then(|error| {
        if error.is_null() {
            None
        } else {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(ToString::to_string)
                .or_else(|| Some(error.to_string()))
        }
    })
}

fn function_call_requires_approval(payload: &Value) -> bool {
    match payload.get("name").and_then(Value::as_str) {
        Some("request_permissions" | "request_user_input") => true,
        Some("exec_command") => {
            let Some(arguments) = payload.get("arguments") else {
                return false;
            };
            match arguments {
                Value::String(arguments) => serde_json::from_str::<Value>(arguments)
                    .ok()
                    .is_some_and(|arguments| exec_command_requires_escalation(&arguments)),
                Value::Object(_) => exec_command_requires_escalation(arguments),
                _ => false,
            }
        }
        _ => false,
    }
}

fn exec_command_requires_escalation(arguments: &Value) -> bool {
    arguments.get("sandbox_permissions").and_then(Value::as_str) == Some("require_escalated")
}

#[cfg(test)]
fn started_turns_in_rollout(contents: &str) -> Vec<String> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.started_turns)
        .unwrap_or_default()
}

#[cfg(test)]
fn completed_turns_in_rollout(contents: &str) -> Vec<(String, u128, Option<i64>, Option<String>)> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.completed_turns)
        .unwrap_or_default()
}

#[cfg(test)]
fn aborted_turns_in_rollout(contents: &str) -> Vec<String> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.aborted_turns)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn identifies_subagent_rollouts_without_hiding_root_sessions() {
        let root = r#"{"type":"session_meta","payload":{"id":"root","thread_source":"codey"}}"#;
        let child = r#"{"type":"session_meta","payload":{"id":"child","thread_source":"subagent","source":{"subagent":{"thread_spawn":{"parent_thread_id":"root","depth":1}}}}}"#;
        let legacy_child =
            r#"{"type":"session_meta","payload":{"id":"child","parent_thread_id":"root"}}"#;

        assert!(!rollout_is_subagent(root));
        assert!(rollout_is_subagent(child));
        assert!(rollout_is_subagent(legacy_child));
    }

    #[test]
    fn recent_rollout_query_skips_malformed_rows_without_losing_valid_sessions() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT NOT NULL,
                    rollout_path,
                    archived INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path, archived, updated_at)
                 VALUES (?1, ?2, 0, ?3)",
                params!["valid", "rollouts/valid.jsonl", 1_i64],
            )
            .unwrap();

        let home = Path::new("codey-home");
        assert_eq!(
            query_recent_codey_rollouts(&connection, home, 0).unwrap(),
            vec![("valid".to_string(), home.join("rollouts/valid.jsonl"),)]
        );

        connection
            .execute(
                "INSERT INTO threads (id, rollout_path, archived, updated_at)
                 VALUES (?1, NULL, 0, ?2)",
                params!["invalid", 2_i64],
            )
            .unwrap();
        assert_eq!(
            query_recent_codey_rollouts(&connection, home, 0).unwrap(),
            vec![("valid".to_string(), home.join("rollouts/valid.jsonl"),)]
        );

        connection.execute_batch("DROP TABLE threads;").unwrap();
        assert!(query_recent_codey_rollouts(&connection, home, 0).is_err());
    }

    #[test]
    fn finds_only_unresolved_waiting_calls() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_permissions","call_id":"pending"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"resolved","internal_chat_message_metadata_passthrough":{"turn_id":"turn-2"}}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"resolved"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"not-waiting"}}
{"type":"turn_context","payload":{"turn_id":"turn-3"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"aborted"}}
{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-3"}}
"#;

        assert_eq!(
            pending_approvals_in_rollout(rollout),
            vec![("turn-1".to_string(), "pending".to_string())]
        );
    }

    #[test]
    fn finds_only_unresolved_escalated_exec_commands() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-exec"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"sandbox_permissions\":\"use_default\"}","call_id":"default"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"not-json","call_id":"invalid"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"sandbox_permissions\":\"require_escalated\"}","call_id":"resolved"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"resolved"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"sandbox_permissions\":\"require_escalated\"}","call_id":"pending"}}
"#;

        assert_eq!(
            pending_approvals_in_rollout(rollout),
            vec![("turn-exec".to_string(), "pending".to_string())]
        );
    }

    #[test]
    fn finds_authoritative_task_lifecycle_events() {
        let rollout = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","duration_ms":1234,"completed_at":200}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-error"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-error","duration_ms":500,"completed_at":300,"error":{"message":"upstream failed"}}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":""}}
{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-2","duration_ms":500}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"","duration_ms":10}}
"#;

        assert_eq!(
            started_turns_in_rollout(rollout),
            vec!["turn-1", "turn-error"]
        );
        assert_eq!(aborted_turns_in_rollout(rollout), vec!["turn-2"]);
        assert_eq!(
            completed_turns_in_rollout(rollout),
            vec![
                ("turn-1".to_string(), 1234, Some(200), None),
                (
                    "turn-error".to_string(),
                    500,
                    Some(300),
                    Some("upstream failed".to_string())
                )
            ]
        );
    }

    #[test]
    fn captures_model_and_reasoning_effort_for_each_turn() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-1","model":"gpt-5.6-luna","effort":"xhigh"}}
{"type":"turn_context","payload":{"turn_id":"turn-2","model":"gpt-5.4","effort":"low"}}
"#;

        let parsed = parse_rollout_events(rollout).unwrap();
        assert_eq!(
            parsed.turn_configurations.get("turn-1"),
            Some(&TurnConfiguration {
                model: "gpt-5.6-luna".to_string(),
                reasoning_effort: "xhigh".to_string(),
            })
        );
        assert_eq!(
            parsed.turn_configurations.get("turn-2"),
            Some(&TurnConfiguration {
                model: "gpt-5.4".to_string(),
                reasoning_effort: "low".to_string(),
            })
        );
    }

    #[test]
    fn rollout_cache_keeps_only_recent_terminal_turn_history() {
        let mut rollout = String::new();
        for index in 0..=MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT {
            rollout.push_str(&format!(
                "{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"turn-{index}\",\"model\":\"model-{index}\"}}}}\n"
            ));
            rollout.push_str(&format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"turn-{index}\"}}}}\n"
            ));
            rollout.push_str(&format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"turn-{index}\",\"completed_at\":{index}}}}}\n"
            ));
        }

        let mut state = RolloutParseState::default();
        state.consume(&rollout);
        let parsed = state.snapshot().unwrap();
        let latest_turn = format!("turn-{MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT}");

        assert_eq!(
            state.terminal_turn_order.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            state.terminal_turns.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            parsed.started_turns.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            parsed.completed_turns.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            parsed.turn_configurations.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert!(!state.terminal_turns.contains("turn-0"));
        assert!(!parsed.started_turns.iter().any(|turn| turn == "turn-0"));
        assert!(
            !parsed
                .completed_turns
                .iter()
                .any(|(turn, _, _, _)| turn == "turn-0")
        );
        assert!(!parsed.turn_configurations.contains_key("turn-0"));
        assert_eq!(
            parsed.started_turns.first().map(String::as_str),
            Some("turn-1")
        );
        assert_eq!(
            parsed.started_turns.last().map(String::as_str),
            Some(latest_turn.as_str())
        );
    }

    #[test]
    fn terminal_turns_release_waiting_approval_state() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"approval-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}
"#;
        let mut state = RolloutParseState::default();

        state.consume(rollout);

        assert!(state.waiting_calls.is_empty());
        assert!(state.pending_approvals().is_empty());
    }

    #[test]
    fn derives_authoritative_session_lifecycle_status() {
        let running = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
"#;
        assert_eq!(
            session_lifecycle_status_in_rollout(running, &[]),
            SessionLifecycleStatus::Running
        );

        let waiting = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"approval-1"}}
"#;
        let pending = pending_approvals_in_rollout(waiting);
        assert_eq!(
            session_lifecycle_status_in_rollout(waiting, &pending),
            SessionLifecycleStatus::Waiting
        );

        let completed = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}
"#;
        assert_eq!(
            session_lifecycle_status_in_rollout(completed, &[]),
            SessionLifecycleStatus::Idle
        );

        let failed = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","error":{"message":"boom"}}}
"#;
        assert_eq!(
            session_lifecycle_status_in_rollout(failed, &[]),
            SessionLifecycleStatus::Error
        );
    }

    #[test]
    fn a_new_successful_turn_clears_an_older_error_status() {
        let rollout = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","error":{"message":"boom"}}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2"}}
"#;

        assert_eq!(
            session_lifecycle_status_in_rollout(rollout, &[]),
            SessionLifecycleStatus::Idle
        );
    }

    #[test]
    fn forked_rollouts_only_mark_inherited_completions_as_snapshot_replays() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-fork.jsonl");
        fs::write(
            &rollout_path,
            r#"
{"type":"session_meta","payload":{"id":"fork","session_id":"fork","forked_from_id":"parent"}}
{"type":"session_meta","payload":{"id":"parent","session_id":"parent","forked_from_id":"grandparent"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"inherited"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"inherited","completed_at":300}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"inherited-aborted"}}
{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"inherited-aborted"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"live"}}
{"type":"session_meta","payload":{"id":"fork","session_id":"fork","forked_from_id":"parent"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"live","completed_at":301}}
"#,
        )
        .unwrap();
        let mut cache = RecentSessionEventCache::default();

        let events = cache.refresh_rollouts(vec![("fork".to_string(), rollout_path.to_path_buf())]);

        assert_eq!(
            events
                .completed_turns
                .iter()
                .map(|turn| (turn.turn_id.as_str(), turn.is_snapshot_replay))
                .collect::<Vec<_>>(),
            vec![("inherited", true), ("live", false)]
        );
        assert_eq!(
            events
                .aborted_turns
                .iter()
                .map(|turn| (turn.turn_id.as_str(), turn.is_snapshot_replay))
                .collect::<Vec<_>>(),
            vec![("inherited-aborted", true)]
        );
    }

    #[test]
    fn unchanged_rollouts_reuse_cached_lifecycle_events() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        fs::write(
            &rollout_path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        )
        .unwrap();
        let rollouts = || vec![("thread-1".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        let first = cache.refresh_rollouts(rollouts());
        let second = cache.refresh_rollouts(rollouts());

        assert_eq!(cache.parse_count, 1);
        assert_eq!(
            first.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Running)
        );
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.started_turns, first.started_turns);

        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&rollout_path)
                .unwrap(),
            r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-1"}}}}"#
        )
        .unwrap();
        let updated = cache.refresh_rollouts(rollouts());

        assert_eq!(cache.parse_count, 2);
        assert_eq!(
            updated.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Idle)
        );
    }

    #[test]
    fn pending_approval_snapshots_are_rebuilt_for_fresh_wait_durations() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-waiting.jsonl");
        fs::write(
            &rollout_path,
            r#"{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_permissions","call_id":"pending"}}
"#,
        )
        .unwrap();
        let rollouts = || vec![("waiting".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        let first = cache.refresh_rollouts(rollouts());
        let second = cache.refresh_rollouts(rollouts());

        assert_eq!(first.pending_approvals.len(), 1);
        assert_eq!(second.pending_approvals.len(), 1);
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&first.started_turns, &second.started_turns));
        assert!(Arc::ptr_eq(&first.aborted_turns, &second.aborted_turns));
        assert!(Arc::ptr_eq(&first.completed_turns, &second.completed_turns));
        assert!(Arc::ptr_eq(
            &first.session_statuses,
            &second.session_statuses
        ));
        assert!(Arc::ptr_eq(
            &first.turn_configurations,
            &second.turn_configurations
        ));
    }

    #[test]
    fn appended_rollout_lines_are_parsed_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        let mut contents = String::from(
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        );
        contents.push('\n');
        // Pad past the fingerprint window so the resume check is meaningful.
        for index in 0..40 {
            contents.push_str(&format!(
                "{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"turn-{index}\",\"model\":\"m\"}}}}\n"
            ));
        }
        fs::write(&rollout_path, &contents).unwrap();
        let rollouts = || vec![("thread-1".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        let first = cache.refresh_rollouts(rollouts());
        assert_eq!(cache.incremental_parse_count, 0);
        assert_eq!(
            first.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Running)
        );

        let mut handle = fs::OpenOptions::new()
            .append(true)
            .open(&rollout_path)
            .unwrap();
        writeln!(
            handle,
            r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-1"}}}}"#
        )
        .unwrap();
        drop(handle);

        let updated = cache.refresh_rollouts(rollouts());

        assert_eq!(
            cache.incremental_parse_count, 1,
            "an appended rollout must resume instead of re-reading the whole file"
        );
        assert_eq!(
            updated.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Idle)
        );
        assert_eq!(updated.started_turns.len(), 1, "history must not be lost");
        assert_eq!(updated.completed_turns.len(), 1);
        assert_eq!(updated.turn_configurations["thread-1"].len(), 40);
    }

    #[test]
    fn a_partial_jsonl_tail_is_resumed_after_the_writer_finishes_the_line() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        fs::write(
            &rollout_path,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn"
            ),
        )
        .unwrap();
        let rollouts = || vec![("thread-1".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        let first = cache.refresh_rollouts(rollouts());
        assert!(first.started_turns.is_empty());

        fs::OpenOptions::new()
            .append(true)
            .open(&rollout_path)
            .unwrap()
            .write_all(b"-1\"}}\n")
            .unwrap();
        let updated = cache.refresh_rollouts(rollouts());

        assert_eq!(cache.incremental_parse_count, 1);
        assert_eq!(
            updated
                .started_turns
                .iter()
                .map(|turn| turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["turn-1"]
        );
        assert_eq!(
            updated.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Running)
        );
    }

    #[test]
    fn rewritten_rollouts_fall_back_to_a_full_parse() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        let original = format!(
            "{}\n{}\n",
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#
        );
        fs::write(&rollout_path, &original).unwrap();
        let rollouts = || vec![("thread-1".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        let first = cache.refresh_rollouts(rollouts());
        assert_eq!(first.started_turns.len(), 2);

        // Codey rewrites rollouts in place when deleting turns; the cached
        // prefix is then stale. Make the rewrite longer than the original so
        // only the fingerprint check — not the shrink check — can catch it.
        let rewritten = format!(
            "{}\n{}\n{}\n",
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-9"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-9"}}"#,
            r#"{"type":"turn_context","payload":{"turn_id":"turn-9","model":"m"}}"#
        );
        assert!(rewritten.len() > original.len());
        fs::write(&rollout_path, &rewritten).unwrap();

        let updated = cache.refresh_rollouts(rollouts());

        assert_eq!(
            cache.incremental_parse_count, 0,
            "an in-place rewrite must not be treated as an append"
        );
        assert_eq!(
            updated
                .started_turns
                .iter()
                .map(|turn| turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["turn-9"]
        );
        assert_eq!(
            updated.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Idle)
        );
    }

    #[test]
    fn header_rewrites_with_an_identical_tail_fall_back_to_a_full_parse() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        let header = |provider: &str| {
            format!(
                r#"{{"type":"session_meta","payload":{{"model_provider":"{provider}","id":"thread-1"}}}}"#
            )
        };
        let body = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#;
        fs::write(
            &rollout_path,
            format!("{}\n{}\n", header("aaaaa_global"), body),
        )
        .unwrap();
        let rollouts = || vec![("thread-1".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        cache.refresh_rollouts(rollouts());

        // Provider normalisation rewrites only the session_meta header, so the
        // consumed tail stays byte-identical; the head check has to catch it.
        fs::write(
            &rollout_path,
            format!(
                "{}\n{}\n{}\n",
                header("codey_global"),
                body,
                r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#
            ),
        )
        .unwrap();

        cache.refresh_rollouts(rollouts());

        assert_eq!(
            cache.incremental_parse_count, 0,
            "a rewritten header must not be resumed from the stale offset"
        );
    }

    #[test]
    fn unchanged_database_files_reuse_read_connections() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        fs::write(
            &rollout_path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        )
        .unwrap();
        let database_path = temp.path().join("state_5.sqlite");
        let database = Connection::open(&database_path).unwrap();
        database
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    rollout_path TEXT NOT NULL,
                    archived INTEGER NOT NULL DEFAULT 0,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        let updated_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        database
            .execute(
                "INSERT INTO threads (id, rollout_path, archived, updated_at)
                 VALUES (?1, ?2, 0, ?3)",
                params!["thread-1", rollout_path.to_string_lossy(), updated_at],
            )
            .unwrap();
        drop(database);

        let mut cache = RecentSessionEventCache::default();
        let first = cache.refresh(temp.path());
        let second = cache.refresh(temp.path());

        assert_eq!(cache.database_open_count, 1);
        assert_eq!(
            first.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Running)
        );
        assert_eq!(second.started_turns, first.started_turns);

        cache.release_database_connections();
        assert!(cache.database_connections.is_empty());
    }
}
