use super::responses::format_upstream_headers;
use super::*;
use crate::codey_plugins::lifecycle::{
    LifecycleDecision, LifecycleError, LifecycleOutcome, LifecycleRequest, LifecycleResponse,
    LifecycleStage, has_plugins,
};
use http_body::{Body as HttpBody, Frame};
use std::pin::Pin;
use std::task::{Context, Poll};

tokio::task_local! {
    pub(super) static CURRENT_LIFECYCLE_FAILURE: std::cell::RefCell<Option<(u16, String)>>;
}

pub(super) fn observe_lifecycle_response_failure(event: &Value) {
    if !responses_event_is_failure(event) {
        return;
    }
    let error = event
        .pointer("/response/error")
        .or_else(|| event.get("error"))
        .unwrap_or(event);
    let status = error
        .pointer("/codey/httpStatus")
        .or_else(|| error.get("status"))
        .or_else(|| event.get("status"))
        .and_then(Value::as_u64)
        .filter(|status| (400..600).contains(status))
        .unwrap_or(502) as u16;
    let code = first_string_at(
        error,
        &[
            "/codey/errorCode",
            "/codey/upstreamCode",
            "/codey/originalCode",
            "/code",
        ],
    )
    .unwrap_or("upstream_response_failed");
    let _ = CURRENT_LIFECYCLE_FAILURE.try_with(|failure| {
        failure
            .borrow_mut()
            .get_or_insert_with(|| (status, code.to_owned()));
    });
}

/// hyper 1.11 HTTP/1 的默认写缓冲上限（`DEFAULT_MAX_BUFFER_SIZE`）。
/// 缓冲还放得下时，连接会先把正文取完并丢掉，再去刷套接字；响应头期限因此
/// 会把仍堵在用户态缓冲里的历史上传算进去。最后一块必须至少这么大，连接
/// 才会先写完它，再结束正文。
const HYPER_H1_WRITE_BUFFER_BYTES: usize = 8192 + 4096 * 100;

pub(super) fn apply_codey_plugin_header_patches(
    headers: &mut HeaderMap,
    patches: Vec<crate::codey_plugins::HeaderPatch>,
) -> Result<(), LifecycleError> {
    let parsed = patches
        .into_iter()
        .map(|patch| {
            if !crate::codey_plugins::allowed_header_name(&patch.name) {
                return None;
            }
            let name = HeaderName::from_bytes(patch.name.as_bytes()).ok()?;
            let value = match patch.value {
                Some(value) => Some(HeaderValue::from_str(&value).ok()?),
                None => None,
            };
            Some((name, value))
        })
        .collect::<Option<Vec<_>>>();
    // 任一补丁无法成为合法 HTTP 头时保持原请求不变。静默跳过会把其他插件
    // 已经校验过的修改一起丢掉，调用方应终止这次生命周期。
    let Some(parsed) = parsed else {
        return Err(LifecycleError {
            status: 502,
            code: "plugin_invalid_headers".into(),
            message: "插件请求生命周期处理失败".into(),
        });
    };
    for (name, value) in parsed {
        if let Some(value) = value {
            headers.insert(name, value);
        } else {
            headers.remove(name);
        }
    }
    Ok(())
}

pub(super) fn lifecycle_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), value.to_owned()))
        })
        .collect()
}

