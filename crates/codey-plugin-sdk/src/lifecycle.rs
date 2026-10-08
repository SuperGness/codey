//! ABI v1 上的可选请求生命周期协议。宿主控制等待、重试和最终响应提交。
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const CAPABILITY: &str = "request.lifecycle.v1";
pub const AUTH_CAPABILITY: &str = "request.lifecycle.auth";
pub const API_KEY_CAPABILITY: &str = "request.lifecycle.api_key";
pub const TURN_STATE_CAPABILITY: &str = "request.lifecycle.turn_state";

/// 只由宿主在请求生命周期中调用。管理接口必须拒绝这些方法名。
pub const METHOD_BEFORE_SEND: &str = "request.beforeSend";
pub const METHOD_AFTER_HEADERS: &str = "request.afterHeaders";
pub const METHOD_RESUME: &str = "request.resume";
pub const METHOD_COMPLETED: &str = "request.completed";
pub const METHOD_FAILED: &str = "request.failed";
pub const METHOD_CANCELLED: &str = "request.cancelled";
pub const HOST_METHODS: &[&str] = &[
    METHOD_BEFORE_SEND,
    METHOD_AFTER_HEADERS,
    METHOD_RESUME,
    METHOD_COMPLETED,
    METHOD_FAILED,
    METHOD_CANCELLED,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    BeforeSend,
    AfterHeaders,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderPatch {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

/// credentials 仅向显式声明 AUTH_CAPABILITY 的可信插件提供。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEvent {
    pub metadata: Value,
    pub request_id: Value,
    pub stage: Stage,
    pub attempt: u32,
    pub headers: BTreeMap<String, String>,
    pub response: Option<Response>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// completed 表示 HTTP 响应传输完成，不表示模型或响应正文中的业务任务成功。
/// 终态通知仅用于清理，返回值不影响已确定的请求结果。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEvent {
    pub metadata: Value,
    pub request_id: Value,
    pub stage: Stage,
    pub attempt: u32,
    #[serde(default)]
    pub token: Option<String>,
    pub status: Option<u16>,
    pub code: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum Action {
    Continue {
        #[serde(default)]
        headers: Vec<HeaderPatch>,
        /// Sensitive; only an authorized initial beforeSend may select a key.
        #[serde(default, rename = "apiKey", skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
    },
    BorrowTurnState {
        #[serde(rename = "targetAccountEmail")]
        target_account_email: String,
        #[serde(rename = "sourceAccountEmail")]
        source_account_email: String,
        model: String,
        #[serde(rename = "timeoutMs")]
        timeout_ms: u64,
    },
    Wait {
        token: String,
        #[serde(
            default,
            rename = "pollAfterMs",
            skip_serializing_if = "Option::is_none"
        )]
        poll_after_ms: Option<u64>,
    },
    Retry {
        #[serde(default)]
        headers: Vec<HeaderPatch>,
    },
    Abort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::{Action, HOST_METHODS};

    #[test]
    fn continue_preserves_legacy_shape_and_only_continue_accepts_api_key() {
        let legacy: Action = serde_json::from_str(r#"{"action":"continue"}"#).unwrap();
        assert_eq!(
            serde_json::to_value(legacy).unwrap(),
            serde_json::json!({"action":"continue","headers":[]})
        );
        let selected: Action =
            serde_json::from_str(r#"{"action":"continue","apiKey":"test-key"}"#).unwrap();
        assert_eq!(
            serde_json::to_value(selected).unwrap()["apiKey"],
            "test-key"
        );
        for action in ["retry", "wait", "abort"] {
            assert!(
                serde_json::from_value::<Action>(
                    serde_json::json!({"action":action,"apiKey":"test-key","token":"job"})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn host_methods_match_the_lifecycle_protocol() {
        assert_eq!(
            HOST_METHODS,
            [
                "request.beforeSend",
                "request.afterHeaders",
                "request.resume",
                "request.completed",
                "request.failed",
                "request.cancelled",
            ]
        );
    }
}
