use super::*;
use crate::codey_plugins::lifecycle::LifecycleRequest;

/// 追加发送只能由已收到的响应触发。连接失败、发送中断和下游已输出均不能重放。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendState {
    Ready,
    Sending,
    ResponseReceived,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryReason {
    Plugin,
    ReasoningText,
    EncryptedReasoning,
}

impl RetryReason {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Plugin => "plugin_request_retry",
            Self::ReasoningText => "reasoning_text_placeholder_retry",
            Self::EncryptedReasoning => "rejected_encrypted_reasoning_retry",
        }
    }
}

/// 一次请求最多发送两次；插件与 reasoning 修复使用同一份预算。
pub(super) struct AttemptBudget {
    state: SendState,
    retry_reason: Option<RetryReason>,
    pub(super) transport: UpstreamTransport,
}

impl Default for AttemptBudget {
    fn default() -> Self {
        Self {
            state: SendState::Ready,
            retry_reason: None,
            transport: UpstreamTransport::Http,
        }
    }
}

impl AttemptBudget {
    pub(super) fn attempt_index(&self) -> u32 {
        u32::from(self.retry_reason.is_some())
    }

    pub(super) fn begin_send(&mut self) -> Result<()> {
        anyhow::ensure!(self.state == SendState::Ready, "上游请求没有可用的发送许可");
        self.state = SendState::Sending;
        Ok(())
    }

    pub(super) fn finish_send(&mut self, received_response: bool) {
        self.state = if received_response {
            SendState::ResponseReceived
        } else {
            SendState::Failed
        };
    }

    pub(super) fn can_retry(&self, response_started: bool) -> bool {
        !response_started
            && self.retry_reason.is_none()
            && self.state == SendState::ResponseReceived
    }

    pub(super) fn reserve_retry(&mut self, reason: RetryReason, response_started: bool) -> bool {
        if !self.can_retry(response_started) {
            return false;
        }
        self.retry_reason = Some(reason);
        self.state = SendState::Ready;
        true
    }

    pub(super) fn retry_reason(&self) -> Option<RetryReason> {
        self.retry_reason
    }
}

/// 编码完成后只保留发送所需的字节和对应内存名额，不再保留 JSON 树。
pub(super) struct EncodedHttpRequest {
    pub(super) body: Bytes,
    pub(super) admission: Option<OwnedSemaphorePermit>,
}

pub(super) struct HttpResponseHead {
    pub(super) status: u16,
    pub(super) request_id: Option<String>,
    pub(super) retry_advice: Option<ResponseRetryAdvice>,
}

impl HttpResponseHead {
    fn capture(response: &reqwest::Response, probe: Option<&RouteRequestLogProbe>) -> Self {
        if let Some(probe) = probe {
            probe.set_upstream_response_headers(&responses::format_upstream_response_headers(
                response.headers(),
            ));
        }
        Self {
            status: response.status().as_u16(),
            request_id: upstream_request_id_from_headers(response.headers()),
            retry_advice: ResponseRetryAdvice::from_headers(response.headers()),
        }
    }

    fn record(&self, probe: Option<&RouteRequestLogProbe>) {
        if let Some(probe) = probe {
            probe.mark_upstream_headers(self.status, self.request_id.as_deref());
        }
    }
}

/// 成功正文仍由协议处理器流式读取；错误正文只读取一次，保留原始响应信息。
pub(super) enum ReceivedHttpResponse {
    Success(reqwest::Response),
    Rejected {
        head: HttpResponseHead,
        body: Vec<u8>,
    },
}

pub(super) struct HttpExecution<'a> {
    pub(super) server: &'a RouterServer,
    pub(super) resolved: &'a RouteSelection,
    pub(super) bridge: ProtocolBridge,
    pub(super) request_kind: ResponsesRequestKind,
    pub(super) compacting: bool,
    pub(super) native_compaction: bool,
    pub(super) compaction_phase: GatewayCompactionPhase,
    pub(super) upstream_url: &'a str,
    pub(super) upstream_stream_requested: bool,
}

