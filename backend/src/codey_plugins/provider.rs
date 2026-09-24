//! 插件线路只在启用时向宿主提交描述。管理接口不能调用描述方法。
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
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub short_name: String,
}

pub enum RouteChange {
    Upsert {
        spec: PluginRouteSpec,
        create_if_missing: bool,
    },
    Release,
}

type RouteHandler = Arc<dyn Fn(&str, RouteChange) -> Result<Option<String>, String> + Send + Sync>;

static ROUTE_HANDLER: Mutex<Option<RouteHandler>> = Mutex::new(None);

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
    parse_route_descriptor(value).map(Some)
}

pub(crate) fn publish_route(
    plugin_id: &str,
    spec: PluginRouteSpec,
    create_if_missing: bool,
) -> Result<Option<String>, String> {
    dispatch(
        plugin_id,
        RouteChange::Upsert {
            spec,
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
    let descriptor: RouteDescriptor =
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
    Ok(PluginRouteSpec {
        name: name.to_string(),
        base_url: descriptor.base_url,
        upstream_protocol: descriptor.upstream_protocol,
        models,
        headers,
        short_name: String::new(),
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
