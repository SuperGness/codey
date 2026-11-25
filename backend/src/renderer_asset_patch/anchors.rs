//! 渲染资产补丁的复合前置规则解析与预编译匹配器。

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use memchr::memmem::Finder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const PATCH_RULES_START: &str = "/*codey-renderer-patch-rules*/";
pub(super) const PATCH_RULES_END: &str = "/*end-codey-renderer-patch-rules*/";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum AssetRuleKind {
    #[default]
    Js,
    Css,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchAnchorRule {
    #[serde(default)]
    kind: AssetRuleKind,
    all_of: Vec<String>,
}

#[derive(Debug)]
struct CompiledMatcher {
    js_rules: Vec<Vec<Finder<'static>>>,
    css_rules: Vec<Vec<Finder<'static>>>,
    digest_hex: String,
}

/// 复合前置字面量匹配器：按 JS / CSS 分类持有预编译 SIMD 查找器，
/// 每条规则内所有字面量同时出现才命中对应资产。
#[derive(Clone, Debug)]
pub(crate) struct PatchAnchorMatcher {
    inner: Arc<CompiledMatcher>,
}

impl PatchAnchorMatcher {
    /// 解析 `renderer_asset_patch.js` 内置的复合规则表并预编译查找器。
    pub(crate) fn for_script(script: &str) -> Result<Self> {
        let start = script
            .find(PATCH_RULES_START)
            .context("缺少复合前置规则表起始标记")?
            + PATCH_RULES_START.len();
        let end = start
            + script[start..]
                .find(PATCH_RULES_END)
                .context("缺少复合前置规则表结束标记")?;
        let raw_rules: Vec<PatchAnchorRule> = serde_json::from_str(&script[start..end])
            .context("复合前置规则表不是有效 JSON 数组")?;
        if raw_rules.is_empty() {
            bail!("复合前置规则表不能为空");
        }
        let mut js_rules = Vec::new();
        let mut css_rules = Vec::new();
        for rule in &raw_rules {
            if rule.all_of.is_empty() || rule.all_of.iter().any(String::is_empty) {
                bail!("复合前置规则的 allOf 不能为空，也不能含空字符串");
            }
            let compiled: Vec<Finder<'static>> = rule
                .all_of
                .iter()
                .map(|lit| Finder::new(lit.as_bytes()).into_owned())
                .collect();
            match rule.kind {
                AssetRuleKind::Js => js_rules.push(compiled),
                AssetRuleKind::Css => css_rules.push(compiled),
            }
        }
        let encoded = serde_json::to_vec(&raw_rules).unwrap_or_default();
        let digest = Sha256::digest(&encoded);
        let mut digest_hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut digest_hex, "{byte:02x}");
        }
        Ok(Self {
            inner: Arc::new(CompiledMatcher {
                js_rules,
                css_rules,
                digest_hex,
            }),
        })
    }

    pub(crate) fn matches_js_bytes(&self, bytes: &[u8]) -> bool {
        Self::matches_rules(&self.inner.js_rules, bytes)
    }

    pub(crate) fn matches_css_bytes(&self, bytes: &[u8]) -> bool {
        Self::matches_rules(&self.inner.css_rules, bytes)
    }

    pub(crate) fn matches_asset_bytes(&self, is_css: bool, bytes: &[u8]) -> bool {
        if is_css {
            self.matches_css_bytes(bytes)
        } else {
            self.matches_js_bytes(bytes)
        }
    }

    pub(crate) fn matches_asset(&self, url_or_path: &str, source: &str) -> bool {
        let path = url_or_path.split(['?', '#']).next().unwrap_or(url_or_path);
        let is_css = path.len() >= 4 && path[path.len() - 4..].eq_ignore_ascii_case(".css");
        self.matches_asset_bytes(is_css, source.as_bytes())
    }

    #[cfg(test)]
    pub(crate) fn matches(&self, source: &str) -> bool {
        self.matches_js_bytes(source.as_bytes())
    }

    fn matches_rules(rules: &[Vec<Finder<'static>>], bytes: &[u8]) -> bool {
        rules
            .iter()
            .any(|rule| rule.iter().all(|finder| finder.find(bytes).is_some()))
    }

    /// 规则集合的 SHA-256 摘要，规则变化时令离线扫描缓存失效。
    pub(crate) fn digest_hex(&self) -> &str {
        &self.inner.digest_hex
    }
}
