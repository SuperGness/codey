//! Scoped environment delivery through native Windows Store activation.
#[cfg(windows)]
use super::platform::{registered_windows_packages, windows_package_full_name};
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::path::PathBuf;

#[cfg(windows)]
const STATE_DIRECTORY: &str = "windows-package-launch";

/// Only the package identity is persisted, never environment values. The lock
/// serializes launch settings across Codey sessions for the current user.
struct LaunchJournal {
    path: PathBuf,
    _lock: File,
}

impl LaunchJournal {
    fn acquire(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory).context("创建 Windows 包启动状态目录失败")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("environment.lock"))
            .context("打开 Windows 包启动状态锁失败")?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("另一个 Codey 会话正在处理 Windows 包启动设置，请稍后重试")?;
        Ok(Self {
            path: directory.join("pending.json"),
            _lock: lock,
        })
    }

    fn recover(&self, mut disable: impl FnMut(&str) -> Result<()>) -> Result<()> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("读取 Windows 包启动清理记录失败"),
        };
        let package: String = serde_json::from_slice(&bytes)
            .context("Windows 包启动清理记录损坏，已停止修改调试设置")?;
        validate_package_record(&package)?;
        disable(&package)?;
        // Never forget a setting until Windows confirms its removal.
        crate::fs_util::remove_file_if_exists(&self.path)
            .context("删除 Windows 包启动清理记录失败")?;
        Ok(())
    }

    fn arm(&self, package: &str) -> Result<()> {
        validate_package_record(package)?;
        // Persist before EnableDebugging, including when that call later fails.
        crate::fs_util::atomic_write_private(&self.path, &serde_json::to_vec(package)?)
            .context("保存 Windows 包启动清理记录失败")
    }
}

