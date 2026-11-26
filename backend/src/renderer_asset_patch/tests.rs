use std::path::Path;
use std::time::SystemTime;

use super::anchors::*;
use super::cache::*;
use super::*;

type TestSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

struct TestService {
    socket: TestSocket,
    context: Arc<ServiceContext>,
    client: tokio::task::JoinHandle<Result<()>>,
}

impl TestService {
    async fn start(cache_dir: &Path) -> Self {
        Self::start_with(cache_dir, |_| {}).await
    }

    async fn start_with(cache_dir: &Path, configure: impl FnOnce(&mut ServiceContext)) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut context = ServiceContext::new(true, cache_dir.to_path_buf());
        configure(&mut context);
        let context = Arc::new(context);
        let client = tokio::spawn({
            let context = context.clone();
            async move {
                let url = format!("ws://{address}");
                let socket = codey_runtime_core::bridge::connect_cdp_websocket(&url)
                    .await
                    .unwrap();
                serve(socket, context).await
            }
        });
        let (stream, _) = listener.accept().await.unwrap();
        let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        Self {
            socket,
            context,
            client,
        }
    }

    async fn recv(&mut self) -> Value {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(2), self.socket.next())
                .await
                .expect("service should send a command")
                .unwrap()
                .unwrap();
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    async fn expect(&mut self, method: &str) -> Value {
        let command = self.recv().await;
        assert_eq!(command["method"], method, "{command}");
        command
    }

    async fn send(&mut self, payload: Value) {
        self.socket
            .send(Message::Text(payload.to_string().into()))
            .await
            .unwrap();
    }

    async fn reply(&mut self, command: &Value, result: Value) {
        self.send(json!({ "id": command["id"], "result": result }))
            .await;
    }

    async fn answer_startup(&mut self, targets: Value) {
        let auto_attach = self.expect("Target.setAutoAttach").await;
        assert_eq!(auto_attach["params"]["waitForDebuggerOnStart"], true);
        self.reply(&auto_attach, json!({})).await;
        let listed = self.expect("Target.getTargets").await;
        self.reply(&listed, json!({ "targetInfos": targets })).await;
    }

    async fn attach_page(&mut self, session: &str, target: &str, url: &str, waiting: bool) {
        self.send(json!({ "method": "Target.attachedToTarget", "params": {
            "sessionId": session, "waitingForDebugger": waiting,
            "targetInfo": { "targetId": target, "type": "page", "url": url }
        }}))
        .await;
        let fetch = self.expect("Fetch.enable").await;
        assert_eq!(fetch["sessionId"], session);
        self.reply(&fetch, json!({})).await;
    }

    async fn finish(mut self) {
        self.socket.close(None).await.unwrap();
        while let Some(Ok(_)) = self.socket.next().await {}
        self.client.await.unwrap().unwrap();
    }
}

fn paused(session: &str, request_id: &str, url: &str, response: bool) -> Value {
    let mut params = json!({ "requestId": request_id, "request": { "url": url } });
    if response {
        params["responseStatusCode"] = json!(200);
        params["responseHeaders"] = json!([
            { "name": "Content-Type", "value": "text/javascript" },
            { "name": "Content-Length", "value": "3" },
        ]);
    }
    json!({ "method": "Fetch.requestPaused", "sessionId": session, "params": params })
}

#[tokio::test]
async fn startup_animation_resumes_the_page_before_registering_the_player() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start_with(temp.path(), |context| {
        context.startup_animation = true;
    })
    .await;
    service.answer_startup(json!([])).await;
    service
        .attach_page(
            "aux-session",
            "aux",
            "app://-/index.html?initialRoute=%2Fglobal-dictation",
            true,
        )
        .await;
    // Auxiliary windows with an explicit initialRoute resume immediately without Page.enable.
    let aux_resume = service.expect("Runtime.runIfWaitingForDebugger").await;
    assert_eq!(aux_resume["sessionId"], "aux-session");
    service.attach_page("main-session", "main", "", true).await;
    // The renderer cannot acknowledge Page.enable while waiting for the debugger.
    let resume = service.expect("Runtime.runIfWaitingForDebugger").await;
    assert_eq!(resume["sessionId"], "main-session");
    let page_enable = service.expect("Page.enable").await;
    assert_eq!(page_enable["sessionId"], "main-session");
    service.reply(&page_enable, json!({})).await;
    let script = service
        .expect("Page.addScriptToEvaluateOnNewDocument")
        .await;
    assert_eq!(script["sessionId"], "main-session");
    assert_eq!(script["params"]["worldName"], "codey-startup");
    assert_eq!(script["params"]["runImmediately"], true);
    service
        .reply(&script, json!({ "identifier": "startup" }))
        .await;

    service.finish().await;
}

