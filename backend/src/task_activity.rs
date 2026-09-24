//! 外部插件按需查询任务数量。没有调用时不扫描会话，也不写出会话内容。
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::pending_approval::{RecentSessionEvents, SessionLifecycleStatus};

static CACHE: Mutex<Option<crate::pending_approval::RecentSessionEventCache>> = Mutex::new(None);

pub(crate) async fn task_counts() -> Value {
    let cache = {
        let mut guard = CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.take().unwrap_or_default()
    };
    let home = crate::codex_config::codex_home().to_path_buf();
    let scanned = tokio::task::spawn_blocking(move || {
        let mut cache = cache;
        let events = cache.refresh(&home);
        let counts = counts_from_events(&events);
        (cache, counts)
    })
    .await;
    let Ok((cache, counts)) = scanned else {
        return json!({ "running": 0, "failed": 0 });
    };
    *CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cache);
    counts
}

fn counts_from_events(events: &RecentSessionEvents) -> Value {
    let mut running = 0_u32;
    let mut failed = 0_u32;
    for status in events.session_statuses.values() {
        match status {
            SessionLifecycleStatus::Running => running += 1,
            SessionLifecycleStatus::Error => failed += 1,
            SessionLifecycleStatus::Idle | SessionLifecycleStatus::Waiting => {}
        }
    }
    json!({ "running": running, "failed": failed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn events(statuses: &[(&str, SessionLifecycleStatus)]) -> RecentSessionEvents {
        RecentSessionEvents {
            session_statuses: Arc::new(HashMap::from_iter(
                statuses
                    .iter()
                    .map(|(id, status)| ((*id).to_string(), *status)),
            )),
            ..RecentSessionEvents::default()
        }
    }

    #[test]
    fn counts_running_and_failed_sessions_without_identifiers() {
        let counts = counts_from_events(&events(&[
            ("session-secret", SessionLifecycleStatus::Running),
            ("failed-secret", SessionLifecycleStatus::Error),
            ("idle", SessionLifecycleStatus::Idle),
            ("waiting", SessionLifecycleStatus::Waiting),
        ]));
        assert_eq!(counts["running"], 1);
        assert_eq!(counts["failed"], 1);
        let encoded = counts.to_string();
        assert!(!encoded.contains("session-secret"));
        assert!(!encoded.contains("failed-secret"));
    }
}
