use std::collections::VecDeque;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use codey_lib::fastctx::protocol::{
    FrameReader, ProtocolFrame, ProtocolLimits, ProtocolState, ResponseDisposition,
    response_disposition,
};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};

pub const RECOVERABLE_WORKER_EXIT_CODE: i32 = 75;
const RECOVERY_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const WORKER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_EOF_GRACE_PERIOD: Duration = Duration::from_secs(5);
const RECOVERY_WINDOW: Duration = Duration::from_secs(60);
const MAX_RECOVERIES_IN_WINDOW: usize = 3;
/// Codex 为每个已加载的线程各起一组 FastCtx MCP，线程空闲后仍常驻。worker 只是
/// 连到共享控制中心的薄代理，重启约 10 ms，因此空闲后回收，下次消息到达时重建。
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// 测试与诊断用的空闲阈值（毫秒），0 表示不回收。
const WORKER_IDLE_TIMEOUT_ENV: &str = "CODEY_FASTCTX_WORKER_IDLE_MS";
const WORKER_IDLE_EXIT_GRACE: Duration = Duration::from_secs(1);
/// fastshell 的后台任务随连接存在；工具清单出现该工具时不回收 worker。
const SHELL_TOOL_MARKER: &[u8] = b"\"run_background\"";
const SHELL_TOOL_SCAN_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct WorkerConfig<'a> {
    argument: &'a str,
    limits: ProtocolLimits,
    idle_timeout: Option<Duration>,
}

fn worker_idle_timeout() -> Option<Duration> {
    match std::env::var(WORKER_IDLE_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(0) => None,
        Some(milliseconds) => Some(Duration::from_millis(milliseconds)),
        None => Some(WORKER_IDLE_TIMEOUT),
    }
}

fn announces_shell_tools(line: &[u8]) -> bool {
    line.len() <= SHELL_TOOL_SCAN_LIMIT
        && line
            .windows(SHELL_TOOL_MARKER.len())
            .any(|window| window == SHELL_TOOL_MARKER)
}

pub async fn run(worker_argument: &str) -> Result<()> {
    let limits = ProtocolLimits::from_environment();
    let worker_config = WorkerConfig {
        argument: worker_argument,
        limits,
        idle_timeout: worker_idle_timeout(),
    };
    let mut worker = Worker::spawn(worker_argument, limits).await?;
    let (client_tx, mut client_rx) = mpsc::channel(32);
    let (eof_tx, eof_rx) = oneshot::channel();
    let client_task = tokio::spawn(read_client_input(
        client_tx,
        eof_tx,
        FrameReader::new(limits),
    ));

    // EOF 的期限独立于协议消息队列，也覆盖转发 stdout、stdin 和排空输出时的等待。
    // 仅在整体退出时取消监督 future，取消的写操作不会被重试或继续使用。
    let outcome = tokio::select! {
        biased;
        () = wait_for_client_eof_deadline(eof_rx) => {
            Err(anyhow::anyhow!(
                "MCP stdin 关闭后 FastCtx worker 未在 {} 秒宽限期内完成退出及输出转发",
                CLIENT_EOF_GRACE_PERIOD.as_secs()
            ))
        }
        outcome = supervise(worker_config, &mut worker, &mut client_rx, tokio::io::stdout()) => outcome,
    };
    client_task.abort();
    let _ = client_task.await;
    client_rx.close();
    while client_rx.try_recv().is_ok() {}
    let finalized = worker.finalize().await;
    combine_supervision_and_finalization(outcome, finalized)
}

async fn wait_for_client_eof_deadline(eof_rx: oneshot::Receiver<tokio::time::Instant>) {
    match eof_rx.await {
        Ok(observed_at) => tokio::time::sleep_until(observed_at + CLIENT_EOF_GRACE_PERIOD).await,
        // 输入错误仍由监督流程报告，不能把 reader 退出误判为正常 EOF。
        Err(_) => std::future::pending::<()>().await,
    }
}