#[tokio::test]
async fn stalled_startup_page_enable_never_blocks_fetch_source_gate_patching() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start_with(temp.path(), |context| {
        context.startup_animation = true;
    })
    .await;
    service.answer_startup(json!([])).await;
    service.attach_page("main-session", "main", "", true).await;
    let resume = service.expect("Runtime.runIfWaitingForDebugger").await;
    assert_eq!(resume["sessionId"], "main-session");
    let page_enable = service.expect("Page.enable").await;
    assert_eq!(page_enable["sessionId"], "main-session");
    // Do NOT reply to Page.enable: simulate cold-start Page.enable stall (#63240).
    let url = "app://-/assets/async/49792.d8d33ace73.js";
    service.send(paused("main-session", "r-gate", url, true)).await;
    let body = service.expect("Fetch.getResponseBody").await;
    service
        .reply(
            &body,
            json!({ "body": "codex-message-from-view old.sendMessageFromView Failed to send message from view" }),
        )
        .await;
    let evaluate = service.expect("Runtime.evaluate").await;
    service
        .reply(
            &evaluate,
            json!({ "result": { "type": "object", "objectId": "obj-gate" } }),
        )
        .await;
    let call_fn = service.expect("Runtime.callFunctionOn").await;
    service
        .reply(
            &call_fn,
            json!({ "result": { "value": { "installed": true, "patched": "patchedGate()", "logs": [] } } }),
        )
        .await;
    let fulfill = service.expect("Fetch.fulfillRequest").await;
    service.reply(&fulfill, json!({})).await;

    service.finish().await;
}

#[tokio::test]
async fn new_pages_resume_only_after_interception_is_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service
        .send(json!({ "method": "Target.attachedToTarget", "params": {
            "sessionId": "worker-session", "waitingForDebugger": true,
            "targetInfo": { "targetId": "worker", "type": "service_worker", "url": "" }
        }}))
        .await;
    let resume_worker = service.expect("Runtime.runIfWaitingForDebugger").await;
    assert_eq!(resume_worker["sessionId"], "worker-session");
    let detach = service.expect("Target.detachFromTarget").await;
    assert_eq!(detach["params"]["sessionId"], "worker-session");

    service
        .send(json!({ "method": "Target.attachedToTarget", "params": {
            "sessionId": "page-session", "waitingForDebugger": true,
            "targetInfo": { "targetId": "main", "type": "page", "url": "" }
        }}))
        .await;
    let fetch = service.expect("Fetch.enable").await;
    assert_eq!(fetch["sessionId"], "page-session");
    let patterns = fetch["params"]["patterns"].as_array().unwrap();
    assert!(patterns.iter().all(|pattern| {
        let expected_type = if pattern["urlPattern"] == PET_SPRITE_URL_PATTERN {
            "Image"
        } else if pattern["urlPattern"] == CSS_ASSET_URL_PATTERN {
            "Stylesheet"
        } else {
            "Script"
        };
        pattern["resourceType"] == expected_type
            && pattern["requestStage"] == "Request"
            && pattern["urlPattern"]
                .as_str()
                .unwrap()
                .starts_with("app://*/assets/")
    }));
    // 拦截落位前不能放行。
    assert!(
        tokio::time::timeout(Duration::from_millis(100), service.socket.next())
            .await
            .is_err()
    );
    service.reply(&fetch, json!({})).await;
    let resume = service.expect("Runtime.runIfWaitingForDebugger").await;
    assert_eq!(resume["sessionId"], "page-session");
    assert!(
        service
            .context
            .page_stats("main")
            .unwrap()
            .attached_before_load
    );
    service.finish().await;
}

#[tokio::test]
async fn existing_pages_are_attached_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service
        .answer_startup(json!([
            { "targetId": "main", "type": "page", "url": "about:blank" },
            { "targetId": "sandbox", "type": "webview", "url": "codex-sandbox://x" },
        ]))
        .await;
    let attach = service.expect("Target.attachToTarget").await;
    assert_eq!(
        attach["params"],
        json!({ "targetId": "main", "flatten": true })
    );
    service.reply(&attach, json!({ "sessionId": "s1" })).await;
    // 已加载完的页面也挂上（后续按需加载的脚本仍可改写），但记为未覆盖首个文档。
    service
        .attach_page("s2", "loaded", "app://-/index.html", false)
        .await;
    let stats = service.context.page_stats("loaded").unwrap();
    assert!(!stats.attached_before_load);
    service.finish().await;
}

