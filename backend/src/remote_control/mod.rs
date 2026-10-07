mod auth;
mod create;
mod desktop;
mod protocol;
mod server;
mod store;
mod tunnel;

use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex as AsyncMutex};

use crate::commands::AppState;

#[derive(Default)]
pub(crate) struct RemoteControl {
    running: AsyncMutex<Option<Running>>,
}

struct Running {
    address: SocketAddr,
    urls: Vec<String>,
    core: Arc<server::Core>,
    task: tokio::task::JoinHandle<()>,
    tunnel: Option<tunnel::Tunnel>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.core.shutdown.send_replace(true);
        self.task.abort();
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartOptions {
    #[serde(default = "default_bind")]
    bind_address: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    public_url: String,
    #[serde(default)]
    tunnel: bool,
}

fn default_bind() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    43129
}

impl RemoteControl {
    pub(crate) async fn shutdown(&self) {
        if let Some(running) = self.running.lock().await.take() {
            running.task.abort();
        }
    }

    async fn status(&self) -> Value {
        let guard = self.running.lock().await;
        match guard.as_ref().filter(|r| !r.task.is_finished()) {
            Some(running) => {
                let mut urls = running.urls.clone();
                if let Some(public) = &*running
                    .core
                    .public_url
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                {
                    urls.insert(0, public.clone());
                }
                json!({"running":true,"address":running.address.to_string(),"urls":urls,
                    "tunnel":running.tunnel.as_ref().map(tunnel::Tunnel::status),
                    "devices":running.core.auth.lock().unwrap_or_else(|e|e.into_inner()).devices()})
            }
            None => json!({"running":false,"devices":[],"urls":[]}),
        }
    }

    async fn start(&self, state: &Arc<AppState>, options: StartOptions) -> Result<Value, String> {
        let mut guard = self.running.lock().await;
        if guard.as_ref().is_some_and(|r| !r.task.is_finished()) {
            return Err("远程控制已启动，请先停止再修改连接设置".into());
        }
        let ip: IpAddr = options
            .bind_address
            .parse()
            .map_err(|_| "监听地址必须是本机 IP 地址")?;
        let public_url = server::validate_public_url(&options.public_url)?;
        if options.tunnel && public_url.is_some() {
            return Err("内置隧道与自定义外网地址不能同时启用".into());
        }
        let listener = TcpListener::bind(SocketAddr::new(ip, options.port))
            .await
            .map_err(|_| "远程控制监听失败，请检查本机地址、端口占用及防火墙设置")?;
        let address = listener.local_addr().map_err(|_| "无法读取远程监听地址")?;
        let mut urls = vec![format!(
            "http://{}",
            SocketAddr::new(
                if ip.is_unspecified() {
                    IpAddr::from([127, 0, 0, 1])
                } else {
                    ip
                },
                address.port()
            )
        )];
        if ip.is_unspecified() {
            // UDP connect only selects the local route; no packet is sent.
            if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
                && socket.connect("192.0.2.1:80").is_ok()
                && let Ok(local) = socket.local_addr()
                && !local.ip().is_loopback()
                && !local.ip().is_unspecified()
            {
                urls.insert(
                    0,
                    format!("http://{}", SocketAddr::new(local.ip(), address.port())),
                );
            }
        }
        let core = Arc::new(server::Core {
            auth: Mutex::new(auth::Auth::default()),
            state: Arc::downgrade(state),
            public_url: RwLock::new(public_url),
            address,
            actions: AsyncMutex::new(Default::default()),
            shutdown: tokio::sync::watch::channel(false).0,
            streams: Arc::new(tokio::sync::Semaphore::new(8)),
        });
        let task_core = Arc::clone(&core);
        let task = tokio::spawn(server::serve(listener, task_core));
        let tunnel = options
            .tunnel
            .then(|| tunnel::Tunnel::start(Arc::clone(&core)));
        *guard = Some(Running {
            address,
            urls,
            core,
            task,
            tunnel,
        });
        drop(guard);
        Ok(self.status().await)
    }

    async fn pairing(&self, url: &str) -> Result<Value, String> {
        let guard = self.running.lock().await;
        let running = guard
            .as_ref()
            .filter(|r| !r.task.is_finished())
            .ok_or("请先启动远程控制")?;
        if !running.urls.iter().any(|candidate| candidate == url)
            && running
                .core
                .public_url
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref()
                != Some(url)
        {
            return Err("请选择当前远程服务的连接地址".into());
        }
        let code = running
            .core
            .auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pairing_code();
        let link = format!("{url}/#pair={code}");
        let qr = qrcode::QrCode::new(link.as_bytes())
            .map_err(|_| "生成配对二维码失败")?
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(240, 240)
            .build();
        Ok(
            json!({"url":link,"qrCode":format!("data:image/svg+xml;base64,{}",STANDARD.encode(qr)),"expiresIn":Duration::from_secs(600).as_secs()}),
        )
    }
}

pub(crate) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    match command {
        "remote_control_status" => Ok(state.remote_control.status().await),
        "start_remote_control" => {
            let options: StartOptions =
                serde_json::from_value(args.clone()).map_err(|_| "远程连接设置无效")?;
            state.remote_control.start(state, options).await
        }
        "stop_remote_control" => {
            state.remote_control.shutdown().await;
            Ok(state.remote_control.status().await)
        }
        "pair_remote_control" => {
            state
                .remote_control
                .pairing(args["url"].as_str().ok_or("缺少连接地址")?)
                .await
        }
        "revoke_remote_device" => {
            let guard = state.remote_control.running.lock().await;
            let running = guard.as_ref().ok_or("远程控制未运行")?;
            running
                .core
                .auth
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .revoke(args["id"].as_str().ok_or("缺少设备标识")?);
            Ok(json!({"status":"ok"}))
        }
        _ => Err("未知远程管理操作".into()),
    }
}
