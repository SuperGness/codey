use super::*;

pub(super) struct RequestPreparationFailure {
    pub(super) status: u16,
    pub(super) code: &'static str,
    pub(super) message: String,
}

/// 协议转换结果包含响应还原所需的工具映射，不再依赖转换前的请求树。
pub(super) struct PreparedProtocolRequest {
    pub(super) body: Value,
    pub(super) tool_bridge: ResponsesToolBridge,
    pub(super) chat_reasoning_summaries: Vec<Option<String>>,
    pub(super) anthropic_context_1m: bool,
}

impl PreparedProtocolRequest {
    pub(super) async fn prepare(
        body: Value,
        bridge: ProtocolBridge,
        route: &RouteTarget,
        upstream_url: &str,
        encoded_body: &mut Option<Vec<u8>>,
        body_mutated: &mut bool,
        admission: &mut Option<OwnedSemaphorePermit>,
    ) -> std::result::Result<Self, RequestPreparationFailure> {
        let offload = bridge != ProtocolBridge::NativeResponses
            && encoded_body
                .as_ref()
                .is_some_and(|body| body.len() >= REQUEST_JSON_OFFLOAD_BYTES);
        let (body, converted) = if offload {
            // 阻塞任务持有工作集名额；外层取消也不能提前释放仍在转换的内存。
            let permit = admission.take();
            let encoded = encoded_body.take();
            let (body, converted, encoded, permit) = tokio::task::spawn_blocking(move || {
                let converted = bridge.convert_responses_body(&body);
                (body, converted, encoded, permit)
            })
            .await
            .map_err(|error| RequestPreparationFailure {
                status: 500,
                code: "request_conversion_failed",
                message: format!("等待 Responses 协议转换任务失败：{error}"),
            })?;
            *encoded_body = encoded;
            *admission = permit;
            (body, converted)
        } else {
            let converted = bridge.convert_responses_body(&body);
            (body, converted)
        };
        let converted = converted.map_err(|error| RequestPreparationFailure {
            status: 400,
            code: "unsupported_responses_payload",
            message: format!(
                "线路「{}」选择了 {}，但当前请求无法转换：{error:#}",
                route_display_name(route),
                bridge.upstream_protocol().label()
            ),
        })?;
        let mut prepared = match converted {
            Some(mut converted) => {
                // 仅恢复当前请求已选、且本线路声明或手动启用的档位。
                // ID-only 线路的兼容 Fast 不代表上游接受 service_tier。
                if let Some(tier) = selected_adapted_service_tier(&body, route) {
                    converted.body["service_tier"] = Value::String(tier.to_string());
                }
                drop(body);
                Self {
                    body: converted.body,
                    tool_bridge: converted.tool_bridge,
                    chat_reasoning_summaries: converted.chat_reasoning_summaries,
                    anthropic_context_1m: false,
                }
            }
            None => Self {
                body,
                tool_bridge: ResponsesToolBridge::default(),
                chat_reasoning_summaries: Vec::new(),
                anthropic_context_1m: false,
            },
        };
        let roots_changed = bridge != ProtocolBridge::NativeResponses
            && !route.official_account
            && normalize_responses_tool_parameter_roots(&mut prepared.body);
        // Moonshot/Kimi 不接受 $ref 的同层关键字，改为 allOf 保持 schema 语义。
        let schema_changed = bridge == ProtocolBridge::ResponsesToChatCompletions
            && upstream_rejects_ref_sibling_keywords(upstream_url)
            && wrap_chat_tool_ref_siblings(&mut prepared.body);
        prepared.anthropic_context_1m = bridge == ProtocolBridge::ResponsesToAnthropicMessages
            && strip_anthropic_context_1m_model_field(&mut prepared.body);
        if roots_changed || schema_changed || prepared.anthropic_context_1m {
            *body_mutated = true;
            *encoded_body = None;
        }
        Ok(prepared)
    }
}

fn selected_adapted_service_tier<'a>(body: &'a Value, route: &RouteTarget) -> Option<&'a str> {
    let tier = body.get("service_tier")?.as_str()?;
    let key = model_id::key(body.get("model")?.as_str()?);
    if tier == "ultrafast"
        && let Some(enabled) = route.model_ultrafast.get(&key)
    {
        return enabled.then_some(tier);
    }
    let declared = route.model_speed_tiers.get(&key)?;
    // Codex 的 Fast 选择在请求里是 priority，目录中的速度 ID 则是 fast。
    let speed = if tier == "priority" { "fast" } else { tier };
    (declared
        .service_tiers
        .iter()
        .any(|entry| entry["id"] == tier)
        || declared
            .additional_speed_tiers
            .iter()
            .any(|id| id == tier || id == speed)
        || declared.default_service_tier.as_deref() == Some(tier))
    .then_some(tier)
}

pub(super) struct RequestEncoding<'a> {
    pub(super) bridge: ProtocolBridge,
    pub(super) requested_model: &'a str,
    pub(super) upstream_model: &'a str,
    pub(super) body_mutated: bool,
}

