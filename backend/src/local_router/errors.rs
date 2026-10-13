use super::*;

pub(crate) const CONTEXT_LENGTH_EXCEEDED: &str = "context_length_exceeded";

// Normalize only documented error codes and narrow provider messages. An HTTP
// 400/413 on its own can mean an invalid image or request, not a full context.
pub(crate) fn is_context_length_error(value: &Value) -> bool {
    let code = first_string_at(
        value,
        &[
            "/response/error/code",
            "/response/error/type",
            "/error/code",
            "/error/type",
            "/code",
        ],
    );
    if code.is_some_and(|code| {
        matches!(
            code,
            "context_length_exceeded" | "context_window_exceeded" | "prompt_too_long"
        )
    }) {
        return true;
    }
    let message = first_string_at(
        value,
        &["/response/error/message", "/error/message", "/message"],
    )
    .unwrap_or_default()
    .to_ascii_lowercase();
    message.starts_with("prompt is too long:")
        || message.starts_with("input is too long for requested model")
        || message.contains("maximum context length")
}

#[derive(Debug)]
pub(crate) struct ContextLengthExceeded;
impl std::fmt::Display for ContextLengthExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("上下文超过目标模型限制，请压缩后重试")
    }
}
impl std::error::Error for ContextLengthExceeded {}

pub(crate) fn check_context_length_error(value: &Value) -> Result<()> {
    if is_context_length_error(value) {
        return Err(ContextLengthExceeded.into());
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct UpstreamStreamFailure(pub(crate) Value);

impl std::fmt::Display for UpstreamStreamFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "上游流返回错误：{}", self.0)
    }
}

impl std::error::Error for UpstreamStreamFailure {}

pub(crate) fn check_upstream_stream_failure(value: &Value) -> Result<()> {
    check_context_length_error(value)?;
    if value.get("error").is_some_and(|error| !error.is_null())
        || value.get("type").and_then(Value::as_str) == Some("error")
    {
        return Err(UpstreamStreamFailure(value.clone()).into());
    }
    Ok(())
}

pub(crate) fn streaming_response_failure(
    error: &anyhow::Error,
    route: &RouteTarget,
    retry_advice: Option<ResponseRetryAdvice>,
) -> ResponsesFailure {
    let (code, message) = streaming_failure_message(error, route);
    let mut failure = ResponsesFailure::new(
        if is_upstream_timeout_error(error) {
            504
        } else {
            502
        },
        code,
        message,
        Some(route),
    );
    if let Some(upstream) = error.downcast_ref::<UpstreamStreamFailure>() {
        let summary = upstream_error_summary(&upstream.0, route);
        if let Some(message) = summary.message {
            failure.event["response"]["error"]["message"] = message.into();
        }
        let metadata = &mut failure.event["response"]["error"]["codey"];
        if let Some(code) = summary.code {
            metadata["upstreamCode"] = code.into();
        }
        if let Some(kind) = summary.error_type {
            metadata["upstreamType"] = kind.into();
        }
        if let Some(status) = upstream
            .0
            .get("status")
            .or_else(|| upstream.0.pointer("/error/status"))
            .and_then(Value::as_u64)
            .filter(|status| (400..600).contains(status))
        {
            failure.status = status as u16;
            metadata["httpStatus"] = status.into();
        }
        if let Some(delay) = upstream
            .0
            .pointer("/error/retry_after_ms")
            .and_then(Value::as_u64)
        {
            failure.event["response"]["error"]["retry_after_ms"] = delay.into();
        }
    }
    failure.retry_advice = retry_advice;
    failure
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UpstreamErrorSummary {
    pub(crate) message: Option<String>,
    pub(crate) error_type: Option<String>,
    pub(crate) code: Option<String>,
}

pub(crate) fn first_string_at<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn sanitize_upstream_error_text(
    value: &str,
    route: &RouteTarget,
    max_chars: usize,
) -> Option<String> {
    let sanitized = redact_upstream_error_text(value, route);
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut chars = collapsed.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.push('…');
    }
    Some(bounded)
}

pub(crate) fn redact_upstream_error_text(value: &str, route: &RouteTarget) -> String {
    let mut sanitized = value.to_string();
    if let Ok(headers) = route.upstream_headers.as_ref() {
        for secret in headers.values().filter_map(|value| value.to_str().ok()) {
            let secret = secret.trim();
            if secret.len() < 4 {
                continue;
            }
            sanitized = sanitized.replace(secret, "***");
            if let Some((scheme, token)) = secret.split_once(' ')
                && scheme.eq_ignore_ascii_case("bearer")
                && token.trim().len() >= 4
            {
                sanitized = sanitized.replace(token.trim(), "***");
            }
        }
    }
    sanitized
}

