#[cfg(target_os = "macos")]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::USER_AGENT;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::AppState;
use crate::config::ConfigStore;

const UPDATE_CHECK_CACHE_TTL: Duration = Duration::from_secs(30);
const UPDATE_DOWNLOAD_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_UPDATE_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct UpdateManifest {
    schema_version: u32,
    version: String,
    tag: String,
    #[serde(default, alias = "releaseNotes")]
    release_notes: Option<String>,
    assets: Vec<UpdateManifestAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct UpdateManifestAsset {
    platform: String,
    arch: String,
    package_type: String,
    file_name: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateCheck {
    pub(crate) current_version: String,
    pub(crate) latest_version: String,
    pub(crate) update_available: bool,
    pub(crate) selected_asset: Option<UpdateAssetInfo>,
    #[serde(default)]
    pub(crate) release_notes: Option<String>,
    #[serde(default)]
    pub(crate) publish_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateAssetInfo {
    pub(crate) platform: String,
    pub(crate) arch: String,
    pub(crate) package_type: String,
    pub(crate) file_name: String,
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateDownload {
    pub(crate) latest_version: String,
    pub(crate) file_path: String,
    pub(crate) file_name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) asset: UpdateAssetInfo,
    #[serde(default)]
    pub(crate) publish_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdentity {
    install_key: String,
    machine_no: String,
}

#[derive(Clone, Debug, Deserialize)]
struct DeviceUpdateResponse {
    #[serde(rename = "trackDelivery", default)]
    track_delivery: Option<bool>,
    #[serde(rename = "updateAvailable", default)]
    update_available: bool,
    #[serde(rename = "currentVersion", default)]
    current_version: Option<String>,
    #[serde(alias = "publishId", default)]
    publish_id: Option<String>,
    #[serde(alias = "releaseNotes", default)]
    release_notes: Option<String>,
    #[serde(alias = "manifestUrl", default)]
    manifest_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingDeviceUpdate {
    publish_id: String,
    version: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct UpdateCandidate {
    pub(crate) check: UpdateCheck,
}

#[derive(Debug)]
struct VerifiedDownloadedUpdate {
    path: PathBuf,
    asset: UpdateAssetInfo,
}

pub(super) struct CachedUpdateCandidate {
    manifest_url: String,
    candidate: UpdateCandidate,
    checked_at: Instant,
}

pub(super) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    match command {
        "check_for_updates" => check_for_updates(state).await,
        "download_update" => download_update(state).await,
        "update_install_report" => update_install_report(state).await,
        "install_downloaded_update" => {
            install_downloaded_update(state, super::string_argument(args, "filePath")?).await
        }
        _ => Err(format!("未知 Codey API 命令：{command}")),
    }
}

pub async fn check_for_updates(state: &Arc<AppState>) -> Result<Value, String> {
    let candidate = check_for_update_candidate(state).await?;
    serde_json::to_value(candidate.check).map_err(|error| error.to_string())
}

pub async fn download_update(state: &Arc<AppState>) -> Result<Value, String> {
    let candidate = update_candidate_with_ttl(state, UPDATE_DOWNLOAD_CACHE_TTL).await?;
    let download = download_update_candidate(state, &candidate).await?;
    serde_json::to_value(download).map_err(|error| error.to_string())
}

/// 读取上一次更新安装留给本次启动的结果，读完即清除，保证只提示一次。
pub async fn update_install_report(state: &Arc<AppState>) -> Result<Value, String> {
    let config_path = state.store.path().to_path_buf();
    let Some(report) = crate::update_helper::read_update_install_report(&config_path) else {
        return Ok(Value::Null);
    };
    if report.is_stale(crate::update_helper::current_unix_timestamp()) {
        crate::update_helper::clear_update_install_report(&config_path);
        return Ok(Value::Null);
    }
    if let Some(pending) = read_pending_device_update(state).await {
        let final_status = match report.status.as_str() {
            "installed" => Some("installed"),
            "failed" | "unverified" => Some("failed"),
            "started" => None,
            _ => None,
        };
        if let Some(status) = final_status {
            let message = (!report.message.is_empty()).then_some(report.message.as_str());
            let mut pending = pending;
            pending.status = Some(status.to_string());
            pending.message = message.map(str::to_string);
            let reported = report_publish_event(state, &pending.publish_id, status, message)
                .await
                .is_ok();
            if reported {
                let _ =
                    tokio::fs::remove_file(pending_device_update_path(&state.store).await?).await;
            } else {
                let _ = write_pending_device_update(state, &pending).await;
            }
        }
    }
    crate::update_helper::clear_update_install_report(&config_path);
    serde_json::to_value(report).map_err(|error| error.to_string())
}

pub(crate) async fn check_for_update_candidate(
    state: &Arc<AppState>,
) -> Result<UpdateCandidate, String> {
    update_candidate_with_ttl(state, UPDATE_CHECK_CACHE_TTL).await
}

async fn update_candidate_with_ttl(
    state: &Arc<AppState>,
    cache_ttl: Duration,
) -> Result<UpdateCandidate, String> {
    let release_admin_url = configured_release_admin_url(state).await?;
    let manifest_url = if let Some(base) = &release_admin_url {
        format!("release-admin:{base}")
    } else {
        configured_update_manifest_url(state).await?
    };
    let mut cache = state.update_candidate_cache.lock().await;
    let now = Instant::now();
    if let Some(candidate) =
        reusable_update_candidate(cache.as_ref(), &manifest_url, now, cache_ttl)
    {
        return Ok(candidate);
    }

    let check = if release_admin_url.is_some() {
        fetch_release_admin_update(state, release_admin_url.as_deref().unwrap()).await?
    } else {
        let manifest = fetch_configured_update_manifest(state, &manifest_url).await?;
        assess_update_manifest(env!("CARGO_PKG_VERSION"), &manifest)?
    };
    *state.available_update.write().await = check.update_available.then(|| check.clone());
    let candidate = UpdateCandidate { check };
    *cache = Some(CachedUpdateCandidate {
        manifest_url,
        candidate: candidate.clone(),
        checked_at: Instant::now(),
    });
    Ok(candidate)
}

async fn configured_release_admin_url(state: &AppState) -> Result<Option<String>, String> {
    let value = state
        .config
        .read()
        .await
        .release_admin_url
        .trim()
        .to_string();
    if value.is_empty() {
        return Ok(None);
    }
    let url = reqwest::Url::parse(&value)
        .map_err(|_| "发布管理服务地址必须是有效的 HTTPS URL".to_string())?;
    if url.scheme() != "https" {
        return Err("发布管理服务地址必须使用 HTTPS".to_string());
    }
    Ok(Some(value.trim_end_matches('/').to_string()))
}

async fn device_identity_path(store: &ConfigStore) -> Result<PathBuf, String> {
    let parent = store
        .path()
        .parent()
        .ok_or_else(|| "Codey 配置路径无父目录，无法保存设备身份".to_string())?;
    Ok(parent.join("release-device.json"))
}

async fn pending_device_update_path(store: &ConfigStore) -> Result<PathBuf, String> {
    let parent = store
        .path()
        .parent()
        .ok_or_else(|| "Codey 配置路径无父目录，无法保存更新状态".to_string())?;
    Ok(parent.join("release-pending-update.json"))
}

async fn read_pending_device_update(state: &AppState) -> Option<PendingDeviceUpdate> {
    let path = pending_device_update_path(&state.store).await.ok()?;
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_pending_device_update(
    state: &AppState,
    pending: &PendingDeviceUpdate,
) -> Result<(), String> {
    let path = pending_device_update_path(&state.store).await?;
    let bytes = serde_json::to_vec(pending).map_err(|error| error.to_string())?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| format!("保存更新回报失败：{error}"))
}

async fn retry_pending_device_event(state: &AppState) {
    let Some(pending) = read_pending_device_update(state).await else {
        return;
    };
    let Some(status) = pending.status.as_deref() else {
        return;
    };
    if report_publish_event(
        state,
        &pending.publish_id,
        status,
        pending.message.as_deref(),
    )
    .await
    .is_ok()
        && let Ok(path) = pending_device_update_path(&state.store).await
    {
        let _ = tokio::fs::remove_file(path).await;
    }
}

async fn load_or_register_device(
    state: &AppState,
    base_url: &str,
) -> Result<DeviceIdentity, String> {
    let path = device_identity_path(&state.store).await?;
    if let Ok(bytes) = tokio::fs::read(&path).await
        && let Ok(identity) = serde_json::from_slice::<DeviceIdentity>(&bytes)
        && !identity.install_key.trim().is_empty()
        && !identity.machine_no.trim().is_empty()
    {
        return Ok(identity);
    }

    let install_key = format!("{}:{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let body = json!({
        "installKey": install_key,
        "platform": current_update_platform(),
        "arch": current_update_arch(),
        "currentVersion": env!("CARGO_PKG_VERSION"),
        // 设备管理需要一眼看到客户端环境，登记时一并带上。
        "codexVersion": current_codex_version().await,
        "osName": current_os_name(),
        "osVersion": current_os_version(),
    });
    let response = state
        .http_client
        .post(format!("{base_url}/api/devices/register"))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("注册设备失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("注册设备失败：{error}"))?;
    let registered = response
        .json::<DeviceIdentityResponse>()
        .await
        .map_err(|error| format!("设备注册响应无效：{error}"))?;
    if registered.machine_no.trim().is_empty() {
        return Err("设备注册响应缺少机器号".to_string());
    }
    let identity = DeviceIdentity {
        install_key,
        machine_no: registered.machine_no,
    };
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建设备身份目录失败：{error}"))?;
    }
    let bytes = serde_json::to_vec(&identity).map_err(|error| error.to_string())?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| format!("保存设备身份失败：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = device_identity_path(&state.store).await?;
        let mut permissions = tokio::fs::metadata(&path)
            .await
            .map_err(|error| format!("读取设备身份权限失败：{error}"))?
            .permissions();
        permissions.set_mode(0o600);
        tokio::fs::set_permissions(path, permissions)
            .await
            .map_err(|error| format!("设置设备身份权限失败：{error}"))?;
    }
    Ok(identity)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdentityResponse {
    machine_no: String,
}

async fn fetch_release_admin_update(
    state: &AppState,
    base_url: &str,
) -> Result<UpdateCheck, String> {
    retry_pending_device_event(state).await;
    // 身份仅用于灰度筛选，注册失败仍可检查公开全量版本。
    let identity = load_or_register_device(state, base_url).await.ok();
    let endpoint = reqwest::Url::parse(&format!("{base_url}/api/updates/check"))
        .map_err(|_| "发布管理服务地址无效".to_string())?;
    // Codex 与系统版本随每次检查更新上报，服务端据此判断版本是否适配本机。
    let mut query = vec![
        ("currentVersion", env!("CARGO_PKG_VERSION").to_string()),
        ("osName", current_os_name().to_string()),
    ];
    if let Some(codex_version) = current_codex_version().await {
        query.push(("codexVersion", codex_version));
    }
    if let Some(os_version) = current_os_version() {
        query.push(("osVersion", os_version));
    }
    let mut request = state.http_client.get(endpoint).query(&query);
    if let Some(identity) = &identity {
        request = request
            .header("x-machine-no", &identity.machine_no)
            .header("x-device-key", &identity.install_key);
    }
    let response = request
        .header(
            USER_AGENT,
            format!("Codey/{} release-admin-check", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("检查发布更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("发布管理服务返回异常：{error}"))?;
    let candidate = response
        .json::<DeviceUpdateResponse>()
        .await
        .map_err(|error| format!("发布更新响应无效：{error}"))?;
    if !candidate.update_available {
        let current = candidate
            .current_version
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        return Ok(UpdateCheck {
            current_version: current.clone(),
            latest_version: current,
            update_available: false,
            selected_asset: None,
            release_notes: None,
            publish_id: None,
        });
    }
    let manifest_url = candidate
        .manifest_url
        .ok_or_else(|| "发布版本缺少更新清单地址".to_string())?;
    let manifest = fetch_configured_update_manifest(state, &manifest_url).await?;
    let mut check = assess_update_manifest(env!("CARGO_PKG_VERSION"), &manifest)?;
    check.release_notes = candidate.release_notes.or(manifest.release_notes);
    check.publish_id = if candidate.track_delivery == Some(false) {
        None
    } else {
        candidate.publish_id
    };
    Ok(check)
}

fn reusable_update_candidate(
    cached: Option<&CachedUpdateCandidate>,
    manifest_url: &str,
    now: Instant,
    cache_ttl: Duration,
) -> Option<UpdateCandidate> {
    let cached = cached?;
    (cached.manifest_url == manifest_url
        && now.saturating_duration_since(cached.checked_at) < cache_ttl)
        .then(|| cached.candidate.clone())
}

pub(crate) async fn download_update_candidate(
    state: &Arc<AppState>,
    candidate: &UpdateCandidate,
) -> Result<UpdateDownload, String> {
    if !candidate.check.update_available {
        return Err(format!(
            "当前已是最新版本 v{}",
            candidate.check.current_version
        ));
    }
    let asset = candidate
        .check
        .selected_asset
        .as_ref()
        .ok_or_else(|| "没有适用于当前系统的可安装更新包".to_string())?;
    let file_path = match download_update_asset(
        &state.http_client,
        &state.store,
        &candidate.check.latest_version,
        asset,
    )
    .await
    {
        Ok(path) => path,
        Err(error) => {
            if let Some(publish_id) = candidate.check.publish_id.as_deref() {
                let _ = report_publish_event(state, publish_id, "failed", Some(&error)).await;
            }
            return Err(error);
        }
    };
    if let Some(publish_id) = candidate.check.publish_id.as_deref() {
        let _ = report_publish_event(state, publish_id, "downloaded", None).await;
        let pending = PendingDeviceUpdate {
            publish_id: publish_id.to_string(),
            version: candidate.check.latest_version.clone(),
            status: None,
            message: None,
        };
        let _ = write_pending_device_update(state, &pending).await;
    }
    Ok(UpdateDownload {
        latest_version: candidate.check.latest_version.clone(),
        file_path: file_path.to_string_lossy().to_string(),
        file_name: asset.file_name.clone(),
        size: asset.size,
        sha256: asset.sha256.clone(),
        asset: asset.clone(),
        publish_id: candidate.check.publish_id.clone(),
    })
}

async fn report_publish_event(
    state: &AppState,
    publish_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), String> {
    let Some(base_url) = configured_release_admin_url(state).await? else {
        return Ok(());
    };
    let identity = load_or_register_device(state, &base_url).await?;
    let body = json!({
        "machineNo": identity.machine_no,
        "status": status,
        "errorMessage": error_message,
    });
    let mut last_error = String::from("上报更新状态失败");
    for attempt in 0..3 {
        match state
            .http_client
            .post(format!("{base_url}/api/publishes/{publish_id}/report"))
            .header("x-machine-no", &identity.machine_no)
            .header("x-device-key", &identity.install_key)
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) => match response.error_for_status() {
                Ok(_) => return Ok(()),
                Err(error) => last_error = format!("上报更新状态失败：{error}"),
            },
            Err(error) => last_error = format!("上报更新状态失败：{error}"),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(150 * (attempt + 1) as u64)).await;
        }
    }
    Err(last_error)
}

pub async fn install_downloaded_update(
    state: &Arc<AppState>,
    file_path: String,
) -> Result<Value, String> {
    // 旧一轮的报告先清掉，否则下面的启动握手会把残留文件误认为助手已接手。
    crate::update_helper::clear_update_install_report(state.store.path());
    if let Err(error) = start_downloaded_update(state, &file_path).await {
        if let Some(mut pending) = read_pending_device_update(state).await {
            pending.status = Some("failed".to_string());
            pending.message = Some(error.clone());
            let _ = write_pending_device_update(state, &pending).await;
            if report_publish_event(state, &pending.publish_id, "failed", Some(&error))
                .await
                .is_ok()
                && let Ok(path) = pending_device_update_path(&state.store).await
            {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
        return Err(error);
    }

    #[cfg(target_os = "windows")]
    {
        // 助手自己也要能被创建出来。若它在真正开始前就夭折（被安全软件拦下、
        // 更新缓存被清理等），这里保持主进程存活并把错误交给前端，用户就不会
        // 看到"Codey 消失、版本没变、也没有任何提示"。
        let config_path = state.store.path().to_path_buf();
        let started = tokio::task::spawn_blocking(move || {
            crate::update_helper::wait_for_helper_start(&config_path, HELPER_START_TIMEOUT)
        })
        .await
        .unwrap_or(false);
        if !started {
            crate::update_helper::clear_update_install_report(state.store.path());
            return Err("更新助手未能启动，请重新打开 Codey 后重试".to_string());
        }
    }

    let shutdown_state = Arc::clone(state);
    tokio::spawn(async move {
        // Let the bridge deliver the response before Codex/Codey starts
        // normal shutdown and releases the executable for replacement.
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown_state.request_update_shutdown();
    });
    Ok(json!({"status":"installing"}))
}

/// 等待助手写下启动记录的窗口。助手要做的是复制一份自身并落一份 JSON，正常
/// 情况瞬间完成；慢磁盘和首次的杀毒扫描可能拖慢，给到 20 秒仍然留有余量。
#[cfg(target_os = "windows")]
const HELPER_START_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) async fn start_downloaded_update(
    state: &AppState,
    file_path: &str,
) -> Result<(), String> {
    let expected_update = resolve_expected_update(state).await?;
    let verified = verify_downloaded_update(&state.store, file_path, &expected_update).await?;
    spawn_update_installer(&verified.path, &verified.asset)
}

/// 取用于安装前比对的更新信息。优先用后台检查留下的结果；那份结果可能因为
/// 进程重启或缓存过期而缺失，此时重新取一次清单，而不是直接拒绝安装——用户
/// 明明已经下载好了安装包。
async fn resolve_expected_update(state: &AppState) -> Result<UpdateCheck, String> {
    if let Some(check) = state.available_update.read().await.clone()
        && check.update_available
        && check.selected_asset.is_some()
    {
        return Ok(check);
    }
    let check = if let Some(base_url) = configured_release_admin_url(state).await? {
        fetch_release_admin_update(state, &base_url).await?
    } else {
        let manifest_url = configured_update_manifest_url(state).await?;
        let manifest = fetch_configured_update_manifest(state, &manifest_url).await?;
        assess_update_manifest(env!("CARGO_PKG_VERSION"), &manifest)?
    };
    if !check.update_available {
        return Err("当前已是最新版本，无需安装更新".to_string());
    }
    Ok(check)
}

async fn configured_update_manifest_url(state: &AppState) -> Result<String, String> {
    let manifest_url = state
        .config
        .read()
        .await
        .update_manifest_url
        .trim()
        .to_string();
    if manifest_url.is_empty() {
        return Err("内置更新地址未配置，请检查构建配置".to_string());
    }
    Ok(manifest_url)
}

async fn fetch_configured_update_manifest(
    state: &AppState,
    manifest_url: &str,
) -> Result<UpdateManifest, String> {
    let url = reqwest::Url::parse(manifest_url)
        .map_err(|_| "更新地址必须是有效的 HTTPS URL".to_string())?;
    if url.scheme() != "https" {
        return Err("更新地址必须使用 HTTPS".to_string());
    }

    let response = state
        .http_client
        .get(url)
        .header(
            USER_AGENT,
            format!("Codey/{} update-check", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("检查更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("更新地址返回异常：{error}"))?;
    if response.url().scheme() != "https" {
        return Err("更新地址重定向到了非 HTTPS 地址".to_string());
    }

    let body = crate::http_response::read_bounded_body(
        response,
        MAX_UPDATE_MANIFEST_BYTES,
        "更新清单响应",
    )
    .await
    .map_err(|error| format!("读取更新清单失败：{error:#}"))?;
    let manifest = serde_json::from_slice::<UpdateManifest>(&body)
        .map_err(|error| format!("更新清单格式无效：{error}"))?;
    Ok(manifest)
}

pub(super) fn assess_update_manifest(
    current_version: &str,
    manifest: &UpdateManifest,
) -> Result<UpdateCheck, String> {
    if manifest.schema_version != 1 {
        return Err(format!("不支持的更新清单版本：{}", manifest.schema_version));
    }
    if manifest.tag != format!("v{}", manifest.version) {
        return Err("更新清单的版本和标签不一致".to_string());
    }
    if manifest.assets.is_empty() {
        return Err("更新清单没有可下载的安装包".to_string());
    }
    for asset in &manifest.assets {
        validate_update_asset(asset)?;
    }

    let current =
        Version::parse(current_version).map_err(|error| format!("当前版本格式无效：{error}"))?;
    let latest = Version::parse(&manifest.version)
        .map_err(|error| format!("更新清单版本格式无效：{error}"))?;
    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        update_available: latest > current,
        selected_asset: selected_update_asset(&manifest.assets).map(|asset| asset_info(&asset)),
        release_notes: manifest.release_notes.clone(),
        publish_id: None,
    })
}

fn validate_update_asset(asset: &UpdateManifestAsset) -> Result<(), String> {
    validate_update_asset_fields(
        &asset.platform,
        &asset.arch,
        &asset.package_type,
        &asset.file_name,
        &asset.url,
        &asset.sha256,
        asset.size,
    )
}

pub(super) fn current_update_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
    }
}

pub(super) fn current_update_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

fn current_os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        std::env::consts::OS
    }
}

/// 系统版本只用于发布管理展示，探测一次后缓存，失败时返回 None 而不是阻断检查更新。
fn current_os_version() -> Option<String> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE.get_or_init(platform_os_version).clone()
}

#[cfg(target_os = "macos")]
fn platform_os_version() -> Option<String> {
    let text = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
    let marker = "<key>ProductVersion</key>";
    let rest = &text[text.find(marker)? + marker.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].trim().to_string())
}

#[cfg(target_os = "windows")]
fn platform_os_version() -> Option<String> {
    // `ver` 输出中的版本号与系统语言无关，只解析其中的数字部分。
    let output = std::process::Command::new("cmd")
        .args(["/c", "ver"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let raw = text.split_whitespace().find(|part| {
        part.chars().filter(|character| *character == '.').count() >= 2
            && part
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.' || character == ']')
    })?;
    let version = raw.trim_matches(|character: char| !character.is_ascii_digit());
    let build = version.split('.').nth(2)?.parse::<u64>().ok()?;
    // 系统名称已由 osName 上报，这里只补世代与 build，渲染成「Windows 11 26100」。
    let generation = if build >= 22000 { "11" } else { "10" };
    Some(format!("{generation} {build}"))
}

#[cfg(target_os = "linux")]
fn platform_os_version() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    for key in ["PRETTY_NAME=", "NAME="] {
        if let Some(value) = text
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(|value| value.trim().trim_matches('"').trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }
    Some("Linux".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_os_version() -> Option<String> {
    None
}

/// Codex 版本在启动阶段已被探测并缓存；缓存缺失时补一次探测。
async fn current_codex_version() -> Option<String> {
    if let Some(version) = crate::error_log::cached_codex_version() {
        return Some(version);
    }
    tokio::task::spawn_blocking(|| crate::error_log::refresh_codex_app_version(None, None))
        .await
        .ok()
        .flatten()
}

fn installable_package_priority(asset: &UpdateManifestAsset) -> Option<u8> {
    match (
        current_update_platform(),
        asset.package_type.trim().to_ascii_lowercase().as_str(),
    ) {
        ("windows", "nsis") => Some(0),
        ("macos", "app-zip") => Some(0),
        _ => None,
    }
}

fn selected_update_asset(assets: &[UpdateManifestAsset]) -> Option<UpdateManifestAsset> {
    let platform = current_update_platform();
    let arch = current_update_arch();
    assets
        .iter()
        .filter_map(|asset| {
            if !asset.platform.eq_ignore_ascii_case(platform)
                || !asset.arch.eq_ignore_ascii_case(arch)
            {
                return None;
            }
            installable_package_priority(asset).map(|priority| (priority, asset))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, asset)| asset.clone())
}

fn asset_info(asset: &UpdateManifestAsset) -> UpdateAssetInfo {
    UpdateAssetInfo {
        platform: asset.platform.clone(),
        arch: asset.arch.clone(),
        package_type: asset.package_type.clone(),
        file_name: asset.file_name.clone(),
        url: asset.url.clone(),
        sha256: asset.sha256.clone(),
        size: asset.size,
    }
}

fn update_download_dir(store: &ConfigStore) -> Result<PathBuf, String> {
    let parent = store
        .path()
        .parent()
        .ok_or_else(|| "Codey 配置路径无父目录，无法创建更新缓存".to_string())?;
    Ok(parent.join("updates"))
}

async fn download_update_asset(
    client: &reqwest::Client,
    store: &ConfigStore,
    version: &str,
    asset: &UpdateAssetInfo,
) -> Result<PathBuf, String> {
    validate_update_asset_info(asset)?;
    let url = reqwest::Url::parse(&asset.url)
        .map_err(|_| format!("安装包地址无效：{}", asset.file_name))?;
    let directory = update_download_dir(store)?.join(format!("v{version}"));
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| format!("创建更新缓存目录失败：{error}"))?;
    let destination = directory.join(&asset.file_name);
    let temporary = directory.join(format!(".{}.download", asset.file_name));
    let _ = tokio::fs::remove_file(&temporary).await;

    let response = client
        .get(url)
        .header(
            USER_AGENT,
            format!("Codey/{} update-download", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|error| format!("下载更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("下载安装包失败：{error}"))?;
    if response.url().scheme() != "https" {
        return Err("安装包地址重定向到了非 HTTPS 地址".to_string());
    }

    let verified_download = async {
        let mut file = tokio::fs::File::create(&temporary)
            .await
            .map_err(|error| format!("创建更新缓存文件失败：{error}"))?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut bytes_written = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("读取更新下载数据失败：{error}"))?;
            bytes_written += chunk.len() as u64;
            if bytes_written > asset.size {
                return Err("下载的安装包大小超过更新清单声明".to_string());
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("写入更新缓存文件失败：{error}"))?;
        }
        file.flush()
            .await
            .map_err(|error| format!("刷新更新缓存文件失败：{error}"))?;
        drop(file);

        if bytes_written != asset.size {
            return Err(format!(
                "安装包大小不一致：期望 {} 字节，实际 {} 字节",
                asset.size, bytes_written
            ));
        }
        let actual_sha256 = format!("{:x}", hasher.finalize());
        if !actual_sha256.eq_ignore_ascii_case(&asset.sha256) {
            return Err("安装包 SHA-256 校验失败".to_string());
        }
        Ok(())
    }
    .await;
    if let Err(error) = verified_download {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    let _ = tokio::fs::remove_file(&destination).await;
    if let Err(error) = tokio::fs::rename(&temporary, &destination).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!("保存更新安装包失败：{error}"));
    }
    Ok(destination)
}

fn validate_update_asset_info(asset: &UpdateAssetInfo) -> Result<(), String> {
    validate_update_asset_fields(
        &asset.platform,
        &asset.arch,
        &asset.package_type,
        &asset.file_name,
        &asset.url,
        &asset.sha256,
        asset.size,
    )
}

fn validate_update_asset_fields(
    platform: &str,
    arch: &str,
    package_type: &str,
    file_name: &str,
    url: &str,
    sha256: &str,
    size: u64,
) -> Result<(), String> {
    if platform.trim().is_empty()
        || arch.trim().is_empty()
        || package_type.trim().is_empty()
        || file_name.trim().is_empty()
        || size == 0
    {
        return Err("更新清单包含不完整的安装包信息".to_string());
    }
    if file_name.contains(['/', '\\']) || Path::new(file_name).components().count() != 1 {
        return Err(format!("安装包文件名无效：{file_name}"));
    }
    let url = reqwest::Url::parse(url).map_err(|_| format!("安装包地址无效：{file_name}"))?;
    if url.scheme() != "https" {
        return Err(format!("安装包地址必须使用 HTTPS：{file_name}"));
    }
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("安装包 SHA-256 无效：{file_name}"));
    }
    Ok(())
}

fn validate_downloaded_update_path(
    store: &ConfigStore,
    file_path: &str,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(file_path);
    if !path.is_absolute() {
        return Err("更新安装包路径必须是绝对路径".to_string());
    }
    let root = update_download_dir(store)?
        .canonicalize()
        .map_err(|error| format!("读取更新缓存目录失败：{error}"))?;
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("读取更新安装包失败：{error}"))?;
    if !canonical.starts_with(&root) {
        return Err("只能安装 Codey 下载缓存中的更新包".to_string());
    }
    if !canonical.is_file() {
        return Err("更新安装包路径必须指向文件".to_string());
    }
    Ok(canonical)
}

