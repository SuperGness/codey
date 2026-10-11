// Exercise the host independently of the desktop application and its UI runtime.
// 宿主模块通过 `crate::fs_util` 取哈希与原子写实现，这里按同一路径接入，
// 使被引入的插件模块能在不带桌面端其余依赖的前提下编译。
#[allow(dead_code)]
#[path = "../../../backend/src/fs_util.rs"]
mod fs_util;
// 独立宿主测试不调用桌面路由入口；未使用代码检查仍由后端 crate 覆盖。
#[allow(dead_code)]
#[path = "../../../backend/src/codey_plugins/mod.rs"]
mod host;

use host::lifecycle::{
    LifecycleDecision, LifecycleOutcome, LifecycleRequest, LifecycleResponse, LifecycleStage,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, env::consts, fs, io::Write, path::Path, process::Command};

const PLUGIN_ID: &str = "dev.codey.header-demo";
const INITIAL_CONFIG: &str = concat!(
    "{\n  \"_comments\": {\"value\": \"用于请求头\"},\n",
    "  \"value\": \"hello-codey\",\n",
    "  \"nested\": {\"_comments\": {\"enabled\": \"是否启用\"}, \"enabled\": true},\n",
    "  \"rules\": [{\"_comments\": {\"model\": \"模型\"}, \"model\": \"gpt6\"}],\n",
    "  \"_business\": 42, \"_commentsLike\": \"原样保留\"\n}\n",
);

fn package(path: &Path, library: &[u8], version: &str) {
    package_with_options(path, library, version, "x-plugin-demo", false);
}

fn package_with_options(path: &Path, library: &[u8], version: &str, header: &str, transport: bool) {
    let filename = library_filename("plugin");
    let manifest = json!({
        "id":PLUGIN_ID,"name":"Demo","version":version,
        "abiVersion":1,"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "entry":format!("lib/{filename}"),"librarySha256":format!("{:x}",Sha256::digest(library)),
        "capabilities": if transport { vec!["request.lifecycle.v1", "provider.route.v1", "provider.transport.v1", "provider.account.v1"] } else { vec!["request.lifecycle.v1"] },
        "headerNames":[header]
    });
    let mut archive = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in [
        (
            "manifest.json".to_owned(),
            serde_json::to_vec(&manifest).unwrap(),
        ),
        ("config.json".to_owned(), INITIAL_CONFIG.as_bytes().to_vec()),
        (format!("lib/{filename}"), library.to_vec()),
    ] {
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(&bytes).unwrap();
    }
    archive.finish().unwrap();
}

fn library_filename(name: &str) -> String {
    format!("{}{name}{}", consts::DLL_PREFIX, consts::DLL_SUFFIX)
}

fn build_plugin(root: &Path) -> Vec<u8> {
    // Compile a separate consumer project, with no Codey workspace membership.
    let project = root.join("standalone-plugin");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        include_str!("fixtures/header_plugin.rs"),
    )
    .unwrap();
    let sdk_path = serde_json::to_string(env!("CARGO_MANIFEST_DIR")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        format!(
            r#"[package]
name = "codey-plugin-header-demo"
version = "0.1.0"
edition = "2024"
[lib]
crate-type = ["cdylib"]
[dependencies]
codey-plugin-sdk = {{ path = {sdk_path} }}
[workspace]
"#
        ),
    )
    .unwrap();
    let target = root.join("target");
    let build = Command::new("cargo")
        .args(["build", "--offline", "--target-dir"])
        .arg(&target)
        .arg("--config")
        .arg(format!("build.build-dir={:?}", root.join("build")))
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let path = target
        .join("debug")
        .join(library_filename("codey_plugin_header_demo"));
    unsafe {
        let library = libloading::Library::new(&path).unwrap();
        assert!(
            library
                .get::<codey_plugin_sdk::EntryPoint>(b"codey_plugin_entry_v1")
                .is_ok()
        );
        assert!(
            library
                .get::<codey_plugin_sdk::EntryPoint>(b"codey_plugin_entry_with_context_v1")
                .is_err()
        );
    }
    fs::read(path).unwrap()
}

