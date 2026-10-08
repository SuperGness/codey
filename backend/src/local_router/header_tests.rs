use super::tests::router_config;
use super::*;

#[test]
fn header_configuration_rejects_invalid_ambiguous_and_oversized_values() {
    let (config, _, _) = router_config("https://example.com/v1".into());
    for (name, value) in [
        ("bad name", "value"),
        ("x-test", "\r\nprivate"),
        ("x-test", "\0"),
        ("x-test", " \t"),
        ("Host", "example.com"),
        ("content-length", "42"),
        ("Content-Encoding", "gzip"),
        ("Content-Type", "text/plain"),
        ("content-type", ""),
        ("Accept", ""),
        ("Proxy-Authorization", "private"),
        (ROUTER_AUTH_HEADER, "private"),
        ("sec-websocket-key", "private"),
    ] {
        let mut profile = config.profiles[0].clone();
        profile
            .model_request_headers
            .insert(name.into(), value.into());
        let error = profile.validate().unwrap_err();
        assert!(!error.contains("private"));
    }
    let mut profile = config.profiles[0].clone();
    profile.model_request_headers =
        BTreeMap::from([("X-Test".into(), "a".into()), ("x-test".into(), "b".into())]);
    assert!(profile.validate().unwrap_err().contains("重复"));
    profile.model_request_headers = BTreeMap::from([("x-test".into(), "x".repeat(8193))]);
    assert!(profile.validate().is_err());
    profile.model_request_headers = (0..129).map(|i| (format!("x-{i}"), "a".into())).collect();
    assert!(profile.validate().is_err());
    profile.model_request_headers = (0..5)
        .map(|i| (format!("x-{i}"), "x".repeat(8192)))
        .collect();
    assert!(profile.validate().is_err());
    profile.model_request_headers = BTreeMap::from([("x-test".into(), "a\tb".into())]);
    assert!(profile.validate().is_ok());
    profile.official_account = true;
    profile
        .model_request_headers
        .insert("Authorization".into(), "private".into());
    assert!(profile.validate().is_err());
}

#[tokio::test]
async fn header_deletions_and_content_type_survive_every_http_protocol_and_config_update() {
    for (protocol, path) in [
        (UPSTREAM_PROTOCOL_OPENAI_RESPONSES, "/responses"),
        (UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS, "/responses"),
        (UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES, "/responses"),
        (UPSTREAM_PROTOCOL_OPENAI_RESPONSES, "/images/generations"),
    ] {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = upstream.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for expected_agent in [None, Some("updated-agent")] {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                assert_eq!(incoming_header(&request, "user-agent"), expected_agent);
                for name in [
                    "originator",
                    PROMPT_CACHE_KEY_HEADER,
                    PROMPT_CACHE_KEY_COMPAT_HEADER,
                    "x-codey-request-id",
                    "x-stainless-os",
                ] {
                    assert!(incoming_header(&request, name).is_none(), "{name}");
                }
                let types = request
                    .headers
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                    .collect::<Vec<_>>();
                assert_eq!(types.len(), 1);
                assert_eq!(types[0].1, "application/json");
                assert!(serde_json::from_slice::<Value>(&request.body).is_ok());
                write_json_response(&mut stream, 500, &json!({"error":{"message":"test"}}))
                    .await
                    .unwrap();
            }
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.default_model = model_alias(&provider, &model);
        config.profiles[0].upstream_protocol = protocol.into();
        // x-codey-request-id 不配置删除覆盖：Codey 内部请求 ID 默认就不得发往上游。
        config.profiles[0].model_request_headers = BTreeMap::from([
            ("user-agent".into(), String::new()),
            ("originator".into(), String::new()),
            (PROMPT_CACHE_KEY_HEADER.into(), String::new()),
            ("Content-Type".into(), "application/json".into()),
        ]);
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for updated in [false, true] {
            if updated {
                config.profiles[0]
                    .model_request_headers
                    .insert("user-agent".into(), "updated-agent".into());
                router.update_config(&config);
            }
            let response = client.post(format!("{}{path}", endpoint.base_url))
                .bearer_auth(&endpoint.token).header("user-agent", "incoming")
                .header("originator", "incoming").header("connection", "x-stainless-os")
                .header("x-stainless-os", "must-not-forward")
                .json(&json!({"model":model_alias(&provider, &model),"input":"hello","prompt":"hello","stream":false}))
                .send().await.unwrap();
            let status = response.status().as_u16();
            let body = response.text().await.unwrap();
            assert_eq!(status, 500, "{protocol} {path}: {body}");
        }
        server.await.unwrap();
        router.stop().await.unwrap();
    }
}

