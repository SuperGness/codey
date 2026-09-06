use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}

impl UpstreamProtocol {
    pub(crate) fn from_profile(official_account: bool, upstream_protocol: &str) -> Self {
        if official_account {
            return Self::OpenAiResponses;
        }
        match upstream_protocol {
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS => Self::OpenAiChatCompletions,
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => Self::AnthropicMessages,
            UPSTREAM_PROTOCOL_OPENAI_RESPONSES => Self::OpenAiResponses,
            _ => Self::OpenAiResponses,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "OpenAI Responses",
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::AnthropicMessages => "Anthropic Messages",
        }
    }

    pub(crate) fn endpoint_label(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "API URL",
            Self::OpenAiChatCompletions => "Chat Completions API URL",
            Self::AnthropicMessages => "Anthropic Messages API URL",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolBridge {
    NativeResponses,
    ResponsesToChatCompletions,
    ResponsesToAnthropicMessages,
}

impl ProtocolBridge {
    pub(crate) fn from_upstream_protocol(protocol: UpstreamProtocol) -> Self {
        match protocol {
            UpstreamProtocol::OpenAiResponses => Self::NativeResponses,
            UpstreamProtocol::OpenAiChatCompletions => Self::ResponsesToChatCompletions,
            UpstreamProtocol::AnthropicMessages => Self::ResponsesToAnthropicMessages,
        }
    }

    pub(crate) fn upstream_protocol(self) -> UpstreamProtocol {
        match self {
            Self::NativeResponses => UpstreamProtocol::OpenAiResponses,
            Self::ResponsesToChatCompletions => UpstreamProtocol::OpenAiChatCompletions,
            Self::ResponsesToAnthropicMessages => UpstreamProtocol::AnthropicMessages,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NativeResponses => "Responses passthrough",
            Self::ResponsesToChatCompletions => "Responses -> Chat Completions",
            Self::ResponsesToAnthropicMessages => "Responses -> Anthropic Messages",
        }
    }

    pub(crate) fn convert_responses_body(
        self,
        body: &Value,
    ) -> Result<Option<ConvertedResponsesRequest>> {
        match self {
            Self::NativeResponses => Ok(None),
            Self::ResponsesToChatCompletions => {
                responses_to_chat_completions_request(body).map(Some)
            }
            Self::ResponsesToAnthropicMessages => {
                responses_to_anthropic_messages_request(body).map(Some)
            }
        }
    }

    pub(crate) fn can_collect_streamed_response(self) -> bool {
        matches!(
            self,
            Self::ResponsesToChatCompletions | Self::ResponsesToAnthropicMessages
        )
    }
}

pub(crate) fn is_hop_by_hop_header(name: &str) -> bool {
    [
        "host",
        "content-length",
        "connection",
        "proxy-connection",
        "keep-alive",
        "transfer-encoding",
        "te",
        "trailer",
        "upgrade",
        "accept-encoding",
    ]
    .iter()
    .any(|blocked| name.eq_ignore_ascii_case(blocked))
}

pub(crate) fn is_sse_content_type(value: &str) -> bool {
    const SSE_CONTENT_TYPE: &[u8] = b"text/event-stream";
    value
        .as_bytes()
        .windows(SSE_CONTENT_TYPE.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(SSE_CONTENT_TYPE))
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (*left ^ *right)
        })
        == 0
}

pub(crate) fn current_router_request_id() -> Option<String> {
    ROUTER_REQUEST_ID
        .try_with(|request_id| request_id.clone())
        .ok()
}

pub(crate) fn current_router_request_started_at() -> Option<Instant> {
    ROUTER_REQUEST_STARTED_AT
        .try_with(|started_at| *started_at)
        .ok()
}

pub(crate) fn router_request_id_header() -> String {
    current_router_request_id()
        .map(|request_id| format!("x-codey-request-id: {request_id}\r\n"))
        .unwrap_or_default()
}