fn validate_package_record(package: &str) -> Result<()> {
    anyhow::ensure!(
        !package.contains(['/', '\\', '\0'])
            && codey_runtime_core::app_paths::packaged_app_full_name(Path::new(package)).as_deref()
                == Some(package),
        "Windows 包启动清理记录不属于受支持的 Codex 包"
    );
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
enum ResumeFeedbackState {
    Pending,
    Resumed { process_id: u32 },
    Cancelled,
    Failed(String),
}

struct ResumeFeedback {
    id: uuid::Uuid,
    path: PathBuf,
}

impl ResumeFeedback {
    fn new(directory: &Path) -> Result<Self> {
        let id = uuid::Uuid::new_v4();
        let path = resume_feedback_path(directory, id);
        crate::fs_util::atomic_write_private(
            &path,
            &serde_json::to_vec(&ResumeFeedbackState::Pending)?,
        )?;
        Ok(Self { id, path })
    }
}

impl Drop for ResumeFeedback {
    fn drop(&mut self) {
        let _ = crate::fs_util::remove_file_if_exists(&self.path);
    }
}

fn resume_feedback_path(directory: &Path, id: uuid::Uuid) -> PathBuf {
    directory.join(format!("resume-{id}.json"))
}

fn read_resume_feedback(path: &Path) -> Result<ResumeFeedbackState> {
    serde_json::from_slice(&std::fs::read(path).context("读取 Windows Store 启动助手状态失败")?)
        .context("Windows Store 启动助手状态无效")
}

fn require_pending_resume(path: &Path) -> Result<()> {
    anyhow::ensure!(
        matches!(read_resume_feedback(path)?, ResumeFeedbackState::Pending),
        "本次 Windows Store 启动已停止，拒绝恢复线程"
    );
    Ok(())
}

pub(super) fn cancel_resume_feedback(path: Option<&Path>) -> Result<()> {
    if let Some(path) = path {
        crate::fs_util::atomic_write_private(
            path,
            &serde_json::to_vec(&ResumeFeedbackState::Cancelled)?,
        )?;
    }
    Ok(())
}

pub(super) async fn wait_for_resume(path: Option<&Path>, process_id: u32) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    loop {
        match read_resume_feedback(path)? {
            ResumeFeedbackState::Pending => {}
            ResumeFeedbackState::Resumed {
                process_id: resumed,
            } => {
                anyhow::ensure!(
                    resumed == process_id,
                    "Windows Store 线程恢复确认不属于本次激活进程"
                );
                return Ok(());
            }
            ResumeFeedbackState::Failed(detail) => {
                anyhow::bail!("Windows Store 线程恢复失败：{detail}");
            }
            ResumeFeedbackState::Cancelled => {
                anyhow::bail!("Windows Store 线程恢复已取消");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

pub(super) async fn wait_for_resume_failure(path: Option<PathBuf>) -> anyhow::Error {
    let Some(path) = path else {
        return std::future::pending().await;
    };
    loop {
        match read_resume_feedback(&path) {
            Ok(ResumeFeedbackState::Pending | ResumeFeedbackState::Resumed { .. }) => {}
            Ok(ResumeFeedbackState::Failed(detail)) => {
                return anyhow::anyhow!("Windows Store 线程恢复失败：{detail}");
            }
            Ok(ResumeFeedbackState::Cancelled) => {
                return anyhow::anyhow!("Windows Store 线程恢复已取消");
            }
            Err(error) => return error,
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(windows)]
pub(crate) fn resume_windows_packaged_thread(
    process_id: u32,
    thread_id: u32,
    launch_id: uuid::Uuid,
    path: &Path,
) -> Result<()> {
    anyhow::ensure!(
        path.is_absolute()
            && path.file_name() == Some(std::ffi::OsStr::new(&format!("resume-{launch_id}.json"))),
        "Windows Store 启动助手通知路径与本次启动不匹配"
    );
    let result = require_pending_resume(path)
        .and_then(|()| resume_windows_packaged_thread_inner(process_id, thread_id, path));
    if let Err(error) = &result
        && path.exists()
    {
        let _ = crate::fs_util::atomic_write_private(
            path,
            &serde_json::to_vec(&ResumeFeedbackState::Failed(format!("{error:#}")))?,
        );
    }
    result
}

#[cfg(windows)]
fn resume_windows_packaged_thread_inner(
    process_id: u32,
    thread_id: u32,
    feedback: &Path,
) -> Result<()> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        GetProcessIdOfThread, OpenThread, PROCESS_NAME_WIN32, QueryFullProcessImageNameW,
        ResumeThread, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
    };
    use windows::core::PWSTR;

    // Keep both kernel objects alive so PID/TID reuse cannot change the target.
    let process = super::platform::WindowsStartupProcess::open(process_id)?;
    let thread = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            false,
            thread_id,
        )
    }
    .context("打开 Windows Store Codex 启动线程失败")?;
    let thread = unsafe { OwnedHandle::from_raw_handle(thread.0) };
    let thread_handle = HANDLE(thread.as_raw_handle());
    let owner = unsafe { GetProcessIdOfThread(thread_handle) };
    anyhow::ensure!(
        owner != 0 && owner == process_id,
        "Windows Store 启动线程不属于指定进程"
    );
    let package = process.package_full_name()?;
    validate_package_record(&package)?;
    let app_dir = super::platform::refresh_windows_packaged_app_dir(Path::new(&package))?;
    anyhow::ensure!(
        windows_package_full_name(&app_dir).as_deref() == Some(package.as_str()),
        "Windows Store 启动进程不属于当前注册的 Codex 包"
    );
    codey_runtime_core::app_paths::validate_codex_app_dir(&app_dir)?;
    let executable = std::fs::canonicalize(codey_runtime_core::app_paths::build_codex_executable(
        &app_dir,
    ))
    .context("核验 Windows Store Codex 启动程序失败")?;
    let mut image = vec![0u16; 32768];
    let mut length = image.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(process.0.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(image.as_mut_ptr()),
            &mut length,
        )
    }
    .context("读取 Windows Store Codex 进程路径失败")?;
    let image = std::fs::canonicalize(PathBuf::from(std::ffi::OsString::from_wide(
        &image[..length as usize],
    )))
    .context("核验 Windows Store Codex 进程路径失败")?;
    anyhow::ensure!(
        super::platform::normalized_windows_path(&image)
            == super::platform::normalized_windows_path(&executable),
        "Windows Store Codex 实际启动程序与已验证安装不一致"
    );
    require_pending_resume(feedback)?;
    let previous = unsafe { ResumeThread(thread_handle) };
    if previous == u32::MAX {
        return Err(windows::core::Error::from_win32())
            .context("恢复 Windows Store Codex 主线程失败");
    }
    anyhow::ensure!(previous <= 1, "Windows Store Codex 主线程仍处于暂停状态");
    crate::fs_util::atomic_write_private(
        feedback,
        &serde_json::to_vec(&ResumeFeedbackState::Resumed { process_id })?,
    )
    .context("保存 Windows Store 线程恢复确认失败")?;
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.windows_package_thread_resumed",
        serde_json::json!({"processId": process_id, "threadId": thread_id, "package": package, "previousSuspendCount": previous}),
    );
    Ok(())
}
#[cfg(any(windows, test))]
fn windows_package_is_unregistered(previous: &str, registered: &[String]) -> bool {
    let family = codey_runtime_core::app_paths::packaged_app_user_model_id(Path::new(previous));
    family.is_some()
        && registered.iter().all(|current| {
            current != previous
                && codey_runtime_core::app_paths::packaged_app_user_model_id(Path::new(current))
                    == family
        })
}