#[tokio::test]
async fn misses_patch_in_the_page_and_hits_fulfill_from_cache() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service
        .attach_page("s1", "main", "about:blank", false)
        .await;

    let url = "app://-/assets/348410.f0081c7d18.js";
    let asset_body =
        "codex-message-from-view old.sendMessageFromView Failed to send message from view";
    service.send(paused("s1", "r1", url, false)).await;
    let intercept = service.expect("Fetch.continueRequest").await;
    assert_eq!(intercept["params"]["interceptResponse"], true);
    service.reply(&intercept, json!({})).await;

    service.send(paused("s1", "r1", url, true)).await;
    let body = service.expect("Fetch.getResponseBody").await;
    let encoded = base64::engine::general_purpose::STANDARD.encode(asset_body);
    service
        .reply(&body, json!({ "body": encoded, "base64Encoded": true }))
        .await;
    let evaluate = service.expect("Runtime.evaluate").await;
    assert_eq!(evaluate["sessionId"], "s1");
    assert_eq!(evaluate["params"]["returnByValue"], false);
    assert!(
        evaluate["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("__codeyRendererPatchEntry")
    );
    service
        .reply(
            &evaluate,
            json!({ "result": { "type": "object", "objectId": "obj-global-1" } }),
        )
        .await;
    let call_fn = service.expect("Runtime.callFunctionOn").await;
    assert_eq!(call_fn["sessionId"], "s1");
    assert_eq!(call_fn["params"]["objectId"], "obj-global-1");
    assert_eq!(call_fn["params"]["arguments"][0]["value"], url);
    assert_eq!(call_fn["params"]["arguments"][1]["value"], asset_body);
    service
        .reply(
            &call_fn,
            json!({ "result": { "value": { "installed": true, "patched": "new()", "logs": [] } } }),
        )
        .await;
    let fulfill = service.expect("Fetch.fulfillRequest").await;
    assert_eq!(
        fulfill["params"]["body"],
        base64::engine::general_purpose::STANDARD.encode("new()")
    );
    assert_eq!(
        fulfill["params"]["responseHeaders"],
        json!([{ "name": "Content-Type", "value": "text/javascript" }])
    );
    service.reply(&fulfill, json!({})).await;

    // 另一个窗口加载同一资产：请求阶段直接用缓存，不再读正文或求值。缓存
    // 写入是异步的，尚未落盘时会先要求响应阶段暂停。
    let mut cached = None;
    for attempt in 0..50 {
        let request_id = format!("again-{attempt}");
        service.send(paused("s1", &request_id, url, false)).await;
        let command = service.recv().await;
        if command["method"] == "Fetch.fulfillRequest" {
            cached = Some(command);
            break;
        }
        assert_eq!(command["method"], "Fetch.continueRequest");
        service.reply(&command, json!({})).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cached = cached.expect("the patched asset should be served from the cache");
    assert_eq!(cached["params"]["body"], fulfill["params"]["body"]);
    assert_eq!(cached["params"]["responseCode"], 200);
    assert_eq!(
        cached["params"]["responseHeaders"],
        fulfill["params"]["responseHeaders"]
    );
    service.reply(&cached, json!({})).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let stats = service.context.page_stats("main").unwrap();
    assert_eq!((stats.patched, stats.cache_hits), (2, 1));
    service.finish().await;
}

#[tokio::test]
async fn failed_evaluation_passes_the_original_response_through_uncached() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;
    let url = "app://-/assets/357055.788c31e830.js";
    service.send(paused("s1", "r1", url, true)).await;
    let body = service.expect("Fetch.getResponseBody").await;
    service
        .reply(
            &body,
            json!({ "body": "codex-message-from-view old.sendMessageFromView Failed to send message from view" }),
        )
        .await;
    let evaluate = service.expect("Runtime.evaluate").await;
    service
        .reply(
            &evaluate,
            json!({ "result": { "type": "object", "objectId": "obj-global-fail" } }),
        )
        .await;
    let call_fn = service.expect("Runtime.callFunctionOn").await;
    service
        .reply(
            &call_fn,
            json!({ "result": { "value": { "installed": false, "logs": [] } } }),
        )
        .await;
    let pass = service.expect("Fetch.continueRequest").await;
    assert_eq!(pass["params"], json!({ "requestId": "r1" }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(service.context.page_stats("main").unwrap().skipped, 1);
    assert!(service.context.cache.load(url).await.is_none());
    service.finish().await;
}

#[tokio::test]
async fn second_miss_reuses_installed_patch_entry_and_recovers_from_stale_object_id() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;

    let body_text =
        "codex-message-from-view old.sendMessageFromView Failed to send message from view";
    let url1 = "app://-/assets/async/111111.aaaaaaaa.js";
    service.send(paused("s1", "r1", url1, true)).await;
    let b1 = service.expect("Fetch.getResponseBody").await;
    service.reply(&b1, json!({ "body": body_text })).await;
    let eval1 = service.expect("Runtime.evaluate").await;
    service
        .reply(
            &eval1,
            json!({ "result": { "type": "object", "objectId": "obj-1" } }),
        )
        .await;
    let call1 = service.expect("Runtime.callFunctionOn").await;
    assert_eq!(call1["params"]["objectId"], "obj-1");
    service
        .reply(
            &call1,
            json!({ "result": { "value": { "installed": true, "patched": "p1()", "logs": [] } } }),
        )
        .await;
    let fulfill1 = service.expect("Fetch.fulfillRequest").await;
    service.reply(&fulfill1, json!({})).await;

    // 第二个未命中 chunk 直接复用 objectId，不再发送 Runtime.evaluate。
    let url2 = "app://-/assets/async/222222.bbbbbbbb.js";
    service.send(paused("s1", "r2", url2, true)).await;
    let b2 = service.expect("Fetch.getResponseBody").await;
    service.reply(&b2, json!({ "body": body_text })).await;
    let call2 = service.expect("Runtime.callFunctionOn").await;
    assert_eq!(call2["params"]["objectId"], "obj-1");
    service
        .reply(
            &call2,
            json!({ "result": { "value": { "installed": true, "patched": "p2()", "logs": [] } } }),
        )
        .await;
    let fulfill2 = service.expect("Fetch.fulfillRequest").await;
    service.reply(&fulfill2, json!({})).await;

    // 重载导致旧 objectId 失效时，自动重新安装入口并重试 callFunctionOn。
    let url3 = "app://-/assets/async/333333.cccccccc.js";
    service.send(paused("s1", "r3", url3, true)).await;
    let b3 = service.expect("Fetch.getResponseBody").await;
    service.reply(&b3, json!({ "body": body_text })).await;
    let stale_call = service.expect("Runtime.callFunctionOn").await;
    assert_eq!(stale_call["params"]["objectId"], "obj-1");
    service
        .send(json!({
            "id": stale_call["id"],
            "error": { "message": "Could not find object with given id" }
        }))
        .await;
    let eval2 = service.expect("Runtime.evaluate").await;
    service
        .reply(
            &eval2,
            json!({ "result": { "type": "object", "objectId": "obj-2" } }),
        )
        .await;
    let retry_call = service.expect("Runtime.callFunctionOn").await;
    assert_eq!(retry_call["params"]["objectId"], "obj-2");
    assert_eq!(retry_call["params"]["arguments"][1]["value"], body_text);
    service
        .reply(
            &retry_call,
            json!({ "result": { "value": { "installed": true, "patched": "p3()", "logs": [] } } }),
        )
        .await;
    let fulfill3 = service.expect("Fetch.fulfillRequest").await;
    service.reply(&fulfill3, json!({})).await;
    service.finish().await;
}

#[tokio::test]
async fn cached_fulfillments_release_a_shared_limited_slot() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;
    let url = "app://-/assets/async/49792.d8d33ace73.js";
    service
        .context
        .cache
        .store(
            url,
            Some(StoredPatch {
                status: 200,
                headers: vec![json!({ "name": "Content-Type", "value": "text/javascript" })],
                body_base64: base64::engine::general_purpose::STANDARD.encode("patched()"),
            }),
        )
        .await;

    for request_id in ["r1", "r2", "r3"] {
        service.send(paused("s1", request_id, url, false)).await;
    }
    let first = service.expect("Fetch.fulfillRequest").await;
    let second = service.expect("Fetch.fulfillRequest").await;
    assert_ne!(first["params"]["requestId"], second["params"]["requestId"]);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), service.recv())
            .await
            .is_err()
    );
    service.reply(&first, json!({})).await;
    let third = service.expect("Fetch.fulfillRequest").await;
    assert_ne!(third["params"]["requestId"], first["params"]["requestId"]);
    assert_ne!(third["params"]["requestId"], second["params"]["requestId"]);
    service.reply(&second, json!({})).await;
    service.reply(&third, json!({})).await;
    service.finish().await;
}

