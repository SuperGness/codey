//! 启动预取：与 Codex 启动并行枚举 `/json`，主页面一出现就交给注入。
//!
//! 资产补丁由浏览器级补丁服务负责，页面不再需要在首个导航前暂停；这里只省掉
//! 注入重试循环里第一次枚举的等待。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use codey_runtime_core::cdp::{CdpTarget, list_targets, pick_injectable_codex_page_target};
use serde_json::json;

/// /json 目标枚举的探测间隔。
const TARGET_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrefetchDiagnostics {
    outcome: &'static str,
    total_ms: u64,
    wait_ms: u64,
    pub(crate) last_discovery_error: Option<String>,
}

type Diagnostics = Arc<Mutex<PrefetchDiagnostics>>;

/// 预取任务随调用方一起取消，避免丢弃 JoinHandle 后继续枚举。
pub struct PrefetchTask {
    handle: tokio::task::JoinHandle<Option<CdpTarget>>,
    started: tokio::time::Instant,
    diagnostics: Diagnostics,
}

impl PrefetchTask {
    pub fn abort(&self) {
        self.handle.abort();
    }

    pub(crate) async fn wait(
        mut self,
        deadline: tokio::time::Instant,
    ) -> (Option<CdpTarget>, PrefetchDiagnostics) {
        let wait_started = tokio::time::Instant::now();
        let result = match tokio::time::timeout_at(deadline, &mut self.handle).await {
            Ok(result) => result,
            Err(_) => {
                self.diagnostics.lock().unwrap().outcome = "handoffTimeout";
                self.handle.abort();
                // abort 与任务完成可能同时发生；已找到的目标仍交给注入侧。
                (&mut self.handle).await
            }
        };
        let target = match result {
            Ok(target) => target,
            Err(error) => {
                let mut diagnostics = self.diagnostics.lock().unwrap();
                if diagnostics.outcome != "handoffTimeout" {
                    diagnostics.outcome = if error.is_panic() {
                        "panicked"
                    } else {
                        "cancelled"
                    };
                }
                None
            }
        };
        let mut diagnostics = self.diagnostics.lock().unwrap().clone();
        diagnostics.total_ms = self.started.elapsed().as_millis() as u64;
        diagnostics.wait_ms = wait_started.elapsed().as_millis() as u64;
        if target.is_some() {
            diagnostics.outcome = "ready";
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "target_prefetch.summary",
            json!(diagnostics),
        );
        (target, diagnostics)
    }
}

impl Drop for PrefetchTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub fn spawn_target_prefetch(debug_port: u16, budget: Duration) -> PrefetchTask {
    let started = tokio::time::Instant::now();
    let diagnostics = Diagnostics::default();
    let task_diagnostics = diagnostics.clone();
    let handle = tokio::spawn(async move {
        let deadline = started + budget;
        while tokio::time::Instant::now() < deadline {
            if let Some(target) = poll_targets_once(debug_port, &task_diagnostics).await {
                return Some(target);
            }
            tokio::time::sleep(TARGET_POLL_INTERVAL).await;
        }
        task_diagnostics.lock().unwrap().outcome = "deadlineExceeded";
        None
    });
    PrefetchTask {
        handle,
        started,
        diagnostics,
    }
}

async fn poll_targets_once(debug_port: u16, diagnostics: &Diagnostics) -> Option<CdpTarget> {
    match list_targets(debug_port).await {
        Ok(targets) => match pick_injectable_codex_page_target(&targets) {
            Ok(target) => Some(target),
            Err(_) => {
                diagnostics.lock().unwrap().last_discovery_error =
                    Some(format!("noMatchingPage: targetCount={}", targets.len()));
                None
            }
        },
        Err(error) => {
            diagnostics.lock().unwrap().last_discovery_error =
                Some(safe_prefetch_error("listTargets", &error));
            None
        }
    }
}

