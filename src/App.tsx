import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  IconCheck,
  IconDeviceFloppy as Save,
  IconGitBranch as GitBranch,
  IconLoader2 as LoaderCircle,
  IconMessageCircleQuestion,
  IconRefresh as RefreshCw,
  IconX,
} from "@tabler/icons-react";
import { invoke } from "./api";
import { TraceLogModule } from "./TraceLogModule";
import { ModelPickerDialog } from "./AppDialogs";
import {
  AppUpdateCard,
  FeaturePolicyCard,
  ModelSection,
  OperationsPanel,
  PromptOptimizationCard,
} from "./AppSections";
import { NotificationChannelsCard } from "./notifications";
import type { NotificationChannel } from "./notifications";
import { errorText, withTimeout } from "./appUtils";
import { formatBytes } from "./formatters";
import { CodeyBrandMark, SettingsModalShell } from "./SettingsModalShell";
import { useModelSelection } from "./useModelSelection";
import type { CrashpadPendingStats, TraceLogStats } from "./traceLogTypes";
import { useRuntimeStatus } from "./useRuntimeStatus";
import { useAppUpdates } from "./useAppUpdates";
import {
  NoticeLoadingText,
  NoticeToast,
  useAppNoticeController,
} from "./useAppNotice";
import {
  ConfirmationDialogHost,
  useConfirmationController,
} from "./useConfirmationDialog";
import { useStableEvent } from "./useStableEvent";
import type {
  AppProps,
  CcSwitchStatus,
  Config,
  CrashpadCleanup,
  FastContextToolsStatus,
  ModelState,
  PluginMarketplaceStatus,
  TraceLogCleanup,
} from "./App.types";
import { Badge, Button, Button as SaveButton } from "./components/semi";
import { WorkflowConsole } from "./workflows";

const Check = IconCheck;
const X = IconX;
const FEEDBACK_GROUP_QR_URL =
  "https://pub-2d17a6a8bc22426a92e297a59f55ccc3.r2.dev/qr.png";
const UNKNOWN_FAST_CONTEXT_TOOLS_STATUS: FastContextToolsStatus = {
  userConfigured: false,
  detectionFailed: true,
};

function configWithoutPromptOptimization(config: Config): Partial<Config> {
  const comparable: Partial<Config> = { ...config };
  delete comparable.settingsRevision;
  delete comparable.promptOptimization;
  return comparable;
}

function hasUnsavedConfigOutsidePromptOptimization(
  current: Config,
  persisted: Config,
) {
  return (
    JSON.stringify(configWithoutPromptOptimization(current)) !==
    JSON.stringify(configWithoutPromptOptimization(persisted))
  );
}

