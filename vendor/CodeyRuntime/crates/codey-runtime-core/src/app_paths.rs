use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::process::Command;

#[derive(Debug, Clone, Copy)]
struct AppPackageSpec {
    identity: &'static str,
    app_id: &'static str,
    executable_names: &'static [&'static str],
    priority: u8,
}

const CODEX_PACKAGE_EXECUTABLES: &[&str] = &["ChatGPT.exe", "Codex.exe"];
const STANDALONE_CODEX_EXECUTABLES: &[&str] = &["ChatGPT.exe", "Codex.exe"];
const CODEX_APP_ENTRY_DIRS: &[&str] = &["app", "bin", "current", "versions/current"];

/// `package.json` names the Codex desktop client has shipped under. The client
/// merged into the ChatGPT app still identifies as `openai-codex-electron`, so
/// discovery must match the Electron metadata rather than the bundle name.
const CODEX_ELECTRON_PACKAGE_NAMES: &[&str] = &[
    "openai-codex-electron",
    "codex",
    "codex-desktop",
    "@openai/codex",
];

/// Windows resolves program names case-insensitively, so an install whose main
/// binary is spelled `codex.exe` must not be skipped.
const EXECUTABLE_NAMES_ARE_CASE_INSENSITIVE: bool = cfg!(windows);

/// Windows refuses a selected file that no Codex launcher sits beside, so a
/// third-party tool's binary cannot name its own directory as the Codex app.
const SELECTED_FILE_REQUIRES_SIBLING_EXECUTABLE: bool = cfg!(windows);

const APP_PACKAGE_SPECS: &[AppPackageSpec] = &[
    AppPackageSpec {
        identity: "OpenAI.Codex",
        app_id: "App",
        executable_names: CODEX_PACKAGE_EXECUTABLES,
        priority: 1,
    },
    AppPackageSpec {
        identity: "OpenAI.CodexBeta",
        app_id: "App",
        executable_names: CODEX_PACKAGE_EXECUTABLES,
        priority: 1,
    },
];

pub fn find_latest_codex_app_dir(root: &Path) -> Option<PathBuf> {
    let mut matches = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let spec = package_spec_from_path(&path)?;
            let version = version_tuple(&path)?;
            let app_dir = package_entry_dir(&path)?;
            Some((spec.priority, version, app_dir))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .reverse()
            .then_with(|| left.1.cmp(&right.1))
    });
    let (_, _, latest) = matches.pop()?;
    Some(latest)
}

pub fn find_latest_codex_app_dir_from_roots(roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .filter_map(|root| find_latest_codex_app_dir(root))
        .max_by(|left, right| compare_app_dir_candidates(left, right))
}

pub fn find_latest_codex_app_dir_default() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // Store activation uses the registered package, which may have moved to another drive.
        find_latest_codex_app_dir_from_appx_package()
    }

    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn find_latest_codex_app_dir_from_appx_package() -> Option<PathBuf> {
    unique_installation(registered_codex_app_dirs()?)
}

#[cfg(windows)]
fn registered_codex_app_dirs() -> Option<Vec<PathBuf>> {
    registered_codex_packages()?
        .iter()
        .map(|package| normalize_codex_app_path(&package.install_location))
        .collect()
}

#[cfg(any(windows, test))]
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RegisteredCodexPackage {
    package_full_name: String,
    install_location: PathBuf,
}

#[cfg(windows)]
fn registered_codex_packages() -> Option<Vec<RegisteredCodexPackage>> {
    let output = Command::new("powershell")
        .creation_flags(crate::windows_create_no_window())
        .args([
            "-NoProfile",
            "-Command",
            "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); $names=@('OpenAI.Codex','OpenAI.CodexBeta'); $packages=@(Get-AppxPackage | Where-Object { $names -contains $_.Name } | Select-Object PackageFullName,InstallLocation); ConvertTo-Json -InputObject $packages -Compress",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    registered_codex_packages_from_output(&output.stdout)
}

#[cfg(any(windows, test))]
fn registered_codex_packages_from_output(output: &[u8]) -> Option<Vec<RegisteredCodexPackage>> {
    let packages: Vec<RegisteredCodexPackage> = serde_json::from_slice(output).ok()?;
    packages
        .iter()
        .all(|package| {
            supported_codex_package_identity(&package.package_full_name)
                && package.install_location.is_absolute()
        })
        .then_some(packages)
}

#[cfg(any(windows, test))]
fn registered_package_full_name(
    app_dir: &Path,
    packages: &[RegisteredCodexPackage],
) -> Option<String> {
    let app_dir = std::fs::canonicalize(app_dir).ok()?;
    let mut matches = packages
        .iter()
        .filter_map(|package| {
            let root = std::fs::canonicalize(&package.install_location).ok()?;
            let entry = normalize_codex_app_path(&root)?;
            let entry = std::fs::canonicalize(entry).ok()?;
            // 注册位置必须指向同一个入口，不能只按祖先目录匹配或跟随越界链接。
            (entry == app_dir && entry.starts_with(root)).then_some(&package.package_full_name)
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    if matches.len() != 1 || !supported_codex_package_identity(matches[0]) {
        return None;
    }
    Some(matches[0].clone())
}

fn unique_installation(paths: Vec<PathBuf>) -> Option<PathBuf> {
    let mut paths = paths
        .into_iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()
        .ok()?;
    paths.sort();
    paths.dedup();
    (paths.len() == 1).then(|| paths.remove(0))
}

pub fn latest_appx_install_location_from_output(output: &str) -> Option<String> {
    let mut paths = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    (paths.len() == 1).then(|| paths[0].to_string())
}

pub fn find_macos_codex_app(search_roots: &[PathBuf]) -> Option<PathBuf> {
    let mut candidates = search_roots
        .iter()
        .flat_map(|root| macos_app_candidates(root))
        .filter(|path| validate_codex_app_dir(path).is_ok())
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    (candidates.len() == 1).then(|| candidates.remove(0))
}

pub fn find_macos_codex_app_default() -> Option<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
        roots.push(home.join("Applications"));
    }
    find_macos_codex_app(&roots)
}

pub fn resolve_codex_app_dir(app_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(app_dir) = app_dir {
        return normalize_codex_app_path(app_dir);
    }
    if cfg!(target_os = "macos") {
        return find_macos_codex_app_default();
    }
    #[cfg(windows)]
    {
        // A failed registration query must not turn into a standalone guess.
        let mut paths = registered_codex_app_dirs()?;
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.extend(standalone_codex_app_dirs_from(Path::new(&local)));
        }
        unique_installation(paths)
    }
    #[cfg(not(windows))]
    {
        find_standalone_codex_app_dir()
    }
}

/// Search for standalone Codex installations (non-MS Store).
///
/// Common paths:
/// - %LOCALAPPDATA%\Programs\Codex\  (standalone installer)
/// - %LOCALAPPDATA%\OpenAI\Codex\bin\  (standalone installer)
/// - %LOCALAPPDATA%\OpenAI\Codex\      (user data root)
/// - %LOCALAPPDATA%\Programs\OpenAI\Codex\ (alternative)
pub fn find_standalone_codex_app_dir() -> Option<PathBuf> {
    let local_appdata = std::env::var_os("LOCALAPPDATA")?;

    find_standalone_codex_app_dir_from(Path::new(&local_appdata))
}

fn find_standalone_codex_app_dir_from(local_appdata: &Path) -> Option<PathBuf> {
    unique_installation(standalone_codex_app_dirs_from(local_appdata))
}

fn standalone_codex_app_dirs_from(local_appdata: &Path) -> Vec<PathBuf> {
    let candidates: &[PathBuf] = &[
        local_appdata.join("Programs").join("Codex"),
        local_appdata.join("OpenAI").join("Codex").join("bin"),
        local_appdata.join("OpenAI").join("Codex"),
        local_appdata.join("Programs").join("OpenAI").join("Codex"),
    ];

    candidates
        .iter()
        .filter_map(|candidate| normalize_codex_app_path(candidate))
        .filter(|path| build_codex_executable(path).is_file())
        .collect()
}

pub fn resolve_codex_app_dir_with_saved(
    app_dir: Option<&Path>,
    saved_app_path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(app_dir) = app_dir {
        return normalize_codex_app_path(app_dir);
    }
    if let Some(saved) = saved_app_path
        .map(str::trim)
        .filter(|saved| !saved.is_empty())
        && let Some(path) = normalize_codex_app_path(Path::new(saved))
    {
        return Some(path);
    }
    resolve_codex_app_dir(None)
}

pub fn normalize_codex_app_path(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }

    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if is_supported_app_executable_name(file_name) {
        let parent = path.parent()?;
        return executable_in_dir(parent).map(|_| parent.to_path_buf());
    }

    if path.extension() == Some(OsStr::new("app")) {
        return path.is_dir().then(|| path.to_path_buf());
    }

    if path.is_file() {
        return install_dir_from_selected_file(path, SELECTED_FILE_REQUIRES_SIBLING_EXECUTABLE);
    }

    if executable_in_dir(path).is_some() {
        return Some(path.to_path_buf());
    }

    let nested = CODEX_APP_ENTRY_DIRS
        .iter()
        .map(|entry| path.join(entry))
        .filter(|nested| executable_in_dir(nested).is_some())
        .collect::<Vec<_>>();
    if let Some(first) = nested.first().cloned() {
        // Preserve the selected layout, but refuse different installations
        // underneath it. Canonicalization allows aliases of the same install.
        return unique_installation(nested).map(|_| first);
    }

    #[cfg(not(windows))]
    if path.is_dir() {
        return Some(path.to_path_buf());
    }

    None
}

