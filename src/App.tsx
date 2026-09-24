import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "@heroui/react";
import {
  IconCheck,
  IconCircleArrowUp,
  IconDeviceFloppy as Save,
  IconGitBranch as GitBranch,
  IconLayoutDashboard,
  IconLoader2 as LoaderCircle,
  IconMessageCircleQuestion,
  IconRefresh as RefreshCw,
  IconSettings,
  IconX,
} from "@tabler/icons-react";
import { invoke } from "./api";
import { rememberOfficialAccounts } from "./officialAccountsRequests";
import { useDraftConfig } from "./useDraftConfig";
import { ModelPickerDialog } from "./AppDialogs";
import { SystemSettingsDialog } from "./SystemSettingsDialog";
import { FeaturePolicyCard, SubagentPolicyCard } from "./FeaturePolicyCard";
import { ModelSection } from "./ModelSection";
import { UsageAnalysisPanel } from "./UsageAnalysisPanel";
import { OperationsPanel } from "./OperationsPanel";
import { CodeyPluginsSection } from "./CodeyPluginsSection";
import { CodexExtensionsPage, type ExtensionTransport } from "./features/codex-extensions";
import { canRepairMainProcessInjection, isMainProcessInjectionConfirmed } from "./runtimeStatusPresentation";
import { repairOperationResult } from "./injectionRepair";
import { PromptOptimizationCard } from "./PromptOptimizationCard";
import {
  getNotificationChannelDefinition,
} from "./notifications";
import type { NotificationChannel } from "./notifications";
import { errorText, withTimeout } from "./appUtils";
import type { DiagnosticStorageCleanup, DiagnosticStorageTarget } from "./diagnosticStorage";
import { DiagnosticCleanupNotice } from "./DiagnosticCleanupNotice";
import { modelIdsEqual, uniqueModelIds } from "./modelIds";
import { globalDefaultForRoute, routeProviderId } from "./modelRoutes";
import { customContextRestoredNote, type ModelRuntimeUpdate } from "./modelSelectionNotice";
import { CodeyBrandMark, SettingsModalShell } from "./SettingsModalShell";
import { SettingsLayout } from "./SettingsLayout";
import { SettingsPageHeader } from "./SettingsPageHeader";
import { useModelSelection } from "./useModelSelection";
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
  ProviderStatus,
  Config,
  FastContextToolsStatus,
  ModelState,
  PluginMarketplaceStatus,
  Profile,
} from "./App.types";
import { Badge, Button, Tooltip } from "./components/ui";

const Check = IconCheck;
const extensionRequest: ExtensionTransport = request => invoke("codex_extensions", { request });
const X = IconX;
const FEEDBACK_GROUP_QR_BASE_URL =
  "https://pub-2d17a6a8bc22426a92e297a59f55ccc3.r2.dev/qr.png";
const UNKNOWN_FAST_CONTEXT_TOOLS_STATUS: FastContextToolsStatus = {
  userConfigured: false,
  detectionFailed: true,
};
const CONFIG_LOAD_TIMEOUT_MS = 30_000;
const PLUGIN_MARKETPLACE_STATUS_TIMEOUT_MS = 15_000;

function localDateCacheKey(date: Date) {
  return [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, "0"),
    String(date.getDate()).padStart(2, "0"),
  ].join("");
}

function thirdPartyRouteModelState(
  config: Config,
  route: Profile,
  catalog: ModelState,
): ModelState {
  const providerId = routeProviderId(route);
  const selectedModels = uniqueModelIds([
    ...(config.selectedModelsByProvider[providerId] || []),
    ...(config.declaredOfficialModelsByProvider[providerId] || []),
  ]);
  return {
    officialModels: [],
    officialModelIds: [],
    thirdPartyModels: selectedModels,
    thirdPartyModelMetadata: catalog.thirdPartyModelMetadata,
    manualThirdPartyModels:
      config.manualThirdPartyModelsByProvider[providerId] || [],
    upstreamModels: uniqueModelIds([
      ...(config.upstreamModelsByProvider[providerId] || []),
      ...selectedModels,
    ]),
    defaultModel:
      globalDefaultForRoute(config, route, selectedModels) || selectedModels[0] || "",
  };
}