pub(crate) fn normalized_endpoint_url(base_url: &str) -> Result<reqwest::Url> {
    let mut url = crate::config::validate_outbound_api_url(base_url.trim(), "线路 API URL")
        .map_err(anyhow::Error::msg)?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub(crate) fn strip_ascii_case_suffix<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let prefix_len = value.len().checked_sub(suffix.len())?;
    value[prefix_len..]
        .eq_ignore_ascii_case(suffix)
        .then_some(&value[..prefix_len])
}

pub(crate) fn responses_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/responses").is_some() {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/responses"))
    }
}

pub(crate) fn image_generation_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/images/generations").is_some() {
        return Ok(base.to_string());
    }
    for suffix in ["/chat/completions", "/responses"] {
        if let Some(prefix) = strip_ascii_case_suffix(base, suffix) {
            return Ok(format!(
                "{}/images/generations",
                prefix.trim_end_matches('/')
            ));
        }
    }
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if has_version_suffix(last_segment) {
        Ok(format!("{base}/images/generations"))
    } else {
        Ok(format!("{base}/v1/images/generations"))
    }
}

pub(crate) fn responses_compact_endpoint(base_url: &str) -> Result<String> {
    let endpoint = responses_endpoint(base_url)?;
    Ok(format!("{}/compact", endpoint.trim_end_matches('/')))
}

pub(crate) fn responses_websocket_endpoint(base_url: &str) -> Result<String> {
    let endpoint = responses_endpoint(base_url)?;
    let mut url = reqwest::Url::parse(&endpoint).context("解析 Responses WebSocket URL 失败")?;
    let websocket_scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        scheme => anyhow::bail!("Responses WebSocket 不支持 URL scheme {scheme}"),
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| anyhow::anyhow!("转换 Responses WebSocket URL scheme 失败"))?;
    Ok(url.to_string())
}

pub(crate) fn chat_completions_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/chat/completions").is_some() {
        return Ok(base.to_string());
    }
    if let Some(prefix) = strip_ascii_case_suffix(base, "/responses") {
        return Ok(format!("{}/chat/completions", prefix.trim_end_matches('/')));
    }
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if has_version_suffix(last_segment) {
        Ok(format!("{base}/chat/completions"))
    } else {
        Ok(format!("{base}/v1/chat/completions"))
    }
}

pub(crate) fn anthropic_messages_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/messages").is_some() {
        return Ok(base.to_string());
    }
    for suffix in ["/chat/completions", "/responses"] {
        if let Some(prefix) = strip_ascii_case_suffix(base, suffix) {
            return Ok(format!("{}/messages", prefix.trim_end_matches('/')));
        }
    }
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if has_version_suffix(last_segment) {
        Ok(format!("{base}/messages"))
    } else {
        Ok(format!("{base}/v1/messages"))
    }
}

pub(crate) fn prepare_upstream_url(
    protocol: UpstreamProtocol,
    base_url: &str,
) -> std::result::Result<String, String> {
    let result = match protocol {
        UpstreamProtocol::OpenAiResponses => responses_endpoint(base_url),
        UpstreamProtocol::OpenAiChatCompletions => chat_completions_endpoint(base_url),
        UpstreamProtocol::AnthropicMessages => anthropic_messages_endpoint(base_url),
    };
    result.map_err(|error| {
        let endpoint = protocol.endpoint_label();
        format!("{endpoint} 无效：{error:#}")
    })
}

pub(crate) fn prepare_upstream_compact_url(
    protocol: UpstreamProtocol,
    base_url: &str,
) -> std::result::Result<String, String> {
    if protocol == UpstreamProtocol::OpenAiResponses {
        responses_compact_endpoint(base_url)
            .map_err(|error| format!("Responses Compact API URL 无效：{error:#}"))
    } else {
        // CC Switch sends legacy compact requests through the same conversion
        // and upstream endpoint as a normal Responses request for adapted
        // Chat Completions and Anthropic routes.
        prepare_upstream_url(protocol, base_url)
    }
}

