//! Reads the Electron fuse wire embedded in the Codex desktop binary.
//!
//! Electron bakes a "fuse wire" into its binary: a sentinel string followed by
//! a version byte, a length byte and one byte per fuse (`0` disabled, `1`
//! enabled, `r` removed). `EnableNodeCliInspectArguments` and
//! `EnableNodeOptionsEnvironmentVariable` are independent: a build can drop
//! `--inspect-brk` while still honouring `NODE_OPTIONS=--require`. Reading the
//! wire before launch lets the launcher prefer `--require` when NODE_OPTIONS is
//! on, then Inspector, then the CLI wrapper, instead of waiting for a debug port
//! that will never answer.

#[cfg(any(windows, target_os = "macos", test))]
use std::io::Read;
#[cfg(any(windows, target_os = "macos", test))]
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::time::Instant;

#[cfg(any(windows, target_os = "macos", test))]
use anyhow::Context;
use anyhow::Result;
#[cfg(any(windows, target_os = "macos", test))]
use serde::{Deserialize, Serialize};

#[cfg(windows)]
pub(crate) mod windows_repair;

/// Sentinel that precedes the fuse wire in every Electron binary (@electron/fuses).
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const FUSE_SENTINEL: &[u8] = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX";
#[cfg(any(windows, target_os = "macos", test))]
const FUSE_WIRE_VERSION_V1: u8 = 1;
/// Index of `EnableNodeOptionsEnvironmentVariable` in the v1 fuse wire (`FuseV1Options`).
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const NODE_OPTIONS_FUSE_INDEX: usize = 2;
/// Index of `EnableNodeCliInspectArguments` in the v1 fuse wire (`FuseV1Options`).
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const NODE_CLI_INSPECT_FUSE_INDEX: usize = 3;
#[cfg(any(windows, target_os = "macos", test))]
const MAX_FUSE_COUNT: usize = 64;
#[cfg(any(windows, target_os = "macos", test))]
const SCAN_CHUNK_BYTES: usize = 8 * 1024 * 1024;
#[cfg(any(windows, target_os = "macos", test))]
const CACHE_FILE: &str = "electron-fuses.json";
#[cfg(any(windows, target_os = "macos", test))]
const MAX_CACHE_BYTES: u64 = 64 * 1024;

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FuseState {
    Enabled,
    Disabled,
    Removed,
    Unknown,
}

#[cfg(any(windows, target_os = "macos", test))]
impl FuseState {
    #[cfg(any(windows, target_os = "macos"))]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            FuseState::Enabled => "enabled",
            FuseState::Disabled => "disabled",
            FuseState::Removed => "removed",
            FuseState::Unknown => "unknown",
        }
    }

    /// Whether `--inspect-brk` can still open a main-process Inspector. An
    /// unknown wire keeps the Inspector attempt so a scan failure never removes
    /// a working path; the runtime probes then decide.
    pub(crate) fn inspector_possible(self) -> bool {
        !matches!(self, FuseState::Disabled | FuseState::Removed)
    }

    /// Whether Electron still honours `NODE_OPTIONS` in the main process. Unknown
    /// keeps the `--require` attempt for the same reason as Inspector.
    pub(crate) fn node_options_possible(self) -> bool {
        !matches!(self, FuseState::Disabled | FuseState::Removed)
    }
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ElectronFuses {
    pub(crate) node_cli_inspect: FuseState,
    pub(crate) node_options: FuseState,
}

#[cfg(any(windows, target_os = "macos", test))]
impl ElectronFuses {
    #[cfg(any(windows, target_os = "macos"))]
    fn unknown() -> Self {
        Self {
            node_cli_inspect: FuseState::Unknown,
            node_options: FuseState::Unknown,
        }
    }

    fn from_wire(wire: Option<&FuseWire>) -> Self {
        Self {
            node_cli_inspect: wire
                .map(|wire| wire.state(NODE_CLI_INSPECT_FUSE_INDEX))
                .unwrap_or(FuseState::Unknown),
            node_options: wire
                .map(|wire| wire.state(NODE_OPTIONS_FUSE_INDEX))
                .unwrap_or(FuseState::Unknown),
        }
    }
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FuseWire {
    pub(crate) version: u8,
    pub(crate) states: String,
}

#[cfg(any(windows, target_os = "macos", test))]
impl FuseWire {
    pub(crate) fn state(&self, index: usize) -> FuseState {
        if self.version != FUSE_WIRE_VERSION_V1 {
            return FuseState::Unknown;
        }
        match self.states.as_bytes().get(index) {
            Some(b'1') => FuseState::Enabled,
            Some(b'0') => FuseState::Disabled,
            Some(b'r') => FuseState::Removed,
            _ => FuseState::Unknown,
        }
    }
}

/// Parses the wire that follows a sentinel found at `sentinel_offset`.
/// Returns `None` when the buffer does not yet hold the complete wire.
#[cfg(any(windows, target_os = "macos", test))]
fn parse_fuse_wire(bytes: &[u8], sentinel_offset: usize) -> Option<Result<FuseWire>> {
    let header = sentinel_offset + FUSE_SENTINEL.len();
    let version = *bytes.get(header)?;
    let count = usize::from(*bytes.get(header + 1)?);
    if count == 0 || count > MAX_FUSE_COUNT {
        return Some(Err(anyhow::anyhow!("Electron fuse wire 长度无效：{count}")));
    }
    let states = bytes.get(header + 2..header + 2 + count)?;
    if !states.iter().all(|byte| matches!(byte, b'0' | b'1' | b'r')) {
        return Some(Err(anyhow::anyhow!("Electron fuse wire 包含未知状态字节")));
    }
    Some(Ok(FuseWire {
        version,
        states: String::from_utf8_lossy(states).into_owned(),
    }))
}

/// Streams through `path` looking for the fuse sentinel. `Ok(None)` means the
/// file holds no sentinel, so it is not an Electron binary with fuses.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn read_fuse_wire(path: &Path) -> Result<Option<FuseWire>> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("读取 Electron 二进制失败：{}", path.display()))?;
    Ok(scan_fuse_wire(&mut file, false)?.map(|(_, wire)| wire))
}