/// Names the install directory a user-selected file belongs to.
///
/// A file only names a Codex install when a launchable launcher sits beside it.
/// Guessing the parent of an unrelated binary (a CLI build or a third-party
/// Codex launcher such as `codex-x.exe`) would hand the launcher a path it can
/// never start, and the failure would only surface as a spawn error.
fn install_dir_from_selected_file(
    path: &Path,
    require_sibling_executable: bool,
) -> Option<PathBuf> {
    let parent = path.parent()?;
    if !require_sibling_executable || executable_in_dir(parent).is_some() {
        return Some(parent.to_path_buf());
    }
    None
}

pub fn build_codex_executable(app_dir: &Path) -> PathBuf {
    if app_dir.extension() == Some(OsStr::new("app")) {
        let macos_dir = app_dir.join("Contents").join("MacOS");
        if let Some(executable) = macos_app_plist_value(app_dir, "CFBundleExecutable")
            .filter(|value| !value.contains('/') && !value.contains('\\'))
        {
            return macos_dir.join(executable);
        }
        return macos_dir.join("Codex");
    }
    if let Some(executable) = executable_in_dir(app_dir) {
        return executable;
    }
    if let Some(spec) = package_spec_from_path(app_dir) {
        return app_dir.join(spec.executable_names[0]);
    }
    app_dir.join("Codex.exe")
}

/// Identifies the application without replacing the OS signature checks.
pub fn validate_codex_app_dir(app_dir: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    anyhow::ensure!(app_dir.is_absolute(), "Codex 安装路径必须是绝对路径");
    let root = std::fs::canonicalize(app_dir).context("Codex 安装路径不存在或无法访问")?;
    anyhow::ensure!(root.is_dir(), "Codex 安装路径不是目录");
    let executable = std::fs::canonicalize(build_codex_executable(&root))
        .context("Codex 启动程序不存在或无法访问")?;
    anyhow::ensure!(
        executable.is_file() && executable.starts_with(&root),
        "Codex 启动程序超出安装目录或不是普通文件"
    );
    let macos = root.extension() == Some(OsStr::new("app"));
    let resources = if macos {
        root.join("Contents/Resources")
    } else {
        root.join("resources")
    };
    let archive = resources.join("app.asar");
    let package = if archive.try_exists().context("无法检查 Codex 应用归档")? {
        asar_app_package(&archive)
    } else {
        read_app_package_json(&resources.join("app/package.json"))
    }
    .context("无法读取 Codex 应用身份，安装可能不完整或版本不受支持")?;
    let name = package.get("name").and_then(serde_json::Value::as_str);
    let product = package
        .get("productName")
        .and_then(serde_json::Value::as_str);
    // Either marker is enough here: the platform identity below is the one that
    // decides, so a renamed bundle or product must not strand an installation.
    anyhow::ensure!(
        product == Some("Codex")
            || name.is_some_and(|name| CODEX_ELECTRON_PACKAGE_NAMES
                .iter()
                .any(|known| name.eq_ignore_ascii_case(known))),
        "应用元数据不属于已支持的 Codex 客户端"
    );
    anyhow::ensure!(
        package
            .get("version")
            .and_then(serde_json::Value::as_str)
            .and_then(normalize_version_value)
            .is_some(),
        "Codex 应用版本缺失或无效"
    );
    if macos {
        anyhow::ensure!(
            macos_app_plist_value(&root, "CFBundleIdentifier").as_deref()
                == Some("com.openai.codex"),
            "macOS 应用标识不属于已支持的 Codex 客户端"
        );
    } else if let Some(package_name) = packaged_app_full_name(&root) {
        anyhow::ensure!(
            supported_codex_package_identity(&package_name),
            "不支持的 Codex Windows 包身份或版本：{package_name}"
        );
    } else {
        anyhow::ensure!(
            !root
                .components()
                .any(|part| part.as_os_str().eq_ignore_ascii_case("WindowsApps")),
            "无法安全识别 Windows 打包安装，未按独立安装处理；安装目录：{}",
            root.display()
        );
        // Standalone distributions can ship an AppxManifest.xml too. The file
        // alone does not give the executable a Windows package identity.
        for directory in root.ancestors().take(3) {
            if directory
                .join("AppxManifest.xml")
                .try_exists()
                .context("无法检查 Codex 安装清单")?
            {
                ensure_unregistered_windows_install(&root)?;
                break;
            }
        }
    }
    Ok(())
}

fn ensure_unregistered_windows_install(app_dir: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use anyhow::Context;

        // Include all identities: an unrecognized registered package must not
        // fall back to standalone launch just because its name is unfamiliar.
        let output = Command::new("powershell")
            .creation_flags(crate::windows_create_no_window())
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r#"$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new()
Get-AppxPackage | ForEach-Object {
    try { $_ | Select-Object -ExpandProperty InstallLocation }
    catch {
        # Removed packages can leave registrations with no installation location.
        if ($_.Exception.InnerException -isnot [System.IO.FileNotFoundException]) { throw }
    }
}"#,
            ])
            .output()
            .context("无法查询 Windows 包注册信息，未按独立安装处理")?;
        anyhow::ensure!(
            output.status.success(),
            "查询 Windows 包注册信息失败，未按独立安装处理"
        );
        let locations =
            String::from_utf8(output.stdout).context("Windows 包注册路径不是有效 UTF-8")?;
        anyhow::ensure!(
            !app_dir_is_within_registered_package(app_dir, &locations)?,
            "无法安全识别 Windows 打包安装，未按独立安装处理：{}",
            app_dir.display()
        );
        Ok(())
    }
    #[cfg(not(windows))]
    anyhow::bail!(
        "无法核实 Windows 包注册信息，未按独立安装处理：{}",
        app_dir.display()
    )
}

