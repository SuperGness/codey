//! 将插件的有界传输帧转换为现有路由器使用的 HTTP 响应。
use super::*;
use crate::codey_plugins::transport::{Instance, Session};
use base64::{Engine, engine::general_purpose::STANDARD};
use codey_plugin_sdk::transport::{self as wire, Frame, Operation};

#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub plugin_id: String,
    pub options: wire::TransportOptions,
    instance: Option<Instance>,
}
impl Target {
    pub(crate) fn new(plugin_id: String, options: wire::TransportOptions) -> Self {
        let instance = Instance::capture(&plugin_id);
        Self {
            plugin_id,
            options,
            instance,
        }
    }
    fn session(&self) -> std::result::Result<Session, String> {
        self.instance.as_ref().ok_or("plugin_disabled")?.open()
    }
}

pub(super) async fn account_headers(target: &Target) -> std::result::Result<HeaderMap, String> {
    let _session = target.session()?;
    let credentials =
        crate::codey_plugins::transport::account_credentials(&target.options.account_email).await?;
    // 刷新期间可能停用或重新启用；此请求只能继续使用原实例。
    let _ = target.session()?;
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credentials.access_token))
            .map_err(|_| "账号令牌格式无效")?,
    );
    headers.insert(
        CHATGPT_ACCOUNT_ID_HEADER,
        HeaderValue::from_str(&credentials.upstream_account_id).map_err(|_| "账号标识格式无效")?,
    );
    Ok(headers)
}

struct Request {
    session: Session,
    id: String,
    finished: bool,
}
impl Drop for Request {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.session
            .cleanup(wire::CANCEL, json!({"requestId": self.id}));
    }
}
impl Request {
    fn finish(&mut self) {
        self.finished = true;
    }
    async fn frame(&self) -> std::result::Result<Frame, String> {
        let value = self
            .session
            .call(wire::READ, json!({"requestId": self.id}))
            .await?;
        serde_json::from_value(value).map_err(|_| "plugin_invalid_frame".into())
    }
}

fn error_response(code: &str) -> reqwest::Response {
    let status = wire::error_http_status(code);
    tokio_tungstenite::tungstenite::http::Response::builder().status(status)
        .header("content-type", "application/json")
        .body(reqwest::Body::from(json!({"error": {"type": "plugin_transport_error", "code": code, "message": "插件请求未能完成"}}).to_string()))
        .expect("static response").into()
}

pub(super) async fn send(
    target: &Target,
    operation: Operation,
    headers: &HeaderMap,
    body: Bytes,
) -> reqwest::Response {
    match send_inner(target, operation, headers, body).await {
        Ok(response) => response,
        Err(code) => error_response(&code),
    }
}