export function App({
  embedded = false,
  modalContainer,
  modalVisible = true,
  requestedView = "settings",
  viewRequestRevision = 0,
  workflowThreadId,
  workflowRunId,
  onAfterClose,
  onClose,
}: AppProps) {
  const [activeView, setActiveView] = useState<"settings" | "workflows">(
    requestedView,
  );
  const workflowViewAvailable = !embedded || Boolean(workflowThreadId);
  const [config, setConfig] = useState<Config | null>(null);
  const persistedConfigRef = useRef<Config | null>(null);
  const { status, setStatus, refreshStatusForLoad } = useRuntimeStatus({
    active: !embedded || modalVisible,
    embedded,
  });
  const [pluginMarketplaceStatus, setPluginMarketplaceStatus] =
    useState<PluginMarketplaceStatus | null>(null);
  const [ccSwitchStatus, setCcSwitchStatus] = useState<CcSwitchStatus | null>(
    null,
  );
  const [fastContextToolsStatus, setFastContextToolsStatus] =
    useState<FastContextToolsStatus>(UNKNOWN_FAST_CONTEXT_TOOLS_STATUS);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [portalContainer, setPortalContainer] = useState<HTMLElement | null>(
    null,
  );
  const popupContainer = modalContainer ?? null;
  const [traceSnapshotStale, setTraceSnapshotStale] = useState(false);
  const noticeController = useAppNoticeController();
  const confirmationController = useConfirmationController();
  const setNotice = noticeController.setNotice;
  const setConfirmation = confirmationController.setConfirmation;

  useEffect(() => {
    if (!modalVisible) return;
    setActiveView(
      requestedView === "workflows" && !workflowViewAvailable
        ? "settings"
        : requestedView,
    );
  }, [
    modalVisible,
    requestedView,
    viewRequestRevision,
    workflowViewAvailable,
  ]);

  const provider = ccSwitchStatus?.provider;
  const isBusy = busy !== null;
  const configLoaded = config !== null;
  const setPersistedConfig = useCallback((next: Config) => {
    persistedConfigRef.current = next;
    setConfig(next);
  }, []);
  const setSubagentOptimization = useCallback((enabled: boolean) => {
    setConfig((current) =>
      current
        ? {
            ...current,
            subagentOptimization: enabled,
            workflow: enabled
              ? { ...current.workflow, enabled: false }
              : current.workflow,
          }
        : current,
    );
    setDirty(true);
  }, []);
  const runOperation = useCallback(
    async (name: string, action: () => Promise<void>) => {
      if (isBusy) return;
      setBusy(name);
      try {
        await action();
      } catch (error) {
        setNotice({ tone: "error", text: errorText(error) });
      } finally {
        setBusy(null);
      }
    },
    [isBusy, setNotice],
  );
  const operationsStatus = useMemo(
    () => ({
      running: status.running,
      codexAppVersion: status.codexAppVersion,
      clientPlatform: status.clientPlatform,
      restartRequired: status.restartRequired,
      restartInProgress: status.restartInProgress,
      startupError: status.startupError,
      codexAppPath: status.codexAppPath,
      maintenance: status.maintenance,
      injectionScripts: status.injectionScripts,
    }),
    [
      status.running,
      status.codexAppVersion,
      status.clientPlatform,
      status.restartRequired,
      status.restartInProgress,
      status.startupError,
      status.codexAppPath,
      status.maintenance,
      status.injectionScripts,
    ],
  );
  const {
    subagentModelOptions,
    modelState,
    setModelState,
    modelPickerVisible,
    setModelPickerVisible,
    customModelInput,
    modelInputError,
    modelSyncWarning,
    draftModelSet,
    draftManualThirdPartyModelKeys,
    manualThirdPartyModelKeys,
    thirdPartyModelOptions,
    fetchCurrentModels,
    toggleDraftModel,
    deleteDraftThirdPartyModel,
    updateCustomModelInput,
    addCustomModel,
    saveModelSelection,
    deleteThirdPartyModel,
    setDefaultModel,
  } = useModelSelection({
    provider,
    runOperation,
    setPersistedConfig,
    setStatus,
    setNotice,
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
    beforeInstall: async () => {
      if (config && dirty) await persist(config);
    },
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
        ccSwitch?: CcSwitchStatus;
        fastContextToolsStatus?: FastContextToolsStatus;
      }>("load_codey_config");
      setPersistedConfig(result.config);
      setCcSwitchStatus(result.ccSwitch ?? null);
      setFastContextToolsStatus(
        result.fastContextToolsStatus ?? UNKNOWN_FAST_CONTEXT_TOOLS_STATUS,
      );
      if (result.modelState) setModelState(result.modelState);
      const [next] = await Promise.all([
        refreshStatusForLoad(),
        refreshPluginMarketplaceStatus(),
      ]);
      const startupError = next.startupError || result.startupError;
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

  function editConfig(next: Config) {
    setConfig(next);
    setDirty(true);
  }

  async function persist(next: Config) {
    const result = await invoke<{
      config: Config;
      ccSwitch?: CcSwitchStatus;
      modelState?: ModelState;
      restartRequired?: boolean;
      subagentConfigHotReloaded?: boolean;
      subagentConfigHotReloadError?: string;
      subagentDefaultsHotReloaded?: boolean;
      subagentDefaultsHotReloadError?: string;
      fastContextToolsStatus?: FastContextToolsStatus;
    }>("save_codey_config", { config: next });
    setPersistedConfig(result.config);
    setFastContextToolsStatus(
      result.fastContextToolsStatus ?? UNKNOWN_FAST_CONTEXT_TOOLS_STATUS,
    );
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

  async function persistNotificationChannels(
    current: Config,
    channels: NotificationChannel[],
    successText: string,
  ) {
    if (isBusy) return false;
    setBusy("save-notification-channel");
    try {
      await persist({
        ...current,
        webhook: {
          ...current.webhook,
          channels,
        },
      });
      setNotice({ tone: "success", text: successText });
      return true;
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
      return false;
    } finally {
      setBusy(null);
    }
  }

  async function addNotificationChannel(channel: NotificationChannel) {
    if (!config) return false;
    const channels = config.webhook.channels.some(
      (existing) => existing.id === channel.id,
    )
      ? config.webhook.channels
      : [...config.webhook.channels, channel];
    return persistNotificationChannels(
      config,
      channels,
      "通知渠道已保存，自动通知已生效",
    );
  }

  async function updateNotificationChannel(
    channelId: string,
    patch: Partial<NotificationChannel>,
  ) {
    if (!config) return false;
    const channels = config.webhook.channels.map((channel) =>
      channel.id === channelId ? { ...channel, ...patch } : channel,
    );
    return persistNotificationChannels(
      config,
      channels,
      "通知渠道已更新，自动通知已生效",
    );
  }

  async function removeNotificationChannel(channelId: string) {
    if (!config) return false;
    const channels = config.webhook.channels.filter(
      (channel) => channel.id !== channelId,
    );
    return persistNotificationChannels(
      config,
      channels,
      "通知渠道已删除",
    );
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

  async function syncPromptOptimizationCurrentProvider() {
    if (!config || isBusy) return false;
    const currentConfig = config;
    setBusy("sync-prompt-provider");
    try {
      const result = await invoke<{ config: Config }>(
        "sync_prompt_optimization_current_provider",
        { config: currentConfig.promptOptimization },
      );
      const hasOtherDraft = hasUnsavedConfigOutsidePromptOptimization(
        currentConfig,
        result.config,
      );
      persistedConfigRef.current = result.config;
      setConfig(
        hasOtherDraft
          ? {
              ...currentConfig,
              settingsRevision: result.config.settingsRevision,
              promptOptimization: result.config.promptOptimization,
            }
          : result.config,
      );
      setDirty(hasOtherDraft);
      window.dispatchEvent(
        new CustomEvent("codey:config-changed", {
          detail: { config: result.config },
        }),
      );
      return true;
    } finally {
      setBusy(null);
    }
  }

  async function saveCurrent() {
    if (!config) return;
    await runOperation("save", async () => {
      const result = await persist(config);
      const subagentHotReloaded = Boolean(
        result.subagentConfigHotReloaded ??
          result.subagentDefaultsHotReloaded,
      );
      const subagentHotReloadFailed = Boolean(
        result.subagentConfigHotReloadError ??
          result.subagentDefaultsHotReloadError,
      );
      setNotice({
        tone:
          result.restartRequired || subagentHotReloadFailed
            ? "info"
            : "success",
        text: subagentHotReloaded
          ? "Codey 设置已保存；子代理模型和思考深度已实时更新"
          : subagentHotReloadFailed
            ? "Codey 设置已保存；子代理配置暂未能热更新，重启 Codex 后生效"
            : result.restartRequired
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
    confirmationController.clear();
    onClose?.();
  }

  function askClearTraceLogs() {
    setConfirmation({
      action: "clear",
      title: "清理 Codex 诊断存储？",
      description:
        "将清空并压缩 logs_*.sqlite，同时删除已稳定写入的 Crashpad 待处理报告。最近写入、未知文件和其他 Crashpad 目录会保留；聊天历史、账号、配置及插件不受影响。清理后的诊断记录无法恢复。",
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

  function askRemoveNotificationChannel(channel: NotificationChannel) {
    const channelName = channel.kind === "telegram" ? "Telegram" : "飞书";
    setConfirmation({
      action: "delete-notification-channel",
      title: `删除${channelName}通知渠道？`,
      description:
        "将立即移除这个通知渠道，删除后不会再接收自动通知。",
      confirmLabel: "删除渠道",
      run: () => void removeNotificationChannel(channel.id),
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
        status: "ok" | "partial";
        traceCleanup?: TraceLogCleanup;
        crashpadCleanup: CrashpadCleanup;
        traceProtectionEnabled: boolean;
        crashpadProtectionEnabled: boolean;
        errors: string[];
        traceLogStats: TraceLogStats;
        crashpadPendingStats: CrashpadPendingStats;
      }>("clear_diagnostic_storage");
      setStatus((current) => ({
        ...current,
        traceLogStats: result.traceLogStats,
        crashpadPendingStats: result.crashpadPendingStats,
      }));
      setTraceSnapshotStale(false);
      const traceCleanup = result.traceCleanup;
      const crashpadCleanup = result.crashpadCleanup;
      if (
        (traceCleanup?.databasesFound ?? 0) === 0 &&
        crashpadCleanup.reportsFound === 0
      ) {
        setNotice({
          tone: "info",
          text: "未发现可清理的 Trace 日志或 Crashpad 待处理报告",
        });
        return;
      }
      const traceDetail = traceCleanup
        ? `${traceCleanup.databasesCleaned} 个日志库、${traceCleanup.rowsDeleted} 条记录`
        : "Trace 日志清理未完成";
      const crashpadDetail =
        `${crashpadCleanup.reportsDeleted} 份 Crashpad 报告、` +
        `${crashpadCleanup.filesDeleted} 个文件`;
      const reclaimed =
        (traceCleanup?.bytesReclaimed ?? 0) + crashpadCleanup.bytesReclaimed;
      const protectionDetail =
        result.traceProtectionEnabled && result.crashpadProtectionEnabled
          ? "双重保护保持开启"
          : "当前仅启用了部分保护";
      setNotice({
        tone: result.errors.length ? "error" : "success",
        text: `已处理 ${traceDetail}；${crashpadDetail}，释放 ${formatBytes(reclaimed)}；${protectionDetail}${result.errors.length ? `，另有 ${result.errors.length} 项未完成` : ""}`,
      });
    });
  }

  async function updateTraceLogStatsSnapshot() {
    const result = await invoke<{
      status: "ok" | "pending";
      traceLogStats: TraceLogStats;
      crashpadPendingStats: CrashpadPendingStats;
    }>("refresh_diagnostic_storage_stats");
    setStatus((current) => ({
      ...current,
      traceLogStats: result.traceLogStats,
      crashpadPendingStats: result.crashpadPendingStats,
    }));
    return result;
  }

  async function refreshTraceLogStats() {
    await runOperation("refresh-trace-stats", async () => {
      const result = await updateTraceLogStatsSnapshot();
      if (result.status === "pending") {
        setNotice({ tone: "info", text: "诊断存储正在统计，请稍候" });
        return;
      }
      setTraceSnapshotStale(false);
      setNotice({ tone: "success", text: "诊断存储统计已更新" });
    });
  }

  const handleCloseSettings = useStableEvent(closeSettings);
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
  const handleAddNotificationChannel = useStableEvent(addNotificationChannel);
  const handleNotificationChannelChange = useStableEvent(
    updateNotificationChannel,
  );
  const handleRequestRemoveNotificationChannel = useStableEvent(
    askRemoveNotificationChannel,
  );
  const handleSubagentOptimizationChange = useStableEvent(
    (checked: boolean) => setSubagentOptimization(checked),
  );
  const handleSyncCurrentProvider = useStableEvent(
    () => void syncCurrentProvider(),
  );
  const handleSyncPromptOptimizationCurrentProvider = useStableEvent(
    syncPromptOptimizationCurrentProvider,
  );
  const handleShowAccountUsageInHeaderChange = useStableEvent(
    (checked: boolean) => {
      if (config) {
        editConfig({ ...config, showAccountUsageInHeader: checked });
      }
    },
  );
  const handleClearTraceLogs = useStableEvent(askClearTraceLogs);
  const handleRefreshTraceLogStats = useStableEvent(
    () => void refreshTraceLogStats(),
  );
  const handleModelPickerOpenChange = useStableEvent((open: boolean) => {
    if (!isBusy || open) setModelPickerVisible(open);
  });
  if (!config || !provider) {
    const loadingContent = (
      <main className="app-shell loading-shell">
        <div className="loading-mark">
          <GitBranch size={17} />
        </div>
        <div>
          <strong>正在载入 Codey</strong>
          <p>
            <NoticeLoadingText controller={noticeController} />
          </p>
        </div>
        <LoaderCircle
          className="spinner loading-spinner"
          size={16}
          aria-hidden="true"
        />
      </main>
    );
    return embedded ? (
      <SettingsModalShell
        afterClose={onAfterClose}
        container={modalContainer}
        onCancel={handleCloseSettings}
        title="Codey 控制台"
        visible={modalVisible}
      >
        {loadingContent}
      </SettingsModalShell>
    ) : (
      loadingContent
    );
  }

  const configHeaderContent = (
    <div className="config-header-inner">
      <div className="config-brand">
        <CodeyBrandMark />
        <div className="config-brand-copy">
          <div className="config-brand-title-row">
            <h1 id={embedded ? "semi-modal-title" : undefined}>Codey 控制台</h1>
            <span className="app-version-tag">
              v{status.appVersion || "0.2.0"}
            </span>
            {dirty && (
              <Badge variant="warning" className="unsaved-badge">
                未保存更改
              </Badge>
            )}
          </div>
          <p>管理 Codex 线路、运行策略、工作流任务与诊断日志</p>
        </div>
      </div>

      {embedded && (
        <div className="config-header-feedback">
          <Button
            aria-describedby="codey-feedback-qr-description"
            aria-label="问题反馈群，悬浮或聚焦查看二维码"
            className="feedback-group-trigger"
            size="sm"
            variant="outline"
          >
            <IconMessageCircleQuestion aria-hidden="true" />
            <span className="feedback-group-label">问题反馈群</span>
          </Button>
          <div className="feedback-qr-popover" role="tooltip">
            <img src={FEEDBACK_GROUP_QR_URL} alt="问题反馈群二维码" />
            <span id="codey-feedback-qr-description">扫码加入问题反馈群</span>
          </div>
        </div>
      )}

      <div className="config-header-right">
        <div className="config-header-actions">
          {embedded && (
            <Button
              aria-label={status.running ? "重启 Codex" : "Codex 未运行"}
              className="title-restart-button"
              disabled={isBusy || status.restartInProgress || !status.running}
              onClick={handleRestartCodex}
              size="sm"
              variant="warning"
            >
              {busy === "restart" || status.restartInProgress ? (
                <LoaderCircle className="spinner" aria-hidden="true" />
              ) : (
                <RefreshCw aria-hidden="true" />
              )}
              <span className="title-action-label">
                {status.running ? "重启 Codex" : "未运行"}
              </span>
            </Button>
          )}
          <SaveButton
            aria-label={dirty ? "保存更改" : "已保存"}
            className={`save-button${embedded ? " title-save-button" : ""}${dirty ? " dirty" : ""}`}
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
            <span className="title-action-label">
              {dirty ? "保存更改" : "已保存"}
            </span>
          </SaveButton>
          {embedded && (
            <Button
              aria-label="关闭配置"
              className="codey-settings-modal-close"
              onClick={handleCloseSettings}
              size="icon-sm"
              variant="ghost"
            >
              <X aria-hidden="true" />
            </Button>
          )}
        </div>
      </div>
    </div>
  );

  const appContent = (
    <main
      className={`app-shell${embedded ? " embedded" : ""}`}
      ref={setPortalContainer}
    >
      <a
        className="skip-link"
        href={
          activeView === "settings"
            ? "#codey-settings-content"
            : "#codey-workflow-content"
        }
      >
        跳至{activeView === "settings" ? "设置" : "工作流"}内容
      </a>

      {!embedded && (
        <div className="macos-titlebar">
          <div className="macos-traffic-lights">
            <Button
              variant="ghost"
              size="icon"
              className="traffic-light close"
              title="关闭"
              aria-label="关闭窗口"
            />
            <Button
              variant="ghost"
              size="icon"
              className="traffic-light minimize"
              title="最小化"
              aria-label="最小化窗口"
            />
            <Button
              variant="ghost"
              size="icon"
              className="traffic-light zoom"
              title="缩放"
              aria-label="全屏缩放"
            />
          </div>
          <div className="macos-titlebar-title">
            <span className="app-title-text">Codey Control Panel</span>
            <span className="app-version-tag">
              v{status.appVersion || "0.2.0"}
            </span>
          </div>
          <div className="macos-titlebar-right" aria-hidden="true" />
        </div>
      )}

      {!embedded && (
        <header className="config-header">{configHeaderContent}</header>
      )}

      {workflowViewAvailable && (
        <nav className="codey-console-tabs" aria-label="Codey 控制台视图">
          <button
            type="button"
            role="tab"
            aria-controls="codey-settings-content"
            aria-selected={activeView === "settings"}
            className={activeView === "settings" ? "active" : undefined}
            onClick={() => setActiveView("settings")}
          >
            设置
          </button>
          <button
            type="button"
            role="tab"
            aria-controls="codey-workflow-content"
            aria-selected={activeView === "workflows"}
            className={activeView === "workflows" ? "active" : undefined}
            onClick={() => setActiveView("workflows")}
          >
            工作流
          </button>
        </nav>
      )}

      <div className="page-scroll">
        {activeView === "settings" ? (
        <div className="page" id="codey-settings-content" role="tabpanel">
          {/* 最上方：运行状态 (Codex 运行与维护，含 Codex 应用路径) */}
          <OperationsPanel
            config={config}
            status={operationsStatus}
            busy={busy}
            isBusy={isBusy}
            pluginMarketplaceStatus={pluginMarketplaceStatus}
            onRepairPluginMarketplace={handleRepairPluginMarketplace}
            onRestart={handleRestartCodex}
            showRestartAction={!embedded}
          />

          {/* 中间区域：分左右两栏 (左侧: 应用更新; 右侧: 消息通知与功能策略) */}
          <div className="upper-dashboard-grid">
            {/* 左侧栏：应用更新 */}
            <div className="dashboard-column upper-left-column">
              <AppUpdateCard
                appVersion={status.appVersion}
                updateResult={updateResult}
                updateCheck={updateCheck}
                downloadedUpdate={downloadedUpdate}
                busy={busy}
                isBusy={isBusy}
                onCheckUpdates={handleCheckForUpdates}
                onDownloadUpdate={handleDownloadUpdate}
                onInstallUpdate={handleInstallDownloadedUpdate}
              />
            </div>

            {/* 右侧栏：消息通知 */}
            <div className="dashboard-column upper-right-column">
              <NotificationChannelsCard
                config={config}
                container={portalContainer}
                popupContainer={popupContainer}
                isBusy={isBusy}
                onAddChannel={handleAddNotificationChannel}
                onChannelChange={handleNotificationChannelChange}
                onRequestRemoveChannel={handleRequestRemoveNotificationChannel}
              />
            </div>
          </div>

          {/* Codey 功能策略：整行独占排布 */}
          <div className="full-row-section feature-full-section">
            <FeaturePolicyCard
              config={config}
              fastContextToolsStatus={fastContextToolsStatus}
              isMacClient={status.clientPlatform === "macos"}
              popupContainer={popupContainer}
              tooltipContainer={portalContainer}
              isBusy={isBusy}
              subagentModelOptions={subagentModelOptions}
              onConfigChange={handleConfigChange}
              onSubagentOptimizationChange={handleSubagentOptimizationChange}
            />
          </div>

          {/* 提示词优化：整行独占排布 */}
          <div className="full-row-section prompt-optimization-full-section">
            <PromptOptimizationCard
              config={config}
              provider={provider}
              busy={busy}
              isBusy={isBusy}
              popupContainer={popupContainer}
              onConfigChange={handleConfigChange}
              onSyncCurrentProvider={
                handleSyncPromptOptimizationCurrentProvider
              }
            />
          </div>

          {/* 线路与模型：整行独占排布 */}
          <div className="full-row-section model-full-section">
            <ModelSection
              provider={provider}
              modelState={modelState}
              dirty={dirty}
              isBusy={isBusy}
              busy={busy}
              showAccountUsageInHeader={config.showAccountUsageInHeader}
              onSyncCurrentProvider={handleSyncCurrentProvider}
              onFetchCurrentModels={fetchCurrentModels}
              onSetDefaultModel={setDefaultModel}
              onDeleteThirdPartyModel={deleteThirdPartyModel}
              manualThirdPartyModelKeys={manualThirdPartyModelKeys}
              onShowAccountUsageInHeaderChange={
                handleShowAccountUsageInHeaderChange
              }
            />
          </div>

          {/* 诊断存储保护：整行独占排布 */}
          <div className="full-row-section trace-full-section">
            <TraceLogModule
              stats={status.traceLogStats}
              crashpadStats={status.crashpadPendingStats}
              crashpadSupported={status.clientPlatform === "macos"}
              snapshotStale={traceSnapshotStale}
              traceProtectionEnabled={config.disableTraceLogWrites}
              crashpadProtectionEnabled={config.protectCrashpadPending}
              clearBusy={busy === "clear-trace-logs"}
              refreshing={busy === "refresh-trace-stats"}
              disabled={isBusy}
              onClear={handleClearTraceLogs}
              onRefresh={handleRefreshTraceLogStats}
            />
          </div>
        </div>
        ) : (
          <div
            className="page workflow-page"
            id="codey-workflow-content"
            role="tabpanel"
          >
            <WorkflowConsole
              key={`${workflowThreadId ?? "all"}:${workflowRunId ?? "latest"}`}
              active={!embedded || modalVisible}
              initialRunId={workflowRunId}
              threadId={workflowThreadId}
            />
          </div>
        )}
      </div>

      <NoticeToast
        autoDismissEnabled={Boolean(config && provider)}
        controller={noticeController}
      />

      <ModelPickerDialog
        open={modelPickerVisible}
        isBusy={isBusy}
        busy={busy}
        container={portalContainer}
        customModelInput={customModelInput}
        modelInputError={modelInputError}
        modelSyncWarning={modelSyncWarning}
        thirdPartyModelOptions={thirdPartyModelOptions}
        modelState={modelState}
        draftModelSet={draftModelSet}
        manualThirdPartyModelKeys={draftManualThirdPartyModelKeys}
        onOpenChange={handleModelPickerOpenChange}
        onCustomModelInputChange={updateCustomModelInput}
        onAddCustomModel={addCustomModel}
        onToggleDraftModel={toggleDraftModel}
        onDeleteThirdPartyModel={deleteDraftThirdPartyModel}
        onSave={saveModelSelection}
      />

      <ConfirmationDialogHost
        container={portalContainer}
        controller={confirmationController}
      />
    </main>
  );
  return embedded ? (
    <SettingsModalShell
      afterClose={onAfterClose}
      container={modalContainer}
      header={
        <div className="semi-modal-header codey-settings-modal-header">
          {configHeaderContent}
        </div>
      }
      onCancel={handleCloseSettings}
      visible={modalVisible}
    >
      {appContent}
    </SettingsModalShell>
  ) : (
    appContent
  );
}
