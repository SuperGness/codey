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
        config.profiles[0].model_request_headers = BTreeMap::from([
            ("user-agent".into(), String::new()),
            ("originator".into(), String::new()),
            (PROMPT_CACHE_KEY_HEADER.into(), String::new()),
            ("x-codey-request-id".into(), String::new()),
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
    assert_eq!(text.matches("[REDACTED]").count(), 6);
    let handshake = upstream_websocket_request("ws://example.com/responses", &headers).unwrap();
    let log = super::responses::format_upstream_headers(handshake.headers());
    assert!(log.contains(RESPONSES_WEBSOCKET_BETA));
    assert!(log.contains("sec-websocket-key: [REDACTED]"));
}