#[cfg(any(windows, target_os = "macos", test))]
fn scan_fuse_wire(
    file: &mut std::fs::File,
    require_unique: bool,
) -> Result<Option<(u64, FuseWire)>> {
    let finder = memchr::memmem::Finder::new(FUSE_SENTINEL);
    // Enough trailing context that a sentinel at the end of one chunk is still
    // matched, and its wire completed, by the next read.
    let overlap = FUSE_SENTINEL.len() + 2 + MAX_FUSE_COUNT;
    let mut buffer: Vec<u8> = Vec::with_capacity(SCAN_CHUNK_BYTES + overlap);
    let mut chunk = vec![0_u8; SCAN_CHUNK_BYTES];
    let mut base = 0_u64;
    let mut found: Option<(u64, FuseWire)> = None;
    loop {
        let read = file.read(&mut chunk).context("读取 Electron 二进制失败")?;
        let end_of_file = read == 0;
        buffer.extend_from_slice(&chunk[..read]);
        for offset in finder.find_iter(&buffer) {
            let absolute = base + offset as u64;
            if found.as_ref().is_some_and(|(at, _)| *at == absolute) {
                continue;
            }
            match parse_fuse_wire(&buffer, offset) {
                Some(wire) => {
                    let wire = wire?;
                    // Preserve read-only detection of universal macOS binaries;
                    // only the Windows mutation path requires a unique wire.
                    if !require_unique {
                        return Ok(Some((absolute, wire)));
                    }
                    anyhow::ensure!(
                        found.is_none(),
                        "Electron 二进制包含多个 fuse wire，无法安全识别运行时"
                    );
                    found = Some((absolute, wire));
                }
                None if end_of_file => {
                    anyhow::bail!("Electron fuse wire 在文件末尾被截断");
                }
                None => break,
            }
        }
        if end_of_file {
            return Ok(found);
        }
        if buffer.len() > overlap {
            let keep_from = buffer.len() - overlap;
            buffer.drain(..keep_from);
            base += keep_from as u64;
        }
    }
}

/// Candidate runtime files in priority order: a split `chrome.dll` first, then
/// the main executable; a macOS bundle resolves to its Electron framework.
/// Split runtimes may keep the fuse wire in either file, so callers that need
/// the wire must use [`electron_runtime_with_wire`] instead of the first entry.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn electron_binary_candidates(app_dir: &Path) -> Vec<PathBuf> {
    if app_dir
        .extension()
        .is_some_and(|extension| extension == "app")
    {
        let Some(frameworks) = std::fs::read_dir(app_dir.join("Contents").join("Frameworks")).ok()
        else {
            return Vec::new();
        };
        let mut frameworks = frameworks
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(" Framework.framework"))
            })
            .collect::<Vec<_>>();
        frameworks.sort();
        let mut candidates = Vec::new();
        for framework in frameworks {
            let Some(stem) = framework
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToString::to_string)
            else {
                continue;
            };
            let Ok(versions) = std::fs::read_dir(framework.join("Versions")) else {
                continue;
            };
            let mut version_dirs = versions
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name != "Current")
                })
                .collect::<Vec<_>>();
            version_dirs.sort();
            if let Some(binary) = version_dirs
                .into_iter()
                .map(|version| version.join(&stem))
                .find(|binary| binary.is_file())
            {
                candidates.push(binary);
            }
        }
        return candidates;
    }
    let mut candidates = Vec::new();
    let runtime = app_dir.join("chrome.dll");
    if runtime.is_file() {
        candidates.push(runtime);
    }
    let executable = codey_runtime_core::app_paths::build_codex_executable(app_dir);
    if executable.is_file() && candidates.iter().all(|candidate| candidate != &executable) {
        candidates.push(executable);
    }
    candidates
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Serialize, Deserialize)]
struct FuseCacheEntry {
    path: String,
    len: u64,
    modified_ms: Option<u64>,
    version: Option<u8>,
    states: Option<String>,
}

#[cfg(any(windows, target_os = "macos"))]
fn cache_path() -> PathBuf {
    codey_runtime_core::paths::default_app_state_dir().join(CACHE_FILE)
}

#[cfg(any(windows, target_os = "macos", test))]
fn binary_signature(path: &Path) -> Option<(u64, Option<u64>)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
    Some((metadata.len(), modified_ms))
}

#[cfg(any(windows, target_os = "macos", test))]
fn load_cached_wire(
    cache: &Path,
    binary: &Path,
    signature: (u64, Option<u64>),
) -> Option<Option<FuseWire>> {
    let bytes = crate::fs_util::read_bounded(cache, MAX_CACHE_BYTES).ok()?;
    let entry: FuseCacheEntry = serde_json::from_slice(&bytes).ok()?;
    if entry.path != binary.to_string_lossy()
        || entry.len != signature.0
        || entry.modified_ms != signature.1
    {
        return None;
    }
    Some(match (entry.version, entry.states) {
        (Some(version), Some(states)) => Some(FuseWire { version, states }),
        _ => None,
    })
}

#[cfg(any(windows, target_os = "macos", test))]
fn store_cached_wire(
    cache: &Path,
    binary: &Path,
    signature: (u64, Option<u64>),
    wire: Option<&FuseWire>,
) -> Result<()> {
    let entry = FuseCacheEntry {
        path: binary.to_string_lossy().into_owned(),
        len: signature.0,
        modified_ms: signature.1,
        version: wire.map(|wire| wire.version),
        states: wire.map(|wire| wire.states.clone()),
    };
    crate::fs_util::atomic_write_private_with_parent(cache, &serde_json::to_vec(&entry)?)
}

/// Reads the wire for `binary`, reusing a cached result while the binary's
/// size and modification time are unchanged.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn cached_fuse_wire(binary: &Path, cache: &Path) -> Result<(Option<FuseWire>, bool)> {
    let signature = binary_signature(binary)
        .with_context(|| format!("读取 Electron 二进制信息失败：{}", binary.display()))?;
    if let Some(wire) = load_cached_wire(cache, binary, signature) {
        return Ok((wire, true));
    }
    let wire = read_fuse_wire(binary)?;
    if let Err(error) = store_cached_wire(cache, binary, signature, wire.as_ref()) {
        crate::error_log::record_failure(
            "compatibility_fallback",
            "store_electron_fuse_cache",
            format!("{error:#}"),
            serde_json::json!({ "cache": cache }),
        );
    }
    Ok((wire, false))
}

