//! 可选的声明式线路。插件只描述线路，不提供传输实现。
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "provider.route.v1";
pub const METHOD_DESCRIBE: &str = "provider.describe";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteHeader {
    pub name: String,
    pub value: String,
}

/// `provider.describe` 的返回值。密钥由用户写在线路上，不在这里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteDescriptor {
    pub name: String,
    pub base_url: String,
    pub upstream_protocol: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub headers: Vec<RouteHeader>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_rejects_unknown_fields() {
        let error = serde_json::from_value::<RouteDescriptor>(serde_json::json!({
            "name": "示例",
            "baseUrl": "https://example.test/v1",
            "upstreamProtocol": "openaiResponses",
            "models": ["demo"],
            "apiKey": "secret"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("apiKey"), "{error}");
    }
}
