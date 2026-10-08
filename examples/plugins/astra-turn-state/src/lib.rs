use codey_plugin_sdk::{
    Plugin, PluginContext,
    lifecycle::METHOD_BEFORE_SEND,
    serde_json::{Value, json},
};

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_EMAIL_LEN: usize = 320;
const MAX_MODEL_LEN: usize = 128;

struct AstraTurnState {
    degraded_account_email: String,
    healthy_account_email: String,
    mint_model: Option<String>,
    timeout_ms: u64,
}

impl AstraTurnState {
    fn from_config(config: Value) -> Result<Self, String> {
        let object = config.as_object().ok_or("插件配置必须是 JSON 对象")?;
        for field in object.keys() {
            if !matches!(
                field.as_str(),
                "degradedAccountEmail"
                    | "healthyAccountEmail"
                    | "mintModel"
                    | "timeoutMs"
                    | "_comments"
            ) {
                return Err("插件配置包含不支持的字段".into());
            }
        }

        let email = |field: &str| -> Result<String, String> {
            let value = object
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{field} 必须是字符串"))?
                .trim()
                .to_ascii_lowercase();
            if !valid_email(&value) {
                return Err(format!("{field} 必须是有效的 ASCII 邮箱地址"));
            }
            Ok(value)
        };
        let degraded_account_email = email("degradedAccountEmail")?;
        let healthy_account_email = email("healthyAccountEmail")?;
        if degraded_account_email == healthy_account_email {
            return Err("degradedAccountEmail 与 healthyAccountEmail 必须不同".into());
        }

        let mint_model = object
            .get("mintModel")
            .and_then(Value::as_str)
            .ok_or("mintModel 必须是字符串")?
            .trim()
            .to_owned();
        let mint_model = if mint_model.is_empty() {
            None
        } else {
            if !valid_model(&mint_model) {
                return Err(
                    "mintModel 必须是 1 到 128 个字节的字母数字、点、下划线、斜线或连字符".into(),
                );
            }
            Some(mint_model)
        };

        let timeout_ms = match object.get("timeoutMs") {
            Some(value) => value.as_u64().ok_or("timeoutMs 必须是整数")?,
            None => DEFAULT_TIMEOUT_MS,
        };
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err("timeoutMs 必须在 1000 到 30000 毫秒之间".into());
        }
        Ok(Self {
            degraded_account_email,
            healthy_account_email,
            mint_model,
            timeout_ms,
        })
    }

    fn before_send(&self, params: Value) -> Value {
        let Some(metadata) = params.get("metadata") else {
            return json!({"action":"continue"});
        };
        if params.get("stage").and_then(Value::as_str) != Some("beforeSend")
            || params.get("attempt").and_then(Value::as_u64) != Some(0)
            || metadata
                .get("officialAccountEmail")
                .and_then(Value::as_str)
                .is_none_or(|email| !email.eq_ignore_ascii_case(&self.degraded_account_email))
            || metadata.get("turnStateAuthorized").and_then(Value::as_bool) != Some(true)
            || metadata.get("requestKind").and_then(Value::as_str) != Some("responses")
        {
            return json!({"action":"continue"});
        }
        let model = self.mint_model.clone().or_else(|| {
            metadata
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let Some(model) = model.filter(|model| valid_model(model)) else {
            return json!({"action":"abort", "code":"astra_invalid_model", "message":"无法确定预请求模型，请填写 mintModel"});
        };
        json!({
            "action": "borrowTurnState",
            "targetAccountEmail": self.degraded_account_email,
            "sourceAccountEmail": self.healthy_account_email,
            "model": model,
            "timeoutMs": self.timeout_ms
        })
    }
}

fn valid_email(value: &str) -> bool {
    value.len() <= MAX_EMAIL_LEN
        && !value.is_empty()
        && value.is_ascii()
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        && value.matches('@').count() == 1
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.is_empty()
                && !local.starts_with('.')
                && !local.ends_with('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !local.contains("..")
                && !domain.contains("..")
        })
}

fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
}

impl Plugin for AstraTurnState {
    fn create(config: Value, _context: PluginContext) -> Result<Self, String> {
        Self::from_config(config)
    }

    fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            METHOD_BEFORE_SEND => Ok(self.before_send(params)),
            "request.afterHeaders" | "request.resume" => Ok(json!({"action":"continue"})),
            "request.completed" | "request.failed" | "request.cancelled" => Ok(json!({})),
            "astra.status" => Ok(
                json!({"enabled": true, "thirdPartyLicense": include_str!("../THIRD_PARTY_LICENSES.txt")}),
            ),
            _ => Err(format!("未知方法: {method}")),
        }
    }
}

codey_plugin_sdk::export_plugin!(AstraTurnState);