/// Resolves the runtime together with its fuse wire. `cache` stores the parsed
/// wire between launches; repair helpers that only validate a target pass
/// `None` and read the wire directly. When no candidate carries a wire the
/// first candidate is returned so callers keep reporting a useful error.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn electron_runtime_with_wire(
    app_dir: &Path,
    cache: Option<&Path>,
) -> Result<(PathBuf, Option<FuseWire>, bool)> {
    let candidates = electron_binary_candidates(app_dir);
    anyhow::ensure!(!candidates.is_empty(), "找不到 Codex 的 Electron 运行时");
    let mut without_wire = None;
    let mut failure = None;
    for candidate in candidates {
        let wire = match cache {
            Some(cache) => cached_fuse_wire(&candidate, cache),
            None => read_fuse_wire(&candidate).map(|wire| (wire, false)),
        };
        match wire {
            Ok((Some(wire), cached)) => return Ok((candidate, Some(wire), cached)),
            Ok((None, cached)) => {
                if without_wire.is_none() {
                    without_wire = Some((candidate, cached));
                }
            }
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error.context(format!(
                        "读取 Electron fuse wire 失败：{}",
                        candidate.display()
                    )));
                }
            }
        }
    }
    if let Some((candidate, cached)) = without_wire {
        return Ok((candidate, None, cached));
    }
    Err(failure.unwrap_or_else(|| anyhow::anyhow!("找不到 Codex 的 Electron 运行时")))
}

/// Resolves the Inspector and `NODE_OPTIONS` fuses for the Codex desktop app.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn read_electron_fuses(app_dir: &Path) -> ElectronFuses {
    let started = Instant::now();
    match electron_runtime_with_wire(app_dir, Some(&cache_path())) {
        Ok((binary, wire, cached)) => {
            let fuses = ElectronFuses::from_wire(wire.as_ref());
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.electron_fuses",
                serde_json::json!({
                    "binary": binary,
                    "cached": cached,
                    "version": wire.as_ref().map(|wire| wire.version),
                    "states": wire.as_ref().map(|wire| wire.states.as_str()),
                    "nodeCliInspect": fuses.node_cli_inspect.as_str(),
                    "nodeOptions": fuses.node_options.as_str(),
                    "scanMs": started.elapsed().as_millis(),
                }),
            );
            fuses
        }
        Err(error) => {
            crate::error_log::record_failure(
                "compatibility_fallback",
                "read_electron_fuses",
                format!("{error:#}"),
                serde_json::json!({ "appPath": app_dir }),
            );
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.electron_fuses",
                serde_json::json!({
                    "appPath": app_dir,
                    "nodeCliInspect": FuseState::Unknown.as_str(),
                    "nodeOptions": FuseState::Unknown.as_str(),
                    "error": format!("{error:#}"),
                    "scanMs": started.elapsed().as_millis(),
                }),
            );
            ElectronFuses::unknown()
        }
    }
}

/// Blocking-pool wrapper for [`read_electron_fuses`]; the first scan of a
/// new Codex build reads the whole executable.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) async fn detect_electron_fuses(app_dir: PathBuf) -> ElectronFuses {
    tokio::task::spawn_blocking(move || read_electron_fuses(&app_dir))
        .await
        .unwrap_or_else(|_| ElectronFuses::unknown())
}

#[cfg(any(windows, test))]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeOptionsBackup {
    format: u8,
    binary: PathBuf,
    offset: u64,
    normalized_sha256: String,
    original: u8,
}

/// Fingerprint the entire runtime with only the target byte normalized. This
/// permits rollback after our one-byte edit, but rejects replaced/updated files.
#[cfg(any(windows, test))]
fn normalized_digest(file: &mut std::fs::File, offset: u64) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut position = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if offset >= position && offset < position + count as u64 {
            buffer[(offset - position) as usize] = b'0';
        }
        hash.update(&buffer[..count]);
        position += count as u64;
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(any(windows, test))]
fn change_node_options(binary: &Path, backup: &Path, cache: &Path, restore: bool) -> Result<bool> {
    let binary = std::fs::canonicalize(binary).context("找不到待修复的 Electron 运行时")?;
    let mut file = open_runtime_exclusive(&binary).with_context(|| {
        format!("无法独占写入 {}；请确认 Codex 已关闭、运行时未被其他程序占用，并确认当前账户具有运行时文件写入权限", binary.display())
    })?;
    let inspection = inspect_node_options(&mut file)?;
    if !prepare_node_options_change(&binary, backup, cache, restore, &inspection)? {
        return Ok(false);
    }
    write_node_options(&mut file, &inspection, if restore { b'0' } else { b'1' })?;
    Ok(true)
}

#[cfg(any(windows, test))]
fn open_runtime_exclusive(binary: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Keep the same exclusive handle through inspection, backup and edit.
        options.share_mode(0);
    }
    options.open(binary)
}

#[cfg(any(windows, test))]
struct NodeOptionsInspection {
    offset: u64,
    current: u8,
    digest: String,
}

#[cfg(any(windows, test))]
fn inspect_node_options(file: &mut std::fs::File) -> Result<NodeOptionsInspection> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let (sentinel, wire) = scan_fuse_wire(file, true)?.context("运行时没有 Electron fuse wire")?;
    anyhow::ensure!(
        wire.version == 1 && (4..=9).contains(&wire.states.len()),
        "不支持的 Electron fuse wire 版本或长度，未修改运行时"
    );
    let current = wire.states.as_bytes()[NODE_OPTIONS_FUSE_INDEX];
    anyhow::ensure!(
        matches!(current, b'0' | b'1'),
        "NODE_OPTIONS fuse 已移除，无法修复"
    );
    let offset = sentinel + FUSE_SENTINEL.len() as u64 + 2 + NODE_OPTIONS_FUSE_INDEX as u64;
    let digest = normalized_digest(file, offset)?;
    Ok(NodeOptionsInspection {
        offset,
        current,
        digest,
    })
}