#[test]
fn header_logs_hide_credentials_but_keep_other_values() {
    let mut headers = HeaderMap::new();
    for name in [
        "authorization",
        "proxy-authorization",
        "cookie",
        "x-api-key",
        "x-oai-attestation",
        "x-codex-turn-state",
        "x-tenant",
    ] {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static("private-value"),
        );
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("x-client-request-id"),
        HeaderValue::from_static("request-123"),
    );
    let text = super::responses::format_upstream_headers(&headers);
    assert!(!text.contains("private-value"));
    assert!(text.contains("content-type: application/json"));
    assert!(text.contains("x-client-request-id: request-123"));
    assert_eq!(text.matches("[REDACTED]").count(), 7);
    let handshake = upstream_websocket_request("ws://example.com/responses", &headers).unwrap();
    let log = super::responses::format_upstream_headers(handshake.headers());
    assert!(log.contains(RESPONSES_WEBSOCKET_BETA));
    assert!(log.contains("sec-websocket-key: [REDACTED]"));
}

#[test]
fn route_specific_credential_headers_are_hidden_without_the_fixed_list() {
    // 线路可以自定义任意请求头，名单外的凭证头同样不能明文落盘。
    let mut headers = HeaderMap::new();
    for name in [
        "api-key",
        "x-goog-api-key",
        "x-auth-token",
        "x-amz-security-token",
    ] {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static("route-secret"),
        );
    }
    headers.insert(
        HeaderName::from_static("x-client-request-id"),
        HeaderValue::from_static("request-123"),
    );
    let text = super::responses::format_upstream_headers(&headers);
    assert!(!text.contains("route-secret"));
    assert_eq!(text.matches("[REDACTED]").count(), 4);
    assert!(text.contains("x-client-request-id: request-123"));

    // 粘性路由令牌与线路凭据在响应日志中都必须脱敏。
    let mut response = HeaderMap::new();
    response.insert(
        HeaderName::from_static("x-codex-turn-state"),
        HeaderValue::from_static("sticky-token"),
    );
    response.insert(
        HeaderName::from_static("x-goog-api-key"),
        HeaderValue::from_static("route-secret"),
    );
    let text = super::responses::format_upstream_response_headers(&response);
    assert!(text.contains("x-codex-turn-state: [REDACTED]"));
    assert!(!text.contains("sticky-token"));
    assert!(!text.contains("route-secret"));
    assert_eq!(text.matches("[REDACTED]").count(), 2);
}

#[test]
fn response_header_logs_hide_credentials_and_turn_state() {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("set-cookie"),
        HeaderValue::from_static("session=private-value"),
    );
    headers.insert(
        HeaderName::from_static("x-codex-turn-state"),
        HeaderValue::from_static("sticky-token"),
    );
    headers.insert(
        HeaderName::from_static("x-models-etag"),
        HeaderValue::from_static("etag-1"),
    );
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_static("Bearer private-value"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let text = super::responses::format_upstream_response_headers(&headers);
    assert!(!text.contains("private-value"));
    assert!(!text.contains("sticky-token"));
    assert!(text.contains("x-codex-turn-state: [REDACTED]"));
    assert!(text.contains("x-models-etag: etag-1"));
    assert!(text.contains("content-type: application/json"));
    assert!(text.contains("set-cookie: [REDACTED]"));
    assert!(text.contains("authorization: [REDACTED]"));
}

#[test]
fn codey_plugin_header_patches_preserve_authentication_and_support_removal() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer fixture"));
    headers.insert("x-plugin-demo", HeaderValue::from_static("old"));
    headers.insert("x-remove-demo", HeaderValue::from_static("remove"));
    super::lifecycle::apply_codey_plugin_header_patches(
        &mut headers,
        vec![
            crate::codey_plugins::HeaderPatch {
                name: "x-plugin-demo".into(),
                value: Some("new".into()),
            },
            crate::codey_plugins::HeaderPatch {
                name: "x-remove-demo".into(),
                value: None,
            },
        ],
    )
    .unwrap();
    assert_eq!(headers[AUTHORIZATION], "Bearer fixture");
    assert_eq!(headers["x-plugin-demo"], "new");
    assert!(!headers.contains_key("x-remove-demo"));
}

#[test]
fn codey_plugin_invalid_patch_does_not_partially_change_headers() {
    for (name, value) in [
        ("authorization", "Bearer changed"),
        ("x-plugin-other", "bad\r\nvalue"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-plugin-demo", HeaderValue::from_static("original"));
        let original = headers.clone();
        let error = super::lifecycle::apply_codey_plugin_header_patches(
            &mut headers,
            vec![
                crate::codey_plugins::HeaderPatch {
                    name: "x-plugin-demo".into(),
                    value: Some("changed".into()),
                },
                crate::codey_plugins::HeaderPatch {
                    name: name.into(),
                    value: Some(value.into()),
                },
            ],
        )
        .unwrap_err();
        assert_eq!(error.code, "plugin_invalid_headers");
        assert_eq!(headers, original);
    }
    let mut headers = HeaderMap::new();
    headers.insert("x-plugin-demo", HeaderValue::from_static("original"));
    super::lifecycle::apply_codey_plugin_header_patches(
        &mut headers,
        vec![crate::codey_plugins::HeaderPatch {
            name: "x-plugin-demo".into(),
            value: Some("café".into()),
        }],
    )
    .unwrap();
    assert_eq!(headers["x-plugin-demo"], "café");
}
