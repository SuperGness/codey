//! Codey 原生插件 ABI v1。仅可信插件：动态库与宿主拥有相同进程权限。
//! Rust trait 只用于插件内部，跨动态库边界只使用下列 C 布局类型。
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

pub mod lifecycle;
pub mod provider;

pub use serde_json;
use serde_json::Value;
use std::{fs, io::Write, path::PathBuf};

pub const ABI_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// 宿主提供的持久目录。目录约定不限制原生代码访问其他位置。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginContext {
    pub plugin_id: String,
    pub plugin_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl PluginContext {
    /// 写入 plugin.log；最多保留当前文件与一个 1 MiB 备份。
    /// 文件锁保护跨实例轮转；日志忙时返回错误，不阻塞等待。
    pub fn log(&self, event: &str) -> Result<(), String> {
        append_log(&self.log_dir, "plugin.log", event)
    }
}

/// 带本地时区与毫秒的标准时间，供文件日志和插件事件复用。
pub fn timestamp_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
}

/// Local log timestamp in the compact format used by plugin.log and events.
pub fn timestamp_log() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 有界事件日志；目录必须由宿主预先创建，不创建或重建目录。
pub fn append_log(directory: &std::path::Path, name: &str, event: &str) -> Result<(), String> {
    static LOG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOG_LOCK.lock().map_err(|_| "日志锁已损坏")?;
    if !["plugin.log", "host.log"].contains(&name) || event.len() > 4096 {
        return Err("日志名称无效或单条事件超过 4096 字节".into());
    }
    let metadata = fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("日志目录必须是普通目录".into());
    }
    let path = directory.join(name);
    let backup = directory.join(format!("{name}.1"));
    let lock_path = directory.join(format!("{name}.lock"));
    for file in [&path, &backup, &lock_path] {
        match fs::symlink_metadata(file) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                return Err("日志文件必须是普通文件".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    // Different plugin versions load separate SDK copies, so a Rust mutex alone
    // cannot serialize their writes. Keep a stable lock file across rotations.
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|e| format!("日志写入被占用或无法锁定: {e}"))?;
    let timestamp = timestamp_log();
    let line = format!("{timestamp} {}\n", event.replace(['\r', '\n'], " "));
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > 1024 * 1024 {
        if backup.exists() {
            fs::remove_file(&backup).map_err(|e| e.to_string())?;
        }
        fs::rename(&path, &backup).map_err(|e| e.to_string())?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|e| e.to_string())
}

#[repr(C)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Self {
            data: std::ptr::null_mut(),
            len: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginApiV1 {
    pub abi_version: u32,
    pub struct_size: u32,
    pub create: unsafe extern "C" fn(*const u8, usize, *mut *mut c_void, *mut Buffer) -> i32,
    pub invoke: unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut Buffer) -> i32,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub free_buffer: unsafe extern "C" fn(Buffer),
}

pub type EntryPoint = unsafe extern "C" fn() -> *const PluginApiV1;

/// 单个实例的调用被串行化。插件必须在 Drop 中结束自己启动的任务。
pub trait Plugin: Send + 'static + Sized {
    fn create(config: Value, context: PluginContext) -> Result<Self, String>;
    fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

unsafe fn read_json(data: *const u8, len: usize) -> Result<Value, String> {
    if len == 0 || len > MAX_MESSAGE_BYTES || data.is_null() {
        return Err("插件消息长度无效".into());
    }
    serde_json::from_slice(unsafe { std::slice::from_raw_parts(data, len) })
        .map_err(|e| e.to_string())
}

fn output(result: Result<Value, String>, out: *mut Buffer) -> i32 {
    if out.is_null() {
        return 1;
    }
    let (code, value) = match result {
        Ok(v) => (0, v),
        Err(e) => (1, serde_json::json!({"error":e})),
    };
    let mut bytes = serde_json::to_vec(&value).unwrap_or_else(|_| b"null".to_vec());
    let code = if bytes.len() > MAX_MESSAGE_BYTES {
        bytes = b"{\"error\":\"plugin output exceeds limit\"}".to_vec();
        1
    } else {
        code
    };
    let bytes = bytes.into_boxed_slice();
    let len = bytes.len();
    let data = Box::into_raw(bytes).cast::<u8>();
    unsafe {
        *out = Buffer { data, len };
    }
    code
}