pub(crate) fn observe_upstream_stream_error(
    probe: Option<&RouteRequestLogProbe>,
    error: &anyhow::Error,
    route: &RouteTarget,
) {
    // 在发送失败终态前记录，避免请求日志先完成后遗漏底层原因。
    if let Some(probe) = probe
        && let Some(summary) = sanitize_upstream_error_text(&format!("{error:#}"), route, 4096)
    {
        probe.mark_upstream_error_summary(&summary);
    }
}

pub(crate) fn upstream_error_summary(value: &Value, route: &RouteTarget) -> UpstreamErrorSummary {
    let message = first_string_at(
        value,
        &[
            "/response/error/message",
            "/error/error/message",
            "/error/message",
            "/message",
            "/detail",
            "/error",
        ],
    )
    .and_then(|message| sanitize_upstream_error_text(message, route, 512));
    let error_type = first_string_at(
        value,
        &["/response/error/type", "/error/error/type", "/error/type"],
    )
    .and_then(|kind| sanitize_upstream_error_text(kind, route, 128))
    .and_then(|kind| safe_upstream_error_identifier(&kind));
    let code = first_string_at(
        value,
        &[
            "/response/error/code",
            "/error/error/code",
            "/error/code",
            "/code",
        ],
    )
    .and_then(|code| sanitize_upstream_error_text(code, route, 128))
    .and_then(|code| safe_upstream_error_identifier(&code));
    UpstreamErrorSummary {
        message,
        error_type,
        code,
    }
}

fn safe_upstream_error_identifier(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
    .then(|| value.to_string())
}

fn upstream_error_log_detail(summary: &UpstreamErrorSummary) -> String {
    // General diagnostic logs retain classification only. Request logs also
    // retain the bounded upstream error body after route credential redaction.
    format!(
        "type={}; code={}",
        summary.error_type.as_deref().unwrap_or("unknown"),
        summary.code.as_deref().unwrap_or("unknown")
    )
}

pub(crate) fn upstream_error_detail(summary: &UpstreamErrorSummary) -> Option<String> {
    let mut detail = summary.message.clone().unwrap_or_default();
    let mut attributes = Vec::new();
    if let Some(error_type) = summary.error_type.as_deref() {
        attributes.push(format!("类型：{error_type}"));
    }
    if let Some(code) = summary.code.as_deref() {
        attributes.push(format!("代码：{code}"));
    }
    if !attributes.is_empty() {
        if detail.is_empty() {
            detail = attributes.join("；");
        } else {
            detail.push_str(&format!("（{}）", attributes.join("；")));
        }
    }
    (!detail.is_empty()).then_some(detail)
}

pub(crate) fn bounded_upstream_request_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars().filter(|character| !character.is_control());
    let bounded = chars.by_ref().take(128).collect::<String>();
    (!bounded.is_empty()).then_some(bounded)
}

pub(crate) fn upstream_request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    [
        "x-request-id",
        "request-id",
        "x-oneapi-request-id",
        "x-amzn-requestid",
        "cf-ray",
    ]
    .iter()
    .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
    .and_then(bounded_upstream_request_id)
}

/// 部分第三方 thinking 模式要求把上一轮的 reasoning 明文原样回传，请求缺少
/// 明文字段时上游拒绝整条请求。DeepSeek 的说法是 `reasoning_content_missing`
/// 或 “reasoning content”，另一些网关写 `reasoning_text`。识别后由调用方补齐
/// 占位明文再重发一次。
pub(crate) fn requires_reasoning_text_fallback(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    text.contains("reasoning_text")
        || text.contains("reasoning_content")
        || text.contains("reasoning content")
}

