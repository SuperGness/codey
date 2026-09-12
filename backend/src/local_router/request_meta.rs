use super::*;

pub(crate) fn should_force_upstream_streaming(
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
    downstream_websocket: bool,
    stream_requested: bool,
) -> bool {
    request_kind == ResponsesRequestKind::Create
        && !downstream_websocket
        && !stream_requested
        && bridge.can_collect_streamed_response()
}

pub(crate) fn request_binding_keys(request: &HttpRequest) -> Vec<String> {
    ["thread-id", "session-id"]
        .into_iter()
        .filter_map(|header_name| {
            let value = incoming_header(request, header_name)?.trim();
            (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
                .then(|| format!("{header_name}:{value}"))
        })
        .collect()
}

pub(crate) fn request_log_codex_session(request: &HttpRequest) -> (Option<&str>, bool) {
    if let Some(parent_thread_id) = incoming_header(request, "x-codex-parent-thread-id") {
        return (valid_codex_session_id(parent_thread_id), true);
    }
    if incoming_header(request, "x-openai-subagent").is_some() {
        return (None, true);
    }
    (
        ["thread-id", "session-id"]
            .into_iter()
            .find_map(|header_name| incoming_header(request, header_name))
            .and_then(valid_codex_session_id),
        false,
    )
}

pub(crate) fn valid_codex_session_id(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then_some(value)
}

pub(crate) fn take_codey_route_metadata(
    request: &mut HttpRequest,
    body: &mut Value,
) -> Result<(Option<String>, bool)> {
    let mut route_hint = None;
    let mut body_mutated = false;
    for (name, value) in &mut request.headers {
        if !name.eq_ignore_ascii_case(TURN_METADATA_HEADER) {
            continue;
        }
        let Ok(mut metadata) = serde_json::from_str::<Value>(value) else {
            continue;
        };
        let extracted = take_route_hint_from_metadata_value(&mut metadata)?;
        // 只在真正删除了 Codey 字段时才重新序列化；否则保留客户端的
        // 原始字节（重序列化会改变键顺序，造成与官方客户端不一致的线上字节）。
        if extracted.is_some() {
            *value = serde_json::to_string(&metadata)
                .context("序列化清理后的 Codex turn metadata 失败")?;
        }
        merge_route_hint(&mut route_hint, extracted)?;
    }

    let Some(client_metadata) = body
        .as_object_mut()
        .and_then(|body| body.get_mut("client_metadata"))
        .and_then(Value::as_object_mut)
    else {
        return Ok((route_hint, false));
    };
    let direct = client_metadata.remove(ROUTE_METADATA_KEY);
    if direct.is_some() {
        body_mutated = true;
    }
    merge_route_hint(
        &mut route_hint,
        direct
            .as_ref()
            .map(validated_route_hint_value)
            .transpose()?,
    )?;
    if let Some(metadata) = client_metadata.get_mut(TURN_METADATA_HEADER) {
        let extracted = match metadata {
            Value::String(serialized) => {
                let Ok(mut parsed) = serde_json::from_str::<Value>(serialized) else {
                    return Ok((route_hint, body_mutated));
                };
                let extracted = take_route_hint_from_metadata_value(&mut parsed)?;
                if extracted.is_some() {
                    *serialized = serde_json::to_string(&parsed)
                        .context("序列化清理后的 Responses client metadata 失败")?;
                    body_mutated = true;
                }
                extracted
            }
            Value::Object(_) => {
                let extracted = take_route_hint_from_metadata_value(metadata)?;
                if extracted.is_some() {
                    body_mutated = true;
                }
                extracted
            }
            _ => None,
        };
        merge_route_hint(&mut route_hint, extracted)?;
    }
    Ok((route_hint, body_mutated))
}

pub(crate) fn should_passthrough_native_responses(
    bridge: ProtocolBridge,
    requested_model: &str,
    upstream_model: &str,
    body_mutated: bool,
) -> bool {
    matches!(bridge, ProtocolBridge::NativeResponses)
        && requested_model == upstream_model
        && !body_mutated
}

pub(crate) fn has_codey_synthetic_previous_response_id(body: &Value) -> bool {
    body.get("previous_response_id")
        .and_then(Value::as_str)
        .is_some_and(is_codey_synthetic_response_id)
}

pub(crate) fn is_codey_synthetic_response_id(response_id: &str) -> bool {
    response_id.trim().starts_with("resp_codey_")
}

pub(crate) struct RawTopLevelObject<'a>(Vec<(String, &'a RawValue)>);

