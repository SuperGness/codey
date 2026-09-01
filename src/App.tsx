import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  IconCheck,
  IconCircleArrowUp,
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
import { FeaturePolicyCard, SubagentPolicyCard } from "./FeaturePolicyCard";
import { ModelSection } from "./ModelSection";
import { OperationsPanel } from "./OperationsPanel";
import { PromptOptimizationCard } from "./PromptOptimizationCard";
import {
  getNotificationChannelDefinition,
  NotificationChannelsCard,
} from "./notifications";
import type { NotificationChannel } from "./notifications";
import { errorText, withTimeout } from "./appUtils";
import { formatBytes } from "./formatters";
import { modelIdsEqual, uniqueModelIds } from "./modelIds";
import { globalDefaultForRoute, routeProviderId } from "./modelRoutes";
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
  ProviderStatus,
  Config,
  CrashpadCleanup,
  FastContextToolsStatus,
  ModelState,
  PluginMarketplaceStatus,
  Profile,
  TraceLogCleanup,
} from "./App.types";
import { Badge, Button, Tooltip } from "./components/mantine";

const Check = IconCheck;
const X = IconX;
const FEEDBACK_GROUP_QR_BASE_URL =
  "https://pub-2d17a6a8bc22426a92e297a59f55ccc3.r2.dev/qr.png";
const UNKNOWN_FAST_CONTEXT_TOOLS_STATUS: FastContextToolsStatus = {
  userConfigured: false,
  detectionFailed: true,
};

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
    officialModelIds: catalog.officialModelIds,
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

