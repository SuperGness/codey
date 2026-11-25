use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(any(windows, test))]
mod windows_packages;

#[derive(Default)]
struct AppValidationContext {
    #[cfg(windows)]
    packages: windows_packages::NativeWindowsPackageSnapshot,
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

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
    AppValidationContext::default()
        .packages
        .codex_packages()
        .ok()?
        .iter()
        .map(|package| normalize_codex_app_path(&package.install_location))
        .collect()
}

#[cfg(any(windows, test))]
struct RegisteredCodexPackage {
    package_full_name: String,
    install_location: PathBuf,
}

#[cfg(any(windows, test))]
fn validate_registered_codex_packages(
    packages: Vec<RegisteredCodexPackage>,
) -> Option<Vec<RegisteredCodexPackage>> {
    packages
        .iter()
        .all(|package| {
            supported_codex_package_identity(&package.package_full_name)
                && package.install_location.is_absolute()
        })
        .then_some(packages)
}

#[cfg(windows)]
fn windows_package_location(
    location: windows::core::Result<windows::core::HSTRING>,
) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::core::HRESULT;

    let location = match location {
        Ok(location) => location,
        // Removed packages can retain registrations without an installed folder.
        Err(error) if error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
            return Ok(None);
        }
        Err(error) => return Err(error).context("无法读取 Windows 包注册路径"),
    };
    if location.is_empty() {
        return Ok(None);
    }
    let location = PathBuf::from(std::ffi::OsString::from_wide(location.as_wide()));
    anyhow::ensure!(location.is_absolute(), "Windows 包注册路径不是绝对路径");
    Ok(Some(location))
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
            // The operation's package snapshot already verified and canonicalized
            // this root; share that result with standalone classification.
            let root = &package.install_location;
            let entry = normalize_codex_app_path(root)?;
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
        resolve_codex_app_dir_with_saved_checked(None, None)
            .ok()
            .flatten()
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
    resolve_codex_app_dir_with_saved_checked(app_dir, saved_app_path)
        .ok()
        .flatten()
}

pub fn resolve_codex_app_dir_with_saved_checked(
    app_dir: Option<&Path>,
    saved_app_path: Option<&str>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(app_dir) = app_dir {
        return normalize_codex_app_path_checked(app_dir).map(Some);
    }
    if let Some(saved) = saved_app_path
        .map(str::trim)
        .filter(|saved| !saved.is_empty())
    {
        return normalize_codex_app_path_checked(Path::new(saved)).map(Some);
    }
    #[cfg(windows)]
    {
        use anyhow::Context;
        let context = AppValidationContext::default();
        let mut required = context
            .packages
            .codex_packages()
            .context("无法查询或解析 Windows Codex 包注册信息，请明确选择应用路径")?
            .iter()
            .map(|package| normalize_codex_app_path_checked(&package.install_location))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let (mut roots, mut diagnostics) = standalone_registry_roots();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            roots.extend([
                local.join("Programs/Codex"),
                local.join("OpenAI/Codex/bin"),
                local.join("OpenAI/Codex"),
                local.join("Programs/OpenAI/Codex"),
            ]);
        }
        required.extend(collect_automatic_app_roots(roots)?);
        let mut optional = Vec::new();
        add_optional_discovery_source(
            &mut optional,
            &mut diagnostics,
            "当前会话进程",
            running_codex_app_dirs(),
        );
        let paths = validate_automatic_app_candidates(
            required
                .into_iter()
                .map(|path| (path, true))
                .chain(optional.into_iter().map(|path| (path, false))),
            |path| validate_codex_app_dir_with_context(path, &context),
        )?;
        finish_automatic_discovery(paths, diagnostics)
    }
    #[cfg(not(windows))]
    Ok(resolve_codex_app_dir(None))
}

/// Resolves an installation and validates its identity exactly once. Explicit
/// selections never fall back to another installation if validation fails.
pub fn resolve_validated_codex_app_dir_with_saved_checked(
    app_dir: Option<&Path>,
    saved_app_path: Option<&str>,
) -> anyhow::Result<Option<PathBuf>> {
    let selected = app_dir.or_else(|| {
        saved_app_path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(Path::new)
    });
    if let Some(selected) = selected {
        let app = normalize_codex_app_path_checked(selected)?;
        validate_codex_app_dir(&app)?;
        return Ok(Some(app));
    }
    let app = resolve_codex_app_dir_with_saved_checked(None, None)?;
    // Windows discovery and macOS bundle discovery already validate candidates.
    #[cfg(not(any(windows, target_os = "macos")))]
    if let Some(app) = &app {
        validate_codex_app_dir(app)?;
    }
    Ok(app)
}

#[cfg(any(windows, test))]
fn validate_automatic_app_candidates(
    candidates: impl IntoIterator<Item = (PathBuf, bool)>,
    mut validate: impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<Vec<PathBuf>> {
    use anyhow::Context;
    let mut unique = Vec::<(PathBuf, bool)>::new();
    for (path, required) in candidates {
        let canonical = match std::fs::canonicalize(&path) {
            Ok(canonical) => canonical,
            Err(error) if required => {
                return Err(error)
                    .with_context(|| format!("无法访问 Codex 候选目录：{}", path.display()));
            }
            Err(_) => continue,
        };
        let existing = unique.iter_mut().find(|(known, _)| {
            if cfg!(windows) {
                known
                    .as_os_str()
                    .eq_ignore_ascii_case(canonical.as_os_str())
            } else {
                known == &canonical
            }
        });
        if let Some((_, known_required)) = existing {
            *known_required |= required;
        } else {
            unique.push((canonical, required));
        }
    }
    let mut accepted = Vec::new();
    for (path, required) in unique {
        match validate(&path) {
            Ok(()) => accepted.push(path),
            Err(error) if required => return Err(error),
            Err(_) => {}
        }
    }
    Ok(accepted)
}

// Automatic roots can be data-only directories. Keep absence separate from
// unreadable directories, incomplete installations and ambiguous candidates.
#[cfg(any(windows, test))]
fn collect_automatic_app_roots(mut roots: Vec<PathBuf>) -> anyhow::Result<Vec<PathBuf>> {
    use anyhow::Context;
    roots.sort();
    roots.dedup();
    let mut paths = Vec::new();
    for root in roots {
        let metadata = match std::fs::metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法访问 Codex 安装目录：{}", root.display()));
            }
        };
        let app = if metadata.is_dir() {
            let Some(app) = app_candidate_from_directory(&root)? else {
                continue;
            };
            app
        } else {
            normalize_codex_app_path_checked(&root)?
        };
        paths.push(app);
    }
    Ok(paths)
}