fn uninstall_loaded_plugin(root: &Path, remove_data: bool) {
    #[cfg(windows)]
    let trash_before: std::collections::BTreeSet<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();

    let result = host::uninstall(PLUGIN_ID, remove_data);
    #[cfg(not(windows))]
    {
        let _ = root;
        result.unwrap();
    }
    #[cfg(windows)]
    if let Err(error) = result {
        // The host pins native mappings until process exit, so Windows may keep
        // a loaded DLL in the trash after the uninstall state has committed.
        let trash: Vec<_> = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| !trash_before.contains(path))
            .collect();
        assert_eq!(trash.len(), 1, "{error}");
        assert!(
            trash[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".trash-"),
            "{error}"
        );
        let expected = format!(
            "插件已卸载，但文件清理失败；请退出 Codey 后删除 {}: ",
            trash[0].canonicalize().unwrap().display()
        );
        assert!(error.starts_with(&expected), "{error}");
        assert!(error.ends_with("(os error 5)"), "{error}");
        let mut pending = trash;
        let mut retained_dll = false;
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else if entry.file_name() == "plugin.dll" {
                    retained_dll = true;
                }
            }
        }
        assert!(retained_dll, "{error}");
    }
    assert!(host::list().unwrap().plugins.is_empty());
}

#[tokio::test]
async fn complete_native_plugin_lifecycle() {
    // 预检查不能初始化管理器、创建目录或加载插件；后续安装仍须显式初始化。
    assert!(host::list().is_err());
    let info = host::host_info();
    assert_eq!(info.platform, consts::OS);
    assert_eq!(info.arch, consts::ARCH);
    assert!(codey_plugin_sdk::config::validate(INITIAL_CONFIG).valid);
    assert!(host::list().is_err());
    if let Some(root) = std::env::var_os("CODEY_TEST_TRANSPORT_STARTUP_ROOT") {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        host::set_route_handler(std::sync::Arc::new(move |id, change| {
            if matches!(change, host::RouteChange::Upsert { .. }) {
                assert!(
                    host::transport::Instance::capture(id)
                        .unwrap()
                        .open()
                        .is_ok(),
                    "startup published the route before its instance"
                );
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(Some("fixture-route".into()));
            }
            Ok(None)
        }));
        host::initialize(root.into()).unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(host::list().unwrap().plugins[0].enabled);
        host::shutdown();
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let library = build_plugin(temp.path());
    let path = temp.path().join("demo.codey-plugin");
    package(&path, &library, "1.0.0");
    host::initialize(temp.path().join("host")).unwrap();
    let inspection = host::inspect(&path).unwrap();
    assert!(host::install(&path, "wrong-hash").is_err());
    assert!(host::list().unwrap().plugins.is_empty());
    let installed = host::install(&path, &inspection.sha256).unwrap();
    assert!(!installed.plugins[0].enabled);
    let initial_config = host::get_config_file(PLUGIN_ID).unwrap();
    assert_eq!(initial_config.content, INITIAL_CONFIG);
    assert_eq!(initial_config.plugin_id, PLUGIN_ID);
    assert_eq!(initial_config.version, "1.0.0");
    assert_eq!(initial_config.path, installed.plugins[0].config_path);
    assert_eq!(
        fs::read_to_string(&initial_config.path).unwrap(),
        initial_config.content
    );
    assert!(host::invoke(PLUGIN_ID, "ping", json!(null)).is_err());
    // A failed consent persistence must not publish a newly loaded instance.
    let state_path = temp.path().join("host/state.json");
    let saved_state = temp.path().join("saved-state.json");
    fs::rename(&state_path, &saved_state).unwrap();
    fs::create_dir(&state_path).unwrap();
    assert!(host::set_enabled(PLUGIN_ID, true).is_err());
    assert!(!LifecycleRequest::new(json!({}), None).is_active());
    assert!(host::invoke(PLUGIN_ID, "ping", json!(null)).is_err());
    assert!(!host::list().unwrap().plugins[0].enabled);
    fs::remove_dir(&state_path).unwrap();
    fs::rename(&saved_state, &state_path).unwrap();
    host::set_enabled(PLUGIN_ID, true).unwrap();
    assert_eq!(
        host::invoke(PLUGIN_ID, "config.received", json!(null)).unwrap(),
        json!({
            "value": "hello-codey",
            "nested": {"enabled": true},
            "rules": [{"model": "gpt6"}],
            "_business": 42,
            "_commentsLike": "原样保留"
        })
    );
    let context = host::invoke(PLUGIN_ID, "storage.context", json!(null)).unwrap();
    let plugin_dir = temp
        .path()
        .join("host/installed/dev.codey.header-demo")
        .canonicalize()
        .unwrap();
    assert_eq!(host::plugin_directory(PLUGIN_ID).unwrap(), plugin_dir);
    assert!(host::plugin_directory("../dev.codey.header-demo").is_err());
    assert!(host::plugin_directory("dev.codey.missing").is_err());
    assert_eq!(context["pluginDir"], plugin_dir.to_str().unwrap());
    assert_eq!(
        context["dataDir"],
        plugin_dir.join("data").to_str().unwrap()
    );
    assert_eq!(context["logDir"], plugin_dir.join("logs").to_str().unwrap());
    host::invoke(PLUGIN_ID, "storage.write", json!({"saved":42})).unwrap();
    assert!(plugin_dir.join("logs/plugin.log").exists());
    assert!(plugin_dir.join("logs/host.log").exists());
    let cleared = host::clear_logs(PLUGIN_ID).unwrap();
    assert_eq!(cleared.plugins[0].log_size_bytes, Some(0));
    assert_eq!(
        host::invoke(PLUGIN_ID, "ping", json!({"echo":1})).unwrap()["value"],
        "hello-codey"
    );
    for invalid in [
        "{",
        "[]",
        "null",
        "{\"_comments\":null}",
        "{\"rules\":[{\"_comments\":{\"model\":42}}]}",
    ] {
        assert!(host::save_config_file(PLUGIN_ID, invalid, &initial_config.sha256).is_err());
    }
    let mut request = LifecycleRequest::new(
        json!({"requestId":"test-request","accountId":"account-handle"}),
        None,
    );
    assert!(request.is_active());
    let LifecycleDecision::Continue(patched) = request
        .dispatch(
            LifecycleStage::BeforeSend,
            0,
            BTreeMap::from([("authorization".into(), "Bearer secret".into())]),
            None,
        )
        .await
        .unwrap()
    else {
        panic!("expected continue");
    };
    assert_eq!(patched.len(), 1);
    assert_eq!(patched[0].name, "x-plugin-demo");
    assert_eq!(patched[0].value.as_deref(), Some("hello-codey"));
    assert!(
        matches!(request.dispatch(LifecycleStage::AfterHeaders, 0, BTreeMap::new(), Some(LifecycleResponse {
        status: 200, headers: BTreeMap::new(),
    })).await.unwrap(), LifecycleDecision::Continue(headers) if headers.is_empty())
    );
    request.finish(LifecycleOutcome::Completed, Some(200), None);
    assert!(!request.is_active());
    // 直接调用插件会占用实例锁，可能让尽力发送的终态通知被跳过。
    // 从夹具文件观察回调结果，避免轮询与终态回调争用实例。
    let terminal_path = plugin_dir.join("data/terminal-received.txt");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fs::read_to_string(&terminal_path).ok().as_deref() == Some("request.completed") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let comment_edit = INITIAL_CONFIG.replace("用于请求头", "更新说明");
    let configured =
        host::save_config_file(PLUGIN_ID, &comment_edit, &initial_config.sha256).unwrap();
    assert!(!configured.plugins[0].restart_required);
    let comment_config = host::get_config_file(PLUGIN_ID).unwrap();
    assert_eq!(comment_config.content, comment_edit);
    assert_ne!(comment_config.sha256, initial_config.sha256);
    assert!(
        host::save_config_file(PLUGIN_ID, INITIAL_CONFIG, &initial_config.sha256)
            .err()
            .unwrap()
            .contains("外部修改")
    );
    assert_eq!(
        fs::read_to_string(&initial_config.path).unwrap(),
        comment_edit
    );
    // External comment edits also change the file hash, but not effective runtime config.
    let external_comment_edit = comment_edit.replace("是否启用", "外部编辑说明");
    fs::write(&initial_config.path, &external_comment_edit).unwrap();
    assert!(!host::list().unwrap().plugins[0].restart_required);
    assert!(
        host::save_config_file(PLUGIN_ID, &comment_edit, &comment_config.sha256)
            .err()
            .unwrap()
            .contains("外部修改")
    );
    let current_config = host::get_config_file(PLUGIN_ID).unwrap();
    let edited_config = "{\n    \"value\": \"new-value\"\n}\n";
    let configured =
        host::save_config_file(PLUGIN_ID, edited_config, &current_config.sha256).unwrap();
    assert_eq!(
        fs::read_to_string(&initial_config.path).unwrap(),
        edited_config
    );
    assert!(host::save_config_file(PLUGIN_ID, "{}", &initial_config.sha256).is_err());
    assert_eq!(
        host::get_config_file(PLUGIN_ID).unwrap().content,
        edited_config
    );
    assert!(configured.plugins[0].restart_required);
    assert_eq!(
        host::invoke(PLUGIN_ID, "ping", json!(null)).unwrap()["value"],
        "hello-codey"
    );
    host::set_enabled(PLUGIN_ID, false).unwrap();
    host::set_enabled(PLUGIN_ID, true).unwrap();
    assert_eq!(
        host::invoke(PLUGIN_ID, "storage.read", json!(null)).unwrap(),
        json!({"saved":42})
    );
    assert_eq!(
        host::invoke(PLUGIN_ID, "storage.context", json!(null)).unwrap(),
        context
    );
    assert_eq!(
        host::invoke(PLUGIN_ID, "ping", json!(null)).unwrap()["value"],
        "new-value"
    );
    package(&path, &library, "1.1.0");
    let inspection = host::inspect(&path).unwrap();
    let upgraded = host::install(&path, &inspection.sha256).unwrap();
    assert!(upgraded.plugins[0].enabled && upgraded.plugins[0].restart_required);
    assert!(host::uninstall(PLUGIN_ID, false).is_err());
    host::set_enabled(PLUGIN_ID, false).unwrap();
    #[cfg(unix)]
    for relative in ["installed", "installed/dev.codey.header-demo"] {
        let source = temp.path().join("host").join(relative);
        let outside = temp.path().join("outside-installed");
        fs::rename(&source, &outside).unwrap();
        let entries_before = fs::read_dir(&outside).unwrap().count();
        std::os::unix::fs::symlink(&outside, &source).unwrap();
        assert!(host::uninstall(PLUGIN_ID, true).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), entries_before);
        assert_eq!(host::list().unwrap().plugins.len(), 1);
        fs::remove_file(&source).unwrap();
        fs::rename(&outside, &source).unwrap();
    }
    uninstall_loaded_plugin(&temp.path().join("host"), false);
    assert!(plugin_dir.join("data/example.json").exists());
    assert!(plugin_dir.join("logs/plugin.log").exists());
    assert!(!plugin_dir.join("versions").exists());
    host::install(&path, &inspection.sha256).unwrap();
    assert_eq!(
        host::get_config_file(PLUGIN_ID).unwrap().content,
        edited_config
    );
    host::set_enabled(PLUGIN_ID, true).unwrap();
    assert_eq!(
        host::invoke(PLUGIN_ID, "storage.read", json!(null)).unwrap(),
        json!({"saved":42})
    );
    host::set_enabled(PLUGIN_ID, false).unwrap();
    uninstall_loaded_plugin(&temp.path().join("host"), true);
    assert!(!plugin_dir.exists());
    host::install(&path, &inspection.sha256).unwrap();
    assert_eq!(
        host::get_config_file(PLUGIN_ID).unwrap().content,
        initial_config.content
    );
    package_with_options(&path, &library, "1.2.0", "x-different-header", false);
    let inspection = host::inspect(&path).unwrap();
    host::install(&path, &inspection.sha256).unwrap();
    host::set_enabled(PLUGIN_ID, true).unwrap();
    let mut request = LifecycleRequest::new(json!({"requestId":"unauthorized-header"}), None);
    let error = request
        .dispatch(LifecycleStage::BeforeSend, 0, BTreeMap::new(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, "plugin_invalid_headers");
    request.finish(LifecycleOutcome::Failed, None, Some(&error.code));
    package(&path, &library, "1.3.0");
    let inspection = host::inspect(&path).unwrap();
    host::install(&path, &inspection.sha256).unwrap();
    let config_file = host::get_config_file(PLUGIN_ID).unwrap();
    host::save_config_file(PLUGIN_ID, "{\"value\":\"recovered\"}", &config_file.sha256).unwrap();
    host::set_enabled(PLUGIN_ID, false).unwrap();
    host::set_enabled(PLUGIN_ID, true).unwrap();
    assert_eq!(
        host::invoke(PLUGIN_ID, "ping", json!(null)).unwrap()["value"],
        "recovered"
    );
    assert_eq!(
        host::invoke(PLUGIN_ID, "storage.context", json!(null)).unwrap(),
        context
    );
    host::set_enabled(PLUGIN_ID, false).unwrap();
    package_with_options(&path, &library, "1.4.0", "x-plugin-demo", true);
    let inspection = host::inspect(&path).unwrap();
    host::install(&path, &inspection.sha256).unwrap();
    host::set_route_handler(std::sync::Arc::new(|id, change| {
        // 同步重入模拟并发管理调用，不依赖调度或 sleep。
        assert!(host::set_enabled(id, true).is_err());
        assert!(host::set_enabled(id, false).is_err());
        if matches!(change, host::RouteChange::Upsert { .. }) {
            assert!(
                host::transport::Instance::capture(id)
                    .unwrap()
                    .open()
                    .is_ok()
            );
            Ok(Some("fixture-route".into()))
        } else {
            Ok(None)
        }
    }));
    host::set_enabled(PLUGIN_ID, true).unwrap();
    host::set_enabled(PLUGIN_ID, false).unwrap();
    host::set_enabled(PLUGIN_ID, true).unwrap();
    host::shutdown();
    assert!(!LifecycleRequest::new(json!({}), None).is_active());
    assert!(host::invoke(PLUGIN_ID, "ping", json!(null)).is_err());
    assert!(host::set_enabled(PLUGIN_ID, true).is_err());
    let restarted = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "complete_native_plugin_lifecycle", "--nocapture"])
        .env(
            "CODEY_TEST_TRANSPORT_STARTUP_ROOT",
            temp.path().join("host"),
        )
        .output()
        .unwrap();
    assert!(
        restarted.status.success(),
        "restart failed: {} {}",
        String::from_utf8_lossy(&restarted.stdout),
        String::from_utf8_lossy(&restarted.stderr)
    );
}

#[test]
fn inspection_does_not_execute_libraries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("fake.codey-plugin");
    package(&path, b"this is not a native library", "1.0.0");
    assert!(host::inspect(&path).is_ok());
}

#[test]
fn archive_traversal_duplicates_and_symlinks_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    for (index, names) in [
        vec!["../escape"],
        vec!["manifest.json", "MANIFEST.JSON"],
        vec!["dir", "dir/file"],
    ]
    .iter()
    .enumerate()
    {
        let path = temp.path().join(format!("bad{index}.codey-plugin"));
        let mut archive = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        for name in names {
            archive
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"{}").unwrap();
        }
        archive.finish().unwrap();
        assert!(host::inspect(&path).is_err());
    }
    let path = temp.path().join("symlink.codey-plugin");
    let mut archive = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    archive
        .add_symlink(
            "linked",
            "/tmp/outside",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    archive.finish().unwrap();
    assert!(host::inspect(&path).is_err());
}
