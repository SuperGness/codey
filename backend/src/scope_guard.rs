//! Scope guard: keeps a short proportionality rule in the model context.
//!
//! Desktop rewrites `developer_instructions` per thread, so the rule travels
//! through Codex lifecycle hooks instead. `SessionStart` fires for new threads
//! and again after compaction installs a summary, which is exactly when an
//! earlier injection would have been dropped from history. Resumed threads
//! already carry the message in their persisted history, so the matcher skips
//! them. Spawned subagents get a shorter variant through `SubagentStart`.

use std::ffi::OsStr;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const HOOK_ARGUMENT: &str = "--codey-scope-guard-hook";
pub(crate) const HOOK_TIMEOUT_SECONDS: u64 = 5;
/// Matched against the `SessionStart` source so resumes skip the process
/// spawn; the handler applies the same rule.
pub(crate) const SESSION_START_MATCHER: &str = "^(startup|clear|compact)$";
const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;
const ROOT_GUIDANCE: &str = include_str!("../resources/scope-guard/root.md");
const CHILD_GUIDANCE: &str = include_str!("../resources/scope-guard/child.md");

#[derive(Debug, Deserialize)]
struct HookInput {
    hook_event_name: String,
    #[serde(default)]
    source: Option<String>,
}

pub fn run_hook_if_requested() -> Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(HOOK_ARGUMENT)) {
        return Ok(false);
    }
    let raw = crate::hook_io::read_stdin_bounded(
        MAX_HOOK_INPUT_BYTES,
        "读取 Codex 范围约束 Hook 输入失败",
    )?;
    // The rule is advisory context, not a gate: an unreadable payload simply
    // injects nothing, matching a launch without this hook.
    let output = if raw.len() as u64 > MAX_HOOK_INPUT_BYTES {
        json!({})
    } else {
        match serde_json::from_slice::<HookInput>(&raw) {
            Ok(input) => hook_output(&input),
            Err(error) => {
                eprintln!("Codey 范围约束 Hook 输入无效，已跳过：{error:#}");
                json!({})
            }
        }
    };
    crate::hook_io::write_output(&output, "序列化 Codex 范围约束 Hook 输出失败")?;
    Ok(true)
}

fn hook_output(input: &HookInput) -> Value {
    let guidance = match input.hook_event_name.as_str() {
        "SessionStart"
            if matches!(
                input.source.as_deref(),
                Some("startup" | "clear" | "compact")
            ) =>
        {
            ROOT_GUIDANCE
        }
        "SubagentStart" => CHILD_GUIDANCE,
        _ => return json!({}),
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": input.hook_event_name,
            "additionalContext": guidance.trim(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(event: &str, source: Option<&str>) -> Value {
        hook_output(&HookInput {
            hook_event_name: event.to_string(),
            source: source.map(str::to_string),
        })
    }

    #[test]
    fn new_and_compacted_sessions_receive_root_guidance() {
        for source in ["startup", "clear", "compact"] {
            let output = output("SessionStart", Some(source));
            assert_eq!(
                output["hookSpecificOutput"]["hookEventName"],
                "SessionStart"
            );
            assert_eq!(
                output["hookSpecificOutput"]["additionalContext"],
                ROOT_GUIDANCE.trim()
            );
        }
    }

    #[test]
    fn resumed_sessions_keep_their_persisted_copy() {
        assert_eq!(output("SessionStart", Some("resume")), json!({}));
        assert_eq!(output("SessionStart", None), json!({}));
    }

    #[test]
    fn subagents_receive_the_short_variant() {
        let output = output("SubagentStart", None);
        assert_eq!(
            output["hookSpecificOutput"]["hookEventName"],
            "SubagentStart"
        );
        assert_eq!(
            output["hookSpecificOutput"]["additionalContext"],
            CHILD_GUIDANCE.trim()
        );
    }

    #[test]
    fn other_events_inject_nothing() {
        assert_eq!(output("UserPromptSubmit", None), json!({}));
    }

    #[test]
    fn guidance_fits_the_default_hook_context_budget() {
        // Codex spills hook context above roughly 2,500 tokens (bytes / 4).
        assert!(ROOT_GUIDANCE.len() < 6_000, "{}", ROOT_GUIDANCE.len());
        assert!(CHILD_GUIDANCE.len() < 1_500, "{}", CHILD_GUIDANCE.len());
    }
}
