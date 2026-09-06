use super::*;

#[async_trait]
pub(crate) trait ResponsesDownstream: Send {
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        Ok(future.await)
    }
    fn is_websocket(&self) -> bool {
        false
    }

    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        None
    }

    fn prepare_adapted_response_context(&mut self, _body: &mut Value) -> bool {
        false
    }

    fn remember_adapted_response(&mut self, _response_id: &str, _output: &[Value]) {}

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()>;

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        self.write_error(status, code, message, None).await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()>;
    async fn start_event_stream(&mut self) -> Result<()>;
    async fn write_event(&mut self, event: &Value) -> Result<()>;
    async fn finish_event_stream(&mut self) -> Result<()>;
    /// Relays a native upstream response. The probe, when present, records
    /// first-byte and completion timing; wrappers inject their own probe.
    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()>;

    async fn proxy_response(&mut self, response: reqwest::Response) -> Result<()> {
        self.proxy_response_with_probe(response, None).await
    }

    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        _route: &RouteTarget,
        _headers: &HeaderMap,
        _body: &mut Value,
        _probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        Ok(UpstreamWebSocketAttempt::UseHttp)
    }

    async fn try_proxy_upstream_websocket(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.try_proxy_upstream_websocket_with_probe(route, headers, body, None)
            .await
    }
}

// HTTP FIN cannot distinguish a legal write-half shutdown from cancellation.
// HTTP also avoids an extra async-trait allocation on every response chunk.
pub(crate) async fn await_upstream<D, T, F>(downstream: &mut D, future: F) -> Result<T>
where
    D: ResponsesDownstream + ?Sized,
    T: Send,
    F: std::future::Future<Output = T> + Send,
{
    if downstream.is_websocket() {
        downstream.wait_for_upstream(future).await
    } else {
        Ok(future.await)
    }
}

#[derive(Debug)]
pub(crate) struct DownstreamClosed;

impl std::fmt::Display for DownstreamClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("下游 WebSocket 已关闭")
    }
}

impl std::error::Error for DownstreamClosed {}

pub(crate) struct ObservedResponsesDownstream<'a, D>
where
    D: ResponsesDownstream + ?Sized,
{
    inner: &'a mut D,
    probe: Option<RouteRequestLogProbe>,
}

impl<'a, D> ObservedResponsesDownstream<'a, D>
where
    D: ResponsesDownstream + ?Sized,
{
    pub(crate) fn new(inner: &'a mut D, probe: Option<RouteRequestLogProbe>) -> Self {
        Self { inner, probe }
    }

    pub(crate) fn finish_result(&self, result: &Result<()>, operation: &str) {
        let Some(probe) = self.probe.as_ref() else {
            return;
        };
        if result.is_ok() {
            probe.finish_success();
        } else {
            probe.mark_cancelled(operation);
            probe.finish_cancelled();
        }
    }
}

#[async_trait]
impl<D> ResponsesDownstream for ObservedResponsesDownstream<'_, D>
where
    D: ResponsesDownstream + ?Sized,
{
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        let result = self.inner.wait_for_upstream(future).await;
        if let Err(error) = &result
            && error.is::<DownstreamClosed>()
            && let Some(probe) = &self.probe
        {
            probe.mark_cancelled("downstream_websocket_closed");
            probe.finish_cancelled();
        }
        result
    }
    fn is_websocket(&self) -> bool {
        self.inner.is_websocket()
    }

    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        self.probe.as_ref()
    }

    fn prepare_adapted_response_context(&mut self, body: &mut Value) -> bool {
        self.inner.prepare_adapted_response_context(body)
    }

    fn remember_adapted_response(&mut self, response_id: &str, output: &[Value]) {
        self.inner.remember_adapted_response(response_id, output);
    }

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.mark_error(status, code);
        }
        let result = self.inner.write_error(status, code, message, route).await;
        self.finish_result(&result, "downstream_error_write_failed");
        result
    }

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.mark_error(status, code);
        }
        let result = self.inner.write_text_error(status, code, message).await;
        self.finish_result(&result, "downstream_error_write_failed");
        result
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.observe_response(status, value);
        }
        let result = self.inner.write_json(status, value).await;
        if result.is_ok()
            && (200..300).contains(&status)
            && let Some(probe) = self.probe.as_ref()
        {
            probe.mark_first_downstream_content();
        }
        self.finish_result(&result, "downstream_json_write_failed");
        result
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        let result = self.inner.start_event_stream().await;
        if let Some(probe) = self.probe.as_ref() {
            if result.is_ok() {
                probe.mark_response_started(200);
            } else {
                probe.mark_cancelled("downstream_stream_header_write_failed");
                probe.finish_cancelled();
            }
        }
        result
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.observe_event(event);
        }
        let result = self.inner.write_event(event).await;
        if let Some(probe) = self.probe.as_ref() {
            if result.is_ok() {
                probe.mark_response_started(200);
                if responses_event_has_user_content(event) {
                    probe.mark_first_downstream_content();
                }
            } else {
                probe.mark_cancelled("downstream_event_write_failed");
                probe.finish_cancelled();
            }
        }
        result
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        let result = self.inner.finish_event_stream().await;
        self.finish_result(&result, "downstream_stream_finish_failed");
        result
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        let result = self
            .inner
            .proxy_response_with_probe(response, probe.or(self.probe.as_ref()))
            .await;
        self.finish_result(&result, "downstream_proxy_write_failed");
        result
    }

    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        let result = self
            .inner
            .try_proxy_upstream_websocket_with_probe(
                route,
                headers,
                body,
                probe.or(self.probe.as_ref()),
            )
            .await;
        match &result {
            Ok(UpstreamWebSocketAttempt::Completed) => {
                if let Some(probe) = self.probe.as_ref() {
                    probe.finish_success();
                }
            }
            Ok(UpstreamWebSocketAttempt::UseHttp) => {}
            Err(error) => {
                if let Some(probe) = self.probe.as_ref() {
                    if error.is::<DownstreamClosed>() {
                        probe.mark_cancelled("downstream_websocket_closed");
                    } else {
                        probe.mark_error(502, "upstream_websocket_proxy_failed");
                    }
                }
            }
        }
        result
    }
}