function onlyLocalRouterToggleChanged(current: Config, persisted: Config) {
  if (current.localRouterEnabled === persisted.localRouterEnabled) return false;
  return JSON.stringify({
    ...current,
    localRouterEnabled: persisted.localRouterEnabled,
    settingsRevision: 0,
  }) === JSON.stringify({ ...persisted, settingsRevision: 0 });
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
  const [config, setConfig] = useState<Config | null>(null);
  const persistedConfigRef = useRef<Config | null>(null);
  const { status, setStatus, refreshStatus, refreshStatusForLoad } =
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
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [portalContainer, setPortalContainer] = useState<HTMLElement | null>(
    null,
  );
  const popupContainer = modalContainer ?? null;
  const getTooltipContainer = useCallback(
    () => popupContainer ?? portalContainer ?? document.body,
    [popupContainer, portalContainer],
  );
  const noticeController = useAppNoticeController();
  const confirmationController = useConfirmationController();
  const setNotice = noticeController.setNotice;
  const setConfirmation = confirmationController.setConfirmation;

  const provider = providerStatus?.provider;
  const isBusy = busy !== null;
  const configLoaded = config !== null;
  const pendingNativeRouterToggle = Boolean(
    config &&
      persistedConfigRef.current &&
      !config.localRouterEnabled &&
      onlyLocalRouterToggleChanged(config, persistedConfigRef.current),
  );
  const canSyncCurrentProvider = !dirty || pendingNativeRouterToggle;
  const setPersistedConfig = useCallback((next: Config) => {
    persistedConfigRef.current = next;
    setConfig(next);
  }, []);
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
    setModelState,
    modelPickerVisible,
    setModelPickerVisible,
    customModelInput,
    modelInputError,
    modelSyncWarning,
    draftAutoReviewSupported,
    setDraftAutoReviewSupported,
    draftModelSet,
    draftManualThirdPartyModelKeys,
    thirdPartyModelOptions,
    openModelPicker,
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
        officialAccountAvailable?: boolean;
        providerStatus?: ProviderStatus;
        fastContextToolsStatus?: FastContextToolsStatus;
      }>("load_codey_config");
      setPersistedConfig(result.config);
      setProviderStatus(result.providerStatus ?? null);
      if (typeof result.officialAccountAvailable === "boolean") {
        setStatus((current) => ({
          ...current,
          officialAccountAvailable: result.officialAccountAvailable,
        }));
      }
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
    await refreshStatus().catch(() => undefined);
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
      nativeMode &&
        dirty &&
        persistedConfigRef.current &&
        onlyLocalRouterToggleChanged(config, persistedConfigRef.current),
    );
    if (dirty && !shouldPersistNativeToggle) return;
    await runOperation("sync-provider", async () => {
      if (shouldPersistNativeToggle) {
        await persist(config);
      }
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
        openModelPicker(
          result.providerStatus.provider.official
            ? result.modelState
            : { ...result.modelState, officialModels: [] },
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
    });
  }

  function applyRouteResult(result: {
    config: Config;
    providerStatus?: ProviderStatus;
    modelState?: ModelState;
    restartRequired?: boolean;
  }) {
    setPersistedConfig(result.config);
    if (result.providerStatus) setProviderStatus(result.providerStatus);
    if (result.modelState) setModelState(result.modelState);
    if (typeof result.restartRequired === "boolean") {
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired,
      }));
    }
    setDirty(false);
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
    if (nativeMode || route.authMode === "officialAccount") {
      await syncCurrentProvider();
      return;
    }
    await runOperation("fetch-route-models", async () => {
      const savedConfig = config;
      const savedRoute = savedConfig.profiles.find((profile) => profile.id === route.id);
      if (!savedRoute) throw new Error("找不到要同步模型的线路");
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
        openModelPicker(
          { ...result.routeModelState, officialModels: [] },
          "",
          savedRoute.id,
          result.config.profiles.find((profile) => profile.id === savedRoute.id)
            ?.supportsAutoReview === true,
        );
        setNotice({
          tone: "success",
          text: `已同步「${savedRoute.name}」的 ${result.models.length} 个模型，请勾选要启用的模型`,
        });
      } catch (error) {
        const warning = `自动同步失败：${errorText(error)}。仍可手动录入当前线路支持的模型 ID。`;
        openModelPicker(
          thirdPartyRouteModelState(savedConfig, savedRoute, modelState),
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
      }>("save_official_route_models", { routeId, models });
      applyRouteResult(modelResult);
      const result = modelResult.config.showAccountUsageInHeader === showAccountUsageInHeader
        ? modelResult
        : await persist({
            ...modelResult.config,
            showAccountUsageInHeader,
          });
      saved = true;
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? "官方账号设置已保存，重启 Codex 后完全生效"
          : "官方账号设置已保存，模型与额度展示已更新",
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
        noticeText =
          "Codey 设置已保存；已校验并修复子代理运行配置，下一次派生将使用当前角色映射";
      } else if (subagentHotReloaded) {
        noticeText = "Codey 设置已保存；子代理模型和思考深度已实时更新";
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
      setNotice({ tone: noticeTone, text: noticeText });
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

  async function clearTraceLogs() {
    await runOperation("clear-trace-logs", async () => {
      const result = await invoke<{
        status: "ok" | "partial";
        traceCleanup?: TraceLogCleanup;
        crashpadCleanup: CrashpadCleanup;
        traceProtectionEnabled: boolean;
        traceLogWriteProtectionActive: boolean;
        crashpadProtectionEnabled: boolean;
        errors: string[];
        traceLogStats: TraceLogStats;
        crashpadPendingStats: CrashpadPendingStats;
      }>("clear_diagnostic_storage");
      setStatus((current) => ({
        ...current,
        traceLogStats: result.traceLogStats,
        crashpadPendingStats: result.crashpadPendingStats,
        traceLogWriteProtectionActive:
          result.traceLogWriteProtectionActive,
      }));
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
  const handleSaveRoute = useStableEvent(saveRoute);
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
        ? "保存并重启 Codex 后启用本地路由"
        : "保存并重启 Codex 后关闭本地路由；线路配置将保持只读",
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
        ? "保存后将实时开启请求日志记录，无需重启 Codex"
        : "保存后将实时关闭请求日志记录，无需重启 Codex；历史日志仍可查看",
    });
  });
  const handleToggleAccountUsage = useStableEvent((checked: boolean) => {
    if (!config) return;
    editConfig({
      ...config,
      showAccountUsageInHeader: checked,
    });
  });
  const handleSaveOfficialRouteSettings = useStableEvent(
    saveOfficialRouteSettings,
  );
  const handleSetRouteDefaultModel = useStableEvent(
    (routeId: string, model: string) => {
      void setRouteDefaultModel(routeId, model);
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
          className="animate-spin loading-animate-spin"
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
  const isCheckingUpdate = busy === "check-update";
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
          <div className="flex min-w-0 items-center gap-2">
            <h1 className="m-0 whitespace-nowrap text-base font-bold tracking-[-0.02em] text-[#1d1d1f]">Codey 控制台</h1>
            <span className="whitespace-nowrap text-[11px] font-medium tracking-[0.01em] text-[#8e8e93]">
              v{status.appVersion || "0.2.0"}
            </span>

            <Tooltip
              content={updateTooltipText}
              getPopupContainer={getTooltipContainer}
              position="bottom"
            >
              <span className="header-update-btn-wrap">
                <button
                  type="button"
                  className={`header-update-pill ${
                    hasUpdate
                      ? "has-update"
                      : downloadedUpdate
                        ? "has-downloaded"
                        : ""
                  }`}
                  disabled={isBusy}
                  aria-label={updateTooltipText}
                  onClick={() => {
                    if (downloadedUpdate) {
                      handleInstallDownloadedUpdate();
                    } else if (hasUpdate) {
                      handleDownloadUpdate();
                    } else {
                      handleCheckForUpdates();
                    }
                  }}
                >
                  {isCheckingUpdate || isDownloadingUpdate || isInstallingUpdate ? (
                    <LoaderCircle className="animate-spin" size={12} aria-hidden="true" />
                  ) : downloadedUpdate ? (
                    <IconCheck size={12} aria-hidden="true" />
                  ) : (
                    <IconCircleArrowUp size={13} aria-hidden="true" />
                  )}
                  {downloadedUpdate ? (
                    <span className="header-update-pill-label">
                      v{downloadedUpdate.latestVersion} 已下载
                    </span>
                  ) : hasUpdate ? (
                    <span className="header-update-pill-label">
                      v{updateCheck?.latestVersion} 可更新
                    </span>
                  ) : null}
                </button>
              </span>
            </Tooltip>

            {dirty && (
              <Badge variant="warning">
                未保存更改
              </Badge>
            )}
          </div>
          <p className="m-0 mt-0.5 text-[11px] text-[#6e6e73] max-[760px]:hidden">管理 Codex 线路、模型服务、运行策略与诊断日志</p>
        </div>
      </div>

      {embedded && (
        <div className="config-header-feedback justify-self-center">
          <Button
            aria-describedby="codey-feedback-qr-description"
            aria-label="问题反馈群，悬浮或聚焦查看二维码"
            className="whitespace-nowrap max-[520px]:w-8! max-[520px]:px-0!"
            size="sm"
            variant="brand-outline"
          >
            <IconMessageCircleQuestion aria-hidden="true" />
            <span className="max-[520px]:hidden">问题反馈群</span>
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
              aria-label={status.running ? "重启 Codex" : "Codex 未运行"}
              className="max-[520px]:w-8! max-[520px]:px-0!"
              disabled={isBusy || status.restartInProgress || !status.running}
              onClick={handleRestartCodex}
              size="sm"
              variant="warning"
            >
              {busy === "restart" || status.restartInProgress ? (
                <LoaderCircle className="animate-spin" aria-hidden="true" />
              ) : (
                <RefreshCw aria-hidden="true" />
              )}
              <span className="max-[520px]:hidden">
                {status.running ? "重启 Codex" : "未运行"}
              </span>
            </Button>
          )}
          <Button
            aria-label={dirty ? "保存更改" : "已保存"}
            className="h-8 min-w-[88px] px-3.5 text-xs max-[520px]:min-w-8! max-[520px]:w-8! max-[520px]:px-0!"
            disabled={!dirty || isBusy}
            onClick={handleSaveCurrent}
            size="sm"
            variant={dirty ? "default" : "secondary"}
          >
            {busy === "save" ? (
              <LoaderCircle className="animate-spin" aria-hidden="true" />
            ) : dirty ? (
              <Save aria-hidden="true" />
            ) : (
              <Check aria-hidden="true" />
            )}
            <span className="max-[520px]:hidden">
              {dirty ? "保存更改" : "已保存"}
            </span>
          </Button>
          {embedded && (
            <Button
              aria-label="关闭配置"
              className="flex-none max-[520px]:h-8! max-[520px]:w-8! max-[520px]:p-0!"
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
      <a className="skip-link" href="#codey-settings-content">
        跳至设置内容
      </a>

      {!embedded && (
        <div className="macos-titlebar">
          <div className="macos-traffic-lights">
            <Button
              variant="ghost"
              size="icon"
              className="size-3! min-w-3! rounded-full! border! border-black/15! bg-[#ff5f56]! p-0! shadow-none! hover:opacity-85"
              title="关闭"
              aria-label="关闭窗口"
            />
            <Button
              variant="ghost"
              size="icon"
              className="size-3! min-w-3! rounded-full! border! border-black/15! bg-[#ffbd2e]! p-0! shadow-none! hover:opacity-85"
              title="最小化"
              aria-label="最小化窗口"
            />
            <Button
              variant="ghost"
              size="icon"
              className="size-3! min-w-3! rounded-full! border! border-black/15! bg-[#27c93f]! p-0! shadow-none! hover:opacity-85"
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
        <header className="z-30 flex flex-col border-b border-black/8 bg-white/75 px-5 py-2.5 backdrop-blur-xl">
          {configHeaderContent}
        </header>
      )}

      <div className="page-scroll">
        <div className="page" id="codey-settings-content">
          {/* 最上方：运行状态 (Codex 运行与维护) */}
          <OperationsPanel
            codexAppPath={config.codexAppPath}
            fastContextToolsStatus={fastContextToolsStatus}
            status={operationsStatus}
            busy={busy}
            isBusy={isBusy}
            pluginMarketplaceStatus={pluginMarketplaceStatus}
            onRepairPluginMarketplace={handleRepairPluginMarketplace}
            onRestart={handleRestartCodex}
            showRestartAction={!embedded}
          />

          {/* 线路与模型：单独一行展示 */}
          <div className="full-row-section">
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
              onToggleLocalRouter={handleToggleLocalRouter}
              onToggleRouteRequestLog={handleToggleRouteRequestLog}
              onSaveRoute={handleSaveRoute}
              onDeleteRoute={handleDeleteRoute}
              onFetchRouteModels={handleFetchRouteModels}
              onToggleAccountUsage={handleToggleAccountUsage}
              onSaveOfficialRouteSettings={handleSaveOfficialRouteSettings}
              onSetDefaultModel={handleSetRouteDefaultModel}
            />
          </div>

          {/* 提示词优化 与 Codey 子代理角色与调度增强：放在一行 */}
          <div className="prompt-subagent-grid">
            {/* 左侧：提示词优化 */}
            <div className="prompt-column">
              <PromptOptimizationCard
                config={config}
                isBusy={isBusy}
                popupContainer={popupContainer}
                subagentModelOptions={subagentModelOptions}
                onConfigChange={handleConfigChange}
                onNotice={setNotice}
              />
            </div>

            {/* 右侧：Codey 子代理角色与调度增强 */}
            <div className="subagent-column">
              <SubagentPolicyCard
                config={config}
                popupContainer={popupContainer}
                tooltipContainer={portalContainer}
                isBusy={isBusy}
                subagentModelOptions={subagentModelOptions}
                onConfigChange={handleConfigChange}
                onSubagentOptimizationChange={handleSubagentOptimizationChange}
              />
            </div>
          </div>

          {/* Codex 功能策略：整行排列 */}
          <div className="full-row-section">
            <FeaturePolicyCard
              config={config}
              fastContextToolsStatus={fastContextToolsStatus}
              isMacClient={status.clientPlatform === "macos"}
              isWindowsClient={status.clientPlatform === "windows"}
              popupContainer={popupContainer}
              tooltipContainer={portalContainer}
              isBusy={isBusy}
              onConfigChange={handleConfigChange}
            />
          </div>

          {/* 消息通知：整行排列，每个渠道 item 占一半 */}
          <div className="full-row-section">
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

          {/* 诊断存储保护：整行独占排布 */}
          <div className="full-row-section">
            <TraceLogModule
              stats={status.traceLogStats}
              crashpadStats={status.crashpadPendingStats}
              crashpadSupported={status.clientPlatform === "macos"}
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
      </div>

      <NoticeToast
        autoDismissEnabled
        controller={noticeController}
      />

      <ModelPickerDialog
        open={modelPickerVisible}
        routeConfigReadOnly={config?.localRouterEnabled === false}
        isBusy={isBusy}
        busy={busy}
        container={portalContainer}
        customModelInput={customModelInput}
        modelInputError={modelInputError}
        modelSyncWarning={modelSyncWarning}
        autoReviewSupported={draftAutoReviewSupported}
        thirdPartyModelOptions={thirdPartyModelOptions}
        modelState={modelEditorState}
        draftModelSet={draftModelSet}
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
        <div className="relative z-[2] flex w-full items-center overflow-visible">
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