#[cfg(any(windows, test))]
fn add_optional_discovery_source(
    paths: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<String>,
    source: &str,
    result: anyhow::Result<Vec<PathBuf>>,
) {
    match result {
        Ok(found) => paths.extend(found),
        Err(error) => diagnostics.push(format!("{source}：{error:#}")),
    }
}

#[cfg(any(windows, test))]
fn finish_automatic_discovery(
    paths: Vec<PathBuf>,
    diagnostics: Vec<String>,
) -> anyhow::Result<Option<PathBuf>> {
    if !diagnostics.is_empty() {
        let _ = crate::diagnostic_log::append_diagnostic_log(
            "codex_app_discovery_source_failed",
            serde_json::json!({"diagnostics": diagnostics}),
        );
    }
    if paths.is_empty() {
        anyhow::ensure!(
            diagnostics.is_empty(),
            "未找到可用的 Codex 安装，部分来源查询失败：{}",
            diagnostics.join("；")
        );
        return Ok(None);
    }
    let display = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("；");
    unique_installation(paths)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("发现多个 Codex 安装，请明确选择应用路径：{display}"))
}

#[cfg(windows)]
fn running_codex_app_dirs() -> anyhow::Result<Vec<PathBuf>> {
    use crate::windows_integration::{enumerate_processes, process_session_id};
    let session = process_session_id(std::process::id())
        .ok_or_else(|| anyhow::anyhow!("无法确定当前 Windows 会话"))?;
    let mut images = Vec::new();
    for process in enumerate_processes()? {
        if process.session_id != Some(session)
            || !is_supported_app_executable_name(&process.exe_file)
        {
            continue;
        }
        if let Some(image) = process.executable_path {
            images.push(image);
        }
    }
    images.sort();
    images.dedup();
    // Names only identify candidates. Identity validation happens after all
    // sources have been combined and canonical directories deduplicated.
    Ok(images
        .iter()
        .filter_map(|image| normalize_codex_app_path_checked(image).ok())
        .collect())
}

#[cfg(windows)]
fn standalone_registry_roots() -> (Vec<PathBuf>, Vec<String>) {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::PCWSTR;
    let mut paths = Vec::new();
    let mut diagnostics = Vec::new();
    // Read fixed application registrations, never execute registry command lines.
    // Metadata validation below decides whether a registration is actually Codex.
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for key in [
            r"Software\Microsoft\Windows\CurrentVersion\App Paths\Codex.exe",
            r"Software\Microsoft\Windows\CurrentVersion\App Paths\ChatGPT.exe",
            r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths\Codex.exe",
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex",
        ] {
            let key_wide = key.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let value = if key.ends_with(r"Uninstall\Codex") {
                "InstallLocation"
            } else {
                ""
            };
            let value_wide = value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let mut buffer = vec![0u16; 32768];
            let mut size = (buffer.len() * 2) as u32;
            let status = unsafe {
                RegGetValueW(
                    hive,
                    PCWSTR(key_wide.as_ptr()),
                    PCWSTR(value_wide.as_ptr()),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    Some(&mut size),
                )
            };
            if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
                continue;
            }
            if status != ERROR_SUCCESS {
                diagnostics.push(format!(
                    "无法读取 Codex 安装注册信息：{key}，错误码 {}",
                    status.0
                ));
                continue;
            }
            let end = buffer
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(buffer.len());
            let value = match String::from_utf16(&buffer[..end]) {
                Ok(value) => value,
                Err(error) => {
                    diagnostics.push(format!("Codex 安装注册信息编码无效：{key}，{error}"));
                    continue;
                }
            };
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    diagnostics.push(format!(
                        "Codex 安装注册路径不是绝对路径：{}",
                        path.display()
                    ));
                    continue;
                }
                // ChatGPT has its own desktop product. Only Codex-identified
                // locations should be a discovery source for that generic name.
                if key.ends_with("ChatGPT.exe")
                    && !path
                        .components()
                        .any(|part| part.as_os_str().eq_ignore_ascii_case("Codex"))
                {
                    continue;
                }
                paths.push(path);
            }
        }
    }
    (paths, diagnostics)
}

pub fn normalize_codex_app_path(path: &Path) -> Option<PathBuf> {
    normalize_codex_app_path_checked(path).ok()
}

/// Resolves only known installer layouts, preserving filesystem and ambiguity errors.
/// Identity validation is deliberately separate so damaged applications stay visible.
pub fn normalize_codex_app_path_checked(path: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    anyhow::ensure!(!path.as_os_str().is_empty(), "Codex 安装路径为空");
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Codex 路径不存在或无法访问：{}", path.display()))?;
    if path.extension() == Some(OsStr::new("app")) && metadata.is_dir() {
        return Ok(path.to_path_buf());
    }
    if metadata.is_file() {
        let parent = path.parent().context("Codex 程序没有父目录")?;
        let supported = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(is_supported_app_executable_name);
        anyhow::ensure!(
            supported
                || !SELECTED_FILE_REQUIRES_SIBLING_EXECUTABLE
                || executable_in_dir(parent).is_some(),
            "所选文件不是 Codex 桌面启动程序：{}",
            path.display()
        );
        // A selected executable names its own app, even when another complete
        // install exists below it. Never replace a damaged explicit selection.
        if supported && resources_present(parent)? {
            return Ok(parent.to_path_buf());
        }
        return normalize_codex_app_path_checked(parent);
    }
    anyhow::ensure!(
        metadata.is_dir(),
        "Codex 安装路径不是目录：{}",
        path.display()
    );
    if let Some(candidate) = app_candidate_from_directory(path)? {
        return Ok(candidate);
    }
    #[cfg(not(windows))]
    return Ok(path.to_path_buf());
    #[cfg(windows)]
    anyhow::bail!("目录内没有 Codex 桌面启动程序：{}", path.display());
}