#[cfg(any(windows, test))]
fn prepare_node_options_change(
    binary: &Path,
    backup: &Path,
    cache: &Path,
    restore: bool,
    inspection: &NodeOptionsInspection,
) -> Result<bool> {
    let NodeOptionsInspection {
        offset,
        current,
        digest,
    } = inspection;
    let saved = match read_node_options_backup(backup)? {
        Some(saved) if node_options_backup_matches(&saved, binary, *offset, digest) => Some(saved),
        // An in-place update replaces the runtime and invalidates the recorded
        // original byte. Enabling may record the current byte again, because the
        // byte below still is the state a later restore must return to.
        Some(_) if restore => {
            anyhow::bail!("NODE_OPTIONS 备份与当前运行时不匹配（文件可能已更新），未修改运行时")
        }
        Some(saved) => {
            crate::error_log::record_failure(
                "runtime_repair_failed",
                "node_options_backup_replaced",
                "NODE_OPTIONS 备份与当前运行时不匹配，已按当前运行时重新记录".to_string(),
                serde_json::json!({
                    "binary": binary,
                    "backup": backup,
                    "savedDigest": saved.normalized_sha256,
                    "runtimeDigest": digest,
                }),
            );
            None
        }
        None if restore => anyhow::bail!("没有此运行时的 NODE_OPTIONS 备份，无法恢复"),
        None => None,
    };
    let desired = if restore { b'0' } else { b'1' };
    // Invalidate even for an already-correct runtime: a previous same-size edit
    // may not have changed its millisecond timestamp.
    match std::fs::remove_file(cache) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("清除 Electron fuse 缓存失败，未修改运行时"),
    }
    if *current == desired {
        return Ok(false);
    }
    if saved.is_none() {
        let saved = NodeOptionsBackup {
            format: 1,
            binary: binary.to_path_buf(),
            offset: *offset,
            normalized_sha256: digest.clone(),
            original: *current,
        };
        crate::fs_util::atomic_write_private_with_parent(backup, &serde_json::to_vec(&saved)?)
            .context("保存 NODE_OPTIONS 原始字节备份失败，未修改运行时")?;
    }
    Ok(true)
}

/// Reads the recorded original byte. A damaged or foreign backup is an error;
/// the caller decides whether it may be replaced.
#[cfg(any(windows, test))]
fn read_node_options_backup(backup: &Path) -> Result<Option<NodeOptionsBackup>> {
    if !backup.exists() {
        return Ok(None);
    }
    let bytes = crate::fs_util::read_bounded(backup, MAX_CACHE_BYTES)?;
    let saved: NodeOptionsBackup =
        serde_json::from_slice(&bytes).context("NODE_OPTIONS 备份损坏，未修改运行时")?;
    Ok(Some(saved))
}

#[cfg(any(windows, test))]
fn node_options_backup_matches(
    saved: &NodeOptionsBackup,
    binary: &Path,
    offset: u64,
    digest: &str,
) -> bool {
    saved.format == 1
        && saved.binary == binary
        && saved.offset == offset
        && saved.original == b'0'
        && saved.normalized_sha256 == digest
}

#[cfg(any(windows, test))]
fn write_node_options(
    file: &mut std::fs::File,
    inspection: &NodeOptionsInspection,
    desired: u8,
) -> Result<()> {
    write_node_options_with(file, inspection, desired, |file| {
        file.sync_all().map_err(Into::into)
    })
}

#[cfg(any(windows, test))]
fn write_node_options_with(
    file: &mut std::fs::File,
    inspection: &NodeOptionsInspection,
    desired: u8,
    mut sync: impl FnMut(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let NodeOptionsInspection {
        offset,
        current,
        digest,
    } = inspection;
    let write_byte = |file: &mut std::fs::File, byte| -> Result<()> {
        file.seek(SeekFrom::Start(*offset))?;
        file.write_all(&[byte])?;
        Ok(())
    };
    let result = (|| {
        write_byte(file, desired)?;
        sync(file)?;
        file.seek(SeekFrom::Start(*offset))?;
        let mut actual = [0];
        file.read_exact(&mut actual)?;
        anyhow::ensure!(
            actual[0] == desired && normalized_digest(file, *offset)? == *digest,
            "NODE_OPTIONS 写入后校验失败"
        );
        Ok(())
    })();
    if let Err(error) = result {
        return match write_byte(file, *current).and_then(|_| file.sync_all().map_err(Into::into)) {
            Ok(()) => Err(error).context("NODE_OPTIONS 修改失败，已还原原始字节"),
            Err(rollback) => anyhow::bail!(
                "NODE_OPTIONS 修改失败：{error:#}；还原也失败：{rollback:#}；请保留原始字节备份"
            ),
        };
    }
    Ok(())
}

#[cfg(windows)]
const NODE_OPTIONS_BACKUP_DIR: &str = "node-options-backups";
/// Automatic attempts live next to the backups so both follow the runtime path.
#[cfg(windows)]
const NODE_OPTIONS_AUTO_REPAIR_DIR: &str = "node-options-auto-repair";

/// Fingerprint of the runtime path; keeps the backup and the automatic-attempt
/// record stable while the install path itself does not change.
#[cfg(windows)]
fn runtime_key(binary: &Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(binary.to_string_lossy().as_bytes()))
}

/// Canonical runtime that carries the fuse wire together with its backup file.
#[cfg(windows)]
fn repair_target(app_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let (binary, _wire, _cached) = electron_runtime_with_wire(app_dir, Some(&cache_path()))?;
    let binary = std::fs::canonicalize(&binary).context("找不到待修复的 Electron 运行时")?;
    let backup = codey_runtime_core::paths::default_app_state_dir()
        .join(NODE_OPTIONS_BACKUP_DIR)
        .join(format!("{}.json", runtime_key(&binary)));
    Ok((binary, backup))
}

