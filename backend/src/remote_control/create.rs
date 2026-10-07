use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
};

/// Only persists a new empty thread. No turn is ever sent to this short-lived
/// app-server; the desktop takes ownership after its clean shutdown.
pub(super) async fn create(
    home: PathBuf,
    app_path: String,
    project_id: String,
    title: String,
) -> Result<Value, String> {
    if title.trim().is_empty() || title.chars().count() > 120 {
        return Err("会话名称须为 1–120 个字符".into());
    }
    let lookup_home = home.clone();
    let projects = tokio::task::spawn_blocking(move || super::store::projects(&lookup_home))
        .await
        .map_err(|_| "读取项目任务失败")??;
    let project = projects
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == project_id)
        .ok_or("请选择 Codex 中已保存的本地项目")?;
    let cwd = project["cwd"].as_str().ok_or("项目目录无效")?;
    if !Path::new(cwd).is_dir() {
        return Err("项目目录不存在，请在电脑端检查工作区".into());
    }
    let selected = (!app_path.trim().is_empty()).then(|| PathBuf::from(app_path));
    let executable = tokio::task::spawn_blocking(move || {
        codey_runtime_core::app_paths::resolve_codex_app_dir_with_saved(selected.as_deref(), None)
            .and_then(|app| codey_runtime_core::app_paths::codex_runtime_executable(&app))
    })
    .await
    .map_err(|_| "检测 Codex 运行时失败")?
    .ok_or("未找到 Codex app-server")?;
    let mut command = Command::new(executable);
    command
        .args(["app-server", "--listen", "stdio://"])
        .current_dir(cwd)
        .env("CODEX_HOME", &home)
        .env_remove("CODEX_CLI_PATH")
        .env_remove("CODEX_APP_SERVER_FORCE_CLI")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|_| "创建会话运行时启动失败")?;
    let mut stdin = child.stdin.take().ok_or("创建会话运行时缺少输入")?;
    let stdout = child.stdout.take().ok_or("创建会话运行时缺少输出")?;
    let mut stdout = BufReader::new(stdout);
    let operation = async {
        rpc(&mut stdin,&mut stdout,1,"initialize",json!({"clientInfo":{"name":"codey_remote","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        stdin
            .write_all(b"{\"method\":\"initialized\"}\n")
            .await
            .map_err(|_| "Codex 初始化失败")?;
        let result = rpc(
            &mut stdin,
            &mut stdout,
            2,
            "thread/start",
            json!({"cwd":cwd,"ephemeral":false}),
        )
        .await?;
        let id = result["thread"]["id"]
            .as_str()
            .ok_or("创建会话结果无效")?
            .to_string();
        super::desktop::validate_thread(&id)?;
        rpc(
            &mut stdin,
            &mut stdout,
            3,
            "thread/name/set",
            json!({"threadId":id,"name":title.trim()}),
        )
        .await?;
        rpc(
            &mut stdin,
            &mut stdout,
            4,
            "thread/read",
            json!({"threadId":id,"includeTurns":true}),
        )
        .await?;
        Ok::<_, String>(id)
    };
    let id = tokio::time::timeout(Duration::from_secs(40), operation)
        .await
        .map_err(|_| "创建会话超时，请先检查电脑会话列表，勿重复创建")??;
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait())
        .await
        .map_err(|_| "创建会话运行时退出超时，请先检查电脑会话列表")?
        .map_err(|_| "创建会话运行时退出失败")?;
    if !status.success() {
        return Err("创建会话运行时异常退出，请先检查电脑会话列表".into());
    }
    open(&id).await?;
    Ok(json!({"id":id}))
}

async fn rpc(
    stdin: &mut (impl AsyncWrite + Unpin),
    stdout: &mut (impl AsyncBufRead + Unpin),
    id: u8,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut bytes = json!({"id":id,"method":method,"params":params})
        .to_string()
        .into_bytes();
    bytes.push(b'\n');
    stdin
        .write_all(&bytes)
        .await
        .map_err(|_| "创建会话运行时已断开，请先检查会话列表")?;
    stdin.flush().await.map_err(|_| "创建会话运行时写入失败")?;
    loop {
        let mut line = Vec::new();
        loop {
            let buffer = stdout
                .fill_buf()
                .await
                .map_err(|_| "读取创建会话结果失败")?;
            if buffer.is_empty() {
                return Err("创建会话运行时已退出，请先检查会话列表".into());
            }
            let end = buffer.iter().position(|&b| b == b'\n');
            let count = end.map_or(buffer.len(), |n| n + 1);
            if line.len() + count > 8 * 1024 * 1024 {
                return Err("创建会话响应过大".into());
            }
            line.extend_from_slice(&buffer[..count]);
            stdout.consume(count);
            if end.is_some() {
                break;
            }
        }
        let value: Value = serde_json::from_slice(&line).map_err(|_| "创建会话响应无效")?;
        if value["id"] != id {
            continue;
        }
        if value.get("error").is_some() {
            return Err("Codex 未能创建会话，请先检查电脑会话列表".into());
        }
        return Ok(value["result"].clone());
    }
}

pub(super) async fn open(id: &str) -> Result<(), String> {
    super::desktop::validate_thread(id)?;
    let url = format!("codex://threads/{id}");
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        let mut command = {
            use std::os::windows::process::CommandExt;
            let mut command = std::process::Command::new("rundll32.exe");
            command
                .args(["url.dll,FileProtocolHandler", &url])
                .creation_flags(0x08000000);
            command
        };
        #[cfg(target_os = "macos")]
        let mut command = {
            let mut c = std::process::Command::new("open");
            c.arg(&url);
            c
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut command = {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(&url);
            c
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|_| "无法打开 Codex 会话，请在电脑端检查 codex:// 协议关联".to_string())
    })
    .await
    .map_err(|_| "打开会话任务失败")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creation_rpc_handles_notifications_errors_and_eof() {
        let (client, server) = tokio::io::duplex(4096);
        let (read, mut write) = tokio::io::split(client);
        let mut read = BufReader::new(read);
        let fixture = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "thread/start");
            server.get_mut().write_all(b"{\"method\":\"notification\"}\n{\"id\":1,\"result\":{\"thread\":{\"id\":\"test\"}}}\n{\"id\":2,\"error\":{\"message\":\"private detail\"}}\n").await.unwrap();
            line.clear();
            server.read_line(&mut line).await.unwrap();
        });
        assert_eq!(
            rpc(&mut write, &mut read, 1, "thread/start", json!({}))
                .await
                .unwrap()["thread"]["id"],
            "test"
        );
        let error = rpc(&mut write, &mut read, 2, "thread/name/set", json!({}))
            .await
            .unwrap_err();
        assert!(!error.contains("private detail"));
        fixture.await.unwrap();
        assert!(
            rpc(&mut write, &mut read, 3, "thread/read", json!({}))
                .await
                .is_err()
        );
    }
}
