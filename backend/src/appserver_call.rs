//! 多个插件共用一个 app-server。第一次调用才启动，并把模型请求留在本地路由。
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use toml_edit::{InlineTable, Value as TomlValue};

pub(crate) const HTTP_PATH: &str = "/codey/api/appserver";
const MAX_CALL_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct Route {
    pub base_url: String,
    pub token: String,
    pub requires_openai_auth: bool,
    pub supports_websockets: bool,
    pub supports_remote_compaction: bool,
}

struct SharedClient {
    route_key: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

static SHARED: Mutex<Option<SharedClient>> = Mutex::const_new(None);

pub(crate) async fn handle(route: &Route, body: &[u8]) -> (u16, Value) {
    if body.len() > MAX_CALL_BYTES {
        return rejected("app-server 请求过大");
    }
    let call = match codey_plugin_sdk::appserver::parse(body) {
        Ok(call) => call,
        Err(message) => return rejected(&message),
    };
    match call {
        codey_plugin_sdk::appserver::Call::Tasks => (
            200,
            codey_plugin_sdk::appserver::success(crate::task_activity::task_counts().await),
        ),
        codey_plugin_sdk::appserver::Call::Method { method, params } => {
            if !codey_plugin_sdk::appserver::allowed_method(&method) {
                return rejected("app-server 方法无效");
            }
            let params = Value::Object(params);
            match invoke(route, &method, &params).await {
                Ok(Reply::Result(result)) => (
                    200,
                    redact(codey_plugin_sdk::appserver::success(result), &route.token),
                ),
                Ok(Reply::Remote(error)) => (
                    200,
                    redact(
                        json!({
                            "schema": codey_plugin_sdk::appserver::SCHEMA,
                            "error": error,
                        }),
                        &route.token,
                    ),
                ),
                Err(message) => (
                    502,
                    codey_plugin_sdk::appserver::failure(redact_text(&message, &route.token)),
                ),
            }
        }
    }
}

pub(crate) async fn shutdown() {
    let mut guard = SHARED.lock().await;
    if let Some(mut client) = guard.take() {
        let _ = client.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), client.child.wait()).await;
    }
}

pub(crate) fn call_is_too_large(content_length: usize) -> bool {
    content_length > MAX_CALL_BYTES
}

fn rejected(message: &str) -> (u16, Value) {
    (400, codey_plugin_sdk::appserver::failure(message))
}

enum Reply {
    Result(Value),
    Remote(Value),
}

async fn invoke(route: &Route, method: &str, params: &Value) -> Result<Reply, String> {
    let mut guard = SHARED.lock().await;
    let key = route_key(route);
    let reusable = guard
        .as_mut()
        .is_some_and(|client| client.route_key == key && !client.exited());
    if !reusable {
        *guard = None;
        *guard = Some(SharedClient::start(route).await?);
    }
    let client = guard.as_mut().expect("共享 app-server 已启动");
    match client.request(method, params).await {
        Ok(reply) => Ok(reply),
        Err(error) => {
            *guard = None;
            Err(error)
        }
    }
}

impl SharedClient {
    async fn start(route: &Route) -> Result<Self, String> {
        let executable = codex_cli().ok_or("未找到 Codex app-server")?;
        let mut command = Command::new(executable);
        command.arg("app-server").arg("--listen").arg("stdio://");
        for config in router_overrides(route) {
            command.arg("-c").arg(config);
        }
        command
            .env("CODEX_HOME", crate::codex_config::codex_home())
            .env_remove("CODEX_CLI_PATH")
            .env_remove("CODEX_APP_SERVER_FORCE_CLI")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| "app-server 启动失败")?;
        let stdin = child.stdin.take().ok_or("app-server 缺少输入")?;
        let stdout = child.stdout.take().ok_or("app-server 缺少输出")?;
        let mut client = Self {
            route_key: route_key(route),
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        client
            .send(
                1,
                "initialize",
                &json!({
                    "clientInfo": {
                        "name": "codey",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {},
                }),
            )
            .await?;
        match client.read_reply(1, Duration::from_secs(10)).await? {
            Reply::Result(_) => Ok(client),
            Reply::Remote(_) => Err("app-server 初始化失败".into()),
        }
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    async fn request(&mut self, method: &str, params: &Value) -> Result<Reply, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(id, method, params).await?;
        self.read_reply(id, Duration::from_secs(30)).await
    }

    async fn send(&mut self, id: u64, method: &str, params: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|_| "无法编码 app-server 请求")?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|_| "无法写入 app-server")?;
        self.stdin
            .flush()
            .await
            .map_err(|_| "无法写入 app-server")?;
        Ok(())
    }

    async fn read_reply(&mut self, id: u64, timeout: Duration) -> Result<Reply, String> {
        tokio::time::timeout(timeout, async {
            loop {
                let mut line = Vec::new();
                let read = self
                    .stdout
                    .read_until(b'\n', &mut line)
                    .await
                    .map_err(|_| "无法读取 app-server")?;
                if read == 0 {
                    return Err("app-server 已退出".into());
                }
                if line.len() > MAX_RESPONSE_BYTES {
                    return Err("app-server 响应过大".into());
                }
                let message: Value =
                    serde_json::from_slice(line.trim_ascii()).map_err(|_| "app-server 响应无效")?;
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error").cloned() {
                    return Ok(Reply::Remote(error));
                }
                return Ok(Reply::Result(
                    message.get("result").cloned().unwrap_or(Value::Null),
                ));
            }
        })
        .await
        .map_err(|_| "app-server 响应超时")?
    }
}

