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
            let app_dir = package_entry_dir(&path, spec)?;
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
        return path.parent().map(Path::to_path_buf);
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
    if app_dir.extension() == Some(OsStr::new("app")) {
        return macos_app_version(app_dir);
    }
    let package_dir = if app_dir
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("app"))
    {
        app_dir.parent()?
    } else {
        app_dir
    };
    codex_package_version(package_dir)
        .or_else(|| codex_directory_version(package_dir))
        .or_else(|| codex_directory_version(app_dir))
        .or_else(|| codex_version_file(package_dir))
        .or_else(|| codex_version_file(app_dir))
        .or_else(|| codex_executable_product_version(app_dir))
}

#[cfg(windows)]
fn codex_executable_product_version(app_dir: &Path) -> Option<String> {
    use std::ffi::c_void;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FFI_SIGNATURE, VS_FIXEDFILEINFO,
        VerQueryValueW,
    };
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

    let mut fixed_info = std::ptr::null_mut::<c_void>();
    let mut fixed_info_len = 0u32;
    let root = ['\\' as u16, 0];
    if !unsafe {
        VerQueryValueW(
            info.as_ptr().cast(),
            PCWSTR(root.as_ptr()),
            &mut fixed_info,
            &mut fixed_info_len,
        )
    }
    .as_bool()
        || fixed_info.is_null()
        || fixed_info_len < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
    {
        return None;
    }

    let fixed_info = unsafe { fixed_info.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
    if fixed_info.dwSignature != VS_FFI_SIGNATURE as u32 {
        return None;
    }
    fixed_windows_product_version(&fixed_info)
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

#[cfg(windows)]
fn fixed_windows_product_version(
    info: &windows::Win32::Storage::FileSystem::VS_FIXEDFILEINFO,
) -> Option<String> {
    let (version_ms, version_ls) = if info.dwProductVersionMS != 0 || info.dwProductVersionLS != 0 {
        (info.dwProductVersionMS, info.dwProductVersionLS)
    } else {
        (info.dwFileVersionMS, info.dwFileVersionLS)
    };
    let mut parts = vec![
        version_ms >> 16,
        version_ms & 0xffff,
        version_ls >> 16,
        version_ls & 0xffff,
    ];
    while parts.len() > 2 && parts.last() == Some(&0) {
        parts.pop();
    }
    let version = parts
        .into_iter()
        .map(|part| part.to_string())
        .collect::<Vec<_>>()
        .join(".");
    normalize_version_value(&version)
}

#[cfg(not(windows))]
fn codex_executable_product_version(_app_dir: &Path) -> Option<String> {
    None
}

#[cfg(any(windows, test))]
fn normalize_version_value(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches(['v', 'V']);
    is_version_like(value).then(|| value.to_string())
}

pub fn codex_runtime_executable(app_dir: &Path) -> Option<PathBuf> {
    let candidates = if app_dir.extension() == Some(OsStr::new("app")) {
        vec![app_dir.join("Contents").join("Resources").join("codex")]
    } else {
        vec![
            app_dir.join("resources").join("codex.exe"),
            app_dir.join("Resources").join("codex.exe"),
        ]
    };
    candidates.into_iter().find(|path| path.is_file())
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

fn codex_package_version(package_dir: &Path) -> Option<String> {
    let path = package_dir.to_string_lossy().replace('\\', "/");
    let name = path
        .split('/')
        .rev()
        .find(|part| codex_package_parts(part).is_some())?;
    let (_, version, _) = codex_package_parts(name)?;
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

fn codex_directory_version(app_dir: &Path) -> Option<String> {
    directory_version(app_dir).or_else(|| {
        app_dir
            .canonicalize()
            .ok()
            .and_then(|path| directory_version(&path))
    })
}

fn directory_version(path: &Path) -> Option<String> {
    let version = path.file_name()?.to_str()?;
    if is_version_like(version) {
        Some(version.to_string())
    } else {
        None
    }
}

fn is_version_like(version: &str) -> bool {
    let mut parts = version.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    if first.is_empty() || !first.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let mut count = 1;
    for part in parts {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

fn codex_version_file(app_dir: &Path) -> Option<String> {
    let version = std::fs::read_to_string(app_dir.join("version")).ok()?;
    let version = version.trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

fn macos_app_version(app_dir: &Path) -> Option<String> {
    macos_app_plist_value(app_dir, "CFBundleShortVersionString")
        .or_else(|| macos_app_plist_value(app_dir, "CFBundleVersion"))
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

fn package_entry_dir(package_dir: &Path, spec: AppPackageSpec) -> Option<PathBuf> {
    let app = package_dir.join("app");
    if app.is_dir() {
        return Some(app);
    }
    for name in spec.executable_names {
        if package_dir.join(name).is_file() {
            return Some(package_dir.to_path_buf());
        }
    }
    None
}

fn executable_in_dir(dir: &Path) -> Option<PathBuf> {
    let names = package_spec_from_path(dir)
        .map(|spec| spec.executable_names)
        .unwrap_or(STANDALONE_CODEX_EXECUTABLES);
    for name in names {
        let entry = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .find(|entry| entry.file_name() == OsStr::new(name) && entry.path().is_file());
        if let Some(entry) = entry {
            return Some(entry.path());
        }
    }
    None
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
    if value.len() < prefix.len() {
        return None;
    }
    let (head, rest) = value.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            normalize_version_value("  v26.803.81509  ").as_deref(),
            Some("26.803.81509")
        );
        assert_eq!(normalize_version_value("Codex 26.803.81509"), None);
    }
}
