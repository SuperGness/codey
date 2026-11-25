//! 渲染资产补丁的前置字面量与复合前置规则解析、匹配器。

use anyhow::{Context, Result, bail};
use memchr::memmem::{self, Finder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
pub(super) const PATCH_ANCHORS_START: &str = "/*codey-renderer-patch-anchors*/";
#[cfg(test)]
pub(super) const PATCH_ANCHORS_END: &str = "/*end-codey-renderer-patch-anchors*/";
pub(super) const PATCH_RULES_START: &str = "/*codey-renderer-patch-rules*/";
pub(super) const PATCH_RULES_END: &str = "/*end-codey-renderer-patch-rules*/";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchAnchorRule {
    all_of: Vec<String>,
}

/// 复合前置字面量匹配器：每条规则内所有字面量同时出现才命中对应 chunk。
#[derive(Clone, Debug)]
pub(crate) struct PatchAnchorMatcher {
    rules: Vec<Vec<String>>,
}

/// 为跨数千个 ASAR chunk 的扫描预先编译的 SIMD 子串查找器组。
pub(crate) struct CompiledPatchAnchorMatcher<'a> {
    rules: Vec<Vec<Finder<'a>>>,
}

impl CompiledPatchAnchorMatcher<'_> {
    pub(crate) fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.iter().all(|finder| finder.find(bytes).is_some()))
    }
}

impl PatchAnchorMatcher {
    /// 把平面前置字面量列表转换为单字面量规则集合，用于兼容旧调用与测试。
    pub(crate) fn from_single_anchors(anchors: &[String]) -> Self {
        Self {
            rules: anchors
                .iter()
                .filter(|anchor| !anchor.is_empty())
                .map(|anchor| vec![anchor.clone()])
                .collect(),
        }
    }

    /// 解析 `renderer_asset_patch.js` 内置的复合规则表。
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
        let mut rules = Vec::with_capacity(raw_rules.len());
        for rule in raw_rules {
            if rule.all_of.is_empty() || rule.all_of.iter().any(String::is_empty) {
                bail!("复合前置规则的 allOf 不能为空，也不能含空字符串");
            }
            rules.push(rule.all_of);
        }
        Ok(Self { rules })
    }

    pub(crate) fn matches(&self, source: &str) -> bool {
        self.matches_bytes(source.as_bytes())
    }

    pub(crate) fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.rules.iter().any(|rule| {
            rule.iter()
                .all(|lit| memmem::find(bytes, lit.as_bytes()).is_some())
        })
    }

    pub(crate) fn compile_finders(&self) -> CompiledPatchAnchorMatcher<'_> {
        CompiledPatchAnchorMatcher {
            rules: self
                .rules
                .iter()
                .map(|rule| rule.iter().map(|lit| Finder::new(lit.as_bytes())).collect())
                .collect(),
        }
    }

    /// 对规范化后的规则集合计算 SHA-256 摘要，规则变化时令扫描缓存失效。
    pub(crate) fn digest_hex(&self) -> String {
        let encoded = serde_json::to_vec(&self.rules).unwrap_or_default();
        let digest = Sha256::digest(&encoded);
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }
}

/// 读取页面补丁登记的前置字面量表。表随二进制内置，由单元测试保证可解析。
#[cfg(test)]
pub(super) fn renderer_patch_anchors(script: &str) -> Result<Vec<String>> {
    let start = script
        .find(PATCH_ANCHORS_START)
        .context("缺少前置字面量表起始标记")?
        + PATCH_ANCHORS_START.len();
    let end = start
        + script[start..]
            .find(PATCH_ANCHORS_END)
            .context("缺少前置字面量表结束标记")?;
    let anchors: Vec<String> =
        serde_json::from_str(&script[start..end]).context("前置字面量表不是 JSON 字符串数组")?;
    if anchors.is_empty() || anchors.iter().any(String::is_empty) {
        bail!("前置字面量表不能为空，也不能含空字符串");
    }
    Ok(anchors)
}