#[tokio::test]
async fn cold_responses_and_cached_requests_share_slots_without_holding_idle_permits() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;
    let cached_url = "app://-/assets/async/49792.d8d33ace73.js";
    let cold_url = "app://-/assets/348410.f0081c7d18.js";
    service
        .context
        .cache
        .store(
            cached_url,
            Some(StoredPatch {
                status: 200,
                headers: Vec::new(),
                body_base64: base64::engine::general_purpose::STANDARD.encode("patched()"),
            }),
        )
        .await;

    service.send(paused("s1", "cold", cold_url, false)).await;
    let intercept = service.expect("Fetch.continueRequest").await;
    assert_eq!(intercept["params"]["interceptResponse"], true);
    service.reply(&intercept, json!({})).await;

    service
        .send(paused("s1", "warm-1", cached_url, false))
        .await;
    let first = service.expect("Fetch.fulfillRequest").await;
    service
        .send(paused("s1", "warm-2", cached_url, false))
        .await;
    let second = service.expect("Fetch.fulfillRequest").await;
    service.send(paused("s1", "cold", cold_url, true)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), service.recv())
            .await
            .is_err()
    );
    service.reply(&first, json!({})).await;
    let body = service.expect("Fetch.getResponseBody").await;
    service
        .reply(
            &body,
            json!({ "body": "codex-message-from-view old.sendMessageFromView Failed to send message from view" }),
        )
        .await;
    let evaluate = service.expect("Runtime.evaluate").await;
    service
        .reply(
            &evaluate,
            json!({ "result": { "type": "object", "objectId": "obj-cold" } }),
        )
        .await;
    let call_fn = service.expect("Runtime.callFunctionOn").await;
    service
        .reply(
            &call_fn,
            json!({ "result": { "value": { "installed": true, "patched": "new()", "logs": [] } } }),
        )
        .await;
    let fulfilled = service.expect("Fetch.fulfillRequest").await;
    service.reply(&fulfilled, json!({})).await;
    service.reply(&second, json!({})).await;
    service.finish().await;
}

#[tokio::test]
async fn cached_fulfillment_errors_release_the_shared_slot() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;
    let url = "app://-/assets/async/49792.d8d33ace73.js";
    service
        .context
        .cache
        .store(
            url,
            Some(StoredPatch {
                status: 200,
                headers: Vec::new(),
                body_base64: base64::engine::general_purpose::STANDARD.encode("patched()"),
            }),
        )
        .await;
    service.send(paused("s1", "r1", url, false)).await;
    let first = service.expect("Fetch.fulfillRequest").await;
    service.send(paused("s1", "r2", url, false)).await;
    let second = service.expect("Fetch.fulfillRequest").await;
    service.send(paused("s1", "r3", url, false)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), service.recv())
            .await
            .is_err()
    );
    service
        .send(json!({
            "id": first["id"],
            "error": { "message": "request cancelled" }
        }))
        .await;
    let pass = service.expect("Fetch.continueRequest").await;
    assert_eq!(pass["params"], json!({ "requestId": "r1" }));
    let third = service.expect("Fetch.fulfillRequest").await;
    assert_eq!(third["params"]["requestId"], "r3");
    service.reply(&second, json!({})).await;
    service.reply(&third, json!({})).await;
    service.finish().await;
}

#[test]
fn plain_response_source_moves_instead_of_copying() {
    let mut body = json!({ "result": { "body": "", "base64Encoded": false } });
    let stored = body
        .pointer_mut("/result/body")
        .and_then(|value| match value {
            Value::String(text) => Some(text),
            _ => None,
        })
        .unwrap();
    stored.push_str(&"x".repeat(8 << 20));
    let pointer = stored.as_ptr();
    let source = take_response_source(body).unwrap();
    assert_eq!(source.as_ptr(), pointer);
}

