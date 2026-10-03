// Small shared client helpers.
use serde_json::Value;

pub fn json_or_text_error(text: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
        if let Some(msg) = parsed.pointer("/error/message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
    }
    text.to_string()
}