pub(crate) struct RawTopLevelObjectVisitor;

impl<'de> Visitor<'de> for RawTopLevelObjectVisitor {
    type Value = RawTopLevelObject<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut fields = Vec::with_capacity(map.size_hint().unwrap_or_default());
        while let Some(field) = map.next_entry::<String, &'de RawValue>()? {
            fields.push(field);
        }
        Ok(RawTopLevelObject(fields))
    }
}

impl<'de> Deserialize<'de> for RawTopLevelObject<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawTopLevelObjectVisitor)
    }
}

pub(crate) fn begin_encoded_json_field(
    output: &mut Vec<u8>,
    first: &mut bool,
    name: &str,
) -> Result<()> {
    if !*first {
        output.push(b',');
    }
    *first = false;
    serde_json::to_writer(&mut *output, name).context("序列化 Responses 请求字段名失败")?;
    output.push(b':');
    Ok(())
}

pub(crate) fn rewrite_native_responses_encoded_body(
    original: &[u8],
    updated: &Value,
) -> Result<Vec<u8>> {
    let RawTopLevelObject(fields) = serde_json::from_slice::<RawTopLevelObject>(original)
        .context("解析 Responses 原始请求字段失败")?;
    let updated = updated
        .as_object()
        .context("Responses 上游请求必须是 JSON 对象")?;
    let mut output = Vec::with_capacity(original.len());
    output.push(b'{');
    let mut first = true;
    let mut saw_model = false;
    let mut saw_client_metadata = false;
    let mut saw_previous_response_id = false;
    for (name, raw) in fields {
        let replacement = match name.as_str() {
            "model" => {
                saw_model = true;
                Some(updated.get("model"))
            }
            "client_metadata" => {
                saw_client_metadata = true;
                Some(updated.get("client_metadata"))
            }
            "previous_response_id" => {
                saw_previous_response_id = true;
                Some(updated.get("previous_response_id"))
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            let Some(replacement) = replacement else {
                continue;
            };
            begin_encoded_json_field(&mut output, &mut first, &name)?;
            serde_json::to_writer(&mut output, replacement)
                .context("序列化 Responses 已更新请求字段失败")?;
        } else {
            begin_encoded_json_field(&mut output, &mut first, &name)?;
            output.extend_from_slice(raw.get().as_bytes());
        }
    }
    for (name, saw_field) in [
        ("model", saw_model),
        ("client_metadata", saw_client_metadata),
        ("previous_response_id", saw_previous_response_id),
    ] {
        if saw_field {
            continue;
        }
        let Some(value) = updated.get(name) else {
            continue;
        };
        begin_encoded_json_field(&mut output, &mut first, name)?;
        serde_json::to_writer(&mut output, value).context("序列化 Responses 新增请求字段失败")?;
    }
    output.push(b'}');
    Ok(output)
}

pub(crate) async fn rewrite_native_responses_encoded_body_offloaded(
    original: Vec<u8>,
    updated: &Value,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<(Vec<u8>, Option<OwnedSemaphorePermit>)> {
    if original.len() < REQUEST_JSON_OFFLOAD_BYTES {
        return Ok((
            rewrite_native_responses_encoded_body(&original, updated)?,
            permit,
        ));
    }
    let updated = updated
        .as_object()
        .context("Responses 上游请求必须是 JSON 对象")?
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "model" | "client_metadata" | "previous_response_id"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    tokio::task::spawn_blocking(move || {
        let body = rewrite_native_responses_encoded_body(&original, &Value::Object(updated));
        drop(original);
        body.map(|body| (body, permit))
    })
    .await
    .context("等待大型 Responses 请求改写任务失败")?
}

pub(crate) fn take_route_hint_from_metadata_value(metadata: &mut Value) -> Result<Option<String>> {
    let Some(metadata) = metadata.as_object_mut() else {
        return Ok(None);
    };
    metadata
        .remove(ROUTE_METADATA_KEY)
        .as_ref()
        .map(validated_route_hint_value)
        .transpose()
}

pub(crate) fn validated_route_hint_value(value: &Value) -> Result<String> {
    let route_hint = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{ROUTE_METADATA_KEY} 必须是字符串"))?
        .trim();
    if route_hint.is_empty() || route_hint.len() > 256 || route_hint.chars().any(char::is_control) {
        anyhow::bail!("{ROUTE_METADATA_KEY} 不是有效的线路 ID");
    }
    Ok(route_hint.to_string())
}

pub(crate) fn merge_route_hint(current: &mut Option<String>, next: Option<String>) -> Result<()> {
    let Some(next) = next else {
        return Ok(());
    };
    if current.as_ref().is_some_and(|current| current != &next) {
        anyhow::bail!("请求头和请求体携带了冲突的 {ROUTE_METADATA_KEY}");
    }
    *current = Some(next);
    Ok(())
}

/// `Connection`/`Proxy-Connection` 列出的请求头按 RFC 9110 属于当前连接，
/// 不得转发到上游。
pub(crate) fn connection_scoped_header_names<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> HashSet<String> {
    let mut names = HashSet::new();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("proxy-connection")
        {
            names.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_ascii_lowercase),
            );
        }
    }
    names
}

