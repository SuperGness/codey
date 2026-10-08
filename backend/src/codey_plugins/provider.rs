//! 插件线路在启用和宿主同步模型时提交描述。管理接口不能调用描述方法。
use super::Manifest;
use super::native::Native;
use codey_plugin_sdk::provider::{CAPABILITY, METHOD_DESCRIBE, RouteDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub(crate) use codey_plugin_sdk::provider::METHOD_DESCRIBE as DESCRIBE_METHOD;

const MAX_MODELS: usize = 32;
const MAX_MODEL_CHARS: usize = 128;
const MAX_NAME_CHARS: usize = 15;
const MODEL_REASONING_EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
const AUTO_REVIEW_MODEL: &str = "codex-auto-review";
const ROUTE_PROTOCOLS: &[&str] = &[
    "openaiResponses",
    "openaiChatCompletions",
    "anthropicMessages",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRouteSpec {
    pub name: String,
    pub base_url: String,
    pub upstream_protocol: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub supports_websockets: bool,
    #[serde(default)]
    pub supports_remote_compaction: bool,
    #[serde(default)]
    pub supports_native_web_search: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_reasoning_efforts: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_contexts: BTreeMap<String, codey_plugin_sdk::provider::ModelContext>,
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub short_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<codey_plugin_sdk::transport::TransportOptions>,
}

#[allow(dead_code)]
pub enum RouteChange {
    Upsert {
        spec: Box<PluginRouteSpec>,
        create_if_missing: bool,
    },
    Release,
}

type RouteHandler = Arc<dyn Fn(&str, RouteChange) -> Result<Option<String>, String> + Send + Sync>;

static ROUTE_HANDLER: Mutex<Option<RouteHandler>> = Mutex::new(None);

#[allow(dead_code)]
pub(crate) fn set_route_handler(handler: RouteHandler) {
    *ROUTE_HANDLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handler);
}

pub(crate) fn describe_if_declared(
    manifest: &Manifest,
    native: &mut Native,
) -> Result<Option<PluginRouteSpec>, String> {
    if !manifest
        .capabilities
        .iter()
        .any(|capability| capability == CAPABILITY)
    {
        return Ok(None);
    }
    let value = native
        .invoke(METHOD_DESCRIBE, json!({}))
        .map_err(|error| format!("插件线路描述失败：{error}"))?;
    let spec = parse_route_descriptor(value)?;
    if spec.transport.is_some()
        && ![
            codey_plugin_sdk::transport::CAPABILITY,
            codey_plugin_sdk::transport::ACCOUNT_CAPABILITY,
        ]
        .iter()
        .all(|needed| manifest.capabilities.iter().any(|cap| cap == needed))
    {
        return Err("自定义传输必须声明传输与账号授权能力".into());
    }
    if manifest
        .capabilities
        .iter()
        .any(|cap| cap == codey_plugin_sdk::transport::CAPABILITY)
        && spec.transport.is_none()
    {
        return Err("自定义传输缺少 transport 描述".into());
    }
    Ok(Some(spec))
}

pub(crate) fn publish_route(
    plugin_id: &str,
    spec: PluginRouteSpec,
    create_if_missing: bool,
) -> Result<Option<String>, String> {
    dispatch(
        plugin_id,
        RouteChange::Upsert {
            spec: Box::new(spec),
            create_if_missing,
        },
    )
}

pub(crate) fn release_route(plugin_id: &str) -> Result<(), String> {
    let Some(handler) = handler() else {
        return Ok(());
    };
    handler(plugin_id, RouteChange::Release).map(|_| ())
}

fn dispatch(plugin_id: &str, change: RouteChange) -> Result<Option<String>, String> {
    let Some(handler) = handler() else {
        return Err("插件线路尚未接入配置".into());
    };
    handler(plugin_id, change)
}

fn handler() -> Option<RouteHandler> {
    ROUTE_HANDLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub(crate) fn parse_route_descriptor(value: Value) -> Result<PluginRouteSpec, String> {
    let mut descriptor: RouteDescriptor =
        serde_json::from_value(value).map_err(|error| format!("插件线路描述格式无效：{error}"))?;
    let name = descriptor.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("插件线路名称需要 1 到 {MAX_NAME_CHARS} 个字符"));
    }
    if descriptor.base_url.trim().is_empty() {
        return Err("插件线路缺少 API URL".into());
    }
    if !ROUTE_PROTOCOLS.contains(&descriptor.upstream_protocol.as_str()) {
        return Err("插件线路协议不受支持".into());
    }
    if (descriptor.supports_websockets
        || descriptor.supports_remote_compaction
        || descriptor.supports_native_web_search)
        && (descriptor.upstream_protocol != "openaiResponses" || descriptor.transport.is_some())
    {
        return Err(
            "原生 WebSocket、远程压缩和 Web Search 声明仅支持标准 HTTP Responses 线路".into(),
        );
    }
    if descriptor.models.is_empty() || descriptor.models.len() > MAX_MODELS {
        return Err(format!("插件线路需要 1 到 {MAX_MODELS} 个模型"));
    }
    let mut models = Vec::with_capacity(descriptor.models.len());
    let mut seen = std::collections::HashSet::new();
    for model in &descriptor.models {
        let model = model.trim();
        if model.is_empty()
            || model.chars().count() > MAX_MODEL_CHARS
            || model
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || model.eq_ignore_ascii_case(AUTO_REVIEW_MODEL)
            || !seen.insert(model.to_ascii_lowercase())
        {
            return Err(format!("插件线路的模型 ID 无效或重复：{model}"));
        }
        models.push(model.to_string());
    }
    let mut model_reasoning_efforts = BTreeMap::new();
    for (model, efforts) in descriptor.model_reasoning_efforts {
        let canonical_model = models
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(model.trim()))
            .ok_or_else(|| format!("插件线路思考强度声明的模型不在模型列表中：{model}"))?
            .clone();
        if efforts.is_empty() {
            return Err(format!(
                "插件线路模型 {canonical_model} 的思考强度声明不能为空"
            ));
        }
        let mut normalized = Vec::with_capacity(efforts.len());
        let mut seen = std::collections::HashSet::new();
        for effort in efforts {
            let effort = effort.trim().to_ascii_lowercase();
            if !MODEL_REASONING_EFFORT_LEVELS.contains(&effort.as_str())
                || !seen.insert(effort.clone())
            {
                return Err(format!(
                    "插件线路模型 {canonical_model} 的思考强度无效或重复：{effort}"
                ));
            }
            normalized.push(effort);
        }
        if model_reasoning_efforts
            .insert(canonical_model.clone(), normalized)
            .is_some()
        {
            return Err(format!("插件线路模型思考强度声明重复：{canonical_model}"));
        }
    }
    let mut model_contexts = BTreeMap::new();
    for (model, caps) in descriptor.model_contexts {
        let canonical_model = models
            .iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(&model))
            .ok_or_else(|| format!("插件线路上下文声明的模型不在模型列表中：{model}"))?
            .clone();
        crate::config::ModelContextConfig {
            context_window_tokens: caps.context_window,
            auto_compact_token_limit: Some(caps.auto_compact_token_limit),
            reserve_output_tokens: caps.reserve_output_tokens,
        }
        .validate()
        .map_err(|error| format!("插件模型 {canonical_model} 上下文声明无效：{error}"))?;
        if model_contexts
            .insert(canonical_model.clone(), caps)
            .is_some()
        {
            return Err(format!("插件线路模型上下文声明重复：{canonical_model}"));
        }
    }
    if descriptor.headers.len() > 32 {
        return Err("插件线路请求头超过 32 项".into());
    }
    let mut headers = BTreeMap::new();
    let mut size = 0usize;
    for header in descriptor.headers {
        let name = header.name.trim().to_ascii_lowercase();
        if !super::allowed_header_name(&name) || headers.contains_key(&name) {
            return Err(format!("插件线路请求头无效或重复：{name}"));
        }
        let value = header.value;
        size += name.len() + value.len();
        if value.len() > 8192
            || size > 32768
            || value
                .bytes()
                .any(|byte| byte < 32 && byte != b'\t' || byte == 127)
            || (!value.is_empty() && value.trim().is_empty())
        {
            return Err(format!("插件线路请求头「{name}」的值无效或过大"));
        }
        headers.insert(name, value);
    }
    if let Some(transport) = &mut descriptor.transport {
        transport.account_email = transport.account_email.trim().to_ascii_lowercase();
        if transport.account_email.len() > 254
            || !transport.account_email.contains('@')
            || transport
                .account_email
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err("请在插件配置中填写有效的账号邮箱".into());
        }
        if descriptor.upstream_protocol != "openaiResponses" || !headers.is_empty() {
            return Err("插件传输须返回标准 Responses，且不能声明线路请求头".into());
        }
        for (model, caps) in &transport.models {
            if !models.contains(model)
                || !(1024..=2_000_000).contains(&caps.context_window)
                || caps.auto_compact_token_limit == 0
                || caps.auto_compact_token_limit > caps.context_window
            {
                return Err("插件模型上下文声明无效".into());
            }
            if model_contexts.get(model).is_some_and(|context| {
                context.context_window != caps.context_window
                    || context.auto_compact_token_limit != caps.auto_compact_token_limit
                    || context.reserve_output_tokens.is_some()
            }) {
                return Err("插件模型上下文与传输声明冲突".into());
            }
        }
    }
    Ok(PluginRouteSpec {
        name: name.to_string(),
        base_url: descriptor.base_url,
        upstream_protocol: descriptor.upstream_protocol,
        models,
        supports_websockets: descriptor.supports_websockets,
        supports_remote_compaction: descriptor.supports_remote_compaction,
        supports_native_web_search: descriptor.supports_native_web_search,
        model_reasoning_efforts,
        model_contexts,
        headers,
        short_name: String::new(),
        transport: descriptor.transport,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor() -> Value {
        json!({
            "name": "示例线路",
            "baseUrl": "https://relay.example/v1",
            "upstreamProtocol": "openaiResponses",
            "models": ["demo-model"],
            "headers": [{"name": "X-Region", "value": "us"}]
        })
    }

    #[test]
    fn parse_accepts_a_bounded_route() {
        let spec = parse_route_descriptor(descriptor()).unwrap();
        assert_eq!(spec.headers.get("x-region").map(String::as_str), Some("us"));
        assert_eq!(spec.models, vec!["demo-model"]);
        assert!(!spec.supports_websockets);
        assert!(!spec.supports_remote_compaction);
        assert!(!spec.supports_native_web_search);
    }

    #[test]
    fn native_capabilities_require_http_responses() {
        for field in [
            "supportsWebsockets",
            "supportsRemoteCompaction",
            "supportsNativeWebSearch",
        ] {
            let mut value = descriptor();
            value[field] = json!(true);
            let spec = parse_route_descriptor(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(spec).unwrap()[field], true);
            for protocol in ["openaiChatCompletions", "anthropicMessages"] {
                value["upstreamProtocol"] = json!(protocol);
                assert!(
                    parse_route_descriptor(value.clone())
                        .unwrap_err()
                        .contains("标准 HTTP Responses")
                );
            }
            value["upstreamProtocol"] = json!("openaiResponses");
            value["headers"] = json!([]);
            value["transport"] = json!({"accountEmail":"user@example.com"});
            assert!(
                parse_route_descriptor(value)
                    .unwrap_err()
                    .contains("标准 HTTP Responses")
            );
        }
    }

    #[test]
    fn parse_accepts_model_reasoning_capabilities() {
        let mut value = descriptor();
        value["modelReasoningEfforts"] = json!({
            "demo-model": ["low", "medium", "high", "xhigh"]
        });
        let spec = parse_route_descriptor(value).unwrap();
        assert_eq!(
            spec.model_reasoning_efforts["demo-model"],
            ["low", "medium", "high", "xhigh"]
        );
    }

    #[test]
    fn parse_accepts_http_contexts_and_rejects_unsafe_budgets() {
        let mut value = descriptor();
        value["modelContexts"] = json!({"DEMO-MODEL": {
            "contextWindow": 200000, "autoCompactTokenLimit": 180000,
            "reserveOutputTokens": 8192
        }});
        let spec = parse_route_descriptor(value.clone()).unwrap();
        assert_eq!(spec.model_contexts["demo-model"].context_window, 200000);
        for contexts in [
            json!({"other":{"contextWindow":200000,"autoCompactTokenLimit":180000}}),
            json!({"demo-model":{"contextWindow":1023,"autoCompactTokenLimit":900}}),
            json!({"demo-model":{"contextWindow":200000,"autoCompactTokenLimit":180001}}),
            json!({"demo-model":{"contextWindow":200000,"autoCompactTokenLimit":180000,"reserveOutputTokens":50000}}),
            json!({"demo-model":{"contextWindow":200000,"autoCompactTokenLimit":0}}),
            json!({"demo-model":{"contextWindow":200000,"autoCompactTokenLimit":180000},"DEMO-MODEL":{"contextWindow":200000,"autoCompactTokenLimit":180000}}),
            json!({" demo-model ":{"contextWindow":200000,"autoCompactTokenLimit":180000}}),
        ] {
            value["modelContexts"] = contexts;
            assert!(parse_route_descriptor(value.clone()).is_err(), "{value}");
        }
    }

    #[test]
    fn legacy_transport_contexts_remain_valid_but_conflicting_maps_fail() {
        let mut value = descriptor();
        value["headers"] = json!([]);
        value["transport"] = json!({"accountEmail":"user@example.com","models": {
            "demo-model":{"contextWindow":200000,"autoCompactTokenLimit":190000}
        }});
        assert!(parse_route_descriptor(value.clone()).is_ok());
        value["modelContexts"] = json!({"demo-model": {
            "contextWindow":200000,"autoCompactTokenLimit":180000
        }});
        assert!(parse_route_descriptor(value).unwrap_err().contains("冲突"));
    }

    #[test]
    fn parse_rejects_invalid_model_reasoning_capabilities() {
        let mut unknown_model = descriptor();
        unknown_model["modelReasoningEfforts"] = json!({"other": ["low"]});
        assert!(
            parse_route_descriptor(unknown_model)
                .unwrap_err()
                .contains("不在模型列表")
        );
        let mut duplicate = descriptor();
        duplicate["modelReasoningEfforts"] = json!({"demo-model": ["xhigh", "xhigh"]});
        assert!(
            parse_route_descriptor(duplicate)
                .unwrap_err()
                .contains("无效或重复")
        );
        let mut empty = descriptor();
        empty["modelReasoningEfforts"] = json!({"demo-model": []});
        assert!(
            parse_route_descriptor(empty)
                .unwrap_err()
                .contains("不能为空")
        );
        let mut duplicate_model = descriptor();
        duplicate_model["modelReasoningEfforts"] =
            json!({"demo-model": ["low"], "DEMO-MODEL": ["xhigh"]});
        assert!(
            parse_route_descriptor(duplicate_model)
                .unwrap_err()
                .contains("声明重复")
        );
    }

    #[test]
    fn parse_rejects_secrets_official_protocol_and_duplicate_models() {
        let mut official = descriptor();
        official["upstreamProtocol"] = json!("official");
        assert!(
            parse_route_descriptor(official)
                .unwrap_err()
                .contains("协议")
        );
        let mut secret = descriptor();
        secret["headers"] = json!([{"name": "Authorization", "value": "Bearer x"}]);
        assert!(
            parse_route_descriptor(secret)
                .unwrap_err()
                .contains("请求头")
        );
        let mut models = descriptor();
        models["models"] = json!(["Demo", "demo"]);
        assert!(parse_route_descriptor(models).unwrap_err().contains("重复"));
    }
}