#[tokio::test]
async fn assets_without_patch_anchors_skip_page_evaluation() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;
    let url = "app://-/assets/async/634303.855e57c3e7.js";
    service.send(paused("s1", "r1", url, true)).await;
    let body = service.expect("Fetch.getResponseBody").await;
    service
        .reply(&body, json!({ "body": "export const icon=1;" }))
        .await;
    let pass = service.expect("Fetch.continueRequest").await;
    assert_eq!(pass["params"], json!({ "requestId": "r1" }));
    let mut cached = false;
    for _ in 0..50 {
        if matches!(
            service.context.cache.load(url).await,
            Some(CachedAsset::Unchanged)
        ) {
            cached = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        cached,
        "unrelated numeric chunks should be cached as unchanged"
    );
    service.finish().await;
}

#[test]
fn compound_rule_matcher_requires_all_literals_and_splits_js_from_css() {
    let matcher = PatchAnchorMatcher::for_script(RENDERER_ASSET_PATCH_SCRIPT).unwrap();
    assert!(!matcher.digest_hex().is_empty());

    // 仅出现规则里的部分前置字面量不应误命中。
    assert!(!matcher.matches("let{agentMode:a}resolvedHostId:b"));
    assert!(
        matcher.matches(
            "CodexComposerAdapter let{agentMode:a} resolvedHostId:b submitDisabledReasons:c"
        )
    );

    // 性能规则必须同时命中全部前置字面量。
    assert!(!matcher.matches("durationThreshold:16 PerformanceEventTiming"));
    assert!(matcher.matches("durationThreshold:16 PerformanceEventTiming eventsSupported:"));
    assert!(!matcher.matches("EventLoopBlockRenderer"));
    assert!(matcher.matches("EventLoopBlockRenderer rendererHangDetectionEnabled"));

    // Orbit preload 始终生效。
    assert!(matcher.matches("allow-same-origin \"orbit-character-preloaded\""));

    // Statsig 启动收敛规则需同时命中 bootstrap deadline 与 fetch timeout 两个字面量。
    assert!(!matcher.matches("Reached the post-login Statsig bootstrap deadline"));
    assert!(matcher.matches(
        "Reached the post-login Statsig bootstrap deadline Timed out while fetching post-login Statsig bootstrap"
    ));

    // JS 与 CSS 规则互不误命中。
    assert!(!matcher.matches_asset(
        "app://-/assets/1.aaaaaaaa.js",
        ".g:has([data-state=open]){}"
    ));
    assert!(matcher.matches_asset(
        "app://-/assets/e77f816560c6.css",
        ".g:has([data-state=open]){}"
    ));
    assert!(!matcher.matches_asset(
        "app://-/assets/e77f816560c6.css",
        "EventLoopBlockRenderer rendererHangDetectionEnabled"
    ));

    assert!(PatchAnchorMatcher::for_script("const r=[];").is_err());
}

#[tokio::test]
async fn unchanged_cache_hit_bypasses_exhausted_patch_permits() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = TestService::start(temp.path()).await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;

    let unchanged_url = "app://-/assets/async/123456.855e57c3e7.js";
    service.context.cache.store(unchanged_url, None).await;
    assert!(service.context.cache.is_known_unchanged(unchanged_url));

    // 占满全部补丁并发许可，验证已知 Unchanged 资产仍可零等待放行。
    let permits_context = Arc::clone(&service.context);
    let _held = permits_context
        .permits
        .acquire_many(MAX_CONCURRENT_PATCHES as u32)
        .await
        .unwrap();
    service
        .send(paused("s1", "bypass-permits", unchanged_url, false))
        .await;
    let pass = service.expect("Fetch.continueRequest").await;
    assert_eq!(pass["params"], json!({ "requestId": "bypass-permits" }));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let stats = service.context.page_stats("main").unwrap();
    assert_eq!(stats.cache_hits, 1);
    drop(_held);
    service.finish().await;
}

#[tokio::test]
async fn cache_writes_preencoded_base64_and_throttles_automatic_prunes() {
    let temp = tempfile::tempdir().unwrap();
    let cache = PatchCache::new(temp.path().to_path_buf(), "script");
    let modern_url = "app://-/assets/52443.52443cbe9ac4.js";
    let encoded = base64::engine::general_purpose::STANDARD.encode("patched()");
    cache
        .store(
            modern_url,
            Some(StoredPatch {
                status: 200,
                headers: Vec::new(),
                body_base64: encoded.clone(),
            }),
        )
        .await;
    let modern_name = cache.entry_name(modern_url).unwrap();
    let disk_body = std::fs::read_to_string(temp.path().join(&modern_name)).unwrap();
    assert_eq!(disk_body, encoded);
    let Some(CachedAsset::Patched(hit)) = cache.load(modern_url).await else {
        panic!("entry should load");
    };
    assert_eq!(hit.body_base64, encoded);
    // 刚执行过自动整理，短时间内再次检查会被节流跳过。
    assert!(!cache.prune_if_due());
}