async fn verify_downloaded_update(
    store: &ConfigStore,
    file_path: &str,
    expected_update: &UpdateCheck,
) -> Result<VerifiedDownloadedUpdate, String> {
    if !expected_update.update_available {
        return Err("当前没有可安装的更新".to_string());
    }
    let asset = expected_update
        .selected_asset
        .as_ref()
        .ok_or_else(|| "没有适用于当前系统的可安装更新包".to_string())?;
    validate_update_asset_info(asset)?;
    let version = Version::parse(&expected_update.latest_version)
        .map_err(|error| format!("待安装更新版本无效：{error}"))?;
    let update_path = validate_downloaded_update_path(store, file_path)?;
    let expected_directory = update_download_dir(store)?
        .join(format!("v{version}"))
        .canonicalize()
        .map_err(|error| format!("读取预期更新缓存目录失败：{error}"))?;
    let expected_path = expected_directory.join(&asset.file_name);
    if update_path != expected_path {
        return Err("更新安装包与最近下载的版本不匹配，请重新下载".to_string());
    }

    let metadata = tokio::fs::metadata(&update_path)
        .await
        .map_err(|error| format!("读取更新安装包信息失败：{error}"))?;
    if metadata.len() != asset.size {
        return Err(format!(
            "安装前校验失败：安装包大小应为 {} 字节，实际为 {} 字节",
            asset.size,
            metadata.len()
        ));
    }

    // The cache can be modified after the original download completed. Hash
    // the final on-disk bytes again immediately before launching the installer.
    let mut file = tokio::fs::File::open(&update_path)
        .await
        .map_err(|error| format!("打开更新安装包失败：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes_read = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| format!("安装前读取更新安装包失败：{error}"))?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(read as u64);
        if bytes_read > asset.size {
            return Err("安装前校验失败：安装包大小超过更新清单声明".to_string());
        }
        hasher.update(&buffer[..read]);
    }
    if bytes_read != asset.size {
        return Err(format!(
            "安装前校验失败：安装包大小应为 {} 字节，实际为 {} 字节",
            asset.size, bytes_read
        ));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(&asset.sha256) {
        return Err("安装前校验失败：安装包 SHA-256 不匹配，请重新下载".to_string());
    }
    Ok(VerifiedDownloadedUpdate {
        path: update_path,
        asset: asset.clone(),
    })
}

#[cfg(target_os = "windows")]
fn spawn_update_installer(update_path: &Path, asset: &UpdateAssetInfo) -> Result<(), String> {
    crate::update_helper::spawn_update_installer(update_path, asset.size, &asset.sha256)
}

#[cfg(target_os = "macos")]
fn spawn_update_installer(update_path: &Path, asset: &UpdateAssetInfo) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    if !update_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err("macOS 更新安装包必须是 .zip".to_string());
    }
    let app_bundle = current_macos_app_bundle()
        .ok_or_else(|| "当前 Codey 不是从 .app 包运行，无法自动替换".to_string())?;
    let script_path = update_path
        .parent()
        .ok_or_else(|| "更新安装包路径无父目录".to_string())?
        .join(format!(
            ".install-codey-update-{}-{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
    let mut script = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&script_path)
        .map_err(|error| format!("创建更新安装脚本失败：{error}"))?;
    if let Err(error) = script.write_all(
        r#"#!/bin/sh
set -eu
rm -f "$0"
parent_pid="$1"
archive="$2"
app_bundle="$3"
app_parent="$(dirname "$app_bundle")"
app_name="$(basename "$app_bundle")"
stage_dir=""
tmp_dir=""
backup_bundle=""
replacement_committed=0
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ "$replacement_committed" -ne 1 ] && [ -n "$backup_bundle" ] && [ -e "$backup_bundle" ]; then
    rm -rf "$app_bundle"
    /bin/mv "$backup_bundle" "$app_bundle" || true
  fi
  if [ -n "$stage_dir" ]; then rm -rf "$stage_dir"; fi
  if [ -n "$tmp_dir" ]; then rm -rf "$tmp_dir"; fi
  exit "$status"
}
trap cleanup EXIT HUP INT TERM
while kill -0 "$parent_pid" 2>/dev/null; do
  sleep 0.2
done
expected_size="$4"
expected_sha256="$5"
archive_parent="$(dirname "$archive")"
archive_name="$(basename "$archive")"
stage_dir="$(/usr/bin/mktemp -d "$archive_parent/.codey-update-stage.XXXXXX")"
/bin/chmod 700 "$stage_dir"
staged_archive="$stage_dir/$archive_name"
/bin/mv "$archive" "$staged_archive"
/bin/chmod 400 "$staged_archive"
actual_size="$(/usr/bin/stat -f '%z' "$staged_archive")"
test "$actual_size" = "$expected_size"
actual_sha256="$(/usr/bin/shasum -a 256 "$staged_archive" | /usr/bin/awk '{print $1}')"
test "$(printf '%s' "$actual_sha256" | /usr/bin/tr '[:upper:]' '[:lower:]')" = "$(printf '%s' "$expected_sha256" | /usr/bin/tr '[:upper:]' '[:lower:]')"
tmp_dir="$app_parent/.${app_name}.codey-update.$$"
rm -rf "$tmp_dir"
mkdir -p "$tmp_dir"
/usr/bin/ditto -x -k "$staged_archive" "$tmp_dir"
test -d "$tmp_dir/$app_name"
backup_bundle="$app_parent/.${app_name}.codey-backup.$$"
rm -rf "$backup_bundle"
if [ -e "$app_bundle" ]; then
  /bin/mv "$app_bundle" "$backup_bundle"
fi
if ! /bin/mv "$tmp_dir/$app_name" "$app_bundle"; then
  exit 1
fi
if ! /usr/bin/open "$app_bundle"; then
  exit 1
fi
replacement_committed=1
if [ -e "$backup_bundle" ]; then rm -rf "$backup_bundle"; fi
"#
        .as_bytes(),
    ) {
        let _ = fs::remove_file(&script_path);
        return Err(format!("写入更新安装脚本失败：{error}"));
    }
    if let Err(error) = script.sync_all() {
        let _ = fs::remove_file(&script_path);
        return Err(format!("刷新更新安装脚本失败：{error}"));
    }
    drop(script);
    if let Err(error) = fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700)) {
        let _ = fs::remove_file(&script_path);
        return Err(format!("设置更新安装脚本权限失败：{error}"));
    }
    let spawn_result = std::process::Command::new("/bin/sh")
        .arg(&script_path)
        .arg(std::process::id().to_string())
        .arg(update_path)
        .arg(app_bundle)
        .arg(asset.size.to_string())
        .arg(&asset.sha256)
        .spawn();
    if let Err(error) = spawn_result {
        let _ = fs::remove_file(&script_path);
        return Err(format!("启动更新安装脚本失败：{error}"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn current_macos_app_bundle() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .ancestors()
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("app"))
        .map(Path::to_path_buf)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn spawn_update_installer(_update_path: &Path, _asset: &UpdateAssetInfo) -> Result<(), String> {
    Err("当前平台暂不支持自动安装更新".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_asset() -> UpdateManifestAsset {
        UpdateManifestAsset {
            platform: current_update_platform().to_string(),
            arch: current_update_arch().to_string(),
            package_type: if cfg!(target_os = "windows") {
                "nsis"
            } else if cfg!(target_os = "macos") {
                "app-zip"
            } else {
                "unsupported"
            }
            .to_string(),
            file_name: "codey-update.pkg".to_string(),
            url: "https://updates.example.test/codey-update.pkg".to_string(),
            sha256: "a".repeat(64),
            size: 42,
        }
    }

    fn valid_manifest(version: &str) -> UpdateManifest {
        UpdateManifest {
            schema_version: 1,
            version: version.to_string(),
            tag: format!("v{version}"),
            release_notes: None,
            assets: vec![valid_asset()],
        }
    }

    fn install_check(version: &str, file_name: &str, contents: &[u8]) -> UpdateCheck {
        let mut asset = asset_info(&valid_asset());
        asset.file_name = file_name.to_string();
        asset.size = contents.len() as u64;
        asset.sha256 = format!("{:x}", Sha256::digest(contents));
        UpdateCheck {
            current_version: "1.0.0".to_string(),
            latest_version: version.to_string(),
            update_available: true,
            selected_asset: Some(asset),
            release_notes: None,
            publish_id: None,
        }
    }

    #[test]
    fn update_manifest_validation_rejects_invalid_security_fields() {
        let mut manifest = valid_manifest("2.0.0");
        assert!(
            assess_update_manifest("1.0.0", &manifest)
                .unwrap()
                .update_available
        );

        manifest.tag = "v1.0.0".to_string();
        assert!(
            assess_update_manifest("1.0.0", &manifest)
                .unwrap_err()
                .contains("版本和标签不一致")
        );

        manifest = valid_manifest("2.0.0");
        manifest.assets[0].sha256 = "not-a-sha".to_string();
        assert!(
            assess_update_manifest("1.0.0", &manifest)
                .unwrap_err()
                .contains("SHA-256")
        );

        manifest = valid_manifest("2.0.0");
        manifest.assets[0].file_name = "../escape.pkg".to_string();
        assert!(
            assess_update_manifest("1.0.0", &manifest)
                .unwrap_err()
                .contains("文件名无效")
        );
    }

    #[test]
    fn update_manifest_equal_version_is_not_available() {
        let check = assess_update_manifest("2.0.0", &valid_manifest("2.0.0")).unwrap();

        assert!(!check.update_available);
        assert_eq!(check.current_version, "2.0.0");
        assert_eq!(check.latest_version, "2.0.0");
    }

    #[test]
    fn update_candidate_cache_obeys_url_and_ttl() {
        let checked_at = Instant::now();
        let candidate = UpdateCandidate {
            check: assess_update_manifest("1.0.0", &valid_manifest("2.0.0")).unwrap(),
        };
        let cached = CachedUpdateCandidate {
            manifest_url: "https://updates.example.test/manifest.json".to_string(),
            candidate: candidate.clone(),
            checked_at,
        };

        assert_eq!(
            reusable_update_candidate(
                Some(&cached),
                "https://updates.example.test/manifest.json",
                checked_at + UPDATE_CHECK_CACHE_TTL - Duration::from_millis(1),
                UPDATE_CHECK_CACHE_TTL,
            )
            .unwrap()
            .check,
            candidate.check,
        );
        assert!(
            reusable_update_candidate(
                Some(&cached),
                "https://updates.example.test/manifest.json",
                checked_at + UPDATE_CHECK_CACHE_TTL,
                UPDATE_CHECK_CACHE_TTL,
            )
            .is_none()
        );
        assert!(
            reusable_update_candidate(
                Some(&cached),
                "https://mirror.example.test/manifest.json",
                checked_at,
                UPDATE_DOWNLOAD_CACHE_TTL,
            )
            .is_none()
        );
    }

    #[test]
    fn downloaded_update_path_must_be_a_file_inside_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        let update_root = directory.path().join("updates");
        let version_dir = update_root.join("v2.0.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        let update = version_dir.join("codey-update.pkg");
        std::fs::write(&update, b"verified").unwrap();
        let outside = directory.path().join("outside.pkg");
        std::fs::write(&outside, b"outside").unwrap();

        assert_eq!(
            validate_downloaded_update_path(&store, update.to_str().unwrap()).unwrap(),
            update.canonicalize().unwrap()
        );
        assert!(
            validate_downloaded_update_path(&store, "relative.pkg")
                .unwrap_err()
                .contains("绝对路径")
        );
        assert!(
            validate_downloaded_update_path(&store, outside.to_str().unwrap())
                .unwrap_err()
                .contains("下载缓存")
        );
        assert!(
            validate_downloaded_update_path(&store, version_dir.to_str().unwrap())
                .unwrap_err()
                .contains("必须指向文件")
        );
    }

    #[tokio::test]
    async fn downloaded_update_is_reverified_immediately_before_install() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        let version_dir = directory.path().join("updates/v2.0.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        let update = version_dir.join("codey-update.pkg");
        let original = b"verified";
        std::fs::write(&update, original).unwrap();
        let check = install_check("2.0.0", "codey-update.pkg", original);

        assert_eq!(
            verify_downloaded_update(&store, update.to_str().unwrap(), &check)
                .await
                .unwrap()
                .path,
            update.canonicalize().unwrap()
        );

        // Keep the byte length unchanged so the digest check, rather than only
        // the metadata check, proves that replacement is rejected.
        std::fs::write(&update, b"tampered").unwrap();
        assert!(
            verify_downloaded_update(&store, update.to_str().unwrap(), &check)
                .await
                .unwrap_err()
                .contains("SHA-256")
        );
    }

    #[tokio::test]
    async fn downloaded_update_must_match_the_selected_version_and_file_name() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        let version_dir = directory.path().join("updates/v2.0.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        let unexpected = version_dir.join("other.pkg");
        let contents = b"verified";
        std::fs::write(&unexpected, contents).unwrap();
        let check = install_check("2.0.0", "codey-update.pkg", contents);

        assert!(
            verify_downloaded_update(&store, unexpected.to_str().unwrap(), &check)
                .await
                .unwrap_err()
                .contains("最近下载的版本")
        );
    }

    #[tokio::test]
    async fn downloaded_update_requires_a_current_valid_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        let mut check = install_check("2.0.0", "codey-update.pkg", b"verified");

        check.update_available = false;
        assert!(
            verify_downloaded_update(&store, "unused", &check)
                .await
                .unwrap_err()
                .contains("没有可安装")
        );

        check.update_available = true;
        check.selected_asset = None;
        assert!(
            verify_downloaded_update(&store, "unused", &check)
                .await
                .unwrap_err()
                .contains("适用于当前系统")
        );

        check = install_check("not-a-version", "codey-update.pkg", b"verified");
        assert!(
            verify_downloaded_update(&store, "unused", &check)
                .await
                .unwrap_err()
                .contains("版本无效")
        );
    }

    #[cfg(unix)]
    #[test]
    fn downloaded_update_path_rejects_symlinks_that_escape_the_cache() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        let update_root = directory.path().join("updates");
        std::fs::create_dir_all(&update_root).unwrap();
        let outside = directory.path().join("outside.pkg");
        std::fs::write(&outside, b"outside").unwrap();
        let link = update_root.join("linked.pkg");
        symlink(&outside, &link).unwrap();

        assert!(
            validate_downloaded_update_path(&store, link.to_str().unwrap())
                .unwrap_err()
                .contains("下载缓存")
        );
    }
}
