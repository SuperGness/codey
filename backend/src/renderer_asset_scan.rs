//! Codex 前端资源的离线扫描。
//!
//! rspack 构建把前端拆成上万个数字 chunk。若按通配地址拦截，每个加载的 chunk
//! 都要经 CDP 暂停一次，冷启动还要整份读回正文再预筛。这里直接读取安装包里的
//! 资源，找出含补丁前置字面量的 chunk，补丁服务只拦截这些精确地址。结果按
//! 归档身份和补丁版本缓存，同一 Codex 版本只扫一次。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::renderer_asset_patch::numeric_asset_name;

/// ASAR 索引上限。Codex 26.1029 的索引约 3 MB，留足余量又不至于读入异常文件。
const MAX_ASAR_HEADER_BYTES: u64 = 64 << 20;
/// 单个脚本上限；超出说明归档异常，整次扫描作废并回到通配拦截。
const MAX_SCANNED_ASSET_BYTES: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssetScan {
    /// 资源目录中的数字 chunk 数；为 0 说明是按文件名拆分的构建。
    pub(crate) numeric_chunks: usize,
    /// 含补丁前置字面量的数字 chunk，路径相对资源目录，如 `async/24191.589dcc9878.js`。
    pub(crate) candidates: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceIdentity {
    path: String,
    len: u64,
    modified_ms: u128,
}

#[derive(Serialize, Deserialize)]
struct CachedScan {
    source: SourceIdentity,
    scan: AssetScan,
}

pub(crate) struct ScanOutcome {
    pub(crate) scan: AssetScan,
    pub(crate) cached: bool,
    /// 缓存写失败只影响下次启动的速度，本次结果照常使用；由调用方记录。
    pub(crate) cache_write_error: Option<String>,
}

/// 读取缓存或扫描安装包。缓存文件名已含补丁版本，前置字面量变化即失效。
pub(crate) fn load_or_scan(
    app_dir: &Path,
    cache_file: &Path,
    anchors: &[String],
) -> Result<ScanOutcome> {
    let source = AssetSource::locate(app_dir)?;
    let identity = source.identity()?;
    if let Some(scan) = read_cache(cache_file, &identity) {
        return Ok(ScanOutcome {
            scan,
            cached: true,
            cache_write_error: None,
        });
    }
    let scan = source.scan(anchors)?;
    let cached = CachedScan {
        source: identity,
        scan: scan.clone(),
    };
    let cache_write_error = serde_json::to_vec(&cached)
        .context("资源扫描缓存序列化失败")
        .and_then(|bytes| crate::fs_util::atomic_write(cache_file, &bytes))
        .err()
        .map(|error| format!("{error:#}"));
    Ok(ScanOutcome {
        scan,
        cached: false,
        cache_write_error,
    })
}

fn read_cache(cache_file: &Path, identity: &SourceIdentity) -> Option<AssetScan> {
    let cached: CachedScan = serde_json::from_slice(&std::fs::read(cache_file).ok()?).ok()?;
    (cached.source == *identity).then_some(cached.scan)
}

enum AssetSource {
    /// `app.asar`；`unpacked` 指向同名 `.asar.unpacked` 目录。
    Asar { archive: PathBuf, unpacked: PathBuf },
    /// 未打包的 `resources/app` 目录。
    Directory { app: PathBuf },
}

impl AssetSource {
    fn locate(app_dir: &Path) -> Result<Self> {
        let resources = if app_dir
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            vec![app_dir.join("Contents/Resources")]
        } else {
            vec![app_dir.join("resources"), app_dir.join("app/resources")]
        };
        for resources in resources {
            let archive = resources.join("app.asar");
            if archive.is_file() {
                return Ok(Self::Asar {
                    unpacked: resources.join("app.asar.unpacked"),
                    archive,
                });
            }
            let app = resources.join("app");
            if app.join("webview/assets").is_dir() {
                return Ok(Self::Directory { app });
            }
        }
        bail!("未找到 Codex 前端资源：{}", app_dir.display())
    }

    fn identity(&self) -> Result<SourceIdentity> {
        let path = match self {
            Self::Asar { archive, .. } => archive.clone(),
            Self::Directory { app } => app.join("webview/index.html"),
        };
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("无法读取 Codex 资源信息：{}", path.display()))?;
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_millis());
        Ok(SourceIdentity {
            path: path.to_string_lossy().into_owned(),
            len: metadata.len(),
            modified_ms,
        })
    }

    fn scan(&self, anchors: &[String]) -> Result<AssetScan> {
        let matcher = AhoCorasick::new(anchors).context("补丁前置字面量无法构建匹配器")?;
        match self {
            Self::Asar { archive, unpacked } => scan_asar(archive, unpacked, &matcher),
            Self::Directory { app } => scan_directory(&app.join("webview/assets"), &matcher),
        }
    }
}

