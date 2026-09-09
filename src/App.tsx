import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { notification } from "antd";
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
import { ModelPickerDialog } from "./AppDialogs";
import { FeaturePolicyCard, SubagentPolicyCard } from "./FeaturePolicyCard";
import { ModelSection } from "./ModelSection";
import { OperationsPanel } from "./OperationsPanel";
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
import { CodeyBrandMark, SettingsModalShell } from "./SettingsModalShell";
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
import { Badge, Button, Tag, Tooltip } from "./components/antd";

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
  const { status, setStatus, refreshStatus, refreshStatusForLoad,
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
  const [cleanupNotification, cleanupNotificationHolder] = notification.useNotification({
    getContainer: getTooltipContainer,
    placement: "bottomRight",
    duration: 5,
  });
  const confirmationController = useConfirmationController();
  const setNotice = noticeController.setNotice;
  const setConfirmation = confirmationController.setConfirmation;
  useEffect(() => {
    if (restartStatusError) setNotice({ tone: "error", text: restartStatusError });
  }, [restartStatusError, setNotice]);

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
    draft1MModelSet,
    draftModelContexts,
    updateDraftModelContext,
    toggleDraft1MModel,
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

  async function reorderRoute(sourceId: string, targetId: string) {
    if (!config || dirty || isBusy || !config.localRouterEnabled) return;
    const profiles = [...config.profiles];
    const sourceIndex = profiles.findIndex((profile) => profile.id === sourceId);
    const targetIndex = profiles.findIndex((profile) => profile.id === targetId);
    if (sourceIndex < 0 || targetIndex < 0 || sourceIndex === targetIndex) return;
    profiles.splice(targetIndex, 0, profiles.splice(sourceIndex, 1)[0]);
    await runOperation("reorder-routes", async () => {
      await persist({ ...config, profiles });
      setNotice({ tone: "success", text: "线路顺序已保存" });
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
    supports1MContextModels: string[],
    enabled: boolean,
    modelContexts: Record<string, import("./App.types").ModelContextConfig>,
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
      }>("save_official_route_models", {
        routeId,
        models,
        supports1MContextModels,
        modelContexts,
        enabled,
        showAccountUsageInHeader,
      });
      applyRouteResult(modelResult);
      saved = true;
      setNotice({
        tone: modelResult.restartRequired ? "info" : "success",
        text: modelResult.restartRequired
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
      setNotice({ tone: noticeTone, text: noticeText });
    });
  }

  function closeSettings() {
    if (isBusy) return;
    if (persistedConfigRef.current) {
      setConfig(persistedConfigRef.current);
    }
    setDirty(false);
    setModelPickerVisible(false);
    confirmationController.clear();
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

  async function analyzeDiagnosticStorage(target: DiagnosticStorageTarget) {
    await runOperation("clear-diagnostic-storage", async () => {
      const key = "diagnostic-storage-cleanup";
      const title = target === "trace" ? "Trace 日志" : "Crashpad";
      cleanupNotification.info({
        key,
        duration: 0,
        className: "cleanup-notification-card",
        title: `${title}：正在分析并清理`,
        description: "正在统计占用并清理，请稍候…",
      });
      try {
        const result = await invoke<DiagnosticStorageCleanup>("clear_diagnostic_storage", { target });
        setStatus((current) => ({
          ...current,
          traceLogWriteProtectionActive: result.traceLogWriteProtectionActive,
        }));
        const incomplete = result.status === "partial" || result.errors.length > 0;
        cleanupNotification.open({
          key,
          className: "cleanup-notification-card",
          type: incomplete ? "warning" : "success",
          title: `${title}：${incomplete ? "部分项目未完成" : "分析并清理完成"}`,
          description: <DiagnosticCleanupNotice result={result} target={target} />,
        });
      } catch (error) {
        cleanupNotification.error({
          key,
          className: "cleanup-notification-card",
          title: `${title}：清理结果未能确认`,
          description: `无法确认清理前后占用及清理情况：${errorText(error)}`,
        });
      }
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
  const handleReorderRoute = useStableEvent(reorderRoute);
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
            <Tag color="volcano" className="header-version-tag">
              v{status.appVersion || "0.0.1"}
            </Tag>

            <Tooltip
              content={updateTooltipText}
              getPopupContainer={getTooltipContainer}
              position="bottom"
            >
              <span className="header-update-btn-wrap">
                <Button
                  size="xs"
                  className="header-update-btn"
                  variant={downloadedUpdate ? "default" : hasUpdate ? "brand-outline" : "ghost"}
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
                    <span>
                      v{downloadedUpdate.latestVersion} 已下载
                    </span>
                  ) : hasUpdate ? (
                    <span>
                      v{updateCheck?.latestVersion} 可更新
                    </span>
                  ) : null}
                </Button>
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
              className="h-8! whitespace-nowrap px-3.5 text-xs max-[520px]:w-8! max-[520px]:px-0!"
              disabled={isBusy || (!restartStatusError && (status.restartInProgress || !status.running))}
              onClick={handleRestartCodex}
              variant="warning"
            >
              {busy === "restart" || (status.restartInProgress && !restartStatusError) ? (
                <LoaderCircle className="animate-spin" aria-hidden="true" />
              ) : (
                <RefreshCw aria-hidden="true" />
              )}
              <span className="max-[520px]:hidden">
                {restartStatusError ? "重新查询状态" : status.running ? "重启 Codex" : "未运行"}
              </span>
            </Button>
          )}
          <Button
            aria-label={dirty ? "保存更改" : "已保存"}
            className="h-8! min-w-[88px] px-3.5 text-xs max-[520px]:min-w-8! max-[520px]:w-8! max-[520px]:px-0!"
            disabled={!dirty || isBusy}
            onClick={handleSaveCurrent}
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
              className="size-8! flex-none p-0! max-[520px]:size-8! max-[520px]:p-0!"
              disabled={isBusy}
              onClick={handleCloseSettings}
              size="icon"
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
        <header className="z-30 flex flex-col border-b border-black/8 bg-white/75 px-5 py-2.5 backdrop-blur-xl shadow-[0_3px_8px_rgba(0,0,0,0.04),0_1px_2px_rgba(0,0,0,0.02)]">
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
            restartStatusUnknown={Boolean(restartStatusError)}
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
              onReorderRoute={handleReorderRoute}
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
              cleanupBusy={busy === "clear-diagnostic-storage"}
              onAnalyzeDiagnosticStorage={handleAnalyzeDiagnosticStorage}
              popupContainer={popupContainer}
              tooltipContainer={portalContainer}
              isBusy={isBusy}
              onConfigChange={handleConfigChange}
              onAddChannel={handleAddNotificationChannel}
              onChannelChange={handleNotificationChannelChange}
              onRequestRemoveChannel={handleRequestRemoveNotificationChannel}
            />
          </div>

        </div>
      </div>

      {cleanupNotificationHolder}
      <NoticeToast
        autoDismissEnabled
        controller={noticeController}
      />

      <ModelPickerDialog
        open={modelPickerVisible}
        routeConfigReadOnly={config?.localRouterEnabled === false}
        isBusy={isBusy}
        busy={busy}
        container={popupContainer}
        customModelInput={customModelInput}
        modelInputError={modelInputError}
        modelSyncWarning={modelSyncWarning}
        autoReviewSupported={draftAutoReviewSupported}
        thirdPartyModelOptions={thirdPartyModelOptions}
        modelState={modelEditorState}
        draftModelSet={draftModelSet}
        draft1MModelSet={draft1MModelSet}
        draftModelContexts={draftModelContexts}
        onUpdateDraftModelContext={updateDraftModelContext}
        onToggleDraft1MModel={toggleDraft1MModel}
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