/// # Safety
/// 输入必须包含 config 与 context；指针须在调用期间有效。
pub unsafe extern "C" fn create<P: Plugin>(
    data: *const u8,
    len: usize,
    instance: *mut *mut c_void,
    out: *mut Buffer,
) -> i32 {
    if instance.is_null() {
        return output(Err("实例输出指针为空".into()), out);
    }
    unsafe {
        *instance = std::ptr::null_mut();
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let input = unsafe { read_json(data, len)? };
        let fields = input.as_object().ok_or("初始化参数必须是对象")?;
        if fields.keys().any(|key| key != "config" && key != "context") {
            return Err("初始化参数只允许 config 和 context".into());
        }
        let context = serde_json::from_value(input.get("context").cloned().ok_or("缺少 context")?)
            .map_err(|e| e.to_string())?;
        let plugin = P::create(input.get("config").cloned().ok_or("缺少 config")?, context)?;
        unsafe {
            *instance = Box::into_raw(Box::new(Mutex::new(plugin))).cast();
        }
        Ok(Value::Null)
    }))
    .unwrap_or_else(|_| Err("插件初始化发生 panic".into()));
    output(result, out)
}

/// # Safety
/// instance 必须由同一个插件的 create 返回，且尚未销毁。
pub unsafe extern "C" fn invoke<P: Plugin>(
    instance: *mut c_void,
    data: *const u8,
    len: usize,
    out: *mut Buffer,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if instance.is_null() {
            return Err("插件实例为空".into());
        }
        let request = unsafe { read_json(data, len)? };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or("缺少 method")?;
        let plugin = unsafe { &*instance.cast::<Mutex<P>>() };
        plugin.lock().map_err(|_| "插件实例锁已损坏")?.invoke(
            method,
            request.get("params").cloned().unwrap_or(Value::Null),
        )
    }))
    .unwrap_or_else(|_| Err("插件调用发生 panic".into()));
    output(result, out)
}

/// # Safety
/// instance 必须由同一个插件的 create 返回，只能销毁一次且不能有并发调用。
pub unsafe extern "C" fn destroy<P: Plugin>(instance: *mut c_void) {
    if !instance.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(instance.cast::<Mutex<P>>()));
        }));
    }
}

/// # Safety
/// buffer 必须由本动态库返回，只能释放一次。宿主不能通过自己的分配器释放它。
pub unsafe extern "C" fn free_buffer(buffer: Buffer) {
    if !buffer.data.is_null() {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                buffer.data,
                buffer.len,
            )));
        }
    }
}

