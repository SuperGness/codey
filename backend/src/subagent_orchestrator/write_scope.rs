//! Write coordination follows complete inputs of known native tools. Arbitrary
//! commands and scripts retain workspace scope; task prose never narrows it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

pub(super) const MAX_WRITE_PATHS: usize = 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WriteScope {
    None,
    Paths(BTreeSet<String>),
    Workspace(Option<String>),
    Unbounded,
}

pub(super) fn write_scope(
    name: &str,
    input: Option<&Value>,
    workspace: Option<&str>,
    tool_workspace: Option<&str>,
) -> WriteScope {
    let command_directory = if matches!(name.trim(), "exec_command" | "functions.exec_command") {
        input.and_then(object_input).and_then(|input| {
            input
                .get("workdir")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    } else {
        None
    };
    let broad = || {
        let different_command_directory = command_directory.as_deref().is_some_and(|directory| {
            workspace.is_none_or(|root| {
                let resolved = fs::canonicalize(directory)
                    .ok()
                    .and_then(|path| path_key(&path));
                let expected = fs::canonicalize(root).ok().and_then(|path| path_key(&path));
                resolved.is_none() || resolved != expected
            })
        });
        if different_command_directory
            || tool_workspace.is_some_and(|cwd| {
                workspace.is_none_or(|root| {
                    super::normalize_coordination_path(cwd).ok()
                        != super::normalize_coordination_path(root).ok()
                })
            })
        {
            WriteScope::Unbounded
        } else {
            WriteScope::Workspace(workspace.map(str::to_owned))
        }
    };
    if !super::tool_requires_workspace_write(name) {
        return WriteScope::None;
    }
    let name = name.trim();
    let patch = match name {
        "apply_patch" | "functions.apply_patch" => input.and_then(patch_input),
        "functions.exec" | "exec" => input.and_then(single_patch_call),
        _ => None,
    };
    if let Some(patch) = patch {
        let Some(paths) = patch_paths(&patch) else {
            return broad();
        };
        return resolve_paths(&paths, tool_workspace).unwrap_or(WriteScope::Unbounded);
    }
    // Only the native FastCtx API has the single-target contract used here.
    if name == "mcp__codey_fastctx__replace" {
        let Some(input) = input.and_then(object_input) else {
            return broad();
        };
        if input.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return WriteScope::None;
        }
        let Some(path) = input.get("path").and_then(Value::as_str) else {
            return broad();
        };
        // Directory replacement may traverse many files, including aliases.
        if !Path::new(path).is_absolute() {
            return broad();
        }
        if Path::new(path).is_dir() {
            let contained = workspace
                .and_then(|root| fs::canonicalize(root).ok())
                .zip(fs::canonicalize(path).ok())
                .is_some_and(|(root, target)| target.starts_with(root));
            return if contained {
                broad()
            } else {
                WriteScope::Unbounded
            };
        }
        return resolve_paths(&[path.to_owned()], tool_workspace).unwrap_or(WriteScope::Unbounded);
    }
    broad()
}

fn object_input(input: &Value) -> Option<Value> {
    let value = if let Some(text) = input.as_str() {
        serde_json::from_str(text).ok()?
    } else {
        input.clone()
    };
    value.is_object().then_some(value)
}

fn patch_input(input: &Value) -> Option<String> {
    if let Some(text) = input.as_str() {
        return Some(text.to_owned());
    }
    let object = input.as_object()?;
    let mut patch = None;
    for key in ["input", "patch"] {
        if let Some(value) = object.get(key) {
            let text = value.as_str()?;
            if patch.is_some_and(|previous| previous != text) {
                return None;
            }
            patch = Some(text);
        }
    }
    patch.map(str::to_owned)
}

fn single_patch_call(input: &Value) -> Option<String> {
    let code = input
        .as_str()
        .or_else(|| input.get("code")?.as_str())?
        .trim();
    let code = if code.starts_with("// @exec:") {
        let (header, body) = code.split_once('\n')?;
        if header.contains(['\u{2028}', '\u{2029}'])
            || !serde_json::from_str::<Value>(header.strip_prefix("// @exec:")?.trim())
                .ok()?
                .is_object()
        {
            return None;
        }
        body.trim()
    } else {
        code
    };
    let code = code.strip_suffix(';').unwrap_or(code).trim();
    let literal = code
        .strip_prefix("text(await tools.apply_patch(")
        .and_then(|code| code.strip_suffix("))"))
        .or_else(|| {
            code.strip_prefix("await tools.apply_patch(")
                .and_then(|code| code.strip_suffix(')'))
        })?;
    // JSON strings exclude interpolation, variables and executable expressions.
    serde_json::from_str(literal.trim()).ok()
}

fn patch_paths(patch: &str) -> Option<Vec<String>> {
    let mut lines = patch.lines();
    if lines.next()? != "*** Begin Patch" {
        return None;
    }
    let mut paths = Vec::new();
    let mut kind = None;
    let mut move_allowed = false;
    for line in lines.by_ref() {
        if line == "*** End Patch" {
            return (!paths.is_empty() && lines.all(|line| line.is_empty())).then_some(paths);
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            paths.push(path.to_owned());
            kind = Some('a');
            move_allowed = false;
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            paths.push(path.to_owned());
            kind = Some('d');
            move_allowed = false;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            paths.push(path.to_owned());
            kind = Some('u');
            move_allowed = true;
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if !move_allowed {
                return None;
            }
            paths.push(path.to_owned());
            move_allowed = false;
        } else {
            move_allowed = false;
            match kind {
                Some('a') if line.starts_with('+') => {}
                Some('u')
                    if line == "@@"
                        || line.starts_with("@@ ")
                        || line == "*** End of File"
                        || line.starts_with([' ', '+', '-'])
                        || line.is_empty() => {}
                _ => return None,
            }
        }
        if paths.len() > MAX_WRITE_PATHS || paths.last().is_some_and(|path| path.is_empty()) {
            return None;
        }
    }
    None
}

fn resolve_paths(paths: &[String], workspace: Option<&str>) -> Option<WriteScope> {
    let workspace = workspace.map(Path::new);
    if workspace.is_some_and(|path| !path.is_absolute()) {
        return None;
    }
    let mut resolved = BTreeSet::new();
    for raw in paths {
        if raw.is_empty() || raw != raw.trim() || raw.contains('\0') || raw.contains("~fastctx~") {
            return None;
        }
        let path = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            workspace?.join(raw)
        };
        let canonical = canonical_target(&path)?;
        // Keep the lexical alias as well, so another task cannot replace an
        // ancestor symlink while this task still owns files reached through it.
        let lexical = path_key(&path)?;
        resolved.insert(lexical.clone());
        resolved.insert(path_key(&canonical)?);
        for ancestor in path.ancestors() {
            if fs::symlink_metadata(ancestor)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                let alias = path_key(ancestor)?;
                if !super::path_is_within(&lexical, &alias) {
                    resolved.insert(alias);
                }
            }
        }
        if resolved.len() > MAX_WRITE_PATHS {
            return None;
        }
    }
    Some(WriteScope::Paths(resolved))
}