#[cfg(any(windows, test))]
fn app_dir_is_within_registered_package(app_dir: &Path, locations: &str) -> anyhow::Result<bool> {
    use anyhow::Context;

    for location in locations
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let location = Path::new(location);
        anyhow::ensure!(location.is_absolute(), "Windows 包注册路径不是绝对路径");
        let package_root = std::fs::canonicalize(location)
            .with_context(|| format!("无法核实 Windows 包注册路径：{}", location.display()))?;
        if app_dir.ancestors().any(|ancestor| {
            ancestor
                .as_os_str()
                .eq_ignore_ascii_case(package_root.as_os_str())
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn codex_app_version(app_dir: &Path) -> Option<String> {
    // 应用版本与平台包版本独立，不能从安装目录名或 build 编号推导。
    if app_dir.extension() == Some(OsStr::new("app")) {
        return macos_app_version(app_dir)
            .or_else(|| codex_resources_app_version(&app_dir.join("Contents").join("Resources")));
    }

    codex_resources_app_version(&app_dir.join("resources"))
        .or_else(|| codex_resources_app_version(&app_dir.join("app").join("resources")))
        .or_else(|| codex_executable_product_version(app_dir))
}

const MAX_ASAR_HEADER_BYTES: u32 = 16 * 1024 * 1024;
const MAX_APP_PACKAGE_JSON_BYTES: u64 = 1024 * 1024;

fn codex_resources_app_version(resources_dir: &Path) -> Option<String> {
    asar_app_version(&resources_dir.join("app.asar"))
        .or_else(|| app_package_json_version(&resources_dir.join("app").join("package.json")))
}

fn app_package_json_version(path: &Path) -> Option<String> {
    let package = read_app_package_json(path)?;
    normalize_version_value(package.get("version")?.as_str()?)
}

fn read_app_package_json(path: &Path) -> Option<serde_json::Value> {
    use std::io::Read;

    let mut contents = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_APP_PACKAGE_JSON_BYTES + 1)
        .read_to_end(&mut contents)
        .ok()?;
    if contents.len() as u64 > MAX_APP_PACKAGE_JSON_BYTES {
        return None;
    }
    serde_json::from_slice(&contents).ok()
}

fn asar_app_version(path: &Path) -> Option<String> {
    let package = asar_app_package(path)?;
    normalize_version_value(package.get("version")?.as_str()?)
}

fn asar_app_package(path: &Path) -> Option<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};

    // 只解析根 package.json 的索引，其余文件条目由反序列化器跳过。
    #[derive(serde::Deserialize)]
    struct Header {
        files: RootFiles,
    }

    #[derive(serde::Deserialize)]
    struct RootFiles {
        #[serde(rename = "package.json")]
        package: PackageEntry,
    }

    #[derive(serde::Deserialize)]
    struct PackageEntry {
        size: u64,
        offset: Option<String>,
        #[serde(default)]
        unpacked: bool,
    }

    let mut archive = std::fs::File::open(path).ok()?;
    let archive_size = archive.metadata().ok()?.len();
    let mut prefix = [0u8; 16];
    archive.read_exact(&mut prefix).ok()?;
    let size_pickle_length = u32::from_le_bytes(prefix[0..4].try_into().ok()?);
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().ok()?);
    let payload_size = u32::from_le_bytes(prefix[8..12].try_into().ok()?);
    let json_size = u32::from_le_bytes(prefix[12..16].try_into().ok()?);
    if size_pickle_length != 4
        || header_size.checked_sub(4)? != payload_size
        || !(8..=MAX_ASAR_HEADER_BYTES).contains(&header_size)
        || json_size == 0
        || json_size > header_size - 8
        || header_size - 8 - json_size > 3
    {
        return None;
    }
    let data_start = 8 + u64::from(header_size);
    if data_start > archive_size {
        return None;
    }
    let mut header_json = vec![0u8; json_size as usize];
    archive.read_exact(&mut header_json).ok()?;
    let header: Header = serde_json::from_slice(&header_json).ok()?;
    let package = header.files.package;
    if package.size > MAX_APP_PACKAGE_JSON_BYTES {
        return None;
    }
    if package.unpacked {
        return read_app_package_json(&path.with_extension("asar.unpacked").join("package.json"));
    }

    let offset = package.offset?.parse::<u64>().ok()?;
    let start = data_start.checked_add(offset)?;
    if start.checked_add(package.size)? > archive_size {
        return None;
    }
    archive.seek(SeekFrom::Start(start)).ok()?;
    let mut contents = vec![0u8; package.size as usize];
    archive.read_exact(&mut contents).ok()?;
    serde_json::from_slice(&contents).ok()
}

#[cfg(windows)]
fn codex_executable_product_version(app_dir: &Path) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW};
    use windows::core::PCWSTR;

    let executable = build_codex_executable(app_dir);
    let executable_wide = executable
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let info_size = unsafe { GetFileVersionInfoSizeW(PCWSTR(executable_wide.as_ptr()), None) };
    if info_size == 0 {
        return None;
    }

    let mut info = vec![0u8; info_size as usize];
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(executable_wide.as_ptr()),
            0,
            info_size,
            info.as_mut_ptr().cast(),
        )
        .ok()?;
    }

    for (language, code_page) in windows_version_translations(&info) {
        let sub_block = format!(r"\StringFileInfo\{language:04x}{code_page:04x}\ProductVersion");
        if let Some(version) = query_windows_version_string(&info, &sub_block) {
            return Some(version);
        }
    }

    // Some installers omit the translation table but still use one of the
    // conventional Unicode or Windows-1252 English string tables.
    for sub_block in [
        r"\StringFileInfo\040904b0\ProductVersion",
        r"\StringFileInfo\040904e4\ProductVersion",
    ] {
        if let Some(version) = query_windows_version_string(&info, sub_block) {
            return Some(version);
        }
    }

    None
}

#[cfg(windows)]
fn windows_version_translations(info: &[u8]) -> Vec<(u16, u16)> {
    use std::ffi::c_void;
    use windows::Win32::Storage::FileSystem::VerQueryValueW;
    use windows::core::PCWSTR;

    let sub_block = r"\VarFileInfo\Translation"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut translations = std::ptr::null_mut::<c_void>();
    let mut translations_len = 0u32;
    if !unsafe {
        VerQueryValueW(
            info.as_ptr().cast(),
            PCWSTR(sub_block.as_ptr()),
            &mut translations,
            &mut translations_len,
        )
    }
    .as_bool()
        || translations.is_null()
        || translations_len < 4
    {
        return Vec::new();
    }

    let translations =
        unsafe { std::slice::from_raw_parts(translations.cast::<u8>(), translations_len as usize) };
    translations
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pair| {
            (
                u16::from_le_bytes([pair[0], pair[1]]),
                u16::from_le_bytes([pair[2], pair[3]]),
            )
        })
        .collect()
}

