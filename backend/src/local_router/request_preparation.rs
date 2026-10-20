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
            Some(converted) => {
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
        let roots_changed =
            !route.official_account && normalize_responses_tool_parameter_roots(&mut prepared.body);
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
