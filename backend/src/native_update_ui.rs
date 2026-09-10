use std::sync::Arc;

#[derive(Clone)]
pub struct NativeUpdateUi {
    platform: Result<Arc<platform::PlatformUi>, Arc<str>>,
}

impl NativeUpdateUi {
    pub fn start() -> Self {
        match platform::PlatformUi::start() {
            Ok(platform) => Self {
                platform: Ok(Arc::new(platform)),
            },
            Err(error) => {
                eprintln!("{error}");
                Self {
                    platform: Err(Arc::from(error)),
                }
            }
        }
    }

    pub fn show_status(&self, message: &str) -> Result<(), String> {
        match &self.platform {
            Ok(platform) => platform.show_status(message),
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn hide_status(&self) -> Result<(), String> {
        match &self.platform {
            Ok(platform) => platform.hide_status(),
            Err(_) => Ok(()),
        }
    }

    pub async fn confirm_update(
        &self,
        current_version: &str,
        latest_version: &str,
    ) -> Result<bool, String> {
        show_dialog(
            format!("发现 Codey v{latest_version} 更新"),
            format!(
                "当前版本为 v{current_version}。是否现在下载、校验并安装更新？安装时 Codey 会退出，并尝试启动新版。"
            ),
            DialogKind::Confirm,
        )
        .await
        .map(|result| result == DialogResult::Primary)
    }

    pub async fn show_update_failure(&self, error: &str) -> Result<(), String> {
        show_dialog(
            "Codey 更新失败".to_string(),
            format!("{error}\n\n你可以进入 Codex 后，从 Codey 设置中重试。"),
            DialogKind::Failure,
        )
        .await
        .map(|_| ())
    }

    pub fn shutdown(&self) {
        if let Ok(platform) = &self.platform {
            platform.shutdown();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogKind {
    Confirm,
    Failure,
    RestoreContext,
}

pub(crate) async fn confirm_context_recovery() -> Result<bool, String> {
    show_dialog(
        "Codey 上下文设置暂时无法使用".to_string(),
        "本机 Codex 模型缓存不完整，暂时无法应用自定义上下文预算。\n\n可以恢复所有模型的默认预算并重新启动，其他设置不受影响。原配置会自动备份。若要继续使用自定义预算，请先直接打开官方 Codex 刷新模型缓存，再返回 Codey 设置。".to_string(),
        DialogKind::RestoreContext,
    )
    .await
    .map(|result| result == DialogResult::Primary)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogResult {
    Primary,
    Secondary,
}

#[cfg(any(windows, target_os = "macos"))]
async fn show_dialog(
    title: String,
    description: String,
    kind: DialogKind,
) -> Result<DialogResult, String> {
    tokio::task::spawn_blocking(move || {
        let buttons = match kind {
            DialogKind::Confirm => {
                rfd::MessageButtons::OkCancelCustom("更新并重启".to_string(), "稍后".to_string())
            }
            DialogKind::Failure => rfd::MessageButtons::OkCustom("进入 Codex".to_string()),
            DialogKind::RestoreContext => rfd::MessageButtons::OkCancelCustom(
                "恢复默认预算并重试".to_string(),
                "退出".to_string(),
            ),
        };
        let result = rfd::MessageDialog::new()
            .set_title(title)
            .set_description(description)
            .set_level(match kind {
                DialogKind::Confirm => rfd::MessageLevel::Info,
                DialogKind::Failure => rfd::MessageLevel::Error,
                DialogKind::RestoreContext => rfd::MessageLevel::Warning,
            })
            .set_buttons(buttons)
            .show();
        match (kind, result) {
            (DialogKind::Confirm, rfd::MessageDialogResult::Custom(label))
                if label == "更新并重启" =>
            {
                DialogResult::Primary
            }
            (DialogKind::Confirm, _) => DialogResult::Secondary,
            (DialogKind::Failure, _) => DialogResult::Primary,
            (DialogKind::RestoreContext, rfd::MessageDialogResult::Custom(label))
                if label == "恢复默认预算并重试" =>
            {
                DialogResult::Primary
            }
            (DialogKind::RestoreContext, _) => DialogResult::Secondary,
        }
    })
    .await
    .map_err(|error| format!("原生更新对话框任务异常退出：{error}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn show_dialog(
    _title: String,
    _description: String,
    kind: DialogKind,
) -> Result<DialogResult, String> {
    Ok(match kind {
        DialogKind::Confirm | DialogKind::RestoreContext => DialogResult::Secondary,
        DialogKind::Failure => DialogResult::Primary,
    })
}

#[cfg(target_os = "macos")]
pub fn run_macos_application<F>(run: F) -> anyhow::Result<()>
where
    F: FnOnce(NativeUpdateUi) -> anyhow::Result<()> + Send + 'static,
{
    platform::run_application(run)
}

#[cfg(windows)]
mod platform {
    use std::sync::{
        Mutex,
        mpsc::{self, Receiver, Sender},
    };
    use std::thread::{self, JoinHandle};

    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
    use windows::Win32::System::SystemServices::{SS_CENTER, SS_CENTERIMAGE};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, GetSystemMetrics,
        HWND_TOPMOST, MSG, PM_NOREMOVE, PeekMessageW, PostQuitMessage, PostThreadMessageW,
        SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
        WM_APP, WM_SETFONT, WS_BORDER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    use windows::core::{PCWSTR, w};

    const WINDOW_WIDTH: i32 = 380;
    const WINDOW_HEIGHT: i32 = 104;

    enum UiCommand {
        Show(String),
        Hide,
        Shutdown,
    }

    struct UiThread {
        command_tx: Sender<UiCommand>,
        thread_id: u32,
        join: Mutex<Option<JoinHandle<()>>>,
    }

    pub struct PlatformUi {
        thread: UiThread,
    }

    impl PlatformUi {
        pub fn start() -> Result<Self, String> {
            let (command_tx, command_rx) = mpsc::channel();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let join = thread::Builder::new()
                .name("codey-native-update-ui".to_string())
                .spawn(move || ui_thread(command_rx, ready_tx))
                .map_err(|error| format!("启动 Windows 更新提示线程失败：{error}"))?;
            let ready = ready_rx
                .recv()
                .map_err(|_| "Windows 更新提示线程未能完成初始化".to_string())?;
            match ready {
                Ok(thread_id) => Ok(Self {
                    thread: UiThread {
                        command_tx,
                        thread_id,
                        join: Mutex::new(Some(join)),
                    },
                }),
                Err(error) => {
                    let _ = join.join();
                    Err(error)
                }
            }
        }

        pub fn show_status(&self, message: &str) -> Result<(), String> {
            self.send(UiCommand::Show(message.to_string()))
        }

        pub fn hide_status(&self) -> Result<(), String> {
            self.send(UiCommand::Hide)
        }

        pub fn shutdown(&self) {
            let _ = self.send(UiCommand::Shutdown);
            if let Ok(mut join) = self.thread.join.lock()
                && let Some(join) = join.take()
            {
                let _ = join.join();
            }
        }

        fn send(&self, command: UiCommand) -> Result<(), String> {
            self.thread
                .command_tx
                .send(command)
                .map_err(|_| "Windows 更新提示线程已经退出".to_string())?;
            unsafe {
                PostThreadMessageW(self.thread.thread_id, WM_APP, WPARAM(0), LPARAM(0))
                    .map_err(|error| format!("唤醒 Windows 更新提示线程失败：{error}"))
            }
        }
    }

    fn ui_thread(command_rx: Receiver<UiCommand>, ready_tx: mpsc::SyncSender<Result<u32, String>>) {
        let mut message = MSG::default();
        unsafe {
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
        }
        let thread_id = unsafe { GetCurrentThreadId() };
        let window = match create_status_window() {
            Ok(window) => window,
            Err(error) => {
                let _ = ready_tx.send(Err(error));
                return;
            }
        };
        if ready_tx.send(Ok(thread_id)).is_err() {
            unsafe {
                let _ = DestroyWindow(window);
            }
            return;
        }

        loop {
            let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if result.0 == 0 {
                break;
            }
            if result.0 == -1 {
                break;
            }
            if message.message == WM_APP {
                let mut should_quit = false;
                while let Ok(command) = command_rx.try_recv() {
                    match command {
                        UiCommand::Show(text) => show_window(window, &text),
                        UiCommand::Hide => unsafe {
                            let _ = ShowWindow(window, SW_HIDE);
                        },
                        UiCommand::Shutdown => {
                            should_quit = true;
                            break;
                        }
                    }
                }
                if should_quit {
                    unsafe {
                        let _ = DestroyWindow(window);
                        PostQuitMessage(0);
                    }
                }
                continue;
            }
            unsafe {
                DispatchMessageW(&message);
            }
        }
    }

    fn create_status_window() -> Result<HWND, String> {
        let style = WINDOW_STYLE(WS_POPUP.0 | WS_BORDER.0 | SS_CENTER.0 | SS_CENTERIMAGE.0);
        let ex_style = WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOPMOST.0);
        let window = unsafe {
            CreateWindowExW(
                ex_style,
                w!("STATIC"),
                w!(""),
                style,
                0,
                0,
                WINDOW_WIDTH,
                WINDOW_HEIGHT,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|error| format!("创建 Windows 更新提示窗失败：{error}"))?;
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        if !font.is_invalid() {
            unsafe {
                SendMessageW(window, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            }
        }
        Ok(window)
    }

    fn show_window(window: HWND, text: &str) {
        let text = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let screen_width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let screen_height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let x = (screen_width - WINDOW_WIDTH).max(0) / 2;
        let y = (screen_height - WINDOW_HEIGHT).max(0) / 2;
        unsafe {
            let _ = SetWindowTextW(window, PCWSTR(text.as_ptr()));
            let _ = SetWindowPos(
                window,
                HWND_TOPMOST,
                x,
                y,
                WINDOW_WIDTH,
                WINDOW_HEIGHT,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, mpsc};
    use std::thread;

    use dispatch2::MainThreadBound;
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSFloatingWindowLevel,
        NSPanel, NSTextField, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    use super::NativeUpdateUi;

    struct MacStatusWindow {
        panel: Retained<NSPanel>,
        label: Retained<NSTextField>,
    }

    struct MacUiState {
        app: Retained<NSApplication>,
        status: Result<MacStatusWindow, String>,
    }

    pub struct PlatformUi {
        state: Arc<MainThreadBound<RefCell<MacUiState>>>,
    }

    impl PlatformUi {
        pub fn start() -> Result<Self, String> {
            let mtm = MainThreadMarker::new()
                .ok_or_else(|| "macOS 更新界面必须在主线程初始化".to_string())?;
            let app = NSApplication::sharedApplication(mtm);

            let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(380.0, 112.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::Buffered,
                false,
            );
            unsafe {
                panel.setReleasedWhenClosed(false);
            }
            panel.setTitle(&NSString::from_str("Codey"));
            panel.setFloatingPanel(true);
            panel.setHidesOnDeactivate(false);
            panel.setLevel(NSFloatingWindowLevel);

            let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(24.0, 22.0),
                NSSize::new(332.0, 48.0),
            ));
            let status = match panel.contentView() {
                Some(content) => {
                    content.addSubview(&label);
                    Ok(MacStatusWindow { panel, label })
                }
                None => Err("macOS 更新提示窗缺少内容视图".to_string()),
            };

            Ok(Self {
                state: Arc::new(MainThreadBound::new(
                    RefCell::new(MacUiState { app, status }),
                    mtm,
                )),
            })
        }

        pub fn show_status(&self, message: &str) -> Result<(), String> {
            let message = message.to_string();
            self.state.get_on_main(|state| -> Result<(), String> {
                let state = state.borrow();
                let status = state.status.as_ref().map_err(Clone::clone)?;
                status.label.setStringValue(&NSString::from_str(&message));
                status.panel.center();
                status.panel.orderFrontRegardless();
                Ok(())
            })
        }

        pub fn hide_status(&self) -> Result<(), String> {
            self.state.get_on_main(|state| {
                if let Ok(status) = &state.borrow().status {
                    status.panel.orderOut(None);
                }
            });
            Ok(())
        }

        pub fn shutdown(&self) {
            self.state.get_on_main(|state| {
                let state = state.borrow();
                if let Ok(status) = &state.status {
                    status.panel.orderOut(None);
                }
                state.app.stop(None);
            });
        }
    }

    pub fn run_application<F>(run: F) -> anyhow::Result<()>
    where
        F: FnOnce(NativeUpdateUi) -> anyhow::Result<()> + Send + 'static,
    {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("macOS 应用循环必须在主线程运行"))?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        let ui = NativeUpdateUi::start();
        let worker_ui = ui.clone();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("codey-runtime".to_string())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| run(worker_ui.clone())))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Codey 运行线程异常退出")));
                worker_ui.shutdown();
                let _ = result_tx.send(result);
            })?;

        app.run();
        let result = result_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Codey 运行线程未返回结果"))?;
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Codey 运行线程回收失败"))?;
        drop(ui);
        result
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub struct PlatformUi;

    impl PlatformUi {
        pub fn start() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn show_status(&self, _message: &str) -> Result<(), String> {
            Ok(())
        }

        pub fn hide_status(&self) -> Result<(), String> {
            Ok(())
        }

        pub fn shutdown(&self) {}
    }
}