pub(super) fn request_lifecycle(
    headers: &HeaderMap,
    resolved: &RouteSelection,
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
    stream: bool,
    subagent: bool,
) -> LifecycleRequest {
    if !has_plugins() {
        return LifecycleRequest::inert();
    }
    let upstream_account_id = headers
        .get(CHATGPT_ACCOUNT_ID_HEADER)
        .and_then(|v| v.to_str().ok());
    let access_token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim());
    // 已解析令牌中的套餐声明只作为可选元数据，不推测缺失的账号身份。
    let account_type = resolved
        .route
        .official_account
        .then(|| {
            access_token
                .and_then(crate::official_accounts::jwt_claims)
                .and_then(|claims| {
                    claims
                        .get("https://api.openai.com/auth")?
                        .get("chatgpt_plan_type")?
                        .as_str()
                        .map(str::to_owned)
                })
        })
        .flatten();
    let metadata = json!({
        "requestId": current_router_request_id().unwrap_or_else(|| Uuid::new_v4().to_string()),
        "routeId": resolved.provider_id,
        "officialAccountId": resolved.route.official_auth.as_ref().map(|auth| auth.account_id.as_str()),
        "officialAccountEmail": official_account_email(&resolved.route),
        "upstreamAccountId": upstream_account_id,
        "accountType": account_type,
        "requestedModel": resolved.requested_model,
        "model": resolved.upstream_model,
        "protocol": bridge.upstream_protocol().label(),
        "stream": stream,
        "requestKind": request_kind.label(),
        "subagent": subagent,
    });
    let credentials = resolved
        .route
        .official_account
        .then(|| {
            access_token.map(|token| {
                json!({
                    "accessToken": token,
                    "upstreamAccountId": upstream_account_id,
                })
            })
        })
        .flatten();
    let mut lifecycle = LifecycleRequest::new(metadata, credentials);
    lifecycle.set_transport_context(
        resolved.route.official_account,
        bridge == ProtocolBridge::NativeResponses && resolved.route.plugin_transport.is_none(),
    );
    lifecycle
}

fn official_account_email(route: &RouteTarget) -> Option<&str> {
    if !route.official_account {
        return None;
    }
    route.official_auth.as_ref()?.email.as_deref()
}

struct SignaledRequestBody {
    bytes: Bytes,
    cursor: usize,
    /// 结束帧交出之后才为真。最后一块数据交出时不能提前变成结束，否则连接
    /// 不会先把写缓冲刷进套接字，响应头期限会把还在缓冲里的历史上传算进去。
    ended: bool,
    uploaded: Option<tokio::sync::oneshot::Sender<()>>,
}

impl SignaledRequestBody {
    /// 下一块的结束位置。剩余部分比写缓冲更小时并入当前块，避免最后一块
    /// 还能留在缓冲里，连接却已经认为正文结束。
    fn next_chunk_end(&self) -> usize {
        let remaining = self.bytes.len() - self.cursor;
        if remaining <= HYPER_H1_WRITE_BUFFER_BYTES {
            return self.bytes.len();
        }
        let end = self.cursor + HYPER_H1_WRITE_BUFFER_BYTES;
        let tail = self.bytes.len() - end;
        if tail < HYPER_H1_WRITE_BUFFER_BYTES {
            self.bytes.len()
        } else {
            end
        }
    }
}

impl HttpBody for SignaledRequestBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let body = self.get_mut();
        if body.cursor >= body.bytes.len() {
            // 大请求的最后一块已经占满写缓冲。is_end_stream 在此之前仍为假，
            // 连接会先把这块刷进套接字，再来取结束帧。这里才通知上传结束。
            body.ended = true;
            if let Some(uploaded) = body.uploaded.take() {
                let _ = uploaded.send(());
            }
            return Poll::Ready(None);
        }
        let end = body.next_chunk_end();
        let chunk = body.bytes.slice(body.cursor..end);
        body.cursor = end;
        Poll::Ready(Some(Ok(Frame::data(chunk))))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.bytes.len().saturating_sub(self.cursor) as u64)
    }

    fn is_end_stream(&self) -> bool {
        self.ended
    }
}

/// 响应头等待。`after_upload` 只覆盖网关收齐正文之后的首字或生成时间；
/// 切模型重放的历史可能还在 HTTP/2 或内核发送缓冲里，按正文大小补上这段传输。
pub(crate) fn response_header_timeout(body_len: usize, after_upload: Duration) -> Duration {
    let transit_secs = (body_len as u64) / BUFFERED_UPLOAD_BYTES_PER_SEC;
    after_upload
        .saturating_add(Duration::from_secs(transit_secs))
        .min(UPSTREAM_RESPONSE_TIMEOUT)
}

