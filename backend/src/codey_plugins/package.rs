use super::Manifest;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Cursor, Read},
    path::Path,
};

pub const MAX_PACKAGE: u64 = 64 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 128 * 1024 * 1024;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub path: String,
    pub sha256: String,
    pub manifest: Manifest,
}

pub struct Package {
    pub inspection: Inspection,
    pub files: BTreeMap<String, Vec<u8>>,
    pub default_config: String,
}

pub fn digest(bytes: &[u8]) -> String {
    crate::fs_util::sha256_hex(bytes)
}

pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
                && ![
                    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
                    "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
                    "LPT8", "LPT9",
                ]
                .contains(
                    &part
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                )
        })
}

pub fn valid_id(id: &str) -> bool {
    id.len() <= 96
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && !id.contains("..")
        && !id.ends_with('.')
}

pub fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if !valid_id(&manifest.id) {
        return Err("插件 ID 无效".into());
    }
    if manifest.name.trim().is_empty() || manifest.name.len() > 160 {
        return Err("插件名称无效".into());
    }
    if manifest.version.len() > 64
        || !safe_relative(&manifest.version)
        || manifest.version.contains('/')
    {
        return Err("插件版本无效".into());
    }
    semver::Version::parse(&manifest.version).map_err(|e| format!("插件版本必须是 semver: {e}"))?;
    if manifest.abi_version != codey_plugin_sdk::ABI_VERSION {
        return Err("不支持该插件 ABI 版本".into());
    }
    if manifest.platform != std::env::consts::OS || manifest.arch != std::env::consts::ARCH {
        return Err(format!(
            "插件平台不匹配，当前为 {} / {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    }
    if !safe_relative(&manifest.entry) {
        return Err("插件入口路径无效".into());
    }
    let extension = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    if Path::new(&manifest.entry)
        .extension()
        .and_then(|v| v.to_str())
        != Some(extension)
    {
        return Err("插件动态库扩展名不匹配".into());
    }
    if manifest.library_sha256.len() != 64
        || !manifest
            .library_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("librarySha256 必须是小写 SHA-256".into());
    }
    let lifecycle = super::lifecycle::enabled(manifest);
    let mut capabilities = HashSet::new();
    if manifest.capabilities.iter().any(|s| {
        ![
            codey_plugin_sdk::appserver::CAPABILITY,
            codey_plugin_sdk::lifecycle::CAPABILITY,
            codey_plugin_sdk::lifecycle::AUTH_CAPABILITY,
            codey_plugin_sdk::provider::CAPABILITY,
        ]
        .contains(&s.as_str())
            || !capabilities.insert(s)
    }) {
        return Err("插件声明了尚未支持的扩展能力".into());
    }
    if manifest
        .header_names
        .iter()
        .any(|s| !super::allowed_header_name(s))
    {
        return Err("插件声明了禁止修改或无效的请求头".into());
    }
    if !manifest.header_names.is_empty() && !lifecycle {
        return Err("headerNames 需要请求扩展能力".into());
    }
    if !["abort", "continue"].contains(
        &manifest
            .lifecycle_failure_policy
            .as_deref()
            .unwrap_or("abort"),
    ) || !(1..=600_000).contains(&manifest.lifecycle_max_wait_ms.unwrap_or(30_000))
    {
        return Err("生命周期失败策略或等待时间无效".into());
    }
    if !lifecycle
        && (manifest
            .capabilities
            .iter()
            .any(|s| s == "request.lifecycle.auth")
            || !manifest.response_header_names.is_empty()
            || manifest.lifecycle_failure_policy.is_some()
            || manifest.lifecycle_max_wait_ms.is_some())
    {
        return Err("生命周期字段需要 request.lifecycle.v1 能力".into());
    }
    for (names, response) in [
        (&manifest.header_names, false),
        (&manifest.response_header_names, true),
    ] {
        let mut seen = HashSet::new();
        if names.len() > 32
            || names.iter().any(|name| {
                !seen.insert(name.to_ascii_lowercase())
                    || if response {
                        !super::lifecycle::allowed_response_header_name(name)
                    } else {
                        !super::allowed_header_name(name)
                    }
            })
        {
            return Err("请求头或响应头声明无效、重复或超过 32 项".into());
        }
    }
    Ok(())
}

pub fn read(path: &Path) -> Result<Package, String> {
    if path.extension().and_then(|s| s.to_str()) != Some("codey-plugin") {
        return Err("请选择 .codey-plugin 安装包".into());
    }
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_PACKAGE {
        return Err("插件包不是普通文件或超过 64 MiB".into());
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PACKAGE {
        return Err("插件包超过大小限制".into());
    }
    let sha256 = digest(&bytes);
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("ZIP 格式无效: {e}"))?;
    if zip.len() > 256 {
        return Err("插件包条目超过 256 个".into());
    }
    let mut files = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let is_dir = entry.is_dir();
        let name = entry
            .name()
            .strip_suffix('/')
            .unwrap_or(entry.name())
            .to_owned();
        if !safe_relative(&name) || !seen.insert(name.to_ascii_lowercase()) {
            return Err("插件包存在非法路径或重复条目".into());
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && !(kind == 0o040000 && is_dir) {
                return Err("插件包不允许符号链接或特殊文件".into());
            }
        }
        if is_dir {
            continue;
        }
        total = total.checked_add(entry.size()).ok_or("插件包大小溢出")?;
        if total > MAX_EXTRACTED || entry.size() > MAX_PACKAGE {
            return Err("插件解压大小超过限制".into());
        }
        let mut content = Vec::new();
        (&mut entry)
            .take(MAX_PACKAGE + 1)
            .read_to_end(&mut content)
            .map_err(|e| e.to_string())?;
        if content.len() as u64 != entry.size() || content.len() as u64 > MAX_PACKAGE {
            return Err("插件条目大小不一致".into());
        }
        files.insert(name, content);
    }
    for name in files.keys() {
        let parts: Vec<_> = name.split('/').collect();
        for i in 1..parts.len() {
            if files
                .keys()
                .any(|p| p.eq_ignore_ascii_case(&parts[..i].join("/")))
            {
                return Err("插件包文件与目录冲突".into());
            }
        }
    }
    let manifest_bytes = files.get("manifest.json").ok_or("缺少 manifest.json")?;
    if manifest_bytes.len() > 1024 * 1024 {
        return Err("manifest 过大".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(manifest_bytes).map_err(|e| format!("manifest 无效: {e}"))?;
    validate_manifest(&manifest)?;
    let library = files.get(&manifest.entry).ok_or("找不到插件入口动态库")?;
    if digest(library) != manifest.library_sha256 {
        return Err("插件动态库 SHA-256 不匹配".into());
    }
    let config = files.get(super::CONFIG_FILE).ok_or("缺少 config.json")?;
    let content = std::str::from_utf8(config).map_err(|_| "配置文件必须为 UTF-8")?;
    super::parse_config(content)?;
    let default_config = content.to_owned();
    Ok(Package {
        inspection: Inspection {
            path: path.to_string_lossy().into(),
            sha256,
            manifest,
        },
        files,
        default_config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_manifest_dependencies_and_header_lists_are_checked() {
        let base = serde_json::to_value(super::super::tests::fixture_package().inspection.manifest)
            .unwrap();
        for field in ["lifecycleFailurePolicy", "lifecycleMaxWaitMs"] {
            let mut value = base.clone();
            value[field] = serde_json::Value::Null;
            assert!(serde_json::from_value::<Manifest>(value).is_err());
        }
        for fields in [
            serde_json::json!({"capabilities":["request.beforeSend"]}),
            serde_json::json!({"capabilities":["request.lifecycle.v1","request.beforeSend"]}),
            serde_json::json!({"headerNames":["x-test"]}),
            serde_json::json!({"capabilities":["request.lifecycle.auth"]}),
            serde_json::json!({"capabilities":["request.lifecycle.v1","request.lifecycle.v1"]}),
            serde_json::json!({"lifecycleFailurePolicy":"abort"}),
            serde_json::json!({"lifecycleMaxWaitMs":30000}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"lifecycleMaxWaitMs":0}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"lifecycleMaxWaitMs":600001}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"lifecycleFailurePolicy":"ignore"}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"responseHeaderNames":["set-cookie"]}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"responseHeaderNames":["x-test","X-Test"]}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"headerNames":["x-test","X-Test"]}),
        ] {
            let mut value = base.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let manifest: Manifest = serde_json::from_value(value).unwrap();
            assert!(validate_manifest(&manifest).is_err());
        }
        for fields in [
            serde_json::json!({"capabilities":["request.lifecycle.v1"],"headerNames":["x-test"]}),
            serde_json::json!({"capabilities":["request.lifecycle.v1"]}),
            serde_json::json!({"capabilities":["provider.route.v1"]}),
            serde_json::json!({"capabilities":["appserver.call.v1"]}),
            serde_json::json!({"capabilities":["request.lifecycle.v1","request.lifecycle.auth"],"responseHeaderNames":["content-type","x-test"],"lifecycleFailurePolicy":"continue","lifecycleMaxWaitMs":600000}),
        ] {
            let mut value = base.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let manifest: Manifest = serde_json::from_value(value).unwrap();
            validate_manifest(&manifest).unwrap();
        }
    }
    fn package_fixture(
        config: Option<&[u8]>,
        legacy_schema: Option<&str>,
    ) -> Result<Package, String> {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fixture.codey-plugin");
        let mut manifest =
            serde_json::to_value(super::super::tests::fixture_package().inspection.manifest)
                .unwrap();
        manifest["librarySha256"] = digest(b"library").into();
        if let Some(schema) = legacy_schema {
            manifest["configSchema"] = schema.into();
            manifest["configUi"] =
                serde_json::json!({"type":"html", "entry":"missing.html", "sha256":"ignored"});
        }
        let mut archive = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let mut entries = vec![
            (
                "manifest.json".to_owned(),
                serde_json::to_vec(&manifest).unwrap(),
            ),
            (
                manifest["entry"].as_str().unwrap().to_owned(),
                b"library".to_vec(),
            ),
        ];
        if let Some(config) = config {
            entries.push(("config.json".into(), config.to_vec()));
        }
        for (name, bytes) in entries {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        archive.finish().unwrap();
        read(&path)
    }

    #[test]
    fn package_requires_json_config_and_rejects_legacy_metadata() {
        let text = b"{\r\n \"value\": 1\r\n}\r\n";
        let package = package_fixture(Some(text), None).unwrap();
        assert_eq!(package.default_config.as_bytes(), text);
        assert!(package_fixture(Some(b"{}"), Some("config.json")).is_err());
        assert!(package_fixture(None, Some("missing.schema.json")).is_err());
        assert!(
            package_fixture(None, None)
                .err()
                .unwrap()
                .contains("缺少 config.json")
        );
        for field in ["configSchema", "configUi"] {
            let mut manifest =
                serde_json::to_value(super::super::tests::fixture_package().inspection.manifest)
                    .unwrap();
            manifest[field] = serde_json::json!({});
            assert!(serde_json::from_value::<Manifest>(manifest).is_err());
        }
        for invalid in [b"[]".as_slice(), b"{broken".as_slice(), &[0xff]] {
            assert!(package_fixture(Some(invalid), None).is_err());
        }
        assert!(
            package_fixture(
                Some(&vec![b' '; super::super::MAX_CONFIG_BYTES as usize + 1]),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn package_validates_comments_and_preserves_the_default_text() {
        let text = "{\r\n  \"_comments\": {\"value\": \"说明\"},\r\n  \"value\": 1, \"rules\": [{\"_comments\": {}}]\r\n}\r\n";
        let package = package_fixture(Some(text.as_bytes()), None).unwrap();
        assert_eq!(package.default_config, text);
        assert_eq!(package.files["config.json"], text.as_bytes());
        for invalid in [
            r#"{"_comments":[]}"#,
            r#"{"rules":[{"_comments":{"model":false}}]}"#,
        ] {
            let error = package_fixture(Some(invalid.as_bytes()), None)
                .err()
                .unwrap();
            assert!(error.contains("_comments"), "{error}");
        }
    }

    #[test]
    fn reject_traversal_and_platform_path_aliases() {
        for path in [
            "../a",
            "/a",
            "a/../b",
            "a\\b",
            "C:/a",
            "a//b",
            "a/CON.txt",
            "a.",
            "a/./b",
        ] {
            assert!(!safe_relative(path), "{path}");
        }
        assert!(safe_relative("lib/plugin.dylib"));
    }
}