pub(crate) fn should_forward_incoming_header(name: &str, official_account: bool) -> bool {
    if name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case(ROUTER_AUTH_HEADER)
        || name.eq_ignore_ascii_case(ROUTE_METADATA_KEY)
        || name.eq_ignore_ascii_case(CONTENT_ENCODING.as_str())
        || name.eq_ignore_ascii_case(CONTENT_TYPE.as_str())
        || name.to_ascii_lowercase().starts_with("x-codey-")
        || is_hop_by_hop_header(name)
    {
        return false;
    }
    // ChatGPT-account headers are required by the official Codex endpoint but
    // must never cross into an API-key provider. Third-party routes receive
    // only content negotiation, Codex client identity, and saved route headers.
    official_account || name.eq_ignore_ascii_case("accept") || is_codex_client_identity_header(name)
}

pub(crate) fn ensure_native_prompt_cache_key(
    headers: &mut HeaderMap,
    body: &Value,
    route_id: &str,
    upstream_url: &str,
    upstream_model: &str,
) -> bool {
    if headers.contains_key(PROMPT_CACHE_KEY_HEADER)
        || headers.contains_key(PROMPT_CACHE_KEY_COMPAT_HEADER)
        || body
            .as_object()
            .is_some_and(|body| body.contains_key(PROMPT_CACHE_KEY_BODY_FIELD))
    {
        return false;
    }
    let key = stable_prompt_cache_key(route_id, upstream_url, upstream_model, headers);
    headers.insert(
        HeaderName::from_static(PROMPT_CACHE_KEY_HEADER),
        HeaderValue::from_str(&key)
            .expect("generated prompt cache key must be a valid header value"),
    );
    true
}

