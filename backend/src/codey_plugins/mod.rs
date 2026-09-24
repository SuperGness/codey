//! Codey 原生插件平台。安装不执行代码，用户显式启用后加载可信动态库。
pub mod lifecycle;
mod logs;
mod provider;

#[allow(unused_imports)]
pub(crate) use provider::{PluginRouteSpec, RouteChange, set_route_handler};
mod native;
mod package;

use native::Native;
pub use package::Inspection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    pub abi_version: u32,
    pub platform: String,
    pub arch: String,
    pub entry: String,
    pub library_sha256: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub header_names: Vec<String>,
    #[serde(default)]
    pub response_header_names: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub lifecycle_failure_policy: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub lifecycle_max_wait_ms: Option<u64>,
}

const CONFIG_FILE: &str = "config.json";
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFile {
    pub plugin_id: String,
    pub version: String,
    pub path: PathBuf,
    pub content: String,
    pub sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    manifest: Manifest,
    enabled: bool,
    directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    load_error: Option<String>,
    /// 最近一次由这个插件登记的线路。用户删掉线路后，启动时不再补回，直到再次启用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route_profile_id: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    #[serde(default)]
    plugins: BTreeMap<String, Record>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub config_path: PathBuf,
    pub capabilities: Vec<String>,
    pub last_error: Option<String>,
    pub restart_required: bool,
    pub active_version: Option<String>,
    pub plugin_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub log_size_bytes: Option<u64>,
}

#[derive(Serialize)]
pub struct PluginList {
    pub plugins: Vec<PluginInfo>,
    pub platform: String,
    pub arch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderPatch {
    pub name: String,
    pub value: Option<String>,
}

struct Manager {
    root: PathBuf,
    state: State,
    live: BTreeMap<String, Active>,
    errors: BTreeMap<String, String>,
    stopping: bool,
    /// 正在执行原生初始化的插件。此期间拒绝同一插件的安装、停用、卸载和配置保存。
    loading: HashSet<String>,
    generations: BTreeMap<String, Vec<Weak<()>>>,
}

#[derive(Clone)]
struct Active {
    instance: Arc<Mutex<Native>>,
    manifest: Manifest,
    config: Value,
    lifecycle: Arc<lifecycle::LifecyclePlugin>,
}

struct PreparedLoad {
    id: String,
    directory: String,
    manifest: Manifest,
    config: Value,
    library_path: PathBuf,
    context: codey_plugin_sdk::PluginContext,
}

static MANAGER: OnceLock<Mutex<Manager>> = OnceLock::new();

pub fn initialize(root: PathBuf) -> Result<(), String> {
    if let Some(manager) = MANAGER.get() {
        return if manager.lock().map_err(|_| "插件管理锁已损坏")?.root == checked_directory(&root)?
        {
            Ok(())
        } else {
            Err("插件管理器已在其他目录初始化".into())
        };
    }
    let mut manager = Manager::open(root)?;
    manager.load_enabled()?;
    manager.update_fast_path();
    MANAGER
        .set(Mutex::new(manager))
        .map_err(|_| "插件管理器并发初始化".to_string())
}

fn manager() -> Result<std::sync::MutexGuard<'static, Manager>, String> {
    MANAGER
        .get()
        .ok_or("插件管理器尚未初始化")?
        .lock()
        .map_err(|_| "插件管理锁已损坏".into())
}

pub fn list() -> Result<PluginList, String> {
    Ok(manager()?.list())
}
pub fn get_config_file(id: &str) -> Result<ConfigFile, String> {
    manager()?.get_config_file(id)
}
pub fn plugin_directory(id: &str) -> Result<PathBuf, String> {
    manager()?.plugin_directory(id)
}
pub fn clear_logs(id: &str) -> Result<PluginList, String> {
    let manager = manager()?;
    logs::clear(&manager.plugin_directory(id)?)?;
    Ok(manager.list())
}
/// 停止接受新调用；已取得实例引用的请求完成后销毁实例。库映射保留至进程退出。
pub fn shutdown() {
    let live = if let Ok(mut m) = manager() {
        m.stopping = true;
        for id in m.live.keys() {
            m.log_event(id, "shutdown");
        }
        m.update_fast_path();
        std::mem::take(&mut m.live)
    } else {
        return;
    };
    drop(live);
}
pub fn inspect(path: &Path) -> Result<Inspection, String> {
    Ok(package::read(path)?.inspection)
}
pub fn install(path: &Path, sha256: &str) -> Result<PluginList, String> {
    let package = package::read(path)?;
    if package.inspection.sha256 != sha256 {
        return Err("插件包在检查后发生变化，请重新检查".into());
    }
    let mut manager = manager()?;
    if manager.stopping {
        return Err("插件管理器正在关闭".into());
    }
    manager.install(package)?;
    Ok(manager.list())
}

pub fn set_enabled(id: &str, enabled: bool) -> Result<PluginList, String> {
    if enabled {
        enable_plugin(id)
    } else {
        disable_plugin(id)
    }
}

/// 一次启用的占位。只在不再持有管理锁时析构；锁损坏时仍能清除占位，
/// 避免插件一直处于「正在启用」。提前 release 后不会清掉后来的另一次启用。
struct LoadingReservation {
    id: String,
    armed: bool,
}

impl LoadingReservation {
    fn release(&mut self, manager: &mut Manager) {
        if self.armed {
            manager.loading.remove(&self.id);
            self.armed = false;
        }
    }
}

impl Drop for LoadingReservation {
    fn drop(&mut self) {
        if self.armed {
            clear_loading_reservation(&self.id);
        }
    }
}

fn clear_loading_reservation(id: &str) {
    if let Some(mutex) = MANAGER.get() {
        clear_loading_in(mutex, id);
    }
}

fn clear_loading_in(mutex: &Mutex<Manager>, id: &str) {
    let mut manager = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    manager.loading.remove(id);
}

fn disable_plugin(id: &str) -> Result<PluginList, String> {
    let mut manager = manager()?;
    if manager.stopping {
        return Err("插件管理器正在关闭".into());
    }
    if !manager.state.plugins.contains_key(id) {
        return Err("插件未安装".into());
    }
    manager.reject_if_loading(id)?;
    let mut state = manager.state.clone();
    state.plugins.get_mut(id).unwrap().enabled = false;
    state.plugins.get_mut(id).unwrap().load_error = None;
    state.plugins.get_mut(id).unwrap().route_profile_id = None;
    manager.commit(state)?;
    let old = manager.live.remove(id);
    manager.errors.remove(id);
    manager.log_event(id, "disabled");
    manager.update_fast_path();
    let result = manager.list();
    drop(manager);
    drop(old); // In-flight Arc references finish before destroy runs.
    provider::release_route(id)?;
    Ok(result)
}

fn enable_plugin(id: &str) -> Result<PluginList, String> {
    match reserve_enable(id)? {
        // 已加载实例只更新同意状态，不重复执行原生初始化。
        EnableStart::AlreadyLive(guard) => commit_enabled(guard, id, false),
        EnableStart::Reserved(reservation) => finish_enable(id, reservation),
    }
}

