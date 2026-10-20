use crate::fsutil;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Managed {
    pub files: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub modes: BTreeMap<PathBuf, u32>,
    pub source: String,
}
pub type Registry = BTreeMap<PathBuf, Managed>;

pub struct SourceFile {
    pub bytes: Vec<u8>,
    pub executable: bool,
}

pub fn metadata(content: &[u8]) -> Result<(String, String)> {
    ensure!(
        content.len() as u64 <= fsutil::MAX_FILE,
        "Skill 文件超过大小限制"
    );
    let text = std::str::from_utf8(content).context("SKILL.md 必须是 UTF-8")?;
    let mut lines = text.lines();
    ensure!(lines.next() == Some("---"), "SKILL.md 缺少 YAML 元数据");
    let mut yaml = String::new();
    let mut ended = false;
    for line in lines {
        if line == "---" {
            ended = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    ensure!(ended, "SKILL.md 元数据未闭合");
    ensure!(yaml.len() <= 32768, "Skill YAML 元数据超过大小限制");
    // 元数据只读取简单值，拒绝标签、锚点和别名，避免解析不可信对象图。
    ensure!(
        !yaml
            .split_whitespace()
            .any(|token| token.starts_with(['!', '&', '*'])),
        "Skill 元数据不支持 YAML 标签、锚点或别名"
    );
    let value: Value = serde_yaml::from_str(&yaml).context("Skill YAML 无效")?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .context("Skill 缺少 name")?;
    let desc = value
        .get("description")
        .and_then(Value::as_str)
        .context("Skill 缺少 description")?;
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--"),
        "Skill name 必须为小写字母、数字和单连字符，长度不超过 64"
    );
    ensure!(
        !desc.trim().is_empty() && desc.len() <= 4096,
        "Skill description 为空或过长"
    );
    Ok((name.to_owned(), desc.to_owned()))
}

pub fn id(manifest: &Path) -> String {
    fsutil::digest(manifest.to_string_lossy().as_bytes())
}

pub fn entries(
    root: &Path,
    scope: &str,
    registry: &Registry,
    disabled: &[PathBuf],
    warnings: &mut Vec<String>,
    cache: &mut fsutil::ReadCache,
) -> Vec<Value> {
    let files = discover(root, warnings);
    files.into_iter().filter(|p| p.file_name().is_some_and(|s| s == "SKILL.md")).map(|manifest| {
        let path = manifest.parent().unwrap();
        let is_system = scope == "system" || manifest.components().any(|c| c.as_os_str() == ".system");
        let ownership = if scope == "plugin" { "plugin" } else if is_system { "builtin" } else if registry.contains_key(path) { "managed" } else { "external" };
        let readonly = ownership == "plugin" || ownership == "builtin";
        let parsed = cache.read(&manifest).and_then(|bytes| metadata(bytes.context("Skill 文件已消失")?));
        let (name, description, error) = match parsed { Ok((n,d)) => (n,d,None), Err(e) => (path.file_name().unwrap_or_default().to_string_lossy().into_owned(), String::new(), Some(e.to_string())) };
        let effective_scope = if is_system { "system" } else { scope };
        let version = if error.is_none() { cache.read(&manifest).ok().flatten().and_then(manifest_version) } else { None };
        let (dependencies, dependency_warnings) = dependencies(path, cache);
        let mut entry = json!({"id":id(&manifest),"name":name,"description":description,"enabled":!disabled.contains(&manifest),"enabledKnown":ownership != "plugin","sourcePath":path,"manifestPath":manifest,"scope":effective_scope,"ownership":ownership,"origin":registry.get(path).map(|m|m.source.as_str()).unwrap_or(""),"readOnly":readonly,"updatedAt":fsutil::updated_at(&manifest),"configurationStatus":if error.is_none(){"valid"}else{"invalid"},"version":version,"dependencies":dependencies,"dependencyWarnings":dependency_warnings,"canEdit":!readonly && ownership == "managed" && fsutil::writable(&manifest),"canToggle":!readonly,"canRemove":!readonly && path != root && fsutil::writable(path),"canCheck":true});
        if readonly { entry["reason"] = json!(if scope == "plugin" { "插件缓存资源，仅展示，不代表当前会话已加载" } else { "Codex 系统资源不可修改" }); }
        if let Some(error) = error { entry["error"] = json!(error); }
        entry
    }).collect()
}

fn manifest_version(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let yaml = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?
        .split("\n---")
        .next()?;
    let value: Value = serde_yaml::from_str(yaml).ok()?;
    let version = value
        .get("version")
        .or_else(|| value.get("metadata")?.get("version"))?;
    version
        .as_str()
        .map(str::to_owned)
        .or_else(|| version.as_number().map(ToString::to_string))
}

/// 列出一个根目录下所有可解析 Skill 的名称与清单路径，用于同范围重名检查。
/// 无法解析的元数据直接跳过：它们已在清单里单独报告，不参与重名判断。
pub fn named_manifests(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut warnings = Vec::new();
    let mut cache = fsutil::ReadCache::default();
    let manifests = discover(root, &mut warnings)
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "SKILL.md"))
        .filter_map(|manifest| {
            let bytes = cache.read(&manifest).ok().flatten()?;
            let (name, _) = metadata(bytes).ok()?;
            Some((name, manifest))
        })
        .collect();
    cache.ensure_complete()?;
    Ok(manifests)
}