pub(crate) fn stable_prompt_cache_key(
    route_id: &str,
    upstream_url: &str,
    upstream_model: &str,
    headers: &HeaderMap,
) -> String {
    let mut hasher = Sha256::new();
    for component in [
        b"codey-prompt-cache-v1".as_slice(),
        route_id.as_bytes(),
        upstream_url.as_bytes(),
        upstream_model.as_bytes(),
    ] {
        update_length_prefixed_digest(&mut hasher, component);
    }
    let (identity_kind, identity) = headers
        .get(CHATGPT_ACCOUNT_ID_HEADER)
        .map(|value| (b"account".as_slice(), value.as_bytes()))
        .or_else(|| {
            headers
                .get(AUTHORIZATION)
                .map(|value| (b"authorization".as_slice(), value.as_bytes()))
        })
        .unwrap_or((b"anonymous".as_slice(), b"".as_slice()));
    update_length_prefixed_digest(&mut hasher, identity_kind);
    update_length_prefixed_digest(&mut hasher, &Sha256::digest(identity));
    let digest = hasher.finalize();
    let seed: [u8; 16] = digest[..16]
        .try_into()
        .expect("SHA-256 digest provides at least 16 bytes");
    // 缓存键以标准 UUID 形态外发，与官方客户端自行携带的会话缓存键一致，
    // 不在值里携带 Codey 标识；同一输入仍生成同一个键。
    uuid::Builder::from_random_bytes(seed)
        .into_uuid()
        .to_string()
}

pub(crate) fn update_length_prefixed_digest(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub(crate) const ROUTING_HINT_HEADER: &str = "x-codex-routing-hint";

/// `x-codex-routing-hint` 的值形如 `model=<模型名>[;tier=<层级>]`，由 Codex 按它
/// 所见的模型名（可能是 Codey 线路别名）生成。请求体的模型名在转发前
/// 已还原为上游模型名，路由提示必须同步改写，否则上游收到与请求体
/// 不一致的模型提示，可能被路由到错误的服务组。
pub(crate) fn align_routing_hint_model(headers: &mut HeaderMap, upstream_model: &str) {
    let Some(value) = headers.get(ROUTING_HINT_HEADER) else {
        return;
    };
    let Ok(value) = value.to_str() else {
        headers.remove(ROUTING_HINT_HEADER);
        return;
    };
    let rewritten = value
        .split(';')
        .map(|segment| {
            if segment.trim().starts_with("model=") {
                format!("model={upstream_model}")
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(";");
    if rewritten == value {
        return;
    }
    match HeaderValue::from_str(&rewritten) {
        Ok(rewritten) => {
            headers.insert(HeaderName::from_static(ROUTING_HINT_HEADER), rewritten);
        }
        Err(_) => {
            headers.remove(ROUTING_HINT_HEADER);
        }
    }
}

pub(crate) fn is_codex_client_identity_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "user-agent"
            | "originator"
            | "version"
            | "openai-beta"
            | "x-openai-originator"
            | "x-openai-client-user-agent"
            | "x-client-request-id"
            | "thread-id"
            | "thread_id"
            | "session-id"
            | "session_id"
            | "prompt-cache-key"
            | "prompt_cache_key"
            | "x-codex-installation-id"
            | "x-codex-window-id"
            | "x-codex-parent-thread-id"
            | "x-codex-beta-features"
            | "x-codex-turn-state"
            | "x-codex-routing-hint"
            | "x-openai-subagent"
            | "x-openai-memgen-request"
            | "x-openai-internal-codex-responses-lite"
            | "x-responsesapi-include-timing-metrics"
    ) || lower.starts_with("x-stainless-")
}

pub(crate) fn incoming_header<'a>(request: &'a HttpRequest, header_name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(header_name))
        .map(|(_, value)| value.as_str())
}

pub(crate) fn request_is_subagent(request: &HttpRequest) -> bool {
    incoming_header(request, "x-openai-subagent").is_some()
        || incoming_header(request, "x-codex-parent-thread-id").is_some()
}

pub(crate) fn incoming_openai_authorization<'a>(
    request: &'a HttpRequest,
    router_bearer_token: &str,
) -> Option<&'a str> {
    let authorization = incoming_header(request, "authorization")?;
    if constant_time_eq(
        authorization.trim().as_bytes(),
        router_bearer_token.as_bytes(),
    ) {
        return None;
    }
    Some(authorization)
}
