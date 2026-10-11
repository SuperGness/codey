//! 异步供应商传输，复用 ABI v1 的短调用与有界分块。
//! 凭据只随开始事件传递；正文不经过管理接口，也不写入临时文件。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CAPABILITY: &str = "provider.transport.v1";
pub const ACCOUNT_CAPABILITY: &str = "provider.account.v1";
pub const START: &str = "provider.request.start";
pub const WRITE: &str = "provider.request.write";
pub const READ: &str = "provider.request.read";
pub const CANCEL: &str = "provider.request.cancel";
pub const STOP: &str = "provider.request.stop";
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// 校验传输响应头，统一名称大小写，并移除宿主不会转发的字段。
/// 重复名称、无效值或超出协议限制时返回固定错误码。
pub fn filter_response_headers(
    headers: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if headers.len() > 32 {
        return Err("plugin_invalid_headers".into());
    }
    let mut filtered = BTreeMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if ![
            "content-type",
            "cache-control",
            "retry-after",
            "x-request-id",
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        if value.len() > 8192
            || !value
                .bytes()
                .all(|byte| byte == b'\t' || (byte >= 32 && byte != 127))
            || filtered.insert(name, value).is_some()
        {
            return Err("plugin_invalid_headers".into());
        }
    }
    Ok(filtered)
}

pub fn is_reserved(method: &str) -> bool {
    method.starts_with("provider.request.")
}

/// 只允许固定错误码跨越宿主边界，未知内容不能携带插件或上游的原始错误。
pub fn public_error_code(code: &str) -> &'static str {
    const CODES: &[&str] = &[
        "invalid_request",
        "invalid_credentials",
        "account_binding_changed",
        "duplicate_request",
        "request_limit",
        "request_not_found",
        "body_too_large",
        "chunk_too_large",
        "invalid_chunk",
        "invalid_body_size",
        "incomplete_body",
        "plugin_stopped",
        "request_timeout",
        "request_cancelled",
        "worker_stopped",
        "upstream_timeout",
        "upstream_connect_failed",
        "upstream_read_failed",
        "upstream_stream_interrupted",
        "upstream_stream_incomplete",
        "upstream_stream_error",
        "upstream_body_too_large",
        "upstream_unauthorized",
        "upstream_forbidden",
        "upstream_rate_limited",
        "upstream_quota_exceeded",
        "upstream_model_unavailable",
        "upstream_unavailable",
        "upstream_rejected_request",
        "invalid_upstream_json",
        "invalid_image_response",
        "image_request_failed",
        "image_upload_failed",
        "history_read_failed",
        "history_write_failed",
        "history_corrupt",
        "history_lock_failed",
        "tool_call_too_large",
        "sse_event_too_large",
        "invalid_sse_encoding",
        "invalid_sse_event",
        "incomplete_terminal_response",
        "inconsistent_terminal_event",
        "upstream_response_unfinished",
    ];
    CODES
        .iter()
        .copied()
        .find(|known| *known == code)
        .unwrap_or("plugin_upstream_failed")
}

