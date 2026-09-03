use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const RULE_SCHEMA_VERSION: u32 = 1;
const LIVE_RULE_FILE: &str = "subagent-rules-v1.json";
const LAST_GOOD_RULE_FILE: &str = "subagent-rules-v1.last-good.json";
const EMBEDDED_RULES: &str = include_str!("../../resources/subagent-rules.default.json");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RoleAccess {
    ReadOnly,
    Write,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RolePolicy {
    pub access: RoleAccess,
    pub visual: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleEffect {
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleActor {
    Root,
    Child,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolClass {
    Read,
    Visual,
    Write,
    Command,
    Network,
    Collaboration,
    Spawn,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuleDefinition {
    pub id: String,
    pub priority: i32,
    pub effect: RuleEffect,
    #[serde(default)]
    pub actors: Vec<RuleActor>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub tool_classes: Vec<ToolClass>,
    pub explanation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuleSet {
    pub schema_version: u32,
    pub revision: u64,
    pub conflict_resolution: String,
    pub fallback: RuleEffect,
    pub roles: BTreeMap<String, RolePolicy>,
    pub rules: Vec<RuleDefinition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuleSource {
    Live,
    LastKnownGood,
    Embedded,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedRuleSet {
    pub rules: RuleSet,
    pub source: RuleSource,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RuleContext<'a> {
    pub actor: RuleActor,
    pub role: Option<&'a str>,
    pub tool_name: &'a str,
    pub tool_class: ToolClass,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuleDecision {
    pub effect: RuleEffect,
    pub rule_id: String,
    pub priority: i32,
    pub explanation: String,
    pub conflicts: Vec<String>,
}

impl RuleSet {
    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema_version == RULE_SCHEMA_VERSION,
            "子代理规则版本不受支持：{}",
            self.schema_version
        );
        anyhow::ensure!(
            self.conflict_resolution == "highest_priority_deny_wins",
            "子代理规则 conflictResolution 必须为 highest_priority_deny_wins"
        );
        anyhow::ensure!(!self.roles.is_empty(), "子代理规则没有角色定义");
        anyhow::ensure!(!self.rules.is_empty(), "子代理规则没有决策项");
        anyhow::ensure!(
            self.fallback == RuleEffect::Deny,
            "子代理规则 fallback 必须为 deny；动态规则只能收紧内置安全边界"
        );

        let mut ids = BTreeSet::new();
        for role in self.roles.keys() {
            anyhow::ensure!(!role.trim().is_empty(), "子代理规则包含空角色 ID");
        }
        for rule in &self.rules {
            anyhow::ensure!(
                !rule.id.trim().is_empty() && ids.insert(rule.id.as_str()),
                "子代理规则 ID 为空或重复：{}",
                rule.id
            );
            anyhow::ensure!(
                (-10_000..=10_000).contains(&rule.priority),
                "规则 {} 的 priority 超出范围",
                rule.id
            );
            anyhow::ensure!(
                !rule.explanation.trim().is_empty(),
                "规则 {} 缺少 explanation",
                rule.id
            );
            for role in &rule.roles {
                anyhow::ensure!(
                    self.roles.contains_key(role),
                    "规则 {} 引用了未知角色 {role}",
                    rule.id
                );
            }
        }
        self.validate_security_baseline()?;
        Ok(())
    }

    fn validate_security_baseline(&self) -> Result<()> {
        const EXPECTED_ROLES: [(&str, RoleAccess, bool); 6] = [
            ("codey_quick_scan", RoleAccess::ReadOnly, false),
            ("codey_deep_research", RoleAccess::ReadOnly, false),
            ("codey_visual_analysis", RoleAccess::ReadOnly, true),
            ("codey_worker", RoleAccess::Write, false),
            ("codey_visual_worker", RoleAccess::Write, true),
            ("default", RoleAccess::ReadOnly, false),
        ];
        anyhow::ensure!(
            self.roles.len() == EXPECTED_ROLES.len(),
            "子代理动态规则不能新增或删除运行时角色"
        );
        for (role, expected_access, expected_visual) in EXPECTED_ROLES {
            let policy = self
                .roles
                .get(role)
                .with_context(|| format!("子代理规则缺少受保护角色 {role}"))?;
            anyhow::ensure!(
                policy.access == expected_access && policy.visual == expected_visual,
                "子代理动态规则不能改变角色 {role} 的 access/visual 安全属性"
            );
            let spawn = self.evaluate(&RuleContext {
                actor: RuleActor::Child,
                role: Some(role),
                tool_name: "agents.spawn_agent",
                tool_class: ToolClass::Spawn,
            });
            anyhow::ensure!(
                spawn.effect == RuleEffect::Deny,
                "子代理规则必须拒绝角色 {role} 的嵌套派生"
            );
            let unknown = self.evaluate(&RuleContext {
                actor: RuleActor::Child,
                role: Some(role),
                tool_name: "unknown_tool",
                tool_class: ToolClass::Unknown,
            });
            anyhow::ensure!(
                unknown.effect == RuleEffect::Deny,
                "子代理规则必须拒绝角色 {role} 的未知工具"
            );
            if expected_access == RoleAccess::ReadOnly {
                let write = self.evaluate(&RuleContext {
                    actor: RuleActor::Child,
                    role: Some(role),
                    tool_name: "apply_patch",
                    tool_class: ToolClass::Write,
                });
                anyhow::ensure!(
                    write.effect == RuleEffect::Deny,
                    "子代理规则必须拒绝只读角色 {role} 的写入工具"
                );
            }
            let visual = self.evaluate(&RuleContext {
                actor: RuleActor::Child,
                role: Some(role),
                tool_name: "view_image",
                tool_class: ToolClass::Visual,
            });
            anyhow::ensure!(
                visual.effect
                    == if expected_visual {
                        RuleEffect::Allow
                    } else {
                        RuleEffect::Deny
                    },
                "子代理规则必须按 visual 属性限制角色 {role} 的视觉工具"
            );
        }
        for role in
            std::iter::once(None).chain(EXPECTED_ROLES.iter().map(|(role, _, _)| Some(*role)))
        {
            for tool in [
                "wait_agent",
                "list_agents",
                "agent_status",
                "interrupt_agent",
                "followup_task",
            ] {
                let collaboration = self.evaluate(&RuleContext {
                    actor: RuleActor::Child,
                    role,
                    tool_name: tool,
                    tool_class: ToolClass::Collaboration,
                });
                anyhow::ensure!(
                    collaboration.effect == RuleEffect::Deny,
                    "子代理规则必须拒绝角色 {} 的编排工具 {tool}",
                    role.unwrap_or("<unbound>")
                );
            }
        }
        Ok(())
    }

    fn validate_not_weaker_than(&self, baseline: &Self) -> Result<()> {
        // The rule language matches only finite actors, roles, tool classes and
        // exact tool names. Sampling every explicit name from both rule sets plus
        // one representative per class therefore covers every decision partition.
        let mut tool_names = BTreeSet::from([
            "read_file".to_string(),
            "apply_patch".to_string(),
            "exec_command".to_string(),
            "web_search".to_string(),
            "wait_agent".to_string(),
            "spawn_agent".to_string(),
            "__codey_unknown_probe__".to_string(),
        ]);
        for rule in self.rules.iter().chain(&baseline.rules) {
            tool_names.extend(rule.tools.iter().cloned());
        }
        let roles = std::iter::once(None)
            .chain(baseline.roles.keys().map(|role| Some(role.as_str())))
            .collect::<Vec<_>>();
        for actor in [RuleActor::Root, RuleActor::Child] {
            for role in &roles {
                for tool_name in &tool_names {
                    let tool_class = classify_tool(tool_name);
                    let context = RuleContext {
                        actor,
                        role: *role,
                        tool_name,
                        tool_class,
                    };
                    let baseline_decision = baseline.evaluate(&context);
                    let candidate_decision = self.evaluate(&context);
                    anyhow::ensure!(
                        baseline_decision.effect != RuleEffect::Deny
                            || candidate_decision.effect != RuleEffect::Allow,
                        "子代理动态规则不能放开内置基线拒绝的能力：actor={actor:?}, role={}, tool={tool_name}, class={tool_class:?}",
                        role.unwrap_or("<unbound>")
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn role_policy(&self, role: &str) -> Option<RolePolicy> {
        self.roles.get(role).copied()
    }

    pub(crate) fn evaluate(&self, context: &RuleContext<'_>) -> RuleDecision {
        let normalized_tool = normalize_tool_name(context.tool_name);
        let mut highest_priority = None;
        let mut winners = Vec::new();
        for rule in &self.rules {
            let matches = (rule.actors.is_empty() || rule.actors.contains(&context.actor))
                && (rule.roles.is_empty()
                    || context
                        .role
                        .is_some_and(|role| rule.roles.iter().any(|value| value == role)))
                && (rule.tools.is_empty()
                    || rule
                        .tools
                        .iter()
                        .any(|tool| tool.eq_ignore_ascii_case(&normalized_tool)))
                && (rule.tool_classes.is_empty()
                    || rule.tool_classes.contains(&context.tool_class));
            if !matches {
                continue;
            }
            match highest_priority {
                None => {
                    highest_priority = Some(rule.priority);
                    winners.push(rule);
                }
                Some(priority) if rule.priority > priority => {
                    highest_priority = Some(rule.priority);
                    winners.clear();
                    winners.push(rule);
                }
                Some(priority) if rule.priority == priority => winners.push(rule),
                Some(_) => {}
            }
        }
        if highest_priority.is_none() {
            return RuleDecision {
                effect: self.fallback,
                rule_id: "fallback".to_string(),
                priority: i32::MIN,
                explanation: format!(
                    "没有规则匹配 actor={:?}, role={}, tool={}, class={:?}；执行 fallback",
                    context.actor,
                    context.role.unwrap_or("<unbound>"),
                    normalized_tool,
                    context.tool_class
                ),
                conflicts: Vec::new(),
            };
        }
        winners.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let selected = winners
            .iter()
            .find(|rule| rule.effect == RuleEffect::Deny)
            .copied()
            .unwrap_or(winners[0]);
        RuleDecision {
            effect: selected.effect,
            rule_id: selected.id.clone(),
            priority: selected.priority,
            explanation: selected.explanation.clone(),
            conflicts: winners
                .iter()
                .filter(|rule| rule.id != selected.id)
                .map(|rule| rule.id.clone())
                .collect(),
        }
    }
}

pub(crate) fn embedded() -> &'static RuleSet {
    static RULES: OnceLock<RuleSet> = OnceLock::new();
    RULES.get_or_init(|| {
        let rules: RuleSet = serde_json::from_str(EMBEDDED_RULES)
            .expect("embedded subagent rules must be valid JSON");
        rules
            .validate()
            .expect("embedded subagent rules must satisfy the schema");
        rules
    })
}

pub(crate) fn load(state_root: &Path) -> LoadedRuleSet {
    // Codey invokes this binary once per Hook event, so a process-local cache
    // cannot be reused by the next rule evaluation. Keep the embedded baseline
    // cached, but read live/last-known-good inputs directly on every event.
    load_uncached(state_root)
}

fn load_uncached(state_root: &Path) -> LoadedRuleSet {
    let live_path = live_rule_path(state_root);
    match load_file(&live_path) {
        Ok(Some((rules, bytes))) => {
            if let Err(error) = persist_last_good(state_root, &bytes) {
                return LoadedRuleSet {
                    rules,
                    source: RuleSource::Live,
                    warning: Some(format!("保存 last-known-good 子代理规则失败：{error:#}")),
                };
            }
            LoadedRuleSet {
                rules,
                source: RuleSource::Live,
                warning: None,
            }
        }
        Ok(None) => LoadedRuleSet {
            rules: embedded().clone(),
            source: RuleSource::Embedded,
            warning: None,
        },
        Err(live_error) => {
            let last_good_path = state_root.join(LAST_GOOD_RULE_FILE);
            match load_file(&last_good_path) {
                Ok(Some((rules, _))) => LoadedRuleSet {
                    rules,
                    source: RuleSource::LastKnownGood,
                    warning: Some(format!(
                        "动态子代理规则无效，已回退 last-known-good：{live_error:#}"
                    )),
                },
                Ok(None) => LoadedRuleSet {
                    rules: embedded().clone(),
                    source: RuleSource::Embedded,
                    warning: Some(format!(
                        "动态子代理规则无效且无可用 last-known-good，已回退内置最小权限规则：{live_error:#}"
                    )),
                },
                Err(last_good_error) => LoadedRuleSet {
                    rules: embedded().clone(),
                    source: RuleSource::Embedded,
                    warning: Some(format!(
                        "动态子代理规则与 last-known-good 均无效，已回退内置最小权限规则；动态规则错误：{live_error:#}；last-known-good 错误：{last_good_error:#}"
                    )),
                },
            }
        }
    }
}

fn load_file(path: &Path) -> Result<Option<(RuleSet, Vec<u8>)>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("读取子代理规则失败：{}", path.display()));
        }
    };
    anyhow::ensure!(bytes.len() <= 256 * 1024, "子代理规则文件超过 256 KiB");
    let rules: RuleSet = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析子代理规则失败：{}", path.display()))?;
    rules.validate()?;
    rules.validate_not_weaker_than(embedded())?;
    Ok(Some((rules, bytes)))
}

fn persist_last_good(state_root: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(state_root)
        .with_context(|| format!("创建子代理规则目录失败：{}", state_root.display()))?;
    let path = state_root.join(LAST_GOOD_RULE_FILE);
    if fs::read(&path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    crate::fs_util::atomic_write(&path, bytes)
        .with_context(|| format!("写入 last-known-good 子代理规则失败：{}", path.display()))
}

pub(crate) fn live_rule_path(state_root: &Path) -> PathBuf {
    state_root.join(LIVE_RULE_FILE)
}

pub(crate) fn classify_tool(tool_name: &str) -> ToolClass {
    let normalized = normalize_tool_name(tool_name);
    if matches!(normalized.as_str(), "agent" | "spawn_agent") {
        ToolClass::Spawn
    } else if matches!(
        normalized.as_str(),
        "wait_agent"
            | "list_agents"
            | "agent_status"
            | "send_message"
            | "followup_task"
            | "interrupt_agent"
    ) {
        ToolClass::Collaboration
    } else if matches!(
        normalized.as_str(),
        "apply_patch" | "replace" | "write_file" | "edit_file" | "create_file" | "delete_file"
    ) {
        ToolClass::Write
    } else if matches!(
        normalized.as_str(),
        "web_search" | "websearch" | "web_run" | "open" | "find"
    ) {
        ToolClass::Network
    } else if matches!(
        normalized.as_str(),
        "read_file" | "inspect_local_file" | "grep" | "glob" | "tool_search"
    ) {
        ToolClass::Read
    } else if matches!(
        normalized.as_str(),
        "view_image" | "screenshot" | "cua_repl_js" | "cua_repl_js_reset" | "open_in_codex"
    ) {
        ToolClass::Visual
    } else if matches!(
        normalized.as_str(),
        "bash" | "shell" | "exec" | "exec_command" | "write_stdin" | "read_thread_terminal"
    ) {
        ToolClass::Command
    } else {
        ToolClass::Unknown
    }
}

pub(crate) fn normalize_tool_name(tool_name: &str) -> String {
    let normalized = tool_name.trim().to_ascii_lowercase();
    let canonical = match normalized.as_str() {
        "agent" | "agents.agent" | "agents/agent" | "agents::agent" | "agents__agent"
        | "agentsagent" => Some("agent"),
        "spawn_agent"
        | "agents.spawn_agent"
        | "agents/spawn_agent"
        | "agents::spawn_agent"
        | "agents__spawn_agent"
        | "agentsspawn_agent" => Some("spawn_agent"),
        "wait_agent" | "agents.wait_agent" | "agents/wait_agent" | "agents::wait_agent"
        | "agents__wait_agent" | "agentswait_agent" | "functions.wait" | "functions/wait"
        | "functions:wait" | "functions__wait" | "functions_wait" => Some("wait_agent"),
        "list_agents"
        | "agents.list_agents"
        | "agents/list_agents"
        | "agents::list_agents"
        | "agents__list_agents"
        | "agentslist_agents" => Some("list_agents"),
        "agent_status"
        | "agents.agent_status"
        | "agents/agent_status"
        | "agents::agent_status"
        | "agents__agent_status" => Some("agent_status"),
        "send_message"
        | "agents.send_message"
        | "agents/send_message"
        | "agents::send_message"
        | "agents__send_message"
        | "agentssend_message" => Some("send_message"),
        "followup_task"
        | "agents.followup_task"
        | "agents/followup_task"
        | "agents::followup_task"
        | "agents__followup_task"
        | "agentsfollowup_task" => Some("followup_task"),
        "interrupt_agent"
        | "agents.interrupt_agent"
        | "agents/interrupt_agent"
        | "agents::interrupt_agent"
        | "agents__interrupt_agent"
        | "agentsinterrupt_agent" => Some("interrupt_agent"),
        "apply_patch" | "functions.apply_patch" => Some("apply_patch"),
        "replace" | "mcp__codey_fastctx__replace" => Some("replace"),
        "write_file" => Some("write_file"),
        "edit_file" => Some("edit_file"),
        "create_file" => Some("create_file"),
        "delete_file" => Some("delete_file"),
        "read_file" => Some("read_file"),
        "inspect_local_file" | "mcp__codey_fastctx__inspect_local_file" => {
            Some("inspect_local_file")
        }
        "grep" | "mcp__codey_fastctx__grep" => Some("grep"),
        "glob" | "mcp__codey_fastctx__glob" => Some("glob"),
        "tool_search" => Some("tool_search"),
        "view_image" | "functions.view_image" => Some("view_image"),
        "mcp__cua_repl__js" => Some("cua_repl_js"),
        "mcp__cua_repl__js_reset" => Some("cua_repl_js_reset"),
        "mcp__codex_app__open_in_codex" => Some("open_in_codex"),
        "bash" => Some("bash"),
        "shell" => Some("shell"),
        "exec" => Some("exec"),
        "exec_command" => Some("exec_command"),
        "write_stdin" => Some("write_stdin"),
        "functions.exec" => Some("exec"),
        "functions.exec_command" => Some("exec_command"),
        "functions.write_stdin" => Some("write_stdin"),
        "read_thread_terminal" | "codex_app__read_thread_terminal" => Some("read_thread_terminal"),
        "web_search" => Some("web_search"),
        "websearch" => Some("websearch"),
        "web.run" | "web/run" | "web::run" | "web__run" | "web_run" | "webrun" => Some("web_run"),
        "open" => Some("open"),
        "find" => Some("find"),
        "screenshot" => Some("screenshot"),
        _ => None,
    };
    canonical.map_or(normalized, ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_rules_are_valid_and_deny_unknown_child_tools() {
        let rules = embedded();
        rules.validate().unwrap();
        let decision = rules.evaluate(&RuleContext {
            actor: RuleActor::Child,
            role: Some("codey_quick_scan"),
            tool_name: "mcp__mystery__mutate",
            tool_class: ToolClass::Unknown,
        });
        assert_eq!(decision.effect, RuleEffect::Deny);

        for (role, tool, class) in [
            (None, "functions.exec", ToolClass::Command),
            (None, "read_file", ToolClass::Read),
            (None, "wait_agent", ToolClass::Collaboration),
        ] {
            let decision = rules.evaluate(&RuleContext {
                actor: RuleActor::Child,
                role,
                tool_name: tool,
                tool_class: class,
            });
            assert_eq!(decision.effect, RuleEffect::Deny, "{role:?} {tool}");
        }
        assert_eq!(
            rules
                .evaluate(&RuleContext {
                    actor: RuleActor::Child,
                    role: None,
                    tool_name: "send_message",
                    tool_class: ToolClass::Collaboration,
                })
                .effect,
            RuleEffect::Allow
        );
        for role in ["codey_quick_scan", "codey_worker"] {
            assert_eq!(
                rules
                    .evaluate(&RuleContext {
                        actor: RuleActor::Child,
                        role: Some(role),
                        tool_name: "web.run",
                        tool_class: ToolClass::Network,
                    })
                    .effect,
                RuleEffect::Allow,
                "{role}"
            );
        }
        for role in ["codey_quick_scan", "codey_worker"] {
            assert_eq!(
                rules
                    .evaluate(&RuleContext {
                        actor: RuleActor::Child,
                        role: Some(role),
                        tool_name: "functions.exec",
                        tool_class: ToolClass::Command,
                    })
                    .effect,
                RuleEffect::Allow,
                "{role}"
            );
        }
    }

    #[test]
    fn untrusted_namespaces_cannot_spoof_trusted_tool_leaf_names() {
        for tool in [
            "mcp__evil__grep",
            "mcp__evil__replace",
            "mcp__evil__bash",
            "mcp__evil__send_message",
            "attacker.spawn_agent",
        ] {
            assert_eq!(classify_tool(tool), ToolClass::Unknown, "{tool}");
            assert_eq!(normalize_tool_name(tool), tool, "{tool}");
            assert_eq!(
                embedded()
                    .evaluate(&RuleContext {
                        actor: RuleActor::Child,
                        role: Some("codey_worker"),
                        tool_name: tool,
                        tool_class: classify_tool(tool),
                    })
                    .effect,
                RuleEffect::Deny,
                "{tool}"
            );
        }
        assert_eq!(classify_tool("mcp__codey_fastctx__grep"), ToolClass::Read);
        assert_eq!(
            classify_tool("mcp__codey_fastctx__replace"),
            ToolClass::Write
        );
    }

    #[test]
    fn highest_priority_uses_deny_to_resolve_a_tie() {
        let mut rules = embedded().clone();
        rules.rules.push(RuleDefinition {
            id: "tie-allow".into(),
            priority: 9_999,
            effect: RuleEffect::Allow,
            actors: vec![RuleActor::Child],
            roles: Vec::new(),
            tools: vec!["mystery".into()],
            tool_classes: Vec::new(),
            explanation: "allow".into(),
        });
        rules.rules.push(RuleDefinition {
            id: "tie-deny".into(),
            priority: 9_999,
            effect: RuleEffect::Deny,
            actors: vec![RuleActor::Child],
            roles: Vec::new(),
            tools: vec!["mystery".into()],
            tool_classes: Vec::new(),
            explanation: "deny".into(),
        });
        let decision = rules.evaluate(&RuleContext {
            actor: RuleActor::Child,
            role: None,
            tool_name: "mystery",
            tool_class: ToolClass::Unknown,
        });
        assert_eq!(decision.effect, RuleEffect::Deny);
        assert_eq!(decision.rule_id, "tie-deny");
        assert_eq!(decision.conflicts, ["tie-allow"]);
    }

    #[test]
    fn invalid_live_rules_fall_back_to_last_known_good_without_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = live_rule_path(temp.path());
        fs::write(&path, EMBEDDED_RULES).unwrap();
        let first = load(temp.path());
        assert_eq!(first.source, RuleSource::Live);

        fs::write(&path, b"{not-json").unwrap();
        let fallback = load(temp.path());
        assert_eq!(fallback.source, RuleSource::LastKnownGood);
        assert!(fallback.warning.is_some());
    }

    #[test]
    fn live_rules_cannot_weaken_the_embedded_security_baseline() {
        let mutations: [fn(&mut RuleSet); 3] = [
            |rules: &mut RuleSet| rules.fallback = RuleEffect::Allow,
            |rules: &mut RuleSet| {
                rules.roles.get_mut("codey_quick_scan").unwrap().access = RoleAccess::Write;
            },
            |rules: &mut RuleSet| {
                rules.rules.push(RuleDefinition {
                    id: "allow-specific-unknown-tool".into(),
                    priority: 9_999,
                    effect: RuleEffect::Allow,
                    actors: vec![RuleActor::Child],
                    roles: vec!["codey_quick_scan".into()],
                    tools: vec!["mcp__unsafe__escape".into()],
                    tool_classes: vec![ToolClass::Unknown],
                    explanation: "test-only permissive mutation".into(),
                });
            },
        ];
        for mutate in mutations {
            let temp = tempfile::tempdir().unwrap();
            let mut permissive = embedded().clone();
            permissive.revision = permissive.revision.saturating_add(1);
            mutate(&mut permissive);
            fs::write(
                live_rule_path(temp.path()),
                serde_json::to_vec(&permissive).unwrap(),
            )
            .unwrap();

            let loaded = load(temp.path());
            assert_eq!(loaded.source, RuleSource::Embedded);
            assert_eq!(loaded.rules.fallback, RuleEffect::Deny);
            assert_eq!(
                loaded.rules.roles["codey_quick_scan"].access,
                RoleAccess::ReadOnly
            );
            assert!(loaded.warning.is_some());
        }
    }

    #[test]
    fn invalid_last_known_good_is_reported_before_embedded_fallback() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(live_rule_path(temp.path()), b"{invalid-live").unwrap();
        fs::write(temp.path().join(LAST_GOOD_RULE_FILE), b"{invalid-lkg").unwrap();

        let loaded = load(temp.path());
        assert_eq!(loaded.source, RuleSource::Embedded);
        let warning = loaded.warning.unwrap();
        assert!(warning.contains("动态规则错误"));
        assert!(warning.contains("last-known-good 错误"));
    }

    #[test]
    fn atomic_same_size_rule_replacement_is_loaded_without_process_cache_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = live_rule_path(temp.path());
        let mut first = embedded().clone();
        first.revision = 2;
        let mut second = first.clone();
        second.revision = 3;
        let first_bytes = serde_json::to_vec(&first).unwrap();
        let second_bytes = serde_json::to_vec(&second).unwrap();
        assert_eq!(first_bytes.len(), second_bytes.len());
        fs::write(&path, first_bytes).unwrap();
        assert_eq!(load(temp.path()).rules.revision, 2);
        crate::fs_util::atomic_write(&path, &second_bytes).unwrap();
        assert_eq!(load(temp.path()).rules.revision, 3);
    }

    #[test]
    fn trusted_tool_aliases_share_canonical_classification() {
        assert_eq!(
            classify_tool("mcp__codey_fastctx__replace"),
            ToolClass::Write
        );
        assert_eq!(classify_tool("functions.exec"), ToolClass::Command);
        assert_eq!(classify_tool("agents.spawn_agent"), ToolClass::Spawn);
        assert_eq!(classify_tool("agents.wait_agent"), ToolClass::Collaboration);
        assert_eq!(
            classify_tool("agentssend_message"),
            ToolClass::Collaboration
        );
        assert_eq!(classify_tool("agentsspawn_agent"), ToolClass::Spawn);
        assert_eq!(classify_tool("web_search"), ToolClass::Network);
        assert_eq!(classify_tool("web.run"), ToolClass::Network);
        assert_eq!(classify_tool("web__run"), ToolClass::Network);
        for tool in [
            "functions.view_image",
            "screenshot",
            "mcp__cua_repl__js",
            "mcp__cua_repl__js_reset",
            "mcp__codex_app__open_in_codex",
        ] {
            assert_eq!(classify_tool(tool), ToolClass::Visual, "{tool}");
            for role in ["codey_visual_analysis", "codey_visual_worker"] {
                assert_eq!(
                    embedded()
                        .evaluate(&RuleContext {
                            actor: RuleActor::Child,
                            role: Some(role),
                            tool_name: tool,
                            tool_class: classify_tool(tool),
                        })
                        .effect,
                    RuleEffect::Allow,
                    "{role} {tool}"
                );
            }
            assert_eq!(
                embedded()
                    .evaluate(&RuleContext {
                        actor: RuleActor::Child,
                        role: Some("codey_quick_scan"),
                        tool_name: tool,
                        tool_class: classify_tool(tool),
                    })
                    .effect,
                RuleEffect::Deny,
                "{tool}"
            );
        }
    }
}