pub fn external_removal_files(path: &Path) -> Result<Vec<PathBuf>> {
    let files = fsutil::walk(path)?;
    let manifest = path.join("SKILL.md");
    ensure!(files.contains(&manifest), "Skill 文件已消失");
    ensure!(
        files.iter().all(|file| {
            !file.components().any(|c| c.as_os_str() == ".system")
                && (file == &manifest || file.file_name().is_none_or(|name| name != "SKILL.md"))
        }),
        "Skill 目录包含系统资源或其他 Skill，请分别处理"
    );
    Ok(files)
}

fn dependencies(path: &Path, cache: &mut fsutil::ReadCache) -> (Vec<Value>, Vec<String>) {
    let parsed = (|| -> Result<Vec<Value>> {
        let Some(bytes) = cache.read(&path.join("agents/openai.yaml"))? else {
            return Ok(vec![]);
        };
        ensure!(bytes.len() <= 32768, "Skill 依赖声明超过大小限制");
        let text = std::str::from_utf8(bytes).context("Skill 依赖声明不是 UTF-8")?;
        ensure!(
            !text
                .split_whitespace()
                .any(|token| token.starts_with(['!', '&', '*'])),
            "Skill 依赖声明不支持 YAML 标签、锚点或别名"
        );
        let value: Value = serde_yaml::from_str(text).context("Skill 依赖声明 YAML 无效")?;
        let Some(tools) = value.get("dependencies").and_then(|d| d.get("tools")) else {
            return Ok(vec![]);
        };
        let tools = tools
            .as_array()
            .context("Skill dependencies.tools 必须为数组")?;
        ensure!(tools.len() <= 100, "Skill 依赖声明超过数量限制");
        tools
            .iter()
            .map(|tool| {
                let kind = tool
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty() && v.len() <= 128)
                    .context("Skill 依赖缺少有效 type")?;
                let name = tool
                    .get("value")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty() && v.len() <= 512)
                    .context("Skill 依赖缺少有效 value")?;
                Ok(json!({"type":kind,"name":name}))
            })
            .collect()
    })();
    match parsed {
        Ok(deps) => (deps, vec![]),
        Err(e) => (vec![], vec![e.to_string()]),
    }
}

pub fn export(root: &Path) -> Result<Vec<u8>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for path in fsutil::walk(root)? {
        let relative = path.strip_prefix(root)?;
        ensure!(
            !relative.to_string_lossy().contains(['\\', ':']),
            "Skill 资源路径不支持安全导出"
        );
        let mode = if fsutil::file_mode(&path)?.is_some_and(|m| m & 0o111 != 0) {
            0o700
        } else {
            0o600
        };
        writer.start_file(
            relative.to_string_lossy().replace('\\', "/"),
            zip::write::SimpleFileOptions::default().unix_permissions(mode),
        )?;
        writer.write_all(&fsutil::read(&path)?.context("Skill 资源已消失")?)?;
    }
    let bytes = writer.finish()?.into_inner();
    ensure!(
        bytes.len() as u64 <= fsutil::MAX_TOTAL,
        "导出 ZIP 超过大小限制"
    );
    Ok(bytes)
}