fn app_candidate_from_directory(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;
    let mut candidates = Vec::new();
    let root_launcher = executable_in_dir_checked(path)?.is_some();
    let root_resources = resources_present(path)?;
    if root_launcher && root_resources {
        candidates.push(path.to_path_buf());
    }
    for entry in bounded_app_entry_dirs(path)? {
        if executable_in_dir_checked(&entry)?.is_some() {
            candidates.push(entry);
        }
    }
    if candidates.is_empty() && root_launcher {
        candidates.push(path.to_path_buf());
    }
    // A resource-free root launcher is the only excluded candidate. Nested
    // executables lacking resources remain candidates and cannot disappear.
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut unique = Vec::<(PathBuf, PathBuf)>::new();
    for candidate in candidates {
        let canonical = std::fs::canonicalize(&candidate)
            .with_context(|| format!("无法访问 Codex 候选目录：{}", candidate.display()))?;
        let root = std::fs::canonicalize(path)?;
        anyhow::ensure!(
            canonical.starts_with(&root),
            "Codex 候选目录超出所选安装目录：{}",
            candidate.display()
        );
        if !unique.iter().any(|(known, _)| known == &canonical) {
            unique.push((canonical, candidate));
        }
    }
    anyhow::ensure!(
        unique.len() == 1,
        "发现多个 Codex 候选目录，请明确选择程序文件；缺少资源的候选也需检查：{}",
        unique
            .iter()
            .map(|(_, path)| path.display().to_string())
            .collect::<Vec<_>>()
            .join("；")
    );
    Ok(Some(unique.remove(0).1))
}

fn resources_present(path: &Path) -> anyhow::Result<bool> {
    use anyhow::Context;
    let resources = path.join("resources");
    // The directory itself counts: removing metadata must not silently redirect
    // an explicitly selected damaged installation to another version.
    resources
        .try_exists()
        .with_context(|| format!("无法检查 Codex 应用资源：{}", resources.display()))
}

fn executable_in_dir_checked(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let mut executable = None;
            for entry in std::fs::read_dir(path)
                .with_context(|| format!("无法读取 Codex 候选目录：{}", path.display()))?
            {
                let entry = entry
                    .with_context(|| format!("无法读取 Codex 候选目录条目：{}", path.display()))?;
                if STANDALONE_CODEX_EXECUTABLES
                    .iter()
                    .any(|name| executable_name_matches(&entry.file_name(), name))
                {
                    let metadata = std::fs::metadata(entry.path()).with_context(|| {
                        format!("无法读取 Codex 启动程序：{}", entry.path().display())
                    })?;
                    if metadata.is_file() && executable.is_none() {
                        executable = Some(entry.path());
                    }
                }
            }
            Ok(executable)
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("无法访问 Codex 候选目录：{}", path.display()))
        }
    }
}

fn bounded_app_entry_dirs(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    use anyhow::Context;
    let mut paths = CODEX_APP_ENTRY_DIRS
        .iter()
        .map(|entry| root.join(entry))
        .collect::<Vec<_>>();
    // Only inspect version-shaped children of the install root and versions/.
    // No recursive walk and no version sorting to choose an arbitrary install.
    for parent in [root.to_path_buf(), root.join("versions")] {
        let entries = match std::fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法扫描 Codex 安装目录：{}", parent.display()));
            }
        };
        for (index, entry) in entries.enumerate() {
            anyhow::ensure!(
                index < 256,
                "Codex 安装目录条目过多，请直接选择程序文件：{}",
                parent.display()
            );
            let entry = entry
                .with_context(|| format!("无法读取 Codex 安装目录条目：{}", parent.display()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let version = name.strip_prefix("app-").unwrap_or(&name);
            if version.split('.').count() >= 2
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            {
                paths.push(entry.path());
                paths.push(entry.path().join("app"));
            }
        }
    }
    Ok(paths)
}

/// Names the install directory a user-selected file belongs to.
///
/// A file only names a Codex install when a launchable launcher sits beside it.
/// Guessing the parent of an unrelated binary (a CLI build or a third-party
/// Codex launcher such as `codex-x.exe`) would hand the launcher a path it can
/// never start, and the failure would only surface as a spawn error.
#[cfg(test)]
fn install_dir_from_selected_file(
    path: &Path,
    require_sibling_executable: bool,
) -> Option<PathBuf> {
    let parent = path.parent()?;
    if !require_sibling_executable || executable_in_dir(parent).is_some() {
        return normalize_codex_app_path(parent);
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
    validate_codex_app_dir_with_context(app_dir, &AppValidationContext::default())
}

fn validate_codex_app_dir_with_context(
    app_dir: &Path,
    context: &AppValidationContext,
) -> anyhow::Result<()> {
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
    let package = if archive
        .try_exists()
        .with_context(|| format!("无法检查 Codex 应用归档：{}", archive.display()))?
    {
        asar_app_package(&archive)
    } else {
        read_app_package_json(&resources.join("app/package.json"))
    }
    .with_context(|| {
        format!(
            "无法读取 Codex 应用身份，安装可能不完整或版本不受支持；安装目录：{}",
            root.display()
        )
    })?;
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
    } else if let Some(package_name) = packaged_app_full_name_with_context(&root, context)? {
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
                ensure_unregistered_windows_install(&root, context)?;
                break;
            }
        }
    }
    Ok(())
}

