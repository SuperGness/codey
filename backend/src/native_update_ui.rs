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
        release_notes: Option<&str>,
    ) -> Result<bool, String> {
        let rollback = semver::Version::parse(latest_version)
            .ok()
            .zip(semver::Version::parse(current_version).ok())
            .is_some_and(|(target, current)| target < current);
        let pages = release_notes.map(update_note_pages).unwrap_or_default();
        let notes = update_note_preview(&pages);
        let title = if rollback {
            format!("回退 Codey 至 v{latest_version}")
        } else {
            format!("发现 Codey v{latest_version} 更新")
        };
        let description = if rollback {
            format!(
                "管理员已授权将 v{current_version} 降级至 v{latest_version}。\n请保存工作；确认后下载安装旧版本并重启，安装前将再次校验授权。{notes}"
            )
        } else {
            format!(
                "当前版本为 v{current_version}。\n是否现在下载、校验并安装更新？安装时会退出 Codex 和 Codey，并尝试启动新版。{notes}"
            )
        };
        loop {
            match show_dialog_with_extra(
                title.clone(),
                description.clone(),
                DialogKind::Confirm,
                if rollback {
                    "回退并重启"
                } else {
                    "更新并重启"
                }
                .to_string(),
                Some("稍后".to_string()),
                (!pages.is_empty()).then(|| "查看完整日志".to_string()),
            )
            .await?
            {
                DialogResult::Primary => return Ok(true),
                DialogResult::Secondary => return Ok(false),
                DialogResult::Extra => show_update_note_pages(latest_version, &pages).await?,
            }
        }
    }

    pub async fn show_update_failure(&self, error: &str) -> Result<(), String> {
        show_dialog(
            "Codey 更新失败".to_string(),
            format!("{error}\n\n你可以进入 Codex 后，从 Codey 设置中重试。"),
            DialogKind::Failure,
            "进入 Codex".to_string(),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn show_startup_failure(&self, error: &str) -> Result<(), String> {
        show_dialog(
            "Codey 启动失败".to_string(),
            format!("{error}\n\nCodey 将退出。处理上述问题后，请重新启动 Codey。"),
            DialogKind::Failure,
            "退出".to_string(),
            None,
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

/// 上下文恢复提示出现的时机：启动或重启 Codex 失败，或保存模型时无法应用自定义预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextRecoveryPurpose {
    /// 启动或重启 Codex 时无法生成带自定义预算的模型目录。
    Launch,
    /// 保存模型时无法生成带自定义预算的模型目录。
    ModelSync,
}

impl ContextRecoveryPurpose {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::ModelSync => "model_sync",
        }
    }
}

pub(crate) async fn confirm_context_recovery(
    purpose: ContextRecoveryPurpose,
) -> Result<bool, String> {
    confirm_context_recovery_with_reason(
        purpose,
        crate::model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE,
    )
    .await
}

fn context_recovery_description(purpose: ContextRecoveryPurpose, reason: &str) -> String {
    let action = match purpose {
        ContextRecoveryPurpose::Launch => "重新启动",
        ContextRecoveryPurpose::ModelSync => "继续保存本次改动",
    };
    format!(
        "{reason}\n\n可以恢复所有模型的默认上下文预算并{action}，其他设置不受影响。原配置会自动备份，自定义模型目录保持不变。若要保留预算，请先检查模型目录与线路中的模型标识，或重新同步模型后重试。"
    )
}

#[cfg(test)]
#[test]
fn context_recovery_prompt_preserves_the_catalog_error_and_reset_scope() {
    let reason = "自定义模型目录缺少已启用的预算模型：custom/gpt-6-astra（custom.json）";
    for purpose in [
        ContextRecoveryPurpose::Launch,
        ContextRecoveryPurpose::ModelSync,
    ] {
        let description = context_recovery_description(purpose, reason);
        assert!(description.starts_with(reason));
        assert!(description.contains("所有模型"));
        assert!(description.contains("自动备份"));
        assert!(description.contains("自定义模型目录保持不变"));
        let action = match purpose {
            ContextRecoveryPurpose::Launch => "重新启动",
            ContextRecoveryPurpose::ModelSync => "继续保存本次改动",
        };
        assert!(description.contains(action));
    }
}

pub(crate) async fn confirm_context_recovery_with_reason(
    purpose: ContextRecoveryPurpose,
    reason: &str,
) -> Result<bool, String> {
    let (primary_label, secondary_label) = match purpose {
        ContextRecoveryPurpose::Launch => ("恢复默认预算并重试", "退出"),
        ContextRecoveryPurpose::ModelSync => ("恢复默认预算并继续", "取消"),
    };
    show_dialog(
        "Codey 上下文设置暂时无法使用".to_string(),
        context_recovery_description(purpose, reason),
        DialogKind::RestoreContext,
        primary_label.to_string(),
        Some(secondary_label.to_string()),
    )
    .await
    .map(|result| result == DialogResult::Primary)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogResult {
    Primary,
    Secondary,
    Extra,
}

// 原生消息框没有滚动区，预览和详情按显示行分页；原始日志与生成规则不变。
fn update_note_pages(notes: &str) -> Vec<String> {
    const LINE_WIDTH: usize = 64;
    const PAGE_LINES: usize = 10;
    let mut lines = Vec::new();
    for line in notes.trim().lines() {
        let line = line
            .trim()
            .strip_prefix("**")
            .and_then(|title| title.strip_suffix("**"))
            .filter(|title| matches!(*title, "新增功能" | "体验优化" | "问题修复"))
            .unwrap_or(line);
        let mut wrapped = String::new();
        let mut width = 0;
        for ch in line.chars() {
            let char_width = if ch.is_ascii() { 1 } else { 2 };
            if width + char_width > LINE_WIDTH {
                lines.push(std::mem::take(&mut wrapped));
                width = 0;
            }
            wrapped.push(ch);
            width += char_width;
        }
        lines.push(wrapped);
    }
    lines
        .chunks(PAGE_LINES)
        .map(|page| page.join("\n"))
        .collect()
}

fn update_note_preview(pages: &[String]) -> String {
    let Some(first) = pages.first() else {
        return String::new();
    };
    let preview = first.lines().take(6).collect::<Vec<_>>().join("\n");
    let more = if pages.len() > 1 || first.lines().count() > 6 {
        "\n…\n其余内容可通过查看完整日志阅读。"
    } else {
        ""
    };
    format!("\n\n更新说明：\n{preview}{more}")
}

#[cfg(test)]
mod update_note_tests {
    use super::*;

    #[test]
    fn long_notes_have_a_short_preview_and_complete_bounded_pages() {
        let notes = (0..40)
            .map(|index| format!("- 第{index}项：{}🌟", "必要的更新说明".repeat(20)))
            .collect::<Vec<_>>()
            .join("\r\n");
        let pages = update_note_pages(&notes);
        assert!(pages.len() > 1);
        for page in &pages {
            assert!(page.lines().count() <= 10);
            for line in page.lines() {
                let width: usize = line
                    .chars()
                    .map(|ch| if ch.is_ascii() { 1 } else { 2 })
                    .sum();
                assert!(width <= 64);
            }
        }
        let rendered: String = pages.concat().chars().filter(|ch| *ch != '\n').collect();
        let original: String = notes
            .chars()
            .filter(|ch| !matches!(ch, '\r' | '\n'))
            .collect();
        assert_eq!(rendered, original);
        let preview = update_note_preview(&pages);
        assert!(preview.chars().count() < 300);
        assert!(preview.contains("其余内容"));
        assert!(!preview.contains("第39项"));
        assert!(pages.last().unwrap().contains("🌟"));
    }

    #[test]
    fn empty_and_short_notes_do_not_gain_an_omission_message() {
        assert!(update_note_pages(" \r\n \t").is_empty());
        assert!(update_note_preview(&[]).is_empty());
        let pages = update_note_pages("- 修复启动问题\r\n- 优化更新提示");
        assert_eq!(pages, ["- 修复启动问题\n- 优化更新提示"]);
        let preview = update_note_preview(&pages);
        assert!(preview.contains("修复启动问题\n- 优化更新提示"));
        assert!(!preview.contains('…'));
    }

    #[test]
    fn category_headings_are_readable_in_plain_text_dialogs() {
        let pages = update_note_pages(
            "**新增功能**\n- 支持账号切换\n\n**体验优化**\n- 简化更新提示\n\n**问题修复**\n- 修复启动失败",
        );
        let text = pages.join("\n");
        assert!(!text.contains("**"));
        for expected in [
            "新增功能",
            "体验优化",
            "问题修复",
            "支持账号切换",
            "简化更新提示",
            "修复启动失败",
        ] {
            assert!(text.contains(expected));
        }
        assert_eq!(update_note_pages("**保留其他文字**"), ["**保留其他文字**"]);
    }

    #[test]
    fn ascii_words_and_rollback_reason_are_retained_across_pages() {
        let notes = format!(
            "回退原因：兼容性问题\n{}\n最后一条更新",
            "LongReleaseIdentifier".repeat(80)
        );
        let pages = update_note_pages(&notes);
        assert!(update_note_preview(&pages).contains("回退原因：兼容性问题"));
        let rendered = pages.join("\n").replace('\n', "");
        assert_eq!(rendered, notes.replace('\n', ""));
        assert!(pages.last().unwrap().contains("最后一条更新"));
    }

    #[test]
    fn viewing_notes_and_dismissing_never_accept_an_update() {
        for (label, expected) in [
            (Some("查看完整日志"), DialogResult::Extra),
            (Some("稍后"), DialogResult::Secondary),
            (None, DialogResult::Secondary),
            (Some("更新并重启"), DialogResult::Primary),
        ] {
            assert_eq!(
                dialog_result_for_label(
                    DialogKind::Confirm,
                    label,
                    "更新并重启",
                    Some("查看完整日志")
                ),
                expected
            );
        }
        assert_eq!(
            dialog_result_for_label(DialogKind::RestoreContext, None, "恢复", None),
            DialogResult::Secondary
        );
        assert_eq!(
            dialog_result_for_label(DialogKind::Failure, None, "退出", None),
            DialogResult::Primary
        );
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[tokio::test]
    async fn headless_dialog_never_confirms_updates_or_context_recovery() {
        for (kind, expected) in [
            (DialogKind::Confirm, DialogResult::Secondary),
            (DialogKind::RestoreContext, DialogResult::Secondary),
            (DialogKind::Failure, DialogResult::Primary),
        ] {
            for extra_label in [None, Some("查看完整日志".to_string())] {
                let result = show_dialog_with_extra(
                    "Codey".to_string(),
                    "需要用户确认".to_string(),
                    kind,
                    "继续".to_string(),
                    Some("取消".to_string()),
                    extra_label,
                )
                .await
                .unwrap();
                assert_eq!(result, expected);
            }
        }
    }
}

async fn show_update_note_pages(version: &str, pages: &[String]) -> Result<(), String> {
    let mut index = 0;
    while let Some(page) = pages.get(index) {
        let last = index + 1 == pages.len();
        match show_dialog_with_extra(
            format!("Codey v{version} 更新日志（{}/{}）", index + 1, pages.len()),
            page.clone(),
            DialogKind::Confirm,
            if last {
                "返回更新提示"
            } else {
                "下一页"
            }
            .to_string(),
            Some("关闭日志".to_string()),
            (index > 0).then(|| "上一页".to_string()),
        )
        .await?
        {
            DialogResult::Primary if !last => index += 1,
            DialogResult::Extra if index > 0 => index -= 1,
            _ => break,
        }
    }
    Ok(())
}

async fn show_dialog(
    title: String,
    description: String,
    kind: DialogKind,
    primary_label: String,
    secondary_label: Option<String>,
) -> Result<DialogResult, String> {
    show_dialog_with_extra(
        title,
        description,
        kind,
        primary_label,
        secondary_label,
        None,
    )
    .await
}

fn dialog_result_for_label(
    kind: DialogKind,
    label: Option<&str>,
    primary_label: &str,
    extra_label: Option<&str>,
) -> DialogResult {
    if kind == DialogKind::Failure || label == Some(primary_label) {
        DialogResult::Primary
    } else if label.is_some() && label == extra_label {
        DialogResult::Extra
    } else {
        DialogResult::Secondary
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn show_dialog_with_extra(
    title: String,
    description: String,
    kind: DialogKind,
    primary_label: String,
    secondary_label: Option<String>,
    extra_label: Option<String>,
) -> Result<DialogResult, String> {
    tokio::task::spawn_blocking(move || {
        let primary_label_for_result = primary_label.clone();
        let extra_label_for_result = extra_label.clone();
        let buttons = match secondary_label {
            Some(secondary_label) if extra_label.is_some() => {
                rfd::MessageButtons::YesNoCancelCustom(
                    primary_label,
                    extra_label.unwrap(),
                    secondary_label,
                )
            }
            Some(secondary_label) => {
                rfd::MessageButtons::OkCancelCustom(primary_label, secondary_label)
            }
            None => rfd::MessageButtons::OkCustom(primary_label),
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
        let label = match &result {
            rfd::MessageDialogResult::Custom(label) => Some(label.as_str()),
            _ => None,
        };
        dialog_result_for_label(
            kind,
            label,
            &primary_label_for_result,
            extra_label_for_result.as_deref(),
        )
    })
    .await
    .map_err(|error| format!("原生更新对话框任务异常退出：{error}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn show_dialog_with_extra(
    _title: String,
    _description: String,
    kind: DialogKind,
    primary_label: String,
    _secondary_label: Option<String>,
    extra_label: Option<String>,
) -> Result<DialogResult, String> {
    Ok(dialog_result_for_label(
        kind,
        None,
        &primary_label,
        extra_label.as_deref(),
    ))
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
