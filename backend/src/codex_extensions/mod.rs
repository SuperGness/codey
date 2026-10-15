//! 宿主适配：系统路径由调用方注入，配置核心、凭证和协议检查分别维护。
mod probe;

use anyhow::{Context, Result, ensure};
use codey_codex_extensions::{ExtensionService, Scope};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::sync::Semaphore;

static REQUESTS: Semaphore = Semaphore::const_new(8);
static PROBES: Semaphore = Semaphore::const_new(2);

pub(crate) async fn dispatch(
    codex_home: PathBuf,
    data_dir: PathBuf,
    user_home: PathBuf,
    request: Value,
    can_show_panels: bool,
) -> Result<Value> {
    ensure!(data_dir.is_absolute(), "扩展数据目录必须为绝对路径");
    let permit = REQUESTS
        .try_acquire()
        .context("扩展管理请求过多，请稍后重试")?;
    let action = request
        .get("action")
        .and_then(Value::as_str)
        .context("缺少扩展管理操作")?;
    if action == "pick_project" || action == "pick_skill" {
        // 副本模式的后端是无窗口的后台服务，系统选择面板无法显示；与其静默
        // 返回「用户取消」，不如让页面提示直接填入绝对路径。
        ensure!(
            can_show_panels,
            "当前为副本模式，请直接填入目录的绝对路径"
        );
        return pick_directory(action == "pick_project").await;
    }
    let service = ExtensionService::new(codex_home, data_dir, user_home);
    if action == "test_mcp" {
        let _probe = PROBES
            .try_acquire()
            .context("已有连接测试进行中，请稍后重试")?;
        // UI 的确认只针对当前看到的版本，过期配置不得在确认后执行。
        let (scope, id, revision) = probe_request(&request)?;
        let project_dir = match &scope {
            Scope::Project { project_path } => Some(project_path.clone()),
            Scope::User => None,
        };
        let config =
            tokio::task::spawn_blocking(move || prepare_probe(&service, &scope, &id, &revision))
                .await
                .context("读取 MCP 配置的任务异常退出")??;
        return Ok(probe::test_mcp(config, project_dir).await);
    }
    // 持有 permit 至阻塞任务完成，即使前端断开也让事务完成或自行恢复。
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        service.dispatch(request)
    })
    .await
    .context("扩展管理任务异常退出")?
}

fn probe_request(request: &Value) -> Result<(Scope, String, String)> {
    ensure!(
        request.get("confirmed").and_then(Value::as_bool) == Some(true),
        "请先确认 MCP 连接测试"
    );
    let scope: Scope = serde_json::from_value(
        request
            .get("scope")
            .cloned()
            .unwrap_or(json!({"kind":"user"})),
    )
    .context("MCP 作用域无效")?;
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("缺少 MCP 标识")?;
    let revision = request
        .get("revision")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("请刷新后重新测试")?;
    Ok((scope, id.to_owned(), revision.to_owned()))
}

fn prepare_probe(
    service: &ExtensionService,
    scope: &Scope,
    id: &str,
    revision: &str,
) -> Result<Value> {
    ensure!(
        service.inventory(scope)?["revision"] == revision,
        "配置已变化，请刷新并重新确认测试"
    );
    let config = service.mcp_configuration(scope, id)?;
    ensure!(
        service.inventory(scope)?["revision"] == revision,
        "读取期间配置已变化，请重新确认测试"
    );
    Ok(config)
}

#[cfg(any(target_os = "macos", windows))]
async fn pick_directory(project: bool) -> Result<Value> {
    tokio::task::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(if project {
                "选择项目目录"
            } else {
                "选择包含 SKILL.md 的目录"
            })
            .pick_folder()
            .map(|path| json!({"path":path}))
            .unwrap_or(Value::Null)
    })
    .await
    .context("目录选择器异常退出")
}

#[cfg(not(any(target_os = "macos", windows)))]
async fn pick_directory(_: bool) -> Result<Value> {
    anyhow::bail!("当前平台请直接填写绝对路径")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_require_confirmation_and_the_reviewed_revision() {
        assert!(probe_request(&json!({"id":"demo","revision":"v1"})).is_err());
        assert!(probe_request(&json!({"confirmed":true,"id":"demo"})).is_err());
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        let config = home.join("config.toml");
        std::fs::write(&config, "[mcp_servers.demo]\ncommand = 'demo'\n").unwrap();
        let service = ExtensionService::new(home.clone(), home.join("data"), home.clone());
        let inventory = service.inventory(&Scope::User).unwrap();
        let revision = inventory["revision"].as_str().unwrap();
        assert_eq!(
            prepare_probe(&service, &Scope::User, "demo", revision).unwrap()["command"],
            "demo"
        );
        std::fs::write(config, "[mcp_servers.demo]\ncommand = 'changed'\n").unwrap();
        assert!(prepare_probe(&service, &Scope::User, "demo", revision).is_err());
        assert!(!home.join("data").exists());
    }
}