#[cfg(windows)]
fn query_windows_version_string(info: &[u8], sub_block: &str) -> Option<String> {
    use std::ffi::c_void;
    use windows::Win32::Storage::FileSystem::VerQueryValueW;
    use windows::core::PCWSTR;

    let sub_block = sub_block
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut value = std::ptr::null_mut::<c_void>();
    let mut value_len = 0u32;
    if !unsafe {
        VerQueryValueW(
            info.as_ptr().cast(),
            PCWSTR(sub_block.as_ptr()),
            &mut value,
            &mut value_len,
        )
    }
    .as_bool()
        || value.is_null()
        || value_len == 0
    {
        return None;
    }

    let value = (0..value_len as usize)
        .map(|index| unsafe { value.cast::<u16>().add(index).read_unaligned() })
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16(&value)
        .ok()
        .and_then(|value| normalize_version_value(&value))
}

#[cfg(not(windows))]
fn codex_executable_product_version(_app_dir: &Path) -> Option<String> {
    None
}

fn normalize_version_value(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches(['v', 'V']);
    let parts = value.split('.').collect::<Vec<_>>();
    (parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|ch| ch.is_ascii_digit())))
    .then(|| value.to_string())
}

/// 内置 CLI 的候选位置。Codex 更新改动过文件名与目录层级，所以按已知命名逐个
/// 尝试：只认单一路径时，一次改名就等于「找不到内置 CLI」并卡死启动。
/// 首选项与历史行为一致，因此存在同名文件时仍选到原来的那一个。
pub fn codex_runtime_executable_candidates(app_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if app_dir.extension() == Some(OsStr::new("app")) {
        let resources = app_dir.join("Contents").join("Resources");
        candidates.push(resources.join("codex"));
        candidates.push(resources.join("codex-cli"));
        // 新版把 CLI 收进 `codex-cli` 包：`bin/codex` 是包清单声明的入口，并且
        // 与 `codex-code-mode-host` 同级，包装器与宿主校验都依赖这一层布局。
        candidates.push(resources.join("codex-cli").join("bin").join("codex"));
        candidates.push(resources.join("bin").join("codex"));
        candidates.push(app_dir.join("Contents").join("MacOS").join("codex"));
    } else {
        let resources = app_dir.join("resources");
        candidates.push(resources.join("codex.exe"));
        candidates.push(app_dir.join("Resources").join("codex.exe"));
        candidates.push(resources.join("bin").join("codex.exe"));
        candidates.push(resources.join("codex-cli").join("bin").join("codex.exe"));
        candidates.push(resources.join("codex-cli.exe"));
    }
    candidates
}

/// 失败时把尝试过的路径带回调用点。缺少这层信息时，一次改名只会留下
/// 「未找到内置 CLI」，排查还得先去猜 Codex 把文件挪到了哪里。
pub fn codex_runtime_executable_missing(app_dir: &Path) -> String {
    format!(
        "Codex App 内未找到内置 CLI；已尝试：{}",
        codex_runtime_executable_candidates(app_dir)
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    )
}

pub fn codex_runtime_executable(app_dir: &Path) -> Option<PathBuf> {
    let gui_executable = build_codex_executable(app_dir);
    codex_runtime_executable_candidates(app_dir)
        .into_iter()
        .find(|path| path.is_file() && !is_same_file(path, &gui_executable))
}

/// 大小写不敏感的文件系统上，候选 `Contents/MacOS/codex` 会命中同目录下的 GUI 主
/// 二进制 `Codex`。把 GUI 程序当作内置 CLI 会让启动在缺少 code-mode 宿主时报错，
/// 因此按文件身份排除；非 Unix 平台退回忽略大小写的路径比较。
fn is_same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(left), Ok(right)) = (std::fs::metadata(left), std::fs::metadata(right)) {
            return left.dev() == right.dev() && left.ino() == right.ino();
        }
    }
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

pub fn packaged_app_user_model_id(app_dir: &Path) -> Option<String> {
    let package_name = packaged_app_full_name(app_dir)?;
    let (spec, _, publisher_id) = codex_package_parts(&package_name)?;
    if publisher_id.is_empty() {
        return None;
    }
    Some(format!("{}_{publisher_id}!{}", spec.identity, spec.app_id))
}

/// 校验、Store 更新和激活共用包身份，非标准目录名由当前用户的注册信息确认。
pub fn packaged_app_full_name(app_dir: &Path) -> Option<String> {
    if let Some(name) = package_name_from_app_dir(app_dir) {
        return Some(name);
    }
    #[cfg(windows)]
    if has_windows_package_marker(app_dir) {
        return registered_package_full_name(app_dir, &registered_codex_packages()?);
    }
    None
}

#[cfg(any(windows, test))]
fn has_windows_package_marker(app_dir: &Path) -> bool {
    app_dir
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("WindowsApps"))
        || app_dir
            .ancestors()
            .take(3)
            .any(|root| root.join("AppxManifest.xml").try_exists().unwrap_or(true))
}

fn package_name_from_app_dir(app_dir: &Path) -> Option<String> {
    let path = app_dir.to_string_lossy().replace('\\', "/");
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for entry in std::iter::once(&"").chain(CODEX_APP_ENTRY_DIRS.iter()) {
        let suffix = entry
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if parts.len() <= suffix.len() {
            continue;
        }
        let index = parts.len() - suffix.len() - 1;
        if parts[index + 1..]
            .iter()
            .zip(&suffix)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
            && codex_package_parts(parts[index]).is_some()
        {
            return Some(parts[index].to_string());
        }
    }
    None
}

fn macos_app_version(app_dir: &Path) -> Option<String> {
    macos_app_plist_value(app_dir, "CFBundleShortVersionString")
        .and_then(|version| normalize_version_value(&version))
}

fn macos_app_plist_value(app_dir: &Path, key: &str) -> Option<String> {
    let path = app_dir.join("Contents").join("Info.plist");
    if let Ok(plist) = std::fs::read_to_string(&path) {
        return plist_string_value(&plist, key);
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(path)
            .output()
            .ok()?;
        if output.status.success() {
            return String::from_utf8(output.stdout)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
    }
    None
}

fn plist_string_value(plist: &str, key: &str) -> Option<String> {
    let (_, after_key) = plist.split_once(&format!("<key>{key}</key>"))?;
    let (_, after_string_open) = after_key.split_once("<string>")?;
    let (value, _) = after_string_open.split_once("</string>")?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn macos_app_candidates(root: &Path) -> Vec<PathBuf> {
    if root.extension() == Some(OsStr::new("app")) {
        return vec![root.to_path_buf()];
    }
    [
        "Codex.app",
        "OpenAI Codex.app",
        "OpenAI.Codex.app",
        "ChatGPT.app",
    ]
    .into_iter()
    .map(|name| root.join(name))
    .collect()
}

fn version_tuple(path: &Path) -> Option<Vec<u32>> {
    let name = path.file_name()?.to_str()?;
    let (_, version, _) = codex_package_parts(name)?;
    let parts = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if parts.is_empty() { None } else { Some(parts) }
}

pub(crate) fn is_supported_app_executable_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("Codex.exe") || name.eq_ignore_ascii_case("ChatGPT.exe")
}

fn package_spec_from_path(path: &Path) -> Option<AppPackageSpec> {
    let package_name = package_name_from_app_dir(path)?;
    let (spec, _, _) = codex_package_parts(&package_name)?;
    Some(spec)
}

fn compare_app_dir_candidates(left: &Path, right: &Path) -> std::cmp::Ordering {
    app_dir_sort_key(left).cmp(&app_dir_sort_key(right))
}

fn app_dir_sort_key(app_dir: &Path) -> Option<(std::cmp::Reverse<u8>, Vec<u32>)> {
    let spec = package_spec_from_path(app_dir)?;
    let package_name = package_name_from_app_dir(app_dir)?;
    Some((
        std::cmp::Reverse(spec.priority),
        version_tuple(Path::new(&package_name))?,
    ))
}

fn package_entry_dir(package_dir: &Path) -> Option<PathBuf> {
    [package_dir.join("app"), package_dir.to_path_buf()]
        .into_iter()
        .chain(
            CODEX_APP_ENTRY_DIRS[1..]
                .iter()
                .map(|entry| package_dir.join(entry)),
        )
        .find(|dir| executable_in_dir(dir).is_some())
}

fn executable_in_dir(dir: &Path) -> Option<PathBuf> {
    let names = package_spec_from_path(dir)
        .map(|spec| spec.executable_names)
        .unwrap_or(STANDALONE_CODEX_EXECUTABLES);
    for name in names {
        let entry = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .find(|entry| {
                executable_name_matches(&entry.file_name(), name) && entry.path().is_file()
            });
        if let Some(entry) = entry {
            return Some(entry.path());
        }
    }
    None
}

fn executable_name_matches(candidate: &OsStr, expected: &str) -> bool {
    executable_name_matches_with(candidate, expected, EXECUTABLE_NAMES_ARE_CASE_INSENSITIVE)
}

fn executable_name_matches_with(candidate: &OsStr, expected: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        candidate.to_string_lossy().eq_ignore_ascii_case(expected)
    } else {
        candidate == OsStr::new(expected)
    }
}

