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
        let prompt = if rollback {
            UpdatePrompt {
                title: format!("回退 Codey 至 v{latest_version}"),
                description: format!(
                    "管理员已授权将 v{current_version} 降级至 v{latest_version}。请保存工作；确认后下载安装旧版本并重启，安装前将再次校验授权。"
                ),
                notes: release_notes.map(parse_update_notes).unwrap_or_default(),
                primary_label: "回退并重启".to_string(),
            }
        } else {
            UpdatePrompt {
                title: format!("发现 Codey v{latest_version} 更新"),
                description: format!(
                    "当前版本为 v{current_version}。更新时会退出 Codex 和 Codey，安装完成后尝试启动新版。"
                ),
                notes: release_notes.map(parse_update_notes).unwrap_or_default(),
                primary_label: "更新并重启".to_string(),
            }
        };
        show_update_prompt(prompt).await
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
    #[cfg_attr(target_os = "macos", allow(dead_code))]
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
}

struct UpdatePrompt {
    title: String,
    description: String,
    notes: Vec<NoteLine>,
    primary_label: String,
}

const UPDATE_LATER_LABEL: &str = "稍后";

/// 更新说明按行归类：版本等标题、分类标题与条目，供原生弹窗分别排版。
#[derive(Clone, Debug, PartialEq, Eq)]
enum NoteLine {
    Heading(String),
    Category(String),
    Item(String),
}

const NOTE_CATEGORIES: [&str; 3] = ["新增功能", "体验优化", "问题修复"];

fn note_category(text: &str) -> Option<&str> {
    let text = text
        .strip_prefix("**")
        .and_then(|inner| inner.strip_suffix("**"))
        .unwrap_or(text);
    NOTE_CATEGORIES.contains(&text).then_some(text)
}

fn parse_update_notes(notes: &str) -> Vec<NoteLine> {
    notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            if let Some(title) = plain_atx_heading(line) {
                return match note_category(title) {
                    Some(category) => Some(NoteLine::Category(category.to_string())),
                    None if title.is_empty() => None,
                    None => Some(NoteLine::Heading(title.to_string())),
                };
            }
            if let Some(category) = note_category(line) {
                return Some(NoteLine::Category(category.to_string()));
            }
            let item = ["-", "*", "•"]
                .iter()
                .find_map(|marker| {
                    line.strip_prefix(marker)
                        .filter(|rest| rest.starts_with([' ', '\t']))
                })
                .map_or(line, str::trim_start);
            Some(NoteLine::Item(item.to_string()))
        })
        .collect()
}

/// 版本标题统一带 v 前缀，非版本号标题保持原样。
fn version_label(heading: &str) -> String {
    if heading.starts_with(|ch: char| ch.is_ascii_digit()) {
        format!("v{heading}")
    } else {
        heading.to_string()
    }
}

/// 无法滚动的消息框只展示最新一节，较早版本汇总为一句，避免弹窗超出屏幕。
#[cfg(any(not(target_os = "macos"), test))]
fn compact_update_notes(notes: &[NoteLine]) -> String {
    const MAX_ITEMS: usize = 12;
    let first_len = notes
        .iter()
        .skip(1)
        .position(|line| matches!(line, NoteLine::Heading(_)))
        .map_or(notes.len(), |index| index + 1);
    let (first, older) = notes.split_at(first_len);
    let mut lines = first.iter().peekable();
    let mut rendered = match lines.next_if(|line| matches!(line, NoteLine::Heading(_))) {
        Some(NoteLine::Heading(heading)) => format!("\n\n更新说明（{}）：", version_label(heading)),
        _ => "\n\n更新说明：".to_string(),
    };
    let mut shown = 0;
    let mut hidden = 0;
    let mut pending_category = None;
    for line in lines {
        match line {
            NoteLine::Category(category) => pending_category = Some(category),
            NoteLine::Item(text) if shown < MAX_ITEMS => {
                if let Some(category) = pending_category.take() {
                    rendered.push_str(&format!("\n{category}"));
                }
                rendered.push_str(&format!("\n- {text}"));
                shown += 1;
            }
            _ => hidden += 1,
        }
    }
    if shown == 0 && older.is_empty() {
        return String::new();
    }
    if hidden > 0 {
        rendered.push_str(&format!("\n…另有 {hidden} 项"));
    }
    let older_versions: Vec<String> = older
        .iter()
        .filter_map(|line| match line {
            NoteLine::Heading(heading) => Some(version_label(heading)),
            _ => None,
        })
        .collect();
    if !older_versions.is_empty() {
        let listed = &older_versions[..older_versions.len().min(3)];
        let more = if older_versions.len() > listed.len() {
            " 等"
        } else {
            ""
        };
        rendered.push_str(&format!(
            "\n\n另含 {} 个较早版本（{}{more}）的更新，完整说明可在 Codey 设置中检查更新查看。",
            older_versions.len(),
            listed.join("、"),
        ));
    }
    rendered
}