fn ensure_unregistered_windows_install(
    app_dir: &Path,
    _context: &AppValidationContext,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use anyhow::Context;

        // Include all identities: an unrecognized registered package must not
        // fall back to standalone launch just because its name is unfamiliar.
        let locations = _context
            .packages
            .all_install_locations()
            .context("无法查询 Windows 包注册信息，未按独立安装处理")?;
        anyhow::ensure!(
            !app_dir_is_within_canonical_package(app_dir, locations),
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

#[cfg(test)]
fn app_dir_is_within_registered_package(
    app_dir: &Path,
    locations: &[PathBuf],
) -> anyhow::Result<bool> {
    use anyhow::Context;

    for location in locations {
        anyhow::ensure!(location.is_absolute(), "Windows 包注册路径不是绝对路径");
        let package_root = std::fs::canonicalize(location)
            .with_context(|| format!("无法核实 Windows 包注册路径：{}", location.display()))?;
        if app_dir_is_within_canonical_package(app_dir, &[package_root]) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(any(windows, test))]
fn app_dir_is_within_canonical_package(app_dir: &Path, locations: &[PathBuf]) -> bool {
    locations.iter().any(|root| {
        app_dir
            .ancestors()
            .any(|ancestor| ancestor.as_os_str().eq_ignore_ascii_case(root.as_os_str()))
    })
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
    let package = read_app_package_json(path).ok()?;
    normalize_version_value(package.get("version")?.as_str()?)
}

fn read_app_package_json(path: &Path) -> anyhow::Result<serde_json::Value> {
    use anyhow::Context;
    use std::io::Read;

    let mut contents = Vec::new();
    std::fs::File::open(path)
        .with_context(|| format!("无法打开应用元数据：{}", path.display()))?
        .take(MAX_APP_PACKAGE_JSON_BYTES + 1)
        .read_to_end(&mut contents)
        .with_context(|| format!("无法读取应用元数据：{}", path.display()))?;
    anyhow::ensure!(
        contents.len() as u64 <= MAX_APP_PACKAGE_JSON_BYTES,
        "应用元数据超过大小上限 {MAX_APP_PACKAGE_JSON_BYTES} 字节：{}",
        path.display()
    );
    serde_json::from_slice(&contents)
        .with_context(|| format!("无法解析应用元数据 JSON：{}", path.display()))
}

fn asar_app_version(path: &Path) -> Option<String> {
    let package = asar_app_package(path).ok()?;
    normalize_version_value(package.get("version")?.as_str()?)
}

fn asar_app_package(path: &Path) -> anyhow::Result<serde_json::Value> {
    use anyhow::Context;
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

    let read_package = || -> anyhow::Result<serde_json::Value> {
        let mut archive = std::fs::File::open(path).context("无法打开 ASAR 文件")?;
        let archive_size = archive.metadata().context("无法读取 ASAR 文件大小")?.len();
        let mut prefix = [0u8; 16];
        archive
            .read_exact(&mut prefix)
            .context("无法读取 ASAR 长度信息，归档可能被截断")?;
        let size_pickle_length = u32::from_le_bytes(prefix[0..4].try_into()?);
        let header_size = u32::from_le_bytes(prefix[4..8].try_into()?);
        let payload_size = u32::from_le_bytes(prefix[8..12].try_into()?);
        let json_size = u32::from_le_bytes(prefix[12..16].try_into()?);
        anyhow::ensure!(
            size_pickle_length == 4,
            "ASAR 长度信息无效：size_pickle_length={size_pickle_length}，应为 4"
        );
        anyhow::ensure!(
            (8..=MAX_ASAR_HEADER_BYTES).contains(&header_size),
            "ASAR 索引长度无效：{header_size}，允许范围为 8..={MAX_ASAR_HEADER_BYTES} 字节"
        );
        anyhow::ensure!(
            header_size - 4 == payload_size,
            "ASAR 索引长度不一致：header_size={header_size}，payload_size={payload_size}"
        );
        anyhow::ensure!(
            json_size > 0 && json_size <= header_size - 8 && header_size - 8 - json_size <= 3,
            "ASAR 索引 JSON 长度或填充无效：json_size={json_size}，header_size={header_size}"
        );
        let data_start = 8 + u64::from(header_size);
        anyhow::ensure!(
            data_start <= archive_size,
            "ASAR 索引超出归档长度，归档可能被截断：索引结束位置 {data_start}，归档长度 {archive_size}"
        );
        let mut header_json = vec![0u8; json_size as usize];
        archive
            .read_exact(&mut header_json)
            .context("无法读取 ASAR 索引 JSON")?;
        let header: Header = serde_json::from_slice(&header_json)
            .context("无法解析 ASAR 根 package.json 索引 JSON")?;
        let package = header.files.package;
        anyhow::ensure!(
            package.size <= MAX_APP_PACKAGE_JSON_BYTES,
            "ASAR package.json 超过大小上限 {MAX_APP_PACKAGE_JSON_BYTES} 字节：{}",
            package.size
        );
        if package.unpacked {
            return read_app_package_json(
                &path.with_extension("asar.unpacked").join("package.json"),
            );
        }

        let offset = package
            .offset
            .context("ASAR package.json 索引缺少 offset")?;
        let offset = offset
            .parse::<u64>()
            .with_context(|| format!("ASAR package.json offset 无效：{offset}"))?;
        let start = data_start
            .checked_add(offset)
            .context("ASAR package.json offset 导致起始位置溢出")?;
        let end = start
            .checked_add(package.size)
            .context("ASAR package.json 大小导致结束位置溢出")?;
        anyhow::ensure!(
            end <= archive_size,
            "ASAR package.json 超出归档长度，归档可能被截断：offset={offset}，size={}，归档长度 {archive_size}",
            package.size
        );
        archive
            .seek(SeekFrom::Start(start))
            .context("无法定位 ASAR package.json")?;
        let mut contents = vec![0u8; package.size as usize];
        archive
            .read_exact(&mut contents)
            .context("无法读取 ASAR package.json")?;
        serde_json::from_slice(&contents).context("无法解析 ASAR package.json JSON")
    };
    read_package().with_context(|| format!("无法读取 Codex 应用归档：{}", path.display()))
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

/// 内置 CLI 的候选位置。优先匹配当前 `codex-cli/bin/codex` 包布局，再回退到旧布局。
pub fn codex_runtime_executable_candidates(app_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if app_dir.extension() == Some(OsStr::new("app")) {
        let resources = app_dir.join("Contents").join("Resources");
        // 当前版本把 CLI 收进 `codex-cli` 包：`bin/codex` 是包清单声明的入口，并且
        // 与 `codex-code-mode-host` 同级，包装器与宿主校验都依赖这一层布局。
        candidates.push(resources.join("codex-cli").join("bin").join("codex"));
        candidates.push(resources.join("codex"));
        candidates.push(resources.join("codex-cli"));
        candidates.push(resources.join("bin").join("codex"));
        candidates.push(app_dir.join("Contents").join("MacOS").join("codex"));
    } else {
        let resources = app_dir.join("resources");
        candidates.push(resources.join("codex-cli").join("bin").join("codex.exe"));
        candidates.push(resources.join("codex.exe"));
        candidates.push(app_dir.join("Resources").join("codex.exe"));
        candidates.push(resources.join("bin").join("codex.exe"));
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
    packaged_app_full_name_with_context(app_dir, &AppValidationContext::default())
        .ok()
        .flatten()
}

fn packaged_app_full_name_with_context(
    app_dir: &Path,
    _context: &AppValidationContext,
) -> anyhow::Result<Option<String>> {
    if let Some(name) = package_name_from_app_dir(app_dir) {
        return Ok(Some(name));
    }
    #[cfg(windows)]
    if has_windows_package_marker(app_dir) {
        return Ok(registered_package_full_name(
            app_dir,
            _context.packages.codex_packages()?,
        ));
    }
    Ok(None)
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
    fn installer_launcher_resolves_to_the_electron_app_for_all_path_sources() {
        for entry in CODEX_APP_ENTRY_DIRS {
            for launcher in STANDALONE_CODEX_EXECUTABLES {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("Programs/Codex");
                let app = root.join(entry);
                write_windows_client(&app);
                std::fs::rename(app.join("Codex.exe"), app.join("ChatGPT.exe")).unwrap();
                let selected_exe = root.join(launcher);
                std::fs::write(&selected_exe, b"installer launcher").unwrap();
                let cli = app.join("resources/codex.exe");
                std::fs::write(&cli, b"CLI fixture").unwrap();

                for selected in [&root, &selected_exe, &app, &app.join("ChatGPT.exe")] {
                    let resolved = resolve_codex_app_dir(Some(selected)).unwrap();
                    assert_eq!(resolved, app);
                    validate_codex_app_dir(&resolved).unwrap();
                    assert_eq!(build_codex_executable(&resolved), app.join("ChatGPT.exe"));
                    assert_eq!(codex_runtime_executable(&resolved), Some(cli.clone()));
                    assert_eq!(
                        resolve_codex_app_dir_with_saved(None, selected.to_str()),
                        Some(app.clone())
                    );
                }
                assert_eq!(
                    find_standalone_codex_app_dir_from(temp.path()),
                    Some(std::fs::canonicalize(&app).unwrap())
                );
            }
        }
    }

    #[test]
    fn installer_layout_supports_unpacked_metadata_and_preserves_flat_installations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_windows_client(root);
        assert_eq!(normalize_codex_app_path(root), Some(root.to_path_buf()));
        let nested = root.join("app");
        std::fs::create_dir_all(nested.join("resources/app")).unwrap();
        std::fs::write(nested.join("ChatGPT.exe"), b"desktop fixture").unwrap();
        std::fs::write(
            nested.join("resources/app/package.json"),
            serde_json::to_vec(&codex_client_package()).unwrap(),
        )
        .unwrap();
        // A directory is ambiguous, but an explicit executable selects its app.
        assert!(normalize_codex_app_path(root).is_none());
        assert_eq!(
            normalize_codex_app_path(&root.join("Codex.exe")),
            Some(root.to_path_buf())
        );
        // Startup forwards the exact executable to model discovery, preserving
        // selection even though an already resolved directory has nested apps.
        let resolved = normalize_codex_app_path_checked(&root.join("Codex.exe")).unwrap();
        let catalog_selection = build_codex_executable(&resolved);
        assert_eq!(
            normalize_codex_app_path_checked(&catalog_selection).unwrap(),
            resolved
        );
        std::fs::remove_dir_all(root.join("resources")).unwrap();
        assert_eq!(normalize_codex_app_path(root), Some(nested.clone()));
        validate_codex_app_dir(&nested).unwrap();
    }

    #[test]
    fn nested_layout_does_not_skip_invalid_application_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("Codex.exe"), b"installer launcher").unwrap();
        let app = root.join("app");
        write_windows_client(&app);
        write_app_asar_metadata(
            &app.join("resources"),
            serde_json::json!({"name": "unrelated-client", "version": "1.0.0"}),
        );
        let resolved = normalize_codex_app_path(root).unwrap();
        assert_eq!(resolved, app);
        assert!(validate_codex_app_dir(&resolved).is_err());
        // A damaged root archive still counts as a separate layout. It must
        // not be hidden by the valid app below it.
        write_windows_client(&app);
        std::fs::create_dir_all(root.join("resources")).unwrap();
        std::fs::write(root.join("resources/app.asar"), b"invalid archive").unwrap();
        assert!(normalize_codex_app_path(root).is_none());
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
    fn automatic_roots_skip_data_directories_and_missing_installations() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Programs/Codex/app");
        write_windows_client(&app);
        let data = temp.path().join("OpenAI/Codex");
        std::fs::create_dir_all(data.join("logs")).unwrap();
        let paths = collect_automatic_app_roots(vec![
            data,
            temp.path().join("Programs/Codex"),
            temp.path().join("missing"),
        ])
        .unwrap();
        assert_eq!(paths, vec![app]);
    }

    #[test]
    fn automatic_roots_support_versioned_bin_installations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("OpenAI/Codex");
        let bin = root.join("bin");
        let app = bin.join("versions/1.2.3/app");
        write_windows_client(&app);
        let paths = collect_automatic_app_roots(vec![root, bin]).unwrap();
        assert_eq!(paths, vec![app]);
    }

    #[test]
    fn automatic_roots_do_not_hide_incomplete_or_ambiguous_installations() {
        let temp = tempfile::tempdir().unwrap();
        let healthy = temp.path().join("healthy");
        let broken = temp.path().join("broken");
        write_windows_client(&healthy);
        std::fs::create_dir(&broken).unwrap();
        std::fs::write(broken.join("Codex.exe"), b"incomplete install").unwrap();
        let roots = collect_automatic_app_roots(vec![healthy.clone(), broken.clone()]).unwrap();
        assert!(
            validate_automatic_app_candidates(
                roots.into_iter().map(|path| (path, true)),
                validate_codex_app_dir
            )
            .is_err()
        );
        write_windows_client(&broken);
        write_windows_client(&broken.join("app"));
        assert!(collect_automatic_app_roots(vec![healthy, broken]).is_err());
    }

    #[test]
    fn automatic_discovery_validates_one_install_for_nine_processes_and_fixed_root() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Codex");
        write_windows_client(&app);
        let mut candidates = vec![(app.clone(), true)];
        candidates.extend((0..9).map(|_| (app.clone(), false)));
        let mut validations = 0;
        let paths = validate_automatic_app_candidates(candidates, |path| {
            validations += 1;
            validate_codex_app_dir(path)
        })
        .unwrap();
        assert_eq!(validations, 1);
        assert_eq!(paths, vec![std::fs::canonicalize(&app).unwrap()]);
        assert_eq!(
            finish_automatic_discovery(paths, vec![]).unwrap(),
            Some(std::fs::canonicalize(&app).unwrap())
        );
    }

    #[test]
    fn required_source_wins_over_duplicate_optional_source_in_either_order() {
        let temp = tempfile::tempdir().unwrap();
        for required_first in [true, false] {
            let mut validations = 0;
            let candidates = [
                (temp.path().to_path_buf(), required_first),
                (temp.path().to_path_buf(), !required_first),
            ];
            let error = validate_automatic_app_candidates(candidates, |_| {
                validations += 1;
                anyhow::bail!("damaged required installation")
            })
            .unwrap_err();
            assert!(error.to_string().contains("damaged required installation"));
            assert_eq!(validations, 1);
        }
    }

    #[test]
    fn optional_invalid_processes_do_not_hide_required_errors_or_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good");
        let other = temp.path().join("other");
        let bad = temp.path().join("bad");
        write_windows_client(&good);
        write_windows_client(&other);
        std::fs::create_dir(&bad).unwrap();
        let paths = validate_automatic_app_candidates(
            [(bad.clone(), false), (good.clone(), true)],
            validate_codex_app_dir,
        )
        .unwrap();
        assert_eq!(paths.len(), 1);
        assert!(
            validate_automatic_app_candidates(
                [(good.clone(), false), (bad, true)],
                validate_codex_app_dir
            )
            .is_err()
        );
        let paths = validate_automatic_app_candidates(
            [(good, true), (other, false)],
            validate_codex_app_dir,
        )
        .unwrap();
        assert!(
            finish_automatic_discovery(paths, vec![])
                .unwrap_err()
                .to_string()
                .contains("多个 Codex 安装")
        );
        let missing = temp.path().join("missing");
        assert!(
            validate_automatic_app_candidates([(missing.clone(), false)], validate_codex_app_dir)
                .unwrap()
                .is_empty()
        );
        assert!(
            validate_automatic_app_candidates([(missing, true)], validate_codex_app_dir).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_discovery_deduplicates_canonical_installation_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Codex");
        let alias = temp.path().join("alias");
        write_windows_client(&app);
        std::os::unix::fs::symlink(&app, &alias).unwrap();
        let mut validations = 0;
        let paths = validate_automatic_app_candidates([(app, true), (alias, false)], |path| {
            validations += 1;
            validate_codex_app_dir(path)
        })
        .unwrap();
        assert_eq!(validations, 1);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn validated_resolver_keeps_explicit_invalid_selection_and_saved_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good");
        let damaged = temp.path().join("damaged");
        write_windows_client(&good);
        write_windows_client(&damaged);
        std::fs::remove_file(damaged.join("resources/app.asar")).unwrap();
        assert!(
            resolve_validated_codex_app_dir_with_saved_checked(Some(&damaged), good.to_str())
                .is_err()
        );
        assert!(
            resolve_validated_codex_app_dir_with_saved_checked(None, damaged.to_str()).is_err()
        );
        assert_eq!(
            resolve_validated_codex_app_dir_with_saved_checked(Some(&good), damaged.to_str())
                .unwrap(),
            Some(good)
        );
    }

    #[test]
    fn optional_source_failures_preserve_verified_installations_and_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        write_windows_client(&app);
        let mut paths = vec![app.clone()];
        let mut diagnostics = Vec::new();
        add_optional_discovery_source(
            &mut paths,
            &mut diagnostics,
            "当前会话进程",
            Err(anyhow::anyhow!("session unavailable")),
        );
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("session unavailable"));
        assert_eq!(
            finish_automatic_discovery(paths, diagnostics.clone()).unwrap(),
            Some(std::fs::canonicalize(&app).unwrap())
        );
        assert!(finish_automatic_discovery(Vec::new(), diagnostics.clone()).is_err());
        let other = temp.path().join("other");
        write_windows_client(&other);
        assert!(finish_automatic_discovery(vec![app, other], diagnostics).is_err());
    }

    #[test]
    fn selected_parent_refuses_multiple_nested_installations() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Codex.exe"), b"installer launcher").unwrap();
        let app = temp.path().join("app");
        write_windows_client(&app);
        assert_eq!(normalize_codex_app_path(temp.path()), Some(app.clone()));
        write_windows_client(&temp.path().join("current"));
        assert!(normalize_codex_app_path(temp.path()).is_none());
        assert_eq!(normalize_codex_app_path(&app), Some(app));
    }

    #[test]
    fn incomplete_nested_candidate_is_reported_instead_of_hidden() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        write_windows_client(&app);
        let broken = temp.path().join("current");
        std::fs::create_dir(&broken).unwrap();
        std::fs::write(broken.join("Codex.exe"), b"incomplete install").unwrap();
        let error = normalize_codex_app_path_checked(temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("多个 Codex 候选"), "{error}");
        assert!(error.contains(&app.display().to_string()), "{error}");
        assert!(error.contains(&broken.display().to_string()), "{error}");
    }

    #[test]
    fn explicit_damaged_install_does_not_switch_to_nested_app() {
        let temp = tempfile::tempdir().unwrap();
        write_windows_client(temp.path());
        write_windows_client(&temp.path().join("app"));
        std::fs::remove_file(temp.path().join("resources/app.asar")).unwrap();
        let selected = temp.path().join("Codex.exe");
        let app = normalize_codex_app_path_checked(&selected).unwrap();
        assert_eq!(app, temp.path());
        assert!(validate_codex_app_dir(&app).is_err());
        assert!(
            resolve_codex_app_dir_with_saved_checked(
                None,
                temp.path().join("missing.exe").to_str()
            )
            .is_err()
        );
    }

    #[test]
    fn version_directory_scan_is_bounded_and_does_not_choose_latest() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("versions/1.2.3/app");
        write_windows_client(&first);
        assert_eq!(normalize_codex_app_path(temp.path()), Some(first));
        let second = temp.path().join("app-9.8.7");
        write_windows_client(&second);
        assert!(
            normalize_codex_app_path_checked(temp.path())
                .unwrap_err()
                .to_string()
                .contains("多个 Codex 候选")
        );
        assert_eq!(
            normalize_codex_app_path(&second.join("Codex.exe")),
            Some(second)
        );
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
        let locations = [package.clone()];
        for app in [&package, &nested] {
            let app = std::fs::canonicalize(app).unwrap();
            assert!(app_dir_is_within_registered_package(&app, &locations).unwrap());
            #[cfg(windows)]
            assert!(
                app_dir_is_within_registered_package(
                    &app,
                    &[PathBuf::from(package.as_os_str().to_ascii_uppercase())],
                )
                .unwrap()
            );
        }
        let sibling = std::fs::canonicalize(sibling).unwrap();
        assert!(!app_dir_is_within_registered_package(&sibling, &locations).unwrap());
        let parent = std::fs::canonicalize(temp.path()).unwrap();
        assert!(!app_dir_is_within_registered_package(&parent, &locations).unwrap());
        assert!(!app_dir_is_within_registered_package(&parent, &[]).unwrap());
    }

    #[test]
    fn registered_package_locations_fail_closed_on_unverifiable_paths() {
        let temp = tempfile::tempdir().unwrap();
        let app = std::fs::canonicalize(temp.path()).unwrap();
        assert!(app_dir_is_within_registered_package(&app, &["relative/package".into()]).is_err());
        let missing = temp.path().join("missing");
        assert!(app_dir_is_within_registered_package(&app, &[missing]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn native_package_locations_skip_only_missing_or_empty_installations() {
        use windows::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND,
        };
        use windows::core::{Error, HRESULT, HSTRING};

        assert!(
            windows_package_location(Ok(HSTRING::new()))
                .unwrap()
                .is_none()
        );
        let missing = Error::from_hresult(HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0));
        assert!(windows_package_location(Err(missing)).unwrap().is_none());
        for code in [ERROR_ACCESS_DENIED, ERROR_PATH_NOT_FOUND] {
            let error = Error::from_hresult(HRESULT::from_win32(code.0));
            assert!(windows_package_location(Err(error)).is_err());
        }
        assert!(windows_package_location(Ok(HSTRING::from("relative/package"))).is_err());
        let location = r"C:\应用 包\Codex";
        assert_eq!(
            windows_package_location(Ok(HSTRING::from(location))).unwrap(),
            Some(PathBuf::from(location)),
        );
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
    fn registered_package_records_preserve_identity_and_install_location() {
        let temp = tempfile::tempdir().unwrap();
        let location = temp.path().join("Codex 安装");
        let full_name = "OpenAI.Codex_26.924.22138.0_x64__2p2nqsd0c76g0";
        let packages = validate_registered_codex_packages(vec![RegisteredCodexPackage {
            package_full_name: full_name.into(),
            install_location: location.clone(),
        }])
        .unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].package_full_name, full_name);
        assert_eq!(packages[0].install_location, location);
        assert!(
            validate_registered_codex_packages(vec![])
                .unwrap()
                .is_empty()
        );
        for (full_name, location) in [
            ("Other.App_26.924.22138.0_x64__2p2nqsd0c76g0", temp.path()),
            ("OpenAI.Codex_26.924.22138.0_x64__unknown", temp.path()),
            (
                "OpenAI.Codex_26.924.99999.0_x64__2p2nqsd0c76g0",
                temp.path(),
            ),
            (full_name, Path::new("relative")),
        ] {
            assert!(
                validate_registered_codex_packages(vec![RegisteredCodexPackage {
                    package_full_name: full_name.into(),
                    install_location: location.into(),
                }])
                .is_none(),
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
            install_location: std::fs::canonicalize(&location).unwrap(),
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
            install_location: std::fs::canonicalize(&location).unwrap(),
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
            install_location: std::fs::canonicalize(root).unwrap(),
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

    #[test]
    fn identity_read_errors_preserve_installation_path_and_json_failure() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Codex");
        write_windows_client(&app);
        std::fs::remove_file(app.join("resources/app.asar")).unwrap();
        let root = std::fs::canonicalize(&app).unwrap();
        let metadata = root.join("resources/app/package.json");
        let missing = validate_codex_app_dir(&app).unwrap_err();
        let diagnostic = format!("{missing:#}");
        assert!(
            diagnostic.contains(&format!("安装目录：{}", root.display())),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("无法打开应用元数据：{}", metadata.display())),
            "{diagnostic}"
        );
        assert_eq!(
            missing.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );

        std::fs::create_dir_all(metadata.parent().unwrap()).unwrap();
        std::fs::write(&metadata, b"{invalid").unwrap();
        let error = validate_codex_app_dir(&app).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains(&format!("无法解析应用元数据 JSON：{}", metadata.display())),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("line 1 column"), "{diagnostic}");
        assert!(error.downcast_ref::<serde_json::Error>().is_some());
    }

    #[test]
    fn identity_validation_falls_back_only_when_the_archive_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Codex");
        write_windows_client(&app);
        let archive = app.join("resources/app.asar");
        let fallback = app.join("resources/app/package.json");
        std::fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        std::fs::write(&fallback, codex_client_package().to_string()).unwrap();
        std::fs::write(&archive, b"truncated").unwrap();
        let error = validate_codex_app_dir(&app).unwrap_err();
        let diagnostic = format!("{error:#}");
        let archive = std::fs::canonicalize(archive).unwrap();
        assert!(
            diagnostic.contains(&archive.display().to_string()),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("无法读取 ASAR 长度信息"),
            "{diagnostic}"
        );
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        std::fs::remove_file(archive).unwrap();
        validate_codex_app_dir(&app).unwrap();
    }

    #[test]
    fn asar_read_errors_preserve_archive_path_and_specific_failure() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("app.asar");
        let package = br#"{"version":"26.915.31945"}"#;
        let valid = test_asar(
            serde_json::json!({ "size": package.len(), "offset": "0" }),
            package,
        );
        let mut invalid_length = valid.clone();
        invalid_length[0..4].copy_from_slice(&0u32.to_le_bytes());
        let mut invalid_index_json = valid.clone();
        invalid_index_json[16] = b'!';
        for (bytes, reason) in [
            (invalid_length, "ASAR 长度信息无效"),
            (valid[..16].to_vec(), "ASAR 索引超出归档长度"),
            (
                invalid_index_json,
                "无法解析 ASAR 根 package.json 索引 JSON",
            ),
            (
                test_asar(serde_json::json!({}), &[]),
                "missing field `size`",
            ),
            (
                test_asar(serde_json::json!({ "size": 0 }), &[]),
                "索引缺少 offset",
            ),
            (
                test_asar(serde_json::json!({ "size": 0, "offset": "invalid" }), &[]),
                "offset 无效：invalid",
            ),
            (
                test_asar(
                    serde_json::json!({ "size": 0, "offset": u64::MAX.to_string() }),
                    &[],
                ),
                "起始位置溢出",
            ),
            (
                test_asar(
                    serde_json::json!({ "size": MAX_APP_PACKAGE_JSON_BYTES + 1, "offset": "0" }),
                    &[],
                ),
                "超过大小上限",
            ),
            (
                valid[..valid.len() - 1].to_vec(),
                "ASAR package.json 超出归档长度",
            ),
            (
                test_asar(serde_json::json!({ "size": 1, "offset": "0" }), b"!"),
                "无法解析 ASAR package.json JSON",
            ),
        ] {
            std::fs::write(&archive, bytes).unwrap();
            let error = asar_app_package(&archive).unwrap_err();
            let diagnostic = format!("{error:#}");
            assert!(
                diagnostic.contains(&archive.display().to_string()),
                "{diagnostic}"
            );
            assert!(diagnostic.contains(reason), "{diagnostic}");
        }
    }

    #[test]
    fn unpacked_package_read_errors_preserve_both_paths_and_size_limit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("app.asar");
        let unpacked = temp.path().join("app.asar.unpacked").join("package.json");
        let entry = serde_json::json!({ "size": 1, "unpacked": true });
        std::fs::write(&archive, test_asar(entry, &[])).unwrap();
        let missing = asar_app_package(&archive).unwrap_err();
        let diagnostic = format!("{missing:#}");
        for path in [&archive, &unpacked] {
            assert!(
                diagnostic.contains(&path.display().to_string()),
                "{diagnostic}"
            );
        }
        assert!(diagnostic.contains("无法打开应用元数据"), "{diagnostic}");
        assert_eq!(
            missing.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );

        std::fs::create_dir_all(unpacked.parent().unwrap()).unwrap();
        std::fs::write(
            &unpacked,
            vec![b' '; MAX_APP_PACKAGE_JSON_BYTES as usize + 1],
        )
        .unwrap();
        let error = asar_app_package(&archive).unwrap_err();
        let diagnostic = format!("{error:#}");
        for path in [&archive, &unpacked] {
            assert!(
                diagnostic.contains(&path.display().to_string()),
                "{diagnostic}"
            );
        }
        assert!(
            diagnostic.contains("应用元数据超过大小上限"),
            "{diagnostic}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn archive_existence_errors_preserve_the_archive_path() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Codex");
        write_windows_client(&app);
        let archive = std::fs::canonicalize(&app)
            .unwrap()
            .join("resources/app.asar");
        std::fs::remove_file(&archive).unwrap();
        std::os::unix::fs::symlink("app.asar", &archive).unwrap();
        let error = validate_codex_app_dir(&app).unwrap_err();
        assert!(
            format!("{error:#}")
                .contains(&format!("无法检查 Codex 应用归档：{}", archive.display())),
            "{error:#}"
        );
        assert!(error.downcast_ref::<std::io::Error>().is_some());
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
        // Windows keeps rejecting the directory on its own: even when the
        // selected-file sibling check is relaxed, the directory candidate
        // still has no Codex launcher to start.
        let expected = if cfg!(windows) {
            None
        } else {
            Some(app_dir.as_path())
        };
        assert_eq!(
            install_dir_from_selected_file(&third_party, false).as_deref(),
            expected
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

        // 同时存在旧布局时优先命中当前包内布局。
        let legacy = contents.join("Resources").join("codex");
        std::fs::write(&legacy, "legacy CLI").unwrap();
        assert_eq!(
            codex_runtime_executable(&app_dir).as_deref(),
            Some(packaged.join("codex").as_path())
        );

        // 包内布局缺失时回退到旧布局。
        std::fs::remove_file(packaged.join("codex")).unwrap();
        assert_eq!(
            codex_runtime_executable(&app_dir).as_deref(),
            Some(legacy.as_path())
        );
    }

    /// Windows 分支同样优先检查包内布局；这里只校验候选路径顺序。
    #[test]
    fn runtime_executable_candidates_include_the_packaged_windows_layout() {
        let app_dir = PathBuf::from("Codex");
        assert_eq!(
            codex_runtime_executable_candidates(&app_dir).first(),
            Some(
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