#[test]
fn cache_read_skips_mtime_touch_before_threshold_and_updates_after() {
    let temp = tempfile::tempdir().unwrap();
    let cache = PatchCache::new(temp.path().to_path_buf(), "script");
    let url = "app://-/assets/async/634303.855e57c3e7.js";
    let name = cache.entry_name(url).unwrap();
    store_entry(temp.path(), &name, None).unwrap();
    let meta_path = temp.path().join(format!("{name}.meta.json"));

    let recent = SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options()
        .append(true)
        .open(&meta_path)
        .unwrap()
        .set_modified(recent)
        .unwrap();
    let before = std::fs::metadata(&meta_path).unwrap().modified().unwrap();
    assert!(matches!(
        load_entry(temp.path(), &name),
        Some(CachedAsset::Unchanged)
    ));
    let after_recent = std::fs::metadata(&meta_path).unwrap().modified().unwrap();
    assert_eq!(after_recent, before);

    let stale = SystemTime::now() - (TOUCH_MIN_INTERVAL + Duration::from_secs(60));
    std::fs::File::options()
        .append(true)
        .open(&meta_path)
        .unwrap()
        .set_modified(stale)
        .unwrap();
    assert!(matches!(
        load_entry(temp.path(), &name),
        Some(CachedAsset::Unchanged)
    ));
    let after_stale = std::fs::metadata(&meta_path).unwrap().modified().unwrap();
    assert!(after_stale > stale + Duration::from_secs(3000));
}

#[test]
fn prune_caps_entry_count_by_recent_use() {
    let temp = tempfile::tempdir().unwrap();
    let cache = PatchCache::new(temp.path().to_path_buf(), "patch-script");
    for index in 0..4u64 {
        let entry = format!("{}-{index}.11111111.js", cache.tag);
        store_entry(temp.path(), &entry, None).unwrap();
        std::fs::File::options()
            .append(true)
            .open(temp.path().join(format!("{entry}.meta.json")))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + index))
            .unwrap();
    }
    cache.prune_to(u64::MAX, 2);
    let mut left: Vec<String> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".meta.json"))
        .collect();
    left.sort();
    let tag = &cache.tag;
    assert_eq!(
        left,
        [
            format!("{tag}-2.11111111.js.meta.json"),
            format!("{tag}-3.11111111.js.meta.json"),
        ]
    );
}

#[test]
fn interception_covers_numeric_css_and_pet_sprites() {
    let urls = |patterns: Value| -> Vec<String> {
        patterns
            .as_array()
            .unwrap()
            .iter()
            .map(|pattern| pattern["urlPattern"].as_str().unwrap().to_string())
            .collect()
    };
    let with_pet = urls(fetch_patterns(true, None));
    let without_pet = urls(fetch_patterns(false, None));
    assert_eq!(
        without_pet,
        vec![
            NUMERIC_ASSET_URL_PATTERN.to_string(),
            CSS_ASSET_URL_PATTERN.to_string(),
        ]
    );
    assert_eq!(
        with_pet,
        vec![
            NUMERIC_ASSET_URL_PATTERN.to_string(),
            CSS_ASSET_URL_PATTERN.to_string(),
            PET_SPRITE_URL_PATTERN.to_string(),
        ]
    );
}

#[test]
fn asset_scans_replace_the_numeric_wildcard_with_exact_chunks() {
    let urls = |patterns: Value| -> Vec<String> {
        patterns
            .as_array()
            .unwrap()
            .iter()
            .map(|pattern| pattern["urlPattern"].as_str().unwrap().to_string())
            .collect()
    };
    let scan = crate::renderer_asset_scan::AssetScan {
        candidates: vec![
            "348410.ae8bb87271.js".to_string(),
            "async/24191.589dcc9878.js".to_string(),
            "async/e77f816560c6.css".to_string(),
        ],
    };
    let exact_patterns = fetch_patterns(false, Some(&scan));
    let exact = urls(exact_patterns.clone());
    assert_eq!(
        exact,
        vec![
            "app://*/assets/348410.ae8bb87271.js*".to_string(),
            "app://*/assets/async/24191.589dcc9878.js*".to_string(),
            "app://*/assets/async/e77f816560c6.css*".to_string(),
        ]
    );
    assert!(
        exact_patterns
            .as_array()
            .unwrap()
            .iter()
            .any(
                |pattern| pattern["urlPattern"] == "app://*/assets/async/e77f816560c6.css*"
                    && pattern["resourceType"] == "Stylesheet"
            )
    );
    let exact_with_pet = urls(fetch_patterns(true, Some(&scan)));
    assert_eq!(exact_with_pet.len(), exact.len() + 1);
    assert!(exact_with_pet.contains(&PET_SPRITE_URL_PATTERN.to_string()));
}

#[test]
fn pet_sprites_are_identified_by_their_hashed_file_names() {
    for (url, pet) in [
        (
            "app://-/assets/hoots-spritesheet-v8.52075ad356.webp",
            "hoots",
        ),
        (
            "app://codex/assets/null-signal-spritesheet-v7.e7af0b42f5.webp?x=1",
            "null-signal",
        ),
    ] {
        assert_eq!(pet_sprite_id(url).as_deref(), Some(pet), "{url}");
    }
    for url in [
        "https://codex.example/assets/hoots-spritesheet-v8.52075ad356.webp",
        "app://-/assets/async/hoots-spritesheet-v8.52075ad356.webp",
        "app://-/assets/hoots-spritesheet-v8.webp",
        "app://-/assets/hoots-spritesheet-v8.52075ad356.png",
        "app://-/assets/Hoots-spritesheet-v8.52075ad356.webp",
        "app://-/assets/-spritesheet-v8.52075ad356.webp",
        "app://-/assets/hoots-spritesheet-vX.52075ad356.webp",
        "app://-/assets/bsod-spritesheet-v5.not-a-hash.webp",
    ] {
        assert_eq!(pet_sprite_id(url), None, "{url}");
    }
}