async fn supervise<W>(
    worker_config: WorkerConfig<'_>,
    worker: &mut Worker,
    client_rx: &mut mpsc::Receiver<ClientInput>,
    mut stdout: W,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut state = ProtocolState::new(worker_config.limits.max_pending_requests);
    let mut recoveries = RecoveryBudget::default();
    let mut client_open = true;
    // worker 已因空闲回收；收到下一条客户端消息时重建并重放初始化。
    let mut parked = false;
    let mut last_activity = tokio::time::Instant::now();
    let mut shell_tools = false;

    loop {
        enum Event {
            Client(Option<ClientInput>),
            WorkerOutput(Option<WorkerOutput>),
            WorkerExit(std::io::Result<ExitStatus>),
            Idle,
        }

        let idle_deadline = worker_config
            .idle_timeout
            .filter(|_| {
                !parked
                    && client_open
                    && !shell_tools
                    && state.initialization_complete()
                    && state.pending_len() == 0
            })
            .map(|timeout| last_activity + timeout);

        // worker stdout 由独立 reader task 经 channel 送达：若在此处直接对管道
        // `read_until`，select! 取消该分支时会丢弃已读入的半行字节。
        let event = {
            let Worker {
                child,
                stdin: _,
                output_rx,
                output_task: _,
            } = worker;
            tokio::select! {
                input = client_rx.recv(), if client_open => Event::Client(input),
                output = output_rx.recv(), if !parked => Event::WorkerOutput(output),
                status = child.wait(), if !parked => Event::WorkerExit(status),
                () = sleep_until_deadline(idle_deadline), if idle_deadline.is_some() => Event::Idle,
            }
        };

        match event {
            Event::Idle => {
                // 先关闭 stdin 让薄代理正常断开控制中心，超时再由 finalize 终止。
                worker.stdin.take();
                let _ = tokio::time::timeout(WORKER_IDLE_EXIT_GRACE, worker.child.wait()).await;
                worker
                    .finalize()
                    .await
                    .context("回收空闲的 FastCtx worker 失败")?;
                parked = true;
            }
            Event::Client(Some(ClientInput::Line(line))) => {
                last_activity = tokio::time::Instant::now();
                state.observe_client(&line)?;
                if parked {
                    recover_worker(worker_config.argument, worker_config.limits, worker, &state)
                        .await
                        .context("空闲回收后重新启动 FastCtx worker 失败")?;
                    parked = false;
                }
                let forwarded = write_protocol_line(
                    worker
                        .stdin
                        .as_mut()
                        .context("FastCtx worker stdin 已关闭")?,
                    &line,
                )
                .await;
                if let Err(error) = forwarded {
                    let status = tokio::time::timeout(WORKER_EXIT_TIMEOUT, worker.child.wait())
                        .await
                        .with_context(|| {
                            format!(
                                "向 FastCtx worker 转发 MCP 请求失败（{error}），worker 未在 {} 秒内退出",
                                WORKER_EXIT_TIMEOUT.as_secs()
                            )
                        })?
                        .with_context(|| {
                            format!("向 FastCtx worker 转发 MCP 请求失败（{error}），且等待退出失败")
                        })?;
                    if !recover_or_finish(
                        status,
                        client_open,
                        worker_config,
                        &mut recoveries,
                        worker,
                        &mut state,
                        &mut stdout,
                    )
                    .await?
                    {
                        return Ok(());
                    }
                }
            }
            Event::Client(Some(ClientInput::Error(error))) => {
                bail!("读取 MCP stdin 失败：{error}");
            }
            Event::Client(Some(ClientInput::Eof)) | Event::Client(None) => {
                if parked {
                    return Ok(());
                }
                client_open = false;
                if let Some(mut stdin) = worker.stdin.take() {
                    stdin
                        .shutdown()
                        .await
                        .context("关闭 FastCtx worker stdin 失败")?;
                }
            }
            Event::WorkerOutput(Some(WorkerOutput::Line(line))) => {
                last_activity = tokio::time::Instant::now();
                shell_tools |= announces_shell_tools(&line);
                state.observe_server(&line);
                stdout
                    .write_all(&line)
                    .await
                    .context("转发 FastCtx MCP 响应失败")?;
                stdout.flush().await.context("刷新 MCP stdout 失败")?;
            }
            Event::WorkerOutput(Some(WorkerOutput::ReadError(error)))
                if error.kind() != std::io::ErrorKind::UnexpectedEof =>
            {
                bail!("读取 FastCtx worker stdout 失败：{error}");
            }
            Event::WorkerOutput(Some(WorkerOutput::ReadError(_))) | Event::WorkerOutput(None) => {
                let status = tokio::time::timeout(WORKER_EXIT_TIMEOUT, worker.child.wait())
                    .await
                    .with_context(|| {
                        format!(
                            "FastCtx worker stdout 关闭后未在 {} 秒内退出",
                            WORKER_EXIT_TIMEOUT.as_secs()
                        )
                    })?
                    .context("等待 FastCtx worker 退出失败")?;
                if !recover_or_finish(
                    status,
                    client_open,
                    worker_config,
                    &mut recoveries,
                    worker,
                    &mut state,
                    &mut stdout,
                )
                .await?
                {
                    return Ok(());
                }
            }
            Event::WorkerExit(Ok(status)) => {
                drain_worker_output(worker, &mut state, &mut stdout).await?;
                if !recover_or_finish(
                    status,
                    client_open,
                    worker_config,
                    &mut recoveries,
                    worker,
                    &mut state,
                    &mut stdout,
                )
                .await?
                {
                    return Ok(());
                }
            }
            Event::WorkerExit(Err(error)) => bail!("等待 FastCtx worker 退出失败：{error}"),
        }
    }
}

