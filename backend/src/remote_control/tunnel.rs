//! Optional Cloudflare Quick Tunnel. Only the transport is a native helper;
//! authentication, HTTP serving and Codex control remain in Rust.
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use super::server::Core;

const MAX_DOWNLOAD: usize = 120 * 1024 * 1024;
const RELEASES: &str = "https://api.github.com/repos/cloudflare/cloudflared/releases/latest";

pub(super) struct Tunnel {
    state: Arc<Mutex<Value>>,
    task: tokio::task::JoinHandle<()>,
}

impl Tunnel {
    pub fn start(core: Arc<Core>) -> Self {
        let state = Arc::new(Mutex::new(
            json!({"status":"starting","message":"正在准备 HTTPS 隧道，首次启动需要下载组件…"}),
        ));
        let progress = Arc::clone(&state);
        let task = tokio::spawn(async move {
            if let Err(error) = run(&core, &progress).await {
                *core.public_url.write().unwrap_or_else(|e| e.into_inner()) = None;
                *progress.lock().unwrap_or_else(|e| e.into_inner()) =
                    json!({"status":"failed","message":error});
            }
        });
        Self { state, task }
    }

    pub fn status(&self) -> Value {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn asset_name() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64" | "aarch64") => Ok("cloudflared-windows-amd64.exe"),
        ("linux", "x86_64") => Ok("cloudflared-linux-amd64"),
        ("linux", "aarch64") => Ok("cloudflared-linux-arm64"),
        ("macos", "x86_64") => Ok("cloudflared-darwin-amd64.tgz"),
        ("macos", "aarch64") => Ok("cloudflared-darwin-arm64.tgz"),
        _ => Err("当前平台不支持自动准备隧道，可使用自定义 HTTPS 地址".into()),
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn cached(root: &Path) -> Option<PathBuf> {
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(root.join("installed.json")).ok()?).ok()?;
    let checksum = metadata["sha256"].as_str().filter(|s| valid_hash(s))?;
    let path = root.join(checksum).join(if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    });
    let size = std::fs::metadata(&path).ok()?.len();
    if size > MAX_DOWNLOAD as u64 {
        return None;
    }
    (hash(&std::fs::read(&path).ok()?) == checksum).then_some(path)
}

async fn fetch(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "下载隧道组件失败，请检查到 GitHub 的网络连接")?
        .error_for_status()
        .map_err(|_| "隧道组件下载服务暂不可用，请稍后重试")?;
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err("隧道组件下载过大".into());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "隧道组件下载中断")?;
        if bytes.len() + chunk.len() > limit {
            return Err("隧道组件下载过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn release_asset(release: &Value, name: &str) -> Result<(String, String), String> {
    let asset = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == name)
        .ok_or("官方发行版缺少当前平台的隧道组件")?;
    let url = asset["browser_download_url"]
        .as_str()
        .filter(|url| {
            url.starts_with("https://github.com/cloudflare/cloudflared/releases/download/")
        })
        .ok_or("隧道组件下载地址无效")?;
    let digest = asset["digest"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| valid_hash(s))
        .ok_or("隧道组件缺少官方 SHA-256 校验值，已取消下载")?;
    Ok((url.into(), digest.to_ascii_lowercase()))
}

#[cfg(target_os = "macos")]
fn unpack(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    for entry in archive.entries().map_err(|_| "隧道压缩包无效")? {
        let entry = entry.map_err(|_| "隧道压缩包无效")?;
        if entry
            .path()
            .ok()
            .is_some_and(|p| p.as_ref() == Path::new("cloudflared"))
            && entry.header().entry_type().is_file()
        {
            let mut executable = Vec::new();
            entry
                .take(MAX_DOWNLOAD as u64 + 1)
                .read_to_end(&mut executable)
                .map_err(|_| "解压隧道组件失败")?;
            if executable.len() > MAX_DOWNLOAD {
                return Err("隧道组件过大".into());
            }
            return Ok(executable);
        }
    }
    Err("压缩包缺少隧道程序".into())
}

#[cfg(not(target_os = "macos"))]
fn unpack(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    Ok(bytes)
}

async fn executable() -> Result<PathBuf, String> {
    let root = crate::config::default_config_path()
        .parent()
        .ok_or("无法定位 Codey 配置目录")?
        .join("remote-tunnel");
    let cache_root = root.clone();
    if let Some(path) = tokio::task::spawn_blocking(move || cached(&cache_root))
        .await
        .map_err(|_| "读取隧道组件失败")?
    {
        return Ok(path);
    }
    let client = reqwest::Client::builder()
        .user_agent("Codey-remote-control")
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "创建隧道下载连接失败")?;
    let release: Value = serde_json::from_slice(&fetch(&client, RELEASES, 2 * 1024 * 1024).await?)
        .map_err(|_| "隧道发行信息无效")?;
    let (url, checksum) = release_asset(&release, asset_name()?)?;
    let bytes = fetch(&client, &url, MAX_DOWNLOAD).await?;
    if hash(&bytes) != checksum {
        return Err("隧道组件校验失败，已取消安装".into());
    }
    tokio::task::spawn_blocking(move || {
        let bytes = unpack(bytes)?;
        let checksum = hash(&bytes);
        let directory = root.join(&checksum);
        std::fs::create_dir_all(&directory).map_err(|_| "创建隧道组件目录失败")?;
        let path = directory.join(if cfg!(windows) {
            "cloudflared.exe"
        } else {
            "cloudflared"
        });
        crate::fs_util::atomic_write_private(&path, &bytes).map_err(|_| "保存隧道组件失败")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "设置隧道组件权限失败")?;
        }
        crate::fs_util::atomic_write_private(
            &root.join("installed.json"),
            json!({"sha256":checksum}).to_string().as_bytes(),
        )
        .map_err(|_| "保存隧道组件信息失败")?;
        Ok(path)
    })
    .await
    .map_err(|_| "安装隧道组件失败")?
}

