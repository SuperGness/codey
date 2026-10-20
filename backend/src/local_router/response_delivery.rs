use super::*;

/// 响应阶段只消费执行结果，不能再次发送上游请求。
pub(super) struct ResponseDelivery<'a> {
    pub(super) resolved: &'a RouteSelection,
    pub(super) bridge: ProtocolBridge,
    pub(super) request_kind: ResponsesRequestKind,
    pub(super) stream_requested: bool,
    pub(super) compacting: bool,
    pub(super) native_compaction: bool,
    pub(super) compact_endpoint_compat: bool,
    pub(super) compaction_phase: GatewayCompactionPhase,
    pub(super) tool_bridge: &'a ResponsesToolBridge,
}

impl ResponseDelivery<'_> {
    pub(super) async fn deliver<D: ResponsesDownstream + ?Sized>(
        &self,
        downstream: &mut D,
        received: ReceivedHttpResponse,
        portable_attempt: Option<&mut PortableAttempt>,
        checkpoint: Option<(PortableHistoryCheckpoint, Arc<Mutex<PortableHistoryCache>>)>,
        xai_fix: Option<XaiResponseFix>,
    ) -> Result<()> {
        let response = match received {
            ReceivedHttpResponse::Rejected { head, body } => {
                let error = UpstreamHttpErrorResponse {
                    status: head.status,
                    upstream_request_id: head.request_id.as_deref(),
                    retry_advice: head.retry_advice,
                    body: &body,
                };
                if self.compacting && !self.native_compaction {
                    if let Some(attempt) = portable_attempt
                        && serde_json::from_slice::<Value>(&body)
                            .ok()
                            .is_some_and(|value| is_context_length_error(&value))
                    {
                        attempt.target_context_rejected();
                    }
                    return write_gateway_compaction_http_error(
                        downstream,
                        error,
                        self.resolved,
                        self.bridge,
                        self.request_kind,
                        self.compaction_phase,
                    )
                    .await;
                }
                observe_siwc_upstream_rejection(&self.resolved.route, head.status, &body);
                return write_upstream_http_error(
                    downstream,
                    error,
                    self.resolved,
                    self.bridge,
                    self.request_kind,
                )
                .await;
            }
            ReceivedHttpResponse::Success(response) => response,
        };
        let result = if self.compacting {
            if self.native_compaction {
                let format = if self.compact_endpoint_compat {
                    CompactionResponseFormat::CompactToResponses
                } else if self.request_kind == ResponsesRequestKind::Create {
                    CompactionResponseFormat::Responses
                } else {
                    CompactionResponseFormat::Compact
                };
                write_validated_compaction(
                    downstream,
                    response,
                    format,
                    self.stream_requested,
                    &self.resolved.route,
                    checkpoint,
                )
                .await
            } else {
                write_gateway_compaction(
                    downstream,
                    response,
                    self.bridge,
                    &self.resolved.upstream_model,
                    self.stream_requested,
                    &self.resolved.route,
                    self.tool_bridge,
                    portable_attempt,
                    self.compaction_phase,
                )
                .await
            }
        } else if self.bridge != ProtocolBridge::NativeResponses {
            write_adapted_upstream_as_responses(
                downstream,
                response,
                self.bridge,
                &self.resolved.upstream_model,
                self.stream_requested,
                &self.resolved.route,
                self.tool_bridge,
            )
            .await
        } else if let Some(fix) = xai_fix {
            XAI_RESPONSE_FIX
                .scope(fix, downstream.proxy_response(response))
                .await
        } else {
            downstream.proxy_response(response).await
        };
        finish_upstream_response_result(downstream, result, self.resolved).await
    }
}

/// HTTP 上游的正文读取和协议转换使用同一个 WebSocket 错误写回契约。
pub(super) async fn finish_upstream_response_result<D: ResponsesDownstream + ?Sized>(
    downstream: &mut D,
    result: Result<()>,
    resolved: &RouteSelection,
) -> Result<()> {
    if let Err(error) = &result
        && let Some(probe) = downstream.request_log_probe()
    {
        use crate::route_request_log::{AttemptOutcome, AttemptPhase};
        if error.is::<DownstreamClosed>() || error.is::<DownstreamResponseInterrupt>() {
            probe.finish_upstream_attempt(
                AttemptOutcome::Cancelled,
                AttemptPhase::DownstreamDelivery,
            );
        } else if is_upstream_timeout_error(error) {
            probe.finish_upstream_attempt(AttemptOutcome::TimedOut, AttemptPhase::ResponseBody);
        }
        // 其余错误由日志终态收尾；已有成功终态时会归为 downstream_delivery。
    }
    if let Err(error) = &result
        && downstream.is_websocket()
        && !error.is::<DownstreamClosed>()
    {
        record_router_failure_nonblocking(
            "local_router_upstream_response_failed",
            "proxy_local_router_request",
            format!("{error:#}"),
            json!({"routeId": resolved.provider_id, "requestId": current_router_request_id()}),
        );
        let detail = sanitize_upstream_error_text(&format!("{error:#}"), &resolved.route, 512)
            .unwrap_or_else(|| "上游响应未能完成".to_string());
        return downstream
            .write_error(
                if is_upstream_timeout_error(error) {
                    504
                } else {
                    502
                },
                if is_upstream_timeout_error(error) {
                    "upstream_timeout"
                } else {
                    "upstream_response_failed"
                },
                format!(
                    "Codey 线路「{}」处理上游 HTTP 响应失败：{detail}",
                    route_display_name(&resolved.route)
                ),
                Some(&resolved.route),
            )
            .await;
    }
    result
}