fn signaled_request_body(bytes: Bytes) -> (reqwest::Body, tokio::sync::oneshot::Receiver<()>) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    if bytes.is_empty() {
        let _ = sender.send(());
        return (reqwest::Body::from(bytes), receiver);
    }
    (
        reqwest::Body::wrap(SignaledRequestBody {
            bytes,
            cursor: 0,
            ended: false,
            uploaded: Some(sender),
        }),
        receiver,
    )
}

/// 等待上游响应头。大请求体（切模型后的整段历史）先上传，上传完成后再应用
/// 响应头期限；上传本身只受响应总期限约束，避免还在传正文时被记成 504。
/// 协议栈取走正文不等于网关已经收齐，期限里包含这段缓冲传输。
pub(super) async fn send_for_response_headers(
    builder: reqwest::RequestBuilder,
    body: Bytes,
    header_timeout: Duration,
    mut on_uploaded: impl FnMut() + Send,
) -> std::result::Result<
    std::result::Result<reqwest::Response, reqwest::Error>,
    tokio::time::error::Elapsed,
> {
    let header_timeout = response_header_timeout(body.len(), header_timeout);
    let (body, mut uploaded) = signaled_request_body(body);
    let send = builder.body(body).send();
    tokio::pin!(send);
    let upload_limit = tokio::time::sleep(UPSTREAM_RESPONSE_TIMEOUT);
    tokio::pin!(upload_limit);
    let uploaded_first = tokio::select! {
        biased;
        result = &mut send => {
            on_uploaded();
            return Ok(result);
        }
        _ = &mut uploaded => true,
        _ = &mut upload_limit => false,
    };
    // 正文已经交给连接，或这次发送不会再继续占用它。先交回准入名额，
    // 再等待响应头，避免高思考的首字等待挡住后续请求。
    on_uploaded();
    if !uploaded_first {
        return Err(
            tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
                .await
                .expect_err("zero timeout"),
        );
    }
    tokio::time::timeout(header_timeout, send).await
}

/// 生命周期只允许在尚未向下游写出响应时重发。与 reasoning 回退共享两次发送预算。
#[allow(clippy::too_many_arguments)]
pub(super) async fn send_lifecycle_http<D, F>(
    downstream: &mut D,
    lifecycle: &mut LifecycleRequest,
    client: &reqwest::Client,
    url: &str,
    headers: &mut HeaderMap,
    body: Bytes,
    on_uploaded: &mut F,
    attempt: &mut u32,
    timeout: Duration,
    plugin: Option<&plugin_transport::Target>,
) -> Result<
    std::result::Result<
        std::result::Result<reqwest::Response, reqwest::Error>,
        tokio::time::error::Elapsed,
    >,
