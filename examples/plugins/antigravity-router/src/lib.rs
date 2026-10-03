//! Declarative loopback route; lifecycle control is opt-in and scoped to a host route ID.
use codey_plugin_sdk::{
    Plugin, PluginContext,
    serde_json::{Value, json},
};
const MARKER: &str = "x-antigravity-provider";
const DEFAULT_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "gemini-3.5-pro",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
];
struct AntigravityRouter {
    lifecycle_enabled: bool,
    base_url: String,
    models: Vec<String>,
    route_id: String,
    retry_once: bool,
}
impl Plugin for AntigravityRouter {
    fn create(config: Value, context: PluginContext) -> Result<Self, String> {
        let object = config
            .as_object()
            .ok_or("configuration must be an object")?;
        for key in object.keys() {
            if ![
                "lifecycleEnabled",
                "enabled",
                "baseUrl",
                "models",
                "routeId",
                "retryOnce",
                "_comments",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("unknown configuration field: {key}"));
            }
        }
        let bool_field = |name: &str, default: bool| -> Result<bool, String> {
            match config.get(name) {
                None => Ok(default),
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| format!("{name} must be a boolean")),
            }
        };
        let legacy_enabled = bool_field("enabled", true)?;
        let lifecycle_enabled = bool_field("lifecycleEnabled", legacy_enabled)?;
        if config.get("enabled").is_some()
            && config.get("lifecycleEnabled").is_some()
            && lifecycle_enabled != legacy_enabled
        {
            return Err(
                "enabled and lifecycleEnabled disagree; remove the deprecated enabled field".into(),
            );
        }
        let retry_once = bool_field("retryOnce", false)?;
        let base_url = config
            .get("baseUrl")
            .map(|v| v.as_str().ok_or("baseUrl must be a string"))
            .transpose()?
            .unwrap_or("http://127.0.0.1:8787/v1")
            .to_string();
        let port = base_url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|v| v.strip_suffix("/v1"))
            .ok_or("baseUrl must be http://127.0.0.1:<port>/v1")?;
        let parsed = port.parse::<u16>().map_err(|_| "invalid loopback port")?;
        if parsed == 0 || parsed.to_string() != port {
            return Err("invalid loopback port".into());
        }
        let models: Vec<String> = match config.get("models") {
            None => DEFAULT_MODELS.iter().map(|s| s.to_string()).collect(),
            Some(value) => value
                .as_array()
                .ok_or("models must be an array")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or("models must contain strings")
                })
                .collect::<Result<_, _>>()?,
        };
        let mut seen = std::collections::HashSet::new();
        if models.is_empty()
            || models.len() > 32
            || models.iter().any(|m| {
                m.is_empty()
                    || m.len() > 128
                    || !m
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    || m.eq_ignore_ascii_case("codex-auto-review")
                    || !seen.insert(m.to_ascii_lowercase())
            })
        {
            return Err("models require 1..32 distinct safe IDs".into());
        }
        let route_id = config
            .get("routeId")
            .map(|v| v.as_str().ok_or("routeId must be a string"))
            .transpose()?
            .unwrap_or("")
            .to_string();
        if route_id.len() > 128
            || route_id
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b"-_.".contains(&b))
        {
            return Err("invalid routeId".into());
        }
        if retry_once && route_id.is_empty() {
            return Err("retryOnce requires an explicit routeId".into());
        }
        context.log("antigravity_router_created")?;
        Ok(Self {
            lifecycle_enabled,
            base_url,
            models,
            route_id,
            retry_once,
        })
    }
    fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let ours = self.lifecycle_enabled
            && !self.route_id.is_empty()
            && params.pointer("/metadata/routeId").and_then(Value::as_str)
                == Some(self.route_id.as_str());
        match method {
            "provider.describe" => Ok(
                json!({"name":"Antigravity","baseUrl":self.base_url,"upstreamProtocol":"openaiResponses","models":self.models,"headers":[{"name":MARKER,"value":"codey-antigravity"}]}),
            ),
            "request.beforeSend" => {
                if ours {
                    Ok(
                        json!({"action":"continue","headers":[{"name":MARKER,"value":"codey-antigravity"}]}),
                    )
                } else {
                    Ok(json!({"action":"continue"}))
                }
            }
            "request.afterHeaders" => {
                if !ours || params.pointer("/response/status").and_then(Value::as_u64) != Some(429)
                {
                    return Ok(json!({"action":"continue"}));
                }
                if self.retry_once && params.get("attempt").and_then(Value::as_u64) == Some(0) {
                    return Ok(json!({"action":"retry"}));
                }
                Ok(
                    json!({"action":"abort","status":429,"code":"antigravity_quota_exhausted","message":"Antigravity quota exhausted. Try later or select another Google account."}),
                )
            }
            "request.completed" | "request.failed" | "request.cancelled" => Ok(json!({})),
            "ping" => Ok(
                json!({"plugin":"antigravity-router","version":"0.9.0","lifecycleEnabled":self.lifecycle_enabled}),
            ),
            _ => Err(format!("unknown method: {method}")),
        }
    }
}
codey_plugin_sdk::export_plugin!(AntigravityRouter);

#[cfg(test)]
mod tests {
    use super::*;

    fn router(config: Value) -> (AntigravityRouter, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let context = PluginContext {
            plugin_id: "dev.codey.antigravity-router".into(),
            plugin_dir: dir.path().into(),
            data_dir: dir.path().into(),
            log_dir: dir.path().into(),
        };
        (AntigravityRouter::create(config, context).unwrap(), dir)
    }

    #[test]
    fn default_route_does_not_intercept_lifecycle_requests() {
        let (mut router, _dir) = router(json!({}));
        let route = router.invoke("provider.describe", json!({})).unwrap();
        assert_eq!(route["upstreamProtocol"], "openaiResponses");
        assert_eq!(route["headers"][0]["name"], MARKER);
        assert_eq!(route["headers"][0]["value"], "codey-antigravity");
        assert_eq!(
            router
                .invoke(
                    "request.beforeSend",
                    json!({"metadata":{"routeId":"other"}})
                )
                .unwrap(),
            json!({"action":"continue"})
        );
        assert!(router.invoke("unknown.method", json!({})).is_err());
    }

    #[test]
    fn scoped_headers_and_retry_are_limited_to_selected_route_and_attempt() {
        let (mut router, _dir) = router(json!({"routeId":"ours","retryOnce":true}));
        assert_eq!(
            router
                .invoke("request.beforeSend", json!({"metadata":{"routeId":"ours"}}))
                .unwrap()["headers"][0]["name"],
            MARKER
        );
        for (route, status, attempt, action) in [
            ("other", 429, 0, "continue"),
            ("ours", 200, 0, "continue"),
            ("ours", 429, 0, "retry"),
            ("ours", 429, 1, "abort"),
        ] {
            assert_eq!(router.invoke("request.afterHeaders", json!({"metadata":{"routeId":route},"response":{"status":status},"attempt":attempt})).unwrap()["action"], action);
        }
    }
}