fn tunnel_url(line: &str) -> Option<String> {
    line.split_whitespace()
        .filter_map(|s| reqwest::Url::parse(s.trim_matches(['|', '"'])).ok())
        .find_map(|url| {
            let host = url.host_str()?;
            let prefix = host.strip_suffix(".trycloudflare.com")?;
            (url.scheme() == "https"
                && !prefix.is_empty()
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none())
            .then(|| url.origin().ascii_serialization())
        })
}

async fn run(core: &Core, progress: &Mutex<Value>) -> Result<(), String> {
    let executable = executable().await?;
    let directory = tempfile::tempdir().map_err(|_| "创建临时隧道目录失败")?;
    let config = directory.path().join("config.yml");
    crate::fs_util::atomic_write_private(&config, b"{}\n").map_err(|_| "准备隧道配置失败")?;
    let address = SocketAddr::new(
        if core.address.ip().is_unspecified() {
            if core.address.is_ipv6() {
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            } else {
                IpAddr::from([127, 0, 0, 1])
            }
        } else {
            core.address.ip()
        },
        core.address.port(),
    );
    let mut command = Command::new(executable);
    command
        .args(["tunnel", "--no-autoupdate", "--config"])
        .arg(config)
        .args(["--url", &format!("http://{address}"), "--protocol", "http2"])
        .args(["--metrics", "127.0.0.1:0", "--grace-period", "2s"])
        .env_remove("TUNNEL_TOKEN")
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|_| "启动隧道组件失败")?;
    let mut lines = BufReader::new(child.stderr.take().ok_or("隧道组件缺少状态输出")?).lines();
    let started = tokio::time::timeout(Duration::from_secs(60), async {
        let mut assigned = None;
        let mut registered = false;
        while let Some(line) = lines.next_line().await.map_err(|_| "读取隧道状态失败")? {
            if let Some(url) = tunnel_url(&line) {
                assigned = Some(url);
                *progress.lock().unwrap_or_else(|e| e.into_inner()) =
                    json!({"status":"starting","message":"已分配外网地址，正在连接 Cloudflare…"});
            }
            registered |= line.contains("Registered tunnel connection");
            if registered && let Some(url) = assigned.take() {
                return Ok::<_, String>(url);
            }
        }
        Err("隧道未建立，请检查网络后重新开启远程控制".into())
    })
    .await
    .map_err(
        |_| "隧道连接超时，请检查网络是否允许 Cloudflare 隧道连接，或改用自定义 HTTPS 地址",
    )??;
    *core.public_url.write().unwrap_or_else(|e| e.into_inner()) = Some(started.clone());
    *progress.lock().unwrap_or_else(|e| e.into_inner()) =
        json!({"status":"running","url":started,"message":"HTTPS 临时隧道已开启"});
    // Drain diagnostics without writing URLs, network details or credentials to logs.
    while lines
        .next_line()
        .await
        .map_err(|_| "读取隧道状态失败")?
        .is_some()
    {}
    let _ = child.wait().await;
    Err("隧道已退出；局域网仍可使用，请重新开启远程控制以恢复外网连接".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_public_tunnel_url_and_release_digest() {
        assert_eq!(
            tunnel_url("| https://a-b.trycloudflare.com |"),
            Some("https://a-b.trycloudflare.com".into())
        );
        for line in [
            "https://trycloudflare.com",
            "https://a.trycloudflare.com.evil.com",
            "http://a.trycloudflare.com",
            "https://u:p@a.trycloudflare.com",
            "https://a.trycloudflare.com/path",
        ] {
            assert!(tunnel_url(line).is_none());
        }
        let mut release = json!({"assets":[{"name":"test","browser_download_url":"https://github.com/cloudflare/cloudflared/releases/download/v/test","digest":format!("sha256:{}","a".repeat(64))}]});
        assert!(release_asset(&release, "test").is_ok());
        release["assets"][0]["digest"] = Value::Null;
        assert!(release_asset(&release, "test").is_err());
    }

    #[test]
    fn cache_rejects_tampered_binaries_and_path_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let checksum = hash(b"binary");
        let dir = temp.path().join(&checksum);
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join(if cfg!(windows) {
            "cloudflared.exe"
        } else {
            "cloudflared"
        });
        std::fs::write(&file, b"binary").unwrap();
        std::fs::write(
            temp.path().join("installed.json"),
            json!({"sha256":checksum}).to_string(),
        )
        .unwrap();
        assert_eq!(cached(temp.path()), Some(file.clone()));
        std::fs::write(file, b"tampered").unwrap();
        assert!(cached(temp.path()).is_none());
        std::fs::write(
            temp.path().join("installed.json"),
            r#"{"sha256":"../outside"}"#,
        )
        .unwrap();
        assert!(cached(temp.path()).is_none());
    }
}