fn canonical_target(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(ancestor).ok()?;
                // Also inspect a symlink's resolved target. Multiple hard-link
                // names cannot be coordinated by path alone.
                if fs::metadata(&canonical).ok()?.is_file() && has_multiple_links(&canonical)? {
                    return None;
                }
                for name in missing.iter().rev() {
                    canonical.push(name);
                }
                return Some(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.file_name()?.to_owned());
                ancestor = ancestor.parent()?;
            }
            Err(_) => return None,
        }
    }
}

#[cfg(unix)]
fn has_multiple_links(path: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    Some(fs::metadata(path).ok()?.nlink() > 1)
}

#[cfg(windows)]
fn has_multiple_links(path: &Path) -> Option<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let file = fs::File::open(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the file owns a live handle and info is writable for this call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some(info.nNumberOfLinks > 1)
}

#[cfg(not(any(unix, windows)))]
fn has_multiple_links(_: &Path) -> Option<bool> {
    None
}

pub(super) fn path_key(path: &Path) -> Option<String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if !normalized.pop() => return None,
            Component::ParentDir => {}
            component => normalized.push(component.as_os_str()),
        }
    }
    if !normalized.is_absolute() {
        return None;
    }
    let key = normalized.to_str()?;
    #[cfg(windows)]
    let key = {
        let key = key.replace('\\', "/");
        if let Some(rest) = key.strip_prefix("//?/UNC/") {
            format!("//{rest}")
        } else {
            key.strip_prefix("//?/").unwrap_or(&key).to_owned()
        }
    };
    // Conservatively coordinate differently cased new names on platforms with
    // commonly case-insensitive filesystems, before the file exists.
    #[cfg(windows)]
    let key = key.as_str();
    Some(comparison_key(key))
}

pub(super) fn comparison_key(path: &str) -> String {
    #[cfg(any(windows, target_os = "macos"))]
    let path = path.to_lowercase();
    #[cfg(target_os = "macos")]
    {
        use unicode_normalization::UnicodeNormalization;
        path.nfd().collect()
    }
    #[cfg(not(target_os = "macos"))]
    path.to_string()
}

pub(super) fn valid_path_key(path: &str) -> bool {
    !path.contains('\0')
        && path.len() <= 4_096
        && path_key(Path::new(path)).as_deref() == Some(path)
}