/// Called after the managed Codex has stopped; the exclusive handle below is
/// the final guard against another process still using this runtime.
#[cfg(windows)]
pub(crate) fn repair_node_options(app_dir: &Path) -> Result<()> {
    let (binary, backup) = repair_target(app_dir)?;
    windows_repair::repair(&binary, &backup, &cache_path())
}

/// Raised instead of opening a UAC prompt when only an administrator may write
/// the runtime. The launcher must not elevate during startup; the manual repair
/// keeps that path.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct RepairNeedsElevation;

#[cfg(windows)]
impl std::fmt::Display for RepairNeedsElevation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("修复受保护的 Codex 运行时需要管理员权限")
    }
}

#[cfg(windows)]
impl std::error::Error for RepairNeedsElevation {}

/// What the launcher's automatic repair attempt reached. Only [`Repaired`]
/// releases the launch path back to `NODE_OPTIONS`; every other outcome keeps
/// the CLI compatibility mode and the manual repair button.
///
/// [`Repaired`]: AutoRepairOutcome::Repaired
#[cfg(windows)]
#[derive(Debug)]
pub(crate) enum AutoRepairOutcome {
    Repaired,
    AlreadyEnabled,
    AlreadyAttempted,
    NeedsElevation,
    Failed(String),
}

#[cfg(windows)]
impl AutoRepairOutcome {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Repaired => "repaired",
            Self::AlreadyEnabled => "already_enabled",
            Self::AlreadyAttempted => "already_attempted",
            Self::NeedsElevation => "needs_elevation",
            Self::Failed(_) => "failed",
        }
    }

    pub(crate) fn repaired(&self) -> bool {
        matches!(self, Self::Repaired)
    }

    pub(crate) fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error.as_str()),
            _ => None,
        }
    }
}

/// One automatic attempt per runtime build. The runtime is identified by size
/// and modification time: a Codex update replaces the file, which releases the
/// record, while a repeated failure keeps the compatibility mode instead of
/// looping.
#[cfg(any(windows, test))]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeOptionsAutoRepairState {
    format: u8,
    binary: PathBuf,
    len: u64,
    modified_ms: Option<u64>,
    outcome: String,
    error: Option<String>,
}

#[cfg(any(windows, test))]
fn auto_repair_attempted(state_path: &Path, binary: &Path, signature: (u64, Option<u64>)) -> bool {
    let Ok(bytes) = crate::fs_util::read_bounded(state_path, MAX_CACHE_BYTES) else {
        return false;
    };
    let Ok(state) = serde_json::from_slice::<NodeOptionsAutoRepairState>(&bytes) else {
        return false;
    };
    state.format == 1
        && state.binary == binary
        && state.len == signature.0
        && state.modified_ms == signature.1
}

#[cfg(any(windows, test))]
fn record_auto_repair_attempt(
    state_path: &Path,
    binary: &Path,
    signature: (u64, Option<u64>),
    outcome: &str,
    error: Option<&str>,
) -> Result<()> {
    let state = NodeOptionsAutoRepairState {
        format: 1,
        binary: binary.to_path_buf(),
        len: signature.0,
        modified_ms: signature.1,
        outcome: outcome.to_string(),
        error: error.map(|error| error.chars().take(512).collect()),
    };
    crate::fs_util::atomic_write_private_with_parent(state_path, &serde_json::to_vec(&state)?)
}

/// Automatic repair for the launcher. It never elevates and never retries the
/// same runtime build twice, so a locked or protected runtime falls back to the
/// CLI compatibility mode instead of blocking or looping during startup.
#[cfg(windows)]
pub(crate) fn auto_repair_node_options(app_dir: &Path) -> AutoRepairOutcome {
    let cache = cache_path();
    let (binary, _wire, _cached) = match electron_runtime_with_wire(app_dir, Some(&cache)) {
        Ok(resolved) => resolved,
        Err(error) => return AutoRepairOutcome::Failed(format!("{error:#}")),
    };
    let binary = match std::fs::canonicalize(&binary) {
        Ok(binary) => binary,
        Err(error) => {
            return AutoRepairOutcome::Failed(format!("找不到待修复的 Electron 运行时：{error}"));
        }
    };
    let Some(signature) = binary_signature(&binary) else {
        return AutoRepairOutcome::Failed("无法读取待修复的 Electron 运行时信息".to_string());
    };
    let key = runtime_key(&binary);
    let state_dir = codey_runtime_core::paths::default_app_state_dir();
    let state_path = state_dir
        .join(NODE_OPTIONS_AUTO_REPAIR_DIR)
        .join(format!("{key}.json"));
    if auto_repair_attempted(&state_path, &binary, signature) {
        return AutoRepairOutcome::AlreadyAttempted;
    }
    let backup = state_dir
        .join(NODE_OPTIONS_BACKUP_DIR)
        .join(format!("{key}.json"));
    let outcome = match windows_repair::repair_without_elevation(&binary, &backup, &cache) {
        Ok(true) => AutoRepairOutcome::Repaired,
        Ok(false) => AutoRepairOutcome::AlreadyEnabled,
        Err(error) if error.downcast_ref::<RepairNeedsElevation>().is_some() => {
            AutoRepairOutcome::NeedsElevation
        }
        Err(error) => AutoRepairOutcome::Failed(format!("{error:#}")),
    };
    if let Err(error) = record_auto_repair_attempt(
        &state_path,
        &binary,
        signature,
        outcome.as_str(),
        outcome.error(),
    ) {
        crate::error_log::record_failure(
            "runtime_repair_failed",
            "record_auto_repair_attempt",
            format!("{error:#}"),
            serde_json::json!({ "state": state_path }),
        );
    }
    outcome
}

fn is_node_options_repair_command(command: &str) -> bool {
    matches!(
        command,
        "--repair-codex-node-options"
            | "--restore-codex-node-options"
            | "--help-codex-node-options"
    )
}