fn codex_cli() -> Option<PathBuf> {
    let app_dir = codey_runtime_core::app_paths::resolve_codex_app_dir(None)?;
    codey_runtime_core::app_paths::codex_runtime_executable(&app_dir)
}

fn route_key(route: &Route) -> String {
    format!(
        "{} {} {} {} {}",
        route.base_url,
        route.token,
        route.requires_openai_auth,
        route.supports_websockets,
        route.supports_remote_compaction
    )
}

fn router_overrides(route: &Route) -> Vec<String> {
    let provider = crate::local_router::ROUTER_PROVIDER_ID;
    let name = if route.supports_remote_compaction {
        "OpenAI"
    } else {
        "Codey Local Router"
    };
    let mut headers = InlineTable::new();
    headers.insert(
        crate::local_router::ROUTER_AUTH_HEADER,
        TomlValue::from(route.token.as_str()),
    );
    let mut overrides = vec![
        override_value("model_provider", TomlValue::from(provider)),
        override_value(
            &format!("model_providers.{provider}.name"),
            TomlValue::from(name),
        ),
        override_value(
            &format!("model_providers.{provider}.base_url"),
            TomlValue::from(route.base_url.trim_end_matches('/')),
        ),
        override_value(
            &format!("model_providers.{provider}.wire_api"),
            TomlValue::from("responses"),
        ),
        override_value(
            &format!("model_providers.{provider}.requires_openai_auth"),
            TomlValue::from(route.requires_openai_auth),
        ),
        override_value(
            &format!("model_providers.{provider}.supports_websockets"),
            TomlValue::from(route.supports_websockets),
        ),
        override_value(
            &format!("model_providers.{provider}.http_headers"),
            TomlValue::InlineTable(headers),
        ),
        override_value("analytics.enabled", TomlValue::from(false)),
    ];
    if !route.requires_openai_auth {
        overrides.push(override_value(
            &format!("model_providers.{provider}.experimental_bearer_token"),
            TomlValue::from(route.token.as_str()),
        ));
    }
    overrides
}

fn override_value(key: &str, value: TomlValue) -> String {
    format!("{key}={value}")
}

fn redact(value: Value, token: &str) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(&text, token)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|item| redact(item, token)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (redact_text(&key, token), redact(item, token)))
                .collect(),
        ),
        other => other,
    }
}

fn redact_text(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_string()
    } else {
        text.replace(token, "[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Route {
        Route {
            base_url: "http://127.0.0.1:43127/v1".into(),
            token: "router-secret".into(),
            requires_openai_auth: false,
            supports_websockets: true,
            supports_remote_compaction: false,
        }
    }

    #[test]
    fn call_accepts_only_listed_schema_calls() {
        let call = codey_plugin_sdk::appserver::parse(
            br#"{"schema":"codey.appserver.v1","call":"codey://getTasks"}"#,
        )
        .unwrap();
        assert_eq!(call, codey_plugin_sdk::appserver::Call::Tasks);
        assert!(codey_plugin_sdk::appserver::parse(br#"codey://getTasks"#).is_err());
        assert!(
            codey_plugin_sdk::appserver::parse(
                br#"{"schema":"codey.appserver.v1","call":"codey://appServer/plugin/share/save","params":{"id":"demo"}}"#,
            )
            .is_err()
        );
        assert!(
            codey_plugin_sdk::appserver::parse(
                br#"{"schema":"codey.appserver.v1","call":"codey://appServer/thread/start","params":[]}"#,
            )
            .is_err()
        );
        assert!(codey_plugin_sdk::appserver::parse(br#"{"method":"thread/list"}"#).is_err());
    }

    #[tokio::test]
    async fn unlisted_method_does_not_start_the_shared_server() {
        let (status, body) = handle(
            &route(),
            br#"{"schema":"codey.appserver.v1","call":"codey://appServer/thread/list","params":{}}"#,
        )
        .await;
        assert_eq!(status, 400);
        assert!(body["error"]["message"].is_string());
        assert!(SHARED.lock().await.is_none());
    }

    #[test]
    fn shared_server_uses_the_local_router_without_exposing_the_token() {
        let rendered = router_overrides(&route()).join("\n");
        assert!(rendered.contains("model_provider=\"codey_router\""));
        assert!(
            rendered
                .contains("model_providers.codey_router.base_url=\"http://127.0.0.1:43127/v1\"")
        );
        assert!(rendered.contains("experimental_bearer_token=\"router-secret\""));
        let response = redact(
            json!({"result":{"note":"router-secret","nested":["router-secret"]}}),
            "router-secret",
        );
        let encoded = response.to_string();
        assert!(!encoded.contains("router-secret"));
        assert!(encoded.contains("[redacted]"));
    }
}