#[cfg(target_os = "macos")]
async fn show_update_prompt(prompt: UpdatePrompt) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || platform::confirm_update(prompt))
        .await
        .map_err(|error| format!("原生更新对话框任务异常退出：{error}"))
}

#[cfg(not(target_os = "macos"))]
async fn show_update_prompt(prompt: UpdatePrompt) -> Result<bool, String> {
    let notes = compact_update_notes(&prompt.notes);
    show_dialog(
        prompt.title,
        format!("{}{notes}", prompt.description),
        DialogKind::Confirm,
        prompt.primary_label,
        Some(UPDATE_LATER_LABEL.to_string()),
    )
    .await
    .map(|result| result == DialogResult::Primary)
}

fn plain_atx_heading(line: &str) -> Option<&str> {
    let level = line.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let title = rest.trim_matches([' ', '\t']);
    let without_closing_hashes = title.trim_end_matches('#');
    if without_closing_hashes.is_empty() || without_closing_hashes.ends_with([' ', '\t']) {
        Some(without_closing_hashes.trim_end_matches([' ', '\t']))
    } else {
        Some(title)
    }
}

#[cfg(test)]
mod update_note_tests {
    use super::*;

    use NoteLine::{Category, Heading, Item};

    fn heading(text: &str) -> NoteLine {
        Heading(text.to_string())
    }
    fn category(text: &str) -> NoteLine {
        Category(text.to_string())
    }
    fn item(text: &str) -> NoteLine {
        Item(text.to_string())
    }

    #[test]
    fn notes_are_classified_into_headings_categories_and_items() {
        assert_eq!(
            parse_update_notes(
                "## 1.5.1\r\n\r\n**新增功能**\n- 支持导入\n\n### 体验优化\n* 更快\n• 更稳\n问题修复\n修复启动\n- \t缩进条目"
            ),
            vec![
                heading("1.5.1"),
                category("新增功能"),
                item("支持导入"),
                category("体验优化"),
                item("更快"),
                item("更稳"),
                category("问题修复"),
                item("修复启动"),
                item("缩进条目"),
            ]
        );
        assert!(parse_update_notes(" \r\n \t").is_empty());
        assert_eq!(
            parse_update_notes("**保留其他文字**"),
            vec![item("**保留其他文字**")]
        );
    }

    #[test]
    fn atx_headings_preserve_the_full_title_and_plain_hashes() {
        for level in 1..=6 {
            for separator in [" ", "\t"] {
                let marker = "#".repeat(level);
                assert_eq!(
                    parse_update_notes(&format!("{marker}{separator}完整标题 C# 和 #标签 ##\t")),
                    vec![heading("完整标题 C# 和 #标签")]
                );
            }
        }
        assert_eq!(
            parse_update_notes("## **新增功能**\n######\n## ###"),
            vec![category("新增功能")]
        );
        assert_eq!(
            parse_update_notes(
                "#tag\n##没有分隔空白\n####### 超过六级\n正文 # 井号\n## 标题###\n## C#\n## 标题 #标签"
            ),
            vec![
                item("#tag"),
                item("##没有分隔空白"),
                item("####### 超过六级"),
                item("正文 # 井号"),
                heading("标题###"),
                heading("C#"),
                heading("标题 #标签"),
            ]
        );
    }

    #[test]
    fn compact_notes_show_the_latest_version_and_summarize_older_ones() {
        let notes = parse_update_notes(
            "## 1.5.1\n\n### 新增功能\n- 支持导入\n\n### 问题修复\n- 修复启动\n\n## 1.5.0\n- 旧条目\n\n## 1.3.10\n- 旧条目\n\n## 1.3.9\n- 旧条目\n\n## 1.3.8\n- 旧条目",
        );
        assert_eq!(
            compact_update_notes(&notes),
            "\n\n更新说明（v1.5.1）：\n新增功能\n- 支持导入\n问题修复\n- 修复启动\n\n另含 4 个较早版本（v1.5.0、v1.3.10、v1.3.9 等）的更新，完整说明可在 Codey 设置中检查更新查看。"
        );
        let single = parse_update_notes("## 1.5.1\n- 新条目\n## 1.5.0\n- 旧条目");
        assert!(compact_update_notes(&single).ends_with(
            "另含 1 个较早版本（v1.5.0）的更新，完整说明可在 Codey 设置中检查更新查看。"
        ));
    }