async fn sleep_until_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn combine_supervision_and_finalization(outcome: Result<()>, finalized: Result<()>) -> Result<()> {
    match (outcome, finalized) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.context("FastCtx supervisor 结束后回收 worker 失败")),
        (Err(error), Err(finalize_error)) => Err(error.context(format!(
            "FastCtx supervisor 报错后回收 worker 也失败：{finalize_error:#}"
        ))),
    }
}

async fn drain_worker_output<W>(
    worker: &mut Worker,
    state: &mut ProtocolState,
    stdout: &mut W,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    loop {
        match worker.output_rx.recv().await {
            Some(WorkerOutput::Line(line)) => {
                state.observe_server(&line);
                stdout
                    .write_all(&line)
                    .await
                    .context("转发 FastCtx worker 退出前的 MCP 响应失败")?;
            }
            Some(WorkerOutput::ReadError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Some(WorkerOutput::ReadError(error)) => {
                bail!("读取 FastCtx worker 退出前的 stdout 失败：{error}")
            }
            None => break,
        }
    }
    stdout
        .flush()
        .await
        .context("刷新 FastCtx worker 退出前的 MCP 响应失败")?;
    Ok(())
}

async fn recover_or_finish<W>(
    status: ExitStatus,
    client_open: bool,
    worker_config: WorkerConfig<'_>,
    recoveries: &mut RecoveryBudget,
    worker: &mut Worker,
    state: &mut ProtocolState,
    stdout: &mut W,
) -> Result<bool>
where
    W: AsyncWrite + Unpin,
{
    if !client_open {
        return Ok(false);
    }
    if status.code() != Some(RECOVERABLE_WORKER_EXIT_CODE) {
        bail!("FastCtx worker 在 MCP stdin 仍开启时退出：{status}");
    }
    if !state.initialization_complete() {
        bail!("FastCtx worker 在 MCP 初始化完成前断开，无法恢复会话");
    }

    let recovery_allowed = recoveries.begin_recovery(Instant::now());
    for error in state.take_pending_errors() {
        stdout
            .write_all(&error)
            .await
            .context("返回 FastCtx transport 恢复错误失败")?;
    }
    stdout
        .flush()
        .await
        .context("刷新 FastCtx transport 恢复错误失败")?;
    if !recovery_allowed {
        bail!(
            "FastCtx worker 在 {} 秒内第 {MAX_RECOVERIES_IN_WINDOW} 次断开，停止自动恢复并让宿主处理 MCP 失败",
            RECOVERY_WINDOW.as_secs()
        );
    }

    worker
        .finalize()
        .await
        .context("回收已断开的 FastCtx worker 失败")?;
    recover_worker(worker_config.argument, worker_config.limits, worker, state).await?;
    Ok(true)
}

/// 60 秒滑动窗口内的恢复预算：worker 反复以可恢复状态码退出时，宿主永远
/// 观测不到 MCP 失败、自身退避策略也无从介入；预算耗尽后由调用方把明确错误
/// 返回给在途请求并整体退出。
#[derive(Default)]
struct RecoveryBudget {
    attempts: VecDeque<Instant>,
}

impl RecoveryBudget {
    fn begin_recovery(&mut self, now: Instant) -> bool {
        while self
            .attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= RECOVERY_WINDOW)
        {
            self.attempts.pop_front();
        }
        if self.attempts.len() + 1 >= MAX_RECOVERIES_IN_WINDOW {
            return false;
        }
        self.attempts.push_back(now);
        true
    }
}

