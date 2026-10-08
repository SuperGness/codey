use serde::{Deserialize, Serialize};

/// User-declared operating budget, never proof of upstream model capacity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelContextConfig {
    pub context_window_tokens: u64,
    #[serde(default)]
    pub auto_compact_token_limit: Option<u64>,
    #[serde(default)]
    pub reserve_output_tokens: Option<u64>,
}

impl ModelContextConfig {
    pub fn validate(&self) -> Result<(), String> {
        let window = self.context_window_tokens;
        if !(1_024..=10_000_000).contains(&window) {
            return Err("上下文窗口必须是 1024 到 10000000 之间的整数 Token".into());
        }
        let reserve = self.reserve_output_tokens.unwrap_or(0);
        if self.reserve_output_tokens == Some(0)
            || reserve >= window
            || (window - reserve) * 100 / window == 0
        {
            return Err("输出预留必须为正整数，并至少保留 1% 的上下文输入空间".into());
        }
        let effective = window * ((window - reserve) * 100 / window) / 100;
        if self
            .auto_compact_token_limit
            .is_some_and(|limit| limit == 0 || limit > effective.min(window * 9 / 10))
        {
            return Err(
                "压缩阈值必须为正整数，且不超过窗口的 90% 和扣除输出预留后的有效窗口".into(),
            );
        }
        Ok(())
    }
}