pub(crate) fn prepare_upstream_websocket_url(
    protocol: UpstreamProtocol,
    base_url: &str,
) -> std::result::Result<String, String> {
    if protocol != UpstreamProtocol::OpenAiResponses {
        return Err(format!("{} 不支持 Responses WebSocket", protocol.label()));
    }
    responses_websocket_endpoint(base_url)
        .map_err(|error| format!("Responses WebSocket API URL 无效：{error:#}"))
}

pub(crate) fn prepare_upstream_headers(
    profile: &crate::config::ProviderProfile,
    protocol: UpstreamProtocol,
) -> std::result::Result<HeaderMap, String> {
    let route_name = profile.name.trim();
    let mut headers = HeaderMap::with_capacity(profile.model_request_headers.len() + 2);
    for (name, value) in &profile.model_request_headers {
        if value.trim().is_empty() {
            continue;
        }
        if is_hop_by_hop_header(name) {
            return Err(format!("线路「{route_name}」包含不允许覆盖的请求头 {name}"));
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("线路「{route_name}」包含非法请求头名称"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| format!("线路「{route_name}」包含非法请求头值"))?;
        headers.insert(name, value);
    }

    let has_custom_authorization = headers.contains_key(AUTHORIZATION);
    if protocol == UpstreamProtocol::AnthropicMessages && has_custom_authorization {
        return Err(format!(
            "线路「{route_name}」使用 Anthropic Messages 时不允许配置 Authorization；请使用该线路的 Key 字段或 x-api-key"
        ));
    }
    if !profile.official_account && !profile.api_key.trim().is_empty() {
        let header_name = if protocol == UpstreamProtocol::AnthropicMessages {
            HeaderName::from_static("x-api-key")
        } else {
            AUTHORIZATION
        };
        if !headers.contains_key(&header_name) {
            let header_value = if protocol == UpstreamProtocol::AnthropicMessages {
                profile.api_key.trim().to_string()
            } else {
                format!("Bearer {}", profile.api_key.trim())
            };
            let value = HeaderValue::from_str(&header_value)
                .map_err(|_| format!("线路「{route_name}」的 API Key 格式无效"))?;
            headers.insert(header_name, value);
        }
    }
    if protocol == UpstreamProtocol::AnthropicMessages
        && !headers.contains_key(HeaderName::from_static("anthropic-version"))
    {
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
    }
    Ok(headers)
}

pub(crate) fn has_version_suffix(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .or_else(|| segment.strip_prefix('V'))
        .is_some_and(|version| version.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
}

#[derive(Clone, Debug)]
pub(crate) struct ConvertedResponsesRequest {
    pub(crate) body: Value,
    pub(crate) tool_bridge: ResponsesToolBridge,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ResponsesToolBridge {
    pub(crate) upstream_to_response: HashMap<String, ResponsesToolName>,
    pub(crate) response_to_upstream: HashMap<ResponsesToolName, String>,
    pub(crate) has_namespace_tools: bool,
    pub(crate) has_custom_tools: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResponsesToolKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResponsesToolName {
    pub(crate) kind: ResponsesToolKind,
    pub(crate) namespace: Vec<String>,
    pub(crate) name: String,
}

impl ResponsesToolName {
    pub(crate) fn plain(name: &str) -> Self {
        Self {
            kind: ResponsesToolKind::Function,
            namespace: Vec::new(),
            name: name.to_string(),
        }
    }

    pub(crate) fn custom_in_namespace(namespace: &[String], name: &str) -> Self {
        Self {
            kind: ResponsesToolKind::Custom,
            namespace: namespace.to_vec(),
            name: name.to_string(),
        }
    }

    pub(crate) fn tool_search() -> Self {
        Self {
            kind: ResponsesToolKind::ToolSearch,
            namespace: Vec::new(),
            name: "tool_search".to_string(),
        }
    }

    pub(crate) fn is_custom(&self) -> bool {
        self.kind == ResponsesToolKind::Custom
    }

    pub(crate) fn is_function(&self) -> bool {
        self.kind == ResponsesToolKind::Function
    }

    pub(crate) fn is_tool_search(&self) -> bool {
        self.kind == ResponsesToolKind::ToolSearch
    }

    pub(crate) fn namespace_string(&self) -> Option<String> {
        (!self.namespace.is_empty()).then(|| self.namespace.join("."))
    }

    pub(crate) fn insert_response_fields(&self, object: &mut serde_json::Map<String, Value>) {
        if self.is_tool_search() {
            object.remove("name");
            object.remove("namespace");
            object.insert("execution".to_string(), Value::String("client".to_string()));
            return;
        }
        object.insert("name".to_string(), Value::String(self.name.clone()));
        if let Some(namespace) = self.namespace_string() {
            object.insert("namespace".to_string(), Value::String(namespace));
        } else {
            object.remove("namespace");
        }
    }
}

impl ResponsesToolBridge {
    pub(crate) fn upstream_name_for_call(&self, tool_name: &ResponsesToolName) -> Result<String> {
        if let Some(upstream_name) = self.response_to_upstream.get(tool_name) {
            return Ok(upstream_name.clone());
        }
        if tool_name.is_function() && tool_name.namespace.is_empty() {
            return Ok(tool_name.name.clone());
        }
        if tool_name.is_custom() {
            anyhow::bail!(
                "custom_tool_call 指向未声明的 custom 工具 {}",
                tool_name.name
            )
        }
        if tool_name.is_tool_search() {
            anyhow::bail!("tool_search_call 指向未声明的 execution=client tool_search 工具")
        }
        anyhow::bail!(
            "function_call 指向未声明的 namespace 工具 {}.{}",
            tool_name.namespace.join("."),
            tool_name.name
        )
    }

    pub(crate) fn restore_upstream_name(&self, upstream_name: &str) -> Result<ResponsesToolName> {
        if let Some(tool_name) = self.upstream_to_response.get(upstream_name) {
            return Ok(tool_name.clone());
        }
        if self.has_namespace_tools && looks_like_namespace_upstream_name(upstream_name) {
            anyhow::bail!("上游返回了未知的 namespace function 名称 {upstream_name}");
        }
        if self.has_custom_tools && looks_like_custom_upstream_name(upstream_name) {
            anyhow::bail!("上游返回了未知的 custom function 名称 {upstream_name}");
        }
        Ok(ResponsesToolName::plain(upstream_name))
    }

    pub(crate) fn restore_stream_upstream_name(
        &self,
        upstream_name: &str,
        final_name: bool,
    ) -> Result<Option<ResponsesToolName>> {
        if let Some(tool_name) = self.upstream_to_response.get(upstream_name) {
            if !final_name
                && self.upstream_to_response.keys().any(|known| {
                    known.len() > upstream_name.len() && known.starts_with(upstream_name)
                })
            {
                return Ok(None);
            }
            return Ok(Some(tool_name.clone()));
        }
        if !final_name
            && self
                .upstream_to_response
                .keys()
                .any(|known| known.starts_with(upstream_name))
        {
            return Ok(None);
        }
        if self.has_namespace_tools && could_be_namespace_upstream_name(upstream_name) {
            if final_name {
                anyhow::bail!("上游返回了未知的 namespace function 名称 {upstream_name}");
            }
            return Ok(None);
        }
        if self.has_custom_tools && could_be_custom_upstream_name(upstream_name) {
            if final_name {
                anyhow::bail!("上游返回了未知的 custom function 名称 {upstream_name}");
            }
            return Ok(None);
        }
        Ok(Some(ResponsesToolName::plain(upstream_name)))
    }
}
