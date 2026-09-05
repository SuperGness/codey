#![cfg(any(windows, target_os = "macos"))]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

fn wrapper_command(home: &Path, target: &Path, port: u16, overrides: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codey"));
    command
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home)
        .env("APPDATA", home)
        .env("CODEX_HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("CODEX_CLI_PATH", "must-not-leak")
        .env("CODEY_CODEX_CLI_WRAPPER_TARGET", target)
        .env("CODEY_CODEX_CLI_WRAPPER_OVERRIDES", overrides)
        .env("CODEY_CODEX_CLI_WRAPPER_PORT", port.to_string())
        .env("CODEY_CODEX_CLI_WRAPPER_TOKEN", "audit-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.env("SystemRoot", std::env::var_os("SystemRoot").unwrap());
    command
}

async fn handshake(listener: TcpListener, child: &mut Child) -> Vec<u8> {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.take(8193).read_to_end(&mut bytes).await.unwrap();
        bytes
    })
    .await;
    if result.is_err() {
        let _ = child.kill().await;
    }
    result.expect("the wrapper must report launch success or failure before the deadline")
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn wrapper_confirms_exec_and_forwards_arguments_without_leaking_its_environment() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("fake codex");
    std::fs::write(&target,
        "#!/bin/sh\n[ -z \"${CODEX_CLI_PATH}${CODEY_CODEX_CLI_WRAPPER_TARGET}${CODEY_CODEX_CLI_WRAPPER_OVERRIDES}${CODEY_CODEX_CLI_WRAPPER_TOKEN}${CODEY_CODEX_CLI_WRAPPER_PORT}\" ] || exit 99\nprintf '%s\\n' \"$@\"\nexit 17\n",
    ).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mut child = wrapper_command(
        temp.path(),
        &target,
        listener.local_addr().unwrap().port(),
        r#"["model=\"audit-model\""]"#,
    )
    .args([
        "app-server",
        "--analytics-default-enabled",
        "-c",
        "model=\"old\"",
    ])
    .spawn()
    .unwrap();
    assert_eq!(handshake(listener, &mut child).await, b"audit-token");
    let output = child.wait_with_output().await.unwrap();
    assert_eq!(output.status.code(), Some(17));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "app-server\n-c\nanalytics.enabled=false\n-c\nmodel=\"audit-model\"\n"
    );
}

#[tokio::test]
async fn wrapper_reports_validation_and_process_creation_errors_instead_of_timing_out() {
    for (target_exists, overrides, expected) in [
        (false, "[]", "兼容目标无效"),
        (true, "invalid json", "解析 Codex CLI 兼容运行时配置失败"),
        (true, "[]", "启动 Codex CLI 失败"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("invalid-cli.exe");
        if target_exists {
            std::fs::write(&target, "not an executable").unwrap();
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut child = wrapper_command(
            temp.path(),
            &target,
            listener.local_addr().unwrap().port(),
            overrides,
        )
        .arg("app-server")
        .spawn()
        .unwrap();
        let bytes = handshake(listener, &mut child).await;
        let payload = bytes
            .strip_prefix(b"audit-token!")
            .expect("an explicit failure marker is required");
        let failure: serde_json::Value = serde_json::from_slice(payload).unwrap();
        assert_eq!(failure["retryable"], false);
        assert!(
            failure["message"].as_str().unwrap().contains(expected),
            "{failure}"
        );
        assert!(!child.wait_with_output().await.unwrap().status.success());
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_wrapper_classifies_a_sharing_violation_as_retryable() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let source =
        std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    let target = temp.path().join("locked-cli.exe");
    std::fs::copy(source, &target).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&target)
        .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut child = wrapper_command(temp.path(), &target, port, "[]")
        .args(["/c", "exit", "17", "app-server"])
        .spawn()
        .unwrap();
    let bytes = handshake(listener, &mut child).await;
    let failure: serde_json::Value =
        serde_json::from_slice(bytes.strip_prefix(b"audit-token!").unwrap()).unwrap();
    assert_eq!(failure["retryable"], true, "{failure}");
    assert!(!child.wait_with_output().await.unwrap().status.success());
    drop(lock);
    let output = wrapper_command(temp.path(), &target, port, "[]")
        .args(["/c", "exit", "17", "app-server"])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(17));
}

#[tokio::test]
async fn wrapper_confirms_spawn_and_allows_later_launches_after_listener_closes() {
    const CHILD_ENV: &str = "CODEY_TEST_CLI_WRAPPER_CHILD";
    if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
        std::process::exit(17);
    }
    let temp = tempfile::tempdir().unwrap();
    let target = std::env::current_exe().unwrap();
    // Run this test as the child; injected CLI arguments stay after libtest's `--`.
    let args = [
        "--exact",
        "wrapper_confirms_spawn_and_allows_later_launches_after_listener_closes",
        "--",
        "app-server",
    ];
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut child = wrapper_command(temp.path(), &target, port, "[]")
        .env(CHILD_ENV, "1")
        .args(args)
        .spawn()
        .unwrap();
    assert_eq!(handshake(listener, &mut child).await, b"audit-token");
    let output = child.wait_with_output().await.unwrap();
    assert_eq!(output.status.code(), Some(17), "{output:?}");
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        wrapper_command(temp.path(), &target, port, "[]")
            .env(CHILD_ENV, "1")
            .args(args)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(17), "{output:?}");
}
