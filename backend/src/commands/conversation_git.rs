use std::sync::Arc;

use serde_json::{Value, json};

use super::{AppState, prompt_optimization, string_argument};
use crate::{codex_config::codex_home, config::PromptOptimizationConfig, conversation_git};

pub(super) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let config = state.config.read().await.conversation_git.clone();
    if !config.enabled {
        return if command == "conversation_git_status" {
            Ok(json!({"visible": false, "reason": "对话 Git 提交增强已关闭"}))
        } else {
            Err("对话 Git 提交增强已关闭".into())
        };
    }
    config.validate()?;
    let session = string_argument(args, "sessionId")?;
    if command == "conversation_git_status" {
        let result = tokio::task::spawn_blocking(move || {
            conversation_git::display_status(codex_home(), &session)
        })
        .await
        .map_err(|error| error.to_string())?;
        return Ok(match result {
            Ok(value) => value,
            Err(error) => {
                json!({"visible": false, "unavailable": true, "reason": format!("{error:#}")})
            }
        });
    }
    if command == "conversation_git_execute" {
        let token = string_argument(args, "token")?;
        let push = args.get("push").and_then(Value::as_bool).unwrap_or(true);
        return tokio::task::spawn_blocking(move || {
            conversation_git::execute(codex_home(), &session, &token, &config.model, push)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"));
    }
    let snapshot_session = session.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        conversation_git::snapshot(codex_home(), &snapshot_session)
    })
    .await
    .map_err(|error| error.to_string())?;
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return Err(format!("{error:#}")),
    };
    let model_config = PromptOptimizationConfig {
        enabled: true,
        mode: crate::config::PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.into(),
        model: config.model.clone(),
        instruction: conversation_git::MESSAGE_INSTRUCTION.into(),
        ..PromptOptimizationConfig::default()
    };
    let request = prompt_optimization::resolve_request_config(state, &model_config)
        .await
        .map_err(|error| {
            format!(
                "提交分析模型不可用：{}",
                error
                    .replace("提示词优化", "Git 提交分析")
                    .replace("，或改用手动配置", "")
            )
        })?;
    let inputs =
        conversation_git::analysis_inputs(&snapshot.diff).map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    for input in inputs {
        let message = crate::prompt_optimization::optimize_prompt_resolved(
            prompt_optimization::optimizer_client(true)?,
            &request,
            &input,
        )
        .await
        .map_err(|error| format!("生成提交说明失败：{error}"))?;
        messages.push(
            conversation_git::validate_message_for_diff(&message, &input)
                .map_err(|error| error.to_string())?,
        );
    }
    let message = if messages.len() == 1 {
        messages.pop().unwrap()
    } else {
        let mut summary_request = request.clone();
        summary_request.instruction = format!(
            "{} 输入为分段分析同一次多文件提交得到的 JSON 提交说明列表。合并为一个说明，必须保留具体正文，只使用列表明确描述的改动，scope 可选，不新增推断。",
            conversation_git::MESSAGE_INSTRUCTION.replace(
                "输入只包含本次实际提交的完整 diff",
                "输入只包含本次实际提交的分析结果"
            )
        );
        let input = serde_json::to_string(&messages).map_err(|error| error.to_string())?;
        let message = crate::prompt_optimization::optimize_prompt_resolved(
            prompt_optimization::optimizer_client(true)?,
            &summary_request,
            &input,
        )
        .await
        .map_err(|error| format!("汇总提交说明失败：{error}"))?;
        conversation_git::validate_message(&message).map_err(|error| error.to_string())?
    };
    let message = conversation_git::validate_message_for_diff(&message, &snapshot.diff)
        .map_err(|error| error.to_string())?;
    let current =
        tokio::task::spawn_blocking(move || conversation_git::snapshot(codex_home(), &session))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("{error:#}"))?;
    if current != snapshot {
        return Err("生成说明期间文件或 Git 状态发生变化，请重新预览".into());
    }
    if state.config.read().await.conversation_git != config {
        return Err("提交模型设置已变化，请重新预览".into());
    }
    conversation_git::save_preview(snapshot, message, config.model)
        .map_err(|error| error.to_string())
}
