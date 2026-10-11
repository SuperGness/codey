use super::*;

const MAX_RETRY_DELAY_MS: u64 = 5 * 60 * 1000;

#[derive(Clone, Debug)]
pub(crate) struct ResponseRetryAdvice {
    pub(crate) header: String,
    deadline: Instant,
}

impl ResponseRetryAdvice {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let value = headers.get("retry-after")?.to_str().ok()?.trim();
        let delay = if let Ok(seconds) = value.parse::<u64>() {
            Duration::from_secs(seconds)
        } else {
            let deadline = chrono::DateTime::parse_from_rfc2822(value).ok()?;
            (deadline.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
        };
        Some(Self {
            header: value.to_owned(),
            deadline: Instant::now() + delay.min(Duration::from_millis(MAX_RETRY_DELAY_MS)),
        })
    }

    fn remaining_ms(&self) -> u64 {
        self.deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

pub(crate) struct ResponsesFailure {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) event: Value,
    pub(crate) retry_advice: Option<ResponseRetryAdvice>,
}

impl ResponsesFailure {
    pub(crate) fn new(
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Self {
        let mut metadata = json!({"httpStatus": status, "errorCode": code});
        if let Some(route) = route {
            metadata["routeId"] = route.provider_id.clone().into();
            metadata["routeName"] = route.route_name.clone().into();
        }
        if let Some(request_id) = current_router_request_id() {
            metadata["requestId"] = request_id.into();
        }
        Self {
            status,
            code: code.into(),
            event: json!({
                "type": "response.failed",
                "response": {
                    "id": format!("resp_codey_{}", Uuid::new_v4()),
                    "object": "response",
                    "created_at": current_unix_timestamp(),
                    "status": "failed",
                    "output": [],
                    "error": {
                        "type": "codey_route_error",
                        "code": code,
                        "message": message,
                        "codey": metadata,
                    },
                    "incomplete_details": Value::Null,
                }
            }),
            retry_advice: None,
        }
    }

    pub(crate) fn message(&self) -> &str {
        self.event
            .pointer("/response/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub(crate) fn normalized_event(&self) -> Value {
        let mut event = self.event.clone();
        normalize_response_failure(&mut event, self.retry_advice.as_ref());
        event
    }

    pub(crate) fn from_json(status: u16, value: &Value) -> Self {
        let message = first_string_at(value, &["/error/message", "/response/error/message"])
            .unwrap_or("Codey 本地路由返回错误")
            .to_owned();
        let mut failure = Self::new(status, "upstream_http_error", message, None);
        if let Some(error) = value
            .get("error")
            .or_else(|| value.pointer("/response/error"))
            .filter(|error| error.is_object())
        {
            failure.event["response"]["error"] = error.clone();
            if !failure.event["response"]["error"]["codey"].is_object() {
                failure.event["response"]["error"]["codey"] = json!({});
            }
            failure.event["response"]["error"]["codey"]["httpStatus"] = status.into();
        }
        failure
    }
}

pub(crate) fn normalized_response_event(event: &Value) -> Cow<'_, Value> {
    if matches!(
        event.get("type").and_then(Value::as_str),
        Some("error" | "response.failed")
    ) {
        let mut normalized = event.clone();
        normalize_response_failure(&mut normalized, None);
        Cow::Owned(normalized)
    } else {
        Cow::Borrowed(event)
    }
}

pub(crate) fn normalize_response_failure(
    event: &mut Value,
    retry_advice: Option<&ResponseRetryAdvice>,
) -> bool {
    let kind = event.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("error" | "response.failed")) {
        return false;
    }
    // Codex uses this bare WebSocket error to discard its response chain and
    // resend the full request. Wrapping it as response.failed (or changing its
    // code to invalid_prompt) turns a recoverable continuation into a fatal error.
    if kind == Some("error")
        && event.pointer("/error/code").and_then(Value::as_str)
            == Some("previous_response_not_found")
    {
        observe_lifecycle_response_failure(event);
        return false;
    }
    let original_event = event.clone();
    let bare = kind == Some("error");
    let context_exceeded = is_context_length_error(event);
    let outer_status = event
        .get("status")
        .or_else(|| event.get("status_code"))
        .cloned();
    if bare {
        let error = event.get("error").cloned().unwrap_or_else(|| event.clone());
        let response_id = event
            .get("response_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("resp_codey_{}", Uuid::new_v4()));
        event["type"] = "response.failed".into();
        event.as_object_mut().unwrap().remove("error");
        event["response"] = json!({
                "id": response_id,
                "object": "response",
                "created_at": current_unix_timestamp(),
                "status": "failed",
                "output": [],
                "error": error,
                "incomplete_details": Value::Null,
        });
    }
    if !event.get("response").is_some_and(Value::is_object) {
        event["response"] = json!({"status":"failed","output":[]});
    }
    if !event["response"]["error"].is_object() {
        let message = event["response"]["error"]
            .as_str()
            .unwrap_or("上游响应失败")
            .to_owned();
        event["response"]["error"] = json!({"message":message});
    }
    let error = event["response"]["error"].as_object_mut().unwrap();
    if !error.get("message").is_some_and(Value::is_string) {
        error.insert("message".into(), "上游响应失败".into());
    }
    let current_code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut metadata = error
        .get("codey")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let original_code = metadata
        .get("originalCode")
        .and_then(Value::as_str)
        .unwrap_or(&current_code)
        .to_owned();
    if let Some(status) = outer_status.filter(Value::is_u64) {
        metadata.entry("httpStatus").or_insert(status);
    }
    let source_code = metadata
        .get("upstreamCode")
        .and_then(Value::as_str)
        .unwrap_or(&original_code);
    let source_type = metadata
        .get("upstreamType")
        .and_then(Value::as_str)
        .unwrap_or(&error_type);
    let status = metadata
        .get("httpStatus")
        .and_then(Value::as_u64)
        .or_else(|| error.get("status").and_then(Value::as_u64));
    let quota = [source_code, source_type].iter().any(|code| {
        matches!(
            *code,
            "insufficient_quota"
                | "quota_exceeded"
                | "usage_limit_reached"
                | "billing_hard_limit_reached"
                | "credit_balance_too_low"
        )
    });
    let invalid = [source_code, source_type].iter().any(|code| {
        matches!(
            *code,
            "invalid_prompt"
                | "invalid_request_error"
                | "invalid_request"
                | "authentication_error"
                | "authentication_failed"
                | "invalid_api_key"
                | "permission_error"
                | "permission_denied"
                | "not_found_error"
                | "model_not_found"
                | "request_too_large"
        )
    });
    let rate_limited = status == Some(429)
        || [source_code, source_type].iter().any(|code| {
            matches!(
                *code,
                "rate_limit_exceeded" | "rate_limit_error" | "too_many_requests"
            )
        });
    let permanent_status = status.is_some_and(|status| {
        ((400..500).contains(&status) && !matches!(status, 408 | 429))
            || matches!(status, 501 | 505)
    }) && original_code != "upstream_unreachable";
    let retry_ms = retry_advice
        .map(ResponseRetryAdvice::remaining_ms)
        .or_else(|| error.get("retry_after_ms").and_then(Value::as_u64))
        .or_else(|| metadata.get("retryAfterMs").and_then(Value::as_u64))
        .map(|delay| delay.min(MAX_RETRY_DELAY_MS));
    let code = if context_exceeded {
        CONTEXT_LENGTH_EXCEEDED
    } else if quota {
        "insufficient_quota"
    } else if permanent_status {
        "invalid_prompt"
    } else if rate_limited {
        "rate_limit_exceeded"
    } else if invalid {
        "invalid_prompt"
    } else if retry_ms.is_some() {
        "rate_limit_exceeded"
    } else if original_code.is_empty() {
        "server_error"
    } else {
        &original_code
    };
    if code == "rate_limit_exceeded"
        && let Some(delay) = retry_ms
    {
        let message = metadata
            .get("originalMessage")
            .or_else(|| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        error.insert(
            "message".into(),
            format!(
                "Please try again in {}.{:03}s. {message}",
                delay / 1000,
                delay % 1000
            )
            .into(),
        );
        metadata.insert("originalMessage".into(), message.into());
        metadata.insert("retryAfterMs".into(), delay.into());
    }
    error.insert("code".into(), code.into());
    metadata.insert("originalCode".into(), original_code.into());
    error.insert("codey".into(), metadata.into());
    observe_lifecycle_response_failure(event);
    *event != original_event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_provider_codes_choose_the_native_retry_contract() {
        for (status, source_code, source_type, expected) in [
            (400, "", "", "invalid_prompt"),
            (401, "", "authentication_error", "invalid_prompt"),
            (403, "", "permission_error", "invalid_prompt"),
            (404, "model_not_found", "", "invalid_prompt"),
            (408, "", "", "upstream_http_error"),
            (413, "", "", "invalid_prompt"),
            (429, "", "invalid_request_error", "rate_limit_exceeded"),
            (
                429,
                "insufficient_quota",
                "invalid_request_error",
                "insufficient_quota",
            ),
            (429, "usage_limit_reached", "", "insufficient_quota"),
            (500, "", "", "upstream_http_error"),
            (501, "", "", "invalid_prompt"),
            (503, "", "overloaded_error", "upstream_http_error"),
            (504, "", "", "upstream_http_error"),
        ] {
            let mut failure =
                ResponsesFailure::new(status, "upstream_http_error", "detail".into(), None);
            failure.event["response"]["error"]["codey"]["upstreamCode"] = source_code.into();
            failure.event["response"]["error"]["codey"]["upstreamType"] = source_type.into();
            let event = failure.normalized_event();
            assert_eq!(
                event["response"]["error"]["code"], expected,
                "{status}: {source_code}/{source_type}"
            );
            assert_eq!(event["response"]["error"]["codey"]["httpStatus"], status);
            assert_eq!(
                event["response"]["error"]["codey"]["originalCode"],
                "upstream_http_error"
            );
        }
        let failure =
            ResponsesFailure::new(424, "upstream_unreachable", "connect failed".into(), None);
        assert_eq!(
            failure.normalized_event()["response"]["error"]["code"],
            "upstream_unreachable"
        );
    }

    #[test]
    fn previous_response_not_found_preserves_the_client_recovery_signal() {
        for status in [None, Some(400), Some(404)] {
            let mut event = json!({
                "type":"error", "sequence_number":7, "stream_id":"stream-1",
                "error":{"type":"invalid_request_error", "code":"previous_response_not_found",
                    "message":"Previous response was not found. Retrying the full request."}
            });
            if let Some(status) = status {
                event["status"] = status.into();
            }
            let original = event.clone();
            assert!(!normalize_response_failure(&mut event, None));
            assert_eq!(event, original);
            assert_eq!(normalized_response_event(&event).as_ref(), &original);
        }
    }

    #[test]
    fn bare_errors_preserve_status_stream_and_sequence_fields() {
        let mut event = json!({"type":"error","status":429,"sequence_number":4,"stream_id":"stream-1","response_id":"resp-1","error":{"message":"limited"}});
        assert!(normalize_response_failure(&mut event, None));
        assert_eq!(event["type"], "response.failed");
        assert_eq!(event["response"]["id"], "resp-1");
        assert_eq!(event["sequence_number"], 4);
        assert_eq!(event["stream_id"], "stream-1");
        assert_eq!(event["response"]["error"]["code"], "rate_limit_exceeded");
        assert_eq!(event["response"]["error"]["codey"]["httpStatus"], 429);
        assert!(!normalize_response_failure(&mut event, None));
    }

    #[test]
    fn context_and_malformed_failures_have_usable_terminal_errors() {
        for input in [
            json!({"type":"error","error":{"code":"context_length_exceeded","message":"full"}}),
            json!({"type":"response.failed","response":{"error":{"type":"prompt_too_long"}}}),
        ] {
            let mut event = input;
            normalize_response_failure(&mut event, None);
            assert_eq!(event["response"]["error"]["code"], CONTEXT_LENGTH_EXCEEDED);
            assert!(event["response"]["error"]["message"].is_string());
        }
        for input in [
            json!({"type":"error","error":"bad gateway"}),
            json!({"type":"response.failed"}),
        ] {
            let mut event = input;
            normalize_response_failure(&mut event, None);
            assert_eq!(event["response"]["error"]["code"], "server_error");
            assert!(event["response"]["error"]["message"].is_string());
        }
    }

    #[test]
    fn upstream_metadata_does_not_bypass_classification() {
        let mut event = json!({"type":"response.failed","response":{"error":{"code":"invalid_request_error","codey":{"originalCode":"invalid_request_error"}}}});
        assert!(normalize_response_failure(&mut event, None));
        assert_eq!(event["response"]["error"]["code"], "invalid_prompt");
        assert!(!normalize_response_failure(&mut event, None));
    }

    #[test]
    fn retry_advice_is_bounded_and_preserves_the_original_message() {
        let mut event = json!({"type":"error","error":{"code":"server_error","message":"busy","retry_after_ms":u64::MAX}});
        normalize_response_failure(&mut event, None);
        let error = &event["response"]["error"];
        assert_eq!(error["code"], "rate_limit_exceeded");
        assert_eq!(error["codey"]["retryAfterMs"], MAX_RETRY_DELAY_MS);
        assert_eq!(error["message"], "Please try again in 300.000s. busy");
        assert!(!normalize_response_failure(&mut event, None));
    }

    #[test]
    fn retry_after_headers_handle_seconds_dates_and_invalid_values() {
        let advice = |header: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_str(header).unwrap());
            ResponseRetryAdvice::from_headers(&headers)
        };
        assert!((1900..=2000).contains(&advice("2").unwrap().remaining_ms()));
        assert_eq!(advice("0").unwrap().remaining_ms(), 0);
        let future = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
        assert!((28000..=30000).contains(&advice(&future).unwrap().remaining_ms()));
        assert_eq!(
            advice("Wed, 21 Oct 2015 07:28:00 GMT")
                .unwrap()
                .remaining_ms(),
            0
        );
        assert!(advice("later").is_none());
        assert!(advice("-1").is_none());
        assert!(advice("1.5").is_none());
        assert!(advice("18446744073709551615").unwrap().remaining_ms() <= MAX_RETRY_DELAY_MS);
    }

    #[test]
    fn retry_after_subtracts_body_read_time_and_does_not_retry_permanent_errors() {
        let advice = ResponseRetryAdvice {
            header: "1".into(),
            deadline: Instant::now() - Duration::from_secs(1),
        };
        let mut temporary = ResponsesFailure::new(503, "upstream_http_error", "busy".into(), None);
        temporary.retry_advice = Some(advice.clone());
        assert_eq!(
            temporary.normalized_event()["response"]["error"]["codey"]["retryAfterMs"],
            0
        );
        let mut permanent = ResponsesFailure::new(401, "upstream_http_error", "auth".into(), None);
        permanent.retry_advice = Some(advice);
        let event = permanent.normalized_event();
        assert_eq!(event["response"]["error"]["code"], "invalid_prompt");
        assert_eq!(event["response"]["error"]["message"], "auth");
    }

    #[test]
    fn successful_events_are_borrowed_and_unchanged() {
        let event = json!({"type":"response.output_text.delta","delta":"hello"});
        assert!(matches!(
            normalized_response_event(&event),
            Cow::Borrowed(_)
        ));
    }
}