/// 资源目录里的数字 chunk：根目录与 `async/` 子目录，规则与拦截时一致。
fn numeric_chunk_paths<'a>(
    entries: impl Iterator<Item = (&'a str, bool)>,
    prefix: &str,
    found: &mut Vec<String>,
) {
    for (name, is_file) in entries {
        if is_file && numeric_asset_name(name) {
            found.push(format!("{prefix}{name}"));
        }
    }
}

fn scan_asar(archive: &Path, unpacked: &Path, matcher: &AhoCorasick) -> Result<AssetScan> {
    let mut file = File::open(archive)
        .with_context(|| format!("无法打开 Codex 归档：{}", archive.display()))?;
    let archive_len = file.metadata().context("无法读取 Codex 归档大小")?.len();
    let mut prefix = [0u8; 16];
    file.read_exact(&mut prefix)
        .context("无法读取 ASAR 长度信息")?;
    let header_size = u64::from(u32::from_le_bytes(prefix[4..8].try_into()?));
    let json_size = u64::from(u32::from_le_bytes(prefix[12..16].try_into()?));
    ensure!(
        u32::from_le_bytes(prefix[0..4].try_into()?) == 4
            && header_size <= MAX_ASAR_HEADER_BYTES
            && json_size > 0
            && json_size + 8 <= header_size,
        "ASAR 索引长度无效"
    );
    let data_start = 8 + header_size;
    ensure!(data_start <= archive_len, "ASAR 索引超出归档长度");
    let mut header = vec![0u8; usize::try_from(json_size)?];
    file.read_exact(&mut header).context("无法读取 ASAR 索引")?;
    let header: Value = serde_json::from_slice(&header).context("无法解析 ASAR 索引")?;
    let assets = ["webview", "assets"]
        .iter()
        .try_fold(&header, |node, name| node.get("files")?.get(*name))
        .and_then(|node| node.get("files"))
        .and_then(Value::as_object)
        .context("ASAR 中没有 webview/assets 目录")?;

    let mut entries: Vec<(String, &Value)> = Vec::new();
    for (name, node) in assets {
        if name == "async" {
            if let Some(children) = node.get("files").and_then(Value::as_object) {
                for (child, child_node) in children {
                    if child_node.get("files").is_none() && numeric_asset_name(child) {
                        entries.push((format!("async/{child}"), child_node));
                    }
                }
            }
        } else if node.get("files").is_none() && numeric_asset_name(name) {
            entries.push((name.clone(), node));
        }
    }

    let mut candidates = Vec::new();
    let mut buffer = Vec::new();
    for (relative, node) in &entries {
        let size = node
            .get("size")
            .and_then(Value::as_u64)
            .with_context(|| format!("ASAR 条目缺少大小：{relative}"))?;
        ensure!(size <= MAX_SCANNED_ASSET_BYTES, "ASAR 条目过大：{relative}");
        buffer.resize(usize::try_from(size)?, 0);
        if node.get("unpacked").and_then(Value::as_bool) == Some(true) {
            let path = unpacked.join("webview/assets").join(relative);
            File::open(&path)
                .and_then(|mut unpacked| unpacked.read_exact(&mut buffer))
                .with_context(|| format!("无法读取未打包资源：{}", path.display()))?;
        } else {
            let offset = node
                .get("offset")
                .and_then(Value::as_str)
                .and_then(|offset| offset.parse::<u64>().ok())
                .with_context(|| format!("ASAR 条目缺少偏移：{relative}"))?;
            let start = data_start
                .checked_add(offset)
                .filter(|start| {
                    start
                        .checked_add(size)
                        .is_some_and(|end| end <= archive_len)
                })
                .with_context(|| format!("ASAR 条目超出归档长度：{relative}"))?;
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(&mut buffer)
                .with_context(|| format!("无法读取 ASAR 条目：{relative}"))?;
        }
        if matcher.is_match(&buffer) {
            candidates.push(relative.clone());
        }
    }
    candidates.sort();
    Ok(AssetScan {
        numeric_chunks: entries.len(),
        candidates,
    })
}

