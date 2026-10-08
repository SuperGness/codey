use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn model_settings_follow_the_applied_catalog_and_declared_efforts() {
    let catalog = json!({"status":"ok","models":["route/model"],"model_metadata":[{"model":"route/model","supported_reasoning_efforts":["low","high"]}]});
    let desktop = json!({"latestModel":"old","latestThreadSettings":{"model":"route/model"}});
    assert!(validate_model_settings(&catalog, &desktop, &json!({"effort":"high"})).is_ok());
    assert!(
        validate_model_settings(
            &catalog,
            &desktop,
            &json!({"model":"route/model","effort":"low"})
        )
        .is_ok()
    );
    assert!(validate_model_settings(&catalog, &desktop, &json!({"model":"removed"})).is_err());
    assert!(validate_model_settings(&catalog, &desktop, &json!({"effort":"ultra"})).is_err());
    assert!(
        validate_model_settings(
            &json!({"status":"failed"}),
            &desktop,
            &json!({"model":"route/model"})
        )
        .is_err()
    );
    let mut disabled = catalog;
    disabled["clear_models"] = json!(true);
    assert!(validate_model_settings(&disabled, &desktop, &json!({"model":"route/model"})).is_err());
}

async fn fixture() -> (Arc<Core>, String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let core = Arc::new(Core {
        auth: Mutex::new(Auth::default()),
        state: Weak::new(),
        public_url: RwLock::new(None),
        address,
        actions: AsyncMutex::new(HashMap::new()),
        shutdown: tokio::sync::watch::channel(false).0,
        streams: Arc::new(Semaphore::new(8)),
    });
    let task = tokio::spawn(serve(listener, Arc::clone(&core)));
    (core, format!("http://{address}"), task)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn pair(core: &Core, base: &str) -> String {
    let code = core.auth.lock().unwrap().pairing_code();
    let response = client()
        .post(format!("{base}/remote/pair"))
        .header("origin", base)
        .json(&json!({"code":code,"name":"测试手机"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(!response.text().await.unwrap().contains(&code));
    cookie.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn requires_pairing_rejects_cross_site_and_revokes_existing_sessions() {
    let (core, base, task) = fixture().await;
    let client = client();
    let response = client
        .get(format!("{base}/remote/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let response = client
        .post(format!("{base}/remote/pair"))
        .header("origin", "https://evil.example")
        .json(&json!({"code":"bad"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let cookie = pair(&core, &base).await;
    assert_eq!(
        client
            .get(format!("{base}/remote/session"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let id = core.auth.lock().unwrap().devices()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    core.auth.lock().unwrap().revoke(&id);
    assert_eq!(
        client
            .get(format!("{base}/remote/session"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

#[tokio::test]
async fn composer_reads_require_pairing_and_handle_a_stopped_host() {
    let (core, base, task) = fixture().await;
    let client = client();
    let cookie = pair(&core, &base).await;
    for path in ["/remote/models", "/remote/defaults"] {
        let endpoint = format!("{base}{path}");
        assert_eq!(
            client
                .post(&endpoint)
                .header("origin", &base)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .post(&endpoint)
                .header("origin", "https://evil.example")
                .header("cookie", &cookie)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = client
            .post(&endpoint)
            .header("origin", &base)
            .header("cookie", &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    assert!(core.actions.lock().await.is_empty());
    task.abort();
}

#[tokio::test]
async fn rejects_dns_rebinding_and_remote_pairing_management() {
    let (core, base, task) = fixture().await;
    let client = client();
    let cookie = pair(&core, &base).await;
    let response = client
        .get(format!("{base}/remote/session"))
        .header("cookie", &cookie)
        .header("host", "attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    for command in [
        "start_remote_control",
        "pair_remote_control",
        "revoke_remote_device",
        "unknown",
    ] {
        let response = client
            .post(format!("{base}/api/{command}"))
            .header("cookie", &cookie)
            .header("origin", &base)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        client
            .post(format!("{base}/remote/logout"))
            .header("cookie", &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn handles_malformed_oversized_and_chunked_bodies_without_dispatch() {
    let (core, base, task) = fixture().await;
    let cookie = pair(&core, &base).await;
    let response = client()
        .post(format!("{base}/remote/action"))
        .header("origin", &base)
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{".repeat(MAX_MESSAGE_BODY + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(core.actions.lock().await.is_empty());
    for body in [json!(null), json!([]), json!("invalid")] {
        let response = client()
            .post(format!("{base}/remote/action"))
            .header("origin", &base)
            .header("cookie", &cookie)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let mut stream = tokio::net::TcpStream::connect(core.address).await.unwrap();
    let request = format!(
        "POST /remote/action HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nCookie: {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\n{{\r\n0\r\n\r\n",
        core.address, base, cookie
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 400"));
    task.abort();
}

#[tokio::test]
async fn repeated_action_ids_never_replay_and_cannot_change_content() {
    let (core, base, task) = fixture().await;
    let cookie = pair(&core, &base).await;
    let token = cookie.split_once('=').unwrap().1;
    let id = uuid::Uuid::new_v4().to_string();
    let args = json!({"requestId":id,"threadId":"invalid","action":"send","text":"test"});
    let first = action_once(&core, token, "/remote/action", &args)
        .await
        .unwrap();
    assert_eq!(first["status"], "failed");
    assert_eq!(
        action_once(&core, token, "/remote/action", &args)
            .await
            .unwrap(),
        first
    );
    let mut changed = args.clone();
    changed["text"] = json!("changed");
    assert!(
        action_once(&core, token, "/remote/action", &changed)
            .await
            .is_err()
    );
    assert_eq!(core.actions.lock().await.len(), 1);
    task.abort();
}

#[tokio::test]
async fn photo_body_budget_only_applies_to_authenticated_message_endpoints() {
    let (core, base, task) = fixture().await;
    let cookie = pair(&core, &base).await;
    let args = json!({"requestId":uuid::Uuid::new_v4().to_string(),"text":"","images":[{"url":"a".repeat(MAX_BODY)}]});
    let client = client();
    for path in ["/remote/create", "/remote/pair", "/api/load_codey_config"] {
        let response = client
            .post(format!("{base}{path}"))
            .header("origin", &base)
            .header("cookie", &cookie)
            .json(&args)
            .send()
            .await
            .unwrap();
        let result: Value = response.json().await.unwrap();
        assert_eq!(
            result["message"],
            if path == "/remote/create" {
                "Codey 正在退出"
            } else {
                "远程请求过大"
            }
        );
    }
    assert_eq!(core.actions.lock().await.len(), 1);
    let response = client
        .post(format!("{base}/remote/create"))
        .header("origin", &base)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    task.abort();
}

#[test]
fn https_origins_are_exact_and_configuration_secrets_are_redacted() {
    for url in [
        "http://example.com",
        "https://u:p@example.com",
        "https://example.com/path",
        "https://example.com/?x=1",
    ] {
        assert!(validate_public_url(url).is_err());
    }
    assert_eq!(
        validate_public_url("https://example.com/")
            .unwrap()
            .as_deref(),
        Some("https://example.com")
    );
    let value = redact_panel_config(
        json!({"config":{"profiles":[{"apiKey":"secret","apiKeyConfigured":true,"modelRequestHeaders":{"Authorization":"secret"}}],"promptOptimization":{"apiKey":"secret"},"webhook":{"channels":[{"botToken":"secret","url":"secret"}]}}}),
    );
    assert!(!value.to_string().contains("secret"));
    assert_eq!(value["config"]["profiles"][0]["apiKeyConfigured"], true);
    assert!(panel_command("save_codey_config"));
    assert!(!panel_command("remote_control_status"));
    for malformed in [
        json!({"config":"plugin setting"}),
        json!({"config":{"profiles":[1, null],"webhook":[],"promptOptimization":[]}}),
    ] {
        assert_eq!(redact_panel_config(malformed.clone()), malformed);
    }
}

#[test]
fn saving_remote_forms_preserves_hidden_headers_and_supports_replacement() {
    let mut config = crate::config::CodeyConfig::default();
    let mut profile = crate::config::ProviderProfile::new("test");
    profile.id = "saved".into();
    profile
        .model_request_headers
        .insert("x-api-key".into(), "private".into());
    config.profiles = vec![profile];
    let hidden = redact_panel_config(json!({"config":config}));
    assert!(
        hidden["config"]["profiles"][0]["modelRequestHeaders"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let restored = restore_panel_headers(&config, "save_codey_config", hidden);
    assert_eq!(
        restored["config"]["profiles"][0]["modelRequestHeaders"]["x-api-key"],
        "private"
    );
    let replacement = json!({"config":{"profiles":[{"id":"saved","modelRequestHeaders":{"x-new":"replacement"}}]}});
    assert_eq!(
        restore_panel_headers(&config, "save_codey_config", replacement.clone()),
        replacement
    );
    for malformed in [
        json!([]),
        json!({"config": []}),
        json!({"config":{"profiles":[null, [], 1]}}),
    ] {
        assert_eq!(
            restore_panel_headers(&config, "save_codey_config", malformed.clone()),
            malformed
        );
    }
}

#[tokio::test]
#[ignore = "requires a running Codex desktop; authenticated websocket reads only"]
async fn live_remote_websocket_smoke() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let (core, base, task) = fixture().await;
    let cookie = pair(&core, &base).await;
    let rows = super::super::store::threads(crate::codex_config::codex_home(), "", false).unwrap();
    let thread = rows[0]["id"].as_str().unwrap();
    let mut request = format!("ws://{}/remote/events/{thread}", core.address)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", base.parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let (mut socket, reply) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(reply.status(), StatusCode::SWITCHING_PROTOCOLS);
    let frame = tokio::time::timeout(Duration::from_secs(12), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert_eq!(value["type"], "state");
    assert_eq!(value["state"]["id"], thread);
    let id = core.auth.lock().unwrap().devices()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    core.auth.lock().unwrap().revoke(&id);
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(frame)) = socket.next().await {
            if frame.is_close() {
                return;
            }
            if frame.is_text()
                && serde_json::from_str::<Value>(frame.to_text().unwrap()).unwrap()["type"]
                    == "disconnected"
            {
                return;
            }
        }
    })
    .await
    .expect("revocation must close an existing websocket");
    core.shutdown.send_replace(true);
    task.abort();
    println!("Authenticated live WebSocket snapshot + device revocation verified");
}

#[tokio::test]
async fn websocket_upgrades_and_checks_shutdown_before_desktop_connection() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let (core, base, task) = fixture().await;
    let cookie = pair(&core, &base).await;
    // A stopped service must never contact a real desktop. This also verifies
    // HTTP upgrade framing without any dependency on a locally running Codex.
    core.shutdown.send_replace(true);
    let mut request = format!(
        "ws://{}/remote/events/{}",
        core.address,
        uuid::Uuid::new_v4()
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("origin", base.parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let (mut socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap()["type"],
        "disconnected"
    );
    task.abort();
}

#[tokio::test]
async fn websocket_requires_authentication_origin_and_valid_upgrade() {
    let (core, base, task) = fixture().await;
    let endpoint = format!("{base}/remote/events/{}", uuid::Uuid::new_v4());
    let client = client();
    assert_eq!(
        client.get(&endpoint).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = pair(&core, &base).await;
    assert_eq!(
        client
            .get(&endpoint)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .get(&endpoint)
            .header("cookie", &cookie)
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let permits = Arc::clone(&core.streams)
        .acquire_many_owned(8)
        .await
        .unwrap();
    let response = client
        .get(&endpoint)
        .header("cookie", &cookie)
        .header("origin", &base)
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(permits);
    task.abort();
}

#[tokio::test]
#[ignore = "downloads the official helper and opens a temporary authenticated test endpoint"]
async fn live_quick_tunnel_smoke() {
    let (core, base, task) = fixture().await;
    struct ListenerGuard(tokio::task::JoinHandle<()>);
    impl Drop for ListenerGuard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _listener = ListenerGuard(task);
    let tunnel = super::super::tunnel::Tunnel::start(Arc::clone(&core));
    let url = tokio::time::timeout(Duration::from_secs(250), async {
        loop {
            let status = tunnel.status();
            if status["status"] == "failed" {
                panic!("{}", status["message"]);
            }
            if let Some(url) = status["url"].as_str() {
                break url.to_string();
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("tunnel preparation timed out");
    let external = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let mut response = None;
    for _ in 0..6 {
        match external.get(format!("{url}/remote/session")).send().await {
            Ok(result) if result.status() == StatusCode::UNAUTHORIZED => {
                response = Some(result);
                break;
            }
            Ok(result) => eprintln!("HTTPS probe status: {}", result.status()),
            Err(error) => eprintln!("HTTPS probe failed: {}", error.without_url()),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(response.is_some(), "public endpoint must enforce pairing");
    let code = core.auth.lock().unwrap().pairing_code();
    let paired = external
        .post(format!("{url}/remote/pair"))
        .header("origin", &url)
        .json(&json!({"code":code,"name":"temporary tunnel test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(paired.status(), StatusCode::OK);
    assert!(
        paired.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Secure")
    );
    let cookie = paired.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_eq!(
        external
            .get(format!("{url}/remote/session"))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client()
            .get(format!("{base}/remote/session"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    println!(
        "Quick Tunnel HTTPS + pairing + Secure cookie verified; temporary endpoint shutting down"
    );
}