impl RequestEncoding<'_> {
    pub(super) async fn encode(
        &self,
        request: &mut HttpRequest,
        body: Value,
        encoded_body: Option<Vec<u8>>,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<EncodedHttpRequest> {
        let native = self.bridge == ProtocolBridge::NativeResponses;
        let summary = native.then(|| RequestBodySummary::from_responses_body(&body, None));
        // 仍有效的原始 JSON 保留大字段字节，只重写必要的顶层字段。
        let encoded = match encoded_body.filter(|_| native) {
            Some(encoded)
                if should_passthrough_native_responses(
                    self.bridge,
                    self.requested_model,
                    self.upstream_model,
                    self.body_mutated,
                ) =>
            {
                drop(body);
                encoded
            }
            Some(encoded) => {
                let (encoded, permit) = rewrite_native_responses_encoded_body_offloaded(
                    encoded,
                    &body,
                    request._body_budget_permit.take(),
                )
                .await?;
                drop(body);
                request._body_budget_permit = permit;
                encoded
            }
            None => {
                serialize_upstream_body(
                    body,
                    &mut request._body_budget_permit,
                    if native {
                        "序列化 Responses 上游请求失败"
                    } else {
                        "序列化转换后的上游请求失败"
                    },
                )
                .await?
            }
        };
        if let (Some(probe), Some(mut summary)) = (probe, summary) {
            summary.bytes = Some(encoded.len() as u64);
            probe.record_upstream_body(summary);
        }
        drop(std::mem::take(&mut request.body));
        let mut admission = request._body_budget_permit.take();
        retain_compact_request_budget(&mut admission, encoded.len());
        Ok(EncodedHttpRequest {
            body: encoded.into(),
            admission,
        })
    }
}