fn scan_directory(assets: &Path, matcher: &AhoCorasick) -> Result<AssetScan> {
    let list = |dir: &Path| -> Result<Vec<(String, bool)>> {
        let mut names = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
            Err(error) => {
                return Err(error).with_context(|| format!("无法列出资源目录：{}", dir.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            names.push((
                entry.file_name().to_string_lossy().into_owned(),
                entry.file_type()?.is_file(),
            ));
        }
        Ok(names)
    };
    let mut paths = Vec::new();
    let root = list(assets)?;
    numeric_chunk_paths(
        root.iter().map(|(name, file)| (name.as_str(), *file)),
        "",
        &mut paths,
    );
    let nested = list(&assets.join("async"))?;
    numeric_chunk_paths(
        nested.iter().map(|(name, file)| (name.as_str(), *file)),
        "async/",
        &mut paths,
    );
    let mut candidates = Vec::new();
    for relative in &paths {
        let path = assets.join(relative);
        let size = std::fs::metadata(&path)?.len();
        ensure!(size <= MAX_SCANNED_ASSET_BYTES, "资源文件过大：{relative}");
        let bytes = std::fs::read(&path)
            .with_context(|| format!("无法读取资源文件：{}", path.display()))?;
        if matcher.is_match(&bytes) {
            candidates.push(relative.clone());
        }
    }
    candidates.sort();
    Ok(AssetScan {
        numeric_chunks: paths.len(),
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchors() -> Vec<String> {
        vec![
            "sendMessageFromView".to_string(),
            "serviceTierAccess:".to_string(),
        ]
    }

    /// 按 Electron ASAR 格式写一个最小归档：Pickle 头 + JSON 索引 + 正文。
    fn write_asar(path: &Path, files: &[(&str, &[u8])], unpacked: &[&str]) {
        let mut body = Vec::new();
        let mut tree = serde_json::json!({ "files": {} });
        for (relative, contents) in files {
            let mut node = &mut tree;
            let parts: Vec<&str> = relative.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                node = node["files"]
                    .as_object_mut()
                    .unwrap()
                    .entry(part.to_string())
                    .or_insert_with(|| serde_json::json!({ "files": {} }));
            }
            let entry = if unpacked.contains(relative) {
                serde_json::json!({ "size": contents.len(), "unpacked": true })
            } else {
                let entry =
                    serde_json::json!({ "size": contents.len(), "offset": body.len().to_string() });
                body.extend_from_slice(contents);
                entry
            };
            node["files"][parts[parts.len() - 1]] = entry;
        }
        let json = serde_json::to_vec(&tree).unwrap();
        let padded = json.len().div_ceil(4) * 4;
        let header_size = (padded + 8) as u32;
        let mut archive = Vec::new();
        archive.extend_from_slice(&4u32.to_le_bytes());
        archive.extend_from_slice(&header_size.to_le_bytes());
        archive.extend_from_slice(&(header_size - 4).to_le_bytes());
        archive.extend_from_slice(&(json.len() as u32).to_le_bytes());
        archive.extend_from_slice(&json);
        archive.resize(8 + header_size as usize, 0);
        archive.extend_from_slice(&body);
        std::fs::write(path, archive).unwrap();
    }

    #[test]
    fn asar_scan_keeps_only_numeric_chunks_with_patch_anchors() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("Codex.app/Contents/Resources");
        std::fs::create_dir_all(resources.join("app.asar.unpacked/webview/assets/async")).unwrap();
        std::fs::write(
            resources.join("app.asar.unpacked/webview/assets/async/5.dddddddddd.js"),
            b"x.sendMessageFromView(y)",
        )
        .unwrap();
        write_asar(
            &resources.join("app.asar"),
            &[
                ("package.json", b"{}"),
                ("webview/index.html", b"<html>"),
                ("webview/assets/1.aaaaaaaaaa.js", b"export const icon=1"),
                ("webview/assets/2.bbbbbbbbbb.js", b"{serviceTierAccess:a}"),
                (
                    "webview/assets/async/3.cccccccccc.js",
                    b"r?.sendMessageFromView&&r()",
                ),
                ("webview/assets/async/4.eeeeeeeeee.js", b"plain"),
                (
                    "webview/assets/async/5.dddddddddd.js",
                    b"x.sendMessageFromView(y)",
                ),
                (
                    "webview/assets/named-90eb42395b7d.js",
                    b"sendMessageFromView",
                ),
                (
                    "webview/assets/hoots-spritesheet-v8.52075ad356.webp",
                    b"RIFF",
                ),
            ],
            &["webview/assets/async/5.dddddddddd.js"],
        );
        let cache = temp.path().join("scan.json");
        let first = load_or_scan(&temp.path().join("Codex.app"), &cache, &anchors()).unwrap();
        assert!(!first.cached);
        assert_eq!(first.scan.numeric_chunks, 5);
        assert_eq!(
            first.scan.candidates,
            [
                "2.bbbbbbbbbb.js",
                "async/3.cccccccccc.js",
                "async/5.dddddddddd.js"
            ]
        );
        let second = load_or_scan(&temp.path().join("Codex.app"), &cache, &anchors()).unwrap();
        assert!(second.cached);
        assert_eq!(second.scan, first.scan);
    }

    #[test]
    fn named_builds_report_no_numeric_chunks() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("Codex.app/Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        write_asar(
            &resources.join("app.asar"),
            &[(
                "webview/assets/app-initial-90eb42395b7d.js",
                b"sendMessageFromView",
            )],
            &[],
        );
        let scan = load_or_scan(
            &temp.path().join("Codex.app"),
            &temp.path().join("scan.json"),
            &anchors(),
        )
        .unwrap()
        .scan;
        assert_eq!(
            scan,
            AssetScan {
                numeric_chunks: 0,
                candidates: Vec::new()
            }
        );
    }

    #[test]
    fn unpacked_app_directories_are_scanned_and_a_changed_archive_rescans() {
        let temp = tempfile::tempdir().unwrap();
        let assets = temp.path().join("Codex/resources/app/webview/assets");
        std::fs::create_dir_all(assets.join("async")).unwrap();
        std::fs::write(assets.parent().unwrap().join("index.html"), b"<html>").unwrap();
        std::fs::write(assets.join("async/7.0123456789.js"), b"serviceTierAccess:").unwrap();
        std::fs::write(assets.join("8.0123456789.js"), b"none").unwrap();
        let cache = temp.path().join("scan.json");
        let app_dir = temp.path().join("Codex");
        let scan = load_or_scan(&app_dir, &cache, &anchors()).unwrap().scan;
        assert_eq!(scan.candidates, ["async/7.0123456789.js"]);
        assert_eq!(scan.numeric_chunks, 2);

        std::fs::write(
            assets.parent().unwrap().join("index.html"),
            b"<html>updated",
        )
        .unwrap();
        assert!(!load_or_scan(&app_dir, &cache, &anchors()).unwrap().cached);
    }

    #[test]
    fn missing_or_corrupt_archives_fail_instead_of_guessing() {
        let temp = tempfile::tempdir().unwrap();
        assert!(load_or_scan(temp.path(), &temp.path().join("scan.json"), &anchors()).is_err());
        let resources = temp.path().join("Codex.app/Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(resources.join("app.asar"), b"not an archive").unwrap();
        assert!(
            load_or_scan(
                &temp.path().join("Codex.app"),
                &temp.path().join("scan.json"),
                &anchors()
            )
            .is_err()
        );
    }
}