#[cfg(any(windows, test))]
fn windows_environment_block(environment: &[(String, String)]) -> Result<Vec<u16>> {
    let mut entries = environment
        .iter()
        .map(|(name, value)| {
            anyhow::ensure!(
                !name.is_empty() && !name.contains(['=', '\0']) && !value.contains('\0'),
                "Windows Codex 兼容环境包含无效字符"
            );
            Ok(format!("{name}={value}"))
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.to_ascii_uppercase());
    let mut block = Vec::new();
    for entry in entries {
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(windows)]
pub(super) struct WindowsPackageDebugSession {
    journal: LaunchJournal,
    feedback: Option<ResumeFeedback>,
    activation_pending: bool,
}

#[cfg(windows)]
impl WindowsPackageDebugSession {
    pub(super) fn start(app_dir: &Path, environment: &[(String, String)]) -> Result<Self> {
        let package_full_name =
            windows_package_full_name(app_dir).context("无法识别 Windows Store Codex 包全名")?;
        let mut session = Self::clean()?;
        session.feedback = Some(ResumeFeedback::new(session.journal.path.parent().unwrap())?);
        session.journal.arm(&package_full_name)?;
        if let Err(error) = enable_windows_packaged_environment(
            &package_full_name,
            environment,
            session.feedback.as_ref().unwrap(),
        ) {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_package_environment_failed",
                serde_json::json!({
                    "package": package_full_name,
                    "environmentEntryCount": environment.len(),
                    "detail": format!("{error:#}"),
                    "hresult": error.downcast_ref::<windows::core::Error>()
                        .map(|native| format!("0x{:08X}", native.code().0 as u32)),
                }),
            );
            let cleared = session.finish();
            return Err(super::platform::startup_activation_error_after_cleanup(
                super::recovery::recoverable(error),
                Ok(()),
                cleared,
            ));
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.windows_package_environment_enabled",
            serde_json::json!({ "package": package_full_name }),
        );
        Ok(session)
    }

    pub(super) fn clean() -> Result<Self> {
        let journal = LaunchJournal::acquire(
            &codey_runtime_core::paths::default_app_state_dir().join(STATE_DIRECTORY),
        )?;
        journal.recover(disable_windows_packaged_environment)?;
        Ok(Self {
            journal,
            feedback: None,
            activation_pending: false,
        })
    }

    pub(super) fn feedback_path(&self) -> Option<PathBuf> {
        self.feedback.as_ref().map(|feedback| feedback.path.clone())
    }

    pub(super) fn begin_activation(&mut self) {
        self.activation_pending = true;
    }

    pub(super) fn finish(mut self) -> Result<()> {
        self.activation_pending = false;
        self.journal.recover(disable_windows_packaged_environment)
    }
}

#[cfg(windows)]
impl Drop for WindowsPackageDebugSession {
    fn drop(&mut self) {
        // Runtime cancellation must retain the journal while COM can still launch.
        if self.activation_pending {
            return;
        }
        if let Err(error) = self.journal.recover(disable_windows_packaged_environment) {
            crate::error_log::record_failure(
                "cleanup_failed",
                "clear_windows_package_environment",
                format!("{error:#}"),
                serde_json::json!({ "pendingCleanupRetained": true }),
            );
        }
    }
}

#[cfg(windows)]
fn with_windows_package_debug_settings<T>(
    operation: impl FnOnce(
        &windows::Win32::UI::Shell::IPackageDebugSettings,
    ) -> windows::core::Result<T>,
) -> Result<T> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Shell::{IPackageDebugSettings, PackageDebugSettings};

    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninitialize = initialized.is_ok();
        initialized.ok().or_else(|error| {
            const RPC_E_CHANGED_MODE: i32 = -2147417850;
            if error.code().0 == RPC_E_CHANGED_MODE {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        let result = (|| {
            let settings: IPackageDebugSettings =
                CoCreateInstance(&PackageDebugSettings, None, CLSCTX_INPROC_SERVER)?;
            operation(&settings)
        })();
        if should_uninitialize {
            CoUninitialize();
        }
        result.map_err(Into::into)
    }
}

#[cfg(windows)]
fn enable_windows_packaged_environment(
    package_full_name: &str,
    environment: &[(String, String)],
    feedback: &ResumeFeedback,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;

    let package_full_name = package_full_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let executable = std::env::current_exe().context("定位 Codey 包启动恢复助手失败")?;
    let feedback_path =
        std::fs::canonicalize(&feedback.path).context("定位 Windows Store 启动助手通知文件失败")?;
    let mut debugger_command = vec![u16::from(b'"')];
    debugger_command.extend(executable.as_os_str().encode_wide());
    debugger_command.extend(
        format!(
            "\" {} --launch-id {} --launch-state \"",
            crate::codex_startup_patch::WINDOWS_PACKAGE_RESUME_ARGUMENT,
            feedback.id,
        )
        .encode_utf16(),
    );
    // Windows file names cannot contain quotes; this path ends with .json,
    // so there is no trailing backslash to escape the closing quote.
    debugger_command.extend(feedback_path.as_os_str().encode_wide());
    debugger_command.push(u16::from(b'"'));
    debugger_command.push(0);
    let environment = windows_environment_block(environment)?;

    with_windows_package_debug_settings(|settings| unsafe {
        let package = PCWSTR(package_full_name.as_ptr());
        settings.DisableDebugging(package)?;
        settings.EnableDebugging(
            package,
            PCWSTR(debugger_command.as_ptr()),
            PCWSTR(environment.as_ptr()),
        )
    })
    .context("为 Windows Store Codex 安装一次性 CLI 兼容环境失败")
}

#[cfg(windows)]
fn disable_windows_packaged_environment(package_full_name: &str) -> Result<()> {
    use windows::core::PCWSTR;

    let name = package_full_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = with_windows_package_debug_settings(|settings| unsafe {
        settings.DisableDebugging(PCWSTR(name.as_ptr()))
    });
    if result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<windows::core::Error>())
        .is_some_and(|error| {
            error.code() == windows::Win32::Foundation::ERROR_NOT_FOUND.to_hresult()
        })
    {
        // A successful empty query confirms uninstall; a same-family update also
        // retires the old identity. Failed queries and live registrations stay fatal.
        let registered = registered_windows_packages(package_full_name)?;
        if windows_package_is_unregistered(package_full_name, &registered) {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_package_cleanup_after_unregister",
                serde_json::json!({ "previousPackage": package_full_name, "registeredPackages": registered }),
            );
            return Ok(());
        }
    }
    result.context("清理 Windows Store Codex 一次性 CLI 兼容环境失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn activation_keeps_feedback_until_delayed_helper_resumes_target() {
        use super::super::windows_activation::ActivationSupervisor;
        use std::sync::{Arc, atomic::AtomicBool};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let journal = LaunchJournal::acquire(directory.path()).unwrap();
        let feedback = ResumeFeedback::new(directory.path()).unwrap();
        let path = feedback.path.clone();
        let helper_path = path.clone();
        let failure_path = path.clone();
        let (activated, activation_seen) = tokio::sync::oneshot::channel();
        let (reply, mut response) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(
            ActivationSupervisor {
                cancelled: Arc::new(AtomicBool::new(false)),
                timeout: Duration::from_secs(1),
                settle_timeout: Duration::from_millis(100),
            }
            .run(
                async move {
                    // Native activation returns a PID before the debugger runs.
                    activated.send(()).unwrap();
                    wait_for_resume(Some(&helper_path), 42).await?;
                    Ok(42)
                },
                wait_for_resume_failure(Some(failure_path)),
                || async { panic!("successful target must remain running") },
                move || {
                    drop(feedback);
                    drop(journal);
                    Ok(())
                },
                reply,
            ),
        );
        activation_seen.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(matches!(
            response.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        require_pending_resume(&path).unwrap();
        assert!(LaunchJournal::acquire(directory.path()).is_err());
        crate::fs_util::atomic_write_private(
            &path,
            &serde_json::to_vec(&ResumeFeedbackState::Resumed { process_id: 42 }).unwrap(),
        )
        .unwrap();
        assert_eq!(response.await.unwrap().unwrap(), 42);
        task.await.unwrap().unwrap();
        assert!(!path.exists());
        assert!(LaunchJournal::acquire(directory.path()).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn missing_helper_times_out_and_cancels_resume_before_cleanup() {
        use super::super::windows_activation::ActivationSupervisor;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let feedback = ResumeFeedback::new(directory.path()).unwrap();
        let path = feedback.path.clone();
        let stops = AtomicUsize::new(0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, response) = tokio::sync::oneshot::channel();
        ActivationSupervisor {
            cancelled: cancelled.clone(),
            timeout: Duration::from_millis(100),
            settle_timeout: Duration::from_millis(100),
        }
        .run(
            wait_for_resume(Some(&path), 42),
            wait_for_resume_failure(Some(path.clone())),
            || {
                assert!(cancelled.load(Ordering::Acquire));
                stops.fetch_add(1, Ordering::Relaxed);
                let result = cancel_resume_feedback(Some(&path));
                async { result }
            },
            || {
                assert!(require_pending_resume(&path).is_err());
                drop(feedback);
                Ok(())
            },
            reply,
        )
        .await
        .unwrap();
        let error = response.await.unwrap().unwrap_err();
        assert!(error.is::<tokio::time::error::Elapsed>());
        assert!(error.is::<super::super::recovery::IntegrationFailure>());
        assert_eq!(stops.load(Ordering::Relaxed), 2);
        assert!(!path.exists());
    }

    #[tokio::test(start_paused = true)]
    async fn resume_confirmation_requires_matching_success_and_rejects_invalid_states() {
        use std::time::Duration;

        wait_for_resume(None, 42).await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let feedback = ResumeFeedback::new(directory.path()).unwrap();
        let path = Some(feedback.path.as_path());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), wait_for_resume(path, 42))
                .await
                .is_err()
        );
        for (state, expected) in [
            (ResumeFeedbackState::Resumed { process_id: 43 }, "不属于"),
            (
                ResumeFeedbackState::Failed("helper failed".into()),
                "helper failed",
            ),
            (ResumeFeedbackState::Cancelled, "取消"),
        ] {
            crate::fs_util::atomic_write_private(
                &feedback.path,
                &serde_json::to_vec(&state).unwrap(),
            )
            .unwrap();
            assert!(
                wait_for_resume(path, 42)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
        crate::fs_util::atomic_write_private(
            &feedback.path,
            &serde_json::to_vec(&ResumeFeedbackState::Resumed { process_id: 42 }).unwrap(),
        )
        .unwrap();
        wait_for_resume(path, 42).await.unwrap();
        assert!(require_pending_resume(&feedback.path).is_err());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_resume_failure(Some(feedback.path.clone()))
            )
            .await
            .is_err()
        );
        std::fs::write(&feedback.path, b"corrupt").unwrap();
        assert!(
            wait_for_resume(path, 42)
                .await
                .unwrap_err()
                .to_string()
                .contains("无效")
        );
        std::fs::remove_file(&feedback.path).unwrap();
        assert!(wait_for_resume(path, 42).await.is_err());
    }

    const PACKAGE: &str = "OpenAI.Codex_26.924.2738.0_x64__2p2nqsd0c76g0";

    #[tokio::test]
    async fn resume_failure_notification_is_scoped_to_one_launch() {
        let directory = tempfile::tempdir().unwrap();
        let first = ResumeFeedback::new(directory.path()).unwrap();
        let second = ResumeFeedback::new(directory.path()).unwrap();
        assert_ne!(first.id, second.id);
        require_pending_resume(&first.path).unwrap();
        crate::fs_util::atomic_write_private(
            &first.path,
            &serde_json::to_vec(&ResumeFeedbackState::Failed(
                "package validation failed".into(),
            ))
            .unwrap(),
        )
        .unwrap();
        let error = wait_for_resume_failure(Some(first.path.clone())).await;
        assert!(format!("{error:#}").contains("package validation failed"));
        assert!(require_pending_resume(&first.path).is_err());
        require_pending_resume(&second.path).unwrap();
        let old_path = first.path.clone();
        drop(first);
        assert!(!old_path.exists());
        assert!(require_pending_resume(&old_path).is_err());
    }

    #[tokio::test]
    async fn cancelled_or_corrupt_notification_refuses_thread_resume() {
        let directory = tempfile::tempdir().unwrap();
        let feedback = ResumeFeedback::new(directory.path()).unwrap();
        cancel_resume_feedback(Some(&feedback.path)).unwrap();
        assert!(require_pending_resume(&feedback.path).is_err());
        assert!(
            wait_for_resume_failure(Some(feedback.path.clone()))
                .await
                .to_string()
                .contains("取消")
        );
        std::fs::write(&feedback.path, b"corrupt").unwrap();
        assert!(require_pending_resume(&feedback.path).is_err());
        assert!(
            wait_for_resume_failure(Some(feedback.path.clone()))
                .await
                .to_string()
                .contains("无效")
        );
    }

    #[test]
    fn journal_retains_failed_cleanup_and_recovers_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let journal = LaunchJournal::acquire(directory.path()).unwrap();
        journal.arm(PACKAGE).unwrap();
        assert_eq!(
            serde_json::from_slice::<String>(&std::fs::read(&journal.path).unwrap()).unwrap(),
            PACKAGE
        );
        let failure = journal.recover(|package| {
            assert_eq!(package, PACKAGE);
            anyhow::bail!("simulated DisableDebugging failure")
        });
        assert!(failure.is_err());
        assert!(journal.path.exists());
        drop(journal); // A process exit releases the lock without removing the pending record.
        let restarted = LaunchJournal::acquire(directory.path()).unwrap();
        let mut calls = 0;
        restarted
            .recover(|package| {
                assert_eq!(package, PACKAGE);
                calls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert!(!restarted.path.exists());
        restarted.recover(|_| panic!("already cleared")).unwrap();
    }

    #[test]
    fn journal_serializes_live_launches_and_releases_lock_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let first = LaunchJournal::acquire(directory.path()).unwrap();
        assert!(LaunchJournal::acquire(directory.path()).is_err());
        drop(first);
        assert!(LaunchJournal::acquire(directory.path()).is_ok());
    }

    #[test]
    fn empty_journal_does_not_change_debug_settings() {
        let directory = tempfile::tempdir().unwrap();
        let journal = LaunchJournal::acquire(directory.path()).unwrap();
        journal.recover(|_| panic!("no pending package")).unwrap();
        assert!(!journal.path.exists());
    }

    #[test]
    fn journal_rejects_invalid_or_unrelated_records_without_disabling() {
        let directory = tempfile::tempdir().unwrap();
        let journal = LaunchJournal::acquire(directory.path()).unwrap();
        for package in [
            "Other.App_1.2.3.4_x64__publisher".to_string(),
            format!("../{PACKAGE}"),
            format!("C:\\{PACKAGE}"),
            format!("{PACKAGE}\0"),
        ] {
            assert!(journal.arm(&package).is_err());
            assert!(!journal.path.exists());
            std::fs::write(&journal.path, serde_json::to_vec(&package).unwrap()).unwrap();
            assert!(journal.recover(|_| panic!("untrusted package")).is_err());
            assert!(journal.path.exists());
            std::fs::remove_file(&journal.path).unwrap();
        }
        std::fs::write(&journal.path, b"{corrupt").unwrap();
        assert!(journal.recover(|_| panic!("corrupt record")).is_err());
        assert!(journal.path.exists());
    }

    #[test]
    fn environment_block_sorts_entries_and_preserves_unicode_and_empty_values() {
        let environment = vec![
            ("z".into(), "中文 路径".into()),
            ("Alpha".into(), "x=y".into()),
            ("EMPTY".into(), String::new()),
        ];
        let block = windows_environment_block(&environment).unwrap();
        assert_eq!(
            String::from_utf16(&block).unwrap(),
            "Alpha=x=y\0EMPTY=\0z=中文 路径\0\0"
        );
        assert_eq!(windows_environment_block(&[]).unwrap(), vec![0, 0]);
    }

    #[test]
    fn environment_block_rejects_invalid_names_and_embedded_nuls() {
        for (name, value) in [
            ("", "value"),
            ("a=b", "value"),
            ("a\0", "value"),
            ("a", "v\0"),
        ] {
            assert!(windows_environment_block(&[(name.into(), value.into())]).is_err());
        }
    }

    #[test]
    fn missing_package_cleanup_accepts_confirmed_uninstall_or_same_family_update() {
        let current = PACKAGE.replace("26.924.2738.0", "26.925.2738.0");
        assert!(windows_package_is_unregistered(PACKAGE, &[current]));
        assert!(windows_package_is_unregistered(PACKAGE, &[]));
        assert!(!windows_package_is_unregistered("unrelated", &[]));
        assert!(!windows_package_is_unregistered(PACKAGE, &[PACKAGE.into()]));
        assert!(!windows_package_is_unregistered(
            PACKAGE,
            &["Other.App_1.2.3.4_x64__publisher".into()]
        ));
    }

    #[test]
    fn uninstalled_package_journal_does_not_block_next_launch() {
        let directory = tempfile::tempdir().unwrap();
        let journal = LaunchJournal::acquire(directory.path()).unwrap();
        journal.arm(PACKAGE).unwrap();
        journal
            .recover(|package| {
                anyhow::ensure!(
                    windows_package_is_unregistered(package, &[]),
                    "still registered"
                );
                Ok(())
            })
            .unwrap();
        assert!(!journal.path.exists());
        journal
            .arm(&PACKAGE.replace("26.924.2738.0", "26.925.2738.0"))
            .unwrap();
    }
}
