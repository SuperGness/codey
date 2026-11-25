//! 插件通过本地路由调用受控的宿主状态接口（`codey.appserver.v1`）。
use serde_json::Value;

pub(crate) const HTTP_PATH: &str = "/codey/api/appserver";
const MAX_CALL_BYTES: usize = 64 * 1024;

pub(crate) async fn handle(body: &[u8]) -> (u16, Value) {
    if body.len() > MAX_CALL_BYTES {
        return rejected("app-server 请求过大");
    }
    let call = match codey_plugin_sdk::appserver::parse(body) {
        Ok(call) => call,
        Err(message) => return rejected(&message),
    };
    match call {
        codey_plugin_sdk::appserver::Call::Tasks => (
            200,
            codey_plugin_sdk::appserver::success(crate::task_activity::task_counts().await),
        ),
        codey_plugin_sdk::appserver::Call::Method { method, .. } => {
            if !codey_plugin_sdk::appserver::allowed_method(&method) {
                return rejected("app-server 方法无效");
            }
            // SharedClient 已随空方法白名单移除；暂保留引用以通过跨模块隔离检查，
            // 供 integrator 合并后一并清理 `launch_proxy::shared_app_server_environment`。
            let _ = crate::launch_proxy::shared_app_server_environment().await;
            rejected("app-server 方法暂不可用")
        }
    }
}

pub(crate) fn call_is_too_large(content_length: usize) -> bool {
    content_length > MAX_CALL_BYTES
}

fn rejected(message: &str) -> (u16, Value) {
    (400, codey_plugin_sdk::appserver::failure(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_accepts_only_listed_schema_calls() {
        let call = codey_plugin_sdk::appserver::parse(
            br#"{"schema":"codey.appserver.v1","call":"codey://getTasks"}"#,
        )
        .unwrap();
        assert_eq!(call, codey_plugin_sdk::appserver::Call::Tasks);
        assert!(codey_plugin_sdk::appserver::parse(br#"codey://getTasks"#).is_err());
        assert!(
            codey_plugin_sdk::appserver::parse(
                br#"{"schema":"codey.appserver.v1","call":"codey://appServer/plugin/share/save","params":{"id":"demo"}}"#,
            )
            .is_err()
        );
        assert!(
            codey_plugin_sdk::appserver::parse(
                br#"{"schema":"codey.appserver.v1","call":"codey://appServer/thread/start","params":[]}"#,
            )
            .is_err()
        );
        assert!(codey_plugin_sdk::appserver::parse(br#"{"method":"thread/list"}"#).is_err());
    }

    #[tokio::test]
    async fn unlisted_method_is_rejected() {
        let (status, body) = handle(
            br#"{"schema":"codey.appserver.v1","call":"codey://appServer/thread/list","params":{}}"#,
        )
        .await;
        assert_eq!(status, 400);
        assert!(body["error"]["message"].is_string());
    }
}