/// 大请求的序列化与解析、转换一样放到阻塞线程；阻塞任务持有工作集名额，
/// 外层取消也不会提前释放仍在序列化的内存。
async fn serialize_upstream_body(
    body: Value,
    permit: &mut Option<OwnedSemaphorePermit>,
    context: &'static str,
) -> Result<Vec<u8>> {
    if !request_budget_covers(permit.as_ref(), REQUEST_JSON_OFFLOAD_BYTES) {
        return serde_json::to_vec(&body).context(context);
    }
    let held = permit.take();
    let (encoded, held) = tokio::task::spawn_blocking(move || {
        let encoded = serde_json::to_vec(&body);
        drop(body);
        (encoded, held)
    })
    .await
    .context("等待大型上游请求序列化任务失败")?;
    *permit = held;
    encoded.context(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_router::tests::router_config;

    const MODEL: &str = "Org/Future-V1";
    const ADAPTED_PROTOCOLS: [&str; 2] = [
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ];

    fn speed_config(protocol: &str) -> (CodeyConfig, String) {
        let (mut config, provider, _) = router_config("http://127.0.0.1:9/v1".into());
        config.profiles[0].upstream_protocol = protocol.into();
        config
            .selected_models_by_provider
            .insert(provider.clone(), vec![MODEL.into(), "Org/Other-V1".into()]);
        (config, provider)
    }

    async fn prepared_wire_body(
        route: &RouteTarget,
        model: &str,
        tier: Option<Value>,
        offload: bool,
    ) -> Value {
        let input = if offload {
            "x".repeat(REQUEST_JSON_OFFLOAD_BYTES)
        } else {
            "hello".to_string()
        };
        let mut body = json!({"model": model, "input": input, "stream": false});
        if let Some(tier) = tier {
            body["service_tier"] = tier;
        }
        let mut request = HttpRequest {
            method: "POST".into(),
            path: "/v1/responses".into(),
            headers: Vec::new(),
            body: serde_json::to_vec(&body).unwrap(),
            _body_budget_permit: None,
        };
        let mut encoded = Some(request.body.clone());
        let mut mutated = false;
        let bridge = ProtocolBridge::from_upstream_protocol(route.protocol);
        let prepared = PreparedProtocolRequest::prepare(
            body,
            bridge,
            route,
            route.upstream_url.as_ref().unwrap(),
            &mut encoded,
            &mut mutated,
            &mut request._body_budget_permit,
        )
        .await
        .unwrap_or_else(|error| panic!("{}: {}", error.code, error.message));
        let encoded = RequestEncoding {
            bridge,
            requested_model: model,
            upstream_model: model,
            body_mutated: mutated,
        }
        .encode(&mut request, prepared.body, encoded, None)
        .await
        .unwrap();
        let body: Value = serde_json::from_slice(&encoded.body).unwrap();
        assert_eq!(body["model"], model);
        assert!(body["messages"].is_array());
        body
    }

    #[tokio::test]
    async fn adapted_service_tier_requires_the_requested_tier_to_be_declared() {
        let cases = [
            (
                json!({"service_tiers":[{"id":"ultrafast"}]}),
                Some(json!("ultrafast")),
                Some("ultrafast"),
            ),
            (
                json!({"additional_speed_tiers":["ultrafast"]}),
                Some(json!("ultrafast")),
                Some("ultrafast"),
            ),
            (
                json!({"service_tiers":[{"id":"priority"}]}),
                Some(json!("priority")),
                Some("priority"),
            ),
            (
                json!({"additional_speed_tiers":["fast"]}),
                Some(json!("priority")),
                Some("priority"),
            ),
            (
                json!({"service_tiers":[{"id":"Future-Tier"}]}),
                Some(json!("Future-Tier")),
                Some("Future-Tier"),
            ),
            (
                json!({"additional_speed_tiers":["Future-Tier"]}),
                Some(json!("Future-Tier")),
                Some("Future-Tier"),
            ),
            (
                json!({"default_service_tier":"Future-Tier"}),
                Some(json!("Future-Tier")),
                Some("Future-Tier"),
            ),
            (json!({"default_service_tier":"ultrafast"}), None, None),
            (
                json!({"service_tiers":[{"id":"ultrafast"}]}),
                Some(Value::Null),
                None,
            ),
            (
                json!({"service_tiers":[{"id":"ultrafast"}]}),
                Some(json!(7)),
                None,
            ),
            (
                json!({"service_tiers":[{"id":"ultrafast"}]}),
                Some(json!("priority")),
                None,
            ),
            (
                json!({"service_tiers":[{"id":"default", "description":"10x speed"}]}),
                Some(json!("ultrafast")),
                None,
            ),
            (json!({}), Some(json!("priority")), None),
        ];
        for protocol in ADAPTED_PROTOCOLS {
            for (declared, selected, expected) in &cases {
                let (mut config, provider) = speed_config(protocol);
                config.upstream_speed_tiers_by_provider.insert(
                    provider.clone(),
                    BTreeMap::from([(
                        "org/future-v1".into(),
                        serde_json::from_value(declared.clone()).unwrap(),
                    )]),
                );
                let snapshot = RouterSnapshot::from_config(&config);
                let body =
                    prepared_wire_body(&snapshot.routes[&provider], MODEL, selected.clone(), false)
                        .await;
                assert_eq!(
                    body.get("service_tier").and_then(Value::as_str),
                    *expected,
                    "{protocol}: declaration={declared}, selection={selected:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn adapted_service_tiers_remain_scoped_to_route_model_and_snapshot() {
        for protocol in ADAPTED_PROTOCOLS {
            let (mut config, provider) = speed_config(protocol);
            let mut second = config.profiles[0].clone();
            second.id = "route-b".into();
            config.profiles.push(second);
            config
                .selected_models_by_provider
                .insert("route-b".into(), vec![MODEL.into()]);
            config.upstream_speed_tiers_by_provider.insert(
                provider.clone(),
                BTreeMap::from([(
                    "org/future-v1".into(),
                    serde_json::from_value(json!({
                        "additional_speed_tiers": ["ultrafast"]
                    }))
                    .unwrap(),
                )]),
            );
            let snapshot = RouterSnapshot::from_config(&config);
            config.upstream_speed_tiers_by_provider.clear();
            let updated = RouterSnapshot::from_config(&config);
            for (route, model, expected) in [
                (&snapshot.routes[&provider], MODEL, Some("ultrafast")),
                (&snapshot.routes[&provider], "Org/Other-V1", None),
                (&snapshot.routes["route-b"], MODEL, None),
                (&updated.routes[&provider], MODEL, None),
            ] {
                let body = prepared_wire_body(route, model, Some(json!("ultrafast")), true).await;
                assert_eq!(body.get("service_tier").and_then(Value::as_str), expected);
            }
        }
    }

    #[tokio::test]
    async fn adapted_manual_ultrafast_only_forwards_an_explicit_ultrafast_selection() {
        for protocol in ADAPTED_PROTOCOLS {
            for enabled in [false, true] {
                let (mut config, provider) = speed_config(protocol);
                config.model_ultrafast_by_provider.insert(
                    provider.clone(),
                    BTreeMap::from([(model_id::key(MODEL), enabled)]),
                );
                if !enabled {
                    config.upstream_speed_tiers_by_provider.insert(
                        provider.clone(),
                        BTreeMap::from([(
                            model_id::key(MODEL),
                            serde_json::from_value(
                                json!({"additional_speed_tiers": ["ultrafast"]}),
                            )
                            .unwrap(),
                        )]),
                    );
                }
                let snapshot = RouterSnapshot::from_config(&config);
                for selection in [
                    None,
                    Some("priority"),
                    Some("ultrafast"),
                    Some("Future-Tier"),
                ] {
                    let body = prepared_wire_body(
                        &snapshot.routes[&provider],
                        MODEL,
                        selection.map(|tier| json!(tier)),
                        false,
                    )
                    .await;
                    let expected = selection.filter(|tier| enabled && *tier == "ultrafast");
                    assert_eq!(body.get("service_tier").and_then(Value::as_str), expected);
                }
                let other = prepared_wire_body(
                    &snapshot.routes[&provider],
                    "Org/Other-V1",
                    Some(json!("ultrafast")),
                    false,
                )
                .await;
                assert!(other.get("service_tier").is_none());
            }
        }
    }
}