#[tokio::test]
async fn slim_pet_keeps_only_the_selected_pet_sprite() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex-home");
    std::fs::create_dir_all(&home).unwrap();
    let sqlite_path = home.join(".codex-global-state.sqlite");
    {
        let conn = rusqlite::Connection::open(&sqlite_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (
                scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
                PRIMARY KEY (scope, key)
            ) WITHOUT ROWID;
            INSERT INTO settings (scope, key, value) VALUES ('global', 'selected-avatar-id', '\"hoots\"');",
        )
        .unwrap();
    }
    let mut service = TestService::start_with(&temp.path().join("cache"), |context| {
        context.codex_home = Some(home.clone());
    })
    .await;
    service.answer_startup(json!([])).await;
    service.attach_page("s1", "main", "", true).await;
    let _ = service.expect("Runtime.runIfWaitingForDebugger").await;

    let selected = "app://-/assets/hoots-spritesheet-v8.52075ad356.webp";
    service.send(paused("s1", "r1", selected, false)).await;
    let pass = service.expect("Fetch.continueRequest").await;
    assert_eq!(pass["params"], json!({ "requestId": "r1" }));

    let other = "app://-/assets/dewey-spritesheet-v5.81cf696c13.webp";
    service.send(paused("s1", "r2", other, false)).await;
    let blocked = service.expect("Fetch.failRequest").await;
    assert_eq!(
        blocked["params"],
        json!({ "requestId": "r2", "errorReason": "BlockedByClient" })
    );

    std::fs::remove_file(&sqlite_path).unwrap();
    let default = "app://-/assets/codex-spritesheet-v6.922b99db4b.webp";
    service.send(paused("s1", "r3", default, false)).await;
    assert_eq!(
        service.expect("Fetch.continueRequest").await["params"],
        json!({ "requestId": "r3" }),
        "without a saved choice Codex shows its default pet"
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(service.context.page_stats("main").unwrap().blocked, 1);
    service.finish().await;
}

#[tokio::test]
async fn late_asset_scans_narrow_interception_on_attached_pages() {
    let temp = tempfile::tempdir().unwrap();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let mut service = TestService::start_with(temp.path(), |context| {
        *context.pending_scan.get_mut().unwrap() = Some(tokio::task::spawn_blocking(move || {
            gate.recv().unwrap();
            Ok(crate::renderer_asset_scan::ScanOutcome {
                scan: crate::renderer_asset_scan::AssetScan {
                    candidates: vec!["async/24191.589dcc9878.js".to_string()],
                },
                cached: false,
                cache_write_error: None,
            })
        }));
    })
    .await;
    service.answer_startup(json!([])).await;
    service
        .send(json!({ "method": "Target.attachedToTarget", "params": {
            "sessionId": "s1", "waitingForDebugger": false,
            "targetInfo": { "targetId": "main", "type": "page", "url": "" }
        }}))
        .await;
    let broad = service.expect("Fetch.enable").await;
    assert!(
        broad["params"]["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pattern| pattern["urlPattern"] == NUMERIC_ASSET_URL_PATTERN)
    );
    service.reply(&broad, json!({})).await;

    release.send(()).unwrap();
    let narrowed = service.expect("Fetch.enable").await;
    assert_eq!(narrowed["sessionId"], "s1");
    let patterns = narrowed["params"]["patterns"].as_array().unwrap();
    assert!(
        patterns
            .iter()
            .any(|pattern| pattern["urlPattern"] == "app://*/assets/async/24191.589dcc9878.js*")
    );
    assert!(
        !patterns
            .iter()
            .any(|pattern| pattern["urlPattern"] == NUMERIC_ASSET_URL_PATTERN)
    );
    service.reply(&narrowed, json!({})).await;
    service.finish().await;
}

#[test]
fn numeric_assets_require_bounded_hashed_chunk_names() {
    for url in [
        "app://-/assets/104974.1e9a7f6707.js",
        "app://codex/assets/async/634303.855e57c3e7.js?cache=1#loader",
        "app://-/assets/async/e77f816560c6.css",
        "app://-/assets/app-initial-90eb4239.css",
    ] {
        assert!(is_patchable_asset_url(url), "{url}");
    }
    for url in [
        "https://codex.example/assets/async/634303.855e57c3e7.js",
        "app://-/assets/app-initial-90eb42395b7d.js",
        "app://-/assets/pets-settings-90eb42395b7d.js",
        "app://-/assets/async/composer.855e57c3e7.js",
        "app://-/assets/async/634303.js",
        "app://-/assets/async/634303.not-a-hash.js",
        "app://-/assets/async/634303.855e57c3e7.mjs",
        "app://-/assets/async/634303.855e57c3e7.js.map",
        "app://-/assets/other/634303.855e57c3e7.js",
        "app://-/assets/634303.855e57c3e7.js/extra",
        "app://-/assets/12345678901.855e57c3e7.js",
        "app://-/assets/app-initial.css",
        "app://-/assets/async/not-a-hash.css",
    ] {
        assert!(!is_patchable_asset_url(url), "{url}");
    }
}

#[test]
fn page_patch_script_matches_rspack_numeric_assets() {
    assert!(
        RENDERER_ASSET_PATCH_SCRIPT
            .contains(r"/^\/assets\/(?:async\/)?\d{1,10}\.[a-f0-9]{8,32}\.js$/i")
    );
}

#[test]
fn only_content_hashed_numeric_js_and_css_assets_are_cacheable() {
    for name in [
        "104974.1e9a7f6707.js",
        "634303.855e57c3e7.js",
        "e77f816560c6.css",
        "app-initial-90eb4239.css",
    ] {
        assert!(cacheable_asset_name(name), "{name}");
    }
    for name in [
        "app-initial-90eb42395b7d.js",
        "app-primary~b4ce9e46.mjs",
        "general-settings-679bf39c78a4.js",
        "app-initial.js",
        "app-initial-dev.js",
        "app-initial.css",
        "app-initial-dev.css",
        "634303.js",
        "634303.not-a-hash.js",
        "634303.855e57c3e7.js.map",
        "..-90eb42395b7d.js",
        "app initial-90eb42395b7d.js",
    ] {
        assert!(!cacheable_asset_name(name), "{name}");
    }
    let cache = PatchCache::new(PathBuf::from("/tmp"), "script");
    assert_eq!(
        cache.entry_name("app://-/assets/104974.1e9a7f6707.js?build=1#x"),
        Some(format!("{}-104974.1e9a7f6707.js", cache.tag))
    );
    assert_eq!(
        cache.entry_name("app://-/assets/async/634303.855e57c3e7.js?build=1#x"),
        Some(format!("{}-634303.855e57c3e7.js", cache.tag))
    );
    assert_eq!(
        cache.entry_name("app://-/assets/app-initial-90eb42395b7d.js"),
        None
    );
}

#[tokio::test]
async fn cache_round_trips_and_rejects_truncated_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let cache = PatchCache::new(temp.path().to_path_buf(), "script");
    let patched = "app://-/assets/52443.52443cbe9ac4.js";
    let unchanged = "app://-/assets/20868.20eab868ec75.js";
    let encoded = base64::engine::general_purpose::STANDARD.encode("patched()");
    let patch = StoredPatch {
        status: 200,
        headers: vec![json!({ "name": "Content-Type", "value": "text/javascript" })],
        body_base64: encoded.clone(),
    };
    cache.store(patched, Some(patch)).await;
    cache.store(unchanged, None).await;
    let Some(CachedAsset::Patched(hit)) = cache.load(patched).await else {
        panic!("patched entry should load");
    };
    assert_eq!(hit.body_base64, encoded);
    assert_eq!(hit.headers.len(), 1);
    assert!(matches!(
        cache.load(unchanged).await,
        Some(CachedAsset::Unchanged)
    ));
    // 另一个补丁版本看不到这些条目。
    let other = PatchCache::new(temp.path().to_path_buf(), "other script");
    assert!(other.load(patched).await.is_none());

    let body = temp.path().join(cache.entry_name(patched).unwrap());
    std::fs::write(&body, "patch").unwrap();
    assert!(cache.load(patched).await.is_none());
}