enum EnableStart {
    AlreadyLive(std::sync::MutexGuard<'static, Manager>),
    Reserved(LoadingReservation),
}

fn reserve_enable(id: &str) -> Result<EnableStart, String> {
    let mut guard = manager()?;
    if guard.stopping {
        return Err("插件管理器正在关闭".into());
    }
    if !guard.state.plugins.contains_key(id) {
        return Err("插件未安装".into());
    }
    if guard.live.contains_key(id) {
        return Ok(EnableStart::AlreadyLive(guard));
    }
    guard.reject_if_loading(id)?;
    let reserved_id = id.to_owned();
    guard.loading.insert(reserved_id.clone());
    // 这次尝试取代上一次失败留下的错误，避免启用过程中仍显示旧故障。
    guard.errors.remove(&reserved_id);
    drop(guard);
    Ok(EnableStart::Reserved(LoadingReservation {
        id: reserved_id,
        armed: true,
    }))
}

fn finish_enable(id: &str, mut reservation: LoadingReservation) -> Result<PluginList, String> {
    let prepared = {
        let mut guard = manager()?;
        match guard.prepare_load(id) {
            Ok(prepared) => prepared,
            Err(error) => {
                guard.note_enable_failure(id, &error);
                reservation.release(&mut guard);
                return Err(error);
            }
        }
    };
    // 原生 create 与 destroy 一样不能占着管理锁：慢初始化会挡住其他插件的启停和列表。
    let mut native = match Native::load(
        &prepared.library_path,
        prepared.config.clone(),
        prepared.context.clone(),
    ) {
        Ok(native) => native,
        Err(error) => {
            if let Ok(mut guard) = manager() {
                guard.note_enable_failure(id, &error);
                reservation.release(&mut guard);
            }
            return Err(error);
        }
    };
    let route = match provider::describe_if_declared(&prepared.manifest, &mut native) {
        Ok(route) => route,
        Err(error) => {
            if let Ok(mut guard) = manager() {
                guard.note_enable_failure(id, &error);
                reservation.release(&mut guard);
            }
            return Err(error);
        }
    };
    let mut guard = match manager() {
        Ok(guard) => guard,
        Err(error) => {
            drop(native);
            return Err(error);
        }
    };
    if guard.stopping || !guard.state.plugins.contains_key(id) {
        let error = if guard.stopping {
            "插件管理器正在关闭"
        } else {
            "插件未安装"
        };
        if !guard.stopping {
            guard.note_enable_failure(id, error);
        }
        reservation.release(&mut guard);
        drop(guard);
        drop(native);
        return Err(error.into());
    }
    if let Err(error) = guard.activation_matches(&prepared) {
        guard.note_enable_failure(id, &error);
        reservation.release(&mut guard);
        drop(guard);
        drop(native);
        return Err(error);
    }
    if guard.live.contains_key(id) {
        reservation.release(&mut guard);
        drop(guard);
        drop(native);
        return Err("插件正在启用，请稍后重试".into());
    }
    if let Err((native, error)) = guard.publish_prepared(&prepared, native) {
        guard.note_enable_failure(id, &error);
        reservation.release(&mut guard);
        drop(guard);
        drop(native);
        return Err(error);
    }
    // 与提交启用状态同一把锁内解除占位，避免析构清掉下一次启用。
    reservation.release(&mut guard);
    let list = commit_enabled(guard, id, true)?;
    if let Some(spec) = route
        && let Err(error) = publish_enabled_route(id, spec, true)
    {
        let _ = disable_plugin(id);
        if let Ok(mut guard) = manager() {
            guard.note_enable_failure(id, &error);
        }
        return Err(error);
    }
    Ok(list)
}

fn publish_enabled_route(
    id: &str,
    spec: provider::PluginRouteSpec,
    create_if_missing: bool,
) -> Result<(), String> {
    let route_id = provider::publish_route(id, spec, create_if_missing)?;
    let mut guard = manager()?;
    if !guard
        .state
        .plugins
        .get(id)
        .is_some_and(|record| record.enabled)
    {
        drop(guard);
        provider::release_route(id)?;
        return Err("插件已停用".into());
    }
    if route_id.is_none() {
        return Ok(());
    }
    let mut state = guard.state.clone();
    if let Some(record) = state.plugins.get_mut(id) {
        record.route_profile_id = route_id;
    }
    guard.commit(state)
}

fn commit_enabled(
    mut manager: std::sync::MutexGuard<'static, Manager>,
    id: &str,
    rollback_live: bool,
) -> Result<PluginList, String> {
    let mut state = manager.state.clone();
    let Some(record) = state.plugins.get_mut(id) else {
        let rolled_back = if rollback_live {
            manager.live.remove(id)
        } else {
            None
        };
        drop(manager);
        drop(rolled_back);
        return Err("插件未安装".into());
    };
    record.enabled = true;
    record.load_error = None;
    if let Err(error) = manager.commit(state) {
        let rolled_back = if rollback_live {
            manager.live.remove(id)
        } else {
            None
        };
        // 插件原生 destroy 不能在全局管理锁内执行：慢插件会卡住全部插件管理。
        drop(manager);
        drop(rolled_back);
        return Err(error);
    }
    manager.errors.remove(id);
    manager.log_event(id, "enabled");
    manager.update_fast_path();
    Ok(manager.list())
}

pub fn save_config_file(
    id: &str,
    content: &str,
    expected_sha256: &str,
) -> Result<PluginList, String> {
    let mut manager = manager()?;
    manager.save_config_file(id, content, expected_sha256)?;
    Ok(manager.list())
}

pub fn uninstall(id: &str, remove_data: bool) -> Result<PluginList, String> {
    if !package::valid_id(id) {
        return Err("插件 ID 无效".into());
    }
    let list = {
        let mut manager = manager()?;
        manager.uninstall(id, remove_data)?;
        manager.list()
    };
    provider::release_route(id)?;
    Ok(list)
}

pub fn invoke(id: &str, method: &str, params: Value) -> Result<Value, String> {
    if codey_plugin_sdk::lifecycle::HOST_METHODS.contains(&method) {
        return Err("该方法由宿主在请求生命周期中调用，不能通过管理接口调用".into());
    }
    if method == provider::DESCRIBE_METHOD {
        return Err("该方法由宿主在启用插件时调用，不能通过管理接口调用".into());
    }
    let instance = {
        manager()?
            .live
            .get(id)
            .map(|p| p.instance.clone())
            .ok_or("插件尚未启用")?
    };
    let result = instance
        .lock()
        .map_err(|_| "插件实例锁已损坏")?
        .invoke(method, params);
    if let Ok(mut m) = manager() {
        match &result {
            // 一次成功说明插件已经恢复正常，不能继续把瞬时失败当作当前故障，
            // 否则界面会一直显示运行异常。
            Ok(_) => {
                m.errors.remove(id);
            }
            Err(e) => {
                m.errors.insert(id.to_owned(), e.clone());
                m.log_event(id, "invoke_failed");
            }
        }
    }
    result
}

fn validate_patches(value: Value, allowed: &[String]) -> Result<Vec<HeaderPatch>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        headers: Vec<HeaderPatch>,
    }
    let mut patches = serde_json::from_value::<Output>(value)
        .map_err(|e| e.to_string())?
        .headers;
    if patches.len() > 32 {
        return Err("插件请求头修改超过 32 项".into());
    }
    let mut names = HashSet::new();
    let mut size = 0;
    for patch in &mut patches {
        patch.name.make_ascii_lowercase();
        if !allowed_header_name(&patch.name)
            || !allowed.iter().any(|n| n.eq_ignore_ascii_case(&patch.name))
            || !names.insert(patch.name.clone())
        {
            return Err("插件修改了未授权或重复请求头".into());
        }
        if let Some(v) = &patch.value {
            size += v.len();
            if v.len() > 16384
                || size > 32768
                || v.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
            {
                return Err("插件请求头值无效或过大".into());
            }
        }
    }
    Ok(patches)
}

pub fn allowed_header_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        && ![
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
            "host",
            "connection",
            "keep-alive",
            "proxy-connection",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "content-length",
            "content-type",
            "content-encoding",
            "accept-encoding",
            "chatgpt-account-id",
            "openai-organization",
            "openai-project",
            "api-key",
            "x-api-key",
        ]
        .contains(&name.as_str())
        && !name.starts_with("x-codey-")
        && !name.starts_with("sec-")
        && !name.contains("token")
        && !name.contains("credential")
        && !name.contains("authorization")
}

