use super::*;

use std::sync::LazyLock;

const REJECTED_REASONING_ITEM_LIMIT: usize = 256;
const MAX_REASONING_ITEM_ID_BYTES: usize = 256;

/// 上游明确拒绝过密文的 reasoning 项。Codex 每轮都会原样重放会话历史，
/// 不记住这些 id，同一会话此后每轮都会先失败一次。item id 全局唯一，
/// 因此不按线路或会话区分；只存于内存，重启后由首次拒绝重新学习。
#[derive(Default)]
struct RejectedReasoningItems {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl RejectedReasoningItems {
    fn insert(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > REJECTED_REASONING_ITEM_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
        true
    }
}

static REJECTED_REASONING_ITEMS: LazyLock<Mutex<RejectedReasoningItems>> =
    LazyLock::new(Mutex::default);

fn rejected_reasoning_items() -> std::sync::MutexGuard<'static, RejectedReasoningItems> {
    REJECTED_REASONING_ITEMS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `invalid_encrypted_content` 只在消息正文里给出 item id，例如
/// "The encrypted content for item rs_… could not be verified."
pub(crate) fn rejected_reasoning_item_id(error: &Value) -> Option<&str> {
    if error.get("code").and_then(Value::as_str) != Some("invalid_encrypted_content") {
        return None;
    }
    let id = error
        .get("message")
        .and_then(Value::as_str)?
        .split_once("for item ")?
        .1
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ':', ';']);
    (!id.is_empty()
        && id.len() <= MAX_REASONING_ITEM_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(id)
}

pub(crate) fn responses_event_rejected_reasoning_item(event: &Value) -> Option<&str> {
    let error = match event.get("type").and_then(Value::as_str) {
        Some("error") => event.get("error"),
        Some("response.failed") => event.pointer("/response/error"),
        _ => None,
    }?;
    rejected_reasoning_item_id(error)
}

pub(crate) fn http_error_rejected_reasoning_item(body: &[u8]) -> Option<String> {
    let body = serde_json::from_slice::<Value>(body).ok()?;
    rejected_reasoning_item_id(body.get("error")?).map(str::to_string)
}

pub(crate) fn record_rejected_reasoning_item(id: &str) {
    if !rejected_reasoning_items().insert(id) {
        return;
    }
    record_router_failure_nonblocking(
        "local_router_reasoning_item_rejected",
        "strip_rejected_reasoning_item",
        "上游无法校验 reasoning 密文，后续请求去掉该项的密文与 id",
        json!({"itemId": id, "requestId": current_router_request_id()}),
    );
}

/// 去掉上游拒绝过的 reasoning 项：保留可见摘要并删除 id 与密文，
/// 没有可见文本时整项删除。其它条目即使 id 相同也不改动。
pub(crate) fn strip_rejected_reasoning_items(body: &mut Value) -> bool {
    let rejected_items = rejected_reasoning_items();
    if rejected_items.ids.is_empty() {
        return false;
    }
    let rejected = |item: &Value| {
        item.get("type").and_then(Value::as_str) == Some("reasoning")
            && item
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| rejected_items.ids.contains(id))
    };
    match body.get_mut("input") {
        Some(Value::Array(input)) => {
            let mut changed = false;
            input.retain_mut(|item| {
                if !rejected(item) {
                    return true;
                }
                changed = true;
                strip_rejected_reasoning_item(item)
            });
            changed
        }
        Some(item @ Value::Object(_)) if rejected(item) => {
            if !strip_rejected_reasoning_item(item) {
                *item = Value::Array(Vec::new());
            }
            true
        }
        _ => false,
    }
}