pub(crate) struct HttpResponsesDownstream {
    pub(crate) stream: TcpStream,
}

impl HttpResponsesDownstream {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self { stream }
    }
}

#[async_trait]
impl ResponsesDownstream for HttpResponsesDownstream {
    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        write_error_response(&mut self.stream, status, code, message, route).await
    }

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        write_text_error_response(&mut self.stream, status, code, message).await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        write_json_response(&mut self.stream, status, value).await
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\n{}connection: close\r\n\r\n",
            router_request_id_header()
        );
        write_all_with_timeout(
            &mut self.stream,
            header.as_bytes(),
            "写入 Responses SSE 响应头失败",
        )
        .await
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        write_responses_sse_event(&mut self.stream, event).await
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        finish_chunked_response(&mut self.stream).await
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        write_proxy_response(&mut self.stream, response, probe).await
    }
}

pub(crate) struct WebSocketResponsesDownstream {
    pub(crate) socket: WebSocketStream<TcpStream>,
    pub(crate) upstream: Option<CachedUpstreamWebSocket>,
    pub(crate) websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    pub(crate) stream_id: Option<String>,
    pub(crate) adapted_history: AdaptedResponsesHistory,
    pub(crate) terminal_started: bool,
    pub(crate) pending_messages: VecDeque<(WebSocketMessage, Option<OwnedSemaphorePermit>)>,
    pub(crate) pending_budget_blocked: bool,
    pub(crate) request_body_budget: Arc<Semaphore>,
}

#[derive(Debug, Default)]
pub(crate) struct AdaptedResponsesHistory {
    pub(crate) last: Option<(String, Vec<Value>)>,
    pub(crate) pending_input: Option<Vec<Value>>,
}

impl AdaptedResponsesHistory {
    pub(crate) fn prepare(&mut self, body: &mut Value) -> bool {
        self.pending_input = None;
        let Some(object) = body.as_object_mut() else {
            return false;
        };
        let input = match object.get("input") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(input)) => input.clone(),
            Some(input @ (Value::String(_) | Value::Object(_))) => vec![input.clone()],
            Some(_) => return false,
        };
        let previous_response_id = object
            .get("previous_response_id")
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
            .map(str::trim);
        let mut context = if let Some(previous_response_id) = previous_response_id {
            if !is_codey_synthetic_response_id(previous_response_id) {
                return false;
            }
            let Some((_, context)) = self
                .last
                .as_ref()
                .filter(|(response_id, _)| response_id == previous_response_id)
            else {
                return false;
            };
            context.clone()
        } else {
            Vec::new()
        };
        context.extend(input);
        self.pending_input = Some(context.clone());
        if previous_response_id.is_none() {
            return false;
        }
        object.remove("previous_response_id");
        object.insert("input".to_string(), Value::Array(context));
        true
    }

    pub(crate) fn remember(&mut self, response_id: &str, output: &[Value]) {
        let Some(mut context) = self.pending_input.take() else {
            return;
        };
        context.extend(output.iter().cloned());
        // ponytail: Codex continuations are linear; retain branches only if a client needs them.
        self.last = Some((response_id.to_string(), context));
    }
}