async fn send_inner(
    target: &Target,
    operation: Operation,
    headers: &HeaderMap,
    body: Bytes,
) -> std::result::Result<reqwest::Response, String> {
    if body.len() > wire::MAX_BODY_BYTES {
        return Err("plugin_request_too_large".into());
    }
    if (operation == Operation::ImageGeneration && !target.options.image_generation)
        || (operation == Operation::ImageEdit && !target.options.image_edit)
    {
        return Err("plugin_operation_unsupported".into());
    }
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or("plugin_auth_missing")?;
    let account = headers
        .get(CHATGPT_ACCOUNT_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or("plugin_auth_missing")?;
    let request = Request {
        session: target.session()?,
        id: Uuid::new_v4().to_string(),
        finished: false,
    };
    request
        .session
        .call(
            wire::START,
            serde_json::to_value(wire::StartRequest {
                request_id: request.id.clone(),
                account_email: target.options.account_email.clone(),
                operation,
                body_bytes: body.len(),
                credentials: wire::Credentials {
                    access_token: token.into(),
                    upstream_account_id: account.into(),
                },
            })
            .map_err(|_| "plugin_invalid_request")?,
        )
        .await?;
    // 所有 ABI 消息保持远小于 1 MiB；空正文仍显式提交结束帧。
    let mut cursor = 0;
    loop {
        let end = (cursor + wire::CHUNK_BYTES).min(body.len());
        request.session.call(wire::WRITE, json!({"requestId": request.id, "data": STANDARD.encode(&body[cursor..end]), "finish": end == body.len()})).await?;
        cursor = end;
        if cursor == body.len() {
            break;
        }
    }
    drop(body);
    let (status, response_headers) = loop {
        match request.frame().await? {
            Frame::Pending => tokio::time::sleep(Duration::from_millis(10)).await,
            Frame::Headers { status, headers } if (200..=599).contains(&status) => {
                break (status, headers);
            }
            Frame::Error { code } => return Err(wire::public_error_code(&code).into()),
            _ => return Err("plugin_invalid_frame_order".into()),
        }
    };
    let mut response = tokio_tungstenite::tungstenite::http::Response::builder().status(status);
    for (name, value) in wire::filter_response_headers(response_headers)? {
        response = response.header(name, value);
    }
    let stream = futures_util::stream::try_unfold(
        (request, 0usize),
        |(mut request, mut received)| async move {
            loop {
                match request.frame().await.map_err(std::io::Error::other)? {
                    Frame::Pending => tokio::time::sleep(Duration::from_millis(10)).await,
                    Frame::Data { data } => {
                        if data.len() > wire::CHUNK_BYTES * 2 {
                            return Err(std::io::Error::other("plugin_chunk_too_large"));
                        }
                        let bytes = STANDARD
                            .decode(data)
                            .map_err(|_| std::io::Error::other("plugin_invalid_chunk"))?;
                        if bytes.len() > wire::CHUNK_BYTES || bytes.is_empty() {
                            return Err(std::io::Error::other("plugin_invalid_chunk"));
                        }
                        received = received
                            .checked_add(bytes.len())
                            .ok_or_else(|| std::io::Error::other("upstream_body_too_large"))?;
                        if received > wire::MAX_BODY_BYTES {
                            return Err(std::io::Error::other("upstream_body_too_large"));
                        }
                        return Ok(Some((Bytes::from(bytes), (request, received))));
                    }
                    Frame::End => {
                        request.finish();
                        return Ok(None);
                    }
                    Frame::Error { code } => {
                        return Err(std::io::Error::other(wire::public_error_code(&code)));
                    }
                    _ => return Err(std::io::Error::other("plugin_stream_failed")),
                }
            }
        },
    );
    response
        .body(reqwest::Body::wrap_stream(stream))
        .map(Into::into)
        .map_err(|_| "plugin_invalid_headers".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codey_plugins::{lifecycle::TestPlugin, transport::with_test_transport};
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    fn target() -> Target {
        Target::new(
            "test.transport".into(),
            wire::TransportOptions {
                account_email: "user@example.com".into(),
                models: BTreeMap::new(),
                image_generation: true,
                image_edit: true,
            },
        )
    }
    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer fake-token"));
        headers.insert(
            CHATGPT_ACCOUNT_ID_HEADER,
            HeaderValue::from_static("fake-account"),
        );
        headers
    }
    async fn wait_cancel(count: &AtomicUsize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while count.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn plugin_transport_chunks_large_bodies_and_filters_headers() {
        let input = vec![b'x'; 1_200_000];
        let output = vec![b'y'; 1_300_000];
        let received = Arc::new(Mutex::new(Vec::new()));
        let captured = received.clone();
        let mut frames = VecDeque::from([Frame::Headers {
            status: 200,
            headers: BTreeMap::from([
                ("content-type".into(), "application/json".into()),
                ("set-cookie".into(), "secret=value".into()),
            ]),
        }]);
        frames.extend(output.chunks(wire::CHUNK_BYTES).map(|chunk| Frame::Data {
            data: STANDARD.encode(chunk),
        }));
        frames.push_back(Frame::End);
        let frames = Mutex::new(frames);
        let cancelled = Arc::new(AtomicUsize::new(0));
        let count = cancelled.clone();
        let plugin = TestPlugin::new("test.transport", move |method, value| {
            match method {
                wire::START => {
                    assert_eq!(value["credentials"]["accessToken"], "fake-token");
                    assert_eq!(value["bodyBytes"], 1_200_000);
                }
                wire::WRITE => {
                    assert!(value.to_string().len() < codey_plugin_sdk::MAX_MESSAGE_BYTES);
                    let data = STANDARD.decode(value["data"].as_str().unwrap()).unwrap();
                    assert!(data.len() <= wire::CHUNK_BYTES);
                    captured.lock().unwrap().extend(data);
                    if value["finish"] == true {
                        assert_eq!(captured.lock().unwrap().len(), 1_200_000);
                    }
                }
                wire::READ => {
                    return Ok(
                        serde_json::to_value(frames.lock().unwrap().pop_front().unwrap()).unwrap(),
                    );
                }
                wire::CANCEL => {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                _ => panic!("unexpected method"),
            }
            Ok(json!({}))
        });
        with_test_transport("test.transport", plugin, async {
            let response = send(
                &target(),
                Operation::Responses,
                &headers(),
                Bytes::from(input.clone()),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert!(response.headers().get("set-cookie").is_none());
            assert_eq!(response.bytes().await.unwrap().as_ref(), output.as_slice());
            assert_eq!(*received.lock().unwrap(), input);
            assert_eq!(cancelled.load(Ordering::SeqCst), 0);
        })
        .await;
    }
    #[tokio::test]
    async fn plugin_transport_drop_cancels_and_disable_allows_only_cleanup() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let count = cancelled.clone();
        let stopped = Arc::new(AtomicUsize::new(0));
        let stop = stopped.clone();
        let plugin = TestPlugin::new("test.transport", move |method, _| {
            match method {
                wire::READ => return Ok(json!({"type":"headers","status":200,"headers":{}})),
                wire::CANCEL => {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                wire::STOP => {
                    stop.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(json!({}))
        });
        with_test_transport("test.transport", plugin, async {
            drop(send(&target(), Operation::Responses, &headers(), Bytes::new()).await);
            wait_cancel(&cancelled).await;
            let session = Session::open("test.transport").unwrap();
            session.disable_for_test();
            assert_eq!(
                session
                    .call(wire::READ, json!({"requestId":"old"}))
                    .await
                    .unwrap_err(),
                "plugin_disabled"
            );
            session.call(wire::STOP, json!({})).await.unwrap();
            assert_eq!(stopped.load(Ordering::SeqCst), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn transport_rejects_case_insensitive_duplicate_headers_and_cancels() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let count = cancelled.clone();
        let plugin = TestPlugin::new("test.transport", move |method, _| {
            if method == wire::READ {
                return Ok(json!({"type":"headers","status":200,"headers":{
                    "Content-Type":"text/event-stream", "content-type":"application/json"
                }}));
            }
            if method == wire::CANCEL {
                count.fetch_add(1, Ordering::SeqCst);
            }
            Ok(json!({}))
        });
        with_test_transport("test.transport", plugin, async {
            let response = send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
            assert_eq!(response.status(), 502);
            assert!(
                response
                    .text()
                    .await
                    .unwrap()
                    .contains("plugin_invalid_headers")
            );
            wait_cancel(&cancelled).await;
        })
        .await;
    }

    #[tokio::test]
    async fn pending_transport_obeys_router_body_deadlines_and_cancels() {
        for total_deadline in [false, true] {
            let reads = AtomicUsize::new(0);
            let cancelled = Arc::new(AtomicUsize::new(0));
            let count = cancelled.clone();
            let plugin = TestPlugin::new("test.transport", move |method, _| {
                if method == wire::READ {
                    return Ok(if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                        json!({"type":"headers","status":200,"headers":{"content-type":"text/event-stream"}})
                    } else {
                        json!({"type":"pending"})
                    });
                }
                if method == wire::CANCEL {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                Ok(json!({}))
            })
            // 虚拟时间跳跃时，后台回调可能尚未完成；让路由器的期限先到期。
            .with_invoke_timeout(UPSTREAM_READ_IDLE_TIMEOUT + Duration::from_secs(1));
            with_test_transport("test.transport", plugin, async {
                let response =
                    send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
                let mut prepared = prepare_upstream_response(response, "plugin test", None)
                    .await
                    .unwrap();
                tokio::time::pause();
                let duration = if total_deadline {
                    Duration::from_secs(1)
                } else {
                    UPSTREAM_READ_IDLE_TIMEOUT
                };
                if total_deadline {
                    prepared.deadline = tokio::time::Instant::now() + duration;
                }
                {
                    let read = read_prepared_upstream_chunk(&mut prepared, "plugin test", None);
                    tokio::pin!(read);
                    // 先轮询读取任务，确保空闲超时计时器已创建，再推进时间。
                    assert!(futures_util::poll!(&mut read).is_pending());
                    tokio::time::advance(duration + Duration::from_millis(1)).await;
                    let error = read.await.unwrap_err();
                    if total_deadline {
                        assert!(error.is::<UpstreamResponseDeadline>());
                    } else {
                        assert!(
                            error.is::<UpstreamReadIdleTimeout>(),
                            "unexpected error: {error:#}"
                        );
                    }
                }
                tokio::time::resume();
                drop(prepared);
                wait_cancel(&cancelled).await;
            })
            .await;
        }
    }
    #[tokio::test]
    async fn plugin_transport_rejects_bad_frames_and_redacts_plugin_errors() {
        for bad in [
            json!({"type":"data","data":"!invalid!"}),
            json!({"type":"data","data":STANDARD.encode(vec![0;wire::CHUNK_BYTES+1])}),
            json!({"type":"error","code":"private-token"}),
        ] {
            let frames = Mutex::new(VecDeque::from([
                json!({"type":"headers","status":200,"headers":{}}),
                bad,
            ]));
            let cancelled = Arc::new(AtomicUsize::new(0));
            let count = cancelled.clone();
            let plugin = TestPlugin::new("test.transport", move |method, _| {
                if method == wire::READ {
                    return Ok(frames.lock().unwrap().pop_front().unwrap());
                }
                if method == wire::CANCEL {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                Ok(json!({}))
            });
            with_test_transport("test.transport", plugin, async {
                let response =
                    send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
                let error = response.bytes().await.unwrap_err().to_string();
                assert!(!error.contains("private-token"));
                wait_cancel(&cancelled).await;
            })
            .await;
        }
        let plugin = TestPlugin::new("test.transport", |method, _| {
            if method == wire::READ {
                return Ok(json!({"type":"data","data":"YQ=="}));
            }
            Ok(json!({}))
        });
        with_test_transport("test.transport", plugin, async {
            let response = send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
            assert_eq!(response.status(), 502);
            assert!(
                response
                    .text()
                    .await
                    .unwrap()
                    .contains("plugin_invalid_frame_order")
            );
        })
        .await;
    }

    #[tokio::test]
    async fn transport_preserves_only_known_error_codes() {
        for (code, status, expected) in [
            ("upstream_timeout", 504, "upstream_timeout"),
            ("upstream_rate_limited", 429, "upstream_rate_limited"),
            ("private-token-from-plugin", 502, "plugin_upstream_failed"),
        ] {
            let plugin = TestPlugin::new("test.transport", move |method, _| {
                Ok(if method == wire::READ {
                    json!({"type":"error","code":code})
                } else {
                    json!({})
                })
            });
            with_test_transport("test.transport", plugin, async {
                let response =
                    send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
                assert_eq!(response.status(), status);
                let text = response.text().await.unwrap();
                assert!(text.contains(expected));
                assert!(!text.contains("private-token"));
            })
            .await;
        }
        let plugin = TestPlugin::new("test.transport", |method, _| {
            if method == wire::START {
                Err("request_limit".into())
            } else {
                Ok(json!({}))
            }
        });
        with_test_transport("test.transport", plugin, async {
            let response = send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
            assert_eq!(response.status(), 429);
            assert!(response.text().await.unwrap().contains("request_limit"));
        })
        .await;
    }

    #[tokio::test]
    async fn stale_route_never_dispatches_to_a_reenabled_instance() {
        let old = TestPlugin::new("test.transport", |_, _| Ok(json!({})));
        with_test_transport("test.transport", old, async {
            let route = target();
            route.session().unwrap().disable_for_test();
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let new = TestPlugin::new("test.transport", move |_, _| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(json!({}))
            });
            with_test_transport("test.transport", new, async {
                assert!(target().session().is_ok());
                let response = send(&route, Operation::Responses, &headers(), Bytes::new()).await;
                assert_eq!(response.status(), 503);
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            })
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn transport_bounds_total_response_and_cancels_overflow() {
        let reads = AtomicUsize::new(0);
        let cancelled = Arc::new(AtomicUsize::new(0));
        let count = cancelled.clone();
        let data = STANDARD.encode(vec![b'x'; wire::CHUNK_BYTES]);
        let plugin = TestPlugin::new("test.transport", move |method, _| {
            if method == wire::READ {
                return Ok(if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    json!({"type":"headers","status":200,"headers":{}})
                } else {
                    json!({"type":"data","data":data})
                });
            }
            if method == wire::CANCEL {
                count.fetch_add(1, Ordering::SeqCst);
            }
            Ok(json!({}))
        });
        with_test_transport("test.transport", plugin, async {
            let mut response =
                send(&target(), Operation::Responses, &headers(), Bytes::new()).await;
            let mut received = 0;
            loop {
                match response.chunk().await {
                    Ok(Some(bytes)) => received += bytes.len(),
                    Err(_) => break,
                    Ok(None) => panic!("oversized response was reported as successful"),
                }
            }
            assert_eq!(received, wire::MAX_BODY_BYTES);
            wait_cancel(&cancelled).await;
        })
        .await;
    }
}