fn codex_package_parts(package_name: &str) -> Option<(AppPackageSpec, &str, &str)> {
    for spec in APP_PACKAGE_SPECS {
        let Some(rest) = strip_prefix_ignore_ascii_case(package_name, spec.identity) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('_') else {
            continue;
        };
        let Some((version, rest)) = rest.split_once('_') else {
            continue;
        };
        let Some((_, publisher_id)) = rest.rsplit_once("__") else {
            continue;
        };
        return Some((*spec, version, publisher_id));
    }
    None
}

fn supported_codex_package_identity(package_name: &str) -> bool {
    codex_package_parts(package_name).is_some_and(|(_, version, publisher)| {
        publisher == "2p2nqsd0c76g0"
            && version.split('.').count() == 4
            && version.split('.').all(|part| part.parse::<u16>().is_ok())
    })
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = value.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_discovery_skips_incomplete_newer_installations() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp
            .path()
            .join("OpenAI.Codex_1.0.0.0_x64__publisher")
            .join("app");
        let incomplete = temp
            .path()
            .join("OpenAI.Codex_2.0.0.0_x64__publisher")
            .join("app");
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::write(valid.join("Codex.exe"), []).unwrap();
        std::fs::create_dir_all(&incomplete).unwrap();
        // A directory named like an executable is not a runnable installation.
        std::fs::create_dir(incomplete.join("Codex.exe")).unwrap();
        assert_eq!(find_latest_codex_app_dir(temp.path()), Some(valid));
    }

    #[test]
    fn package_discovery_uses_root_launcher_when_app_directory_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("OpenAI.Codex_1.0.0.0_x64__publisher");
        let nested = package.join("app");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_latest_codex_app_dir(temp.path()), None);
        std::fs::write(package.join("Codex.exe"), []).unwrap();
        assert_eq!(find_latest_codex_app_dir(temp.path()), Some(package));
        std::fs::write(nested.join("ChatGPT.exe"), []).unwrap();
        assert_eq!(find_latest_codex_app_dir(temp.path()), Some(nested));
    }

    #[test]
    fn package_discovery_ignores_non_ascii_directory_names() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("中文应用目录")).unwrap();
        assert_eq!(find_latest_codex_app_dir(temp.path()), None);
        assert_eq!(
            strip_prefix_ignore_ascii_case("openai.codex_suffix", "OpenAI.Codex"),
            Some("_suffix")
        );
    }

    #[test]
    fn standalone_discovery_includes_local_programs_codex() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("Programs").join("Codex");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("Codex.exe"), []).unwrap();

        assert_eq!(
            find_standalone_codex_app_dir_from(temp.path()).as_deref(),
            std::fs::canonicalize(&app_dir).ok().as_deref()
        );
    }

    #[test]
    fn product_version_normalization_accepts_desktop_version_format() {
        for (raw, expected) in [
            ("  v26.803.81509  ", "26.803.81509"),
            ("V26.915.31945", "26.915.31945"),
            ("26.903.0", "26.903.0"),
        ] {
            assert_eq!(normalize_version_value(raw).as_deref(), Some(expected));
        }
        for raw in [
            "26.915.4065.0",
            "26.903",
            "9922",
            "Codex 26.803.81509",
            "26..1",
        ] {
            assert_eq!(normalize_version_value(raw), None, "{raw}");
        }
    }

    fn test_asar(package_entry: serde_json::Value, data: &[u8]) -> Vec<u8> {
        let header = serde_json::to_vec(&serde_json::json!({
            "files": { "package.json": package_entry }
        }))
        .unwrap();
        let padded_size = (header.len() + 3) & !3;
        let header_size = padded_size + 8;
        let mut bytes = Vec::new();
        for value in [
            4,
            header_size as u32,
            header_size as u32 - 4,
            header.len() as u32,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&header);
        bytes.resize(8 + header_size, 0);
        bytes.extend_from_slice(data);
        bytes
    }

    fn write_app_asar_metadata(resources: &Path, package: serde_json::Value) {
        let package = serde_json::to_vec(&package).unwrap();
        let mut data = b"other file".to_vec();
        let entry = serde_json::json!({ "size": package.len(), "offset": data.len().to_string() });
        data.extend_from_slice(&package);
        std::fs::create_dir_all(resources).unwrap();
        std::fs::write(resources.join("app.asar"), test_asar(entry, &data)).unwrap();
    }

    fn write_app_asar(resources: &Path, version: &str) {
        write_app_asar_metadata(
            resources,
            serde_json::json!({
                "name": "openai-codex-electron",
                "version": version,
                "codexBuildNumber": "9922"
            }),
        );
    }

    /// The client that replaced `Codex.app` installs as `ChatGPT.app`, so the
    /// installation identity has to come from the bundle and Electron metadata.
    fn write_macos_bundle(bundle: &Path, bundle_id: &str, package: serde_json::Value) {
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::write(contents.join("MacOS").join("ChatGPT"), "desktop").unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<key>CFBundleExecutable</key><string>ChatGPT</string>\
                 <key>CFBundleIdentifier</key><string>{bundle_id}</string>"
            ),
        )
        .unwrap();
        write_app_asar_metadata(&contents.join("Resources"), package);
    }

    fn codex_client_package() -> serde_json::Value {
        serde_json::json!({
            "name": "openai-codex-electron",
            "productName": "Codex",
            "version": "26.924.22138"
        })
    }

    fn write_windows_client(app: &Path) {
        std::fs::create_dir_all(app).unwrap();
        std::fs::write(app.join("Codex.exe"), b"desktop fixture").unwrap();
        write_app_asar_metadata(&app.join("resources"), codex_client_package());
    }

    #[test]
    fn windows_discovery_refuses_multiple_installations() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("Programs/Codex");
        let second = temp.path().join("OpenAI/Codex/bin");
        write_windows_client(&first);
        write_windows_client(&second);
        assert!(find_standalone_codex_app_dir_from(temp.path()).is_none());
        assert!(unique_installation(vec![first.clone(), second]).is_none());
        assert_eq!(
            unique_installation(vec![first.clone(), first.clone()]),
            Some(std::fs::canonicalize(first).unwrap())
        );
        assert!(
            latest_appx_install_location_from_output("C:/Store/stable\nD:/Store/beta").is_none()
        );
        assert_eq!(
            latest_appx_install_location_from_output("C:/Store/stable\nC:/Store/stable"),
            Some("C:/Store/stable".into())
        );
    }

    #[test]
    fn selected_parent_refuses_multiple_nested_installations() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        write_windows_client(&app);
        assert_eq!(normalize_codex_app_path(temp.path()), Some(app.clone()));
        write_windows_client(&temp.path().join("current"));
        assert!(normalize_codex_app_path(temp.path()).is_none());
        assert_eq!(normalize_codex_app_path(&app), Some(app));
    }

    #[cfg(unix)]
    #[test]
    fn selected_parent_accepts_aliases_of_the_same_installation() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        write_windows_client(&app);
        std::os::unix::fs::symlink(&app, temp.path().join("current")).unwrap();
        assert_eq!(normalize_codex_app_path(temp.path()), Some(app));
    }

    #[test]
    fn validates_windows_channels_and_rejects_unknown_package_identity() {
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "Standalone",
            "OpenAI.Codex_26.915.4065.0_x64__2p2nqsd0c76g0/app",
            "OpenAI.CodexBeta_26.915.4065.0_arm64__2p2nqsd0c76g0/app",
        ] {
            let app = temp.path().join(name);
            write_windows_client(&app);
            validate_codex_app_dir(&app).unwrap();
        }
        for name in [
            "OpenAI.Codex_26.915.4065.0_x64__unknown/app",
            "OpenAI.Codex_26.915.99999.0_x64__2p2nqsd0c76g0/app",
            "WindowsApps/Unknown/app",
        ] {
            let app = temp.path().join(name);
            write_windows_client(&app);
            assert!(validate_codex_app_dir(&app).is_err(), "{name}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn standalone_client_with_appx_manifest_is_not_a_registered_package() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Programs/Codex");
        write_windows_client(&app);
        // Standalone distributions can retain the Store manifest even though
        // their executable is flattened out of the manifest's app/ directory.
        std::fs::write(
            app.join("AppxManifest.xml"),
            r#"<Package><Applications><Application Id="App" Executable="app/ChatGPT.exe" /></Applications></Package>"#,
        )
        .unwrap();
        validate_codex_app_dir(&app).unwrap();
        assert!(packaged_app_user_model_id(&app).is_none());
        std::fs::rename(
            app.join("AppxManifest.xml"),
            app.parent().unwrap().join("AppxManifest.xml"),
        )
        .unwrap();
        validate_codex_app_dir(&app).unwrap();
        assert!(packaged_app_user_model_id(&app).is_none());
        std::fs::rename(
            app.parent().unwrap().join("AppxManifest.xml"),
            temp.path().join("AppxManifest.xml"),
        )
        .unwrap();
        validate_codex_app_dir(&app).unwrap();
        assert!(packaged_app_user_model_id(&app).is_none());
    }

    #[test]
    fn registered_package_locations_match_whole_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("UnknownPackage");
        let nested = package.join("app");
        let sibling = temp.path().join("UnknownPackage-copy");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let locations = format!("\r\n{}\r\n", package.display());
        for app in [&package, &nested] {
            let app = std::fs::canonicalize(app).unwrap();
            assert!(app_dir_is_within_registered_package(&app, &locations).unwrap());
            #[cfg(windows)]
            assert!(
                app_dir_is_within_registered_package(&app, &locations.to_ascii_uppercase())
                    .unwrap()
            );
        }
        let sibling = std::fs::canonicalize(sibling).unwrap();
        assert!(!app_dir_is_within_registered_package(&sibling, &locations).unwrap());
        let parent = std::fs::canonicalize(temp.path()).unwrap();
        assert!(!app_dir_is_within_registered_package(&parent, &locations).unwrap());
        assert!(!app_dir_is_within_registered_package(&parent, "\r\n").unwrap());
    }

    #[test]
    fn registered_package_locations_fail_closed_on_unverifiable_paths() {
        let temp = tempfile::tempdir().unwrap();
        let app = std::fs::canonicalize(temp.path()).unwrap();
        assert!(app_dir_is_within_registered_package(&app, "relative/package").is_err());
        let missing = temp.path().join("missing");
        assert!(app_dir_is_within_registered_package(&app, &missing.to_string_lossy()).is_err());
    }

    #[test]
    fn packaged_identity_follows_every_supported_entry_layout() {
        let temp = tempfile::tempdir().unwrap();
        for (index, entry) in ["", "app", "bin", "current", "versions/current"]
            .into_iter()
            .enumerate()
        {
            let package = temp.path().join(format!(
                "WindowsApps/OpenAI.Codex_26.924.{index}.0_x64__2p2nqsd0c76g0"
            ));
            let app = package.join(entry);
            write_windows_client(&app);
            let discovered = normalize_codex_app_path(&package).unwrap();
            validate_codex_app_dir(&discovered)
                .unwrap_or_else(|error| panic!("{entry}: {error:#}"));
            assert_eq!(
                packaged_app_user_model_id(&discovered).as_deref(),
                Some("OpenAI.Codex_2p2nqsd0c76g0!App"),
                "{entry}"
            );
            assert_eq!(
                find_latest_codex_app_dir(package.parent().unwrap()),
                Some(app)
            );
        }
    }

    #[test]
    fn registered_package_output_preserves_identity_and_install_location() {
        let temp = tempfile::tempdir().unwrap();
        let location = temp.path().join("Codex 安装");
        let full_name = "OpenAI.Codex_26.924.22138.0_x64__2p2nqsd0c76g0";
        let output = serde_json::to_vec(&serde_json::json!([{
            "PackageFullName": full_name,
            "InstallLocation": location,
        }]))
        .unwrap();
        let packages = registered_codex_packages_from_output(&output).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].package_full_name, full_name);
        assert_eq!(packages[0].install_location, location);
        assert!(
            registered_codex_packages_from_output(b"[]")
                .unwrap()
                .is_empty()
        );
        for output in [b"".as_slice(), b"null", b"{}", b"not JSON", b"[{}]"] {
            assert!(registered_codex_packages_from_output(output).is_none());
        }
        for (full_name, location) in [
            ("Other.App_26.924.22138.0_x64__2p2nqsd0c76g0", temp.path()),
            ("OpenAI.Codex_26.924.22138.0_x64__unknown", temp.path()),
            (
                "OpenAI.Codex_26.924.99999.0_x64__2p2nqsd0c76g0",
                temp.path(),
            ),
            (full_name, Path::new("relative")),
        ] {
            let output = serde_json::to_vec(&serde_json::json!([{
                "PackageFullName": full_name, "InstallLocation": location,
            }]))
            .unwrap();
            assert!(
                registered_codex_packages_from_output(&output).is_none(),
                "{full_name}"
            );
        }
    }

    #[test]
    fn registered_identity_matches_only_the_exact_normalized_installation() {
        let temp = tempfile::tempdir().unwrap();
        let location = temp.path().join("自定义 Store 目录");
        let app = location.join("app");
        write_windows_client(&app);
        std::fs::write(location.join("AppxManifest.xml"), b"fixture").unwrap();
        assert!(has_windows_package_marker(&app));
        assert!(package_name_from_app_dir(&app).is_none());
        let full_name = "OpenAI.Codex_26.924.22138.0_x64__2p2nqsd0c76g0";
        let mut packages = vec![RegisteredCodexPackage {
            package_full_name: full_name.into(),
            install_location: location.clone(),
        }];
        assert_eq!(
            registered_package_full_name(&app, &packages).as_deref(),
            Some(full_name)
        );
        assert!(registered_package_full_name(&app, &[]).is_none());
        assert!(registered_package_full_name(&app.join("resources"), &packages).is_none());
        let unrelated = temp.path().join("unrelated");
        write_windows_client(&unrelated);
        assert!(registered_package_full_name(&unrelated, &packages).is_none());
        packages.push(RegisteredCodexPackage {
            package_full_name: full_name.into(),
            install_location: location.clone(),
        });
        assert_eq!(
            registered_package_full_name(&app, &packages).as_deref(),
            Some(full_name)
        );
        packages[1].package_full_name = "OpenAI.CodexBeta_26.924.22138.0_x64__2p2nqsd0c76g0".into();
        assert!(registered_package_full_name(&app, &packages).is_none());
        packages.truncate(1);
        packages[0].package_full_name = "OpenAI.Codex_26.924.22138.0_x64__unknown".into();
        assert!(registered_package_full_name(&app, &packages).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn registered_identity_rejects_entry_links_outside_the_package() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("registered");
        std::fs::create_dir(&root).unwrap();
        let external = temp.path().join("external");
        write_windows_client(&external);
        let app = root.join("app");
        std::os::unix::fs::symlink(&external, &app).unwrap();
        let packages = [RegisteredCodexPackage {
            package_full_name: "OpenAI.Codex_26.924.22138.0_x64__2p2nqsd0c76g0".into(),
            install_location: root,
        }];
        assert!(registered_package_full_name(&app, &packages).is_none());
        assert!(registered_package_full_name(&external, &packages).is_none());
    }

    #[test]
    fn unrecognized_packaged_layouts_never_become_standalone_installations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("WindowsApps/unregistered");
        let app = root.join("versions/current");
        write_windows_client(&app);
        std::fs::write(root.join("AppxManifest.xml"), b"fixture").unwrap();
        let error = validate_codex_app_dir(&app).unwrap_err().to_string();
        assert!(error.contains("无法安全识别 Windows 打包安装"), "{error}");
        assert!(error.contains(&std::fs::canonicalize(&app).unwrap().display().to_string()));
        assert!(packaged_app_user_model_id(&app).is_none());
        assert!(
            packaged_app_user_model_id(Path::new(
                r"C:\WindowsApps\Other.App_1.2.3.4_x64__2p2nqsd0c76g0\app"
            ))
            .is_none()
        );
        assert!(
            packaged_app_user_model_id(Path::new(
                r"C:\WindowsApps\OpenAI.Codex_1.2.3.4_x64__2p2nqsd0c76g0\app\resources"
            ))
            .is_none()
        );
    }

    #[test]
    fn rejects_missing_relative_and_corrupt_installations() {
        let temp = tempfile::tempdir().unwrap();
        assert!(validate_codex_app_dir(Path::new("relative/Codex.app")).is_err());
        assert!(validate_codex_app_dir(&temp.path().join("missing")).is_err());
        let app = temp.path().join("Codex");
        write_windows_client(&app);
        write_app_asar_metadata(
            &app.join("resources"),
            serde_json::json!({
                "name": "openai-codex-electron", "version": "broken"
            }),
        );
        assert!(validate_codex_app_dir(&app).is_err());
        let fallback = app.join("resources/app");
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(
            fallback.join("package.json"),
            codex_client_package().to_string(),
        )
        .unwrap();
        std::fs::write(app.join("resources/app.asar"), b"truncated").unwrap();
        assert!(validate_codex_app_dir(&app).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_executable_symlinks_outside_the_installation() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Codex.app");
        write_macos_bundle(&app, "com.openai.codex", codex_client_package());
        let binary = app.join("Contents/MacOS/ChatGPT");
        let external = temp.path().join("unrelated");
        std::fs::write(&external, b"not Codex").unwrap();
        std::fs::remove_file(&binary).unwrap();
        std::os::unix::fs::symlink(external, binary).unwrap();
        assert!(validate_codex_app_dir(&app).is_err());
    }

    #[test]
    fn macos_chatgpt_bundle_is_accepted_as_the_codex_client() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("ChatGPT.app");
        write_macos_bundle(&bundle, "com.openai.codex", codex_client_package());

        assert!(
            validate_codex_app_dir(&bundle).is_ok(),
            "合法的合并版客户端必须通过校验"
        );
        assert_eq!(
            find_macos_codex_app(&[temp.path().to_path_buf()]).as_deref(),
            std::fs::canonicalize(&bundle).ok().as_deref()
        );
    }

    #[test]
    fn macos_bundle_requires_the_codex_bundle_identifier() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("ChatGPT.app");
        write_macos_bundle(&bundle, "com.example.chatgpt", codex_client_package());

        assert!(validate_codex_app_dir(&bundle).is_err());
        assert_eq!(find_macos_codex_app(&[temp.path().to_path_buf()]), None);
    }

    #[test]
    fn electron_metadata_must_identify_the_codex_client() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("ChatGPT.app");
        write_macos_bundle(
            &bundle,
            "com.openai.codex",
            serde_json::json!({ "name": "unrelated-electron-app", "version": "1.2.3" }),
        );

        let error = validate_codex_app_dir(&bundle).unwrap_err();
        assert!(format!("{error:#}").contains("应用元数据"), "{error:#}");
    }

    #[test]
    fn product_name_alone_identifies_the_client() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("ChatGPT.app");
        write_macos_bundle(
            &bundle,
            "com.openai.codex",
            serde_json::json!({
                "name": "renamed-desktop-client",
                "productName": "Codex",
                "version": "26.924.22138"
            }),
        );

        assert!(validate_codex_app_dir(&bundle).is_ok());
    }

    #[test]
    fn macos_discovery_refuses_ambiguous_installations() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["ChatGPT.app", "Codex.app"] {
            write_macos_bundle(
                &temp.path().join(name),
                "com.openai.codex",
                codex_client_package(),
            );
        }

        assert_eq!(find_macos_codex_app(&[temp.path().to_path_buf()]), None);
    }

    #[test]
    fn app_version_uses_application_metadata_instead_of_windows_package_version() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp
            .path()
            .join("OpenAI.Codex_26.915.4065.0_x64__publisher");
        let app_dir = package.join("app");
        write_app_asar(&app_dir.join("resources"), "26.915.31945");
        for path in [&package, &app_dir] {
            assert_eq!(codex_app_version(path).as_deref(), Some("26.915.31945"));
        }
        assert_eq!(version_tuple(&package), Some(vec![26, 915, 4065, 0]));

        let standalone = temp.path().join("Codex");
        write_app_asar(&standalone.join("resources"), "26.915.31945");
        assert_eq!(
            codex_app_version(&standalone).as_deref(),
            Some("26.915.31945")
        );
    }

    #[test]
    fn app_version_does_not_use_package_directory_or_component_version_file() {
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "OpenAI.Codex_26.915.4065.0_x64__publisher",
            "26.915.4065.0",
            "26.915.4065",
        ] {
            let directory = temp.path().join(name);
            let app_dir = directory.join("app");
            std::fs::create_dir_all(&app_dir).unwrap();
            std::fs::write(directory.join("version"), "26.915.4065.0").unwrap();
            std::fs::write(app_dir.join("version"), "40.0.0").unwrap();
            assert_eq!(codex_app_version(&directory), None);
            assert_eq!(codex_app_version(&app_dir), None);
        }
    }

    #[test]
    fn macos_app_version_uses_short_version_and_asar_instead_of_build_number() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("Codex.app");
        let contents = bundle.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        assert_eq!(codex_app_version(&bundle), None);

        let plist = contents.join("Info.plist");
        std::fs::write(&plist, "<key>CFBundleVersion</key><string>9922</string>").unwrap();
        assert_eq!(codex_app_version(&bundle), None);
        write_app_asar(&contents.join("Resources"), "26.915.31945");
        assert_eq!(codex_app_version(&bundle).as_deref(), Some("26.915.31945"));
        std::fs::write(&plist, "<key>CFBundleShortVersionString</key><string>26.908.70816</string><key>CFBundleVersion</key><string>9275</string>").unwrap();
        assert_eq!(codex_app_version(&bundle).as_deref(), Some("26.908.70816"));
    }

    #[test]
    fn app_version_reads_unpacked_package_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("resources");
        let app = resources.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), r#"{"version":"26.908.70816"}"#).unwrap();
        assert_eq!(
            codex_app_version(temp.path()).as_deref(),
            Some("26.908.70816")
        );

        let package = br#"{"version":"26.915.31945","codexBuildNumber":"9922"}"#;
        let unpacked = resources.join("app.asar.unpacked");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::fs::write(unpacked.join("package.json"), package).unwrap();
        let entry = serde_json::json!({ "size": package.len(), "unpacked": true });
        std::fs::write(resources.join("app.asar"), test_asar(entry, &[])).unwrap();
        assert_eq!(
            codex_app_version(temp.path()).as_deref(),
            Some("26.915.31945")
        );
    }

    #[test]
    fn asar_app_version_rejects_invalid_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("app.asar");
        let package = br#"{"version":"26.915.31945"}"#;
        for entry in [
            serde_json::json!({ "size": package.len(), "offset": u64::MAX.to_string() }),
            serde_json::json!({ "size": MAX_APP_PACKAGE_JSON_BYTES + 1, "offset": "0" }),
            serde_json::json!({ "size": package.len() + 1, "offset": "0" }),
            serde_json::json!({ "size": package.len(), "offset": "invalid" }),
        ] {
            std::fs::write(&archive, test_asar(entry, package)).unwrap();
            assert_eq!(asar_app_version(&archive), None);
        }

        let entry = serde_json::json!({ "size": package.len(), "offset": "0" });
        let valid = test_asar(entry, package);
        let mut oversized = valid.clone();
        oversized[4..8].copy_from_slice(&(MAX_ASAR_HEADER_BYTES + 1).to_le_bytes());
        oversized[8..12].copy_from_slice(&(MAX_ASAR_HEADER_BYTES - 3).to_le_bytes());
        for bytes in [
            vec![],
            valid[..15].to_vec(),
            valid[..valid.len() - 1].to_vec(),
            oversized,
        ] {
            std::fs::write(&archive, bytes).unwrap();
            assert_eq!(asar_app_version(&archive), None);
        }
        write_app_asar(temp.path(), "26.915.4065.0");
        assert_eq!(asar_app_version(&archive), None);
    }

    #[test]
    fn selected_executable_requires_a_sibling_codex_launcher() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("Codex-X");
        std::fs::create_dir_all(&app_dir).unwrap();
        let third_party = app_dir.join("codex-x.exe");
        std::fs::write(&third_party, []).unwrap();

        // A third-party launcher must not turn its own directory into the Codex app.
        assert_eq!(install_dir_from_selected_file(&third_party, true), None);
        assert_eq!(
            install_dir_from_selected_file(&third_party, false).as_deref(),
            Some(app_dir.as_path())
        );

        std::fs::write(app_dir.join("Codex.exe"), []).unwrap();
        assert_eq!(
            install_dir_from_selected_file(&third_party, true).as_deref(),
            Some(app_dir.as_path())
        );
    }

    /// 新版把 CLI 收进 `codex-cli` 包，入口是 `bin/codex`。候选必须跟到包内，
    /// 否则一次布局调整就等于「找不到内置 CLI」并卡死启动。
    #[test]
    fn runtime_executable_follows_the_packaged_cli_layout() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("ChatGPT.app");
        let contents = app_dir.join("Contents");
        let packaged = contents.join("Resources").join("codex-cli").join("bin");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<key>CFBundleExecutable</key><string>ChatGPT</string>",
        )
        .unwrap();
        std::fs::write(contents.join("MacOS").join("ChatGPT"), "desktop").unwrap();
        std::fs::write(packaged.join("codex"), "packaged CLI").unwrap();
        std::fs::write(packaged.join("codex-code-mode-host"), "packaged host").unwrap();

        assert_eq!(
            codex_runtime_executable(&app_dir).as_deref(),
            Some(packaged.join("codex").as_path())
        );

        // 历史布局仍在时保持原选择。
        let legacy = contents.join("Resources").join("codex");
        std::fs::write(&legacy, "legacy CLI").unwrap();
        assert_eq!(
            codex_runtime_executable(&app_dir).as_deref(),
            Some(legacy.as_path())
        );
    }

    /// Windows 分支同样要覆盖包内布局；这里只校验候选路径拼接。
    #[test]
    fn runtime_executable_candidates_include_the_packaged_windows_layout() {
        let app_dir = PathBuf::from("Codex");
        assert!(
            codex_runtime_executable_candidates(&app_dir).contains(
                &app_dir
                    .join("resources")
                    .join("codex-cli")
                    .join("bin")
                    .join("codex.exe")
            )
        );
    }

    #[test]
    fn windows_executable_lookup_ignores_letter_case() {
        assert!(executable_name_matches_with(
            OsStr::new("codex.exe"),
            "Codex.exe",
            true
        ));
        assert!(executable_name_matches_with(
            OsStr::new("CHATGPT.EXE"),
            "ChatGPT.exe",
            true
        ));
        assert!(!executable_name_matches_with(
            OsStr::new("codex.exe"),
            "Codex.exe",
            false
        ));
        assert!(!executable_name_matches_with(
            OsStr::new("codex-x.exe"),
            "Codex.exe",
            true
        ));
    }

    /// A third-party Codex launcher installs its own binary beside no Codex
    /// desktop app, so its directory must not resolve to a launchable app.
    #[cfg(windows)]
    #[test]
    fn third_party_launcher_does_not_name_a_codex_install_dir() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("Codex-X");
        std::fs::create_dir_all(&app_dir).unwrap();
        let third_party = app_dir.join("codex-x.exe");
        std::fs::write(&third_party, []).unwrap();

        assert_eq!(normalize_codex_app_path(&third_party), None);
        assert_eq!(normalize_codex_app_path(&app_dir), None);

        std::fs::write(app_dir.join("Codex.exe"), []).unwrap();
        assert_eq!(
            normalize_codex_app_path(&third_party).as_deref(),
            Some(app_dir.as_path())
        );
    }

    /// Windows installs may spell the launcher in lower case; the discovery
    /// must still find it.
    #[cfg(windows)]
    #[test]
    fn lowercase_launcher_name_still_names_its_install_dir() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("codex.exe"), []).unwrap();

        assert_eq!(
            normalize_codex_app_path(temp.path()).as_deref(),
            Some(temp.path())
        );
    }
}