async fn recover_worker(
    worker_argument: &str,
    limits: ProtocolLimits,
    worker: &mut Worker,
    state: &ProtocolState,
) -> Result<()> {
    // 握手可能被客户端 EOF 期限取消；先交回 run 持有，确保它总能终止并 wait 新 worker。
    *worker = Worker::spawn(worker_argument, limits).await?;
    complete_recovery_handshake(worker, state).await
}

async fn complete_recovery_handshake(worker: &mut Worker, state: &ProtocolState) -> Result<()> {
    let recovery = tokio::time::timeout(
        RECOVERY_HANDSHAKE_TIMEOUT,
        replay_initialization(worker, state),
    )
    .await;
    match recovery {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            let finalized = worker.finalize().await;
            fail_after_finalization(error, finalized)
        }
        Err(_) => {
            let finalized = worker.finalize().await;
            fail_after_finalization(
                anyhow::anyhow!("恢复 FastCtx MCP 初始化超过 15 秒"),
                finalized,
            )
        }
    }
}

fn fail_after_finalization<T>(error: anyhow::Error, finalized: Result<()>) -> Result<T> {
    match finalized {
        Ok(()) => Err(error),
        Err(finalize_error) => Err(error.context(format!(
            "FastCtx supervisor 报错后回收 worker 也失败：{finalize_error:#}"
        ))),
    }
}

async fn replay_initialization(worker: &mut Worker, state: &ProtocolState) -> Result<()> {
    let request = state
        .initialize_request()
        .context("缺少可安全重放的 MCP initialize 请求")?;
    let initialize_id = state
        .initialize_id()
        .context("MCP initialize 请求缺少 id")?;
    write_protocol_line(
        worker
            .stdin
            .as_mut()
            .context("FastCtx worker stdin 已关闭")?,
        request,
    )
    .await
    .context("向恢复后的 FastCtx worker 重放 initialize 失败")?;

    loop {
        let line = match worker.output_rx.recv().await {
            Some(WorkerOutput::Line(line)) => line,
            Some(WorkerOutput::ReadError(error)) => {
                bail!("读取恢复后的 FastCtx initialize 响应失败：{error}")
            }
            None => {
                let status = worker
                    .child
                    .wait()
                    .await
                    .context("等待恢复后的 FastCtx worker 退出失败")?;
                bail!("恢复后的 FastCtx worker 在 initialize 响应前退出：{status}");
            }
        };
        match response_disposition(&line, initialize_id) {
            ResponseDisposition::Unrelated => continue,
            ResponseDisposition::Success => break,
            ResponseDisposition::Error => {
                bail!("恢复后的 FastCtx worker 拒绝 initialize")
            }
            ResponseDisposition::Invalid => {
                bail!("恢复后的 FastCtx worker 返回了无效 initialize 响应")
            }
        }
    }

    if let Some(notification) = state.initialized_notification() {
        write_protocol_line(
            worker
                .stdin
                .as_mut()
                .context("FastCtx worker stdin 已关闭")?,
            notification,
        )
        .await
        .context("向恢复后的 FastCtx worker重放 initialized 通知失败")?;
    }
    Ok(())
}

struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    output_rx: mpsc::Receiver<WorkerOutput>,
    output_task: Option<tokio::task::JoinHandle<()>>,
}

enum WorkerOutput {
    Line(ProtocolFrame),
    ReadError(std::io::Error),
}

async fn read_worker_output(
    stdout: ChildStdout,
    sender: mpsc::Sender<WorkerOutput>,
    frame_reader: FrameReader,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match frame_reader.read_line(&mut reader).await {
            Ok(Some(line)) => {
                if sender.send(WorkerOutput::Line(line)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = sender.send(WorkerOutput::ReadError(error)).await;
                return;
            }
        }
    }
}

impl Worker {
    async fn spawn(worker_argument: &str, limits: ProtocolLimits) -> Result<Self> {
        let executable = std::env::current_exe().context("定位 Codey FastCtx worker 失败")?;
        let mut command = Command::new(executable);
        command
            .arg(worker_argument)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().context("启动 Codey FastCtx worker 失败")?;
        let stdin = child.stdin.take().context("FastCtx worker 缺少 stdin")?;
        let stdout = child.stdout.take().context("FastCtx worker 缺少 stdout")?;
        let (output_tx, output_rx) = mpsc::channel(32);
        let output_task = tokio::spawn(read_worker_output(
            stdout,
            output_tx,
            FrameReader::new(limits),
        ));
        Ok(Self {
            child,
            stdin: Some(stdin),
            output_rx,
            output_task: Some(output_task),
        })
    }

    async fn finalize(&mut self) -> Result<()> {
        self.stdin.take();
        let mut errors = Vec::new();

        let should_kill = match self.child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(error) => {
                errors.push(format!("检查 worker 状态失败：{error}"));
                true
            }
        };
        if should_kill
            && let Err(error) = self.child.start_kill()
            && !matches!(self.child.try_wait(), Ok(Some(_)))
        {
            errors.push(format!("终止 worker 失败：{error}"));
        }

        if let Some(output_task) = self.output_task.take() {
            output_task.abort();
            if let Err(error) = output_task.await
                && !error.is_cancelled()
            {
                errors.push(format!("等待 worker stdout reader task 失败：{error}"));
            }
        }
        self.output_rx.close();
        while self.output_rx.try_recv().is_ok() {}

        match tokio::time::timeout(WORKER_EXIT_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => errors.push(format!("等待 worker 退出失败：{error}")),
            Err(_) => errors.push(format!(
                "终止 FastCtx worker 后未在 {} 秒内确认退出",
                WORKER_EXIT_TIMEOUT.as_secs()
            )),
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!(errors.join("；"))
        }
    }
}

enum ClientInput {
    Line(ProtocolFrame),
    Eof,
    Error(String),
}

async fn read_client_input(
    sender: mpsc::Sender<ClientInput>,
    eof_sender: oneshot::Sender<tokio::time::Instant>,
    frame_reader: FrameReader,
) {
    let mut stdin = BufReader::new(tokio::io::stdin());
    loop {
        match frame_reader.read_line(&mut stdin).await {
            Ok(Some(line)) => {
                if sender.send(ClientInput::Line(line)).await.is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = eof_sender.send(tokio::time::Instant::now());
                let _ = sender.send(ClientInput::Eof).await;
                return;
            }
            Err(error) => {
                let _ = sender.send(ClientInput::Error(error.to_string())).await;
                return;
            }
        }
    }
}