>
where
    D: ResponsesDownstream + ?Sized,
    F: FnMut() + Send,
{
    let url = reqwest::Url::parse(url)?;
    lifecycle.set_upstream_target(&url)?;
    let mut retained_body = Some(body);
    loop {
        let decision = await_upstream(
            downstream,
            lifecycle.dispatch(
                LifecycleStage::BeforeSend,
                *attempt,
                lifecycle_headers(headers),
                None,
            ),
        )
        .await??;
        match decision {
            LifecycleDecision::Continue(patches) => {
                apply_codey_plugin_header_patches(headers, patches)?
            }
            LifecycleDecision::Retry(_) => {
                return Err(LifecycleError {
                    status: 409,
                    code: "plugin_retry_invalid_stage".into(),
                    message: "请求发送前不能要求重发".into(),
                }
                .into());
            }
        }
        lifecycle.apply_api_key(headers)?;
        await_upstream(downstream, lifecycle.mint_turn_state(client)).await??;
        lifecycle.apply_turn_state(headers)?;
        if let Some(probe) = downstream.request_log_probe() {
            probe.set_upstream_request_headers(&format_upstream_headers(headers));
        }
        let payload = retained_body
            .as_ref()
            .expect("plugin retry keeps the uploaded body")
            .clone();
        // 以这条请求启动时的插件快照为准。上传过程中全局插件被关掉时，
        // 快照仍可能在响应头之后要求重发，正文必须还在。
        let keep_uploaded_body = lifecycle.is_active();
        let result = await_upstream(
            downstream,
            send_route_response_headers(
                plugin,
                codey_plugin_sdk::transport::Operation::Responses,
                client,
                url.as_str(),
                headers,
                payload,
                timeout,
                &mut || {
                    if !keep_uploaded_body {
                        retained_body.take();
                    }
                    on_uploaded();
                },
            ),
        )
        .await?;
        let Ok(Ok(response)) = &result else {
            return Ok(result);
        };
        let decision = await_upstream(
            downstream,
            lifecycle.dispatch(
                LifecycleStage::AfterHeaders,
                *attempt,
                lifecycle_headers(headers),
                Some(LifecycleResponse {
                    status: response.status().as_u16(),
                    headers: lifecycle_headers(response.headers()),
                }),
            ),
        )
        .await??;
        match decision {
            LifecycleDecision::Continue(_) => return Ok(result),
            LifecycleDecision::Retry(patches) => {
                if *attempt >= 1 || downstream.event_stream_started() {
                    return Err(LifecycleError {
                        status: 409,
                        code: "plugin_retry_limit".into(),
                        message: "请求已达到重发上限或响应已经开始".into(),
                    }
                    .into());
                }
                drop(result);
                apply_codey_plugin_header_patches(headers, patches)?;
                *attempt += 1;
                if let Some(probe) = downstream.request_log_probe() {
                    probe.mark_fallback("plugin_request_retry");
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_route_response_headers(
    plugin: Option<&plugin_transport::Target>,
    operation: codey_plugin_sdk::transport::Operation,
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    body: Bytes,
    limit: Duration,
    mut on_uploaded: impl FnMut() + Send,
) -> std::result::Result<
    std::result::Result<reqwest::Response, reqwest::Error>,
    tokio::time::error::Elapsed,
> {
    if let Some(target) = plugin {
        let result = tokio::time::timeout(
            response_header_timeout(body.len(), limit),
            plugin_transport::send(target, operation, headers, body),
        )
        .await;
        on_uploaded();
        result.map(Ok)
    } else {
        send_for_response_headers(
            client.post(url).headers(headers.clone()),
            body,
            limit,
            on_uploaded,
        )
        .await
    }
}

/// 汇总写回的错误与传输结果，避免 HTTP 错误成功写回时被误报为请求完成。
pub(super) struct LifecycleDownstream<'a, D: ResponsesDownstream + ?Sized> {
    pub inner: &'a mut D,
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl<D: ResponsesDownstream + ?Sized> LifecycleDownstream<'_, D> {
    pub fn finish(&self, lifecycle: &mut LifecycleRequest, result: &Result<()>) {
        let cancelled = result.as_ref().err().is_some_and(|error| {
            error.is::<DownstreamClosed>()
                || error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::NotConnected
                    )
                })
        });
        let outcome = if cancelled {
            LifecycleOutcome::Cancelled
        } else if result.is_err() || self.error.is_some() || self.status.is_some_and(|s| s >= 400) {
            LifecycleOutcome::Failed
        } else {
            LifecycleOutcome::Completed
        };
        let code = self.error.as_deref().or_else(|| {
            result.as_ref().err().map(|_| {
                if cancelled {
                    "downstream_closed"
                } else {
                    "upstream_response_failed"
                }
            })
        });
        lifecycle.finish(outcome, self.status, code);
    }
}

#[async_trait]
impl<D: ResponsesDownstream + ?Sized> ResponsesDownstream for LifecycleDownstream<'_, D> {
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        self.inner.wait_for_upstream(future).await
    }
    fn is_websocket(&self) -> bool {
        self.inner.is_websocket()
    }
    fn event_stream_started(&self) -> bool {
        self.inner.event_stream_started()
    }
    fn set_streaming_errors(&mut self, enabled: bool) {
        self.inner.set_streaming_errors(enabled);
    }
    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        self.inner.request_log_probe()
    }
    fn select_route(&mut self, route: &RouteTarget) {
        self.inner.select_route(route);
    }
    fn prepare_native_http_fallback(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
    ) -> Result<bool> {
        self.inner
            .prepare_native_http_fallback(route, headers, body)
    }
    fn prepare_adapted_response_context(&mut self, body: &mut Value) -> Result<bool> {
        self.inner.prepare_adapted_response_context(body)
    }
    fn remember_adapted_response(&mut self, id: &str, output: &[Value]) -> Result<()> {
        self.inner.remember_adapted_response(id, output)
    }
    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        self.status = Some(status);
        self.error = Some(code.into());
        self.inner.write_error(status, code, message, route).await
    }
    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        self.status = Some(status);
        self.error = Some(code.into());
        self.inner.write_text_error(status, code, message).await
    }
    async fn write_response_failure(&mut self, failure: &ResponsesFailure) -> Result<()> {
        self.status = Some(failure.status);
        self.error = Some(failure.code.clone());
        self.inner.write_response_failure(failure).await
    }
    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        self.status = Some(status);
        self.inner.write_json(status, value).await
    }
    async fn start_event_stream(&mut self) -> Result<()> {
        self.status = Some(200);
        self.inner.start_event_stream().await
    }
    async fn write_event(&mut self, event: &Value) -> Result<()> {
        observe_lifecycle_response_failure(event);
        self.inner.write_event(event).await
    }
    async fn finish_event_stream(&mut self) -> Result<()> {
        self.inner.finish_event_stream().await
    }
    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        self.status = Some(response.status().as_u16());
        self.inner.proxy_response_with_probe(response, probe).await
    }
    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        discard: bool,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.inner
            .try_proxy_upstream_websocket_with_probe(route, headers, body, discard, probe)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_email_comes_only_from_official_route_metadata() {
        let (config, _, model) =
            super::super::tests::router_config("https://example.com/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let mut route = snapshot
            .target_for_model(&model)
            .unwrap()
            .route
            .as_ref()
            .clone();
        route.official_auth = Some(OfficialRouteAuth {
            account_id: "local-account".into(),
            email: Some("member@example.com".into()),
            path: PathBuf::from("/unused/auth.json"),
            accepts_incoming_authorization: false,
        });
        // 非官方线路即使意外携带账号元数据，也不能将邮箱交给插件。
        assert!(official_account_email(&route).is_none());
        route.official_account = true;
        assert_eq!(official_account_email(&route), Some("member@example.com"));
        route.official_auth.as_mut().unwrap().email = None;
        assert_eq!(json!(official_account_email(&route)), Value::Null);
        route.official_auth = None;
        assert_eq!(json!(official_account_email(&route)), Value::Null);
    }

    #[tokio::test]
    async fn upload_signal_arrives_only_after_the_body_is_consumed() {
        let payload = vec![7_u8; 40_000];
        let (mut body, mut uploaded) = signaled_request_body(Bytes::from(payload.clone()));
        assert_eq!(
            HttpBody::size_hint(&body).exact(),
            Some(payload.len() as u64)
        );
        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        let mut seen = 0_usize;
        loop {
            match Pin::new(&mut body).poll_frame(&mut context) {
                Poll::Ready(Some(Ok(frame))) => {
                    seen += frame.into_data().unwrap().len();
                    assert!(
                        uploaded.try_recv().is_err(),
                        "upload signal must wait until every chunk is taken"
                    );
                }
                Poll::Ready(Some(Err(error))) => panic!("{error}"),
                Poll::Ready(None) => break,
                Poll::Pending => panic!("in-memory body should be ready"),
            }
        }
        assert_eq!(seen, payload.len());
        uploaded.try_recv().unwrap();

        let (_empty, mut empty_uploaded) = signaled_request_body(Bytes::new());
        empty_uploaded.try_recv().unwrap();

        // 大于写缓冲的正文要拆成至少一块满缓冲的帧，最后一块也不得小于缓冲，
        // 否则连接会在尾部尚未写出时结束正文。
        let large = vec![9_u8; HYPER_H1_WRITE_BUFFER_BYTES * 2 + 10];
        let (mut body, mut uploaded) = signaled_request_body(Bytes::from(large));
        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut context) else {
            panic!("expected the first full-buffer chunk");
        };
        assert_eq!(
            frame.into_data().unwrap().len(),
            HYPER_H1_WRITE_BUFFER_BYTES
        );
        assert!(uploaded.try_recv().is_err());
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut context) else {
            panic!("expected the trailing chunk to stay above the write buffer");
        };
        assert_eq!(
            frame.into_data().unwrap().len(),
            HYPER_H1_WRITE_BUFFER_BYTES + 10
        );
        assert!(!HttpBody::is_end_stream(&body));
        assert!(uploaded.try_recv().is_err());
        assert!(matches!(
            Pin::new(&mut body).poll_frame(&mut context),
            Poll::Ready(None)
        ));
        assert!(HttpBody::is_end_stream(&body));
        uploaded.try_recv().unwrap();
    }

    #[test]
    fn replayed_history_keeps_a_first_token_budget_after_the_buffered_upload() {
        let small = response_header_timeout(32 * 1024, UPSTREAM_RESPONSE_HEADER_TIMEOUT);
        assert_eq!(small, UPSTREAM_RESPONSE_HEADER_TIMEOUT);

        let replayed = 8 * 1024 * 1024;
        let extended = response_header_timeout(replayed, UPSTREAM_RESPONSE_HEADER_TIMEOUT);
        assert_eq!(
            extended,
            UPSTREAM_RESPONSE_HEADER_TIMEOUT
                + Duration::from_secs((replayed as u64) / BUFFERED_UPLOAD_BYTES_PER_SEC)
        );
        assert!(extended < UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT);

        let huge = response_header_timeout(usize::MAX, UPSTREAM_RESPONSE_HEADER_TIMEOUT);
        assert_eq!(huge, UPSTREAM_RESPONSE_TIMEOUT);
    }

    #[tokio::test]
    async fn upload_releases_admission_before_response_headers() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (body_read, body_read_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            body_read.send(request.body.len()).unwrap();
            release_rx.await.unwrap();
            let body = b"ok";
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(body).await.unwrap();
        });
        let budget = Arc::new(Semaphore::new(1));
        let permit = budget.clone().try_acquire_owned().unwrap();
        let mut permit = Some(permit);
        let client = reqwest::Client::new();
        let send = send_for_response_headers(
            client.post(format!("http://{address}/")),
            Bytes::from_static(b"{\"input\":\"hello\"}"),
            Duration::from_secs(5),
            || retain_compact_request_budget(&mut permit, 0),
        );
        tokio::pin!(send);
        let uploaded = tokio::time::timeout(Duration::from_secs(2), async {
            body_read_rx.await.unwrap();
            loop {
                if budget.available_permits() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });
        tokio::select! {
            result = &mut send => panic!("headers arrived before the body was released: {result:?}"),
            result = uploaded => result.expect("admission should be free while headers are still pending"),
        }
        release.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), send)
            .await
            .expect("response headers")
            .expect("transport")
            .expect("upstream");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        server.await.unwrap();
    }
}