/// 上游非 2xx 错误正文读取超时：与压缩路径一致返回结构化 504，而不是让
/// 下游连接在没有响应的情况下断开。
pub(crate) async fn write_upstream_error_body_timeout<D>(
    downstream: &mut D,
    resolved: &RouteSelection,
    compacting: bool,
    error: &anyhow::Error,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let detail = sanitize_upstream_error_text(&format!("{error:#}"), &resolved.route, 256)
        .unwrap_or_else(|| "上游错误响应正文未在期限内读完".to_string());
    let (code, message) = if compacting {
        (
            "compaction_timeout",
            format!(
                "远程压缩读取上游错误响应超时（{detail}），原始会话历史未被 Codey 修改，请稍后重试"
            ),
        )
    } else {
        (
            "upstream_timeout",
            format!(
                "Codey 线路「{}」读取上游错误响应超时：{detail}",
                route_display_name(&resolved.route)
            ),
        )
    };
    record_router_failure_nonblocking(
        "local_router_upstream_error_body_timeout",
        "proxy_local_router_request",
        format!("读取上游错误响应正文超时；{detail}"),
        serde_json::json!({
            "routeId": resolved.provider_id.as_str(),
            "routeName": resolved.route.route_name.as_str(),
            "requestedModel": resolved.requested_model.as_str(),
            "model": resolved.upstream_model.as_str(),
            "upstream": resolved.route.upstream_authority.as_str(),
            "requestId": current_router_request_id(),
        }),
    );
    downstream
        .write_error(504, code, message, Some(&resolved.route))
        .await
}

pub(crate) struct UpstreamHttpErrorResponse<'a> {
    pub(crate) status: u16,
    pub(crate) upstream_request_id: Option<&'a str>,
    pub(crate) retry_advice: Option<ResponseRetryAdvice>,
    pub(crate) body: &'a [u8],
}

/// SIWC 直连上游把凭据打回时标记账号的 SIWC 槽位失效：卡片出现「重新授权」，
/// 下一次账号同步会把该线路切回 Codex backend-api 平面。限流、额度超限和
/// 暂时性不可用不标失效——凭据本身仍然有效。
pub(crate) fn observe_siwc_upstream_rejection(route: &RouteTarget, status: u16, body: &[u8]) {
    let Some(auth) = route.official_auth.as_ref() else {
        return;
    };
    if auth.siwc_path.is_none() {
        return;
    }
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| {
                    value
                        .pointer("/response/error/code")
                        .and_then(Value::as_str)
                })
                .map(str::to_string)
        });
    let reason = match code.as_deref() {
        Some("subscription_sharing_invalid_user") => "官方直连授权无法验证账号身份，请重新授权",
        // 资格性拒绝（免费账号/订阅掉线/组织账号）：凭据本身没坏，但直连对
        // 这个账号不可用，标记后对话回落 Codex 平面，卡片如实提示原因。
        Some("subscription_sharing_user_not_eligible") => {
            "该账号暂不具备订阅共享资格（需生效中的付费订阅），对话已恢复默认通道"
        }
        Some("chatpass_v2_scope_not_authorized")
        | Some("chatpass_v2_invalid_authorization_context") => {
            "官方直连的授权范围已失效，请重新授权"
        }
        Some(_) => {
            if status == 401 {
                "官方直连凭据被官方拒绝，请重新授权"
            } else {
                return;
            }
        }
        // direct-admission 的 401 可能没有结构化 code；凭据被明确拒绝同样按
        // 失效处理，400/403/429/503 一律不动。
        None if status == 401 => "官方直连凭据被官方拒绝，请重新授权",
        None => return,
    };
    let store = crate::official_accounts::OfficialAccountStore::for_config_path(
        &crate::config::default_config_path(),
    );
    let account_id = auth.account_id.clone();
    let reason = reason.to_string();
    tokio::task::spawn_blocking(
        move || match store.mark_siwc_invalid(&account_id, &reason) {
            Ok(true) => crate::official_accounts::notify_siwc_invalidated(&account_id),
            Ok(false) => {}
            Err(error) => record_router_failure_nonblocking(
                "siwc_mark_invalid_failed",
                "observe_siwc_upstream_rejection",
                error.to_string(),
                serde_json::json!({"accountId": account_id}),
            ),
        },
    );
}

