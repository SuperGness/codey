use std::{path::Path, sync::Arc};

use serde_json::{Value, json};

use crate::commands::AppState;

async fn desktop_request(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let runtime = state
        .runtime
        .lock()
        .await
        .clone()
        .ok_or("Codex 桌面尚未启动")?;
    let websocket = runtime.renderer_websocket_url().await;
    crate::cdp::remote_control_request(&websocket, &args).await
}

pub(super) async fn open(state: &Arc<AppState>, id: &str) -> Result<(), String> {
    super::desktop::validate_thread(id)?;
    desktop_request(state, json!({"action":"resume","threadId":id})).await?;
    Ok(())
}

// Called only for the first submitted message; an untouched phone draft does
// not create a persisted thread or a second app-server.
pub(super) async fn create(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
    let project = saved_project(args).await?;
    let params = creation_params(&project, args)?;
    let cwd = params["cwd"].as_str().ok_or("项目目录无效")?;
    if !Path::new(cwd).is_dir() {
        return Err("项目目录不存在，请在电脑端检查工作区".into());
    }
    let result = desktop_request(state, json!({"action":"create","params":params})).await?;
    super::desktop::validate_thread(result["id"].as_str().ok_or("创建会话结果无效")?)?;
    Ok(result)
}

pub(super) async fn defaults(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
    let project = saved_project(args).await?;
    desktop_request(state, json!({"action":"defaults","cwd":project["cwd"]})).await
}

async fn saved_project(args: &Value) -> Result<Value, String> {
    let projects =
        tokio::task::spawn_blocking(|| super::store::projects(crate::codex_config::codex_home()))
            .await
            .map_err(|_| "读取项目任务失败")??;
    projects
        .as_array()
        .into_iter()
        .flatten()
        .find(|project| project["id"] == args["projectId"])
        .cloned()
        .ok_or_else(|| "请选择 Codex 中已保存的本地项目".into())
}

fn creation_params(project: &Value, args: &Value) -> Result<Value, String> {
    let cwd = project["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .ok_or("项目目录无效")?;
    let (_, _, turn) = super::desktop::action_params(&json!({}), "send", args)?;
    let mut params = json!({
        "input":turn["turnStart"]["request"]["input"],
        "clientUserMessageId":args["requestId"],
        "cwd":cwd, "workspaceRoots":project["rootPaths"].as_array().filter(|roots| !roots.is_empty()).cloned().unwrap_or_else(|| vec![json!(cwd)]),
        "workspaceKind":"project", "projectAssignment":{"projectId":project["id"],"projectKind":"local"},
        "useAppServerPermissionDefault":true,
        "attachments":[], "commentAttachments":[], "collaborationMode":null,
    });
    if args.get("model").is_some()
        || args.get("effort").is_some()
        || args.get("serviceTier").is_some()
        || args.get("permissionMode").is_some()
    {
        let (_, _, settings) =
            super::desktop::action_params(&json!({"cwd":cwd}), "settings", args)?;
        let settings = &settings["threadSettings"];
        if settings.get("effort").is_some() && settings.get("model").is_none() {
            return Err("请先选择模型再设置思考程度".into());
        }
        if let Some(model) = settings.get("model") {
            params["collaborationMode"] = json!({"mode":"default","settings":{"model":model,"reasoning_effort":settings["effort"],"developer_instructions":null}});
        }
        if let Some(tier) = settings.get("serviceTier") {
            params["serviceTier"] = tier.clone();
        }
        if settings.get("permissions").is_some() {
            params["useAppServerPermissionDefault"] = json!(false);
            params["permissionsConfig"] = json!({
                "activePermissionProfile":settings["activePermissionProfile"],
                "approvalPolicy":settings["approvalPolicy"], "approvalsReviewer":settings["approvalsReviewer"],
                "sandboxPolicy":settings["sandboxPolicy"], "runtimeWorkspaceRoots":params["workspaceRoots"],
            });
        }
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_message_uses_native_creation_without_a_manual_title() {
        let project =
            json!({"id":"p","cwd":"E:/code/codey","rootPaths":["E:/code/codey","E:/other"]});
        let args = json!({"text":"开始任务","requestId":uuid::Uuid::new_v4().to_string()});
        let params = creation_params(&project, &args).unwrap();
        assert_eq!(params["input"][0]["text"], "开始任务");
        assert_eq!(
            params["projectAssignment"],
            json!({"projectId":"p","projectKind":"local"})
        );
        assert_eq!(params["workspaceRoots"], project["rootPaths"]);
        assert_eq!(params["useAppServerPermissionDefault"], true);
        assert!(params.get("initialTitle").is_none());
        assert!(params.get("permissionsConfig").is_none());
        assert!(params.get("serviceTier").is_none());
    }

    #[test]
    fn creation_validates_messages_and_only_applies_explicit_settings() {
        let project = json!({"id":"p","cwd":"E:/code/codey"});
        let args = json!({"text":"测试","requestId":uuid::Uuid::new_v4().to_string(),"model":"route/model","effort":"high","permissionMode":"read-only"});
        let params = creation_params(&project, &args).unwrap();
        assert_eq!(
            params["collaborationMode"]["settings"]["model"],
            "route/model"
        );
        assert_eq!(
            params["permissionsConfig"]["activePermissionProfile"]["id"],
            ":read-only"
        );
        assert_eq!(params["useAppServerPermissionDefault"], false);
        for patch in [
            json!({"text":" "}),
            json!({"text":"a".repeat(100_001)}),
            json!({"requestId":"invalid"}),
            json!({"effort":"invalid"}),
            json!({"permissionMode":"invalid"}),
        ] {
            let mut invalid = args.clone();
            for (key, value) in patch.as_object().unwrap() {
                invalid[key] = value.clone();
            }
            assert!(creation_params(&project, &invalid).is_err());
        }
        assert!(creation_params(&json!({"id":"p"}), &args).is_err());
        assert!(
            creation_params(
                &project,
                &json!({"text":"ok","requestId":args["requestId"],"effort":"high"})
            )
            .is_err()
        );
    }

    #[test]
    fn creation_passes_fast_and_explicit_standard_to_the_first_turn() {
        let project = json!({"id":"p","cwd":"E:/code/codey"});
        for tier in [json!("priority"), json!("default"), Value::Null] {
            let args = json!({"text":"开始","requestId":uuid::Uuid::new_v4().to_string(),"serviceTier":tier});
            let params = creation_params(&project, &args).unwrap();
            assert_eq!(params.get("serviceTier"), Some(&tier));
            assert_eq!(params["collaborationMode"], Value::Null);
            assert_eq!(params["useAppServerPermissionDefault"], true);
        }
        let args = json!({"text":"开始","requestId":uuid::Uuid::new_v4().to_string(),"serviceTier":"fast"});
        assert!(
            creation_params(&project, &args)
                .unwrap_err()
                .contains("速度模式")
        );
    }

    #[test]
    fn first_message_supports_photos_without_text_and_rejects_invalid_images() {
        let project = json!({"id":"p","cwd":"E:/code/codey"});
        let url = "data:image/gif;base64,R0lGODlh";
        let mut args = json!({"text":"","images":[{"url":url}],"requestId":uuid::Uuid::new_v4().to_string()});
        let params = creation_params(&project, &args).unwrap();
        assert_eq!(params["input"], json!([{"type":"image","url":url}]));
        assert_eq!(params["attachments"], json!([]));
        assert_eq!(params["useAppServerPermissionDefault"], true);
        args["images"][0]["url"] = json!("https://example.test/private.png");
        assert!(creation_params(&project, &args).is_err());
    }
}
