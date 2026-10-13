use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use futures_util::{StreamExt, TryStreamExt, stream};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::{AppState, prompt_optimization, string_argument};
use crate::{codex_config::codex_home, config::PromptOptimizationConfig, conversation_git};

struct PreviewWork {
    session: String,
    cancellation: Arc<conversation_git::Cancellation>,
    created: Instant,
}

static WORK: OnceLock<Mutex<HashMap<String, PreviewWork>>> = OnceLock::new();
static PREVIEW_LIMIT: Semaphore = Semaphore::const_new(2);
static REMOTE_LIMIT: Semaphore = Semaphore::const_new(4);
static STATUS_LIMIT: Semaphore = Semaphore::const_new(4);
static EXECUTE_LIMIT: Semaphore = Semaphore::const_new(4);
static BLOCKING_LIMIT: Semaphore = Semaphore::const_new(4);

struct PreviewGuard {
    id: String,
    cancellation: Arc<conversation_git::Cancellation>,
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut work) = WORK.get_or_init(Mutex::default).lock() {
            work.remove(&self.id);
        }
    }
}

fn register_work(
    session: &str,
    id: &str,
    cancel: bool,
) -> Result<Arc<conversation_git::Cancellation>, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "提交预览请求标识无效")?;
    let mut work = WORK
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "提交预览状态不可用")?;
    work.retain(|_, item| item.created.elapsed() < Duration::from_secs(300));
    if let Some(item) = work.get(id) {
        if item.session != session {
            return Err("提交预览不属于当前对话".into());
        }
        if cancel {
            item.cancellation.cancel();
            return Ok(Arc::clone(&item.cancellation));
        }
        return Err(if item.cancellation.check().is_err() {
            "提交预览已取消"
        } else {
            "提交预览请求已在执行"
        }
        .into());
    }
    if work.len() >= 64 {
        return Err("提交预览请求过多，请稍后重试".into());
    }
    let cancellation = Arc::new(conversation_git::Cancellation::default());
    if cancel {
        cancellation.cancel();
    }
    work.insert(
        id.into(),
        PreviewWork {
            session: session.into(),
            cancellation: Arc::clone(&cancellation),
            created: Instant::now(),
        },
    );
    Ok(cancellation)
}

async fn blocking<T: Send + 'static>(
    limit: &'static Semaphore,
    cancellation: Arc<conversation_git::Cancellation>,
    task: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, String> {
    let permit = limit.acquire().await.map_err(|_| "Git 任务队列不可用")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        conversation_git::with_cancellation(cancellation, task)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("{error:#}"))
}

async fn analyze_chunks<F, Fut>(inputs: Vec<String>, analyze: F) -> Result<Vec<String>, String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    stream::iter(inputs)
        .map(analyze)
        .buffered(2)
        .try_collect()
        .await
}

