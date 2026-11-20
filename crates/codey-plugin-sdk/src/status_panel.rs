//! Read-only status cards rendered by Codey in its existing desktop entry.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const CAPABILITY: &str = "ui.status.v1";
pub const READ_METHOD: &str = "ui.status.read";
pub const MAX_ROWS: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub reading: Reading,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Reading {
    Ready {
        value: String,
        observed_at: i64,
    },
    Stale {
        value: String,
        observed_at: i64,
        message: String,
    },
    Unavailable {
        message: String,
    },
}

fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

fn observed_at(value: i64) -> bool {
    (0..=8_640_000_000_000_000).contains(&value)
}

impl Panel {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.rows.len() > MAX_ROWS {
            return Err("状态卡片最多显示 12 行");
        }
        let mut ids = HashSet::new();
        for row in &self.rows {
            if row.id.is_empty()
                || row.id.len() > 48
                || !row
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                || !ids.insert(row.id.as_str())
                || !text(&row.label, 40)
                || row.detail.as_ref().is_some_and(|detail| !text(detail, 48))
            {
                return Err("状态行标识或标签无效");
            }
            let valid = match &row.reading {
                Reading::Ready {
                    value,
                    observed_at: at,
                } => text(value, 40) && observed_at(*at),
                Reading::Stale {
                    value,
                    observed_at: at,
                    message,
                } => text(value, 40) && observed_at(*at) && text(message, 64),
                Reading::Unavailable { message } => text(message, 64),
            };
            if !valid {
                return Err("状态读数或更新时间无效");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zero_stale_and_missing_are_distinct() {
        let payload = json!({"rows":[
            {"id":"zero","label":"可用额度","reading":{"state":"ready","value":"0","observedAt":1723190000000_i64}},
            {"id":"stale","label":"上次读数","reading":{"state":"stale","value":"4.20","observedAt":1723190000000_i64,"message":"查询超时"}},
            {"id":"missing","label":"新服务","reading":{"state":"unavailable","message":"待配置"}}
        ]});
        let panel: Panel = serde_json::from_value(payload.clone()).unwrap();
        panel.validate().unwrap();
        assert_eq!(serde_json::to_value(&panel).unwrap(), payload);
        let mut duplicated = panel;
        duplicated.rows.push(duplicated.rows[0].clone());
        assert!(duplicated.validate().is_err());
    }

    #[test]
    fn rejects_interactive_payloads_and_invalid_times() {
        assert!(serde_json::from_value::<Panel>(json!({"rows":[],"html":"<script>"})).is_err());
        let panel: Panel = serde_json::from_value(json!({"rows":[{"id":"service","label":"Service","reading":{"state":"ready","value":"OK","observedAt":i64::MAX}}]})).unwrap();
        assert!(panel.validate().is_err());
    }
}