#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn codey_plugin_entry_v1() -> *const $crate::PluginApiV1 {
            static API: $crate::PluginApiV1 = $crate::PluginApiV1 {
                abi_version: $crate::ABI_VERSION,
                struct_size: std::mem::size_of::<$crate::PluginApiV1>() as u32,
                create: $crate::create::<$plugin>,
                invoke: $crate::invoke::<$plugin>,
                destroy: $crate::destroy::<$plugin>,
                free_buffer: $crate::free_buffer,
            };
            &API
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_does_not_rotate_while_another_instance_holds_the_lock() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("plugin.log");
        fs::write(&current, vec![b'x'; 1024 * 1024]).unwrap();
        let lock = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(root.path().join("plugin.log.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        assert!(append_log(root.path(), "plugin.log", "blocked").is_err());
        assert_eq!(fs::metadata(&current).unwrap().len(), 1024 * 1024);
        assert!(!root.path().join("plugin.log.1").exists());
        drop(lock);
        append_log(root.path(), "plugin.log", "rotated").unwrap();
        assert_eq!(
            fs::metadata(root.path().join("plugin.log.1"))
                .unwrap()
                .len(),
            1024 * 1024
        );
        assert!(fs::read_to_string(current).unwrap().contains("rotated"));
    }

    struct PanicPlugin;
    impl Plugin for PanicPlugin {
        fn create(_: Value, _: PluginContext) -> Result<Self, String> {
            Ok(Self)
        }
        fn invoke(&mut self, _: &str, _: Value) -> Result<Value, String> {
            panic!("fixture panic")
        }
    }

    #[test]
    fn panic_stays_inside_abi_and_buffers_are_released_by_plugin() {
        let mut instance = std::ptr::null_mut();
        let mut result = Buffer::default();
        let input = br#"{"config":{},"context":{"pluginId":"test","pluginDir":"plugin","dataDir":"data","logDir":"logs"}}"#;
        assert_eq!(
            unsafe {
                create::<PanicPlugin>(input.as_ptr(), input.len(), &mut instance, &mut result)
            },
            0
        );
        unsafe {
            free_buffer(result);
        }
        let input = br#"{"method":"panic","params":null}"#;
        let mut result = Buffer::default();
        assert_eq!(
            unsafe { invoke::<PanicPlugin>(instance, input.as_ptr(), input.len(), &mut result) },
            1
        );
        let value: Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(result.data, result.len) })
                .unwrap();
        assert!(value["error"].as_str().unwrap().contains("panic"));
        unsafe {
            free_buffer(result);
            destroy::<PanicPlugin>(instance);
        }
    }

    #[test]
    fn invalid_input_is_reported_without_creating_instance() {
        let mut instance = std::ptr::null_mut();
        let mut result = Buffer::default();
        assert_eq!(
            unsafe { create::<PanicPlugin>(std::ptr::null(), 10, &mut instance, &mut result) },
            1
        );
        assert!(instance.is_null());
        unsafe {
            free_buffer(result);
        }
    }

    #[test]
    fn initialization_requires_the_complete_envelope_without_extra_fields() {
        let context = serde_json::json!({
            "pluginId": "test", "pluginDir": "plugin", "dataDir": "data", "logDir": "logs"
        });
        for input in [
            serde_json::json!({}),
            serde_json::json!({"config": {}}),
            serde_json::json!({"context": context}),
            serde_json::json!({"config": {}, "context": null}),
            serde_json::json!({"config": {}, "context": {"pluginId": "test"}}),
            serde_json::json!({"config": {}, "context": context, "extra": true}),
            serde_json::json!([]),
        ] {
            let bytes = serde_json::to_vec(&input).unwrap();
            let mut instance = std::ptr::null_mut();
            let mut result = Buffer::default();
            assert_eq!(
                unsafe {
                    create::<PanicPlugin>(bytes.as_ptr(), bytes.len(), &mut instance, &mut result)
                },
                1,
                "{input}"
            );
            assert!(instance.is_null());
            unsafe {
                free_buffer(result);
            }
        }
    }

    #[test]
    fn event_logs_are_bounded_and_do_not_recreate_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("plugin.log"), vec![b'x'; 1024 * 1024]).unwrap();
        append_log(temp.path(), "plugin.log", "rotated\nentry").unwrap();
        assert_eq!(
            fs::metadata(temp.path().join("plugin.log.1"))
                .unwrap()
                .len(),
            1024 * 1024
        );
        assert!(
            fs::read_to_string(temp.path().join("plugin.log"))
                .unwrap()
                .contains("rotated entry")
        );
        assert!(append_log(temp.path(), "plugin.log", &"x".repeat(4097)).is_err());
        let missing = temp.path().join("missing");
        assert!(append_log(&missing, "plugin.log", "event").is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn event_log_timestamp_uses_compact_local_format() {
        let temp = tempfile::tempdir().unwrap();
        append_log(temp.path(), "plugin.log", "timestamp-check").unwrap();
        let line = fs::read_to_string(temp.path().join("plugin.log")).unwrap();
        let (date, rest) = line.split_once(' ').unwrap();
        assert_eq!(date.len(), 10);
        let time = rest.split_once(' ').unwrap().0;
        assert_eq!(time.len(), 8);
        assert_eq!(&time[2..3], ":");
        assert_eq!(&time[5..6], ":");
    }

    #[cfg(unix)]
    #[test]
    fn event_log_symlinks_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"preserve").unwrap();
        std::os::unix::fs::symlink(&outside, temp.path().join("plugin.log")).unwrap();
        assert!(append_log(temp.path(), "plugin.log", "event").is_err());
        assert_eq!(fs::read(outside).unwrap(), b"preserve");
    }
}