pub(super) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let session = string_argument(args, "sessionId")?;
    let session = crate::session_metadata::normalize_session_id(&session).to_string();
    if command == "conversation_git_cancel" {
        let id = string_argument(args, "requestId")?;
        register_work(&session, &id, true)?;
        conversation_git::cancel_preview(&session, &id, args.get("token").and_then(Value::as_str))
            .map_err(|error| error.to_string())?;
        return Ok(json!({"status":"cancelled"}));
    }
    let config = state.config.read().await.conversation_git.clone();
    if !config.enabled {
        return if command == "conversation_git_status" {
            Ok(json!({"visible": false, "reason": "对话 Git 提交增强已关闭"}))
        } else {
            Err("对话 Git 提交增强已关闭".into())
        };
    }
    config.validate()?;
    if command == "conversation_git_status" {
        let Ok(permit) = STATUS_LIMIT.try_acquire() else {
            return Ok(
                json!({"visible":false,"unavailable":true,"reason":"Git 状态查询繁忙，请稍后重试"}),
            );
        };
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
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
        let permit = EXECUTE_LIMIT
            .try_acquire()
            .map_err(|_| "Git 提交繁忙，请稍后重试")?;
        return tokio::task::spawn_blocking(move || {
            let _permit = permit;
            conversation_git::execute(codex_home(), &session, &token, &config.model, push)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"));
    }
    let id = match args.get("requestId") {
        Some(_) => string_argument(args, "requestId")?,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let cancellation = register_work(&session, &id, false)?;
    let _guard = PreviewGuard {
        id: id.clone(),
        cancellation: Arc::clone(&cancellation),
    };
    let work = async {
        if command == "conversation_git_push_preview" {
            let token = string_argument(args, "token")?;
            let model = config.model.clone();
            let result = blocking(&REMOTE_LIMIT, Arc::clone(&cancellation), move || {
                conversation_git::prepare_push_preview(&session, &token, &model)
            })
            .await?;
            if state.config.read().await.conversation_git != config {
                return Err("提交模型设置已变化，请重新预览".into());
            }
            return Ok(result);
        }
        let _permit = PREVIEW_LIMIT
            .try_acquire()
            .map_err(|_| "提交分析繁忙，请稍后重试")?;
        let snapshot_session = session.clone();
        let snapshot = blocking(&BLOCKING_LIMIT, Arc::clone(&cancellation), move || {
            conversation_git::snapshot(codex_home(), &snapshot_session)
        })
        .await?;
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
        let client = prompt_optimization::optimizer_client(true)?;
        let mut messages = analyze_chunks(inputs, |input| {
            let request = &request;
            async move {
                let message =
                    crate::prompt_optimization::optimize_prompt_resolved(client, request, &input)
                        .await
                        .map_err(|error| format!("生成提交说明失败：{error}"))?;
                conversation_git::validate_message(&message).map_err(|error| error.to_string())
            }
        })
        .await?;
        let message = if messages.len() == 1 {
            messages.pop().unwrap()
        } else {
            let mut summary_request = request.clone();
            summary_request.instruction = format!(
                "{} 输入为分段分析同一次提交得到的 JSON 提交说明列表。合并为一个说明，正文由你按实际改动判断是否需要；有正文时保留具体信息，只使用列表明确描述的改动，scope 可选，不新增推断。",
                conversation_git::MESSAGE_INSTRUCTION.replace(
                    "输入只包含本次实际提交的完整 diff",
                    "输入只包含本次实际提交的分析结果"
                )
            );
            let input = serde_json::to_string(&messages).map_err(|error| error.to_string())?;
            let message = crate::prompt_optimization::optimize_prompt_resolved(
                client,
                &summary_request,
                &input,
            )
            .await
            .map_err(|error| format!("汇总提交说明失败：{error}"))?;
            conversation_git::validate_message(&message).map_err(|error| error.to_string())?
        };
        let snapshot = blocking(&BLOCKING_LIMIT, Arc::clone(&cancellation), move || {
            conversation_git::revalidate_snapshot(codex_home(), &snapshot)?;
            Ok(snapshot)
        })
        .await?;
        if state.config.read().await.conversation_git != config {
            return Err("提交模型设置已变化，请重新预览".into());
        }
        blocking(&BLOCKING_LIMIT, Arc::clone(&cancellation), move || {
            conversation_git::save_local_preview(snapshot, message, config.model, id)
        })
        .await
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err("提交预览已取消".into()),
        result = tokio::time::timeout(Duration::from_secs(300), work) => {
            result.map_err(|_| "提交预览超时，请拆分改动或稍后重试".to_string())?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chunk_analysis_is_bounded_ordered_and_dropped_on_cancellation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Active<'a>(&'a AtomicUsize);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let inputs = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        let result = analyze_chunks(inputs.clone(), |input| {
            let active = &active;
            let maximum = &maximum;
            async move {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                let _guard = Active(active);
                maximum.fetch_max(count, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(if input == "first" { 20 } else { 1 }))
                    .await;
                Ok(input)
            }
        })
        .await
        .unwrap();
        assert_eq!(result, inputs);
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        let pending = analyze_chunks(inputs, |_| async {
            active.fetch_add(1, Ordering::SeqCst);
            let _guard = Active(&active);
            std::future::pending::<Result<String, String>>().await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pending)
                .await
                .is_err()
        );
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancellation_is_bound_to_request_and_handles_queued_requests() {
        let id = uuid::Uuid::new_v4().to_string();
        let cancellation = register_work("session", &id, false).unwrap();
        let _guard = PreviewGuard {
            id: id.clone(),
            cancellation: Arc::clone(&cancellation),
        };
        assert!(register_work("other", &id, true).is_err());
        assert!(cancellation.check().is_ok());
        register_work("session", &id, true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
            .await
            .unwrap();
        assert!(cancellation.check().is_err());
        let queued = uuid::Uuid::new_v4().to_string();
        register_work("session", &queued, true).unwrap();
        assert!(register_work("session", &queued, false).is_err());
        let another = uuid::Uuid::new_v4().to_string();
        let current = register_work("session", &another, false).unwrap();
        let _current = PreviewGuard {
            id: another,
            cancellation: Arc::clone(&current),
        };
        assert!(current.check().is_ok());
    }
}