pub(crate) fn run_node_options_repair_if_requested() -> Result<bool> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        return Ok(false);
    };
    if !is_node_options_repair_command(command) {
        return Ok(false);
    }
    let restore = match command {
        "--repair-codex-node-options" => false,
        "--restore-codex-node-options" => true,
        "--help-codex-node-options" => {
            let help = "Codey Windows 主进程注入修复\n\n--repair-codex-node-options [Codex 安装目录]\n启用 NODE_OPTIONS，备份原始字节。\n--restore-codex-node-options [Codex 安装目录]\n校验当前运行时后恢复原始字节。\n\n省略目录时使用 Codey 配置中的安装目录。请先退出 Codex 和其他 Codey 进程。修复需要文件写入权限；商店更新后可能需要重新修复。修改运行时可能使原签名失效。";
            println!("{help}");
            #[cfg(windows)]
            rfd::MessageDialog::new()
                .set_title("Codey 帮助")
                .set_description(help)
                .show();
            return Ok(true);
        }
        _ => return Ok(false),
    };
    let result = (|| -> Result<String> {
        anyhow::ensure!(args.len() <= 2, "用法：{command} [Codex 安装目录]");
        #[cfg(not(windows))]
        {
            let _ = restore;
            anyhow::bail!("NODE_OPTIONS 运行时修复仅支持 Windows");
        }
        #[cfg(windows)]
        {
            let directory = if let Some(path) = args.get(1) {
                PathBuf::from(path)
            } else {
                let config = crate::config::ConfigStore::default().load()?;
                anyhow::ensure!(
                    !config.codex_app_path.trim().is_empty(),
                    "请提供 Codex 安装目录"
                );
                PathBuf::from(config.codex_app_path)
            };
            let directory = codey_runtime_core::app_paths::normalize_codex_app_path(&directory)
                .context("无效的 Codex 安装目录")?;
            let processes = codey_runtime_core::windows_enumerate_processes()
                .context("无法检查 Codex 运行状态")?;
            anyhow::ensure!(
                !processes
                    .iter()
                    .any(|process| process.process_id != std::process::id()
                        && ["chatgpt.exe", "codex.exe", "codey.exe"]
                            .iter()
                            .any(|name| process.exe_file.eq_ignore_ascii_case(name))),
                "请先退出 Codex 和其他 Codey 进程，再执行 NODE_OPTIONS 修复或恢复"
            );
            let (binary, backup) = repair_target(&directory)?;
            let changed = change_node_options(&binary, &backup, &cache_path(), restore)?;
            let backup_note = if backup.is_file() {
                format!("\n原始字节备份：{}", backup.display())
            } else {
                String::new()
            };
            Ok(format!(
                "{}。请重新从 Codey 启动 Codex。\n运行时：{}{backup_note}",
                if !changed {
                    "NODE_OPTIONS 已处于目标状态"
                } else if restore {
                    "已恢复 NODE_OPTIONS 原始状态"
                } else {
                    "已启用 NODE_OPTIONS"
                },
                binary.display()
            ))
        }
    })();
    #[cfg(windows)]
    rfd::MessageDialog::new()
        .set_title("Codey 主进程注入修复")
        .set_description(match &result {
            Ok(message) => message.clone(),
            Err(error) => format!("{error:#}"),
        })
        .set_level(if result.is_ok() {
            rfd::MessageLevel::Info
        } else {
            rfd::MessageLevel::Error
        })
        .show();
    println!("{}", result?);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preferred runtime file: the main executable on Windows and Linux
    /// (including split chrome.dll runtimes), or the renamed Electron framework
    /// inside a macOS bundle.
    fn electron_binary_path(app_dir: &Path) -> Option<PathBuf> {
        electron_binary_candidates(app_dir).into_iter().next()
    }

    pub(super) fn wire_bytes(states: &str) -> Vec<u8> {
        let mut bytes = FUSE_SENTINEL.to_vec();
        bytes.push(FUSE_WIRE_VERSION_V1);
        bytes.push(states.len() as u8);
        bytes.extend_from_slice(states.as_bytes());
        bytes
    }

    #[test]
    fn repair_commands_leave_codex_cli_help_and_other_arguments_untouched() {
        for command in ["--help", "-h", "--version", "app-server", "--debug-port"] {
            assert!(!is_node_options_repair_command(command));
        }
        for command in [
            "--repair-codex-node-options",
            "--restore-codex-node-options",
            "--help-codex-node-options",
        ] {
            assert!(is_node_options_repair_command(command));
        }
    }

    #[test]
    fn fuse_wire_maps_each_state_byte() {
        let wire = FuseWire {
            version: 1,
            states: "010011001".to_string(),
        };
        assert_eq!(wire.state(0), FuseState::Disabled);
        assert_eq!(wire.state(1), FuseState::Enabled);
        assert_eq!(wire.state(NODE_OPTIONS_FUSE_INDEX), FuseState::Disabled);
        assert_eq!(wire.state(NODE_CLI_INSPECT_FUSE_INDEX), FuseState::Disabled);
        assert_eq!(wire.state(9), FuseState::Unknown);
        assert_eq!(
            FuseWire {
                version: 2,
                states: "1".to_string()
            }
            .state(0),
            FuseState::Unknown
        );
        assert!(FuseState::Enabled.inspector_possible());
        assert!(FuseState::Unknown.inspector_possible());
        assert!(!FuseState::Disabled.inspector_possible());
        assert!(!FuseState::Removed.inspector_possible());
        assert!(FuseState::Enabled.node_options_possible());
        assert!(FuseState::Unknown.node_options_possible());
        assert!(!FuseState::Disabled.node_options_possible());
        assert!(!FuseState::Removed.node_options_possible());
        let fuses = ElectronFuses::from_wire(Some(&wire));
        assert_eq!(fuses.node_cli_inspect, FuseState::Disabled);
        assert_eq!(fuses.node_options, FuseState::Disabled);
    }

    #[test]
    fn scanner_finds_the_wire_across_chunk_boundaries_and_reports_absence() {
        let temp = tempfile::tempdir().unwrap();
        // Straddle the chunk boundary so half of the sentinel sits in each read.
        let padding = SCAN_CHUNK_BYTES - FUSE_SENTINEL.len() / 2;
        let mut bytes = vec![b'x'; padding];
        bytes.extend(wire_bytes("0100110r1"));
        bytes.extend(std::iter::repeat_n(b'y', 4096));
        let binary = temp.path().join("straddle.bin");
        std::fs::write(&binary, &bytes).unwrap();
        assert_eq!(
            read_fuse_wire(&binary).unwrap(),
            Some(FuseWire {
                version: 1,
                states: "0100110r1".to_string()
            })
        );

        // A wire cut off by the end of one chunk must be completed by the next.
        let mut bytes = vec![b'x'; SCAN_CHUNK_BYTES - FUSE_SENTINEL.len() - 3];
        bytes.extend(wire_bytes("110011001"));
        let binary = temp.path().join("tail.bin");
        std::fs::write(&binary, &bytes).unwrap();
        assert_eq!(
            read_fuse_wire(&binary)
                .unwrap()
                .unwrap()
                .state(NODE_CLI_INSPECT_FUSE_INDEX),
            FuseState::Disabled
        );

        let plain = temp.path().join("plain.bin");
        std::fs::write(&plain, b"no electron here").unwrap();
        assert_eq!(read_fuse_wire(&plain).unwrap(), None);

        let truncated = temp.path().join("truncated.bin");
        std::fs::write(&truncated, &wire_bytes("0101")[..FUSE_SENTINEL.len() + 2]).unwrap();
        assert!(read_fuse_wire(&truncated).is_err());

        let invalid = temp.path().join("invalid.bin");
        let mut bytes = FUSE_SENTINEL.to_vec();
        bytes.extend([1, 3, b'0', b'x', b'1']);
        std::fs::write(&invalid, bytes).unwrap();
        assert!(read_fuse_wire(&invalid).is_err());
    }

    #[test]
    fn cache_is_reused_only_while_the_binary_signature_matches() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("Codex.exe");
        std::fs::write(&binary, wire_bytes("010011001")).unwrap();
        let cache = temp.path().join("state").join(CACHE_FILE);

        let (wire, cached) = cached_fuse_wire(&binary, &cache).unwrap();
        assert!(!cached);
        assert_eq!(wire.unwrap().states, "010011001");
        let (wire, cached) = cached_fuse_wire(&binary, &cache).unwrap();
        assert!(cached);
        assert_eq!(wire.unwrap().states, "010011001");

        // A different build (size change) must be scanned again.
        std::fs::write(&binary, wire_bytes("0101110011")).unwrap();
        let (wire, cached) = cached_fuse_wire(&binary, &cache).unwrap();
        assert!(!cached);
        assert_eq!(wire.unwrap().states, "0101110011");

        // Binaries without a wire are cached as such too.
        let plain = temp.path().join("plain.exe");
        std::fs::write(&plain, b"not electron").unwrap();
        assert_eq!(cached_fuse_wire(&plain, &cache).unwrap(), (None, false));
        assert_eq!(cached_fuse_wire(&plain, &cache).unwrap(), (None, true));
    }

    #[test]
    fn electron_binary_is_the_executable_or_the_bundled_framework() {
        let temp = tempfile::tempdir().unwrap();
        let windows_app = temp.path().join("app");
        std::fs::create_dir_all(&windows_app).unwrap();
        assert_eq!(electron_binary_path(&windows_app), None);
        std::fs::write(windows_app.join("Codex.exe"), "exe").unwrap();
        assert_eq!(
            electron_binary_path(&windows_app),
            Some(windows_app.join("Codex.exe"))
        );
        std::fs::write(windows_app.join("chrome.dll"), wire_bytes("010011001")).unwrap();
        assert_eq!(
            electron_binary_path(&windows_app),
            Some(windows_app.join("chrome.dll"))
        );
        assert_eq!(
            read_fuse_wire(&electron_binary_path(&windows_app).unwrap())
                .unwrap()
                .unwrap()
                .state(NODE_OPTIONS_FUSE_INDEX),
            FuseState::Disabled
        );

        let bundle = temp.path().join("Codex.app");
        let framework =
            bundle.join("Contents/Frameworks/Codex Framework.framework/Versions/152.0.1");
        std::fs::create_dir_all(&framework).unwrap();
        std::fs::create_dir_all(bundle.join("Contents/Frameworks/Other.framework/Versions/A"))
            .unwrap();
        assert_eq!(electron_binary_path(&bundle), None);
        std::fs::write(framework.join("Codex Framework"), "framework").unwrap();
        assert_eq!(
            electron_binary_path(&bundle),
            Some(framework.join("Codex Framework"))
        );
    }

    #[test]
    fn repair_and_restore_only_change_node_options_and_invalidate_cache() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("chrome.dll");
        let backup = temp.path().join("backup.json");
        let cache = temp.path().join(CACHE_FILE);
        let mut original = b"runtime prefix".to_vec();
        original.extend(wire_bytes("010011001"));
        original.extend(b"runtime suffix");
        std::fs::write(&binary, &original).unwrap();
        cached_fuse_wire(&binary, &cache).unwrap();
        assert!(change_node_options(&binary, &backup, &cache, false).unwrap());
        assert!(!cache.exists());
        let patched = std::fs::read(&binary).unwrap();
        assert_eq!(
            original
                .iter()
                .zip(&patched)
                .filter(|(a, b)| a != b)
                .count(),
            1
        );
        let (wire, cached) = cached_fuse_wire(&binary, &cache).unwrap();
        assert!(!cached);
        assert_eq!(wire.unwrap().states, "011011001");
        assert!(!change_node_options(&binary, &backup, &cache, false).unwrap());
        assert!(change_node_options(&binary, &backup, &cache, true).unwrap());
        assert!(!cache.exists());
        assert_eq!(std::fs::read(&binary).unwrap(), original);
        assert!(!change_node_options(&binary, &backup, &cache, true).unwrap());
    }

    #[test]
    fn repair_rejects_ambiguous_removed_unknown_and_truncated_wires() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("chrome.dll");
        let backup = temp.path().join("backup.json");
        let cache = temp.path().join(CACHE_FILE);
        let mut duplicate = wire_bytes("010011001");
        duplicate.extend(wire_bytes("010011001"));
        let mut unknown = wire_bytes("010011001");
        unknown[FUSE_SENTINEL.len()] = 2;
        for bytes in [
            duplicate,
            unknown,
            wire_bytes("01r011001"),
            wire_bytes("0100110011"),
            FUSE_SENTINEL.to_vec(),
            b"no wire".to_vec(),
        ] {
            std::fs::write(&binary, &bytes).unwrap();
            assert!(change_node_options(&binary, &backup, &cache, false).is_err());
            assert_eq!(std::fs::read(&binary).unwrap(), bytes);
            assert!(!backup.exists());
        }
    }

    #[test]
    fn restore_rejects_missing_damaged_or_stale_backups() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("chrome.dll");
        let backup = temp.path().join("backup.json");
        let cache = temp.path().join(CACHE_FILE);
        std::fs::write(&binary, wire_bytes("010011001")).unwrap();
        assert!(change_node_options(&binary, &backup, &cache, true).is_err());
        change_node_options(&binary, &backup, &cache, false).unwrap();
        let saved = std::fs::read(&backup).unwrap();
        std::fs::write(&backup, b"bad json").unwrap();
        assert!(change_node_options(&binary, &backup, &cache, true).is_err());
        std::fs::write(&backup, saved).unwrap();
        // An in-place update replaces the runtime and invalidates the recorded
        // original byte: restoring must refuse, enabling records the new byte.
        let updated = wire_bytes("110011001");
        std::fs::write(&binary, &updated).unwrap();
        assert!(change_node_options(&binary, &backup, &cache, true).is_err());
        assert_eq!(std::fs::read(&binary).unwrap(), updated);
        assert!(change_node_options(&binary, &backup, &cache, false).unwrap());
        assert_eq!(
            read_fuse_wire(&binary).unwrap().unwrap().states,
            "111011001"
        );
        assert!(change_node_options(&binary, &backup, &cache, true).unwrap());
        assert_eq!(std::fs::read(&binary).unwrap(), updated);
    }

    #[test]
    fn wire_is_read_from_the_executable_when_the_preferred_file_has_none() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let dll = app.join("chrome.dll");
        std::fs::write(&dll, b"no fuse wire here").unwrap();
        let executable = app.join("Codex.exe");
        std::fs::write(&executable, wire_bytes("010011001")).unwrap();
        assert_eq!(electron_binary_path(&app), Some(dll.clone()));
        let (binary, wire, cached) = electron_runtime_with_wire(&app, None).unwrap();
        assert_eq!(binary, executable);
        assert_eq!(wire.unwrap().states, "010011001");
        assert!(!cached);

        // Without any wire the preferred runtime is reported so callers keep a
        // useful error message.
        std::fs::write(&executable, b"no fuse wire here either").unwrap();
        let (binary, wire, _) = electron_runtime_with_wire(&app, None).unwrap();
        assert_eq!(binary, dll);
        assert!(wire.is_none());
        assert!(electron_runtime_with_wire(&temp.path().join("missing"), None).is_err());
    }

    #[test]
    fn automatic_repair_is_attempted_once_per_runtime_build() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("chrome.dll");
        std::fs::write(&binary, wire_bytes("010011001")).unwrap();
        let state_path = temp.path().join("auto-repair.json");
        let signature = binary_signature(&binary).unwrap();
        assert!(!auto_repair_attempted(&state_path, &binary, signature));
        record_auto_repair_attempt(
            &state_path,
            &binary,
            signature,
            "failed",
            Some(&"错误信息".repeat(200)),
        )
        .unwrap();
        assert!(auto_repair_attempted(&state_path, &binary, signature));
        let saved: NodeOptionsAutoRepairState =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(saved.outcome, "failed");
        assert_eq!(
            saved.error.as_deref().map(str::chars).map(Iterator::count),
            Some(512)
        );

        // A new runtime build releases the record, and a damaged record never
        // blocks the automatic attempt permanently.
        std::fs::write(&binary, wire_bytes("0100110011")).unwrap();
        let updated = binary_signature(&binary).unwrap();
        assert!(!auto_repair_attempted(&state_path, &binary, updated));
        std::fs::write(&state_path, b"damaged").unwrap();
        assert!(!auto_repair_attempted(&state_path, &binary, updated));

        // A record for another runtime does not skip this one.
        record_auto_repair_attempt(
            &state_path,
            &temp.path().join("other.dll"),
            updated,
            "repaired",
            None,
        )
        .unwrap();
        assert!(!auto_repair_attempted(&state_path, &binary, updated));
    }

    #[test]
    fn repair_does_not_write_if_cache_cannot_be_invalidated() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("chrome.dll");
        let backup = temp.path().join("backup.json");
        let cache = temp.path().join("cache-directory");
        std::fs::create_dir(&cache).unwrap();
        let original = wire_bytes("010011001");
        std::fs::write(&binary, &original).unwrap();
        assert!(change_node_options(&binary, &backup, &cache, false).is_err());
        assert_eq!(std::fs::read(&binary).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn repair_refuses_a_runtime_open_in_another_process() {
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("chrome.dll");
        std::fs::write(&binary, wire_bytes("010011001")).unwrap();
        let _reader = std::fs::File::open(&binary).unwrap();
        let error = change_node_options(
            &binary,
            &temp.path().join("backup"),
            &temp.path().join("cache"),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Codex 已关闭"));
    }

    /// Real-world check against the installed Codex desktop app when present.
    #[test]
    fn installed_codex_desktop_reports_its_inspect_fuse() {
        let candidates = ["/Applications/ChatGPT.app", "/Applications/Codex.app"];
        let Some(binary) = candidates
            .iter()
            .map(Path::new)
            .find_map(electron_binary_path)
        else {
            return;
        };
        let wire = read_fuse_wire(&binary)
            .expect("installed Electron binary should be readable")
            .expect("installed Electron binary should carry a fuse wire");
        assert_eq!(wire.version, FUSE_WIRE_VERSION_V1);
        assert_ne!(
            wire.state(NODE_CLI_INSPECT_FUSE_INDEX),
            FuseState::Unknown,
            "{wire:?}"
        );
        assert_ne!(
            wire.state(NODE_OPTIONS_FUSE_INDEX),
            FuseState::Unknown,
            "{wire:?}"
        );
    }
}
