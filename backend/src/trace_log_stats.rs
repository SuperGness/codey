use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::trace_log_guard;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLogStatsSnapshot {
    pub captured_at: u64,
    pub database_bytes: u64,
    pub errors: Vec<String>,
}

impl TraceLogStatsSnapshot {
    pub fn idle() -> Self {
        Self::default()
    }
}

pub fn snapshot(home: &Path) -> TraceLogStatsSnapshot {
    let mut snapshot = TraceLogStatsSnapshot {
        captured_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        ..TraceLogStatsSnapshot::default()
    };
    match trace_log_guard::log_database_paths(home) {
        Ok(paths) => {
            for path in paths {
                snapshot.database_bytes = snapshot
                    .database_bytes
                    .saturating_add(trace_log_guard::database_family_bytes(&path));
            }
        }
        Err(error) => snapshot.errors.push(error.to_string()),
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn measures_only_log_database_families_without_reading_sqlite_contents() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("sqlite")).unwrap();
        for (name, bytes) in [
            ("logs_2.sqlite", 10),
            ("logs_2.sqlite-wal", 20),
            ("logs_2.sqlite-shm", 30),
            ("logs_2.sqlite-journal", 40),
            ("sqlite/logs_1.sqlite", 50),
            ("state.sqlite", 100),
        ] {
            fs::write(temp.path().join(name), vec![0; bytes]).unwrap();
        }
        let result = snapshot(temp.path());
        assert_eq!(result.database_bytes, 150);
        assert!(result.captured_at > 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn missing_directory_is_empty_and_unreadable_directory_reports_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = snapshot(&temp.path().join("missing"));
        assert_eq!(missing.database_bytes, 0);
        assert!(missing.errors.is_empty());
        fs::write(temp.path().join("file"), b"not a directory").unwrap();
        assert!(!snapshot(&temp.path().join("file")).errors.is_empty());
    }
}