impl Manager {
    fn open(root: PathBuf) -> Result<Self, String> {
        if let Err(e) = fs::symlink_metadata(&root) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e.to_string());
            }
            fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        }
        let canonical_root = checked_directory(&root)?;
        checked_child_directory(&canonical_root, "installed", true)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let state_path = root.join("state.json");
        let state: State = if state_path.exists() {
            let bytes = fs::read(&state_path).map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err("插件状态文件过大".into());
            }
            serde_json::from_slice(&bytes).map_err(|e| format!("插件状态损坏: {e}"))?
        } else {
            State::default()
        };
        for (id, record) in &state.plugins {
            if !package::valid_id(id) || record.manifest.id != *id {
                return Err("插件状态 ID 无效或不一致".into());
            }
            // 这里只校验管理操作依赖的路径；兼容性错误由列表展示，加载前仍完整校验。
            validate_artifact_directory(&record.directory)?;
        }
        let errors = state
            .plugins
            .iter()
            .filter_map(|(id, record)| {
                record
                    .load_error
                    .as_ref()
                    .map(|error| (id.clone(), error.clone()))
            })
            .collect();
        Ok(Self {
            root: canonical_root,
            state,
            live: BTreeMap::new(),
            errors,
            stopping: false,
            loading: HashSet::new(),
            generations: BTreeMap::new(),
        })
    }

    fn load_enabled(&mut self) -> Result<(), String> {
        let ids: Vec<_> = self
            .state
            .plugins
            .iter()
            .filter(|(_, record)| record.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Err(error) = self.load(&id) {
                let message = format!("启动加载失败，插件已自动停用，请检查后重新启用：{error}");
                let mut state = self.state.clone();
                let record = state.plugins.get_mut(&id).unwrap();
                record.enabled = false;
                record.load_error = Some(message.clone());
                self.commit(state)
                    .map_err(|cause| format!("无法保存插件 {id} 的自动停用状态：{cause}"))?;
                self.log_event(&id, "load_failed");
                self.errors.insert(id, message);
            }
        }
        Ok(())
    }

    fn commit(&mut self, state: State) -> Result<(), String> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
        temp.write_all(&bytes).map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(self.root.join("state.json"))
            .map_err(|e| e.to_string())?;
        self.state = state;
        Ok(())
    }

    fn context(&self, id: &str, create: bool) -> Result<codey_plugin_sdk::PluginContext, String> {
        let root = checked_directory(&self.root)?;
        let installed = checked_child_directory(&root, "installed", false)?;
        let plugin_dir = checked_child_directory(&installed, id, false)?;
        for name in ["data", "logs"] {
            validate_optional_child(&plugin_dir, name)?;
        }
        let data_dir = checked_child_directory(&plugin_dir, "data", create)?;
        let log_dir = checked_child_directory(&plugin_dir, "logs", create)?;
        Ok(codey_plugin_sdk::PluginContext {
            plugin_id: id.to_owned(),
            plugin_dir,
            data_dir,
            log_dir,
        })
    }

    fn log_event(&self, id: &str, event: &str) {
        // Fixed event names only: native error strings may contain private input.
        if let Ok(context) = self.context(id, false) {
            let _ = codey_plugin_sdk::append_log(&context.log_dir, "host.log", event);
        }
    }

    fn uninstall(&mut self, id: &str, remove_data: bool) -> Result<(), String> {
        if self.stopping {
            return Err("插件管理器正在关闭".into());
        }
        let record = self.state.plugins.get(id).ok_or("插件未安装")?;
        if record.enabled || self.live.contains_key(id) {
            return Err("请先停用插件再卸载".into());
        }
        self.reject_if_loading(id)?;
        if self
            .generations
            .get(id)
            .is_some_and(|items| items.iter().any(|item| item.strong_count() > 0))
        {
            return Err(
                "插件仍有调用或实例正在退出，请完成后重试；若无法退出，请重启 Codey".into(),
            );
        }
        let root = checked_directory(&self.root)?;
        let installed = checked_child_directory(&root, "installed", false)?;
        let source = installed.join(id);
        let mut sources = Vec::new();
        match fs::symlink_metadata(&source) {
            Ok(_) => {
                let plugin_dir = checked_child_directory(&installed, id, false)?;
                for name in ["versions", "data", "logs"] {
                    validate_optional_child(&plugin_dir, name)?;
                }
                if remove_data {
                    sources.push(plugin_dir);
                } else {
                    // 保留配置、数据和日志，移除已卸载插件的其他文件。
                    for entry in fs::read_dir(&plugin_dir).map_err(|e| e.to_string())? {
                        let entry = entry.map_err(|e| e.to_string())?;
                        if entry.file_name() == "data"
                            || entry.file_name() == "logs"
                            || entry.file_name() == CONFIG_FILE
                        {
                            continue;
                        }
                        let path = entry.path();
                        // 插件可以把缓存写进自己的目录，普通文件只删除它本身；
                        // 符号链接仍然拒绝，避免顺着链接删到插件目录之外。
                        if path.is_dir() {
                            checked_directory(&path)?;
                        } else {
                            checked_file(&path)?;
                        }
                        sources.push(path);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let mut state = self.state.clone();
        state.plugins.remove(id);
        // Move before state commit; any move/commit failure restores the original installation.
        let mut moved = Vec::new();
        for source in sources {
            let trash = root.join(format!(".trash-{}", uuid::Uuid::new_v4()));
            if let Err(error) = fs::rename(&source, &trash) {
                for (source, trash) in moved.iter().rev() {
                    let _ = fs::rename(trash, source);
                }
                return Err(format!("无法移除插件文件，请重启 Codey 后重试: {error}"));
            }
            moved.push((source, trash));
        }
        if let Err(error) = self.commit(state) {
            for (source, trash) in moved.iter().rev() {
                let _ = fs::rename(trash, source);
            }
            return Err(error);
        }
        self.generations.remove(id);
        self.errors.remove(id);
        if !remove_data {
            self.log_event(id, "uninstalled_data_retained");
        }
        for (_, trash) in moved {
            if let Err(error) = fs::remove_dir_all(&trash) {
                return Err(format!(
                    "插件已卸载，但文件清理失败；请退出 Codey 后删除 {}: {error}",
                    trash.display()
                ));
            }
        }
        Ok(())
    }

    fn install(&mut self, package: package::Package) -> Result<(), String> {
        let inspection = package.inspection;
        let id = inspection.manifest.id.clone();
        self.reject_if_loading(&id)?;
        let old = self.state.plugins.get(&id);
        if let Some(old) = old
            && semver::Version::parse(&inspection.manifest.version).unwrap()
                <= semver::Version::parse(&old.manifest.version).map_err(|e| e.to_string())?
        {
            return Err("仅允许安装更高版本；相同版本不可覆盖".into());
        }
        let enabled = old.is_some_and(|record| record.enabled);
        // 升级成功后清理旧版本目录，先取成 owned 值，避免状态借用跨过后续提交。
        let previous_directory = old.map(|record| record.directory.clone());
        // Unique paths prevent dlopen from reusing a retained mapping after reinstall.
        let version_directory = format!("{}-{}", inspection.manifest.version, uuid::Uuid::new_v4());
        let directory = format!("versions/{version_directory}");
        // Validate existing parents before staging or moving package contents.
        let root = checked_directory(&self.root)?;
        let installed = checked_child_directory(&root, "installed", true)?;
        let plugin_directory = checked_child_directory(&installed, &id, true)?;
        // Validate every existing persistent child before creating any of them.
        for name in ["versions", "data", "logs"] {
            validate_optional_child(&plugin_directory, name)?;
        }
        let versions = checked_child_directory(&plugin_directory, "versions", true)?;
        checked_child_directory(&plugin_directory, "data", true)?;
        checked_child_directory(&plugin_directory, "logs", true)?;
        let config_path = plugin_directory.join(CONFIG_FILE);
        let initial_content = package.default_config.clone();
        if fs::symlink_metadata(&config_path).is_ok() {
            checked_config_file(&config_path)?;
        }
        let destination = versions.join(&version_directory);
        if destination.exists() {
            return Err("目标版本目录已存在".into());
        }
        let stage = tempfile::Builder::new()
            .prefix(".stage-")
            .tempdir_in(&self.root)
            .map_err(|e| e.to_string())?;
        for (name, bytes) in package.files {
            let file = stage.path().join(name);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::write(&file, bytes).map_err(|e| e.to_string())?;
        }
        fs::rename(stage.path(), &destination).map_err(|e| e.to_string())?;
        let created_config = match ensure_config_file(&config_path, &initial_content) {
            Ok(created) => created,
            Err(error) => {
                let _ = fs::remove_dir_all(&destination);
                return Err(error);
            }
        };
        let mut state = self.state.clone();
        state.plugins.insert(
            id.clone(),
            Record {
                manifest: inspection.manifest,
                enabled,
                directory: directory.clone(),
                load_error: None,
                route_profile_id: None,
            },
        );
        if let Err(e) = self.commit(state) {
            let _ = fs::remove_dir_all(destination);
            if created_config {
                let _ = fs::remove_file(&config_path);
            }
            return Err(e);
        }
        self.errors.remove(&id);
        self.log_event(&id, "installed");
        // 安装总是新建唯一版本目录，升级留下的旧目录不清理会一直累积磁盘占用。
        if let Some(previous) = previous_directory
            && previous != directory
        {
            let stale = plugin_directory.join(&previous);
            // 状态文件可能被外部改写，因此只删 versions 下的普通目录。
            if stale.parent() == Some(versions.as_path())
                && let Ok(stale) = checked_directory(&stale)
            {
                let _ = fs::remove_dir_all(stale);
            }
        }
        Ok(())
    }

    fn config_path(&self, id: &str) -> Result<PathBuf, String> {
        Ok(self.plugin_directory(id)?.join(CONFIG_FILE))
    }

    fn plugin_directory(&self, id: &str) -> Result<PathBuf, String> {
        if !package::valid_id(id) {
            return Err("插件 ID 无效".into());
        }
        let record = self.state.plugins.get(id).ok_or("插件未安装")?;
        if record.manifest.id != id {
            return Err("插件状态 ID 不一致".into());
        }
        let root = checked_directory(&self.root)?;
        let installed = checked_child_directory(&root, "installed", false)?;
        checked_child_directory(&installed, id, false)
    }

    fn get_config_file(&self, id: &str) -> Result<ConfigFile, String> {
        let path = self.config_path(id)?;
        let content = read_config_text(&path)?;
        Ok(ConfigFile {
            plugin_id: id.to_owned(),
            version: self.state.plugins[id].manifest.version.clone(),
            sha256: package::digest(content.as_bytes()),
            path,
            content,
        })
    }

    fn save_config_file(
        &mut self,
        id: &str,
        content: &str,
        expected_sha256: &str,
    ) -> Result<(), String> {
        if self.stopping {
            return Err("插件管理器正在关闭".into());
        }
        self.reject_if_loading(id)?;
        parse_config(content)?;
        let current = self.get_config_file(id)?;
        if current.sha256 != expected_sha256 {
            return Err("配置文件已被外部修改，请重新读取后再保存".into());
        }
        let temp = config_temp_file(&current.path, content)?;
        // Recheck after staging so edits made while preparing the write are not overwritten.
        if package::digest(read_config_text(&self.config_path(id)?)?.as_bytes()) != expected_sha256
        {
            return Err("配置文件已被外部修改，请重新读取后再保存".into());
        }
        temp.persist(&current.path).map_err(|e| e.to_string())?;
        self.log_event(id, "configuration_saved");
        Ok(())
    }

    fn reject_if_loading(&self, id: &str) -> Result<(), String> {
        if self.loading.contains(id) {
            Err("插件正在启用，请稍后重试".into())
        } else {
            Ok(())
        }
    }

    fn note_enable_failure(&mut self, id: &str, error: &str) {
        if self.stopping || !self.state.plugins.contains_key(id) {
            return;
        }
        self.log_event(id, "enable_failed");
        self.errors.insert(id.to_owned(), error.to_owned());
    }

    fn load(&mut self, id: &str) -> Result<(), String> {
        let prepared = self.prepare_load(id)?;
        let mut native = Native::load(
            &prepared.library_path,
            prepared.config.clone(),
            prepared.context.clone(),
        )?;
        let route = provider::describe_if_declared(&prepared.manifest, &mut native)?;
        let create_if_missing = self
            .state
            .plugins
            .get(id)
            .is_none_or(|record| record.route_profile_id.is_none());
        self.publish_prepared(&prepared, native)
            .map_err(|(_, error)| error)?;
        if let Some(spec) = route {
            let route_id = match provider::publish_route(id, spec, create_if_missing) {
                Ok(route_id) => route_id,
                Err(error) => {
                    let removed = self.live.remove(id);
                    drop(removed);
                    return Err(error);
                }
            };
            if let Some(route_id) = route_id {
                let mut state = self.state.clone();
                if let Some(record) = state.plugins.get_mut(id) {
                    record.route_profile_id = Some(route_id);
                }
                if let Err(error) = self.commit(state) {
                    let _ = provider::release_route(id);
                    let removed = self.live.remove(id);
                    drop(removed);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn prepare_load(&self, id: &str) -> Result<PreparedLoad, String> {
        let record = self.state.plugins.get(id).ok_or("插件未安装")?;
        package::validate_manifest(&record.manifest)?;
        if record.manifest.id != id {
            return Err("插件状态 ID 不一致".into());
        }
        let config = parse_config(&self.get_config_file(id)?.content)?;
        let host_root = checked_directory(&self.root)?;
        let installed = checked_child_directory(&host_root, "installed", false)?;
        let plugin_directory = checked_child_directory(&installed, id, false)?;
        let root = checked_artifact_directory(&plugin_directory, &record.directory)?;
        let context = self.context(id, true)?;
        let parts: Vec<_> = record.manifest.entry.split('/').collect();
        let mut parent = root.clone();
        for name in &parts[..parts.len() - 1] {
            parent = checked_child_directory(&parent, name, false)?;
        }
        let library_path = parent.join(parts.last().unwrap());
        verify_library(&library_path, &record.manifest.library_sha256)?;
        Ok(PreparedLoad {
            id: id.to_owned(),
            directory: record.directory.clone(),
            manifest: record.manifest.clone(),
            config,
            library_path,
            context,
        })
    }

    fn activation_matches(&self, prepared: &PreparedLoad) -> Result<(), String> {
        let record = self.state.plugins.get(&prepared.id).ok_or("插件未安装")?;
        if record.directory != prepared.directory
            || serde_json::to_value(&record.manifest).map_err(|error| error.to_string())?
                != serde_json::to_value(&prepared.manifest).map_err(|error| error.to_string())?
        {
            return Err("插件在启用过程中发生变化，请重试".into());
        }
        verify_library(&prepared.library_path, &prepared.manifest.library_sha256)?;
        let config = parse_config(&self.get_config_file(&prepared.id)?.content)?;
        if config != prepared.config {
            return Err("插件配置在启用过程中发生变化，请重试".into());
        }
        Ok(())
    }

    fn publish_prepared(
        &mut self,
        prepared: &PreparedLoad,
        native: Native,
    ) -> Result<(), (Native, String)> {
        let Some(record) = self.state.plugins.get(&prepared.id) else {
            return Err((native, "插件未安装".into()));
        };
        let manifest = record.manifest.clone();
        let generations = self.generations.entry(prepared.id.clone()).or_default();
        generations.retain(|generation| generation.strong_count() > 0);
        generations.push(native.lifetime());
        let instance = Arc::new(Mutex::new(native));
        let lifecycle = lifecycle::LifecyclePlugin::native(&manifest, instance.clone());
        self.live.insert(
            prepared.id.clone(),
            Active {
                instance,
                manifest,
                config: prepared.config.clone(),
                lifecycle,
            },
        );
        self.log_event(&prepared.id, "loaded");
        Ok(())
    }

    fn list(&self) -> PluginList {
        PluginList {
            platform: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            plugins: self
                .state
                .plugins
                .iter()
                .map(|(id, r)| {
                    let live = self.live.get(id);
                    let config = self
                        .get_config_file(id)
                        .and_then(|file| parse_config(&file.content));
                    let config_error = config.as_ref().err().cloned();
                    let restart_required = live.is_some_and(|p| {
                        p.manifest.version != r.manifest.version || config.as_ref() != Ok(&p.config)
                    });
                    let last_error = config_error
                        .or_else(|| self.errors.get(id).cloned())
                        .or_else(|| package::validate_manifest(&r.manifest).err());
                    let status = if self.loading.contains(id) {
                        "enabling"
                    } else if last_error.is_some() {
                        "error"
                    } else if live.is_some() {
                        "enabled"
                    } else {
                        "disabled"
                    };
                    PluginInfo {
                        id: id.clone(),
                        name: r.manifest.name.clone(),
                        version: r.manifest.version.clone(),
                        description: r.manifest.description.clone(),
                        enabled: r.enabled,
                        status: status.into(),
                        config_path: self.root.join("installed").join(id).join(CONFIG_FILE),
                        capabilities: r.manifest.capabilities.clone(),
                        last_error,
                        restart_required,
                        active_version: live.map(|p| p.manifest.version.clone()),
                        plugin_dir: self.root.join("installed").join(id),
                        data_dir: self.root.join("installed").join(id).join("data"),
                        log_dir: self.root.join("installed").join(id).join("logs"),
                        log_size_bytes: self
                            .plugin_directory(id)
                            .and_then(|dir| logs::size(&dir))
                            .ok(),
                    }
                })
                .collect(),
        }
    }

    fn update_fast_path(&self) {
        lifecycle::publish(
            self.live
                .values()
                .filter(|_| !self.stopping)
                .filter(|active| lifecycle::enabled(&active.manifest))
                .map(|active| active.lifecycle.clone())
                .collect(),
        );
    }
}

fn verify_library(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("插件动态库必须是普通文件，不能是符号链接".into());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > package::MAX_PACKAGE || package::digest(&bytes) != expected_sha256 {
        return Err("已安装动态库校验失败".into());
    }
    Ok(())
}

fn parse_config(content: &str) -> Result<Value, String> {
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Err("配置文件超过 1 MiB".into());
    }
    let mut value: Value = serde_json::from_str(content)
        .map_err(|e| format!("config.json 不是有效 JSON，请打开配置文件修复：{e}"))?;
    if !value.is_object() {
        return Err("config.json 的根值必须是 JSON 对象".into());
    }
    remove_config_comments(&mut value, "$")?;
    Ok(value)
}

// 仅移除运行配置中的说明，保存和冲突检查始终使用文件原文。
fn remove_config_comments(value: &mut Value, path: &str) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            if let Some(comments) = object.remove("_comments") {
                let comments_path = format!("{path}[\"_comments\"]");
                let entries = comments
                    .as_object()
                    .ok_or_else(|| format!("{comments_path} 必须是对象，每项说明必须是字符串"))?;
                for (key, description) in entries {
                    if !description.is_string() {
                        return Err(format!(
                            "{comments_path}[{}] 说明必须是字符串",
                            serde_json::to_string(key).unwrap()
                        ));
                    }
                }
            }
            for (key, child) in object {
                remove_config_comments(
                    child,
                    &format!("{path}[{}]", serde_json::to_string(key).unwrap()),
                )?;
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter_mut().enumerate() {
                remove_config_comments(child, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn checked_config_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("无法读取 config.json：{e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("配置文件必须是普通文件，不能是符号链接".into());
    }
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    if canonical.parent() != path.parent() {
        return Err("配置文件超出安装目录".into());
    }
    Ok(())
}

fn read_config_text(path: &Path) -> Result<String, String> {
    checked_config_file(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .map_err(|e| format!("无法读取 config.json：{e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("配置文件必须是普通文件，不能是符号链接".into());
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err("配置文件超过 1 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("配置文件超过 1 MiB".into());
    }
    String::from_utf8(bytes).map_err(|_| "配置文件必须为 UTF-8".into())
}

fn config_temp_file(path: &Path, content: &str) -> Result<tempfile::NamedTempFile, String> {
    let parent = path.parent().ok_or("配置文件路径无效")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    Ok(temp)
}

fn ensure_config_file(path: &Path, content: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            checked_config_file(path)?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            parse_config(content)?;
            config_temp_file(path, content)?
                .persist_noclobber(path)
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn checked_directory(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("插件安装路径必须是普通目录，不能是符号链接".into());
    }
    path.canonicalize().map_err(|e| e.to_string())
}

/// 普通文件同样只接受非链接目标：卸载删除的是条目本身，不会跟随链接。
fn checked_file(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("插件安装路径包含符号链接或特殊文件".into());
    }
    path.canonicalize().map_err(|e| e.to_string())
}

fn validate_optional_child(parent: &Path, name: &str) -> Result<(), String> {
    match fs::symlink_metadata(parent.join(name)) {
        Ok(_) => {
            checked_child_directory(parent, name, false)?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn validate_artifact_directory(directory: &str) -> Result<(), String> {
    if !package::safe_relative(directory)
        || !matches!(
            directory.split('/').collect::<Vec<_>>().as_slice(),
            ["versions", _]
        )
    {
        return Err("插件安装目录必须使用 versions/<版本目录> 格式".into());
    }
    Ok(())
}

fn checked_artifact_directory(plugin_dir: &Path, directory: &str) -> Result<PathBuf, String> {
    validate_artifact_directory(directory)?;
    let parts: Vec<_> = directory.split('/').collect();
    match parts.as_slice() {
        ["versions", name] => {
            let versions = checked_child_directory(plugin_dir, "versions", false)?;
            checked_child_directory(&versions, name, false)
        }
        _ => Err("插件安装目录无效".into()),
    }
}

fn checked_child_directory(parent: &Path, name: &str, create: bool) -> Result<PathBuf, String> {
    let path = parent.join(name);
    if create {
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let canonical = checked_directory(&path)?;
    if canonical.parent() != Some(parent) {
        return Err("插件路径超出安装目录".into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_comments_are_removed_recursively_without_changing_business_values() {
        let content = json!({
            "_comments": {"value": "说明", "_comments": "说明对象中的键不作为配置解析"},
            "value": "_comments 字符串保持原样",
            "_private": true,
            "_commentsLike": {"value": 7},
            "nested": {"_comments": {}, "enabled": false},
            "rules": [{"_comments": {"model": "模型"}, "model": "gpt6"},
                [null, {"_comments": {}, "lengths": [292]}]]
        });
        assert_eq!(
            parse_config(&content.to_string()).unwrap(),
            json!({
                "value": "_comments 字符串保持原样",
                "_private": true,
                "_commentsLike": {"value": 7},
                "nested": {"enabled": false},
                "rules": [{"model": "gpt6"}, [null, {"lengths": [292]}]]
            })
        );
    }

    #[test]
    fn config_comments_reject_invalid_types_at_their_json_paths() {
        for invalid in [Value::Null, json!(true), json!(7), json!("说明"), json!([])] {
            let content = json!({"rules": [{"_comments": invalid}]}).to_string();
            let error = parse_config(&content).unwrap_err();
            assert!(
                error.starts_with("$[\"rules\"][0][\"_comments\"] 必须是对象"),
                "{error}"
            );
        }
        for invalid in [Value::Null, json!(false), json!(42), json!([]), json!({})] {
            let content = json!({"rules": [{"_comments": {"a\"b": invalid}}]}).to_string();
            let error = parse_config(&content).unwrap_err();
            assert_eq!(
                error,
                "$[\"rules\"][0][\"_comments\"][\"a\\\"b\"] 说明必须是字符串"
            );
        }
        assert!(parse_config("{\"_comments\":{\"note\":\"valid\"}, // comment\n}").is_err());
    }

    #[test]
    fn config_comment_text_counts_toward_the_utf8_size_limit() {
        let content = format!(
            "{{\"_comments\":{{\"note\":\"{}\"}}}}",
            "说".repeat(MAX_CONFIG_BYTES as usize / 3)
        );
        assert!(parse_config(&content).unwrap_err().contains("1 MiB"));
    }

    pub(super) fn fixture_package() -> package::Package {
        let entry = if cfg!(target_os = "macos") {
            "lib.dylib"
        } else if cfg!(target_os = "windows") {
            "lib.dll"
        } else {
            "lib.so"
        };
        let manifest: Manifest = serde_json::from_value(json!({
            "id":"test.boundary","name":"Boundary","version":"1.0.0","abiVersion":1,
            "platform":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "entry":entry,"librarySha256":"0".repeat(64)
        }))
        .unwrap();
        package::Package {
            inspection: Inspection {
                path: "fixture.codey-plugin".into(),
                sha256: "0".repeat(64),
                manifest,
            },
            files: BTreeMap::from([("file.txt".into(), b"fixture".to_vec())]),
            default_config: "{}\n".into(),
        }
    }

    #[test]
    fn failed_startup_load_stays_disabled_across_restarts() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let mut state = manager.state.clone();
        state.plugins.get_mut("test.boundary").unwrap().enabled = true;
        manager.commit(state).unwrap();

        manager.load_enabled().unwrap();
        let plugin = manager.list().plugins.remove(0);
        assert!(!plugin.enabled);
        assert_eq!(plugin.status, "error");
        assert!(plugin.last_error.as_ref().unwrap().contains("已自动停用"));
        let persisted = fs::read(root.path().join("state.json")).unwrap();

        let mut reopened = Manager::open(root.path().into()).unwrap();
        reopened.load_enabled().unwrap();
        assert_eq!(fs::read(root.path().join("state.json")).unwrap(), persisted);
        assert_eq!(reopened.list().plugins[0].last_error, plugin.last_error);
        assert!(!reopened.list().plugins[0].enabled);

        // 升级包会清除旧版本的加载错误，但不会自动启用。
        let mut upgrade = fixture_package();
        upgrade.inspection.manifest.version = "1.0.1".into();
        reopened.install(upgrade).unwrap();
        let repaired = Manager::open(root.path().into()).unwrap().list();
        assert!(!repaired.plugins[0].enabled);
        assert!(repaired.plugins[0].last_error.is_none());
    }

    #[test]
    fn incompatible_installed_plugin_does_not_block_manager_startup() {
        for enabled in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            manager.install(fixture_package()).unwrap();
            let config = manager.get_config_file("test.boundary").unwrap();
            let mut other = fixture_package();
            other.inspection.manifest.id = "test.compatible".into();
            manager.install(other).unwrap();

            // 模拟升级宿主前保存的旧协议插件，绕过新安装包的兼容性检查。
            let mut state = manager.state.clone();
            let record = state.plugins.get_mut("test.boundary").unwrap();
            record.manifest.capabilities = vec!["request.beforeSend".into()];
            record.enabled = enabled;
            manager.commit(state).unwrap();

            let mut reopened = Manager::open(root.path().into()).unwrap();
            reopened.load_enabled().unwrap();
            let plugins = reopened.list().plugins;
            assert_eq!(plugins.len(), 2);
            let obsolete = &plugins[0];
            assert_eq!(obsolete.id, "test.boundary");
            assert!(!obsolete.enabled);
            assert_eq!(obsolete.status, "error");
            assert!(
                obsolete
                    .last_error
                    .as_ref()
                    .unwrap()
                    .contains("尚未支持的扩展能力")
            );
            assert_eq!(plugins[1].id, "test.compatible");
            assert_eq!(plugins[1].status, "disabled");
            assert!(plugins[1].last_error.is_none());
            assert!(reopened.live.is_empty());
            assert!(
                reopened
                    .load("test.boundary")
                    .unwrap_err()
                    .contains("尚未支持的扩展能力")
            );
            assert_eq!(fs::read_to_string(&config.path).unwrap(), config.content);

            let persisted = fs::read(root.path().join("state.json")).unwrap();
            let mut restarted = Manager::open(root.path().into()).unwrap();
            restarted.load_enabled().unwrap();
            assert_eq!(fs::read(root.path().join("state.json")).unwrap(), persisted);
            assert_eq!(restarted.list().plugins[0].last_error, obsolete.last_error);
            assert!(!restarted.list().plugins[0].enabled);

            if enabled {
                let mut upgrade = fixture_package();
                upgrade.inspection.manifest.version = "1.0.1".into();
                restarted.install(upgrade).unwrap();
                let repaired = Manager::open(root.path().into()).unwrap().list();
                assert_eq!(repaired.plugins[0].status, "disabled");
                assert!(repaired.plugins[0].last_error.is_none());
            } else {
                restarted.uninstall("test.boundary", false).unwrap();
                assert_eq!(restarted.list().plugins.len(), 1);
            }
            assert_eq!(fs::read_to_string(&config.path).unwrap(), config.content);
        }
    }

    #[test]
    fn config_file_preserves_text_and_rejects_external_edits() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        let mut package = fixture_package();
        package.default_config =
            "{\r\n  \"_comments\": {\"value\": \"初始说明\"},\r\n  \"value\":  1\r\n}\r\n".into();
        let initial = package.default_config.clone();
        manager.install(package).unwrap();
        let file = manager.get_config_file("test.boundary").unwrap();
        assert_eq!(file.content, initial);
        assert_eq!(file.sha256, package::digest(initial.as_bytes()));
        let edited = "{\n  \"_comments\": {\"value\": \"修改后的说明\"},\n  \"value\": 2, \"中文\": true\n}\n";
        manager
            .save_config_file("test.boundary", edited, &file.sha256)
            .unwrap();
        assert_eq!(fs::read_to_string(&file.path).unwrap(), edited);
        assert!(
            manager
                .save_config_file("test.boundary", "{}", &file.sha256)
                .unwrap_err()
                .contains("外部修改")
        );
        let latest = manager.get_config_file("test.boundary").unwrap();
        fs::write(&file.path, "{\"external\":true}").unwrap();
        assert!(
            manager
                .save_config_file("test.boundary", "{}", &latest.sha256)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(&file.path).unwrap(),
            "{\"external\":true}"
        );
        assert!(manager.get_config_file("../test.boundary").is_err());
        assert!(manager.get_config_file("missing").is_err());
        let info = serde_json::to_value(manager.list()).unwrap();
        assert_eq!(
            info["plugins"][0]["configPath"],
            file.path.to_string_lossy().as_ref()
        );
        for removed in ["config", "configSchema", "configUi", "activeConfig"] {
            assert!(info["plugins"][0].get(removed).is_none());
        }
    }

    #[test]
    fn plugin_directory_resolves_installed_root_and_rejects_invalid_ids() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let directory = manager.plugin_directory("test.boundary").unwrap();
        assert_eq!(
            directory,
            manager.context("test.boundary", false).unwrap().plugin_dir
        );
        assert_eq!(directory, manager.list().plugins[0].plugin_dir);
        assert_eq!(directory.file_name().unwrap(), "test.boundary");
        assert_eq!(
            directory.parent().unwrap().file_name().unwrap(),
            "installed"
        );
        assert!(
            manager
                .plugin_directory("../test.boundary")
                .unwrap_err()
                .contains("无效")
        );
        assert!(
            manager
                .plugin_directory("missing")
                .unwrap_err()
                .contains("未安装")
        );
    }

    #[test]
    fn damaged_config_can_be_read_and_repaired_but_never_loaded() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let path = manager.get_config_file("test.boundary").unwrap().path;
        fs::write(&path, "{broken").unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        let broken = manager.get_config_file("test.boundary").unwrap();
        assert_eq!(broken.content, "{broken");
        assert!(
            manager.list().plugins[0]
                .last_error
                .as_ref()
                .unwrap()
                .contains("JSON")
        );
        assert!(manager.load("test.boundary").unwrap_err().contains("JSON"));
        for invalid in ["[]", "null", "{", "true"] {
            assert!(
                manager
                    .save_config_file("test.boundary", invalid, &broken.sha256)
                    .is_err()
            );
        }
        manager
            .save_config_file("test.boundary", "{}\n", &broken.sha256)
            .unwrap();
        assert!(manager.list().plugins[0].last_error.is_none());
        fs::write(&path, [0xff]).unwrap();
        assert!(
            manager
                .get_config_file("test.boundary")
                .unwrap_err()
                .contains("UTF-8")
        );
        fs::write(&path, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert!(
            manager
                .get_config_file("test.boundary")
                .unwrap_err()
                .contains("1 MiB")
        );
        assert!(
            manager
                .save_config_file(
                    "test.boundary",
                    &" ".repeat(MAX_CONFIG_BYTES as usize + 1),
                    ""
                )
                .unwrap_err()
                .contains("1 MiB")
        );
    }

    #[test]
    fn obsolete_state_fields_are_rejected_without_rewriting_files() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let config = manager.get_config_file("test.boundary").unwrap();
        let current = serde_json::to_value(&manager.state).unwrap();
        for field in ["config", "retainedConfig", "configSchema", "configUi"] {
            let mut obsolete = current.clone();
            match field {
                "config" => obsolete["plugins"]["test.boundary"][field] = json!({"old":true}),
                "retainedConfig" => obsolete[field] = json!({"test.boundary":{}}),
                _ => obsolete["plugins"]["test.boundary"]["manifest"][field] = json!({}),
            }
            let bytes = serde_json::to_vec(&obsolete).unwrap();
            fs::write(root.path().join("state.json"), &bytes).unwrap();
            assert!(Manager::open(root.path().into()).is_err(), "{field}");
            assert_eq!(fs::read(root.path().join("state.json")).unwrap(), bytes);
            assert_eq!(fs::read_to_string(&config.path).unwrap(), config.content);
        }
    }

    #[test]
    fn opening_does_not_synthesize_a_missing_configuration() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let path = manager.get_config_file("test.boundary").unwrap().path;
        fs::remove_file(&path).unwrap();
        let mut reopened = Manager::open(root.path().into()).unwrap();
        assert!(!path.exists());
        assert!(reopened.get_config_file("test.boundary").is_err());
        assert!(reopened.load("test.boundary").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn config_file_rejects_symlink_files_and_parents() {
        for relative in [
            "installed",
            "installed/test.boundary",
            "installed/test.boundary/config.json",
        ] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            manager.install(fixture_package()).unwrap();
            let file = manager.get_config_file("test.boundary").unwrap();
            let source = root.path().join(relative);
            let destination = outside.path().join("moved");
            fs::rename(&source, &destination).unwrap();
            std::os::unix::fs::symlink(&destination, &source).unwrap();
            assert!(
                manager
                    .get_config_file("test.boundary")
                    .unwrap_err()
                    .contains("符号链接")
            );
            assert!(
                manager
                    .save_config_file("test.boundary", "{}", &file.sha256)
                    .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn installation_rejects_linked_parent_directories_before_writing() {
        use std::os::unix::fs::symlink;
        for relative in [
            "installed",
            "installed/test.boundary",
            "installed/test.boundary/versions",
            "installed/test.boundary/data",
            "installed/test.boundary/logs",
        ] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            let path = root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if path.exists() {
                fs::remove_dir(&path).unwrap();
            }
            symlink(outside.path(), &path).unwrap();
            assert!(manager.install(fixture_package()).is_err(), "{relative}");
            assert!(manager.state.plugins.is_empty());
            assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
            assert!(!root.path().join("state.json").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn opening_rejects_linked_installation_directory() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("installed")).unwrap();
        assert!(Manager::open(root.path().into()).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn installation_rejects_non_directory_parents() {
        for relative in ["installed", "installed/test.boundary"] {
            let root = tempfile::tempdir().unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            let path = root.path().join(relative);
            if path.exists() {
                fs::remove_dir(&path).unwrap();
            }
            fs::write(&path, b"preserve").unwrap();
            assert!(manager.install(fixture_package()).is_err(), "{relative}");
            assert_eq!(fs::read(&path).unwrap(), b"preserve");
            assert!(manager.state.plugins.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn loading_rejects_linked_parent_directories() {
        for relative in [
            "installed",
            "installed/test.boundary",
            "installed/test.boundary/versions",
            "installed/test.boundary/data",
            "installed/test.boundary/logs",
        ] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let mut manager = Manager::open(root.path().into()).unwrap();
            manager.install(fixture_package()).unwrap();
            let path = root.path().join(relative);
            let moved = outside.path().join("moved");
            fs::rename(&path, &moved).unwrap();
            std::os::unix::fs::symlink(&moved, &path).unwrap();
            let error = manager.load("test.boundary").unwrap_err();
            assert!(error.contains("符号链接"), "{error}");
            assert!(manager.live.is_empty());
        }
    }

    #[test]
    fn patches_are_validated_as_a_whole() {
        let allowed = vec!["x-example".into()];
        assert!(validate_patches(json!({"headers":[{"name":"x-example","value":"ok"},{"name":"authorization","value":"bad"}]}),&allowed).is_err());
        assert!(
            validate_patches(
                json!({"headers":[{"name":"x-example","value":"ok\r\nInjected: yes"}]}),
                &allowed
            )
            .is_err()
        );
        assert!(
            validate_patches(
                json!({"headers":[{"name":"X-Example","value":"ok"}]}),
                &allowed
            )
            .is_ok()
        );
        assert!(
            validate_patches(
                json!({"headers":[{"name":"x-example","value":"ok\t "}]}),
                &allowed
            )
            .is_ok()
        );
        // http 1 接受 0x80–0xFF，非 ASCII 文本可以成为请求头值。
        assert!(
            validate_patches(
                json!({"headers":[{"name":"x-example","value":"café"}]}),
                &allowed
            )
            .is_ok()
        );
        assert!(
            validate_patches(
                json!({"headers":[{"name":"x-example","value":"bad\u{007f}"}]}),
                &allowed
            )
            .is_err()
        );
    }

    #[test]
    fn management_invoke_rejects_host_lifecycle_methods() {
        for method in codey_plugin_sdk::lifecycle::HOST_METHODS {
            let error = invoke("test.boundary", method, json!({})).unwrap_err();
            assert!(error.contains("生命周期"), "{method}: {error}");
        }
        let describe = invoke("test.boundary", provider::DESCRIBE_METHOD, json!({})).unwrap_err();
        assert!(describe.contains("启用插件"), "{describe}");
        let ordinary = invoke("test.boundary", "ping", json!({})).unwrap_err();
        assert!(!ordinary.contains("生命周期"), "{ordinary}");
    }

    #[test]
    fn enabling_blocks_conflicting_changes_until_the_reservation_is_released() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let context = manager.context("test.boundary", false).unwrap();
        fs::write(context.data_dir.join("sentinel"), b"preserve").unwrap();
        let config = manager.get_config_file("test.boundary").unwrap();
        manager.loading.insert("test.boundary".into());
        assert_eq!(manager.list().plugins[0].status, "enabling");
        assert!(
            manager
                .uninstall("test.boundary", true)
                .unwrap_err()
                .contains("正在启用")
        );
        assert!(context.data_dir.join("sentinel").exists());
        assert!(
            manager
                .save_config_file("test.boundary", "{\"value\":1}\n", &config.sha256)
                .unwrap_err()
                .contains("正在启用")
        );
        assert_eq!(fs::read_to_string(&config.path).unwrap(), config.content);
        let mut upgrade = fixture_package();
        upgrade.inspection.manifest.version = "1.0.1".into();
        assert!(manager.install(upgrade).unwrap_err().contains("正在启用"));
        assert_eq!(
            manager.state.plugins["test.boundary"].manifest.version,
            "1.0.0"
        );
        manager.loading.remove("test.boundary");
        assert_eq!(manager.list().plugins[0].status, "disabled");
        manager.uninstall("test.boundary", true).unwrap();
        assert!(!context.plugin_dir.exists());
    }

    #[test]
    fn poisoned_lock_still_clears_the_enable_reservation() {
        let root = tempfile::tempdir().unwrap();
        let manager = Mutex::new(Manager::open(root.path().into()).unwrap());
        manager
            .lock()
            .unwrap()
            .loading
            .insert("test.boundary".into());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = manager.lock().unwrap();
            panic!("poison the plugin manager lock");
        }));
        assert!(manager.lock().is_err());
        clear_loading_in(&manager, "test.boundary");
        let guard = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(!guard.loading.contains("test.boundary"));
    }

    #[test]
    fn released_reservation_does_not_cancel_a_later_enable() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.loading.insert("test.boundary".into());
        let mut reservation = LoadingReservation {
            id: "test.boundary".into(),
            armed: true,
        };
        reservation.release(&mut manager);
        assert!(!reservation.armed);
        manager.loading.insert("test.boundary".into());
        drop(reservation);
        assert!(manager.loading.contains("test.boundary"));
    }

    #[test]
    fn prepared_enable_rejects_config_or_library_changes_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let id = "test.boundary";
        let directory = manager.state.plugins[id].directory.clone();
        let entry = manager.state.plugins[id].manifest.entry.clone();
        let library = manager
            .plugin_directory(id)
            .unwrap()
            .join(&directory)
            .join(entry);
        let bytes = b"not-a-real-dylib";
        fs::write(&library, bytes).unwrap();
        let mut state = manager.state.clone();
        state.plugins.get_mut(id).unwrap().manifest.library_sha256 = package::digest(bytes);
        manager.commit(state).unwrap();
        let prepared = manager.prepare_load(id).unwrap();
        assert!(manager.activation_matches(&prepared).is_ok());
        let config = manager.get_config_file(id).unwrap();
        fs::write(&config.path, b"{\"value\":1}\n").unwrap();
        assert!(
            manager
                .activation_matches(&prepared)
                .unwrap_err()
                .contains("配置")
        );
        fs::write(&config.path, config.content).unwrap();
        assert!(manager.activation_matches(&prepared).is_ok());
        fs::write(&library, b"changed").unwrap();
        assert!(
            manager
                .activation_matches(&prepared)
                .unwrap_err()
                .contains("校验失败")
        );
        assert!(manager.live.is_empty());
    }
    #[test]
    fn credential_and_transport_headers_are_forbidden() {
        for name in [
            "Authorization",
            "Cookie",
            "X-Codey-Key",
            "Connection",
            "Transfer-Encoding",
            "ChatGPT-Account-ID",
            "X-Auth-Token",
        ] {
            assert!(!allowed_header_name(name), "{name}");
        }
        assert!(allowed_header_name("X-Codex-Turn-State"));
    }

    #[test]
    fn failed_state_commit_rolls_back_installation() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        fs::create_dir(root.path().join("state.json")).unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "id":"test.rollback","name":"Rollback","version":"1.0.0","abiVersion":1,
            "platform":std::env::consts::OS,"arch":std::env::consts::ARCH,
            "entry":"lib.so","librarySha256":"0".repeat(64)
        }))
        .unwrap();
        let package = package::Package {
            inspection: Inspection {
                path: "fixture.codey-plugin".into(),
                sha256: "0".repeat(64),
                manifest,
            },
            files: BTreeMap::from([("file.txt".into(), b"fixture".to_vec())]),
            default_config: "{}\n".into(),
        };
        assert!(manager.install(package).is_err());
        assert!(manager.state.plugins.is_empty());
        assert!(
            !root
                .path()
                .join("installed/test.rollback/config.json")
                .exists()
        );
        assert_eq!(
            fs::read_dir(root.path().join("installed/test.rollback/versions"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn persistent_directories_survive_upgrade_restart_and_retained_uninstall() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let context = manager.context("test.boundary", false).unwrap();
        fs::write(context.data_dir.join("saved.json"), b"persistent").unwrap();
        fs::write(context.log_dir.join("plugin.log"), b"log\n").unwrap();
        let config_path = context.plugin_dir.join(CONFIG_FILE);
        fs::write(&config_path, b"{\"saved\":true}\n").unwrap();
        let mut upgrade = fixture_package();
        upgrade.inspection.manifest.version = "2.0.0".into();
        manager.install(upgrade).unwrap();
        let mut reopened = Manager::open(root.path().into()).unwrap();
        assert_eq!(
            context.data_dir,
            reopened.context("test.boundary", false).unwrap().data_dir
        );
        assert_eq!(
            fs::read(context.data_dir.join("saved.json")).unwrap(),
            b"persistent"
        );
        reopened.uninstall("test.boundary", false).unwrap();
        assert!(!context.plugin_dir.join("versions").exists());
        assert!(context.log_dir.join("plugin.log").exists());
        assert_eq!(fs::read(&config_path).unwrap(), b"{\"saved\":true}\n");
        reopened.install(fixture_package()).unwrap();
        assert_eq!(
            fs::read(context.data_dir.join("saved.json")).unwrap(),
            b"persistent"
        );
        assert_eq!(fs::read(&config_path).unwrap(), b"{\"saved\":true}\n");
        reopened.uninstall("test.boundary", true).unwrap();
        assert!(!context.plugin_dir.exists());
    }

    #[test]
    fn uninstall_waits_for_every_retired_generation_without_deleting_data() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let first = Arc::new(());
        let second = Arc::new(());
        manager.generations.insert(
            "test.boundary".into(),
            vec![Arc::downgrade(&first), Arc::downgrade(&second)],
        );
        let context = manager.context("test.boundary", false).unwrap();
        fs::write(context.data_dir.join("sentinel"), b"preserve").unwrap();
        drop(second);
        assert!(
            manager
                .uninstall("test.boundary", true)
                .unwrap_err()
                .contains("正在退出")
        );
        assert!(context.data_dir.join("sentinel").exists());
        drop(first);
        manager.uninstall("test.boundary", true).unwrap();
        assert!(!context.plugin_dir.exists());
    }

    #[test]
    fn artifact_directory_requires_the_versions_layout() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::open(root.path().into()).unwrap();
        manager.install(fixture_package()).unwrap();
        let context = manager.context("test.boundary", false).unwrap();
        let old = manager.state.plugins["test.boundary"].directory.clone();
        assert_eq!(
            checked_artifact_directory(&context.plugin_dir, &old).unwrap(),
            context.plugin_dir.join(&old)
        );
        for invalid in [
            "1.0.0",
            "data",
            "logs",
            "versions",
            "versions/a/b",
            "../outside",
        ] {
            assert!(checked_artifact_directory(&context.plugin_dir, invalid).is_err());
            let mut state = manager.state.clone();
            state.plugins.get_mut("test.boundary").unwrap().directory = invalid.into();
            manager.commit(state).unwrap();
            assert!(Manager::open(root.path().into()).is_err(), "{invalid}");
            assert!(context.plugin_dir.join(&old).exists());
        }
    }
}