export function App({
  embedded = false,
  modalContainer,
  modalVisible = true,
  onAfterClose,
  onClose,
}: AppProps) {
  const feedbackGroupQrUrl =
    `${FEEDBACK_GROUP_QR_BASE_URL}?date=${localDateCacheKey(new Date())}`;
  const {
    config,
    setConfig,
    dirty,
    setDirty,
    persistedConfigRef,
    setPersistedConfig,
    canSyncCurrentProvider,
    editConfig,
    isOnlyNativeRouterToggle,
    adoptRouteConfig,
    discardDraft,
  } = useDraftConfig();
  const { status, setStatus, markRestartInProgress, refreshStatus, refreshStatusForLoad,
    restartStatusError, setRestartStatusError } =
    useRuntimeStatus({
      active: !embedded || modalVisible,
      embedded,
    });
  const [pluginMarketplaceStatus, setPluginMarketplaceStatus] =
    useState<PluginMarketplaceStatus | null>(null);
  const [providerStatus, setProviderStatus] = useState<ProviderStatus | null>(
    null,
  );
  const [fastContextToolsStatus, setFastContextToolsStatus] =
    useState<FastContextToolsStatus>(UNKNOWN_FAST_CONTEXT_TOOLS_STATUS);
  const [usageAnalysisOpen, setUsageAnalysisOpen] = useState(false);
  const loadGenerationRef = useRef(0);
  const settingsScroll = useRef<HTMLDivElement>(null);
  const usageReturn = useRef<{ trigger: HTMLElement; scrollTop: number } | null>(null);
  const handleOpenUsageAnalysis = useCallback((trigger: HTMLElement) => {
    usageReturn.current = { trigger, scrollTop: settingsScroll.current?.scrollTop ?? 0 };
    setUsageAnalysisOpen(true);
  }, []);
  useEffect(() => {
    if (usageAnalysisOpen || !usageReturn.current) return;
    const previous = usageReturn.current;
    const frame = requestAnimationFrame(() => {
      if (settingsScroll.current) settingsScroll.current.scrollTop = previous.scrollTop;
      previous.trigger.focus({ preventScroll: true });
      usageReturn.current = null;
    });
    return () => cancelAnimationFrame(frame);
  }, [usageAnalysisOpen]);
  const [busy, setBusy] = useState<string | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [configRepairNotice, setConfigRepairNotice] = useState<{ tone: "info" | "success" | "error"; text: string } | null>(null);
  const [injectionRepairRequested, setInjectionRepairRequested] = useState(false);
  const [systemSettingsOpen, setSystemSettingsOpen] = useState(false);
  const popupContainer = modalContainer ?? null;
  const noticeController = useAppNoticeController();
  // 诊断清理结果用 HeroUI Toast 展示；同一次清理的“进行中 / 结果”共用一条提示，后者替换前者。
  const cleanupToastKey = useRef<string | null>(null);
  const confirmationController = useConfirmationController();
  const setNotice = noticeController.set;
  const setConfirmation = confirmationController.set;
  useEffect(() => {
    if (restartStatusError) setNotice({ tone: "error", text: restartStatusError });
  }, [restartStatusError, setNotice]);

  const provider = providerStatus?.provider;
  const isBusy = busy !== null || (injectionRepairRequested && !restartStatusError);
  useEffect(() => {
    if (!injectionRepairRequested || status.restartInProgress) return;
    setInjectionRepairRequested(false);
    const confirmed = isMainProcessInjectionConfirmed(status);
    setNotice({
      tone: status.startupError || !confirmed ? "error" : "success",
      text: status.startupError || (confirmed
        ? "主进程注入已修复，Codex 已自动重启"
        : "尚未确认主进程注入修复成功，请查看运行状态或失败提示"),
    });
  }, [injectionRepairRequested, status.restartInProgress, status.startupError, status.running, status.maintenance, setNotice]);
  const configLoaded = config !== null;
  const setSubagentOptimization = useCallback((enabled: boolean) => {
    setConfig((current) =>
      current ? { ...current, subagentOptimization: enabled } : current,
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
      codexAppPath: status.codexAppPath,
      maintenance: status.maintenance,
      injectionScripts: status.injectionScripts,
      fastContextToolsActive: status.fastContextToolsActive,
      subagentOptimizationActive: status.subagentOptimizationActive,
      notificationChannelsActive: status.notificationChannelsActive,
      activeNotificationChannelCount: status.activeNotificationChannelCount,
      traceLogWriteProtectionActive: status.traceLogWriteProtectionActive,
      crashpadDiskProtectionActive: status.crashpadDiskProtectionActive,
    }),
    [
      status.running,
      status.codexAppVersion,
      status.clientPlatform,
      status.restartRequired,
      status.restartInProgress,
      status.codexAppPath,
      status.maintenance,
      status.injectionScripts,
      status.fastContextToolsActive,
      status.subagentOptimizationActive,
      status.notificationChannelsActive,
      status.activeNotificationChannelCount,
      status.traceLogWriteProtectionActive,
      status.crashpadDiskProtectionActive,
    ],
  );
  const {
    subagentModelOptions,
    modelState,
    modelEditorState,
    officialOnly,
    setModelState,
    modelPickerVisible,
    modelPickerLoading,
    setModelPickerVisible,
    beginModelPickerLoad,
    completeModelPickerLoad,
    customModelInput,
    modelInputError,
    modelSyncWarning,
    draftAutoReviewSupported,
    setDraftAutoReviewSupported,
    draftModelSet,
    draftModelContexts,
    updateDraftModelContext,
    draftReasoningEfforts,
    reasoningEffortAutoByModel,
    updateDraftReasoningEffort,
    resetDraftReasoningEffort,
    draftManualThirdPartyModelKeys,
    thirdPartyModelOptions,
    toggleDraftModel,
    deleteDraftThirdPartyModel,
    updateCustomModelInput,
    addCustomModel,
    saveModelSelection,
  } = useModelSelection({
    config,
    currentProvider: provider ?? null,
    officialAccountAvailable: status.officialAccountAvailable === true,
    runOperation,
    setPersistedConfig,
    setStatus,
    setNotice,
  });
  const {
    automaticallyChecking,
    updateResult,
    updateCheck,
    downloadedUpdate,
    checkForUpdates,
    askDownloadUpdate,
    askInstallDownloadedUpdate,
  } = useAppUpdates({
    embedded,
    configLoaded,
    autoCheckCodeyUpdates: config?.autoCheckCodeyUpdates !== false,
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
    return () => {
      loadGenerationRef.current += 1;
    };
  }, []);

  const refreshPluginRoutesRef = useRef<() => void>(() => undefined);
  refreshPluginRoutesRef.current = () => {
    void invoke<{
      config: Config;
      providerStatus?: ProviderStatus;
      modelState?: ModelState;
    }>("load_codey_config")
      .then((result) => {
        const merged = adoptRouteConfig(result.config);
        if (!merged) return;
        setDirty(merged.dirty);
        if (result.providerStatus) setProviderStatus(result.providerStatus);
        if (result.modelState) setModelState(result.modelState);
      })
      .catch(() => undefined);
  };
  useEffect(() => {
    const refresh = () => refreshPluginRoutesRef.current();
    window.addEventListener("codey:plugin-routes-changed", refresh);
    return () => window.removeEventListener("codey:plugin-routes-changed", refresh);
  }, []);

  async function load() {
    const generation = ++loadGenerationRef.current;
    const current = () => generation === loadGenerationRef.current;
    setLoadFailed(false);
    const timer = window.setTimeout(() => {
      if (!current()) return;
      setLoadFailed(true);
      setNotice({ tone: "error", text: "加载配置超时，请重新检查" });
    }, CONFIG_LOAD_TIMEOUT_MS);
    try {
      const result = await invoke<{
        config: Config;
        modelState?: ModelState;
        startupError?: string;
        officialAccountAvailable?: boolean;
        providerStatus?: ProviderStatus;
        fastContextToolsStatus?: FastContextToolsStatus;
      }>("load_codey_config");
      window.clearTimeout(timer);
      if (!current()) return;
      setLoadFailed(false);
      setPersistedConfig(result.config);
      setProviderStatus(result.providerStatus ?? null);
      if (!result.providerStatus) throw new Error("未能读取当前服务配置，请重新检查");
      if (typeof result.officialAccountAvailable === "boolean") {
        setStatus((currentStatus) => ({
          ...currentStatus,
          officialAccountAvailable: result.officialAccountAvailable,
        }));
      }
      setFastContextToolsStatus(
        result.fastContextToolsStatus ?? UNKNOWN_FAST_CONTEXT_TOOLS_STATUS,
      );
      if (result.modelState) setModelState(result.modelState);
      const [next] = await Promise.all([
        refreshStatusForLoad(),
        refreshPluginMarketplaceStatus(current),
      ]);
      if (!current()) return;
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
      window.clearTimeout(timer);
      if (!current()) return;
      setLoadFailed(true);
      setNotice({ tone: "error", text: errorText(error) });
    }
  }

  async function refreshPluginMarketplaceStatus(isCurrent: () => boolean = () => true) {
    try {
      const next = await withTimeout(
        invoke<PluginMarketplaceStatus>("plugin_marketplace_status"),
        PLUGIN_MARKETPLACE_STATUS_TIMEOUT_MS,
        "插件市场状态查询超时",
      );
      if (!isCurrent()) return next;
      setPluginMarketplaceStatus(next);
      return next;
    } catch (error) {
      const next: PluginMarketplaceStatus = {
        status: "error",
        needsRepair: true,
        message: errorText(error),
      };
      if (!isCurrent()) return next;
      setPluginMarketplaceStatus(next);
      return next;
    }
  }

  function changeAutomaticUpdateChecks(enabled: boolean) {
    if (!config || isBusy) return;
    if (enabled) {
      editConfig({ ...config, autoCheckCodeyUpdates: true });
      return;
    }
    setConfirmation({
      action: "disable-auto-update-check",
      title: "关闭自动检查 Codey 更新？",
      description: "关闭后，若 Codex 更新导致 Codey 插件无法启动，需要手动下载最新版插件包。",
      confirmLabel: "确认关闭",
      run: () => {
        setConfig((current) => current ? { ...current, autoCheckCodeyUpdates: false } : current);
        setDirty(true);
      },
    });
  }

  async function persist(next: Config) {
    const result = await invoke<{
      config: Config;
      providerStatus?: ProviderStatus;
      modelState?: ModelState;
      restartRequired?: boolean;
      modelHotReloaded?: boolean;
      modelHotReloadError?: string;
      routeRequestLogHotReloaded?: boolean;
      routeRequestLogHealth?:
        | "not_applicable"
        | "unchanged"
        | "enabled"
        | "disabled"
        | "superseded"
        | "failed";
      routeRequestLogHotReloadError?: string;
      subagentConfigHotReloaded?: boolean;
      subagentConfigRepaired?: boolean;
      subagentConfigHealth?: string;
      subagentConfigRepairReasons?: string[];
      subagentConfigHotReloadError?: string;
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
    if (result.providerStatus) setProviderStatus(result.providerStatus);
    if (result.modelState) setModelState(result.modelState);
    if (typeof result.restartRequired === "boolean") {
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired,
      }));
    }
    setDirty(false);
    // 保存结果已包含配置、模型和重启状态；其余运行状态在后台补充。
    void refreshStatus().catch(() => undefined);
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
    if (!config || isBusy) return;
    const nativeMode = config?.localRouterEnabled === false;
    const shouldPersistNativeToggle = Boolean(
      nativeMode && dirty && isOnlyNativeRouterToggle(config),
    );
    if (dirty && !shouldPersistNativeToggle) return;
    await runOperation("sync-provider", async () => {
      if (shouldPersistNativeToggle) {
        await persist(config);
      }
      const session = nativeMode
        ? beginModelPickerLoad(provider?.official ? null : provider?.id ?? null, false)
        : 0;
      try {
        const result = await invoke<{
          config: Config;
          providerStatus: ProviderStatus;
          modelState: ModelState;
          restartRequired?: boolean;
        }>("sync_current_provider");
        setPersistedConfig(result.config);
        setProviderStatus(result.providerStatus);
        setModelState(result.modelState);
        setStatus((current) => ({
          ...current,
          restartRequired: result.restartRequired ?? current.restartRequired,
        }));
        if (nativeMode) {
          completeModelPickerLoad(
            session,
            result.providerStatus.provider.official
              ? result.modelState
              : { ...result.modelState, officialModels: [], officialModelIds: [] },
            "",
            result.providerStatus.provider.official ? null : result.providerStatus.provider.id,
          );
        }
        setNotice({
          tone: result.restartRequired ? "info" : "success",
          text: nativeMode
            ? `已同步当前线路「${result.providerStatus.provider.name}」，请勾选要启用的模型`
            : result.restartRequired
              ? "已重新读取 Codex 配置，重启后应用当前线路"
              : "已重新读取 Codex 配置",
        });
      } catch (error) {
        if (nativeMode) {
          completeModelPickerLoad(
            session,
            provider?.official ? modelState : { ...modelState, officialModels: [], officialModelIds: [] },
            `自动同步失败：${errorText(error)}。请重试以读取当前账号可用模型。`,
            provider?.official ? null : provider?.id ?? null,
          );
        }
        throw error;
      }
    });
  }

  function applyRouteResult(result: {
    config: Config;
    providerStatus?: ProviderStatus;
    modelState?: ModelState;
    restartRequired?: boolean;
  }) {
    const merged = adoptRouteConfig(result.config);
    if (!merged) return;
    if (result.providerStatus) setProviderStatus(result.providerStatus);
    if (result.modelState) setModelState(result.modelState);
    if (typeof result.restartRequired === "boolean") {
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired,
      }));
    }
    setDirty(merged.dirty);
    window.dispatchEvent(
      new CustomEvent("codey:config-changed", {
        detail: { config: result.config },
      }),
    );
  }

  async function saveRoute(route: Profile) {
    if (!config) return false;
    let saved = false;
    await runOperation("save-route", async () => {
      const routeExists = config.profiles.some(
        (profile) => profile.id === route.id,
      );
      const nextConfig = {
        ...config,
        profiles: routeExists
          ? config.profiles.map((profile) =>
              profile.id === route.id ? route : profile
            )
          : [...config.profiles, route],
      };
      const result = await persist(nextConfig);
      saved = true;
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? `线路「${route.name}」已保存，重启 Codex 后注册新的接入配置`
          : `线路「${route.name}」已保存，模型选择器已刷新`,
      });
    });
    return saved;
  }

  async function setRouteEnabled(routeId: string, enabled: boolean) {
    if (!config || dirty || isBusy || !config.localRouterEnabled) return false;
    const route = config.profiles.find((profile) => profile.id === routeId);
    if (!route) return false;
    let saved = false;
    await runOperation("set-route-enabled", async () => {
      const result = await invoke<{
        config: Config;
        providerStatus?: ProviderStatus;
        modelState: ModelState;
        restartRequired?: boolean;
        modelHotReloadError?: string;
        subagentConfigHotReloadError?: string;
      }>("set_route_enabled", {
        routeId,
        enabled,
        expectedRevision: config.settingsRevision,
      });
      applyRouteResult(result);
      saved = true;
      const reloadError = result.modelHotReloadError || result.subagentConfigHotReloadError;
      setNotice({
        tone: reloadError || result.restartRequired ? "info" : "success",
        text: `线路「${route.name}」已${enabled ? "启用" : "停用"}${reloadError
          ? `，部分运行配置未更新：${reloadError}`
          : result.restartRequired ? "，重启 Codex 后完全生效" : ""}`,
      });
    });
    return saved;
  }

  async function reorderRoute(sourceId: string, targetId: string) {
    if (!config || dirty || isBusy || !config.localRouterEnabled) return;
    const profiles = [...config.profiles];
    const sourceIndex = profiles.findIndex((profile) => profile.id === sourceId);
    const targetIndex = profiles.findIndex((profile) => profile.id === targetId);
    if (sourceIndex < 0 || targetIndex < 0 || sourceIndex === targetIndex) return;
    if ((profiles[sourceIndex].enabled === false) !== (profiles[targetIndex].enabled === false)) return;
    profiles.splice(targetIndex, 0, profiles.splice(sourceIndex, 1)[0]);
    await runOperation("reorder-routes", async () => {
      await persist({ ...config, profiles });
      setNotice({ tone: "success", text: "线路顺序已保存" });
    });
  }

  async function reorderRouteModels(routeId: string, models: string[]) {
    if (!config || dirty || isBusy || !config.localRouterEnabled) return;
    const route = config.profiles.find((profile) => profile.id === routeId);
    if (!route) return;
    await runOperation("reorder-route-models", async () => {
      const result = await invoke<{
        config: Config;
        modelState: ModelState;
        restartRequired?: boolean;
      } & ModelRuntimeUpdate>("reorder_route_models", {
        routeId,
        models,
        expectedRevision: config.settingsRevision,
      });
      applyRouteResult(result);
      // 顺序调整即时生效，不按重启状态提示；只有推送到选择器失败时才提醒。
      setNotice(
        result.modelHotReloadError
          ? {
              tone: "info",
              text: `模型顺序已保存，但模型列表未刷新：${result.modelHotReloadError}`,
            }
          : { tone: "success", text: "模型顺序已保存" },
      );
    });
  }

  async function deleteRoute(routeId: string) {
    if (!config || dirty) return;
    await runOperation("delete-route", async () => {
      const result = await invoke<{
        config: Config;
        providerStatus: ProviderStatus;
        modelState: ModelState;
        restartRequired?: boolean;
        modelHotReloaded?: boolean;
      }>("delete_route", {
        routeId,
        expectedRevision: config.settingsRevision,
      });
      applyRouteResult(result);
      setNotice({
        tone: result.modelHotReloaded === false ? "info" : "success",
        text: "线路已删除，相关模型已从选择器移除",
      });
    });
  }

  function requestDeleteRoute(routeId: string) {
    if (!config) return;
    const persisted = persistedConfigRef.current;
    const persistedRoute = persisted?.profiles.some(
      (profile) => profile.id === routeId,
    );
    if (!persistedRoute) {
      const profiles = config.profiles.filter((profile) => profile.id !== routeId);
      if (profiles.length === 0) return;
      const next = {
        ...config,
        activeProfileId:
          config.activeProfileId === routeId
            ? profiles[0].id
            : config.activeProfileId,
        profiles,
      };
      setConfig(next);
      setDirty(
        !persisted ||
          JSON.stringify({ ...next, settingsRevision: 0 }) !==
            JSON.stringify({ ...persisted, settingsRevision: 0 }),
      );
      return;
    }
    if (dirty) {
      setNotice({ tone: "info", text: "请先保存或放弃当前更改，再删除已保存线路" });
      return;
    }
    const route = config.profiles.find((profile) => profile.id === routeId);
    setConfirmation({
      action: "delete-route",
      title: `删除线路「${route?.name || "未命名线路"}」？`,
      description: "该线路及其模型选择会立即从对话模型选择器移除。此操作无法撤销。",
      confirmLabel: "删除线路",
      run: () => void deleteRoute(routeId),
    });
  }

  async function fetchRouteModels(route: Profile) {
    if (!config) return;
    const nativeMode = !config.localRouterEnabled;
    if (nativeMode) {
      await syncCurrentProvider();
      return;
    }
    await runOperation("fetch-route-models", async () => {
      const savedConfig = config;
      const savedRoute = savedConfig.profiles.find((profile) => profile.id === route.id);
      if (!savedRoute) throw new Error("找不到要同步模型的线路");
      const session = beginModelPickerLoad(
        savedRoute.id,
        savedRoute.supportsAutoReview === true,
      );
      try {
        const result = await invoke<{
          config: Config;
          providerStatus: ProviderStatus;
          modelState: ModelState;
          routeModelState: ModelState;
          models: string[];
          restartRequired?: boolean;
          modelHotReloaded?: boolean;
        }>("fetch_route_models", {
          routeId: savedRoute.id,
          expectedRevision: savedConfig.settingsRevision,
        });
        applyRouteResult(result);
        completeModelPickerLoad(
          session,
          savedRoute.authMode === "officialAccount"
            ? result.routeModelState
            : { ...result.routeModelState, officialModels: [], officialModelIds: [] },
          "",
          savedRoute.id,
          result.config.profiles.find((profile) => profile.id === savedRoute.id)
            ?.supportsAutoReview === true,
        );
      } catch (error) {
        const warning = savedRoute.authMode === "officialAccount"
          ? `自动同步失败：${errorText(error)}。请重试以读取当前账号可用模型。`
          : `自动同步失败：${errorText(error)}。仍可手动录入当前线路支持的模型 ID。`;
        completeModelPickerLoad(
          session,
          savedRoute.authMode === "officialAccount"
            ? modelState
            : thirdPartyRouteModelState(savedConfig, savedRoute, modelState),
          warning,
          savedRoute.id,
          savedRoute.supportsAutoReview === true,
        );
        setNotice({
          tone: "error",
          text: "模型同步失败，已打开手动配置",
        });
      }
    });
  }

  async function saveOfficialRouteSettings(
    routeId: string,
    models: string[],
    showAccountUsageInHeader: boolean,
    enabled: boolean,
    modelContexts: Record<string, import("./App.types").ModelContextConfig>,
    upstreamProxy?: string,
    routeSettings?: {
      accountId: string;
      routeName: string;
      routeShortName: string;
      baseUrl: string;
    },
  ) {
    if (!config) return false;
    const profile = config.profiles.find((candidate) => candidate.id === routeId);
    if (!profile || profile.authMode !== "officialAccount") return false;
    if (models.length === 0) {
      setNotice({ tone: "info", text: "官方账号线路至少需要保留一个模型" });
      return false;
    }
    let saved = false;
    await runOperation("save-official-route-settings", async () => {
      const modelResult = await invoke<{
        config: Config;
        modelState: ModelState;
        restartRequired?: boolean;
        modelHotReloaded?: boolean;
        customContextsRestored?: boolean;
      } & import("./App.types").OfficialAccountsResult>("save_official_route_models", {
        routeId,
        models,
        modelContexts,
        enabled,
        showAccountUsageInHeader,
        // undefined 表示保持现状（如只同步模型），空字符串表示清除代理。
        ...(upstreamProxy === undefined ? {} : { upstreamProxy }),
        // 线路名、短名称、网关和代理写入同一账号记录，避免再打一次派生/热更新。
        ...(routeSettings
          ? {
              accountId: routeSettings.accountId,
              routeName: routeSettings.routeName,
              routeShortName: routeSettings.routeShortName,
              baseUrl: routeSettings.baseUrl,
            }
          : {}),
      });
      applyRouteResult(modelResult);
      if (routeSettings) handleOfficialAccountsChanged(modelResult);
      saved = true;
      const restartNote = modelResult.restartRequired
        ? "，重启 Codex 后完全生效"
        : "，模型与额度展示已更新";
      setNotice({
        tone:
          modelResult.restartRequired || modelResult.customContextsRestored
            ? "info"
            : "success",
        text: `官方账号设置已保存${restartNote}${customContextRestoredNote(modelResult)}`,
      });
    });
    return saved;
  }

  async function setRouteDefaultModel(routeId: string, model: string) {
    if (!config) return;
    const profile = config.profiles.find((candidate) => candidate.id === routeId);
    if (!profile) return;
    const providerId = profile.sourceProviderId || profile.id;
    const configuredOfficialModels = config.selectedModelsByProvider[providerId] || [];
    const enabledModels = profile.authMode === "officialAccount"
      ? configuredOfficialModels.length > 0
        ? configuredOfficialModels
        : modelState.officialModelIds
      : [
          ...(config.selectedModelsByProvider[providerId] || []),
          ...(config.declaredOfficialModelsByProvider[providerId] || []),
        ];
    if (!enabledModels.some((candidate) => modelIdsEqual(candidate, model))) {
      setNotice({ tone: "error", text: `模型 ${model} 不属于该线路` });
      return;
    }
    await runOperation("save-default-model", async () => {
      const result = await invoke<{
        config: Config;
        modelState: ModelState;
        restartRequired?: boolean;
      }>("save_default_model", {
        routeId,
        model,
      });
      applyRouteResult(result);
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: `已将全局默认模型设为「${profile.name} / ${model}」`,
      });
    });
  }

  async function saveCurrent() {
    if (!config) return;
    await runOperation("save", async () => {
      const retryCountChanged =
        persistedConfigRef.current?.streamMaxRetries !== config.streamMaxRetries;
      const result = await persist(config);
      const subagentHotReloaded = Boolean(result.subagentConfigHotReloaded);
      const subagentHotReloadFailed = Boolean(result.subagentConfigHotReloadError);
      const subagentConfigRepaired = Boolean(result.subagentConfigRepaired);
      const requestLogHealth = result.routeRequestLogHealth;
      const requestLogHotReloadFailed = requestLogHealth === "failed";
      const requestLogSuperseded = requestLogHealth === "superseded";
      const restartSuffix = result.restartRequired
        ? "；其他启动参数将在重启 Codex 后生效"
        : "";
      let noticeTone: "success" | "info" | "error" =
        result.restartRequired || subagentHotReloadFailed ? "info" : "success";
      let noticeText = result.restartRequired
        ? "Codey 设置已保存，启动参数将在重启 Codex 后生效"
        : "Codey 设置已保存";
      if (subagentConfigRepaired) {
        noticeText = "Codey 设置已保存；子代理配置已同步";
      } else if (subagentHotReloaded) {
        noticeText = "Codey 设置已保存；子代理配置已实时更新";
      } else if (subagentHotReloadFailed) {
        noticeText = "Codey 设置已保存；子代理配置暂未能热更新，重启 Codex 后生效";
      }
      if (requestLogHotReloadFailed) {
        noticeTone = "error";
        noticeText = `Codey 设置已保存；${result.routeRequestLogHotReloadError || "请求日志记录未能实时更新"}`;
      } else if (requestLogSuperseded) {
        noticeTone = "info";
        noticeText = `Codey 设置已保存；${result.routeRequestLogHotReloadError || "请求日志配置被更新的设置取代，请确认当前开关状态"}`;
      } else if (requestLogHealth === "enabled") {
        noticeText = `Codey 设置已保存；请求日志记录已实时开启，无需重启${restartSuffix}`;
      } else if (requestLogHealth === "disabled") {
        noticeText = `Codey 设置已保存；请求日志记录已实时关闭，无需重启${restartSuffix}`;
      }
      if (retryCountChanged && result.restartRequired) {
        noticeText = "Codey 设置已保存；会话重试次数将在重启 Codex 后生效";
      }
      setNotice({ tone: noticeTone, text: noticeText });
    });
  }

  function closeSettings() {
    if (isBusy) return;
    discardDraft();
    setModelPickerVisible(false);
    setUsageAnalysisOpen(false);
    usageReturn.current = null;
    setConfirmation(null);
    onClose?.();
  }

  function askRestartCodex() {
    if (restartStatusError) {
      void runOperation("restart", async () => {
        const next = await refreshStatus();
        setNotice({
          tone: next.startupError ? "error" : "info",
          text: next.startupError || (next.restartInProgress
            ? "Codex 仍在重启，请稍候"
            : next.running ? "Codex 已运行" : "Codex 未运行"),
        });
      });
      return;
    }
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
    const channelName = getNotificationChannelDefinition(channel.kind).addLabel;
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
      try {
        await withTimeout(invoke("restart_codey"), 10_000,
          "重启请求超时，暂时无法确认执行状态，请点击重新查询状态");
      } catch (error) {
        setRestartStatusError("暂时无法确认重启请求的执行状态，请点击重新查询状态");
        throw error;
      }
      markRestartInProgress();
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
            result.configChanged ||
            result.initializedRemote ||
            result.configuredRemote
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

  async function repairMainProcessInjection() {
    if (!config || restartStatusError || !canRepairMainProcessInjection(status)) return;
    await runOperation("repair-main-process-injection", async () => {
      if (dirty) await persist(config);
      setNotice({ tone: "info", text: "正在退出 Codex 并修复主进程注入，成功后将自动重启…" });
      try {
        await withTimeout(invoke("repair_main_process_injection"), 10_000,
          "修复请求暂未确认，请稍后重新查询状态");
        setInjectionRepairRequested(true);
      } catch (error) {
        const result = repairOperationResult(error);
        setNotice({ tone: result.tone, text: result.text });
        // 退出客户端会断开内嵌页面连接；后端明确拒绝才不会开始修复。
        if (!result.unconfirmed) return;
      }
      markRestartInProgress();
    });
  }

  function askRepairCodexConfig() {
    if (isBusy) return;
    setConfirmation({
      action: "repair-codex-config",
      title: "修复 Codex 配置？",
      description: "将检查配置文件及相关路径，修改已有配置前自动备份，并修复能够确认的问题。修复成功后，请从 Codey 重启 Codex 使修改生效。",
      confirmLabel: "确认修复",
      run: () => void repairCodexConfig(),
    });
  }

  async function repairCodexConfig() {
    await runOperation("repair-codex-config", async () => {
      const report = (notice: { tone: "info" | "success" | "error"; text: string }) => {
        setConfigRepairNotice(notice);
        setNotice(notice);
      };
      report({ tone: "info", text: "正在检查并修复 Codex 配置…" });
      try {
        const result = await invoke<{
          message: string;
          configPath: string;
          repaired: boolean;
          backupPath?: string | null;
        }>("repair_codex_config");
        const summary = result.message || (result.repaired
          ? "Codex 配置已修复"
          : "Codex 配置检查通过，无需修改");
        report({
          tone: "success",
          text: [result.repaired ? "修复成功，请从 Codey 重启 Codex 使修改生效。" : null, summary, `配置文件：${result.configPath}`, result.backupPath ? `备份文件：${result.backupPath}` : null].filter(Boolean).join("\n"),
        });
      } catch (error) {
        report({ tone: "error", text: `Codex 配置修复未完成：${errorText(error)}。请查看 Codey 错误日志；连接中断时可重新检查执行结果。` });
      }
    });
  }

  async function analyzeDiagnosticStorage(target: DiagnosticStorageTarget) {
    await runOperation("clear-diagnostic-storage", async () => {
      const title = target === "trace" ? "Trace 日志" : "Crashpad";
      const replaceCleanupToast = (next: () => string) => {
        if (cleanupToastKey.current) toast.close(cleanupToastKey.current);
        cleanupToastKey.current = next();
      };
      replaceCleanupToast(() => toast(`${title}：正在分析并清理`, {
        description: "正在统计占用并清理，请稍候…",
        isLoading: true,
        timeout: 0,
      }));
      try {
        const result = await invoke<DiagnosticStorageCleanup>("clear_diagnostic_storage", { target });
        setStatus((current) => ({
          ...current,
          traceLogWriteProtectionActive: result.traceLogWriteProtectionActive,
        }));
        const incomplete = result.status === "partial" || result.errors.length > 0;
        replaceCleanupToast(() => toast(`${title}：${incomplete ? "部分项目未完成" : "分析并清理完成"}`, {
          description: <DiagnosticCleanupNotice result={result} target={target} />,
          variant: incomplete ? "warning" : "success",
          timeout: 8_000,
        }));
      } catch (error) {
        replaceCleanupToast(() => toast.danger(`${title}：清理结果未能确认`, {
          description: `无法确认清理前后占用及清理情况：${errorText(error)}`,
          timeout: 8_000,
        }));
      }
    });
  }

  const handleCloseSettings = useStableEvent(closeSettings);
  const handleSaveCurrent = useStableEvent(() => void saveCurrent());
  const handleRepairPluginMarketplace = useStableEvent(
    () => void repairPluginMarketplace(),
  );
  const handleRestartCodex = useStableEvent(askRestartCodex);
  const handleRepairMainProcessInjection = useStableEvent(
    () => void repairMainProcessInjection(),
  );
  const handleFooterUpdateClick = useStableEvent(() => {
    if (downloadedUpdate) {
      askInstallDownloadedUpdate();
    } else if (hasUpdate) {
      askDownloadUpdate();
    } else {
      void checkForUpdates();
    }
  });
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
  const handleSaveRoute = useStableEvent(saveRoute);
  const handleSetRouteEnabled = useStableEvent(setRouteEnabled);
  const handleReorderRoute = useStableEvent(reorderRoute);
  const handleReorderRouteModels = useStableEvent(reorderRouteModels);
  const handleDeleteRoute = useStableEvent(requestDeleteRoute);
  const handleFetchRouteModels = useStableEvent((route: Profile) => {
    void fetchRouteModels(route);
  });
  const handleToggleLocalRouter = useStableEvent((checked: boolean) => {
    if (!config) return;
    editConfig({
      ...config,
      localRouterEnabled: checked,
    });
    setNotice({
      tone: "info",
      text: checked
        ? "保存并重启 Codex 后开启本地路由"
        : "保存并重启 Codex 后关闭本地路由",
    });
  });
  const handleToggleRouteRequestLog = useStableEvent((checked: boolean) => {
    if (!config) return;
    editConfig({
      ...config,
      routeRequestLog: {
        ...config.routeRequestLog,
        enabled: checked,
        ...(checked ? { backend: "sqlite" as const } : {}),
      },
    });
    setNotice({
      tone: "info",
      text: checked
        ? "保存后开启日志记录，无需重启"
        : "保存后关闭日志记录，无需重启",
    });
  });
  const handleOfficialAccountsChanged = useStableEvent(
    (result: import("./App.types").OfficialAccountsResult) => {
      rememberOfficialAccounts(result);
      if (result.config) {
        applyRouteResult({
          config: result.config,
          modelState: result.modelState,
          restartRequired: result.restartRequired,
        });
      }
      if (typeof result.officialAccountAvailable === "boolean") {
        setStatus((current) => ({
          ...current,
          officialAccountAvailable: result.officialAccountAvailable,
        }));
      }
    },
  );
  const handleNotice = useStableEvent(
    (notice: { tone: "success" | "info" | "error"; text: string }) => setNotice(notice),
  );
  const handleSaveOfficialRouteSettings = useStableEvent(
    saveOfficialRouteSettings,
  );
  const handleSetRouteDefaultModel = useStableEvent(
    (routeId: string, model: string) => {
      void setRouteDefaultModel(routeId, model);
    },
  );
  const handleAnalyzeDiagnosticStorage = useStableEvent(
    (target: DiagnosticStorageTarget) => void analyzeDiagnosticStorage(target),
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
          <strong>{loadFailed ? "Codey 加载失败" : "正在载入 Codey"}</strong>
          <p>
            <NoticeLoadingText controller={noticeController} />
          </p>
          {loadFailed && (
            <div className="flex flex-wrap gap-2 mt-3">
              <Button variant="outline" size="sm" disabled={isBusy} onClick={askRepairCodexConfig}>
                {busy === "repair-codex-config" ? <LoaderCircle className="animate-spin" aria-hidden="true" /> : <RefreshCw aria-hidden="true" />}
                {busy === "repair-codex-config" ? "检查修复中…" : "修复 Codex 配置"}
              </Button>
              <Button variant="secondary" size="sm" disabled={isBusy} onClick={() => void runOperation("reload-config", load)}>
                重新检查
              </Button>
            </div>
          )}
        </div>
        {!loadFailed && <LoaderCircle
          className="animate-spin loading-animate-spin"
          size={16}
          aria-hidden="true"
        />}
        <ConfirmationDialogHost
          container={popupContainer}
          controller={confirmationController}
        />
      </main>
    );
    return embedded ? (
      <SettingsModalShell
        afterClose={onAfterClose}
        container={modalContainer}
        onCancel={handleCloseSettings}
        title="Codey 配置"
        visible={modalVisible}
      >
        {loadingContent}
      </SettingsModalShell>
    ) : (
      loadingContent
    );
  }

  const hasUpdate =
    updateCheck?.updateAvailable === true &&
    Boolean(updateCheck.selectedAsset);
  const isCheckingUpdate = busy === "check-update" || automaticallyChecking;
  const isDownloadingUpdate = busy === "download-update";
  const isInstallingUpdate = busy === "install-update";
  const updateTooltipText = downloadedUpdate
    ? `新版本 v${downloadedUpdate.latestVersion} 已下载，点击安装并重启`
    : hasUpdate
      ? `发现新版本 v${updateCheck?.latestVersion}，点击下载更新`
      : isCheckingUpdate
        ? "正在检查更新…"
        : updateResult?.text
          ? `${updateResult.text}（点击再次检查）`
          : "检查 Codey 在线更新";

  const configHeaderContent = (
    <div className="grid w-full min-w-0 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-5 max-[760px]:grid-cols-[minmax(0,1fr)_auto_auto] max-[760px]:gap-2.5">
      <div className="flex min-w-0 items-center gap-3 justify-self-start max-[760px]:gap-2">
        <CodeyBrandMark />
        <div className="flex min-w-0 flex-col">
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <h1 className="m-0 whitespace-nowrap text-base font-bold tracking-[-0.02em] text-[var(--codey-text,#1d1d1f)]">Codey 控制台</h1>
            {dirty && (
              <Badge variant="warning">
                未保存更改
              </Badge>
            )}
          </div>
          <p className="m-0 mt-0.5 text-[11px] text-[var(--codey-muted,#6e6e73)] max-[760px]:hidden">管理 Codex 线路、模型服务、运行策略与诊断日志</p>
        </div>
      </div>

      {embedded && (
        <div className="config-header-feedback justify-self-center">
          <Button
            aria-describedby="codey-feedback-qr-description"
            aria-label="问题反馈群，鼠标悬停或键盘聚焦查看二维码"
            className="h-8! whitespace-nowrap px-3.5 text-xs max-[760px]:w-8! max-[760px]:px-0!"
            variant="brand-outline"
          >
            <IconMessageCircleQuestion aria-hidden="true" />
            <span className="max-[760px]:hidden">问题反馈群</span>
          </Button>
          <div className="feedback-qr-popover" role="tooltip">
            <img src={feedbackGroupQrUrl} alt="问题反馈群二维码" />
            <span id="codey-feedback-qr-description">扫码加入问题反馈群</span>
          </div>
        </div>
      )}

      <div className="flex min-w-0 items-center gap-4 justify-self-end">
        <div className="flex items-center gap-2">
          {embedded && (
            <Button
              aria-label={restartStatusError ? "重新查询状态" : status.running ? "重启 Codex" : "Codex 未运行"}
              className="h-8! whitespace-nowrap px-3 text-xs max-[520px]:w-8! max-[520px]:px-0!"
              disabled={isBusy || (!restartStatusError && (status.restartInProgress || !status.running))}
              onClick={handleRestartCodex}
              variant="warning"
            >
              {busy === "restart" || (status.restartInProgress && !restartStatusError) ? (
                <LoaderCircle className="animate-spin" size={14} aria-hidden="true" />
              ) : (
                <RefreshCw size={14} aria-hidden="true" />
              )}
              <span className="max-[520px]:hidden">
                {restartStatusError ? "重新查询状态" : status.running ? "重启 Codex" : "未运行"}
              </span>
            </Button>
          )}
          <Button
            aria-label={dirty ? "保存更改" : "已保存"}
            className="h-8! min-w-[84px] px-3 text-xs max-[520px]:min-w-8! max-[520px]:w-8! max-[520px]:px-0!"
            disabled={!dirty || isBusy}
            onClick={handleSaveCurrent}
            variant={dirty ? "default" : "secondary"}
          >
            {busy === "save" ? (
              <LoaderCircle className="animate-spin" size={14} aria-hidden="true" />
            ) : dirty ? (
              <Save size={14} aria-hidden="true" />
            ) : (
              <Check size={14} aria-hidden="true" />
            )}
            <span className="max-[520px]:hidden">
              {dirty ? "保存更改" : "已保存"}
            </span>
          </Button>
          {embedded && (
            <Button
              aria-label="关闭配置"
              className="size-8! flex-none p-0! rounded-full! max-[520px]:size-8! max-[520px]:p-0!"
              disabled={isBusy}
              onClick={handleCloseSettings}
              size="icon"
              variant="ghost"
            >
              <X size={15} aria-hidden="true" />
            </Button>
          )}
        </div>
      </div>
    </div>
  );

  const appContent = (
    <main className={`app-shell${embedded ? " embedded" : ""}`}>
      <a className="skip-link" href={usageAnalysisOpen ? "#usage-analysis-title" : "#codey-settings-content"}>
        {usageAnalysisOpen ? "跳至用量分析" : "跳至设置内容"}
      </a>

      {!embedded && (
        <div className="macos-titlebar">
          <div className="macos-traffic-lights">
            <Button
              variant="ghost"
              size="icon"
              className="size-3! min-w-3! rounded-full! border! border-[rgb(var(--codey-ink-rgb,0,0,0))]/15! bg-[#ff5f56]! p-0! shadow-none! hover:opacity-85"
              title="关闭"
              aria-label="关闭窗口"
            />
            <Button
              variant="ghost"
              size="icon"
              className="size-3! min-w-3! rounded-full! border! border-[rgb(var(--codey-ink-rgb,0,0,0))]/15! bg-[#ffbd2e]! p-0! shadow-none! hover:opacity-85"
              title="最小化"
              aria-label="最小化窗口"
            />
            <Button
              variant="ghost"
              size="icon"
              className="size-3! min-w-3! rounded-full! border! border-[rgb(var(--codey-ink-rgb,0,0,0))]/15! bg-[#27c93f]! p-0! shadow-none! hover:opacity-85"
              title="缩放"
              aria-label="全屏缩放"
            />
          </div>
          <div className="macos-titlebar-title">
            <span className="app-title-text">Codey Control Panel</span>
          </div>
          <div className="macos-titlebar-right" aria-hidden="true" />
        </div>
      )}

      {!embedded && (
        <header className="z-30 flex flex-col border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface,#fff)]/75 px-5 py-2.5 backdrop-blur-xl shadow-[0_3px_8px_rgba(0,0,0,0.04),0_1px_2px_rgba(0,0,0,0.02)]">
          {configHeaderContent}
        </header>
      )}

      {usageAnalysisOpen ? (
        <div className="page-scroll">
          <UsageAnalysisPanel onBack={() => setUsageAnalysisOpen(false)} />
        </div>
      ) : (
      <SettingsLayout
        contentRef={settingsScroll}
        sidebarFooter={
          <div className="sidebar-footer-bar">
            <div className="sidebar-footer-left">
              <span className="sidebar-footer-version font-mono">
                v{status.appVersion || "0.0.1"}
                {(updateCheck?.updateAvailable === true || Boolean(downloadedUpdate)) && (
                  <span
                    className="ml-1.5 inline-flex shrink-0"
                    role="img"
                    aria-label={downloadedUpdate ? "新版本已下载，待安装" : "有新版本可用"}
                    title={downloadedUpdate ? "新版本已下载，待安装" : "有新版本可用"}
                  >
                    <span className="size-1.5 rounded-full bg-[var(--codey-red)]" aria-hidden="true" />
                  </span>
                )}
              </span>
              <Tooltip content={updateTooltipText} position="top">
                <Button
                  size="icon-sm"
                  variant="ghost"
                  className="sidebar-footer-icon-btn"
                  disabled={isBusy && !isCheckingUpdate}
                  aria-label={updateTooltipText}
                  onClick={handleFooterUpdateClick}
                >
                  {isCheckingUpdate || isDownloadingUpdate || isInstallingUpdate ? (
                    <LoaderCircle className="animate-spin" size={14} aria-hidden="true" />
                  ) : downloadedUpdate ? (
                    <IconCheck size={14} className="text-success" aria-hidden="true" />
                  ) : hasUpdate ? (
                    <span className="relative inline-flex items-center justify-center">
                      <IconCircleArrowUp size={15} className="text-primary" aria-hidden="true" />
                      <span className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-primary animate-pulse" />
                    </span>
                  ) : (
                    <IconCircleArrowUp size={15} aria-hidden="true" />
                  )}
                </Button>
              </Tooltip>
            </div>

            <div className="sidebar-footer-right">
              <Tooltip content="系统设置与偏好" position="top">
                <Button
                  size="icon-sm"
                  variant="ghost"
                  className="sidebar-footer-icon-btn"
                  aria-label="系统设置与偏好"
                  onClick={() => setSystemSettingsOpen(true)}
                >
                  <IconSettings size={15} aria-hidden="true" />
                </Button>
              </Tooltip>
            </div>
          </div>
        }
        sections={{
          overview: (
            <>
              <SettingsPageHeader
                id="overview-title"
                title="基础功能"
                icon={<IconLayoutDashboard size={15} />}
                description="查看 Codex 运行状态，管理客户端功能与通知。"
              />
              <OperationsPanel
                codexAppPath={config.codexAppPath}
                fastContextToolsStatus={fastContextToolsStatus}
                status={operationsStatus}
                busy={busy}
                isBusy={isBusy}
                pluginMarketplaceStatus={pluginMarketplaceStatus}
                onRepairPluginMarketplace={handleRepairPluginMarketplace}
                onRepairMainProcessInjection={handleRepairMainProcessInjection}
                onRepairCodexConfig={askRepairCodexConfig}
                configRepairNotice={configRepairNotice}
                injectionRepairing={injectionRepairRequested || busy === "repair-main-process-injection"}
                onRestart={handleRestartCodex}
                restartStatusUnknown={Boolean(restartStatusError)}
                showRestartAction={!embedded}
              />
              <FeaturePolicyCard
                config={config}
                fastContextToolsStatus={fastContextToolsStatus}
                isMacClient={status.clientPlatform === "macos"}
                isWindowsClient={status.clientPlatform === "windows"}
                cleanupBusy={busy === "clear-diagnostic-storage"}
                onAnalyzeDiagnosticStorage={handleAnalyzeDiagnosticStorage}
                popupContainer={popupContainer}
                isBusy={isBusy}
                onConfigChange={handleConfigChange}
                onAddChannel={handleAddNotificationChannel}
                onChannelChange={handleNotificationChannelChange}
                onRequestRemoveChannel={handleRequestRemoveNotificationChannel}
              />
            </>
          ),
          models: (
            <ModelSection
              config={config}
              currentProvider={provider ?? null}
              officialAccountAvailable={status.officialAccountAvailable === true}
              popupContainer={popupContainer}
              modelState={modelState}
              dirty={dirty}
              canSyncCurrentProvider={canSyncCurrentProvider}
              isBusy={isBusy}
              busy={busy}
              showAccountUsageInHeader={config.showAccountUsageInHeader}
              subagentModelOptions={subagentModelOptions}
              onToggleLocalRouter={handleToggleLocalRouter}
              onToggleRouteRequestLog={handleToggleRouteRequestLog}
              onOpenUsageAnalysis={handleOpenUsageAnalysis}
              onSaveRoute={handleSaveRoute}
              onSetRouteEnabled={handleSetRouteEnabled}
              onReorderRoute={handleReorderRoute}
              onReorderRouteModels={handleReorderRouteModels}
              onDeleteRoute={handleDeleteRoute}
              onFetchRouteModels={handleFetchRouteModels}
              onOfficialAccountsChanged={handleOfficialAccountsChanged}
              onNotice={handleNotice}
              onSaveOfficialRouteSettings={handleSaveOfficialRouteSettings}
              onSetDefaultModel={handleSetRouteDefaultModel}
              onConfigChange={handleConfigChange}
              onRequestConfirmation={setConfirmation}
            />
          ),
          prompt: (
            <PromptOptimizationCard
              config={config}
              isBusy={isBusy}
              subagentModelOptions={subagentModelOptions}
              onConfigChange={handleConfigChange}
              onNotice={setNotice}
            />
          ),
          subagents: (
            <SubagentPolicyCard
              config={config}
              isBusy={isBusy}
              subagentModelOptions={subagentModelOptions}
              onConfigChange={handleConfigChange}
              onSubagentOptimizationChange={handleSubagentOptimizationChange}
            />
          ),
          plugins: <CodeyPluginsSection container={popupContainer} />,
          mcp: (active) => (
            <CodexExtensionsPage kind="mcp" active={active} request={extensionRequest} container={popupContainer} />
          ),
          skills: (active) => (
            <CodexExtensionsPage kind="skill" active={active} request={extensionRequest} container={popupContainer} />
          ),
        }}
      />
      )}

      <NoticeToast controller={noticeController} />

      <ModelPickerDialog
        open={modelPickerVisible}
        routeConfigReadOnly={config?.localRouterEnabled === false}
        officialOnly={officialOnly}
        isBusy={isBusy}
        busy={busy}
        container={popupContainer}
        customModelInput={customModelInput}
        modelInputError={modelInputError}
        modelSyncWarning={modelSyncWarning}
        loading={modelPickerLoading}
        autoReviewSupported={draftAutoReviewSupported}
        thirdPartyModelOptions={thirdPartyModelOptions}
        modelState={modelEditorState}
        draftModelSet={draftModelSet}
        draftModelContexts={draftModelContexts}
        draftReasoningEfforts={draftReasoningEfforts}
        reasoningEffortAutoByModel={reasoningEffortAutoByModel}
        onUpdateDraftModelContext={updateDraftModelContext}
        onUpdateDraftReasoningEffort={updateDraftReasoningEffort}
        onResetDraftReasoningEffort={resetDraftReasoningEffort}
        manualThirdPartyModelKeys={draftManualThirdPartyModelKeys}
        onOpenChange={handleModelPickerOpenChange}
        onCustomModelInputChange={updateCustomModelInput}
        onAddCustomModel={addCustomModel}
        onToggleDraftModel={toggleDraftModel}
        onDeleteThirdPartyModel={deleteDraftThirdPartyModel}
        onAutoReviewSupportedChange={setDraftAutoReviewSupported}
        onSave={saveModelSelection}
      />

      <ConfirmationDialogHost
        container={popupContainer}
        controller={confirmationController}
      />

      <SystemSettingsDialog
        open={systemSettingsOpen}
        onOpenChange={setSystemSettingsOpen}
        container={popupContainer}
        appVersion={status.appVersion}
        codexAppVersion={status.codexAppVersion}
        codexAppPath={config.codexAppPath || status.codexAppPath}
        isBusy={isBusy}
        busy={busy}
        onRepairCodexConfig={askRepairCodexConfig}
        configRepairNotice={configRepairNotice}
        autoCheckCodeyUpdates={config.autoCheckCodeyUpdates !== false}
        onAutoCheckCodeyUpdatesChange={changeAutomaticUpdateChecks}
      />
    </main>
  );
  return embedded ? (
    <SettingsModalShell
      afterClose={onAfterClose}
      container={modalContainer}
      header={
        <div className="flex w-full items-center overflow-visible">
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