/// 仅保留传输分类和状态码，端点响应正文、页面地址及凭据不进入预取诊断。
fn safe_prefetch_error(stage: &str, error: &anyhow::Error) -> String {
    let message = format!("{error:#}").to_ascii_lowercase();
    let mut reasons = Vec::new();
    for (needle, reason) in [
        ("timed out", "timeout"),
        ("timeout", "timeout"),
        ("connection refused", "connectionRefused"),
        ("10061", "connectionRefused"),
        ("10060", "timeout"),
        ("connection reset", "connectionReset"),
        ("dns", "nameResolution"),
        ("decode", "invalidResponse"),
        ("decoding", "invalidResponse"),
        ("websocketdebuggerurl", "missingWebSocketUrl"),
    ] {
        if message.contains(needle) && !reasons.contains(&reason.to_string()) {
            reasons.push(reason.to_string());
        }
    }
    for status in 100..=599 {
        if message.contains(&format!("({status} ")) {
            reasons.push(format!("httpStatus={status}"));
        }
    }
    if reasons.is_empty() {
        reasons.push("endpointRequestFailed".to_string());
    }
    format!("{stage}: {}", reasons.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn task_from_handle(handle: tokio::task::JoinHandle<Option<CdpTarget>>) -> PrefetchTask {
        PrefetchTask {
            handle,
            started: tokio::time::Instant::now(),
            diagnostics: Diagnostics::default(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pending_prefetch_stops_at_handoff_deadline() {
        let (stopped, stopped_rx) = tokio::sync::oneshot::channel();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let task = task_from_handle(tokio::spawn(async move {
            let _guard = NotifyOnDrop(Some(stopped));
            let _ = ready.send(());
            std::future::pending().await
        }));
        ready_rx.await.unwrap();
        let started = tokio::time::Instant::now();
        let (target, diagnostics) = task.wait(started + Duration::from_secs(8)).await;
        assert!(target.is_none());
        assert_eq!(started.elapsed(), Duration::from_secs(8));
        assert_eq!(diagnostics.outcome, "handoffTimeout");
        stopped_rx.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_prefetch_cancels_the_running_task() {
        let (stopped, stopped_rx) = tokio::sync::oneshot::channel();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let task = task_from_handle(tokio::spawn(async move {
            let _guard = NotifyOnDrop(Some(stopped));
            let _ = ready.send(());
            std::future::pending().await
        }));
        ready_rx.await.unwrap();
        drop(task);
        tokio::time::timeout(Duration::from_secs(1), stopped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn completed_target_is_preserved_at_an_expired_deadline() {
        let task = task_from_handle(tokio::spawn(async {
            Some(CdpTarget {
                id: "ready-target".to_string(),
                target_type: "page".to_string(),
                title: "Codex".to_string(),
                url: "app://-/index.html".to_string(),
                web_socket_debugger_url: Some("ws://localhost/devtools/page/ready".to_string()),
            })
        }));
        tokio::task::yield_now().await;
        assert!(task.handle.is_finished());
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        let (target, diagnostics) = task.wait(deadline).await;
        assert!(target.is_some_and(|target| target.id == "ready-target"));
        assert_eq!(diagnostics.outcome, "ready");
    }

    #[tokio::test]
    async fn http_errors_are_diagnostic_without_response_body_or_urls() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 12\r\nConnection: close\r\n\r\nsecret-token").await.unwrap();
            }
        });
        let diagnostics = Diagnostics::default();
        assert!(poll_targets_once(port, &diagnostics).await.is_none());
        server.abort();
        let detail = diagnostics
            .lock()
            .unwrap()
            .last_discovery_error
            .clone()
            .unwrap();
        assert!(detail.contains("httpStatus=503"), "{detail}");
        assert!(!detail.contains("secret-token"));
        assert!(!detail.contains("localhost"));
        assert!(!detail.contains(&port.to_string()));
    }

    #[test]
    fn prefetch_error_summary_never_includes_endpoint_payloads() {
        let error = anyhow::anyhow!(
            "HTTP status client error (403 Forbidden) for url (http://user:secret@localhost/json?token=private); body=personal data"
        );
        assert_eq!(
            safe_prefetch_error("listTargets", &error),
            "listTargets: httpStatus=403"
        );
    }
}
