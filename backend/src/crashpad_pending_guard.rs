use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Serialize, Serializer};
use uuid::Uuid;

pub const PENDING_HARD_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
pub const PENDING_TARGET_BYTES: u64 = 384 * 1024 * 1024;
pub const GUARD_INTERVAL: Duration = Duration::from_secs(5 * 60);

const COMPLETE_REPORT_QUIET_PERIOD: Duration = Duration::from_secs(10 * 60);
const ORPHAN_FILE_QUIET_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashpadPendingStatsSnapshot {
    pub pending: bool,
    pub captured_at: u64,
    pub directories_found: usize,
    pub reports_found: usize,
    pub complete_reports: usize,
    pub files_found: usize,
    pub managed_files: usize,
    pub orphan_files: usize,
    pub unmanaged_files: usize,
    pub pending_bytes: u64,
    pub managed_bytes: u64,
    pub oldest_timestamp: Option<u64>,
    pub newest_timestamp: Option<u64>,
    pub hard_limit_bytes: u64,
    pub target_bytes: u64,
    pub over_limit: bool,
    pub protection_enabled: bool,
    pub errors: Vec<String>,
}

impl CrashpadPendingStatsSnapshot {
    pub fn idle(protection_enabled: bool) -> Self {
        Self {
            hard_limit_bytes: PENDING_HARD_LIMIT_BYTES,
            target_bytes: PENDING_TARGET_BYTES,
            protection_enabled,
            ..Self::default()
        }
    }

    pub fn pending(protection_enabled: bool) -> Self {
        Self {
            pending: true,
            captured_at: timestamp_seconds(SystemTime::now()),
            ..Self::idle(protection_enabled)
        }
    }
}

#[derive(Debug, Clone)]
pub struct CrashpadPendingStatsHandle {
    snapshot: Arc<RwLock<CrashpadPendingStatsSnapshot>>,
}

impl CrashpadPendingStatsHandle {
    pub fn idle(protection_enabled: bool) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(CrashpadPendingStatsSnapshot::idle(
                protection_enabled,
            ))),
        }
    }

    pub fn begin_refresh(&self, protection_enabled: bool) -> bool {
        let mut current = match self.snapshot.write() {
            Ok(current) => current,
            Err(poisoned) => poisoned.into_inner(),
        };
        if current.pending {
            return false;
        }
        *current = CrashpadPendingStatsSnapshot::pending(protection_enabled);
        true
    }

    pub fn replace(&self, snapshot: CrashpadPendingStatsSnapshot) {
        match self.snapshot.write() {
            Ok(mut current) => *current = snapshot,
            Err(poisoned) => *poisoned.into_inner() = snapshot,
        }
    }

    pub fn replace_if_idle(&self, snapshot: CrashpadPendingStatsSnapshot) -> bool {
        let mut current = match self.snapshot.write() {
            Ok(current) => current,
            Err(poisoned) => poisoned.into_inner(),
        };
        if current.pending {
            return false;
        }
        *current = snapshot;
        true
    }
}

impl Default for CrashpadPendingStatsHandle {
    fn default() -> Self {
        Self::idle(true)
    }
}