#[test]
fn prune_drops_other_versions_and_least_recently_used_entries() {
    let temp = tempfile::tempdir().unwrap();
    let cache = PatchCache::new(temp.path().to_path_buf(), "script");
    std::fs::write(temp.path().join("stale-10000.90eb42395b7d.js"), "x").unwrap();
    let big = base64::engine::general_purpose::STANDARD.encode("x".repeat(300));
    for (index, name) in ["1.11111111.js", "2.22222222.js", "3.33333333.js"]
        .iter()
        .enumerate()
    {
        let entry = format!("{}-{name}", cache.tag);
        let patch = StoredPatch {
            status: 200,
            headers: Vec::new(),
            body_base64: big.clone(),
        };
        store_entry(temp.path(), &entry, Some(patch)).unwrap();
        let used = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + index as u64);
        std::fs::File::options()
            .append(true)
            .open(temp.path().join(format!("{entry}.meta.json")))
            .unwrap()
            .set_modified(used)
            .unwrap();
    }
    cache.prune_to(600, usize::MAX);
    let mut left: Vec<String> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let tag = &cache.tag;
    assert_eq!(
        left,
        [
            format!("{tag}-3.33333333.js"),
            format!("{tag}-3.33333333.js.meta.json"),
        ]
    );
}

#[test]
fn prune_preserves_in_flight_temp_files_for_current_tag_only() {
    let temp = tempfile::tempdir().unwrap();
    let cache = PatchCache::new(temp.path().to_path_buf(), "script");
    let tag = &cache.tag;
    let fresh_temp = format!(".{tag}-entry-11111111.js.codey-1234.tmp");
    let stale_temp = format!(".{tag}-stale-22222222.js.codey-5678.tmp");
    let other_tag_temp = ".other-entry-33333333.js.codey-9999.tmp";
    std::fs::write(temp.path().join(&fresh_temp), "fresh").unwrap();
    std::fs::write(temp.path().join(&stale_temp), "stale").unwrap();
    std::fs::write(temp.path().join(other_tag_temp), "other").unwrap();
    std::fs::File::options()
        .append(true)
        .open(temp.path().join(&stale_temp))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(100))
        .unwrap();

    cache.prune();

    let left: Vec<String> = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, [fresh_temp]);
}

#[test]
fn rewritten_assets_drop_stale_framing_headers() {
    let headers = json!([
        { "name": "Content-Type", "value": "text/javascript" },
        { "name": "content-length", "value": "1234" },
        { "name": "ETag", "value": "\"abc\"" },
        { "name": "Content-Encoding", "value": "br" },
        { "value": "no-name-is-dropped" },
    ]);
    let kept = fresh_response_headers(headers.as_array().expect("头是数组"));
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["name"], "Content-Type");
}
