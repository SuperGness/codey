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
    fn event_stream_started(&self) -> bool {
        false
    }

    fn set_streaming_errors(&mut self, _enabled: bool) {}

    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        None
    }

    fn select_route(&mut self, _route: &RouteTarget) {}

    fn prepare_native_http_fallback(
        &mut self,
        _route: &RouteTarget,
        _headers: &HeaderMap,
        _body: &mut Value,
    ) -> Result<bool> {
        Ok(false)
    }

    fn prepare_adapted_response_context(&mut self, _body: &mut Value) -> Result<bool> {
        Ok(false)
    }

    fn remember_adapted_response(&mut self, _response_id: &str, _output: &[Value]) -> Result<()> {
        Ok(())
    }

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

    async fn write_response_failure(&mut self, failure: &ResponsesFailure) -> Result<()> {
        self.write_error(
            failure.status,
            &failure.code,
            failure.message().into(),
            None,
        )
        .await
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
        _discard_opaque_reasoning: bool,
        _probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        Ok(UpstreamWebSocketAttempt::UseHttp)
    }

    async fn try_proxy_upstream_websocket(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        discard_opaque_reasoning: bool,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.try_proxy_upstream_websocket_with_probe(
            route,
            headers,
            body,
            discard_opaque_reasoning,
            None,
        )
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
    if downstream.is_websocket() || downstream.event_stream_started() {
        downstream.wait_for_upstream(future).await
    } else {
        Ok(future.await)
    }
}

#[derive(Debug)]
pub(crate) struct DownstreamClosed;

impl std::fmt::Display for DownstreamClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("下游连接已关闭")
    }
}

impl std::error::Error for DownstreamClosed {}

#[derive(Debug)]
pub(crate) struct DownstreamResponseInterrupt;

impl std::fmt::Display for DownstreamResponseInterrupt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("下游请求中断当前 Responses 响应")
    }
}

impl std::error::Error for DownstreamResponseInterrupt {}

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
        } else if result
            .as_ref()
            .err()
            .is_some_and(|error| error.is::<DownstreamClosed>())
        {
            probe.mark_cancelled(operation);
            probe.finish_cancelled();
        } else {
            probe.mark_error(502, "upstream_response_failed");
            probe.finish_failed();
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
    fn event_stream_started(&self) -> bool {
        self.inner.event_stream_started()
    }

    fn set_streaming_errors(&mut self, enabled: bool) {
        self.inner.set_streaming_errors(enabled);
    }

    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        self.probe.as_ref()
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

    fn remember_adapted_response(&mut self, response_id: &str, output: &[Value]) -> Result<()> {
        self.inner.remember_adapted_response(response_id, output)
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

    async fn write_response_failure(&mut self, failure: &ResponsesFailure) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.mark_error(failure.status, &failure.code);
        }
        let result = self.inner.write_response_failure(failure).await;
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
        discard_opaque_reasoning: bool,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        let result = self
            .inner
            .try_proxy_upstream_websocket_with_probe(
                route,
                headers,
                body,
                discard_opaque_reasoning,
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
    event_stream_started: bool,
    streaming_errors: bool,
}

impl HttpResponsesDownstream {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            event_stream_started: false,
            streaming_errors: false,
        }
    }
}

#[async_trait]
impl ResponsesDownstream for HttpResponsesDownstream {
    fn event_stream_started(&self) -> bool {
        self.event_stream_started
    }