async fn write_protocol_line<W>(writer: &mut W, line: &[u8]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(line).await?;
    if !line.ends_with(b"\n") {
        writer.write_all(b"\n").await?;
    }
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_budget_refuses_the_third_recovery_inside_one_window() {
        let mut budget = RecoveryBudget::default();
        let start = Instant::now();

        assert!(budget.begin_recovery(start));
        assert!(budget.begin_recovery(start + Duration::from_secs(10)));
        assert!(!budget.begin_recovery(start + Duration::from_secs(20)));
        // 前两次尝试仍在窗口内时，后续恢复继续被拒绝。
        assert!(!budget.begin_recovery(start + Duration::from_secs(59)));
        // 最早一次滑出窗口后恢复名额释放。
        assert!(budget.begin_recovery(start + Duration::from_secs(61)));
        assert!(budget.begin_recovery(start + Duration::from_secs(70)));
        assert!(!budget.begin_recovery(start + Duration::from_secs(71)));
    }

    #[test]
    fn shell_tool_listing_disables_idle_reclaim_detection() {
        assert!(announces_shell_tools(
            br#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"run_background"}]}}"#
        ));
        assert!(!announces_shell_tools(
            br#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"grep"}]}}"#
        ));
        let mut large = vec![b'A'; SHELL_TOOL_SCAN_LIMIT];
        large.extend_from_slice(SHELL_TOOL_MARKER);
        assert!(
            !announces_shell_tools(&large),
            "large tool output is not scanned"
        );
    }

    #[test]
    fn fatal_error_keeps_worker_finalization_failure_in_the_error_chain() {
        let error = combine_supervision_and_finalization(
            Err(anyhow::anyhow!("读取 MCP stdin 失败")),
            Err(anyhow::anyhow!("等待 worker 退出失败")),
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("读取 MCP stdin 失败"), "{message}");
        assert!(message.contains("等待 worker 退出失败"), "{message}");
    }

    #[cfg(unix)]
    fn handshake_test_worker(script: &str) -> (Worker, u32) {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (output_tx, output_rx) = mpsc::channel(4);
        let output_task = tokio::spawn(read_worker_output(
            stdout,
            output_tx,
            FrameReader::new(ProtocolLimits::default()),
        ));
        let worker = Worker {
            child,
            stdin: Some(stdin),
            output_rx,
            output_task: Some(output_task),
        };
        (worker, pid)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_handshake_failure_terminates_and_reaps_the_new_worker() {
        let mut state = ProtocolState::with_default_limits();
        state
            .observe_client(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
            )
            .unwrap();
        let (mut worker, pid) = handshake_test_worker(
            "IFS= read -r line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1}'; IFS= read -r hold; exec sleep 30",
        );

        let error = complete_recovery_handshake(&mut worker, &state)
            .await
            .expect_err("invalid recovery response must fail the handshake");
        assert!(
            format!("{error:#}").contains("无效 initialize 响应"),
            "{error:#}"
        );
        let exists = unsafe { libc::kill(pid as i32, 0) };
        assert_eq!(exists, -1, "failed recovery worker {pid} was not reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_recovery_handshake_leaves_worker_available_for_finalization() {
        let mut state = ProtocolState::with_default_limits();
        state
            .observe_client(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
            )
            .unwrap();
        let (mut worker, pid) = handshake_test_worker("IFS= read -r line; exec sleep 30");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                complete_recovery_handshake(&mut worker, &state),
            )
            .await
            .is_err()
        );
        assert!(worker.child.try_wait().unwrap().is_none());
        worker.finalize().await.unwrap();
        assert!(worker.child.try_wait().unwrap().is_some());
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
