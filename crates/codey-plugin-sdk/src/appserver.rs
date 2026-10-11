//! 本地路由 app-server 的 JSON 协议类型；不提供 HTTP 客户端或认证授权。
//! 请求和响应都使用 `codey.appserver.v1`。
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const CAPABILITY: &str = "appserver.call.v1";
pub const SCHEMA: &str = "codey.appserver.v1";

/// 已开放的 app-server 方法。未列入的调用不会执行，也不会启动共享进程。
pub const METHODS: &[&str] = &[];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    pub call: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Map<String, Value>>,
}

impl Request {
    /// 构造无需参数的任务数量查询；此类型不负责 HTTP 连接或认证。
    pub fn get_tasks() -> Self {
        Self {
            schema: SCHEMA.into(),
            call: "codey://getTasks".into(),
            params: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Tasks,
    Method {
        method: String,
        params: Map<String, Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCounts {
    pub running: u32,
    pub failed: u32,
}

pub fn parse(bytes: &[u8]) -> Result<Call, String> {
    let request: Request = serde_json::from_slice(bytes).map_err(|_| schema_error())?;
    if request.schema != SCHEMA {
        return Err(schema_error());
    }
    let Some(uri) = request.call.strip_prefix("codey://") else {
        return Err(schema_error());
    };
    parse_uri(uri, request.params)
}

pub fn allowed_method(method: &str) -> bool {
    METHODS.contains(&method) && method_name_is_safe(method)
}

fn parse_uri(path: &str, params: Option<Map<String, Value>>) -> Result<Call, String> {
    if path.is_empty() || path.contains(['?', '#', ' ', '\\']) {
        return Err(schema_error());
    }
    if path == "getTasks" {
        if params.is_some() {
            return Err(schema_error());
        }
        return Ok(Call::Tasks);
    }
    let Some(method) = path.strip_prefix("appServer/") else {
        return Err(schema_error());
    };
    if !allowed_method(method) {
        return Err(schema_error());
    }
    Ok(Call::Method {
        method: method.to_string(),
        params: params.unwrap_or_default(),
    })
}

fn method_name_is_safe(method: &str) -> bool {
    let mut chars = method.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    method.len() <= 80
        && first.is_ascii_alphabetic()
        && method != "initialize"
        && !method.contains("..")
        && !method.starts_with('/')
        && !method.contains("//")
        && chars.all(|char| char.is_ascii_alphanumeric() || matches!(char, '/' | '_' | '-' | '.'))
}

pub fn success(result: impl Serialize) -> Value {
    json!({
        "schema": SCHEMA,
        "result": result,
    })
}

pub fn failure(message: impl AsRef<str>) -> Value {
    json!({
        "schema": SCHEMA,
        "error": { "message": message.as_ref() },
    })
}

fn schema_error() -> String {
    "请求不符合 codey.appserver.v1".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_request_round_trips_and_result_decodes() {
        let request = Request::get_tasks();
        let bytes = serde_json::to_vec(&request).unwrap();
        assert_eq!(parse(&bytes).unwrap(), Call::Tasks);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("params").is_none());
        let response = success(TaskCounts {
            running: 2,
            failed: 1,
        });
        let counts: TaskCounts = serde_json::from_value(response["result"].clone()).unwrap();
        assert_eq!(
            counts,
            TaskCounts {
                running: 2,
                failed: 1
            }
        );
    }

    #[test]
    fn schema_accepts_listed_calls_only() {
        let tasks = parse(br#"{"schema":"codey.appserver.v1","call":"codey://getTasks"}"#).unwrap();
        assert_eq!(tasks, Call::Tasks);
        assert!(parse(br#"codey://getTasks"#).is_err());
        assert!(
            parse(br#"{"schema":"codey.appserver.v1","call":"codey://getTasks","params":{}}"#,)
                .is_err()
        );
        assert!(
            parse(
                br#"{"schema":"codey.appserver.v1","call":"codey://appServer/thread/list","params":{"limit":1}}"#,
            )
            .is_err()
        );
        assert!(
            parse(br#"{"schema":"codey.appserver.v1","call":"codey://getTasks","token":"secret"}"#)
                .is_err()
        );
        assert!(parse(br#"{"schema":"codey.appserver.v1","call":{"type":"tasks"}}"#).is_err());
        assert!(METHODS.is_empty());
        assert!(!allowed_method("thread/list"));
        assert!(!allowed_method("initialize"));
        let published = include_str!("../schema/appserver.v1.json");
        assert!(published.contains("\"const\": \"codey://getTasks\""));
        assert!(!published.contains("appServer/"));
    }
}