    #[test]
    fn compact_notes_cap_long_sections_and_skip_empty_notes() {
        let notes = (0..40)
            .map(|index| format!("- 第{index}项🌟"))
            .collect::<Vec<_>>()
            .join("\r\n");
        let rendered = compact_update_notes(&parse_update_notes(&format!("**体验优化**\n{notes}")));
        assert!(rendered.starts_with("\n\n更新说明：\n体验优化\n- 第0项🌟"));
        assert!(rendered.contains("第11项🌟"));
        assert!(!rendered.contains("第12项"));
        assert!(rendered.ends_with("\n…另有 28 项"));
        assert!(compact_update_notes(&[]).is_empty());
        assert!(compact_update_notes(&parse_update_notes("**新增功能**")).is_empty());
        assert_eq!(
            compact_update_notes(&parse_update_notes("- 修复启动问题\n- 优化更新提示")),
            "\n\n更新说明：\n- 修复启动问题\n- 优化更新提示"
        );
    }

    #[test]
    fn dismissing_never_accepts_an_update() {
        for (label, expected) in [
            (Some("稍后"), DialogResult::Secondary),
            (None, DialogResult::Secondary),
            (Some("更新并重启"), DialogResult::Primary),
        ] {
            assert_eq!(
                dialog_result_for_label(DialogKind::Confirm, label, "更新并重启"),
                expected
            );
        }
        assert_eq!(
            dialog_result_for_label(DialogKind::RestoreContext, None, "恢复"),
            DialogResult::Secondary
        );
        assert_eq!(
            dialog_result_for_label(DialogKind::Failure, None, "退出"),
            DialogResult::Primary
        );
    }
}

