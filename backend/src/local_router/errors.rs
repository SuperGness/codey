use super::*;

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
    .and_then(|kind| sanitize_upstream_error_text(kind, route, 128));
    let code = first_string_at(
        value,
        &[
            "/response/error/code",
            "/error/error/code",
            "/error/code",
            "/code",
        ],
    )
    .and_then(|code| sanitize_upstream_error_text(code, route, 128));
    UpstreamErrorSummary {
        message,
        error_type,
        code,
    }
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
    let summary = parsed
        .as_ref()
        .map(|value| upstream_error_summary(value, &resolved.route))
        .unwrap_or_default();
    let detail = upstream_error_detail(&summary);
    if let (Some(probe), Some(detail)) = (probe.as_ref(), detail.as_deref()) {
        probe.mark_upstream_error_summary(detail);
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
        message.clone(),
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
    if downstream.is_websocket() {
        downstream
            .write_error(
                status,
                "upstream_http_error",
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
        message.clone(),
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
    error_summary
}
