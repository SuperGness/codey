import { useEffect, useMemo, useRef, useState } from "react";
import {
  IconActivity as Activity,
  IconAlertCircle as CircleAlert,
  IconCheck,
  IconCircleCheck as CircleCheck,
  IconDeviceFloppy as Save,
  IconGitBranch as GitBranch,
  IconLoader2 as LoaderCircle,
  IconX,
} from "@tabler/icons-react";
import { invoke } from "./api";
import { formatBytes, TraceLogModule, type TraceLogStats } from "./TraceLogModule";
import { CodexAppPathDialog, ConfirmationDialog, ModelPickerDialog } from "./AppDialogs";
import { AppUpdateCard, FeaturePolicyCard, ModelSection, OperationsPanel, WebhookCard } from "./AppSections";
import type {
  AppProps,
  CcSwitchStatus,
  CodexAppDirectorySelection,
  Config,
  Confirmation,
  InlineResult,
  ModelState,
  Notice,
  RuntimeStatus,
  TraceLogCleanup,
  UpdateCheck,
  UpdateDownload,
} from "./App.types";
import { Badge, Button, Button as SaveButton } from "./components/ui";

const Check = IconCheck;
const X = IconX;

function CodeyBrandMark() {
  return (
    <svg className="config-brand-mark" viewBox="0 0 350 350" aria-hidden="true" focusable="false">
      <rect x="0" y="0" width="350" height="350" rx="34" fill="#fff" />
      <path
        d="M70 301c-16 0-24-18-13-30l73-77c8-8 8-20 0-28L65 101C50 86 57 61 78 57c9-2 18 1 25 8l91 91c18 18 18 46 0 64l-66 66c-6 6-2 15 7 15h183"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="22"
      />
    </svg>
  );
}

const SUBAGENT_MODEL = "gpt-5.6-luna";
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);
const supportsModel = (models: string[], expected: string) => models.some(
  (model) => model.trim().toLowerCase() === expected,
);

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error(message)), timeoutMs);
    promise.then((value) => {
      window.clearTimeout(timer);
      resolve(value);
    }, (error) => {
      window.clearTimeout(timer);
      reject(error);
    });
  });
}