impl HttpExecution<'_> {
    fn response_header_timeout(&self) -> Duration {
        if self.upstream_stream_requested {
            if self.compacting {
                COMPACTION_RESPONSE_HEADER_TIMEOUT
            } else {
                UPSTREAM_RESPONSE_HEADER_TIMEOUT
            }
        } else {
            UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT
        }
    }

    fn reasoning_retry_allowed(&self) -> bool {
        matches!(
            self.bridge,
            ProtocolBridge::NativeResponses | ProtocolBridge::ResponsesToChatCompletions
        ) && self.request_kind == ResponsesRequestKind::Create
            && !self.compacting
            && !self.resolved.route.official_account
    }

    /// 官方账号同样可能拒绝历史中的 reasoning 密文；只在正文确实带密文时保留重发副本。
    fn encrypted_reasoning_retry_allowed(&self, encoded: &[u8]) -> bool {
        self.bridge == ProtocolBridge::NativeResponses
            && (!self.compacting || self.native_compaction)
            && memchr::memmem::find(encoded, b"\"encrypted_content\"").is_some()
    }

    /// 只在明确的 400 reasoning 缺失或密文校验失败响应后构造修复请求。
    /// 无法取得工作集预算时保留原错误。
    fn prepare_reasoning_retry(
        &self,
        error_body: &[u8],
        encoded: Option<&[u8]>,
        summaries: &[Option<String>],
        admission: &mut Option<OwnedSemaphorePermit>,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<Option<(Bytes, RetryReason)>> {
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let rejected_item = (self.bridge == ProtocolBridge::NativeResponses)
            .then(|| http_error_rejected_reasoning_item(error_body))
            .flatten();
        let reason = if rejected_item.is_some() {
            RetryReason::EncryptedReasoning
        } else if self.reasoning_retry_allowed() && requires_reasoning_text_fallback(error_body) {
            RetryReason::ReasoningText
        } else {
            return Ok(None);
        };
        if let Some(id) = rejected_item.as_deref() {
            // 即使本次无法重发，后续请求也应避开这一项。
            record_rejected_reasoning_item(id);
        }
        if grow_request_body_budget(admission, &self.server.request_body_budget, encoded.len())
            .is_err()
        {
            return Ok(None);
        }
        let mut body =
            serde_json::from_slice::<Value>(encoded).context("解析 reasoning 回退请求失败")?;
        let repaired = match reason {
            RetryReason::EncryptedReasoning => strip_rejected_reasoning_items(&mut body),
            _ => fill_missing_reasoning_text_with_chat_summaries(&mut body, summaries),
        };
        if !repaired {
            return Ok(None);
        }
        let encoded =
            serde_json::to_vec(&body).context("序列化补齐 reasoning 明文的 Responses 请求失败")?;
        if let Some(probe) = probe {
            probe.record_upstream_body(RequestBodySummary::from_responses_body(
                &body,
                Some(encoded.len() as u64),
            ));
        }
        Ok(Some((encoded.into(), reason)))
    }

    /// 执行阶段只负责发送和有限修复，不向成功正文写入任何协议事件。
    pub(super) async fn send<D: ResponsesDownstream + ?Sized>(
        &self,
        downstream: &mut D,
        lifecycle: &mut LifecycleRequest,
        client: &reqwest::Client,
        headers: &mut HeaderMap,
        request: EncodedHttpRequest,
        summaries: &[Option<String>],
    ) -> Result<Option<ReceivedHttpResponse>> {
        let EncodedHttpRequest {
            body: mut encoded,
            mut admission,
        } = request;
        let mut retryable_encoded = (self.reasoning_retry_allowed()
            || self.encrypted_reasoning_retry_allowed(&encoded))
        .then(|| encoded.clone());
        let mut attempts = AttemptBudget {
            transport: if self.upstream_stream_requested {
                UpstreamTransport::HttpSse
            } else {
                UpstreamTransport::Http
            },
            ..Default::default()
        };
        let header_timeout = self.response_header_timeout();
        loop {
            let retained_after_upload = if retryable_encoded.is_some() || lifecycle.is_active() {
                encoded.len()
            } else {
                0
            };
            let result = send_lifecycle_http(
                downstream,
                lifecycle,
                client,
                self.upstream_url,
                headers,
                encoded,
                &mut || retain_compact_request_budget(&mut admission, retained_after_upload),
                &mut attempts,
                header_timeout,
                self.resolved.route.plugin_transport.as_ref(),
            )
            .await?;
            let Some(response) = self.finish_send(downstream, result, header_timeout).await? else {
                return Ok(None);
            };
            let head = HttpResponseHead::capture(&response, downstream.request_log_probe());
            if response.status().is_success() {
                head.record(downstream.request_log_probe());
                return Ok(Some(ReceivedHttpResponse::Success(response)));
            }
            let can_repair_reasoning = head.status == 400
                && retryable_encoded.is_some()
                && attempts.can_retry(downstream.event_stream_started());
            if !can_repair_reasoning {
                // 响应头已排除修复可能，无须在读取错误正文时继续占用请求内存。
                retryable_encoded.take();
                admission.take();
                head.record(downstream.request_log_probe());
            }
            let probe = downstream.request_log_probe().cloned();
            let result = await_upstream(
                downstream,
                read_bounded_upstream_error_body(
                    response,
                    probe.as_ref(),
                    upstream_response_body_deadline(),
                ),
            )
            .await;
            let body = match result {
                Ok(Ok(body)) => body,
                Ok(Err(error)) | Err(error) => {
                    // 错误正文已不可用，无法构造修复请求；慢下游写回前释放重放副本和名额。
                    retryable_encoded.take();
                    admission.take();
                    if can_repair_reasoning {
                        head.record(downstream.request_log_probe());
                    }
                    self.finish_error_body_failure(downstream, error, head)
                        .await?;
                    return Ok(None);
                }
            };
            if can_repair_reasoning
                && attempts.can_retry(downstream.event_stream_started())
                && let Some((retry, reason)) = self.prepare_reasoning_retry(
                    &body,
                    retryable_encoded.as_deref(),
                    summaries,
                    &mut admission,
                    probe.as_ref(),
                )?
            {
                // 预读期间可能发生取消或下游输出；发送许可在真正重发前再次检查。
                if attempts.reserve_retry(reason, downstream.event_stream_started()) {
                    if let Some(probe) = probe.as_ref() {
                        probe.finish_upstream_attempt(
                            crate::route_request_log::AttemptOutcome::Superseded,
                            crate::route_request_log::AttemptPhase::ResponseBody,
                        );
                        probe.mark_fallback(
                            attempts.retry_reason().expect("reserved retry").label(),
                        );
                    }
                    retryable_encoded.take();
                    encoded = retry;
                    retain_compact_request_budget(&mut admission, encoded.len());
                    continue;
                }
            }
            if can_repair_reasoning {
                head.record(downstream.request_log_probe());
            }
            return Ok(Some(ReceivedHttpResponse::Rejected { head, body }));
        }
    }

    async fn finish_error_body_failure<D: ResponsesDownstream + ?Sized>(
        &self,
        downstream: &mut D,
        error: anyhow::Error,
        head: HttpResponseHead,
    ) -> Result<()> {
        // 取消直接向外传播，让生命周期归类为取消，不写成压缩失败。
        if error.is::<DownstreamClosed>() || error.is::<DownstreamResponseInterrupt>() {
            return Err(error);
        }
        if let Some(probe) = downstream.request_log_probe() {
            use crate::route_request_log::{AttemptOutcome, AttemptPhase};
            probe.finish_upstream_attempt(
                if is_upstream_timeout_error(&error) {
                    AttemptOutcome::TimedOut
                } else {
                    AttemptOutcome::Failed
                },
                AttemptPhase::ResponseBody,
            );
        }
        if self.compacting && !self.native_compaction {
            write_gateway_compaction_response_error(
                downstream,
                error,
                &self.resolved.route,
                self.compaction_phase,
                head.request_id.as_deref(),
                head.retry_advice,
            )
            .await
        } else if is_upstream_timeout_error(&error) {
            write_upstream_error_body_timeout(downstream, self.resolved, self.compacting, &error)
                .await
        } else {
            finish_upstream_response_result(downstream, Err(error), self.resolved).await
        }
    }

    async fn finish_send<D: ResponsesDownstream + ?Sized>(
        &self,
        downstream: &mut D,
        response_result: std::result::Result<
            std::result::Result<reqwest::Response, reqwest::Error>,
            tokio::time::error::Elapsed,
        >,
        response_header_timeout: Duration,
    ) -> Result<Option<reqwest::Response>> {
        let resolved = self.resolved;
        let bridge = self.bridge;
        let request_kind = self.request_kind;
        let upstream_stream_requested = self.upstream_stream_requested;
        let compacting = self.compacting;
        let response = match response_result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) if compacting && error.is_timeout() => {
                let detail = sanitize_upstream_error_text(
                    &error.without_url().to_string(),
                    &resolved.route,
                    256,
                )
                .unwrap_or_else(|| "上游未在期限内返回响应头".to_string());
                downstream
                    .write_error(
                        504,
                        "compaction_timeout",
                        format!(
                            "远程压缩等待上游响应超时（{detail}），原始会话历史未被 Codey 修改，请稍后重试"
                        ),
                        Some(&resolved.route),
                    )
                    .await?;
                return Ok(None);
            }
            Ok(Err(error)) => {
                let timeout = error.is_timeout();
                let connect = error.is_connect();
                let sanitized_error = error.without_url().to_string();
                record_router_failure_nonblocking(
                    "local_router_upstream_failed",
                    "proxy_local_router_request",
                    sanitized_error,
                    serde_json::json!({
                        "routeId": resolved.provider_id.as_str(),
                        "routeName": resolved.route.route_name.as_str(),
                        "requestedModel": resolved.requested_model.as_str(),
                        "model": resolved.upstream_model.as_str(),
                        "timeout": timeout,
                        "connect": connect,
                        "upstream": resolved.route.upstream_authority.as_str(),
                        "upstreamProtocol": bridge.upstream_protocol().label(),
                        "protocolBridge": bridge.label(),
                        "requestKind": request_kind.label(),
                        "upstreamStream": upstream_stream_requested,
                        "responseHeaderTimeoutSeconds": response_header_timeout.as_secs(),
                        "requestId": current_router_request_id(),
                    }),
                );
                let route_name = route_display_name(&resolved.route);
                let upstream = resolved.route.upstream_authority.as_str();
                let (status, code, message) = if timeout {
                    (
                        504,
                        "upstream_timeout",
                        format!(
                            "Codey 线路「{route_name}」请求上游 {upstream} 超时；请检查上游服务状态或网络连接"
                        ),
                    )
                } else {
                    (
                        424,
                        "upstream_unreachable",
                        format!(
                            "Codey 线路「{route_name}」无法连接上游 {upstream}；请确认上游服务已启动，并检查线路 URL、证书和网络设置"
                        ),
                    )
                };
                downstream.write_text_error(status, code, message).await?;
                return Ok(None);
            }
            Err(_) => {
                record_router_failure_nonblocking(
                    "local_router_upstream_failed",
                    "wait_for_local_router_upstream_headers",
                    "等待上游响应头超时",
                    serde_json::json!({
                        "routeId": resolved.provider_id.as_str(),
                        "routeName": resolved.route.route_name.as_str(),
                        "requestedModel": resolved.requested_model.as_str(),
                        "model": resolved.upstream_model.as_str(),
                        "timeout": true,
                        "stage": "response_headers",
                        "upstream": resolved.route.upstream_authority.as_str(),
                        "upstreamProtocol": bridge.upstream_protocol().label(),
                        "protocolBridge": bridge.label(),
                        "requestKind": request_kind.label(),
                        "upstreamStream": upstream_stream_requested,
                        "responseHeaderTimeoutSeconds": response_header_timeout.as_secs(),
                        "requestId": current_router_request_id(),
                    }),
                );
                if compacting {
                    // 压缩的等待期限与普通请求不同，失败保持压缩专用的结构化
                    // 错误码，客户端据此显示可重试的压缩超时提示。
                    downstream
                        .write_error(
                            504,
                            "compaction_timeout",
                            format!(
                                "远程压缩等待上游 {} 返回响应头超时，原始会话历史未被 Codey 修改，请稍后重试",
                                resolved.route.upstream_authority
                            ),
                            Some(&resolved.route),
                        )
                        .await?;
                } else {
                    downstream
                        .write_text_error(
                            504,
                            "upstream_header_timeout",
                            format!(
                                "Codey 线路「{}」等待上游 {} 返回响应头超时",
                                route_display_name(&resolved.route),
                                resolved.route.upstream_authority
                            ),
                        )
                        .await?;
                }
                return Ok(None);
            }
        };
        Ok(Some(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_received_response_can_authorize_one_shared_retry() {
        for first in [RetryReason::Plugin, RetryReason::ReasoningText] {
            let mut budget = AttemptBudget::default();
            assert!(!budget.can_retry(false));
            budget.begin_send().unwrap();
            assert!(!budget.can_retry(false));
            budget.finish_send(true);
            assert!(!budget.reserve_retry(first, true));
            assert!(budget.reserve_retry(first, false));
            assert_eq!(budget.attempt_index(), 1);
            assert_eq!(budget.retry_reason(), Some(first));
            budget.begin_send().unwrap();
            budget.finish_send(true);
            for next in [RetryReason::Plugin, RetryReason::ReasoningText] {
                assert!(!budget.reserve_retry(next, false));
            }
        }
    }

    #[test]
    fn unknown_send_outcome_never_permits_replay() {
        let mut budget = AttemptBudget::default();
        budget.begin_send().unwrap();
        assert!(budget.begin_send().is_err());
        budget.finish_send(false);
        assert!(!budget.reserve_retry(RetryReason::Plugin, false));
        assert!(!budget.reserve_retry(RetryReason::ReasoningText, false));
    }
}