    fn set_streaming_errors(&mut self, enabled: bool) {
        self.streaming_errors = enabled;
    }

    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        if self.event_stream_started {
            await_http_stream_upstream(&mut self.stream, future).await
        } else {
            Ok(future.await)
        }
    }

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        if self.streaming_errors || self.event_stream_started {
            return self
                .write_response_failure(&ResponsesFailure::new(status, code, message, route))
                .await;
        }
        write_error_response(&mut self.stream, status, code, message, route).await
    }

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        if self.streaming_errors || self.event_stream_started {
            return self.write_error(status, code, message, None).await;
        }
        write_text_error_response(&mut self.stream, status, code, message).await
    }

    async fn write_response_failure(&mut self, failure: &ResponsesFailure) -> Result<()> {
        if self.streaming_errors || self.event_stream_started {
            self.start_event_stream().await?;
            self.write_event(&failure.normalized_event()).await?;
            self.finish_event_stream().await
        } else if failure.code == CONTEXT_LENGTH_EXCEEDED {
            write_json_response(
                &mut self.stream,
                failure.status,
                &json!({
                    "error": failure.event["response"]["error"],
                }),
            )
            .await
        } else {
            write_text_error_response_with_retry_after(
                &mut self.stream,
                failure.status,
                &failure.code,
                failure.message(),
                failure
                    .retry_advice
                    .as_ref()
                    .map(|advice| advice.header.as_str()),
            )
            .await
        }
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        write_json_response(&mut self.stream, status, value).await
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        if self.event_stream_started {
            return Ok(());
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\n{}connection: close\r\n\r\n",
            router_request_id_header()
        );
        write_all_with_timeout(
            &mut self.stream,
            header.as_bytes(),
            "写入 Responses SSE 响应头失败",
        )
        .await?;
        self.event_stream_started = true;
        Ok(())
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        let event = normalized_response_event(event);
        write_responses_sse_event(&mut self.stream, &event).await
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        finish_chunked_response(&mut self.stream).await
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        write_proxy_response(&mut self.stream, response, probe, true).await
    }
}

pub(crate) struct WebSocketResponsesDownstream {
    pub(crate) socket: WebSocketStream<TcpStream>,
    pub(crate) upstream: Option<CachedUpstreamWebSocket>,
    pub(crate) websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    pub(crate) stream_id: Option<String>,
    pub(crate) adapted_history: AdaptedResponsesHistory,
    pub(crate) native_history: NativeResponsesHistory,
    pub(crate) steering: SteeringState,
    pub(crate) pending_interrupt: Option<Value>,
    pub(crate) interrupt_forwarding: bool,
    pub(crate) terminal_started: bool,
    pub(crate) pending_messages: VecDeque<(WebSocketMessage, Option<OwnedSemaphorePermit>)>,
    pub(crate) pending_budget_blocked: bool,
    pub(crate) request_body_budget: Arc<Semaphore>,
    pub(crate) config_changes: tokio::sync::watch::Receiver<u64>,
    pub(crate) idle_registry: Arc<Mutex<IdleDownstreamRegistry>>,
}

