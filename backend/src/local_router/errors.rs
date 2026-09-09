use super::*;

pub(crate) const CONTEXT_LENGTH_EXCEEDED: &str = "context_length_exceeded";

// Normalize only documented error codes and narrow provider messages. An HTTP
// 400/413 on its own can mean an invalid image or request, not a full context.
pub(crate) fn is_context_length_error(value: &Value) -> bool {
    let code = first_string_at(
        value,
        &[
            "/response/error/code",
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
    ["x-request-id", "request-id", "x-amzn-requestid", "cf-ray"]
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .and_then(bounded_upstream_request_id)
}

pub(crate) async fn write_upstream_http_error<D>(
    downstream: &mut D,
    response: reqwest::Response,
    resolved: &RouteSelection,
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let status = response.status().as_u16();
    let upstream_request_id = upstream_request_id_from_headers(response.headers());
    let probe = downstream.request_log_probe().cloned();
    let body = await_upstream(
        downstream,
        read_bounded_upstream_error_body(response, probe.as_ref()),
    )
    .await??;
    let parsed = serde_json::from_slice::<Value>(&body).ok();
    let context_exceeded = parsed.as_ref().is_some_and(is_context_length_error);
    let summary = parsed
        .as_ref()
        .map(|value| upstream_error_summary(value, &resolved.route))
        .unwrap_or_default();
    let detail = upstream_error_detail(&summary);
    if let Some(probe) = probe.as_ref() {
        let mut original =
            redact_upstream_error_text(&String::from_utf8_lossy(&body), &resolved.route);
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
    if context_exceeded || downstream.is_websocket() {
        downstream
            .write_error(
                status,
                if context_exceeded {
                    CONTEXT_LENGTH_EXCEEDED
                } else {
                    "upstream_http_error"
                },
                message,
                Some(&resolved.route),
            )
            .await
    } else {
        downstream
            .write_text_error(status, "upstream_http_error", message)
            .await
    }
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
