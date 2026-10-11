use codey_plugin_sdk::{
    Plugin, PluginContext,
    serde_json::{self, Value, json},
};

struct HeaderPlugin {
    value: String,
    received_config: Value,
    context: PluginContext,
}

impl Plugin for HeaderPlugin {
    fn create(config: Value, context: PluginContext) -> Result<Self, String> {
        let value = config
            .get("value")
            .and_then(Value::as_str)
            .ok_or("缺少 value 配置")?
            .to_owned();
        context.log("header_demo_created")?;
        Ok(Self {
            value,
            received_config: config,
            context,
        })
    }

    fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "provider.describe" => Ok(json!({
                "name": "Fixture",
                "baseUrl": "https://example.invalid/v1",
                "upstreamProtocol": "openaiResponses",
                "models": ["fixture-model"],
                "headers": [],
                "transport": {
                    "accountEmail": "fixture@example.com",
                    "models": {},
                    "imageGeneration": false,
                    "imageEdit": false
                }
            })),
            "provider.request.stop" => Ok(json!({})),
            "config.received" => Ok(self.received_config.clone()),
            "request.beforeSend" => Ok(
                json!({"action":"continue","headers":[{"name":"x-plugin-demo", "value":self.value}]}),
            ),
            "request.afterHeaders" => Ok(json!({"action":"continue"})),
            "request.completed" | "request.failed" | "request.cancelled" => {
                std::fs::write(self.context.data_dir.join("terminal-received.txt"), method)
                    .map_err(|e| e.to_string())?;
                Ok(json!({}))
            }
            "ping" => Ok(json!({"plugin":"header-demo","value":self.value,"params":params})),
            "storage.write" => {
                let bytes = serde_json::to_vec(&params).map_err(|e| e.to_string())?;
                std::fs::write(self.context.data_dir.join("example.json"), bytes)
                    .map_err(|e| e.to_string())?;
                self.context.log("example_saved")?;
                Ok(Value::Null)
            }
            "storage.read" => {
                let bytes = std::fs::read(self.context.data_dir.join("example.json"))
                    .map_err(|e| e.to_string())?;
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())
            }
            "storage.context" => serde_json::to_value(&self.context).map_err(|e| e.to_string()),
            _ => Err(format!("未知方法: {method}")),
        }
    }
}

codey_plugin_sdk::export_plugin!(HeaderPlugin);