fn discover(root: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    if !root.exists() {
        return vec![];
    }
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut found = Vec::new();
    let mut count = 0usize;
    while let Some((dir, depth)) = pending.pop() {
        if depth > 20 {
            warnings.push("部分 Skill 目录超过层级限制".to_owned());
            continue;
        }
        if let Err(e) = fsutil::safe_path(&dir) {
            warnings.push(e.to_string());
            continue;
        }
        let listing = match fs::read_dir(&dir) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("无法读取 {}: {e}", dir.display()));
                continue;
            }
        };
        for entry in listing {
            count += 1;
            if count > 20000 {
                warnings.push("Skill 发现达到条目限制，部分目录未展示".to_owned());
                return found;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warnings.push(e.to_string());
                    continue;
                }
            };
            let path = entry.path();
            if let Err(e) = fsutil::safe_path(&path) {
                warnings.push(e.to_string());
                continue;
            }
            if path.is_dir() {
                pending.push((path, depth + 1));
            } else if path.file_name().is_some_and(|s| s == "SKILL.md") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

pub fn load_source(source: &Path) -> Result<BTreeMap<PathBuf, SourceFile>> {
    fsutil::safe_path(source)?;
    let mut files = BTreeMap::new();
    if source.is_dir() {
        let mut total = 0u64;
        for path in fsutil::walk(source)? {
            let bytes = fsutil::read(&path)?.context("源文件已消失")?;
            total += bytes.len() as u64;
            ensure!(total <= fsutil::MAX_TOTAL, "目录内容超过总大小限制");
            files.insert(
                path.strip_prefix(source)?.to_path_buf(),
                SourceFile {
                    bytes,
                    executable: fsutil::file_mode(&path)?.is_some_and(|m| m & 0o111 != 0),
                },
            );
        }
    } else {
        ensure!(source.is_file(), "Skill 来源不存在");
        ensure!(
            fs::metadata(source)?.len() <= fsutil::MAX_TOTAL,
            "ZIP 超过大小限制"
        );
        let mut zip =
            zip::ZipArchive::new(fs::File::open(source)?).context("仅支持本地目录或 ZIP")?;
        ensure!(zip.len() <= fsutil::MAX_FILES, "ZIP 条目超过限制");
        let mut total = 0u64;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            ensure!(
                !entry.name().contains('\\') && !entry.name().contains(':'),
                "ZIP 路径包含不安全字符"
            );
            let relative = entry.enclosed_name().context("ZIP 路径越界")?;
            ensure!(
                !relative.components().any(|c| matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )),
                "ZIP 路径越界"
            );
            ensure!(relative.components().count() <= 20, "ZIP 目录层级超过限制");
            let unix_mode = entry.unix_mode().unwrap_or(0);
            let mode = unix_mode & 0o170000;
            ensure!(
                mode == 0 || mode == 0o100000 || mode == 0o040000,
                "ZIP 包含符号链接或特殊文件"
            );
            if entry.is_dir() {
                continue;
            }
            ensure!(entry.size() <= fsutil::MAX_FILE, "ZIP 文件超过限制");
            let mut bytes = Vec::new();
            (&mut entry)
                .take(fsutil::MAX_FILE + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= fsutil::MAX_FILE,
                "ZIP 解压文件超过限制"
            );
            total += bytes.len() as u64;
            ensure!(total <= fsutil::MAX_TOTAL, "ZIP 解压总量超过限制");
            ensure!(
                files
                    .insert(
                        relative,
                        SourceFile {
                            bytes,
                            executable: unix_mode & 0o111 != 0
                        }
                    )
                    .is_none(),
                "ZIP 包含重复目标路径"
            );
        }
    }
    let roots: Vec<_> = files
        .keys()
        .filter(|p| p.file_name().is_some_and(|s| s == "SKILL.md"))
        .map(|p| p.parent().unwrap_or(Path::new("")).to_path_buf())
        .collect();
    ensure!(
        roots.len() == 1,
        "来源必须恰好包含一个 SKILL.md，多个 Skill 请分别导入"
    );
    let root = &roots[0];
    let selected: BTreeMap<_, _> = files
        .into_iter()
        .filter_map(|(p, b)| p.strip_prefix(root).ok().map(|r| (r.to_path_buf(), b)))
        .collect();
    metadata(
        &selected
            .get(Path::new("SKILL.md"))
            .context("未找到 SKILL.md")?
            .bytes,
    )?;
    Ok(selected)
}

pub fn source_digest(files: &BTreeMap<PathBuf, SourceFile>) -> String {
    let pairs: Vec<_> = files
        .iter()
        .map(|(p, b)| (p.to_string_lossy(), fsutil::digest(&b.bytes), b.executable))
        .collect();
    fsutil::digest(&serde_json::to_vec(&pairs).expect("路径摘要可序列化"))
}
