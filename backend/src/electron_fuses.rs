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

/// Locates the binary that carries the fuse wire: the main executable on
/// Windows and Linux (including split chrome.dll runtimes), or the renamed
/// Electron framework inside a macOS bundle.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn electron_binary_path(app_dir: &Path) -> Option<PathBuf> {
    if app_dir
        .extension()
        .is_some_and(|extension| extension == "app")
    {
        let frameworks = std::fs::read_dir(app_dir.join("Contents").join("Frameworks")).ok()?;
        let mut candidates = frameworks
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(" Framework.framework"))
            })
            .collect::<Vec<_>>();
        candidates.sort();
        for framework in candidates {
            let stem = framework.file_stem()?.to_str()?.to_string();
            let versions = std::fs::read_dir(framework.join("Versions")).ok()?;
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
                return Some(binary);
            }
        }
        return None;
    }
    let runtime = app_dir.join("chrome.dll");
    if runtime.is_file() {
        return Some(runtime);
    }
    let executable = codey_runtime_core::app_paths::build_codex_executable(app_dir);
    executable.is_file().then_some(executable)
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

/// Resolves the Inspector and `NODE_OPTIONS` fuses for the Codex desktop app.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn read_electron_fuses(app_dir: &Path) -> ElectronFuses {
    let started = Instant::now();
    let Some(binary) = electron_binary_path(app_dir) else {
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.electron_fuses",
            serde_json::json!({
                "appPath": app_dir,
                "nodeCliInspect": FuseState::Unknown.as_str(),
                "nodeOptions": FuseState::Unknown.as_str(),
                "error": "electron binary not found",
            }),
        );
        return ElectronFuses::unknown();
    };
    match cached_fuse_wire(&binary, &cache_path()) {
        Ok((wire, cached)) => {
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
                serde_json::json!({ "binary": binary }),
            );
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.electron_fuses",
                serde_json::json!({
                    "binary": binary,
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
    let saved = if backup.exists() {
        let saved: NodeOptionsBackup =
            serde_json::from_slice(&crate::fs_util::read_bounded(backup, MAX_CACHE_BYTES)?)
                .context("NODE_OPTIONS 备份损坏，未修改运行时")?;
        anyhow::ensure!(
            saved.format == 1
                && saved.binary == binary
                && saved.offset == *offset
                && saved.original == b'0'
                && saved.normalized_sha256 == *digest,
            "NODE_OPTIONS 备份与当前运行时不匹配（文件可能已更新），未修改运行时"
        );
        Some(saved)
    } else {
        None
    };
    if restore {
        anyhow::ensure!(
            saved.is_some(),
            "没有此运行时的 NODE_OPTIONS 备份，无法恢复"
        );
    }
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

/// Called after the managed Codex has stopped; the exclusive handle below is
/// the final guard against another process still using this runtime.
#[cfg(windows)]
pub(crate) fn repair_node_options(app_dir: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    let binary = electron_binary_path(app_dir).context("找不到 Electron 运行时")?;
    let binary = std::fs::canonicalize(binary)?;
    let key = format!("{:x}", Sha256::digest(binary.to_string_lossy().as_bytes()));
    let backup = codey_runtime_core::paths::default_app_state_dir()
        .join("node-options-backups")
        .join(format!("{key}.json"));
    windows_repair::repair(&binary, &backup, &cache_path())?;
    Ok(())
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
            let binary = electron_binary_path(&directory).context("找不到 Electron 运行时")?;
            let binary = std::fs::canonicalize(binary)?;
            use sha2::{Digest, Sha256};
            let key = format!("{:x}", Sha256::digest(binary.to_string_lossy().as_bytes()));
            let backup = codey_runtime_core::paths::default_app_state_dir()
                .join("node-options-backups")
                .join(format!("{key}.json"));
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
        let updated = wire_bytes("111011001");
        std::fs::write(&binary, &updated).unwrap();
        assert!(change_node_options(&binary, &backup, &cache, true).is_err());
        assert!(change_node_options(&binary, &backup, &cache, false).is_err());
        assert_eq!(std::fs::read(&binary).unwrap(), updated);
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
