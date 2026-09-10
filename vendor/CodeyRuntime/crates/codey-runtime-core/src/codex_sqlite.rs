use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension};

pub fn default_codex_home_dir() -> PathBuf {
    crate::codex_home::default_codex_home_dir()
}

pub fn codex_session_db_paths_from_home(home: &Path) -> Vec<PathBuf> {
    CodexSessionDbDiscoveryCache::default().session_db_paths_from_home(home)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionDbFileSignature {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl SessionDbFileSignature {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionDbCandidateSignature {
    database: SessionDbFileSignature,
    wal: Option<SessionDbFileSignature>,
}

impl SessionDbCandidateSignature {
    fn read(path: &Path) -> Option<Self> {
        let database = SessionDbFileSignature::from_metadata(&fs::metadata(path).ok()?);
        let mut wal_path = path.as_os_str().to_os_string();
        wal_path.push("-wal");
        let wal_path = PathBuf::from(wal_path);
        let wal = fs::metadata(wal_path)
            .ok()
            .map(|metadata| SessionDbFileSignature::from_metadata(&metadata));
        Some(Self { database, wal })
    }
}

#[derive(Debug)]
struct CachedSessionDbProbe {
    signature: SessionDbCandidateSignature,
    has_session_table: bool,
}

/// Stateful discovery for callers that scan session databases repeatedly.
///
/// Directory membership is checked on every scan, while an unchanged candidate
/// reuses its previous schema classification. The database and WAL signatures
/// invalidate that classification after replacement or a later schema change.
#[derive(Debug, Default)]
pub struct CodexSessionDbDiscoveryCache {
    candidates: HashMap<PathBuf, CachedSessionDbProbe>,
    #[cfg(test)]
    probe_count: usize,
}

impl CodexSessionDbDiscoveryCache {
    pub fn session_db_paths_from_home(&mut self, home: &Path) -> Vec<PathBuf> {
        let candidates = codex_sqlite_dir_candidates(home);
        let active_candidates = candidates.iter().cloned().collect::<HashSet<_>>();
        self.candidates
            .retain(|path, _| active_candidates.contains(path));

        let mut paths = Vec::new();
        for path in candidates {
            let Some(signature) = SessionDbCandidateSignature::read(&path) else {
                self.candidates.remove(&path);
                continue;
            };
            let has_session_table = if let Some(cached) = self
                .candidates
                .get(&path)
                .filter(|cached| cached.signature == signature)
            {
                cached.has_session_table
            } else {
                let Ok(has_session_table) = has_session_table(&path) else {
                    // A transient read failure is not a stable schema classification.
                    self.candidates.remove(&path);
                    continue;
                };
                #[cfg(test)]
                {
                    self.probe_count += 1;
                }
                self.candidates.insert(
                    path.clone(),
                    CachedSessionDbProbe {
                        signature,
                        has_session_table,
                    },
                );
                has_session_table
            };
            if has_session_table {
                paths.push(path);
            }
        }

        let legacy = legacy_state_db_path(home);
        if !paths.iter().any(|path| path == &legacy) {
            paths.push(legacy);
        }
        paths
    }
}

pub fn codex_sqlite_sidecar_paths(db_path: &Path) -> [PathBuf; 3] {
    [
        db_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", db_path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", db_path.to_string_lossy())),
    ]
}

fn legacy_state_db_path(home: &Path) -> PathBuf {
    home.join("state_5.sqlite")
}

fn codex_sqlite_dir_candidates(home: &Path) -> Vec<PathBuf> {
    let sqlite_dir = home.join("sqlite");
    let Ok(entries) = fs::read_dir(sqlite_dir) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_file())
                .map(|_| entry.path())
        })
        .filter(|path| is_sqlite_candidate(path))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        (
            path.file_name()
                .map(|name| name != OsStr::new("codex-dev.db"))
                .unwrap_or(true),
            path.file_name().map(|name| name.to_os_string()),
        )
    });
    candidates
}

fn is_sqlite_candidate(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("db") | Some("sqlite") | Some("sqlite3")
    )
}

