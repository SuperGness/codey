//! Official (ChatGPT) account management commands: add accounts through the
//! OAuth login flow, keep several of them, and pick exactly one as the default
//! that Codex runs as.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    AppState, LaunchOfficialAccountStatus, current_model_state_async, error_log,
    hot_reload_runtime_models, open_system_browser, prepare_routes_for_current_launch,
    redacted_config, runtime_config_requires_restart, save_config_to_store,
};
use crate::codex_config::codex_home;
use crate::official_accounts::{
    LoginPhase, OfficialAccountRecord, OfficialAccountStore, refresh_if_stale, start_login,
};

fn accounts_payload(store: &OfficialAccountStore) -> Result<Value, String> {
    let default_account_id = store
        .default_account_id()
        .map_err(|error| format!("{error:#}"))?;
    let accounts = store.summaries().map_err(|error| format!("{error:#}"))?;
    Ok(json!({
        "accounts": accounts,
        "defaultAccountId": default_account_id,
    }))
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base_object), Some(extra_object)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra_object {
            base_object.insert(key.clone(), value.clone());
        }
    }
    base
}

pub(super) async fn list_official_accounts(state: &Arc<AppState>) -> Result<Value, String> {
    let store = state.official_accounts();
    let home = codex_home().to_path_buf();
    let payload = tokio::task::spawn_blocking(move || {
        if let Err(error) = store.sync_default_from_codex_home(&home) {
            error_log::record_failure(
                "official_account_sync_failed",
                "list_official_accounts",
                format!("{error:#}"),
                json!({}),
            );
        }
        accounts_payload(&store)
    })
    .await
    .map_err(|error| format!("读取官方账号列表任务异常退出：{error}"))??;
    let config = state.config.read().await;
    Ok(merge(
        payload,
        json!({
            "status": "ok",
            "officialAccountAvailable": config.official_account_available_this_launch,
            "officialAccountStatus": config.official_account_status_this_launch,
        }),
    ))
}

pub(super) async fn start_official_account_login(state: &Arc<AppState>) -> Result<Value, String> {
    {
        let mut logins = state.official_account_logins.lock().await;
        logins.cancel_all();
    }
    // Give the aborted listener a moment to release the callback port.
    tokio::task::yield_now().await;
    let session = start_login(state.http_client.clone())
        .await
        .map_err(|error| format!("{error:#}"))?;
    let auth_url = session.auth_url.clone();
    let login_id = uuid::Uuid::new_v4().to_string();
    state
        .official_account_logins
        .lock()
        .await
        .insert(login_id.clone(), session);
    let browser_opened = {
        let url = auth_url.clone();
        tokio::task::spawn_blocking(move || open_system_browser(&url))
            .await
            .map_err(|error| format!("打开系统浏览器任务异常退出：{error}"))?
            .is_ok()
    };
    Ok(json!({
        "status": "wait",
        "loginId": login_id,
        "authUrl": auth_url,
        "browserOpened": browser_opened,
    }))
}

pub(super) async fn poll_official_account_login(
    state: &Arc<AppState>,
    login_id: String,
) -> Result<Value, String> {
    let phase = {
        let mut logins = state.official_account_logins.lock().await;
        logins.remove_expired();
        let Some(session) = logins.get(&login_id) else {
            return Ok(json!({
                "status": "expired",
                "message": "登录已过期或已取消，请重新添加账号",
            }));
        };
        let phase = session.phase();
        if !matches!(phase, LoginPhase::Waiting) {
            logins.remove(&login_id);
        }
        phase
    };
    match phase {
        LoginPhase::Waiting => Ok(json!({ "status": "wait" })),
        LoginPhase::Failed(message) => Ok(json!({ "status": "failed", "message": message })),
        LoginPhase::Completed(record) => {
            let payload = add_account(state, record).await?;
            Ok(merge(payload, json!({ "status": "ok" })))
        }
    }
}

pub(super) async fn cancel_official_account_login(
    state: &Arc<AppState>,
    login_id: String,
) -> Result<Value, String> {
    if let Some(session) = state.official_account_logins.lock().await.remove(&login_id) {
        session.cancel();
    }
    Ok(json!({ "status": "ok" }))
}

pub(super) async fn import_current_codex_login(state: &Arc<AppState>) -> Result<Value, String> {
    let home = codex_home().to_path_buf();
    let record = tokio::task::spawn_blocking(move || OfficialAccountStore::read_codex_login(&home))
        .await
        .map_err(|error| format!("读取 Codex 登录信息任务异常退出：{error}"))?
        .map_err(|error| format!("{error:#}"))?
        .ok_or_else(|| "当前 Codex 没有 ChatGPT 官方账号登录，无法导入".to_string())?;
    let payload = add_account(state, record).await?;
    Ok(merge(payload, json!({ "status": "ok" })))
}