impl Serialize for CrashpadPendingStatsHandle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.snapshot.read() {
            Ok(snapshot) => snapshot.serialize(serializer),
            Err(poisoned) => poisoned.into_inner().serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashpadCleanupReport {
    pub directories_found: usize,
    pub reports_found: usize,
    pub reports_deleted: usize,
    pub files_found: usize,
    pub files_deleted: usize,
    pub orphan_files_deleted: usize,
    pub unmanaged_files: usize,
    pub skipped_recent_reports: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub limit_applied: bool,
    pub still_over_limit: bool,
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub struct CrashpadGuardRun {
    pub cleanup: CrashpadCleanupReport,
    pub snapshot: CrashpadPendingStatsSnapshot,
}

#[derive(Clone, Copy)]
struct CleanupPolicy {
    hard_limit_bytes: u64,
    target_bytes: u64,
    complete_report_quiet_period: Duration,
    orphan_file_quiet_period: Duration,
}

impl Default for CleanupPolicy {
    fn default() -> Self {
        Self {
            hard_limit_bytes: PENDING_HARD_LIMIT_BYTES,
            target_bytes: PENDING_TARGET_BYTES,
            complete_report_quiet_period: COMPLETE_REPORT_QUIET_PERIOD,
            orphan_file_quiet_period: ORPHAN_FILE_QUIET_PERIOD,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingFileKind {
    Dump,
    Sidecar,
}

#[derive(Clone, Debug)]
struct FileIdentity {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug)]
struct PendingFile {
    path: PathBuf,
    name: String,
    kind: PendingFileKind,
    identity: FileIdentity,
}

impl PendingFile {
    fn length(&self) -> u64 {
        self.identity.length
    }

    fn modified(&self) -> Option<SystemTime> {
        self.identity.modified
    }
}

#[derive(Debug)]
struct ReportGroup {
    files: Vec<PendingFile>,
    complete: bool,
    modified: Option<SystemTime>,
}

#[derive(Default)]
struct ReportGroupBuilder {
    dump: Option<PendingFile>,
    sidecar: Option<PendingFile>,
}

impl ReportGroupBuilder {
    fn insert(&mut self, file: PendingFile) {
        match file.kind {
            PendingFileKind::Dump => self.dump = Some(file),
            PendingFileKind::Sidecar => self.sidecar = Some(file),
        }
    }

    fn finish(self) -> ReportGroup {
        let complete = self.dump.is_some() && self.sidecar.is_some();
        let mut files = Vec::with_capacity(2);
        if let Some(dump) = self.dump {
            files.push(dump);
        }
        if let Some(sidecar) = self.sidecar {
            files.push(sidecar);
        }
        let modified = files.iter().filter_map(PendingFile::modified).max();
        ReportGroup {
            files,
            complete,
            modified,
        }
    }
}

#[derive(Default)]
struct PendingInventory {
    directories_found: usize,
    reports: Vec<ReportGroup>,
    files_found: usize,
    managed_files: usize,
    unmanaged_files: usize,
    pending_bytes: u64,
    managed_bytes: u64,
    oldest: Option<SystemTime>,
    newest: Option<SystemTime>,
    errors: Vec<String>,
}

impl PendingInventory {
    fn complete_reports(&self) -> usize {
        self.reports.iter().filter(|report| report.complete).count()
    }

    fn orphan_files(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| !report.complete)
            .map(|report| report.files.len())
            .sum()
    }

    fn into_snapshot(self, protection_enabled: bool) -> CrashpadPendingStatsSnapshot {
        let over_limit = self.pending_bytes > PENDING_HARD_LIMIT_BYTES;
        CrashpadPendingStatsSnapshot {
            pending: false,
            captured_at: timestamp_seconds(SystemTime::now()),
            directories_found: self.directories_found,
            reports_found: self.reports.len(),
            complete_reports: self.complete_reports(),
            files_found: self.files_found,
            managed_files: self.managed_files,
            orphan_files: self.orphan_files(),
            unmanaged_files: self.unmanaged_files,
            pending_bytes: self.pending_bytes,
            managed_bytes: self.managed_bytes,
            oldest_timestamp: self.oldest.map(timestamp_seconds),
            newest_timestamp: self.newest.map(timestamp_seconds),
            hard_limit_bytes: PENDING_HARD_LIMIT_BYTES,
            target_bytes: PENDING_TARGET_BYTES,
            over_limit,
            protection_enabled,
            errors: self.errors,
        }
    }
}

pub fn snapshot_system(protection_enabled: bool) -> CrashpadPendingStatsSnapshot {
    inventory(&system_pending_directories()).into_snapshot(protection_enabled)
}

pub fn enforce_system_limit() -> CrashpadGuardRun {
    cleanup(
        &system_pending_directories(),
        CleanupMode::EnforceLimit,
        CleanupPolicy::default(),
        true,
    )
}

pub fn clear_system(protection_enabled: bool) -> CrashpadGuardRun {
    cleanup(
        &system_pending_directories(),
        CleanupMode::ClearStableReports,
        CleanupPolicy::default(),
        protection_enabled,
    )
}

#[derive(Clone, Copy)]
enum CleanupMode {
    EnforceLimit,
    ClearStableReports,
}

fn cleanup(
    pending_directories: &[PathBuf],
    mode: CleanupMode,
    policy: CleanupPolicy,
    protection_enabled: bool,
) -> CrashpadGuardRun {
    let now = SystemTime::now();
    let mut pending_inventory = inventory(pending_directories);
    let mut report = CrashpadCleanupReport {
        directories_found: pending_inventory.directories_found,
        reports_found: pending_inventory.reports.len(),
        files_found: pending_inventory.files_found,
        unmanaged_files: pending_inventory.unmanaged_files,
        bytes_before: pending_inventory.pending_bytes,
        limit_applied: matches!(mode, CleanupMode::EnforceLimit)
            && pending_inventory.pending_bytes > policy.hard_limit_bytes,
        errors: std::mem::take(&mut pending_inventory.errors),
        ..CrashpadCleanupReport::default()
    };

    let should_delete = match mode {
        CleanupMode::EnforceLimit => report.limit_applied,
        CleanupMode::ClearStableReports => true,
    };
    if should_delete {
        pending_inventory
            .reports
            .sort_by_key(|group| group.modified.unwrap_or(UNIX_EPOCH));
        let mut estimated_remaining = report.bytes_before;
        for group in &pending_inventory.reports {
            if matches!(mode, CleanupMode::EnforceLimit)
                && estimated_remaining <= policy.target_bytes
            {
                break;
            }
            let quiet_period = if group.complete {
                policy.complete_report_quiet_period
            } else {
                policy.orphan_file_quiet_period
            };
            if !old_enough(group.modified, now, quiet_period) {
                if group.complete {
                    report.skipped_recent_reports += 1;
                }
                continue;
            }
            if !group.complete && matches!(mode, CleanupMode::EnforceLimit) {
                continue;
            }
            match delete_group(group) {
                Ok((files_deleted, bytes_deleted)) => {
                    report.files_deleted += files_deleted;
                    report.reports_deleted += usize::from(group.complete && files_deleted > 0);
                    report.orphan_files_deleted +=
                        usize::from(!group.complete).saturating_mul(files_deleted);
                    estimated_remaining = estimated_remaining.saturating_sub(bytes_deleted);
                }
                Err(error) => report.errors.push(error),
            }
        }
    }

    let snapshot = inventory(pending_directories).into_snapshot(protection_enabled);
    report.bytes_after = snapshot.pending_bytes;
    report.bytes_reclaimed = report.bytes_before.saturating_sub(report.bytes_after);
    report.still_over_limit = snapshot.pending_bytes > policy.hard_limit_bytes;
    report.errors.extend(snapshot.errors.iter().cloned());
    CrashpadGuardRun {
        cleanup: report,
        snapshot,
    }
}

fn delete_group(group: &ReportGroup) -> Result<(usize, u64), String> {
    if group.files.iter().any(|file| !identity_matches(file)) {
        return Err("Crashpad 待处理报告在清理前发生变化，已跳过".to_string());
    }

    let mut files_deleted = 0usize;
    let mut bytes_deleted = 0u64;
    for file in &group.files {
        if !identity_matches(file) {
            return Err(format!(
                "Crashpad 文件 {} 在清理过程中发生变化，已停止处理该报告",
                file.name
            ));
        }
        match fs::remove_file(&file.path) {
            Ok(()) => {
                files_deleted += 1;
                bytes_deleted = bytes_deleted.saturating_add(file.length());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("删除 Crashpad 文件 {} 失败：{error}", file.name));
            }
        }
    }
    Ok((files_deleted, bytes_deleted))
}

fn identity_matches(file: &PendingFile) -> bool {
    let Ok(metadata) = fs::symlink_metadata(&file.path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != file.identity.device || metadata.ino() != file.identity.inode {
            return false;
        }
    }
    metadata.file_type().is_file()
        && metadata.len() == file.identity.length
        && metadata.modified().ok() == file.identity.modified
}

fn inventory(pending_directories: &[PathBuf]) -> PendingInventory {
    let mut inventory = PendingInventory::default();
    for directory in pending_directories {
        collect_pending_directory(directory, &mut inventory);
    }
    inventory
}

fn collect_pending_directory(directory: &Path, inventory: &mut PendingInventory) {
    let directory_metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            inventory.errors.push(format!(
                "读取 Crashpad pending 目录状态失败：{}：{error}",
                directory.display()
            ));
            return;
        }
    };
    if !directory_metadata.file_type().is_dir() {
        inventory.errors.push(format!(
            "Crashpad pending 路径不是可信的普通目录，已跳过：{}",
            directory.display()
        ));
        return;
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            inventory.errors.push(format!(
                "读取 Crashpad pending 目录失败：{}：{error}",
                directory.display()
            ));
            return;
        }
    };
    inventory.directories_found += 1;
    let mut groups = HashMap::<Uuid, ReportGroupBuilder>::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                inventory
                    .errors
                    .push(format!("读取 Crashpad pending 目录项失败：{error}"));
                continue;
            }
        };
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                inventory.errors.push(format!(
                    "读取 Crashpad pending 文件状态失败：{}：{error}",
                    entry.file_name().to_string_lossy()
                ));
                continue;
            }
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let length = metadata.len();
        let modified = metadata.modified().ok();
        inventory.files_found += 1;
        inventory.pending_bytes = inventory.pending_bytes.saturating_add(length);
        inventory.oldest = min_time(inventory.oldest, modified);
        inventory.newest = max_time(inventory.newest, modified);
        let Some((report_id, kind)) = parse_pending_file_name(&entry.file_name()) else {
            inventory.unmanaged_files += 1;
            continue;
        };
        inventory.managed_files += 1;
        inventory.managed_bytes = inventory.managed_bytes.saturating_add(length);
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        let file = PendingFile {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().to_string(),
            kind,
            identity: FileIdentity {
                length,
                modified,
                #[cfg(unix)]
                device: metadata.dev(),
                #[cfg(unix)]
                inode: metadata.ino(),
            },
        };
        groups.entry(report_id).or_default().insert(file);
    }
    inventory
        .reports
        .extend(groups.into_values().map(ReportGroupBuilder::finish));
}