fn dialog_result_for_label(
    kind: DialogKind,
    label: Option<&str>,
    primary_label: &str,
) -> DialogResult {
    if kind == DialogKind::Failure || label == Some(primary_label) {
        DialogResult::Primary
    } else {
        DialogResult::Secondary
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn show_dialog(
    title: String,
    description: String,
    kind: DialogKind,
    primary_label: String,
    secondary_label: Option<String>,
) -> Result<DialogResult, String> {
    tokio::task::spawn_blocking(move || {
        let primary_label_for_result = primary_label.clone();
        let buttons = match secondary_label {
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
        dialog_result_for_label(kind, label, &primary_label_for_result)
    })
    .await
    .map_err(|error| format!("原生更新对话框任务异常退出：{error}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn show_dialog(
    _title: String,
    _description: String,
    kind: DialogKind,
    primary_label: String,
    _secondary_label: Option<String>,
) -> Result<DialogResult, String> {
    Ok(dialog_result_for_label(kind, None, &primary_label))
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
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication,
        NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType, NSBorderType,
        NSColor, NSFloatingWindowLevel, NSFont, NSFontAttributeName,
        NSForegroundColorAttributeName, NSMutableParagraphStyle, NSPanel,
        NSParagraphStyleAttributeName, NSScrollView, NSTextField, NSTextView, NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary,
        NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString,
    };

    use super::{NativeUpdateUi, NoteLine, UPDATE_LATER_LABEL, UpdatePrompt, version_label};

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

    const NOTES_WIDTH: f64 = 420.0;
    const NOTES_MAX_HEIGHT: f64 = 260.0;
    const NOTES_INSET: f64 = 10.0;
    const ITEM_INDENT: f64 = 14.0;

    /// 更新确认使用带滚动区的 NSAlert：说明再长，按钮也始终留在屏幕内。
    pub(super) fn confirm_update(prompt: UpdatePrompt) -> bool {
        dispatch2::run_on_main(move |mtm| {
            autoreleasepool(|_| {
                let app = NSApplication::sharedApplication(mtm);
                let alert = NSAlert::new(mtm);
                alert.setAlertStyle(NSAlertStyle::Informational);
                alert.setMessageText(&NSString::from_str(&prompt.title));
                alert.setInformativeText(&NSString::from_str(&prompt.description));
                alert.addButtonWithTitle(&NSString::from_str(&prompt.primary_label));
                let later = alert.addButtonWithTitle(&NSString::from_str(UPDATE_LATER_LABEL));
                later.setKeyEquivalent(&NSString::from_str("\u{1b}"));
                let notes = (!prompt.notes.is_empty()).then(|| notes_view(&prompt.notes, mtm));
                if let Some((scroll, _)) = &notes {
                    alert.setAccessoryView(Some(scroll));
                }
                alert.layout();
                // 布局会调整文本视图尺寸并改变滚动位置，布局后再回到开头，先展示最新版本。
                if let Some((_, text_view)) = &notes {
                    text_view.setSelectedRange(NSRange::new(0, 0));
                    text_view.scrollPoint(NSPoint::new(0.0, 0.0));
                }
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
                alert.runModal() == NSAlertFirstButtonReturn
            })
        })
    }

    fn notes_view(
        notes: &[NoteLine],
        mtm: MainThreadMarker,
    ) -> (Retained<NSScrollView>, Retained<NSTextView>) {
        let content = NSMutableAttributedString::new();
        let mut previous: Option<&NoteLine> = None;
        for line in notes {
            let (text, font, color, before, indent) = match line {
                NoteLine::Heading(heading) => (
                    version_label(heading),
                    NSFont::boldSystemFontOfSize(13.0),
                    NSColor::labelColor(),
                    if previous.is_some() { 14.0 } else { 0.0 },
                    false,
                ),
                NoteLine::Category(category) => (
                    category.clone(),
                    NSFont::boldSystemFontOfSize(11.0),
                    NSColor::secondaryLabelColor(),
                    if matches!(previous, None | Some(NoteLine::Heading(_))) {
                        4.0
                    } else {
                        8.0
                    },
                    false,
                ),
                NoteLine::Item(item) => (
                    format!("•\t{item}"),
                    NSFont::systemFontOfSize(12.0),
                    NSColor::labelColor(),
                    if matches!(previous, Some(NoteLine::Heading(_))) {
                        4.0
                    } else {
                        2.0
                    },
                    true,
                ),
            };
            let paragraph = NSMutableParagraphStyle::new();
            paragraph.setParagraphSpacingBefore(before);
            paragraph.setLineSpacing(2.0);
            if indent {
                paragraph.setTabStops(Some(&NSArray::new()));
                paragraph.setDefaultTabInterval(ITEM_INDENT);
                paragraph.setHeadIndent(ITEM_INDENT);
            }
            let separator = if previous.is_some() { "\n" } else { "" };
            let attributes = unsafe {
                NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(
                    &[
                        NSFontAttributeName,
                        NSForegroundColorAttributeName,
                        NSParagraphStyleAttributeName,
                    ],
                    &[&*font, &*color, &*paragraph],
                )
            };
            let piece = unsafe {
                NSAttributedString::initWithString_attributes(
                    NSAttributedString::alloc(),
                    &NSString::from_str(&format!("{separator}{text}")),
                    Some(&attributes),
                )
            };
            content.appendAttributedString(&piece);
            previous = Some(line);
        }

        let text_width = NOTES_WIDTH - 2.0;
        let text_view = NSTextView::initWithFrame(
            NSTextView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(text_width, 0.0)),
        );
        text_view.setEditable(false);
        text_view.setSelectable(true);
        text_view.setDrawsBackground(false);
        text_view.setTextContainerInset(NSSize::new(NOTES_INSET, NOTES_INSET));
        text_view.setVerticallyResizable(true);
        text_view.setHorizontallyResizable(false);
        text_view.setMinSize(NSSize::new(0.0, 0.0));
        text_view.setMaxSize(NSSize::new(text_width, f64::MAX));
        text_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        let mut content_height = NOTES_MAX_HEIGHT;
        unsafe {
            if let Some(storage) = text_view.textStorage() {
                storage.setAttributedString(&content);
            }
            if let (Some(container), Some(layout)) =
                (text_view.textContainer(), text_view.layoutManager())
            {
                container.setWidthTracksTextView(true);
                container.setContainerSize(NSSize::new(text_width - 2.0 * NOTES_INSET, f64::MAX));
                layout.ensureLayoutForTextContainer(&container);
                content_height =
                    layout.usedRectForTextContainer(&container).size.height + 2.0 * NOTES_INSET;
            }
        }
        let height = content_height.ceil().min(NOTES_MAX_HEIGHT);
        text_view.setFrameSize(NSSize::new(text_width, content_height.max(height)));

        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(NOTES_WIDTH, height + 2.0),
            ),
        );
        scroll.setBorderType(NSBorderType::BezelBorder);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(true);
        scroll.setBackgroundColor(&NSColor::textBackgroundColor());
        scroll.setDocumentView(Some(&text_view));
        (scroll, text_view)
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