pub fn error_http_status(code: &str) -> u16 {
    match code {
        "plugin_disabled"
        | "plugin_busy"
        | "plugin_stopped"
        | "worker_stopped"
        | "upstream_unavailable" => 503,
        "request_limit" | "upstream_rate_limited" | "upstream_quota_exceeded" => 429,
        "request_timeout" | "upstream_timeout" | "plugin_callback_timeout" => 504,
        "body_too_large" | "plugin_request_too_large" | "chunk_too_large" => 413,
        "invalid_credentials" | "plugin_auth_missing" | "upstream_unauthorized" => 401,
        "upstream_forbidden" => 403,
        "account_binding_changed" => 409,
        "invalid_request"
        | "invalid_chunk"
        | "invalid_body_size"
        | "incomplete_body"
        | "plugin_operation_unsupported" => 400,
        _ => 502,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_header_helpers_enforce_host_boundaries() {
        let headers = BTreeMap::from([
            ("Content-Type".into(), "text/event-stream".into()),
            ("Set-Cookie".into(), "private=value".into()),
        ]);
        let value = serde_json::to_value(Frame::headers(200, headers).unwrap()).unwrap();
        assert_eq!(value["headers"]["content-type"], "text/event-stream");
        assert_eq!(value["headers"].as_object().unwrap().len(), 1);
        for headers in [
            BTreeMap::from([
                ("Content-Type".into(), "text/event-stream".into()),
                ("content-type".into(), "application/json".into()),
            ]),
            BTreeMap::from([("x-request-id".into(), "x".repeat(8193))]),
            BTreeMap::from([("x-request-id".into(), "bad\r\nvalue".into())]),
            (0..33)
                .map(|index| (format!("x-{index}"), String::new()))
                .collect(),
        ] {
            assert_eq!(
                Frame::headers(200, headers).unwrap_err(),
                "plugin_invalid_headers"
            );
        }
        assert!(Frame::headers(199, BTreeMap::new()).is_err());
        assert!(Frame::headers(600, BTreeMap::new()).is_err());
        assert!(
            Frame::headers(
                599,
                BTreeMap::from([("x-request-id".into(), "x".repeat(8192))])
            )
            .is_ok()
        );
    }

    #[test]
    fn error_frame_helper_redacts_unknown_errors() {
        for (input, expected) in [
            ("request_timeout", "request_timeout"),
            ("Bearer secret", "plugin_upstream_failed"),
        ] {
            let value = serde_json::to_value(Frame::error(input)).unwrap();
            assert_eq!(
                value,
                serde_json::json!({"type": "error", "code": expected})
            );
        }
    }

    #[test]
    fn transport_error_codes_never_expose_unknown_text() {
        assert_eq!(public_error_code("upstream_timeout"), "upstream_timeout");
        assert_eq!(
            public_error_code("Bearer private-token"),
            "plugin_upstream_failed"
        );
        assert_eq!(error_http_status("request_limit"), 429);
        assert_eq!(error_http_status("upstream_timeout"), 504);
        assert_eq!(error_http_status("body_too_large"), 413);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransportOptions {
    /// 必填，按邮箱唯一匹配宿主已保存的账号；不会回退默认账号。
    pub account_email: String,
    #[serde(default)]
    pub models: BTreeMap<String, ModelCapabilities>,
    #[serde(default)]
    pub image_generation: bool,
    #[serde(default)]
    pub image_edit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCapabilities {
    pub context_window: u64,
    pub auto_compact_token_limit: u64,
}

// 不实现 Debug，防止凭据被诊断输出意外记录。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials {
    pub access_token: String,
    pub upstream_account_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRequest {
    pub request_id: String,
    pub account_email: String,
    pub operation: Operation,
    pub body_bytes: usize,
    pub credentials: Credentials,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Responses,
    ImageGeneration,
    ImageEdit,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteRequest {
    pub request_id: String,
    /// Base64 编码，每块解码后最多 CHUNK_BYTES。
    pub data: String,
    pub finish: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestId {
    pub request_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    Pending,
    Headers {
        status: u16,
        headers: BTreeMap<String, String>,
    },
    Data {
        data: String,
    },
    End,
    /// 仅提供固定错误码，不回传包含正文、URL 参数或凭据的错误文本。
    Error {
        code: String,
    },
}

impl Frame {
    /// 构造经过校验的响应头帧；正文仍由插件按传输协议分块提供。
    pub fn headers(status: u16, headers: BTreeMap<String, String>) -> Result<Self, String> {
        if !(200..=599).contains(&status) {
            return Err("plugin_invalid_frame_order".into());
        }
        Ok(Self::Headers {
            status,
            headers: filter_response_headers(headers)?,
        })
    }

    /// 构造公开错误帧，未知错误文本统一替换，避免泄漏凭据或正文。
    pub fn error(code: &str) -> Self {
        Self::Error {
            code: public_error_code(code).into(),
        }
    }
}