fn parse_pending_file_name(name: &OsStr) -> Option<(Uuid, PendingFileKind)> {
    let name = name.to_str()?;
    if let Some(report_id) = name.strip_suffix(".dmp") {
        return Uuid::parse_str(report_id)
            .ok()
            .map(|report_id| (report_id, PendingFileKind::Dump));
    }
    let report_id = name.strip_suffix("_sidecar.json")?;
    Uuid::parse_str(report_id)
        .ok()
        .map(|report_id| (report_id, PendingFileKind::Sidecar))
}

fn old_enough(modified: Option<SystemTime>, now: SystemTime, quiet_period: Duration) -> bool {
    modified
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= quiet_period)
}

fn min_time(current: Option<SystemTime>, candidate: Option<SystemTime>) -> Option<SystemTime> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}

fn max_time(current: Option<SystemTime>, candidate: Option<SystemTime>) -> Option<SystemTime> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}

fn timestamp_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn system_pending_directories() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let Some(base_dirs) = directories::BaseDirs::new() else {
            return Vec::new();
        };
        let application_support = base_dirs.data_dir();
        vec![
            application_support.join("Codex/Crashpad/pending"),
            application_support.join("com.openai.codex/web/Crashpad/pending"),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_paths(directory: &Path, id: &str) -> (PathBuf, PathBuf) {
        (
            directory.join(format!("{id}.dmp")),
            directory.join(format!("{id}_sidecar.json")),
        )
    }

    fn write_report(directory: &Path, id: &str, dump_bytes: usize) {
        let (dump, sidecar) = report_paths(directory, id);
        fs::write(dump, vec![7u8; dump_bytes]).unwrap();
        fs::write(sidecar, b"{}").unwrap();
    }

    fn test_policy(hard_limit_bytes: u64, target_bytes: u64) -> CleanupPolicy {
        CleanupPolicy {
            hard_limit_bytes,
            target_bytes,
            complete_report_quiet_period: Duration::ZERO,
            orphan_file_quiet_period: Duration::ZERO,
        }
    }

    #[test]
    fn missing_pending_directory_is_empty_and_not_created() {
        let temp = tempfile::tempdir().unwrap();
        let pending = temp.path().join("missing/pending");

        let snapshot = inventory(std::slice::from_ref(&pending)).into_snapshot(true);

        assert_eq!(snapshot.directories_found, 0);
        assert_eq!(snapshot.files_found, 0);
        assert!(!pending.exists());
    }

    #[test]
    fn snapshot_counts_only_uuid_report_files_as_managed() {
        let temp = tempfile::tempdir().unwrap();
        let pending = temp.path().join("pending");
        fs::create_dir(&pending).unwrap();
        write_report(&pending, "01234567-89ab-cdef-0123-456789abcdef", 64);
        fs::write(pending.join("notes.txt"), b"leave me").unwrap();
        fs::create_dir(pending.join("nested")).unwrap();

        let snapshot = inventory(std::slice::from_ref(&pending)).into_snapshot(true);

        assert_eq!(snapshot.directories_found, 1);
        assert_eq!(snapshot.reports_found, 1);
        assert_eq!(snapshot.complete_reports, 1);
        assert_eq!(snapshot.files_found, 3);
        assert_eq!(snapshot.managed_files, 2);
        assert_eq!(snapshot.unmanaged_files, 1);
        assert_eq!(snapshot.managed_bytes, 66);
    }

    #[test]
    fn limit_cleanup_removes_oldest_complete_reports_to_low_watermark() {
        let temp = tempfile::tempdir().unwrap();
        let pending = temp.path().join("pending");
        fs::create_dir(&pending).unwrap();
        write_report(&pending, "00000000-0000-0000-0000-000000000001", 80);
        std::thread::sleep(Duration::from_millis(5));
        write_report(&pending, "00000000-0000-0000-0000-000000000002", 80);

        let run = cleanup(
            std::slice::from_ref(&pending),
            CleanupMode::EnforceLimit,
            test_policy(100, 85),
            true,
        );

        assert!(run.cleanup.limit_applied);
        assert_eq!(run.cleanup.reports_deleted, 1);
        assert_eq!(run.cleanup.files_deleted, 2);
        assert_eq!(run.snapshot.complete_reports, 1);
        assert!(run.snapshot.pending_bytes <= 85);
    }

    #[test]
    fn manual_cleanup_preserves_unknown_files_and_deletes_stable_orphans() {
        let temp = tempfile::tempdir().unwrap();
        let pending = temp.path().join("pending");
        fs::create_dir(&pending).unwrap();
        write_report(&pending, "00000000-0000-0000-0000-000000000001", 32);
        fs::write(
            pending.join("00000000-0000-0000-0000-000000000002.dmp"),
            b"orphan",
        )
        .unwrap();
        fs::write(pending.join("keep.txt"), b"unknown").unwrap();

        let run = cleanup(
            std::slice::from_ref(&pending),
            CleanupMode::ClearStableReports,
            test_policy(u64::MAX, u64::MAX),
            true,
        );

        assert_eq!(run.cleanup.reports_deleted, 1);
        assert_eq!(run.cleanup.orphan_files_deleted, 1);
        assert_eq!(run.cleanup.files_deleted, 3);
        assert!(pending.join("keep.txt").is_file());
        assert_eq!(run.snapshot.unmanaged_files, 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_counted_or_deleted() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let pending = temp.path().join("pending");
        fs::create_dir(&pending).unwrap();
        let outside = temp.path().join("outside.dmp");
        fs::write(&outside, b"outside").unwrap();
        symlink(
            &outside,
            pending.join("00000000-0000-0000-0000-000000000001.dmp"),
        )
        .unwrap();

        let run = cleanup(
            std::slice::from_ref(&pending),
            CleanupMode::ClearStableReports,
            test_policy(u64::MAX, u64::MAX),
            true,
        );

        assert_eq!(run.cleanup.files_deleted, 0);
        assert!(outside.is_file());
    }
}