/// 返回 false 表示该项没有可保留的可见文本。
fn strip_rejected_reasoning_item(item: &mut Value) -> bool {
    if matches!(port_reasoning_item(item), HistoryItemRewrite::Drop) {
        return false;
    }
    if let Some(object) = item.as_object_mut() {
        object.remove("id");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::tests::{connect_router_websocket, router_config};
    use super::*;
    use crate::model_id::model_alias;

    fn rejection(id: &str) -> Value {
        json!({
            "type":"invalid_request_error",
            "code":"invalid_encrypted_content",
            "message":format!("The encrypted content for item {id} could not be verified. Reason: Encrypted content item_id did not match the target item id."),
        })
    }

    fn unique_id() -> String {
        format!("rs_{}", Uuid::new_v4().simple())
    }

    #[test]
    fn rejected_item_id_is_read_only_from_invalid_encrypted_content() {
        let id = "rs_07dd4b33bf132047016ae11da78eb487d212a4c7d014a880d1";
        assert_eq!(rejected_reasoning_item_id(&rejection(id)), Some(id));
        let mut other_code = rejection(id);
        other_code["code"] = json!("invalid_request");
        assert_eq!(rejected_reasoning_item_id(&other_code), None);
        let trailing = json!({"code":"invalid_encrypted_content","message":format!("The encrypted content for item {id}.")});
        assert_eq!(rejected_reasoning_item_id(&trailing), Some(id));
        for message in [
            "could not be verified",
            "for item rs_\"x\" bad",
            "for item ",
        ] {
            let error = json!({"code":"invalid_encrypted_content","message":message});
            assert_eq!(rejected_reasoning_item_id(&error), None, "{message}");
        }

        let error = rejection(id);
        let ws = json!({"type":"error","status":400,"error":error});
        let failed = json!({"type":"response.failed","response":{"error":error}});
        let delta = json!({"type":"response.output_text.delta","error":error});
        assert_eq!(responses_event_rejected_reasoning_item(&ws), Some(id));
        assert_eq!(responses_event_rejected_reasoning_item(&failed), Some(id));
        assert_eq!(responses_event_rejected_reasoning_item(&delta), None);
        let http = serde_json::to_vec(&json!({"error":error})).unwrap();
        assert_eq!(
            http_error_rejected_reasoning_item(&http).as_deref(),
            Some(id)
        );
    }

    #[test]
    fn rejected_items_keep_visible_summary_and_lose_id_and_ciphertext() {
        let (summarized, opaque, healthy) = (unique_id(), unique_id(), unique_id());
        let mut body = json!({"input":[
            {"type":"message","role":"user","content":"task"},
            {"type":"reasoning","id":summarized,"encrypted_content":"bad","summary":[{"type":"summary_text","text":"plan"}]},
            {"type":"reasoning","id":opaque,"encrypted_content":"bad","summary":[]},
            {"type":"reasoning","id":healthy,"encrypted_content":"good","summary":[]},
            {"type":"message","id":summarized,"role":"assistant","content":"done"},
        ]});
        let original = body.clone();
        assert!(!strip_rejected_reasoning_items(&mut body));
        assert_eq!(body, original);
        record_rejected_reasoning_item(&summarized);
        record_rejected_reasoning_item(&opaque);
        assert!(strip_rejected_reasoning_items(&mut body));
        assert_eq!(
            body["input"],
            json!([
                {"type":"message","role":"user","content":"task"},
                {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},
                {"type":"reasoning","id":healthy,"encrypted_content":"good","summary":[]},
                {"type":"message","id":summarized,"role":"assistant","content":"done"},
            ])
        );
        assert!(!strip_rejected_reasoning_items(&mut body));

        let mut single = json!({"input":{"type":"reasoning","id":opaque,"encrypted_content":"bad","summary":[]}});
        assert!(strip_rejected_reasoning_items(&mut single));
        assert_eq!(single["input"], json!([]));

        let mut normalized = original;
        assert!(normalize_native_responses_context(&mut normalized, false));
        assert_eq!(normalized["input"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn rejected_item_registry_is_bounded() {
        let mut items = RejectedReasoningItems::default();
        assert!(items.insert("rs_first"));
        assert!(!items.insert("rs_first"));
        for index in 0..REJECTED_REASONING_ITEM_LIMIT {
            assert!(items.insert(&format!("rs_{index}")));
        }
        assert_eq!(items.ids.len(), REJECTED_REASONING_ITEM_LIMIT);
        assert_eq!(items.order.len(), REJECTED_REASONING_ITEM_LIMIT);
        assert!(!items.ids.contains("rs_first"));
    }

    fn history(id: &str) -> Value {
        json!([
            {"role":"user","content":"task"},
            {"type":"reasoning","id":id,"encrypted_content":"mismatched","summary":[{"type":"summary_text","text":"plan"}]},
            {"role":"user","content":"continue"},
        ])
    }

    fn stripped(id: &str) -> Value {
        let mut input = history(id);
        input[1] = json!({"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]});
        input
    }

    fn completed(id: &str) -> Value {
        json!({"type":"response.completed","response":{"id":id,"object":"response","status":"completed","output":[]}})
    }

    #[tokio::test]
    async fn http_rejection_retries_without_the_item_and_later_requests_skip_it() {
        let id = unique_id();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let rejected = rejection(&id);
        let upstream = tokio::spawn(async move {
            let mut attempts = Vec::new();
            while attempts.len() < 3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let Ok(request) = read_http_request(&mut stream).await else {
                    continue;
                };
                let body = serde_json::from_slice::<Value>(&request.body).unwrap();
                attempts.push(body["input"].clone());
                let (status, response) = if attempts.len() == 1 {
                    (400, json!({"error":rejected}))
                } else {
                    (200, completed("resp-ok")["response"].clone())
                };
                write_json_response(&mut stream, status, &response)
                    .await
                    .unwrap();
            }
            attempts
        });
        let (config, provider, model) = router_config(format!("http://{address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        for _ in 0..2 {
            let response = reqwest::Client::new()
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({
                    "model":model_alias(&provider, &model),
                    "input":history(&id),
                    "store":false,
                    "stream":false,
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }
        let attempts = upstream.await.unwrap();
        assert_eq!(attempts, [history(&id), stripped(&id), stripped(&id)]);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn websocket_rejection_falls_back_to_http_and_later_requests_skip_it() {
        let id = unique_id();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let rejected = rejection(&id);
        let expected = (history(&id), stripped(&id));
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected response.create");
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["input"], expected.0);
            let error = json!({"type":"error","status":400,"error":rejected});
            ws.send(WebSocketMessage::Text(error.to_string().into()))
                .await
                .unwrap();

            let (mut http, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut http).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["input"], expected.1);
            assert!(body.get("type").is_none(), "{body}");
            let response = format!("data: {}\n\n", completed("resp-http"));
            http.write_all(format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                response.len()
            ).as_bytes()).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected later response.create");
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["input"], expected.1);
            ws.send(WebSocketMessage::Text(
                completed("resp-ws").to_string().into(),
            ))
            .await
            .unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(3), ws.next()).await;
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let mut client = connect_router_websocket(&router.endpoint()).await;
        for expected in ["resp-http", "resp-ws"] {
            let request = json!({
                "type":"response.create",
                "model":model_alias(&provider, &model),
                "input":history(&id),
            });
            client
                .send(WebSocketMessage::Text(request.to_string().into()))
                .await
                .unwrap();
            let terminal = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let WebSocketMessage::Text(text) = client.next().await.unwrap().unwrap() else {
                        continue;
                    };
                    let event: Value = serde_json::from_str(&text).unwrap();
                    if responses_event_is_terminal(&event) {
                        return event;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(terminal["response"]["id"], expected, "{terminal}");
        }
        client.close(None).await.unwrap();
        upstream.await.unwrap();
        router.stop().await.unwrap();
    }
}
