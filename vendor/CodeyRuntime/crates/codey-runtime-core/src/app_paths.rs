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
            .or_else(|| find_latest_codex_app_dir_from_roots(&windows_app_package_roots()))
    }

    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn find_latest_codex_app_dir_from_appx_package() -> Option<PathBuf> {
    let output = Command::new("powershell")
        .creation_flags(crate::windows_create_no_window())
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "$names=@('OpenAI.Codex','OpenAI.CodexBeta'); Get-AppxPackage | Where-Object { $names -contains $_.Name } | Sort-Object Version -Descending | Select-Object -First 1 -ExpandProperty InstallLocation",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    latest_appx_install_location_from_output(&String::from_utf8_lossy(&output.stdout))
        .and_then(|location| normalize_codex_app_path(Path::new(&location)))
}

pub fn latest_appx_install_location_from_output(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

#[cfg(windows)]
fn windows_app_package_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        roots.push(PathBuf::from(program_files).join("WindowsApps"));
    }
    if let Some(program_files) = std::env::var_os("ProgramW6432") {
        roots.push(PathBuf::from(program_files).join("WindowsApps"));
    }
    roots.push(PathBuf::from(r"C:\Program Files\WindowsApps"));
    roots.sort();
    roots.dedup();
    roots
}

pub fn find_macos_codex_app(search_roots: &[PathBuf]) -> Option<PathBuf> {
    for root in search_roots {
        for candidate in macos_app_candidates(root) {
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    None
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
    // Windows: try MS Store version first, then standalone install
    find_latest_codex_app_dir_default().or_else(find_standalone_codex_app_dir)
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
    let candidates: &[PathBuf] = &[
        local_appdata.join("Programs").join("Codex"),
        local_appdata.join("OpenAI").join("Codex").join("bin"),
        local_appdata.join("OpenAI").join("Codex"),
        local_appdata.join("Programs").join("OpenAI").join("Codex"),
    ];

    for candidate in candidates {
        if let Some(path) = normalize_codex_app_path(candidate)
            && build_codex_executable(&path).exists()
        {
            return Some(path);
        }
    }
    None
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
        return Some(path.to_path_buf());
    }

    if path.is_file() {
        return install_dir_from_selected_file(path, SELECTED_FILE_REQUIRES_SIBLING_EXECUTABLE);
    }

    if executable_in_dir(path).is_some() {
        return Some(path.to_path_buf());
    }

    let nested = [
        path.join("app"),
        path.join("bin"),
        path.join("current"),
        path.join("versions").join("current"),
    ]
    .into_iter()
    .find(|nested| executable_in_dir(nested).is_some());
    if nested.is_some() {
        return nested;
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
    use std::io::Read;

    let mut contents = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_APP_PACKAGE_JSON_BYTES + 1)
        .read_to_end(&mut contents)
        .ok()?;
    package_json_version(&contents)
}

fn package_json_version(contents: &[u8]) -> Option<String> {
    if contents.len() as u64 > MAX_APP_PACKAGE_JSON_BYTES {
        return None;
    }
    let package: serde_json::Value = serde_json::from_slice(contents).ok()?;
    normalize_version_value(package.get("version")?.as_str()?)
}

fn asar_app_version(path: &Path) -> Option<String> {
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
        return app_package_json_version(
            &path.with_extension("asar.unpacked").join("package.json"),
        );
    }

    let offset = package.offset?.parse::<u64>().ok()?;
    let start = data_start.checked_add(offset)?;
    if start.checked_add(package.size)? > archive_size {
        return None;
    }
    archive.seek(SeekFrom::Start(start)).ok()?;
    let mut contents = vec![0u8; package.size as usize];
    archive.read_exact(&mut contents).ok()?;
    package_json_version(&contents)
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
    let package_name = package_name_from_app_dir(app_dir)?;
    let (spec, _, publisher_id) = codex_package_parts(&package_name)?;
    if publisher_id.is_empty() {
        return None;
    }
    Some(format!("{}_{publisher_id}!{}", spec.identity, spec.app_id))
}

fn package_name_from_app_dir(app_dir: &Path) -> Option<String> {
    let path = app_dir.to_string_lossy().replace('\\', "/");
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let mut package_name = parts.next_back()?;
    if package_name.eq_ignore_ascii_case("app") {
        package_name = parts.next_back()?;
    }
    Some(package_name.to_string())
}

fn macos_app_version(app_dir: &Path) -> Option<String> {
    macos_app_plist_value(app_dir, "CFBundleShortVersionString")
        .and_then(|version| normalize_version_value(&version))
}

fn macos_app_plist_value(app_dir: &Path, key: &str) -> Option<String> {
    let plist = std::fs::read_to_string(app_dir.join("Contents").join("Info.plist")).ok()?;
    plist_string_value(&plist, key)
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
    let package_dir = if app_dir
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("app"))
    {
        app_dir.parent().unwrap_or(app_dir)
    } else {
        app_dir
    };
    Some((
        std::cmp::Reverse(spec.priority),
        version_tuple(package_dir)?,
    ))
}

fn package_entry_dir(package_dir: &Path) -> Option<PathBuf> {
    [package_dir.join("app"), package_dir.to_path_buf()]
        .into_iter()
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
            Some(app_dir.as_path())
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

    fn write_app_asar(resources: &Path, version: &str) {
        let package = serde_json::to_vec(&serde_json::json!({
            "name": "openai-codex-electron",
            "version": version,
            "codexBuildNumber": "9922"
        }))
        .unwrap();
        let mut data = b"other file".to_vec();
        let entry = serde_json::json!({ "size": package.len(), "offset": data.len().to_string() });
        data.extend_from_slice(&package);
        std::fs::create_dir_all(resources).unwrap();
        std::fs::write(resources.join("app.asar"), test_asar(entry, &data)).unwrap();
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
