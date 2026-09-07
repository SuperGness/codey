//! Reads the Electron fuse wire embedded in the Codex desktop binary.
//!
//! Electron bakes a "fuse wire" into its binary: a sentinel string followed by
//! a version byte, a length byte and one byte per fuse (`0` disabled, `1`
//! enabled, `r` removed). Current Codex builds ship with
//! `EnableNodeCliInspectArguments` disabled, so Electron drops `--inspect-brk`
//! before Node ever sees it and the main-process Inspector patch can never
//! attach. Reading the wire before launch lets the launcher start on the CLI
//! wrapper directly instead of waiting for a debug port that will never answer.

use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(any(windows, target_os = "macos"))]
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Sentinel that precedes the fuse wire in every Electron binary (@electron/fuses).
pub(crate) const FUSE_SENTINEL: &[u8] = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX";
const FUSE_WIRE_VERSION_V1: u8 = 1;
/// Index of `EnableNodeCliInspectArguments` in the v1 fuse wire (`FuseV1Options`).
pub(crate) const NODE_CLI_INSPECT_FUSE_INDEX: usize = 3;
const MAX_FUSE_COUNT: usize = 64;
const SCAN_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const CACHE_FILE: &str = "electron-fuses.json";
const MAX_CACHE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FuseState {
    Enabled,
    Disabled,
    Removed,
    Unknown,
}

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FuseWire {
    pub(crate) version: u8,
    pub(crate) states: String,
}

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
pub(crate) fn read_fuse_wire(path: &Path) -> Result<Option<FuseWire>> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("读取 Electron 二进制失败：{}", path.display()))?;
    let finder = memchr::memmem::Finder::new(FUSE_SENTINEL);
    // Enough trailing context that a sentinel at the end of one chunk is still
    // matched, and its wire completed, by the next read.
    let overlap = FUSE_SENTINEL.len() + 2 + MAX_FUSE_COUNT;
    let mut buffer: Vec<u8> = Vec::with_capacity(SCAN_CHUNK_BYTES + overlap);
    let mut chunk = vec![0_u8; SCAN_CHUNK_BYTES];
    let mut sentinel_at: Option<usize> = None;
    loop {
        let read = file
            .read(&mut chunk)
            .with_context(|| format!("读取 Electron 二进制失败：{}", path.display()))?;
        let end_of_file = read == 0;
        buffer.extend_from_slice(&chunk[..read]);
        if sentinel_at.is_none() {
            sentinel_at = finder.find(&buffer);
        }
        if let Some(offset) = sentinel_at {
            match parse_fuse_wire(&buffer, offset) {
                Some(wire) => return wire.map(Some),
                None if end_of_file => {
                    anyhow::bail!("Electron fuse wire 在文件末尾被截断");
                }
                None => continue,
            }
        }
        if end_of_file {
            return Ok(None);
        }
        if buffer.len() > overlap {
            let keep_from = buffer.len() - overlap;
            buffer.drain(..keep_from);
        }
    }
}

/// Locates the binary that carries the fuse wire: the main executable on
/// Windows and Linux, the renamed Electron framework inside a macOS bundle.
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
    let executable = codey_runtime_core::app_paths::build_codex_executable(app_dir);
    executable.is_file().then_some(executable)
}

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

fn binary_signature(path: &Path) -> Option<(u64, Option<u64>)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
    Some((metadata.len(), modified_ms))
}

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

/// Resolves whether the Codex desktop app at `app_dir` honours `--inspect-brk`.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn node_cli_inspect_state(app_dir: &Path) -> FuseState {
    let started = Instant::now();
    let Some(binary) = electron_binary_path(app_dir) else {
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.electron_fuses",
            serde_json::json!({
                "appPath": app_dir,
                "nodeCliInspect": FuseState::Unknown.as_str(),
                "error": "electron binary not found",
            }),
        );
        return FuseState::Unknown;
    };
    match cached_fuse_wire(&binary, &cache_path()) {
        Ok((wire, cached)) => {
            let state = wire
                .as_ref()
                .map(|wire| wire.state(NODE_CLI_INSPECT_FUSE_INDEX))
                .unwrap_or(FuseState::Unknown);
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.electron_fuses",
                serde_json::json!({
                    "binary": binary,
                    "cached": cached,
                    "version": wire.as_ref().map(|wire| wire.version),
                    "states": wire.as_ref().map(|wire| wire.states.as_str()),
                    "nodeCliInspect": state.as_str(),
                    "scanMs": started.elapsed().as_millis(),
                }),
            );
            state
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
                    "error": format!("{error:#}"),
                    "scanMs": started.elapsed().as_millis(),
                }),
            );
            FuseState::Unknown
        }
    }
}

/// Blocking-pool wrapper for [`node_cli_inspect_state`]; the first scan of a
/// new Codex build reads the whole executable.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) async fn detect_node_cli_inspect_state(app_dir: PathBuf) -> FuseState {
    tokio::task::spawn_blocking(move || node_cli_inspect_state(&app_dir))
        .await
        .unwrap_or(FuseState::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_bytes(states: &str) -> Vec<u8> {
        let mut bytes = FUSE_SENTINEL.to_vec();
        bytes.push(FUSE_WIRE_VERSION_V1);
        bytes.push(states.len() as u8);
        bytes.extend_from_slice(states.as_bytes());
        bytes
    }

    #[test]
    fn fuse_wire_maps_each_state_byte() {
        let wire = FuseWire {
            version: 1,
            states: "010011001".to_string(),
        };
        assert_eq!(wire.state(0), FuseState::Disabled);
        assert_eq!(wire.state(1), FuseState::Enabled);
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
    }
}
