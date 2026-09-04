#![cfg(target_os = "macos")]

use std::io::Read;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Duration;

#[test]
fn wrapper_confirms_exec_and_forwards_arguments_without_leaking_its_environment() {
    for executable in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("fake codex");
        std::fs::write(
            &target,
            "#!/bin/sh\n[ -z \"${CODEX_CLI_PATH}${CODEY_CODEX_CLI_WRAPPER_TARGET}${CODEY_CODEX_CLI_WRAPPER_OVERRIDES}${CODEY_CODEX_CLI_WRAPPER_TOKEN}${CODEY_CODEX_CLI_WRAPPER_PORT}\" ] || exit 99\nprintf '%s\\n' \"$@\"\nexit 17\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &target,
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_codey"))
            .env_clear()
            .env("HOME", temp.path())
            .env("CODEX_HOME", temp.path())
            .env("PATH", "/usr/bin:/bin")
            .env("CODEX_CLI_PATH", "must-not-leak")
            .env("CODEY_CODEX_CLI_WRAPPER_TARGET", &target)
            .env(
                "CODEY_CODEX_CLI_WRAPPER_OVERRIDES",
                r#"["model=\"audit-model\""]"#,
            )
            .env("CODEY_CODEX_CLI_WRAPPER_PORT", address.port().to_string())
            .env("CODEY_CODEX_CLI_WRAPPER_TOKEN", "audit-token")
            .args([
                "app-server",
                "--analytics-default-enabled",
                "-c",
                "model=\"old\"",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("wrapper did not connect before deadline");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut handshake = String::new();
        stream.read_to_string(&mut handshake).unwrap();
        let output = child.wait_with_output().unwrap();
        if executable {
            assert_eq!(handshake, "audit-token");
            assert_eq!(output.status.code(), Some(17));
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                "app-server\n-c\nanalytics.enabled=false\n-c\nmodel=\"audit-model\"\n",
            );
        } else {
            assert_eq!(handshake, "audit-token!");
            assert!(!output.status.success());
        }
    }
}
