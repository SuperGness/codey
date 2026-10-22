use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use tokio_tungstenite::tungstenite::Message;

const APP_QUIT_TIMEOUT: Duration = Duration::from_secs(1);
const APP_QUIT_SCRIPT: &str = r#"(async () => {
  const bridge = window.electronBridge;
  if (typeof bridge?.sendMessageFromView !== "function") return false;
  await bridge.sendMessageFromView({ type: "quit-app" });
  return true;
})()"#;

pub(crate) async fn request_app_quit(renderer_websocket_url: &str) -> Result<()> {
    // Keep WebSocket state from inflating every launch/cleanup caller's future.
    Box::pin(tokio::time::timeout(APP_QUIT_TIMEOUT, async {
        let renderer = loopback_renderer_websocket_url(renderer_websocket_url)?;
        let (mut socket, _) = tokio_tungstenite::connect_async(renderer.as_str())
            .await
            .context("连接 Codex 页面调试端口失败")?;
        // Browser.close only closes windows: Codex stays resident on Windows.
        // Its native quit message calls app.quit(), including tray disposal and
        // the application's normal shutdown hooks. Codey owns the next launch.
        socket
            .send(Message::Text(
                serde_json::json!({
                    "id": 1,
                    "method": "Runtime.evaluate",
                    "params": {
                        "expression": APP_QUIT_SCRIPT,
                        "awaitPromise": true,
                        "returnByValue": true,
                        "allowUnsafeEvalBlockedByCSP": true,
                    },
                })
                .to_string()
                .into(),
            ))
            .await
            .context("发送 Codex 正常退出请求失败")?;
        // Quitting may disconnect CDP before the IPC/evaluate reply arrives.
        // A disconnect after dispatch only earns the graceful-exit wait; the
        // caller must still verify the captured process identities have exited.
        while let Some(Ok(message)) = socket.next().await {
            let Message::Text(text) = message else {
                if message.is_close() {
                    break;
                }
                continue;
            };
            let response: serde_json::Value =
                serde_json::from_str(&text).context("解析 Codex 退出请求响应失败")?;
            if response["id"] != 1 {
                continue;
            }
            anyhow::ensure!(
                response.get("error").is_none()
                    && response["result"].get("exceptionDetails").is_none()
                    && response["result"]["result"]["value"] == true,
                "Codex 页面未接受正常退出请求",
            );
            break;
        }
        Ok(())
    }))
    .await
    .context("请求 Codex 正常退出超时")?
}

fn loopback_renderer_websocket_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value).context("Codex 调试 WebSocket 地址无效")?;
    anyhow::ensure!(
        url.scheme() == "ws"
            && url.port().is_some_and(|port| port != 0)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .path()
                .strip_prefix("/devtools/page/")
                .is_some_and(|id| !id.is_empty() && !id.contains('/')),
        "Codex 调试地址必须是本机页面 WebSocket 端点",
    );
    let host = url.host_str().context("Codex 调试地址缺少主机")?;
    if host == "localhost" {
        url.set_host(Some("127.0.0.1"))?;
    } else {
        anyhow::ensure!(
            host.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback()),
            "Codex 调试地址必须使用本机回环地址",
        );
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn app_quit_future_keeps_callers_small() {
        let request = request_app_quit("ws://127.0.0.1:9222/devtools/page/test");
        let size = std::mem::size_of_val(&request);
        assert!(size <= 1024, "app quit future is {size} bytes");
    }

    #[tokio::test]
    async fn app_quit_uses_native_ipc_and_accepts_disconnect_during_exit() {
        for reply_before_exit in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let message = socket.next().await.unwrap().unwrap();
                let command: serde_json::Value =
                    serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_eq!(command["method"], "Runtime.evaluate");
                assert_eq!(command["params"]["expression"], APP_QUIT_SCRIPT);
                assert_eq!(command["params"]["awaitPromise"], true);
                assert_eq!(command["params"]["returnByValue"], true);
                if reply_before_exit {
                    for response in [
                        serde_json::json!({"method": "Runtime.executionContextDestroyed"}),
                        serde_json::json!({"id": 2, "result": {}}),
                        serde_json::json!({
                            "id": 1, "result": {"result": {"type": "boolean", "value": true}},
                        }),
                    ] {
                        socket
                            .send(Message::Text(response.to_string().into()))
                            .await
                            .unwrap();
                    }
                }
                // Dropping without a reply models the main process exiting
                // before its renderer can acknowledge the native quit request.
            });
            request_app_quit(&format!(
                "ws://localhost:{}/devtools/page/test",
                address.port()
            ))
            .await
            .unwrap();
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn app_quit_rejects_missing_bridge_and_protocol_or_evaluation_errors() {
        for response in [
            serde_json::json!({
                "id": 1, "result": {"result": {"type": "boolean", "value": false}},
            }),
            serde_json::json!({"id": 1, "error": {"message": "Method not found"}}),
            serde_json::json!({
                "id": 1, "result": {"exceptionDetails": {"text": "Uncaught"}},
            }),
            serde_json::json!({"id": 1, "result": {"result": {"type": "undefined"}}}),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                socket.next().await.unwrap().unwrap();
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
            });
            let error = request_app_quit(&format!("ws://{address}/devtools/page/test"))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("未接受正常退出请求"));
            server.await.unwrap();
        }
    }

    #[test]
    fn app_quit_rejects_nonlocal_ambiguous_or_nonrenderer_urls() {
        for url in [
            "invalid",
            "ws://example.com:9222/devtools/page/test",
            "wss://127.0.0.1:9222/devtools/page/test",
            "ws://127.0.0.1/devtools/page/test",
            "ws://127.0.0.1:0/devtools/page/test",
            "ws://user:password@127.0.0.1:9222/devtools/page/test",
            "ws://127.0.0.1:9222/devtools/page/test?token=private",
            "ws://127.0.0.1:9222/devtools/page/test#private",
            "ws://127.0.0.1:9222/devtools/browser/test",
            "ws://127.0.0.1:9222/devtools/page/",
            "ws://127.0.0.1:9222/devtools/page/test/extra",
        ] {
            assert!(loopback_renderer_websocket_url(url).is_err(), "{url}");
        }
        assert!(loopback_renderer_websocket_url("ws://[::1]:9222/devtools/page/test").is_ok());
    }

    #[tokio::test]
    async fn app_quit_has_a_deadline_when_the_renderer_does_not_answer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            std::future::pending::<()>().await;
        });
        let error = request_app_quit(&format!("ws://{address}/devtools/page/test"))
            .await
            .unwrap_err();
        assert!(
            error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some()
        );
        server.abort();
    }
}