/// Tool calls announce their payload in a separate event, so an item whose
/// payload has not arrived yet is not replayable.
fn streamed_item_arguments(item: &Value) -> Option<&str> {
    let field = match item.get("type").and_then(Value::as_str)? {
        "function_call" => "arguments",
        "custom_tool_call" => "input",
        _ => return Some(""),
    };
    item.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Items are the same history entry when they share an item id, or, for tool
/// calls without one, the same call id.
/// Items are the same history entry when they describe the same tool call, or
/// failing that share an item id. Tool calls compare by call id first because
/// an upstream may renumber an item id while keeping the call.
fn same_history_item(left: &Value, right: &Value) -> bool {
    let identity = |item: &Value| {
        item.get("call_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|id| format!("call_id:{id}"))
            .or_else(|| {
                item.get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
    };
    match (identity(left), identity(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[derive(Debug, Default)]
pub(crate) struct AdaptedResponsesHistory {
    pub(crate) last: Option<(String, Vec<Value>)>,
    pub(crate) pending_input: Option<Vec<Value>>,
    pub(crate) requires_native_http: bool,
    /// Items the upstream streamed for the current turn. Streaming upstreams
    /// announce tool calls through output-item events and may omit them from
    /// the terminal `response.output`, so the terminal array alone cannot be
    /// trusted to describe the turn. Each entry carries whether the payload
    /// has arrived.
    pending_output: Vec<(usize, Value, bool)>,
    pending_output_overflowed: bool,
    last_bytes: usize,
    budget: RetainedMemoryBudget,
}

const STREAMED_OUTPUT_ITEM_LIMIT: usize = 512;

impl AdaptedResponsesHistory {
    pub(crate) fn clear_pending(&mut self) {
        self.pending_input = None;
        self.pending_output.clear();
        self.pending_output_overflowed = false;
        self.budget
            .resize(self.last_bytes)
            .expect("releasing retained history budget cannot fail");
    }

    /// Retain the output items of the turn being built. A streaming upstream
    /// can report a tool call only through its output-item events while the
    /// terminal event omits it, which would strand that call's result next
    /// turn as an unpaired tool result.
    pub(crate) fn record_output_item(&mut self, event: &Value) {
        if self.pending_input.is_none() {
            return;
        }
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .map(|index| index as usize);
        match kind {
            "response.output_item.added" | "response.output_item.done" => {
                let Some(item) = event.get("item").filter(|item| item.is_object()) else {
                    return;
                };
                // A skeleton announced by `added` still needs its arguments.
                let complete =
                    kind == "response.output_item.done" || streamed_item_arguments(item).is_some();
                self.store_streamed_item(
                    index.unwrap_or(self.pending_output.len()),
                    item.clone(),
                    complete,
                );
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                let field = if kind == "response.function_call_arguments.done" {
                    "arguments"
                } else {
                    "input"
                };
                let Some(value) = event.get(field).filter(|value| !value.is_null()) else {
                    return;
                };
                let Some(index) = index.or_else(|| self.streamed_item_index(event)) else {
                    return;
                };
                let Some(existing) = self
                    .pending_output
                    .iter_mut()
                    .find(|(slot, _, _)| *slot == index)
                else {
                    return;
                };
                if let Some(item) = existing.1.as_object_mut() {
                    item.insert(field.to_string(), value.clone());
                }
                existing.2 = true;
            }
            _ => {}
        }
    }

    fn store_streamed_item(&mut self, index: usize, item: Value, complete: bool) {
        if let Some(existing) = self
            .pending_output
            .iter_mut()
            .find(|(slot, _, _)| *slot == index)
        {
            if complete || !existing.2 {
                existing.1 = item;
                existing.2 = complete;
            }
            return;
        }
        if self.pending_output.len() >= STREAMED_OUTPUT_ITEM_LIMIT {
            self.pending_output_overflowed = true;
            return;
        }
        self.pending_output.push((index, item, complete));
    }

    /// Match an arguments/input event to an already announced item by its id.
    fn streamed_item_index(&self, event: &Value) -> Option<usize> {
        let item_id = event
            .get("item_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())?;
        self.pending_output
            .iter()
            .find(|(_, item, _)| item.get("id").and_then(Value::as_str) == Some(item_id))
            .map(|(index, _, _)| *index)
    }

    pub(crate) fn has_streamed_output(&self) -> bool {
        !self.pending_output.is_empty()
    }

    /// Merge the streamed items into the terminal array. Streamed items fill
    /// positions the terminal event dropped; items the terminal event already
    /// carries win. Items still missing their arguments are skipped, because a
    /// replayed call without arguments is worse than a missing call.
    fn take_streamed_output(&mut self, terminal: &[Value]) -> Vec<Value> {
        let mut streamed = std::mem::take(&mut self.pending_output);
        if streamed.is_empty() {
            return terminal.to_vec();
        }
        streamed.sort_by_key(|(index, _, _)| *index);
        let mut merged = terminal.to_vec();
        for (index, item, complete) in streamed {
            if !complete {
                continue;
            }
            // 匿名输出也可能同时出现在流式事件和终态中；仅按位置去重，
            // 保留不同位置上内容相同的合法消息。
            if merged.get(index) == Some(&item)
                || merged
                    .iter()
                    .any(|existing| same_history_item(existing, &item))
            {
                continue;
            }
            if index < merged.len() {
                merged.insert(index, item);
            } else {
                merged.push(item);
            }
        }
        merged
    }

    pub(crate) fn prepare(&mut self, body: &mut Value) -> Result<bool> {
        self.prepare_context(body, false, true, None)
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.last_bytes
    }

    pub(crate) fn stage_native(
        &mut self,
        body: &mut Value,
        previous: Option<&Self>,
    ) -> Result<bool> {
        self.prepare_context(body, true, false, previous)
    }

    fn prepare_context(
        &mut self,
        body: &mut Value,
        native: bool,
        expand: bool,
        previous_history: Option<&Self>,
    ) -> Result<bool> {
        self.pending_input = None;
        self.pending_output.clear();
        self.pending_output_overflowed = false;
        self.budget.resize(self.last_bytes)?;
        // Count without allocating a second encoded request. Reserve before
        // cloning history into pending state and the expanded request body.
        let request_bytes = bounded_json_bytes(body, MAX_REQUEST_BYTES)?;
        let Some(object) = body.as_object_mut() else {
            return Ok(false);
        };
        let previous_response_id = object
            .get("previous_response_id")
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
            .map(str::trim);
        let (last, previous_bytes) = previous_history
            .map_or((&self.last, self.last_bytes), |history| {
                (&history.last, history.last_bytes)
            });
        let previous = if let Some(previous_response_id) = previous_response_id {
            if !native && !is_codey_synthetic_response_id(previous_response_id) {
                return Ok(false);
            }
            let Some((_, context)) = last
                .as_ref()
                .filter(|(response_id, _)| response_id == previous_response_id)
            else {
                anyhow::bail!("会话历史已失效，请压缩上下文后重新发送完整输入");
            };
            Some(context)
        } else {
            None
        };
        let expanded_bytes = request_bytes.saturating_add(if previous.is_some() {
            previous_bytes
        } else {
            0
        });
        if expanded_bytes > MAX_REQUEST_BYTES {
            anyhow::bail!("展开后的会话历史超过上限，请先压缩上下文");
        }
        self.budget.resize(
            self.last_bytes
                .saturating_add(expanded_bytes.saturating_mul(2)),
        )?;
        let input = match object.get("input") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(input)) => input.clone(),
            Some(Value::String(input)) if native => vec![json!({"role":"user","content":input})],
            Some(input @ (Value::String(_) | Value::Object(_))) => vec![input.clone()],
            Some(_) => return Ok(false),
        };
        let mut context = previous.cloned().unwrap_or_default();
        context.extend(input);
        self.requires_native_http = previous_history
            .is_some_and(|history| history.requires_native_http)
            || context.iter().any(native_item_requires_http);
        if !expand || previous_response_id.is_none() {
            self.pending_input = Some(context);
            return Ok(false);
        }
        self.pending_input = Some(context.clone());
        object.remove("previous_response_id");
        object.insert("input".to_string(), Value::Array(context));
        Ok(true)
    }

    pub(crate) fn remember(&mut self, response_id: &str, output: &[Value]) -> Result<()> {
        if self.pending_input.is_none() {
            self.clear_pending();
            return Ok(());
        }
        if self.pending_output_overflowed {
            anyhow::bail!("流式输出条目超过会话历史上限，无法确认完整上下文，请重新发送完整输入");
        }
        let output = self.take_streamed_output(output);
        self.requires_native_http |= output.iter().any(native_item_requires_http);
        let bytes = bounded_json_bytes(
            self.pending_input
                .as_ref()
                .expect("pending input was checked"),
            MAX_REQUEST_BYTES,
        )?
        .saturating_add(bounded_json_bytes(&output, MAX_REQUEST_BYTES)?);
        if bytes > MAX_REQUEST_BYTES {
            anyhow::bail!("会话历史超过上限，请先压缩上下文");
        }
        self.budget
            .resize(self.last_bytes.saturating_add(bytes.saturating_mul(2)))?;
        let mut context = self
            .pending_input
            .take()
            .expect("pending input was checked");
        context.extend(output);
        // ponytail: Codex continuations are linear; retain branches only if a client needs them.
        self.last = Some((response_id.to_string(), context));
        self.last_bytes = bytes;
        self.budget.resize(bytes)?;
        Ok(())
    }
}