pub(crate) async fn write_upstream_http_error<D>(
    downstream: &mut D,
    response: UpstreamHttpErrorResponse<'_>,
    resolved: &RouteSelection,
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let UpstreamHttpErrorResponse {
        status,
        upstream_request_id,
        retry_advice,
        body,
    } = response;
    let upstream_request_id = upstream_request_id.map(str::to_string);
    let probe = downstream.request_log_probe().cloned();
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let context_exceeded = parsed.as_ref().is_some_and(is_context_length_error);
    let summary = parsed
        .as_ref()
        .map(|value| upstream_error_summary(value, &resolved.route))
        .unwrap_or_default();
    let detail = upstream_error_detail(&summary);
    if let Some(probe) = probe.as_ref() {
        let mut original =
            redact_upstream_error_text(&String::from_utf8_lossy(body), &resolved.route);
        if body.len() == MAX_UPSTREAM_ERROR_BYTES {
            original.push_str("\n[上游错误正文达到读取上限，内容可能不完整]");
        }
        probe.mark_upstream_error_summary(&original);
    }
    let mut message = format!(
        "Codey 线路「{}」请求模型 {} 时，上游返回 HTTP {status}",
        route_display_name(&resolved.route),
        resolved.upstream_model
    );
    if let Some(detail) = detail.as_deref() {
        message.push_str(&format!("：{detail}"));
    }
    if let Some(request_id) = upstream_request_id.as_deref() {
        message.push_str(&format!("（上游请求 ID：{request_id}）"));
    }
    record_router_failure_nonblocking(
        "local_router_upstream_http_error",
        "proxy_local_router_response",
        format!(
            "上游返回 HTTP {status}；{}",
            upstream_error_log_detail(&summary)
        ),
        serde_json::json!({
            "routeId": resolved.provider_id.as_str(),
            "routeName": resolved.route.route_name.as_str(),
            "requestedModel": resolved.requested_model.as_str(),
            "model": resolved.upstream_model.as_str(),
            "status": status,
            "upstreamRequestId": upstream_request_id,
            "upstreamErrorType": summary.error_type,
            "upstreamErrorCode": summary.code,
            "upstream": resolved.route.upstream_authority.as_str(),
            "upstreamProtocol": bridge.upstream_protocol().label(),
            "protocolBridge": bridge.label(),
            "requestKind": request_kind.label(),
            "requestId": current_router_request_id(),
        }),
    );
    let code = if context_exceeded {
        CONTEXT_LENGTH_EXCEEDED
    } else {
        "upstream_http_error"
    };
    let mut failure = ResponsesFailure::new(status, code, message, Some(&resolved.route));
    let metadata = &mut failure.event["response"]["error"]["codey"];
    if let Some(code) = summary.code {
        metadata["upstreamCode"] = code.into();
    }
    if let Some(kind) = summary.error_type {
        metadata["upstreamType"] = kind.into();
    }
    if let Some(request_id) = upstream_request_id {
        metadata["upstreamRequestId"] = request_id.into();
    }
    failure.retry_advice = retry_advice;
    downstream.write_response_failure(&failure).await
}

pub(crate) fn annotate_upstream_websocket_failure(
    event: &mut Value,
    route: &RouteTarget,
    model: &str,
    upstream_url: &str,
) -> Option<String> {
    let summary = upstream_error_summary(event, route);
    let error_summary = upstream_error_detail(&summary);
    let detail = error_summary.as_deref().unwrap_or("上游未提供具体错误信息");
    let message = format!(
        "Codey 线路「{}」请求模型 {model} 时，Responses WebSocket 上游返回错误：{detail}",
        route_display_name(route)
    );
    let upstream_request_id = first_string_at(
        event,
        &[
            "/response/error/request_id",
            "/error/request_id",
            "/request_id",
        ],
    )
    .and_then(bounded_upstream_request_id);
    record_router_failure_nonblocking(
        "local_router_upstream_websocket_error",
        "proxy_responses_websocket_event",
        format!(
            "Responses WebSocket 上游返回错误；{}",
            upstream_error_log_detail(&summary)
        ),
        serde_json::json!({
            "routeId": route.provider_id.as_str(),
            "routeName": route.route_name.as_str(),
            "model": model,
            "upstream": route.upstream_authority.as_str(),
            "upstreamEndpoint": upstream_url,
            "upstreamRequestId": upstream_request_id,
            "upstreamErrorType": summary.error_type,
            "upstreamErrorCode": summary.code,
            "requestId": current_router_request_id(),
        }),
    );

    let mut updated = false;
    if let Some(error) = event
        .get_mut("response")
        .and_then(Value::as_object_mut)
        .and_then(|response| response.get_mut("error"))
        .and_then(Value::as_object_mut)
    {
        error.insert("message".to_string(), Value::String(message.clone()));
        updated = true;
    }
    if !updated && let Some(error) = event.get_mut("error").and_then(Value::as_object_mut) {
        error.insert("message".to_string(), Value::String(message.clone()));
        updated = true;
    }
    if !updated && let Some(object) = event.as_object_mut() {
        object.insert("message".to_string(), Value::String(message));
    }
    Some(upstream_error_log_detail(&summary))
}
