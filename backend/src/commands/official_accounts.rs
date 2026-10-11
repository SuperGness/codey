//! Official (ChatGPT) account management commands: add accounts through the
//! OAuth login flow, keep several of them, and pick exactly one as the default
//! that Codex runs as.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    AppState, LaunchOfficialAccountStatus, argument, current_model_state_async, error_log,
    hot_reload_runtime_models, official_account_available_for_usage, open_system_browser,
    optional_argument, prepare_routes_for_current_launch, query_official_account_usage,
    redacted_config, runtime_config_requires_restart, save_config_to_store, string_argument,
};
use crate::codex_config::codex_home;
use crate::config::{
    CodeyConfig, MAX_ROUTE_NAME_CHARS, MAX_ROUTE_SHORT_NAME_CHARS, validate_outbound_proxy_url,
};
use crate::official_accounts::{
    LoginPhase, OfficialAccountInvalid, OfficialAccountRecord, OfficialAccountStore,
    refresh_if_stale_cached, start_login,
};

pub(super) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    match command {
        "query_official_account_usage" => match (
            optional_argument::<bool>(args, "forceRefresh"),
            optional_argument::<String>(args, "accountId"),
        ) {
            (Ok(force), Ok(account_id)) => {
                Ok(query_official_account_usage(state, force.unwrap_or(false), account_id).await)
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "store_official_account_usage" => match (
            argument::<u64>(args, "authGeneration"),
            argument::<crate::account_usage::AccountUsageSnapshot>(args, "snapshot"),
        ) {
            (Ok(generation), Ok(snapshot)) => {
                if !official_account_available_for_usage(&*state.config.read().await) {
                    Err("当前没有可用的官方账号".to_string())
                } else {
                    state
                        .account_usage_cache
                        .lock()
                        .await
                        .for_codex_home(codex_home())
                        .store_displayed_snapshot(
                            &crate::account_usage::codex_auth_path(codex_home()),
                            generation,
                            snapshot,
                        )
                        .map(|()| json!({"status": "ok"}))
                        .map_err(|error| error.to_string())
                }
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "list_official_accounts" => list_official_accounts(state).await,
        "refresh_official_account_routes" => {
            refresh_official_route_after_account_change(state).await
        }
        "start_official_account_login" => start_official_account_login(state).await,
        "poll_official_account_login" => match string_argument(args, "loginId") {
            Ok(login_id) => poll_official_account_login(state, login_id).await,
            Err(error) => Err(error),
        },
        "cancel_official_account_login" => match string_argument(args, "loginId") {
            Ok(login_id) => cancel_official_account_login(state, login_id).await,
            Err(error) => Err(error),
        },
        "import_current_codex_login" => import_current_codex_login(state).await,
        "import_official_account_credential" => match string_argument(args, "credential") {
            Ok(credential) => import_official_account_credential(state, credential).await,
            Err(error) => Err(error),
        },
        "set_default_official_account" => match string_argument(args, "accountId") {
            Ok(account_id) => set_default_official_account(state, account_id).await,
            Err(error) => Err(error),
        },
        "remove_official_account" => match string_argument(args, "accountId") {
            Ok(account_id) => remove_official_account(state, account_id).await,
            Err(error) => Err(error),
        },
        "save_official_account_route_settings" => match (
            string_argument(args, "accountId"),
            optional_argument::<String>(args, "routeName"),
            optional_argument::<String>(args, "routeShortName"),
            optional_argument::<String>(args, "upstreamProxy"),
            optional_argument::<String>(args, "baseUrl"),
        ) {
            (
                Ok(account_id),
                Ok(route_name),
                Ok(route_short_name),
                Ok(upstream_proxy),
                Ok(base_url),
            ) => {
                save_official_account_route_settings(
                    state,
                    account_id,
                    route_name.unwrap_or_default(),
                    route_short_name.unwrap_or_default(),
                    upstream_proxy.unwrap_or_default(),
                    base_url.unwrap_or_default(),
                )
                .await
            }
            (Err(error), _, _, _, _)
            | (_, Err(error), _, _, _)
            | (_, _, Err(error), _, _)
            | (_, _, _, Err(error), _)
            | (_, _, _, _, Err(error)) => Err(error),
        },
        _ => Err(format!("未知 Codey API 命令：{command}")),
    }
}

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
            let payload = add_account(state, *record).await?;
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

pub(super) async fn import_official_account_credential(
    state: &Arc<AppState>,
    credential: String,
) -> Result<Value, String> {
    let record = crate::official_accounts::official_account_from_manual_input(
        &state.http_client,
        &credential,
    )
    .await
    .map_err(|error| format!("{error:#}"))?;
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
        // 新账号立刻带上按添加顺序生成的默认线路名和短名称（官方账号1 / 官1），
        // 之后仍可在官方线路卡片上改成别的名称。
        store.ensure_generated_route_settings()?;
        Ok(!had_default)
    })
    .await
    .map_err(|error| format!("保存官方账号任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))?;
    if make_default {
        return set_default_official_account(state, account_id).await;
    }
    // 每个账号都有自己的线路，新增后立刻重算，避免要等到下次启动才出现。
    let payload = refresh_official_route_after_account_change(state).await?;
    Ok(merge(payload, json!({ "accountId": account_id })))
}

/// Refreshes one stored account's tokens when they are expired or old enough
/// to matter, and writes the refreshed document back. Idle accounts keep
/// working this way without being switched to the default first.
///
/// 默认账号的凭据文档由 Codex 自己维护在 Codex home 的 auth.json：这里只把
/// 它同步回账号记录，不主动轮换 refresh token，避免 Codex 手里那份凭据失效。
pub(super) async fn refresh_official_account_tokens(
    state: &Arc<AppState>,
    account_id: &str,
) -> Result<OfficialAccountRecord, String> {
    // 刷新统一串行执行：并发轮换同一个 refresh token 会让先写回的凭据立刻失效。
    let _refresh_guard = state.official_account_refresh_lock.lock().await;
    let store = state.official_accounts();
    let lookup_store = store.clone();
    let lookup_id = account_id.to_string();
    let lookup_home = codex_home().to_path_buf();
    let (mut record, is_default) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<(OfficialAccountRecord, bool)> {
            let is_default =
                lookup_store.default_account_id()?.as_deref() == Some(lookup_id.as_str());
            if is_default {
                // Codex 会在运行期间替换 auth.json，先把最新副本取回来，
                // 账号记录才不会停留在启动时的旧令牌上。
                lookup_store.sync_default_from_codex_home(&lookup_home)?;
            }
            let record = lookup_store
                .get(&lookup_id)?
                .ok_or_else(|| anyhow::anyhow!("找不到官方账号：{lookup_id}"))?;
            Ok((record, is_default))
        })
        .await
        .map_err(|error| format!("读取官方账号任务异常退出：{error}"))?
        .map_err(|error| format!("{error:#}"))?;
    if is_default {
        return Ok(record);
    }
    // 已确认失效的账号不再尝试刷新令牌：官方每次都会拒绝，重复轮询只会
    // 反复请求官方接口、抬高风控概率。额度查询拿到本地原因后直接返回失效
    // 状态，重新添加账号会写入新凭据并清除标记。
    if record.invalid() {
        return Ok(record);
    }
    let expected = record.clone();
    let proxy = super::official_account_usage_proxy(state, account_id).await;
    match refresh_if_stale_cached(
        &state.http_client,
        &mut record,
        proxy.as_deref(),
        Some(&state.official_proxied_clients),
    )
    .await
    {
        Ok(true) => {
            let refreshed_store = store.clone();
            let refreshed = record.clone();
            record = tokio::task::spawn_blocking(move || {
                refreshed_store.update_credentials_if_current(&expected, &refreshed)
            })
            .await
            .map_err(|error| format!("保存官方账号任务异常退出：{error}"))?
            .map_err(|error| format!("{error:#}"))?
            .ok_or_else(|| "官方账号已移除，忽略旧的刷新结果".to_string())?;
        }
        Ok(false) => {}
        Err(error) => {
            // 官方明确拒绝凭据时把失效状态写回账号记录，账号列表和额度查询
            // 都以它为准；网络故障等其他错误只记日志，不改账号状态。
            if let Some(invalid) = error.downcast_ref::<OfficialAccountInvalid>() {
                record.mark_invalid(invalid.reason());
                let marked_store = store.clone();
                let marked = record.clone();
                record = tokio::task::spawn_blocking(move || {
                    marked_store.update_credentials_if_current(&expected, &marked)
                })
                .await
                .map_err(|error| format!("保存官方账号任务异常退出：{error}"))?
                .map_err(|error| format!("{error:#}"))?
                .ok_or_else(|| "官方账号已移除，忽略旧的刷新结果".to_string())?;
                if !record.invalid() {
                    return Ok(record);
                }
                error_log::record_failure(
                    "official_account_invalid",
                    "refresh_official_account_tokens",
                    format!("{error:#}"),
                    json!({
                        "accountId": account_id,
                        "detail": invalid.detail(),
                    }),
                );
                // 失效账号的线路要立刻从配置里下线，避免本地路由继续把它
                // 当成可用线路展示。
                refresh_official_routes_after_invalid_account(
                    state,
                    "refresh_official_account_tokens",
                    account_id,
                )
                .await;
            } else {
                error_log::record_failure(
                    "official_account_refresh_failed",
                    "refresh_official_account_tokens",
                    format!("{error:#}"),
                    json!({ "accountId": account_id }),
                );
            }
        }
    }
    Ok(record)
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
    // Stored tokens may be days old when switching accounts. Refresh them
    // best-effort so Codex starts with a live session; a failure still hands
    // over the stored copy, which Codex can refresh itself.
    let record = refresh_official_account_tokens(state, &account_id).await?;
    if record.invalid() {
        let label = record.email.clone().unwrap_or_else(|| account_id.clone());
        return Err(format!(
            "官方账号「{label}」已失效，无法设为默认；请重新添加该账号"
        ));
    }

    // 页头额度跟随默认账号，切换后确保额度显示处于开启状态。
    {
        let _guard = state.config_write_lock.lock().await;
        let mut config = state.config.read().await.clone();
        if !config.show_account_usage_in_header {
            config.show_account_usage_in_header = true;
            config.settings_revision = config.settings_revision.saturating_add(1);
            let config = save_config_to_store(state, config).await?;
            *state.config.write().await = config;
        }
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
    let (removed_default, promoted, accounts_left) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<(bool, Option<String>, bool)> {
            let was_default = store.default_account_id()?.as_deref() == Some(account_id.as_str());
            if was_default && let Some(record) = store.get(&account_id)? {
                OfficialAccountStore::clear_codex_login_if_matches(&home, &record)?;
            }
            store.remove(&account_id)?;
            let mut remaining = store.list()?;
            // 默认账号被移除后把剩下最早的可用账号补上，Codex 仍然有可用的登录
            // 身份；失效账号必须跳过，否则补位会因凭据被拒绝而整个失败。
            let promoted = if was_default {
                remaining.sort_by(|left, right| {
                    left.added_at
                        .cmp(&right.added_at)
                        .then_with(|| left.id.cmp(&right.id))
                });
                remaining
                    .iter()
                    .find(|record| !record.invalid())
                    .map(|record| record.id.clone())
            } else {
                None
            };
            Ok((was_default, promoted, !remaining.is_empty()))
        })
        .await
        .map_err(|error| format!("移除官方账号任务异常退出：{error}"))?
        .map_err(|error| format!("{error:#}"))?;
    if let Some(promoted) = promoted {
        let payload = set_default_official_account(state, promoted).await?;
        return Ok(merge(payload, json!({ "status": "ok" })));
    }
    if removed_default && !accounts_left {
        // 最后一个官方账号已经删除，配置里由它派生的线路随之失效；先撤掉这些
        // 线路再重新准备，本地路由不会继续转发到已经不存在的账号。
        drop_derived_official_routes(state).await?;
    }
    let payload = refresh_official_route_after_account_change(state).await?;
    Ok(merge(payload, json!({ "status": "ok" })))
}

/// Removes every derived official route from the running configuration and
/// persists the result. Used when the account store is empty, so the local
/// router stops exposing routes whose accounts no longer exist.
pub(super) async fn drop_derived_official_routes(state: &Arc<AppState>) -> Result<(), String> {
    let _config_write_guard = state.config_write_lock.lock().await;
    let previous = state.config.read().await.clone();
    if !previous
        .profiles
        .iter()
        .any(|profile| profile.official_account)
    {
        return Ok(());
    }
    let mut next = previous.clone();
    next.apply_launch_official_profiles(Vec::new());
    next = next.normalize();
    if next.settings_revision == previous.settings_revision {
        next.settings_revision = previous.settings_revision.saturating_add(1);
    }
    let next = save_config_to_store(state, next).await?;
    *state.config.write().await = next;
    Ok(())
}

/// Saves the route name, short name, gateway and upstream proxy of one
/// official account and re-derives every official route, so the edited
/// account shows its new name without becoming the default.
pub(super) async fn save_official_account_route_settings(
    state: &Arc<AppState>,
    account_id: String,
    route_name: String,
    route_short_name: String,
    upstream_proxy: String,
    base_url: String,
) -> Result<Value, String> {
    let account_id = write_official_account_route_settings(
        state,
        account_id,
        route_name,
        route_short_name,
        upstream_proxy,
        base_url,
    )
    .await?;
    let payload = refresh_official_route_after_account_change(state).await?;
    Ok(merge(
        payload,
        json!({ "status": "ok", "accountId": account_id }),
    ))
}

/// Writes one account's route overrides without re-deriving routes. Callers
/// that already persist models in the same turn apply the derived routes once.
pub(crate) async fn write_official_account_route_settings(
    state: &Arc<AppState>,
    account_id: String,
    route_name: String,
    route_short_name: String,
    upstream_proxy: String,
    base_url: String,
) -> Result<String, String> {
    let account_id = account_id.trim().to_string();
    if account_id.is_empty() {
        return Err("缺少要保存线路设置的官方账号".to_string());
    }
    let route_name = route_name.trim().to_string();
    let route_short_name = route_short_name.trim().to_string();
    let upstream_proxy = upstream_proxy.trim().to_string();
    if route_short_name.is_empty() {
        return Err("请输入短名称".to_string());
    }
    // 与渲染层的线路名上限保持一致，避免直接调用后端接口写入界面无法保存的名称。
    if route_name.chars().count() > MAX_ROUTE_NAME_CHARS {
        return Err(format!("线路名最多 {MAX_ROUTE_NAME_CHARS} 个字符"));
    }
    if route_short_name.chars().count() > MAX_ROUTE_SHORT_NAME_CHARS {
        return Err(format!("短名称最多 {MAX_ROUTE_SHORT_NAME_CHARS} 个字符"));
    }
    if !upstream_proxy.is_empty() {
        validate_outbound_proxy_url(&upstream_proxy, "官方账号线路的上游代理")?;
    }
    // 留空或官方默认地址都表示不覆盖网关。
    let base_url = crate::codex_provider::normalize_official_gateway_base_url(&base_url)?;
    // Official and third-party routes share one short-name namespace because the
    // short name prefixes every route-scoped model name.
    if !route_short_name.is_empty()
        && let Some(conflict) = state
            .config
            .read()
            .await
            .profiles
            .iter()
            .find(|profile| {
                !profile.official_account && profile.short_name.trim() == route_short_name
            })
            .map(|profile| profile.name.clone())
    {
        return Err(format!(
            "短名称「{route_short_name}」已被线路「{conflict}」使用"
        ));
    }

    let store = state.official_accounts();
    let saved_id = account_id.clone();
    let saved_name = Some(route_name);
    let saved_short_name = (!route_short_name.is_empty()).then_some(route_short_name);
    let saved_proxy = (!upstream_proxy.is_empty()).then_some(upstream_proxy);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let accounts = store.list()?;
        if !accounts.iter().any(|record| record.id == saved_id) {
            anyhow::bail!("找不到官方账号：{saved_id}");
        }
        if let Some(short_name) = saved_short_name.as_deref()
            && let Some(other) = accounts.iter().find(|record| {
                record.id != saved_id && record.route_short_name.as_deref() == Some(short_name)
            })
        {
            let label = other.email.as_deref().unwrap_or(other.id.as_str());
            anyhow::bail!("短名称「{short_name}」已被官方账号 {label} 使用");
        }
        store.update_route_settings(
            &saved_id,
            saved_name,
            saved_short_name,
            saved_proxy,
            base_url,
        )?;
        Ok(())
    })
    .await
    .map_err(|error| format!("保存官方账号线路设置任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))?;
    Ok(account_id)
}

/// Re-runs the launch-time route preparation against the account store and
/// pushes the resulting routes to a running Codex without a restart.
pub(crate) async fn refresh_official_route_after_account_change(
    state: &Arc<AppState>,
) -> Result<Value, String> {
    let prepare_error = prepare_routes_for_current_launch(state).await.err();
    let config = state.config.read().await.clone();
    let model_state = current_model_state_async(&config).await?;
    let hot_reload = hot_reload_runtime_models(state).await;
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
    // 存储账号的线路自带凭据，默认登录缺失时它们仍然可以继续使用。
    let routes_available = config.usable_official_routes().next().is_some();
    if let Some(error) = prepare_error {
        response = merge(response, json!({ "warning": error }));
    } else if unauthenticated && !routes_available {
        response = merge(
            response,
            json!({ "warning": "当前没有默认官方账号，官方线路已停用" }),
        );
    }
    Ok(merge(response, accounts))
}

/// 官方线路派生时会按第三方线路占用的短名称调整编号，账号记录跟随写回后，
/// 面板里显示和编辑的短名称才与线路列表、模型名称前缀一致。
pub(super) async fn reconcile_official_account_short_names(
    store: &OfficialAccountStore,
    config: &CodeyConfig,
) -> Result<(), String> {
    let resolved = config
        .profiles
        .iter()
        .filter_map(|profile| {
            let account_id = profile.official_account_id.as_deref()?;
            let short_name = profile.short_name.trim();
            (!short_name.is_empty()).then(|| (account_id.to_string(), short_name.to_string()))
        })
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        return Ok(());
    }
    let store = store.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        for (account_id, short_name) in resolved {
            let Some(record) = store.get(&account_id)? else {
                continue;
            };
            let saved = record
                .route_short_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if saved == Some(short_name.as_str()) {
                continue;
            }
            store.update_route_short_name(&account_id, &short_name)?;
        }
        Ok(())
    })
    .await
    .map_err(|error| format!("回写官方账号短名称任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))
}

/// 账号失效后立刻重新派生官方线路，让失效账号的线路从配置里下线。重算失败
/// 只记日志：额度查询要继续返回账号失效这个结果，不能被线路刷新的错误覆盖。
pub(super) async fn refresh_official_routes_after_invalid_account(
    state: &Arc<AppState>,
    stage: &str,
    account_id: &str,
) {
    // 启动预热可能带着失效标记写入之前的解析结果，先丢弃再重新派生。
    let _ = state.take_official_account_probe_prewarm().await;
    if let Err(error) = refresh_official_route_after_account_change(state).await {
        error_log::record_failure(
            "official_account_route_refresh_failed",
            stage,
            error,
            json!({ "accountId": account_id }),
        );
    }
}
