import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
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
import {
  formatBytes,
  TraceLogModule,
  type TraceLogStats,
} from "./TraceLogModule";
import {
  CodexAppPathDialog,
  ConfirmationDialog,
  ModelPickerDialog,
} from "./AppDialogs";
import {
  AppUpdateCard,
  ExperimentalFeaturesCard,
  FeaturePolicyCard,
  ModelSection,
  OperationsPanel,
} from "./AppSections";
import { NotificationChannelsCard } from "./notifications";
import { errorText, withTimeout } from "./appUtils";
import { useModelSelection } from "./useModelSelection";
import { useNotifications } from "./useNotifications";
import { useRuntimeStatus } from "./useRuntimeStatus";
import { useAppUpdates } from "./useAppUpdates";
import type {
  AppProps,
  CcSwitchStatus,
  CodexAppDirectorySelection,
  Config,
  Confirmation,
  ModelState,
  Notice,
  PluginMarketplaceStatus,
  TraceLogCleanup,
} from "./App.types";
import { Badge, Button, Button as SaveButton } from "./components/semi";

const Check = IconCheck;
const X = IconX;
/** Fired by the Shadow overlay mask / toggle so App can discard dirty state first. */
const SETTINGS_REQUEST_CLOSE_EVENT = "codey-settings-request-close";