export function App({ embedded = false, onClose }: AppProps) {
  const [config, setConfig] = useState<Config | null>(null);
  const persistedConfigRef = useRef<Config | null>(null);
  const [status, setStatus] = useState<RuntimeStatus>({ running: false });
  const [ccSwitchStatus, setCcSwitchStatus] = useState<CcSwitchStatus | null>(null);
  const [modelState, setModelState] = useState<ModelState>({
    officialModels: [],
    officialModelIds: [],
    thirdPartyModels: [],
    upstreamModels: [],
    defaultModel: "",
  });
  const [modelPickerVisible, setModelPickerVisible] = useState(false);
  const [modelQuery, setModelQuery] = useState("");
  const [draftModels, setDraftModels] = useState<string[]>([]);
  const [notice, setNotice] = useState<Notice>({ tone: "info", text: "正在连接 Codey…" });
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [webhookResult, setWebhookResult] = useState<InlineResult>({ tone: "idle", text: "" });
  const [updateResult, setUpdateResult] = useState<InlineResult>({ tone: "idle", text: "" });
  const [updateCheck, setUpdateCheck] = useState<UpdateCheck | null>(null);
  const [downloadedUpdate, setDownloadedUpdate] = useState<UpdateDownload | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [codexAppPathDialogVisible, setCodexAppPathDialogVisible] = useState(false);
  const [selectedCodexAppPath, setSelectedCodexAppPath] = useState("");
  const [codexAppPathError, setCodexAppPathError] = useState("");
  const [portalContainer, setPortalContainer] = useState<HTMLElement | null>(null);
  const [traceSnapshotStale, setTraceSnapshotStale] = useState(false);

  const provider = ccSwitchStatus?.provider;
  const officialSlugs = useMemo(
    () => new Set(modelState.officialModelIds),
    [modelState.officialModelIds],
  );
  const draftModelSet = useMemo(
    () => new Set(draftModels),
    [draftModels],
  );
  const filteredUpstreamModels = useMemo(() => {
    const query = modelQuery.trim().toLowerCase();
    return query
      ? modelState.upstreamModels.filter((model) => model.toLowerCase().includes(query))
      : modelState.upstreamModels;
  }, [modelQuery, modelState.upstreamModels]);
  const isBusy = busy !== null;

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => {
    if (!status.traceLogStats?.pending) return;
    const delays = [250, 500, 1_000, 2_000, 5_000];
    let cancelled = false;
    let timer = 0;
    let delayIndex = 0;
    const poll = () => {
      if (cancelled) return;
      const delay = delays[delayIndex];
      delayIndex = Math.min(delayIndex + 1, delays.length - 1);
      timer = window.setTimeout(async () => {
        try {
          const next = await invoke<RuntimeStatus>("runtime_status");
          if (cancelled) return;
          setStatus(next);
          if (next.traceLogStats?.pending) poll();
        } catch {
          poll();
        }
      }, delay);
    };
    poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [status.traceLogStats?.pending]);

  useEffect(() => {
    if (!status.restartInProgress) return;
    let cancelled = false;
    let timer = 0;
    const poll = () => {
      timer = window.setTimeout(async () => {
        try {
          const next = await invoke<RuntimeStatus>("runtime_status");
          if (cancelled) return;
          setStatus(next);
          if (next.restartInProgress) poll();
        } catch {
          if (!cancelled) poll();
        }
      }, 500);
    };
    poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [status.restartInProgress]);

  async function load() {
    try {
      const result = await invoke<{
        config: Config;
        modelState?: ModelState;
        startupError?: string;
        codexAppPathSelectionRequired?: boolean;
        ccSwitch?: CcSwitchStatus;
      }>("load_codey_config");
      setPersistedConfig(result.config);
      setCcSwitchStatus(result.ccSwitch ?? null);
      if (result.modelState) setModelState(result.modelState);
      const next = await refreshStatus();
      const startupError = next.startupError || result.startupError;
      setCodexAppPathDialogVisible(Boolean(result.codexAppPathSelectionRequired));
      if (startupError) {
        setNotice({ tone: "error", text: `自动启动失败：${startupError}` });
      } else if (next.restartRequired) {
        setNotice({ tone: "info", text: "已保存的配置需重启 Codex 后生效" });
      } else {
        setNotice({
          tone: next.running ? "success" : "info",
          text: next.running
            ? "当前线路和模型目录已同步"
            : "Codey 运行时已就绪",
        });
      }
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    }
  }

  async function refreshStatus() {
    const next = await invoke<RuntimeStatus>("runtime_status");
    setStatus(next);
    return next;
  }

  function setPersistedConfig(next: Config) {
    persistedConfigRef.current = next;
    setConfig(next);
  }

  function editConfig(next: Config) {
    setConfig(next);
    setDirty(true);
  }

  function setSubagentOptimization(enabled: boolean) {
    setConfig((current) => current
      ? { ...current, subagentOptimization: enabled }
      : current);
    setDirty(true);
  }

  function updateWebhook(patch: Partial<Config["webhook"]>) {
    if (!config) return;
    editConfig({ ...config, webhook: { ...config.webhook, ...patch } });
    setWebhookResult({ tone: "idle", text: "" });
  }

  async function persist(next: Config) {
    const result = await invoke<{
      config: Config;
      ccSwitch?: CcSwitchStatus;
      modelState?: ModelState;
      restartRequired?: boolean;
    }>("save_codey_config", { config: next });
    setPersistedConfig(result.config);
    window.dispatchEvent(new CustomEvent("codey:config-changed", {
      detail: { config: result.config },
    }));
    if (result.ccSwitch) setCcSwitchStatus(result.ccSwitch);
    if (result.modelState) setModelState(result.modelState);
    if (typeof result.restartRequired === "boolean") {
      setStatus((current) => ({ ...current, restartRequired: result.restartRequired }));
    }
    setDirty(false);
    return result;
  }

  async function chooseCodexAppDirectory() {
    if (isBusy) return;
    setBusy("pick-codex-app-directory");
    setCodexAppPathError("");
    try {
      const result = await invoke<CodexAppDirectorySelection>("pick_codex_app_directory");
      if (result.status === "selected" && result.path) {
        setSelectedCodexAppPath(result.path);
      }
    } catch (error) {
      setCodexAppPathError(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function confirmCodexAppPath() {
    if (!selectedCodexAppPath || isBusy) return;
    setBusy("set-codex-app-path");
    setCodexAppPathError("");
    try {
      const result = await invoke<{
        config: Config;
        ccSwitch?: CcSwitchStatus;
        modelState?: ModelState;
      }>("set_codex_app_path", { path: selectedCodexAppPath });
      setPersistedConfig(result.config);
      if (result.ccSwitch) setCcSwitchStatus(result.ccSwitch);
      if (result.modelState) setModelState(result.modelState);
      await invoke("launch_codey");
      await refreshStatus();
      setCodexAppPathDialogVisible(false);
      setSelectedCodexAppPath("");
      setNotice({ tone: "success", text: "Codex 应用路径已校验并保存，客户端已启动" });
    } catch (error) {
      setCodexAppPathError(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function syncCurrentProvider() {
    if (dirty || isBusy) return;
    await runOperation("sync-provider", async () => {
      const result = await invoke<{
        config: Config;
        ccSwitch: CcSwitchStatus;
        modelState: ModelState;
        restartRequired?: boolean;
      }>("sync_current_provider");
      setPersistedConfig(result.config);
      setCcSwitchStatus(result.ccSwitch);
      setModelState(result.modelState);
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired ?? current.restartRequired,
      }));
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? `已读取「${result.ccSwitch.provider.name}」，重启 Codex 后应用线路`
          : `已同步「${result.ccSwitch.provider.name}」`,
      });
    });
  }

  async function runOperation(name: string, action: () => Promise<void>) {
    if (isBusy) return;
    setBusy(name);
    try {
      await action();
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    } finally {
      setBusy(null);
    }
  }

  async function saveCurrent() {
    if (!config) return;
    await runOperation("save", async () => {
      const result = await persist(config);
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? "Codey 设置已保存，启动参数将在重启 Codex 后生效"
          : "Codey 设置已保存",
      });
    });
  }

  function closeSettings() {
    if (persistedConfigRef.current) {
      setConfig(persistedConfigRef.current);
    }
    setDirty(false);
    setModelPickerVisible(false);
    setConfirmation(null);
    onClose?.();
  }

  async function fetchCurrentModels() {
    if (!provider || provider.official) return;
    await runOperation("fetch-models", async () => {
      const result = await withTimeout(
        invoke<{ modelState: ModelState; restartRequired?: boolean }>("fetch_current_provider_models"),
        15_000,
        "获取上游模型超时，请检查当前线路",
      );
      setModelState(result.modelState);
      if (typeof result.restartRequired === "boolean") {
        setStatus((current) => ({ ...current, restartRequired: result.restartRequired }));
      }
      setDraftModels(result.modelState.thirdPartyModels);
      setModelQuery("");
      setModelPickerVisible(true);
    });
  }

  async function updateSubagentOptimization(checked: boolean) {
    if (!checked) {
      setSubagentOptimization(false);
      return;
    }
    if (!provider) {
      setNotice({ tone: "error", text: "当前线路尚未就绪，无法校验子代理模型" });
      return;
    }
    await runOperation("check-subagent-model", async () => {
      let supported = false;
      if (provider.official) {
        supported = modelState.officialModels.some(
          (model) => model.slug === SUBAGENT_MODEL && model.supported,
        );
      } else {
        let result: {
          models: string[];
          modelState: ModelState;
          restartRequired?: boolean;
        };
        try {
          result = await withTimeout(
            invoke("fetch_current_provider_models"),
            15_000,
            "获取上游模型超时，请检查当前线路",
          );
        } catch (error) {
          throw new Error(`无法确认当前第三方 API 是否支持 ${SUBAGENT_MODEL}：${errorText(error)}`);
        }
        setModelState(result.modelState);
        if (typeof result.restartRequired === "boolean") {
          setStatus((current) => ({ ...current, restartRequired: result.restartRequired }));
        }
        supported = supportsModel(result.models, SUBAGENT_MODEL);
      }

      if (!supported) {
        setNotice({
          tone: "error",
          text: `当前${provider.official ? "官方账号" : "第三方 API"}不支持 ${SUBAGENT_MODEL}，无法开启子代理协作优化`,
        });
        return;
      }
      setSubagentOptimization(true);
      setNotice({
        tone: "success",
        text: `已确认当前线路支持 ${SUBAGENT_MODEL}，保存并重启 Codex 后生效`,
      });
    });
  }

  function toggleDraftModel(model: string, checked: boolean) {
    setDraftModels((current) => checked
      ? current.includes(model) ? current : [...current, model]
      : current.filter((item) => item !== model));
  }

  async function saveModelSelection() {
    await runOperation("save-models", async () => {
      const result = await invoke<{
        config: Config;
        modelState: ModelState;
        restartRequired?: boolean;
      }>("save_selected_models", { models: draftModels });
      setPersistedConfig(result.config);
      setModelState(result.modelState);
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired ?? current.restartRequired,
      }));
      setModelPickerVisible(false);
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? `已更新模型列表，共 ${result.modelState.thirdPartyModels.length} 个三方模型；重启 Codex 后生效`
          : `已更新模型列表，共 ${result.modelState.thirdPartyModels.length} 个三方模型`,
      });
    });
  }

  async function setDefaultModel(model: string) {
    await runOperation("save-default-model", async () => {
      const result = await invoke<{
        config: Config;
        modelState: ModelState;
        restartRequired?: boolean;
      }>("save_default_model", { model });
      setPersistedConfig(result.config);
      setModelState(result.modelState);
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired ?? current.restartRequired,
      }));
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? `已将 ${result.modelState.defaultModel} 设为默认模型；重启 Codex 后新对话生效`
          : `已将 ${result.modelState.defaultModel} 设为默认模型`,
      });
    });
  }

  async function testWebhook() {
    if (!config || isBusy) return;
    setBusy("test-webhook");
    setWebhookResult({ tone: "pending", text: "正在发送测试通知…" });
    try {
      const next = { ...config, webhook: { ...config.webhook, enabled: true } };
      await persist(next);
      await withTimeout(
        invoke("test_webhook"),
        12_000,
        "飞书测试在 12 秒内没有完成，请检查 Webhook 地址和网络",
      );
      setWebhookResult({ tone: "success", text: "测试卡片已发送，三种会话状态通知已开启" });
      setNotice({ tone: "success", text: "飞书机器人连接成功" });
    } catch (error) {
      const text = errorText(error);
      setWebhookResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  async function checkForUpdates() {
    if (!config || isBusy) return;
    setBusy("check-update");
    setUpdateResult({ tone: "pending", text: "正在检查更新…" });
    setUpdateCheck(null);
    setDownloadedUpdate(null);
    try {
      const result = await withTimeout(
        invoke<UpdateCheck>("check_for_updates"),
        12_000,
        "检查更新超时，请检查网络",
      );
      setUpdateCheck(result);
      const text = result.updateAvailable
        ? result.selectedAsset
          ? `发现 v${result.latestVersion} 更新（当前 v${result.currentVersion}）`
          : `发现 v${result.latestVersion} 更新，但当前系统暂无可安装包`
        : `当前已是最新版本 v${result.currentVersion}`;
      setUpdateResult({ tone: result.updateAvailable && !result.selectedAsset ? "error" : "success", text });
      setNotice({
        tone: result.updateAvailable && result.selectedAsset ? "info" : result.updateAvailable ? "error" : "success",
        text,
      });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  async function downloadUpdate() {
    if (!config || isBusy || !updateCheck?.updateAvailable || !updateCheck.selectedAsset) return;
    setBusy("download-update");
    setDownloadedUpdate(null);
    setUpdateResult({ tone: "pending", text: "正在下载并校验更新…" });
    try {
      const result = await withTimeout(
        invoke<UpdateDownload>("download_update"),
        300_000,
        "下载更新超时，请稍后重试",
      );
      setDownloadedUpdate(result);
      const text = `已下载 ${result.fileName}（${formatBytes(result.size)}），校验通过`;
      setUpdateResult({ tone: "success", text });
      setNotice({ tone: "success", text });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  function askInstallDownloadedUpdate() {
    if (!downloadedUpdate || isBusy) return;
    setConfirmation({
      action: "install-update",
      title: "安装更新",
      description: `Codey 将退出当前实例，安装 ${downloadedUpdate.fileName}，然后尝试启动新版。`,
      confirmLabel: "安装并重启",
      run: () => void installDownloadedUpdate(),
    });
  }

  async function installDownloadedUpdate() {
    if (!downloadedUpdate || isBusy) return;
    setBusy("install-update");
    setUpdateResult({ tone: "pending", text: "正在启动安装器…" });
    try {
      await invoke("install_downloaded_update", { filePath: downloadedUpdate.filePath });
      const text = "正在退出 Codey 并启动安装器…";
      setUpdateResult({ tone: "pending", text });
      setNotice({ tone: "info", text });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
      setBusy(null);
    }
  }

  function askClearTraceLogs() {
    setConfirmation({
      action: "clear",
      title: "清理 Codex 日志库？",
      description: "将清空并压缩 logs_*.sqlite，只删除本地诊断/Trace 日志，不影响聊天历史、账号、配置或插件。清理后的诊断记录无法恢复。",
      confirmLabel: "确认清理",
      run: () => void clearTraceLogs(),
    });
  }

  function askRestartCodex() {
    setConfirmation({
      action: "restart",
      title: "重启 Codex？",
      description: "当前 Codex 客户端将被关闭并由 Codey 自动重新拉起，正在执行的本地任务会被中断。",
      confirmLabel: "重启 Codex",
      run: () => void restartCodex(),
    });
  }

  async function restartCodex() {
    if (!config) return;
    await runOperation("restart", async () => {
      if (dirty) await persist(config);
      setNotice({
        tone: "info",
        text: "正在重启 Codex，Codey 将自动重新拉起客户端…",
      });
      await invoke("restart_codey");
      setStatus((current) => ({
        ...current,
        restartInProgress: true,
      }));
    });
  }

  async function clearTraceLogs() {
    await runOperation("clear-trace-logs", async () => {
      const result = await invoke<{
        cleanup: TraceLogCleanup;
        protectionEnabled: boolean;
      }>("clear_codex_trace_logs");
      const cleanup = result.cleanup;
      if (cleanup.databasesFound === 0) {
        setNotice({ tone: "info", text: "未发现 Codex 日志库，无需清理" });
        return;
      }
      setTraceSnapshotStale(true);
      const protectionDetail = result.protectionEnabled
        ? "Trace 写盘保护保持开启"
        : "Trace 写盘保护保持关闭";
      setNotice({
        tone: "success",
        text: `已清理 ${cleanup.databasesCleaned} 个日志库、${cleanup.rowsDeleted} 条记录，释放 ${formatBytes(cleanup.bytesReclaimed)}；${protectionDetail}，可手动刷新统计`,
      });
    });
  }

  async function refreshTraceLogStats() {
    await runOperation("refresh-trace-stats", async () => {
      const result = await invoke<{
        status: "ok" | "pending";
        traceLogStats: TraceLogStats;
      }>("refresh_trace_log_stats");
      setStatus((current) => ({ ...current, traceLogStats: result.traceLogStats }));
      if (result.status === "pending") {
        setNotice({ tone: "info", text: "Trace 日志正在统计，请稍候" });
        return;
      }
      setTraceSnapshotStale(false);
      setNotice({ tone: "success", text: "Trace 日志统计已更新" });
    });
  }

  if (!config || !provider) {
    return (
      <main className="app-shell loading-shell">
        <div className="loading-mark"><GitBranch size={17} /></div>
        <div>
          <strong>正在载入 Codey</strong>
          <p>{notice.text}</p>
        </div>
        <LoaderCircle className="spinner loading-spinner" size={16} aria-hidden="true" />
      </main>
    );
  }

  return (
    <main className={`app-shell${embedded ? " embedded" : ""}`} ref={setPortalContainer}>
      <a className="skip-link" href="#codey-settings-content">跳至设置内容</a>

      <div className="macos-titlebar">
        <div className="macos-traffic-lights">
          <button className="traffic-light close" title="关闭" onClick={embedded ? closeSettings : undefined} aria-label="关闭窗口" />
          <button className="traffic-light minimize" title="最小化" aria-label="最小化窗口" />
          <button className="traffic-light zoom" title="缩放" aria-label="全屏缩放" />
        </div>
        <div className="macos-titlebar-title">
          <span className="app-title-text">Codey Control Panel</span>
          <span className="app-version-tag">v{status.appVersion || "0.2.0"}</span>
        </div>
        <div className="macos-titlebar-right">
          <span className={`status-pill ${status.running ? "online" : "offline"}`}>
            <span className="status-pill-dot" />
            {status.running ? (status.activeProfileName || "已就绪") : "未启动"}
          </span>
        </div>
      </div>

      <header className="config-header">
        <div className="config-header-inner">
          <div className="config-brand">
            <CodeyBrandMark />
            <div className="config-brand-copy">
              <div className="config-brand-title-row">
                <h1>Codey 控制台</h1>
                <Badge variant={provider.official ? "outline" : "secondary"}>
                  {provider.name}
                </Badge>
                {dirty && (
                  <Badge variant="warning" className="unsaved-badge">
                    未保存更改
                  </Badge>
                )}
              </div>
              <p>管理 Codex 线路、模型服务、运行策略与诊断日志</p>
            </div>
          </div>

          <div className="config-header-right">
            <div className="config-header-actions">
              <SaveButton
                className={`save-button${dirty ? " dirty" : ""}`}
                disabled={!dirty || isBusy}
                onClick={() => void saveCurrent()}
              >
                {busy === "save"
                  ? <LoaderCircle className="spinner" aria-hidden="true" />
                  : dirty
                    ? <Save aria-hidden="true" />
                    : <Check aria-hidden="true" />}
                {dirty ? "保存更改" : "已保存"}
              </SaveButton>
              {embedded && (
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="关闭设置"
                  onClick={closeSettings}
                >
                  <X aria-hidden="true" />
                </Button>
              )}
            </div>
          </div>
        </div>
      </header>

      <div className="page-scroll">
        <div className="page" id="codey-settings-content">
          {/* 最上方：运行状态 (Codex 运行与维护，含 Codex 应用路径) */}
          <OperationsPanel
            config={config}
            status={status}
            busy={busy}
            isBusy={isBusy}
            onRestart={askRestartCodex}
          />

          {/* 中间区域：分左右两栏 (左侧: 上方应用更新, 下方飞书通知; 右侧: 功能策略) */}
          <div className="upper-dashboard-grid">
            {/* 左侧栏：上方应用更新，下方飞书通知 */}
            <div className="dashboard-column upper-left-column">
              <AppUpdateCard
                status={status}
                updateResult={updateResult}
                updateCheck={updateCheck}
                downloadedUpdate={downloadedUpdate}
                busy={busy}
                isBusy={isBusy}
                onCheckUpdates={() => void checkForUpdates()}
                onDownloadUpdate={() => void downloadUpdate()}
                onInstallUpdate={askInstallDownloadedUpdate}
              />

              <WebhookCard
                config={config}
                busy={busy}
                isBusy={isBusy}
                webhookResult={webhookResult}
                onWebhookChange={updateWebhook}
                onTestWebhook={() => void testWebhook()}
              />
            </div>

            {/* 右侧栏：Codey 功能策略 */}
            <div className="dashboard-column upper-right-column">
              <FeaturePolicyCard
                config={config}
                busy={busy}
                isBusy={isBusy}
                subagentModel={SUBAGENT_MODEL}
                onConfigChange={editConfig}
                onSubagentOptimizationChange={(checked) => void updateSubagentOptimization(checked)}
              />
            </div>
          </div>

          {/* 线路与模型：整行独占排布 */}
          <div className="full-row-section model-full-section">
            <ModelSection
              ccSwitchStatus={ccSwitchStatus}
              provider={provider}
              modelState={modelState}
              dirty={dirty}
              isBusy={isBusy}
              busy={busy}
              onSyncCurrentProvider={() => void syncCurrentProvider()}
              onFetchCurrentModels={() => void fetchCurrentModels()}
              onSetDefaultModel={(model) => void setDefaultModel(model)}
            />
          </div>

          {/* Trace 日志分析：整行独占排布 */}
          <div className="full-row-section trace-full-section">
            <TraceLogModule
              stats={status.traceLogStats}
              snapshotStale={traceSnapshotStale}
              protectionEnabled={config.disableTraceLogWrites}
              clearBusy={busy === "clear-trace-logs"}
              refreshing={busy === "refresh-trace-stats"}
              disabled={isBusy}
              onClear={askClearTraceLogs}
              onRefresh={() => void refreshTraceLogStats()}
            />
          </div>
        </div>
      </div>

      {notice.text && (
        <div className={`notice-toast ${notice.tone}`} role="status" aria-live="polite">
          {notice.tone === "success"
            ? <CircleCheck size={17} />
            : notice.tone === "error"
              ? <CircleAlert size={17} />
              : <Activity size={17} />}
          <span>{notice.text}</span>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="关闭提示"
            onClick={() => setNotice({ tone: "info", text: "" })}
          >
            <X aria-hidden="true" />
          </Button>
        </div>
      )}

      <ModelPickerDialog
        open={modelPickerVisible}
        isBusy={isBusy}
        busy={busy}
        container={portalContainer}
        modelQuery={modelQuery}
        filteredUpstreamModels={filteredUpstreamModels}
        modelState={modelState}
        officialSlugs={officialSlugs}
        draftModelSet={draftModelSet}
        onOpenChange={(open) => {
          if (!isBusy || open) setModelPickerVisible(open);
        }}
        onModelQueryChange={setModelQuery}
        onDraftModelsChange={setDraftModels}
        onToggleDraftModel={toggleDraftModel}
        onSave={() => void saveModelSelection()}
      />

      <ConfirmationDialog
        confirmation={confirmation}
        container={portalContainer}
        onClose={() => setConfirmation(null)}
        onConfirm={(pending) => {
          setConfirmation(null);
          pending.run();
        }}
      />

      {status.clientPlatform === "windows" && (
        <CodexAppPathDialog
          open={codexAppPathDialogVisible}
          selectedPath={selectedCodexAppPath}
          error={codexAppPathError}
          isBusy={isBusy}
          busy={busy}
          container={portalContainer}
          onOpenChange={(open) => {
            if (!isBusy) setCodexAppPathDialogVisible(open);
          }}
          onChooseDirectory={() => void chooseCodexAppDirectory()}
          onConfirm={() => void confirmCodexAppPath()}
        />
      )}
    </main>
  );
}
