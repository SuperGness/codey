use codey_plugin_sdk::{ABI_VERSION, Buffer, EntryPoint, PluginApiV1};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
};
struct Native {
    api: PluginApiV1,
    instance: *mut c_void,
    _library: libloading::Library,
    _dir: tempfile::TempDir,
}
impl Drop for Native {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.instance) };
    }
}
fn library_path() -> PathBuf {
    std::env::var_os("CODEY_PLUGIN_DLL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Cargo's build-dir can relocate test binaries away from final artifacts.
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target/debug")
                .join(if cfg!(target_os = "windows") {
                    "codey_plugin_antigravity_router.dll"
                } else if cfg!(target_os = "macos") {
                    "libcodey_plugin_antigravity_router.dylib"
                } else {
                    "libcodey_plugin_antigravity_router.so"
                })
        })
}
fn create(path: &Path, config: Value) -> Result<Native, String> {
    let library = unsafe { libloading::Library::new(path) }.expect("load DLL");
    let entry: libloading::Symbol<EntryPoint> =
        unsafe { library.get(b"codey_plugin_entry_v1\0") }.unwrap();
    let api = unsafe { *entry() };
    assert_eq!(api.abi_version, ABI_VERSION);
    assert_eq!(api.struct_size as usize, std::mem::size_of::<PluginApiV1>());
    let dir = tempfile::tempdir().unwrap();
    for sub in ["data", "logs"] {
        std::fs::create_dir(dir.path().join(sub)).unwrap();
    }
    let input=serde_json::to_vec(&json!({"config":config,"context":{"pluginId":"dev.codey.antigravity-router","pluginDir":dir.path(),"dataDir":dir.path().join("data"),"logDir":dir.path().join("logs")}})).unwrap();
    let mut instance = std::ptr::null_mut();
    let mut output = Buffer::default();
    let status = unsafe { (api.create)(input.as_ptr(), input.len(), &mut instance, &mut output) };
    let result = decode(&api, output);
    if status != 0 {
        assert!(instance.is_null());
        return Err(result.to_string());
    }
    Ok(Native {
        api,
        instance,
        _library: library,
        _dir: dir,
    })
}
fn decode(api: &PluginApiV1, output: Buffer) -> Value {
    let bytes = if output.len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(output.data, output.len) }
    };
    let v = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    unsafe { (api.free_buffer)(output) };
    v
}
fn invoke(n: &mut Native, m: &str, p: Value) -> Value {
    let input = serde_json::to_vec(&json!({"method":m,"params":p})).unwrap();
    let mut output = Buffer::default();
    let s = unsafe { (n.api.invoke)(n.instance, input.as_ptr(), input.len(), &mut output) };
    let v = decode(&n.api, output);
    assert_eq!(s, 0, "{v}");
    v
}
fn event(route: &str, attempt: u32) -> Value {
    json!({"attempt":attempt,"metadata":{"routeId":route},"response":{"status":429}})
}
#[test]
fn abi_route_scoping_and_destroy() {
    let mut n = create(&library_path(), json!({"routeId":"our-route"})).unwrap();
    assert_eq!(invoke(&mut n, "ping", json!({}))["version"], "0.9.0");
    let route = invoke(&mut n, "provider.describe", json!({}));
    assert_eq!(route["name"], "Antigravity");
    assert_eq!(route["baseUrl"], "http://127.0.0.1:8787/v1");
    assert!(route["models"].as_array().unwrap().len() <= 32);
    assert!(route.get("transport").is_none());
    assert_eq!(
        invoke(&mut n, "request.afterHeaders", event("other", 0)),
        json!({"action":"continue"})
    );
    assert_eq!(
        invoke(&mut n, "request.afterHeaders", event("our-route", 0))["action"],
        "abort"
    );
    drop(n);
    let mut disabled = create(
        &library_path(),
        json!({"lifecycleEnabled":false,"routeId":"our-route","retryOnce":true}),
    )
    .unwrap();
    assert_eq!(
        invoke(&mut disabled, "request.afterHeaders", event("our-route", 0)),
        json!({"action":"continue"})
    );
    let mut defaults = create(&library_path(), json!({})).unwrap();
    let mut legacy = create(
        &library_path(),
        json!({"enabled":false,"routeId":"our-route"}),
    )
    .unwrap();
    assert_eq!(
        invoke(&mut legacy, "request.afterHeaders", event("our-route", 0)),
        json!({"action":"continue"})
    );
    assert_eq!(
        invoke(&mut defaults, "request.afterHeaders", event("any", 0)),
        json!({"action":"continue"})
    );
    let mut retry = create(
        &library_path(),
        json!({"routeId":"our-route","retryOnce":true}),
    )
    .unwrap();
    assert_eq!(
        invoke(&mut retry, "request.afterHeaders", event("our-route", 0))["action"],
        "retry"
    );
    assert_eq!(
        invoke(&mut retry, "request.afterHeaders", event("our-route", 1))["action"],
        "abort"
    );
}
#[test]
fn reject_unsafe_configuration() {
    for c in [
        json!([]),
        json!({"baseUrl":"https://evil.test/v1"}),
        json!({"baseUrl":"http://127.0.0.1:0/v1"}),
        json!({"lifecycleEnabled":"true"}),
        json!({"enabled":"true"}),
        json!({"enabled":true,"lifecycleEnabled":false}),
        json!({"models":[]}),
        json!({"models":["A","a"]}),
        json!({"models":["bad\nheader"]}),
        json!({"retryOnce":true}),
        json!({"retryOnce":2}),
        json!({"secret":"x"}),
        json!({"models":vec!["m";33]}),
    ] {
        assert!(create(&library_path(), c.clone()).is_err(), "{c}");
    }
}