function CodeyBrandMark() {
  return (
    <svg
      className="config-brand-mark"
      viewBox="0 0 350 350"
      aria-hidden="true"
      focusable="false"
    >
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

function useStableEvent<Args extends unknown[], Result>(
  callback: (...args: Args) => Result,
): (...args: Args) => Result {
  const callbackRef = useRef(callback);
  useLayoutEffect(() => {
    callbackRef.current = callback;
  }, [callback]);
  return useCallback((...args: Args) => callbackRef.current(...args), []);
}

export function App({ embedded = false, onClose }: AppProps) {
  const [config, setConfig] = useState<Config | null>(null);
  const persistedConfigRef = useRef<Config | null>(null);
  const { status, setStatus, refreshStatus, refreshStatusForLoad } =
    useRuntimeStatus({ embedded });
  const [pluginMarketplaceStatus, setPluginMarketplaceStatus] =
    useState<PluginMarketplaceStatus | null>(null);
  const [ccSwitchStatus, setCcSwitchStatus] = useState<CcSwitchStatus | null>(
    null,
  );
  const [notice, setNotice] = useState<Notice>({
    tone: "info",
    text: "正在连接 Codey…",
  });
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [codexAppPathDialogVisible, setCodexAppPathDialogVisible] =
    useState(false);
  const [selectedCodexAppPath, setSelectedCodexAppPath] = useState("");
  const [codexAppPathError, setCodexAppPathError] = useState("");
  const [portalContainer, setPortalContainer] = useState<HTMLElement | null>(
    null,
  );
  const [traceSnapshotStale, setTraceSnapshotStale] = useState(false);

  // Toast lives in the main shell only; keep loading copy until config arrives.
  useEffect(() => {
    if (!config || !notice.text) return;
    const timeoutMs = notice.tone === "error" ? 7000 : 4000;
    const token = notice.text;
    const timer = window.setTimeout(() => {
      setNotice((current) =>
        current.text === token ? { tone: "info", text: "" } : current,
      );
    }, timeoutMs);
    return () => window.clearTimeout(timer);
  }, [config, notice.text, notice.tone]);

  const provider = ccSwitchStatus?.provider;
  const isBusy = busy !== null;
  const configLoaded = config !== null;
  const {
    subagentModel,
    modelState,
    setModelState,
    modelPickerVisible,
    setModelPickerVisible,
    modelQuery,
    setModelQuery,
    setDraftModels,
    officialSlugs,
    draftModelSet,
    filteredUpstreamModels,
    fetchCurrentModels,
    updateSubagentOptimization,
    toggleDraftModel,
    saveModelSelection,
    setDefaultModel,
  } = useModelSelection({
    provider,
    runOperation,
    setPersistedConfig,
    setStatus,
    setNotice,
    setSubagentOptimization,
  });
  const {
    webhookResults,
    addNotificationChannel,
    updateNotificationChannel,
    removeNotificationChannel,
    testWebhook,
  } = useNotifications({
    config,
    isBusy,
    setConfig,
    setDirty,
    setBusy,
    setNotice,
    persist,
  });
  const {
    updateResult,
    updateCheck,
    downloadedUpdate,
    checkForUpdates,
    downloadUpdate,
    askInstallDownloadedUpdate,
  } = useAppUpdates({
    embedded,
    configLoaded,
    isBusy,
    setBusy,
    setNotice,
    setConfirmation,
  });

  useEffect(() => {
    void load();
  }, []);

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
      const [next] = await Promise.all([
        refreshStatusForLoad(),
        refreshPluginMarketplaceStatus(),
      ]);
      const startupError = next.startupError || result.startupError;
      setCodexAppPathDialogVisible(
        Boolean(result.codexAppPathSelectionRequired),
      );
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

  async function refreshPluginMarketplaceStatus() {
    try {
      const next = await invoke<PluginMarketplaceStatus>(
        "plugin_marketplace_status",
      );
      setPluginMarketplaceStatus(next);
      return next;
    } catch (error) {
      const next: PluginMarketplaceStatus = {
        status: "error",
        needsRepair: true,
        message: errorText(error),
      };
      setPluginMarketplaceStatus(next);
      return next;
    }
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
    setConfig((current) =>
      current ? { ...current, subagentOptimization: enabled } : current,
    );
    setDirty(true);
  }

  async function persist(next: Config) {
    const result = await invoke<{
      config: Config;
      ccSwitch?: CcSwitchStatus;
      modelState?: ModelState;
      restartRequired?: boolean;
    }>("save_codey_config", { config: next });
    setPersistedConfig(result.config);
    window.dispatchEvent(
      new CustomEvent("codey:config-changed", {
        detail: { config: result.config },
      }),
    );
    if (result.ccSwitch) setCcSwitchStatus(result.ccSwitch);
    if (result.modelState) setModelState(result.modelState);
    if (typeof result.restartRequired === "boolean") {
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired,
      }));
    }
    setDirty(false);
    return result;
  }

  async function chooseCodexAppDirectory() {
    if (isBusy) return;
    setBusy("pick-codex-app-directory");
    setCodexAppPathError("");
    try {
      const result = await invoke<CodexAppDirectorySelection>(
        "pick_codex_app_directory",
      );
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
      setNotice({
        tone: "success",
        text: "Codex 应用路径已校验并保存，客户端已启动",
      });
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

  async function syncOfficialExperimentalFeatures() {
    if (!config) return;
    await runOperation("sync-experimental-features", async () => {
      const result = await invoke<{
        experimentalFeatures: Config["experimentalFeatures"];
      }>("sync_official_experimental_features");
      editConfig({
        ...config,
        experimentalFeatures: result.experimentalFeatures,
      });
      setNotice({
        tone: "info",
        text: "已同步官方试验性功能配置，保存并重启 Codex 后生效",
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

  const handleCloseSettings = useStableEvent(closeSettings);

  useEffect(() => {
    if (!embedded && !onClose) return;
    const onRequestClose = () => handleCloseSettings();
    window.addEventListener(SETTINGS_REQUEST_CLOSE_EVENT, onRequestClose);
    return () => {
      window.removeEventListener(SETTINGS_REQUEST_CLOSE_EVENT, onRequestClose);
    };
  }, [embedded, onClose, handleCloseSettings]);

  function askClearTraceLogs() {
    setConfirmation({
      action: "clear",
      title: "清理 Codex 日志库？",
      description:
        "将清空并压缩 logs_*.sqlite，只删除本地诊断/Trace 日志，不影响聊天历史、账号、配置或插件。清理后的诊断记录无法恢复。",
      confirmLabel: "确认清理",
      run: () => void clearTraceLogs(),
    });
  }

  function askRestartCodex() {
    setConfirmation({
      action: "restart",
      title: "重启 Codex？",
      description:
        "当前 Codex 客户端将被关闭并由 Codey 自动重新拉起，正在执行的本地任务会被中断。",
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

  async function repairPluginMarketplace() {
    await runOperation("repair-plugin-marketplace", async () => {
      const result = await withTimeout(
        invoke<PluginMarketplaceStatus>("repair_plugin_marketplace"),
        30_000,
        "插件市场修复超时，请稍后重试",
      );
      setPluginMarketplaceStatus(result);
      if (result.status === "ready") {
        setNotice({
          tone: "success",
          text:
            result.configChanged || result.initializedRemote
              ? "插件市场已修复并立即生效，无需重启 Codex"
              : "插件市场状态正常，无需修改",
        });
        return;
      }
      setNotice({
        tone: "error",
        text: "插件市场仍有缺失项，请检查本地市场文件后重试",
      });
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
      setStatus((current) => ({
        ...current,
        traceLogStats: result.traceLogStats,
      }));
      if (result.status === "pending") {
        setNotice({ tone: "info", text: "Trace 日志正在统计，请稍候" });
        return;
      }
      setTraceSnapshotStale(false);
      setNotice({ tone: "success", text: "Trace 日志统计已更新" });
    });
  }

  const handleSaveCurrent = useStableEvent(() => void saveCurrent());
  const handleRepairPluginMarketplace = useStableEvent(
    () => void repairPluginMarketplace(),
  );
  const handleRestartCodex = useStableEvent(askRestartCodex);
  const handleCheckForUpdates = useStableEvent(() => void checkForUpdates());
  const handleDownloadUpdate = useStableEvent(() => void downloadUpdate());
  const handleInstallDownloadedUpdate = useStableEvent(
    askInstallDownloadedUpdate,
  );
  const handleConfigChange = useStableEvent(editConfig);
  const handleSyncOfficialExperimentalFeatures = useStableEvent(
    () => void syncOfficialExperimentalFeatures(),
  );
  const handleAddNotificationChannel = useStableEvent(addNotificationChannel);
  const handleNotificationChannelChange = useStableEvent(
    updateNotificationChannel,
  );
  const handleRemoveNotificationChannel = useStableEvent(
    removeNotificationChannel,
  );
  const handleTestWebhook = useStableEvent(
    (channelId: string) => void testWebhook(channelId),
  );
  const handleSubagentOptimizationChange = useStableEvent(
    (checked: boolean) => void updateSubagentOptimization(checked),
  );
  const handleSyncCurrentProvider = useStableEvent(
    () => void syncCurrentProvider(),
  );
  const handleFetchCurrentModels = useStableEvent(
    () => void fetchCurrentModels(),
  );
  const handleSetDefaultModel = useStableEvent(
    (model: string) => void setDefaultModel(model),
  );
  const handleClearTraceLogs = useStableEvent(askClearTraceLogs);
  const handleRefreshTraceLogStats = useStableEvent(
    () => void refreshTraceLogStats(),
  );
  const handleModelPickerOpenChange = useStableEvent((open: boolean) => {
    if (!isBusy || open) setModelPickerVisible(open);
  });
  const handleToggleDraftModel = useStableEvent(toggleDraftModel);
  const handleSaveModelSelection = useStableEvent(
    () => void saveModelSelection(),
  );
  const handleConfirmationClose = useStableEvent(() => setConfirmation(null));
  const handleConfirmationConfirm = useStableEvent((pending: Confirmation) => {
    setConfirmation(null);
    pending.run();
  });
  const handleCodexAppPathOpenChange = useStableEvent((open: boolean) => {
    if (!isBusy) setCodexAppPathDialogVisible(open);
  });
  const handleChooseCodexAppDirectory = useStableEvent(
    () => void chooseCodexAppDirectory(),
  );
  const handleConfirmCodexAppPath = useStableEvent(
    () => void confirmCodexAppPath(),
  );

  if (!config || !provider) {
    return (
      <main className="app-shell loading-shell">
        <div className="loading-mark">
          <GitBranch size={17} />
        </div>
        <div>
          <strong>正在载入 Codey</strong>
          <p>{notice.text}</p>
        </div>
        <LoaderCircle
          className="spinner loading-spinner"
          size={16}
          aria-hidden="true"
        />
      </main>
    );
  }

  return (
    <main
      className={`app-shell${embedded ? " embedded" : ""}`}
      ref={setPortalContainer}
    >
      <a className="skip-link" href="#codey-settings-content">
        跳至设置内容
      </a>

      <div className="panel-titlebar">
        <div className="panel-titlebar-side" aria-hidden="true" />
        <div className="panel-titlebar-title">
          <span className="app-title-text">Codey Control Panel</span>
          <span className="app-version-tag">
            v{status.appVersion || "0.2.0"}
          </span>
        </div>
        <div className="panel-titlebar-side panel-titlebar-side-end">
          {(embedded || onClose) && (
            <button
              type="button"
              className="panel-titlebar-close"
              data-codey-tip="关闭"
              onClick={handleCloseSettings}
              aria-label="关闭"
            >
              <X size={16} aria-hidden="true" />
            </button>
          )}
        </div>
      </div>

      <header className="config-header">
        <div className="config-header-inner">
          <div className="config-brand">
            <CodeyBrandMark />
            <div className="config-brand-copy">
              <div className="config-brand-title-row">
                <h1>Codey 控制台</h1>
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
                onClick={handleSaveCurrent}
              >
                {busy === "save" ? (
                  <LoaderCircle className="spinner" aria-hidden="true" />
                ) : dirty ? (
                  <Save aria-hidden="true" />
                ) : (
                  <Check aria-hidden="true" />
                )}
                {dirty ? "保存更改" : "已保存"}
              </SaveButton>
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
            pluginMarketplaceStatus={pluginMarketplaceStatus}
            onRepairPluginMarketplace={handleRepairPluginMarketplace}
            onRestart={handleRestartCodex}
          />

          {/* 中间区域：分左右两栏 (左侧: 应用更新与试验性功能; 右侧: 消息通知与功能策略) */}
          <div className="upper-dashboard-grid">
            {/* 左侧栏：上方应用更新，下方试验性功能 */}
            <div className="dashboard-column upper-left-column">
              <AppUpdateCard
                status={status}
                updateResult={updateResult}
                updateCheck={updateCheck}
                downloadedUpdate={downloadedUpdate}
                busy={busy}
                isBusy={isBusy}
                onCheckUpdates={handleCheckForUpdates}
                onDownloadUpdate={handleDownloadUpdate}
                onInstallUpdate={handleInstallDownloadedUpdate}
              />

              <ExperimentalFeaturesCard
                config={config}
                status={status}
                busy={busy}
                isBusy={isBusy}
                onConfigChange={handleConfigChange}
                onSyncOfficial={handleSyncOfficialExperimentalFeatures}
              />
            </div>

            {/* 右侧栏：上方消息通知，下方 Codey 功能策略 */}
            <div className="dashboard-column upper-right-column">
              <NotificationChannelsCard
                config={config}
                busy={busy}
                isBusy={isBusy}
                webhookResults={webhookResults}
                onAddChannel={handleAddNotificationChannel}
                onChannelChange={handleNotificationChannelChange}
                onRemoveChannel={handleRemoveNotificationChannel}
                onTestWebhook={handleTestWebhook}
              />

              <FeaturePolicyCard
                config={config}
                status={status}
                busy={busy}
                isBusy={isBusy}
                subagentModel={subagentModel}
                onConfigChange={handleConfigChange}
                onSubagentOptimizationChange={handleSubagentOptimizationChange}
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
              onSyncCurrentProvider={handleSyncCurrentProvider}
              onFetchCurrentModels={handleFetchCurrentModels}
              onSetDefaultModel={handleSetDefaultModel}
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
              onClear={handleClearTraceLogs}
              onRefresh={handleRefreshTraceLogStats}
            />
          </div>
        </div>
      </div>

      {notice.text && (
        <div
          className={`notice-toast ${notice.tone}`}
          role="status"
          aria-live="polite"
        >
          {notice.tone === "success" ? (
            <CircleCheck size={17} />
          ) : notice.tone === "error" ? (
            <CircleAlert size={17} />
          ) : (
            <Activity size={17} />
          )}
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
        onOpenChange={handleModelPickerOpenChange}
        onModelQueryChange={setModelQuery}
        onDraftModelsChange={setDraftModels}
        onToggleDraftModel={handleToggleDraftModel}
        onSave={handleSaveModelSelection}
      />

      <ConfirmationDialog
        confirmation={confirmation}
        container={portalContainer}
        onClose={handleConfirmationClose}
        onConfirm={handleConfirmationConfirm}
      />

      {status.clientPlatform === "windows" && (
        <CodexAppPathDialog
          open={codexAppPathDialogVisible}
          selectedPath={selectedCodexAppPath}
          error={codexAppPathError}
          isBusy={isBusy}
          busy={busy}
          container={portalContainer}
          onOpenChange={handleCodexAppPathOpenChange}
          onChooseDirectory={handleChooseCodexAppDirectory}
          onConfirm={handleConfirmCodexAppPath}
        />
      )}
    </main>
  );
}