/// Stores a freshly obtained account. The first stored account becomes the
/// default immediately so the official route works without another click.
async fn add_account(
    state: &Arc<AppState>,
    record: OfficialAccountRecord,
) -> Result<Value, String> {
    let store = state.official_accounts();
    let account_id = record.id.clone();
    let make_default = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let had_default = store.default_account_id()?.is_some();
        store.upsert(&record)?;
        Ok(!had_default)
    })
    .await
    .map_err(|error| format!("保存官方账号任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))?;
    if make_default {
        return set_default_official_account(state, account_id).await;
    }
    let store = state.official_accounts();
    let payload = tokio::task::spawn_blocking(move || accounts_payload(&store))
        .await
        .map_err(|error| format!("读取官方账号列表任务异常退出：{error}"))??;
    Ok(merge(payload, json!({ "accountId": account_id })))
}

pub(super) async fn set_default_official_account(
    state: &Arc<AppState>,
    account_id: String,
) -> Result<Value, String> {
    let account_id = account_id.trim().to_string();
    if account_id.is_empty() {
        return Err("缺少要设为默认的官方账号".to_string());
    }
    let store = state.official_accounts();
    let lookup_store = store.clone();
    let lookup_id = account_id.clone();
    let mut record = tokio::task::spawn_blocking(move || lookup_store.get(&lookup_id))
        .await
        .map_err(|error| format!("读取官方账号任务异常退出：{error}"))?
        .map_err(|error| format!("{error:#}"))?
        .ok_or_else(|| format!("找不到官方账号：{account_id}"))?;

    // Stored tokens may be days old when switching accounts. Refresh them
    // best-effort so Codex starts with a live session; a failure still hands
    // over the stored copy, which Codex can refresh itself.
    match refresh_if_stale(&state.http_client, &mut record).await {
        Ok(true) => {
            let refreshed_store = store.clone();
            let refreshed = record.clone();
            tokio::task::spawn_blocking(move || refreshed_store.upsert(&refreshed))
                .await
                .map_err(|error| format!("保存官方账号任务异常退出：{error}"))?
                .map_err(|error| format!("{error:#}"))?;
        }
        Ok(false) => {}
        Err(error) => error_log::record_failure(
            "official_account_refresh_failed",
            "set_default_official_account",
            format!("{error:#}"),
            json!({ "accountId": account_id }),
        ),
    }

    let home = codex_home().to_path_buf();
    let activate_store = store.clone();
    let activate_record = record.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        OfficialAccountStore::write_codex_login(&home, &activate_record)?;
        activate_store.set_default_account_id(Some(&activate_record.id))?;
        Ok(())
    })
    .await
    .map_err(|error| format!("切换默认官方账号任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))?;

    // "设为默认并显示额度": the default account's usage shows in the header.
    {
        let _guard = state.config_write_lock.lock().await;
        let mut config = state.config.read().await.clone();
        if !config.show_account_usage_in_header {
            config.show_account_usage_in_header = true;
            config.settings_revision = config.settings_revision.saturating_add(1);
            save_config_to_store(state, &config).await?;
            *state.config.write().await = config;
        }
    }

    // Usage is cached per auth file fingerprint; the file just changed, so the
    // next read reflects the new account. Route availability is recomputed
    // from the store like at launch.
    let payload = refresh_official_route_after_account_change(state).await?;
    Ok(merge(payload, json!({ "accountId": record.id })))
}

pub(super) async fn remove_official_account(
    state: &Arc<AppState>,
    account_id: String,
) -> Result<Value, String> {
    let account_id = account_id.trim().to_string();
    if account_id.is_empty() {
        return Err("缺少要移除的官方账号".to_string());
    }
    let store = state.official_accounts();
    let home = codex_home().to_path_buf();
    let removed_default = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let was_default = store.default_account_id()?.as_deref() == Some(account_id.as_str());
        if was_default && let Some(record) = store.get(&account_id)? {
            OfficialAccountStore::clear_codex_login_if_matches(&home, &record)?;
        }
        store.remove(&account_id)?;
        Ok(was_default)
    })
    .await
    .map_err(|error| format!("移除官方账号任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))?;
    if removed_default {
        return refresh_official_route_after_account_change(state).await;
    }
    let store = state.official_accounts();
    let payload = tokio::task::spawn_blocking(move || accounts_payload(&store))
        .await
        .map_err(|error| format!("读取官方账号列表任务异常退出：{error}"))??;
    Ok(merge(payload, json!({ "status": "ok" })))
}

/// Re-runs the launch-time route preparation against the account store and
/// pushes the resulting routes to a running Codex without a restart.
async fn refresh_official_route_after_account_change(
    state: &Arc<AppState>,
) -> Result<Value, String> {
    let prepare_error = prepare_routes_for_current_launch(state).await.err();
    let config = state.config.read().await.clone();
    let model_state = current_model_state_async(&config).await?;
    let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
    let restart_required = runtime_config_requires_restart(state, &config).await;
    let store = state.official_accounts();
    let accounts = tokio::task::spawn_blocking(move || accounts_payload(&store))
        .await
        .map_err(|error| format!("读取官方账号列表任务异常退出：{error}"))??;
    let unauthenticated =
        config.official_account_status_this_launch == LaunchOfficialAccountStatus::Unauthenticated;
    let mut response = hot_reload.add_to_response(json!({
        "status": "ok",
        "config": redacted_config(&config),
        "modelState": model_state,
        "officialAccountAvailable": config.official_account_available_this_launch,
        "officialAccountStatus": config.official_account_status_this_launch,
        "restartRequired": restart_required,
    }));
    if let Some(error) = prepare_error {
        response = merge(response, json!({ "warning": error }));
    } else if unauthenticated && !config.official_account_available_this_launch {
        response = merge(
            response,
            json!({ "warning": "当前没有默认官方账号，官方线路已停用" }),
        );
    }
    Ok(merge(response, accounts))
}