fn has_session_table(path: &Path) -> rusqlite::Result<bool> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(db
        .query_row(
            concat!(
                "SELECT 1 FROM sqlite_master ",
                "WHERE type = 'table' AND name IN (",
                "'threads', 'automation_runs', 'inbox_items', 'local_thread_catalog'",
                ") LIMIT 1"
            ),
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{CodexSessionDbDiscoveryCache, codex_session_db_paths_from_home};

    #[test]
    fn discovery_does_not_cache_failed_schema_reads() {
        let home = tempdir().unwrap();
        let sqlite = home.path().join("sqlite");
        std::fs::create_dir_all(&sqlite).unwrap();
        let path = sqlite.join("invalid.db");
        std::fs::write(&path, "not a SQLite database").unwrap();
        let mut cache = CodexSessionDbDiscoveryCache::default();
        cache.session_db_paths_from_home(home.path());
        assert!(!cache.candidates.contains_key(&path));
    }

    #[test]
    fn discovers_catalog_only_session_databases() {
        let home = tempdir().unwrap();
        let sqlite = home.path().join("sqlite");
        std::fs::create_dir_all(&sqlite).unwrap();
        let catalog = sqlite.join("catalog.db");
        Connection::open(&catalog)
            .unwrap()
            .execute(
                "CREATE TABLE local_thread_catalog (thread_id TEXT NOT NULL)",
                [],
            )
            .unwrap();

        assert!(
            codex_session_db_paths_from_home(home.path())
                .iter()
                .any(|path| path == &catalog)
        );
    }

    #[test]
    fn discovery_cache_skips_unchanged_probes_and_detects_wal_schema_changes() {
        let home = tempdir().unwrap();
        let sqlite = home.path().join("sqlite");
        std::fs::create_dir_all(&sqlite).unwrap();
        let candidate = sqlite.join("candidate.db");
        let database = Connection::open(&candidate).unwrap();
        let journal_mode = database
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        database
            .execute("CREATE TABLE unrelated (id INTEGER)", [])
            .unwrap();

        let legacy = home.path().join("state_5.sqlite");
        let mut cache = CodexSessionDbDiscoveryCache::default();

        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![legacy.clone()]
        );
        assert_eq!(cache.probe_count, 1);

        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![legacy.clone()]
        );
        assert_eq!(
            cache.probe_count, 1,
            "an unchanged candidate must not query sqlite_master again"
        );

        database
            .execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
            .unwrap();

        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![candidate.clone(), legacy.clone()]
        );
        assert_eq!(
            cache.probe_count, 2,
            "a schema change recorded only in the WAL must invalidate discovery"
        );

        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![candidate, legacy]
        );
        assert_eq!(cache.probe_count, 2);
    }

    #[test]
    fn discovery_cache_tracks_add_delete_replace_and_keeps_legacy_state() {
        let home = tempdir().unwrap();
        let sqlite = home.path().join("sqlite");
        std::fs::create_dir_all(&sqlite).unwrap();
        let legacy = home.path().join("state_5.sqlite");
        Connection::open(&legacy)
            .unwrap()
            .execute("CREATE TABLE unrelated (id INTEGER)", [])
            .unwrap();

        let primary = sqlite.join("primary.db");
        Connection::open(&primary)
            .unwrap()
            .execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
            .unwrap();
        let mut cache = CodexSessionDbDiscoveryCache::default();

        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![primary.clone(), legacy.clone()]
        );
        assert_eq!(cache.probe_count, 1);

        let added = sqlite.join("added.sqlite");
        Connection::open(&added)
            .unwrap()
            .execute("CREATE TABLE automation_runs (id TEXT PRIMARY KEY)", [])
            .unwrap();
        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![added.clone(), primary.clone(), legacy.clone()]
        );
        assert_eq!(cache.probe_count, 2);

        let replacement = home.path().join("replacement.sqlite");
        Connection::open(&replacement)
            .unwrap()
            .execute("CREATE TABLE unrelated (id INTEGER)", [])
            .unwrap();
        std::fs::remove_file(&primary).unwrap();
        std::fs::rename(&replacement, &primary).unwrap();

        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![added.clone(), legacy.clone()]
        );
        assert_eq!(
            cache.probe_count, 3,
            "a replacement at the same path must be probed"
        );

        std::fs::remove_file(&added).unwrap();
        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![legacy.clone()]
        );
        assert_eq!(cache.probe_count, 3);
        assert!(!cache.candidates.contains_key(&added));
        assert_eq!(
            cache.session_db_paths_from_home(home.path()),
            vec![legacy],
            "legacy state_5.sqlite remains a candidate regardless of schema"
        );
    }
}
