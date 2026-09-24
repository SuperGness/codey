import { memo, useCallback, useEffect, useMemo, useState } from "react";
import {
  IconCheck as Check,
  IconChartDonut,
  IconCpu,
  IconEdit as Edit,
  IconEye,
  IconEyeOff,
  IconFileText,
  IconGripVertical,
  IconHelpCircle,
  IconInfoCircle,
  IconListDetails,
  IconPlus as Plus,
  IconRefresh as RefreshCw,
  IconRoute,
  IconServer as Server,
  IconShieldCheck,
  IconSparkles,
  IconTrash as Trash,
} from "@tabler/icons-react";

import type { Confirmation, Config, ModelContextConfig, ModelState, OfficialAccount, OfficialAccountsResult, Profile, ProviderStatus } from "./App.types";
import { OfficialAccountsPanel } from "./OfficialAccountsPanel";
import { SettingsPageHeader } from "./SettingsPageHeader";
import { ModelCombobox } from "./components/ModelCombobox";
import type { SubagentModelOption } from "./subagentModels";
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  NumberInput,
  PasswordInput,
  Select,
  Switch,
  Tooltip,
} from "./components/ui";
import { modelIdsEqual, modelKey, moveModelId, uniqueModelIds } from "./modelIds";
import {
  MAX_ROUTE_NAME_CHARACTERS,
  validateOfficialRouteSettings,
  type OfficialRouteSettingsDraft,
} from "./officialRouteSettings";
import { headersTextFromMap, parseHeadersText } from "./requestHeaders";
import { globalDefaultForRoute, routeProviderId, sortRoutesByEnabled } from "./modelRoutes";
import { maskEmail, maskUrl } from "./sensitiveText";
import {
  MAX_ROUTE_SHORT_NAME_CHARACTERS,
  validateThirdPartyRouteShortName,
} from "./routeShortNames";
import { validateOutboundApiUrl, validateOutboundProxyUrl } from "./urlValidation";
import { invoke } from "./api";
import { listOfficialAccounts, rememberOfficialAccounts } from "./officialAccountsRequests";
import { readHostTheme } from "./overlayTheme";

type ModelSectionProps = {
  config: Config;
  currentProvider: ProviderStatus["provider"] | null;
  officialAccountAvailable: boolean;
  popupContainer: HTMLElement | null;
  modelState: ModelState;
  dirty: boolean;
  canSyncCurrentProvider: boolean;
  isBusy: boolean;
  busy: string | null;
  showAccountUsageInHeader: boolean;
  subagentModelOptions?: SubagentModelOption[];
  onToggleLocalRouter: (checked: boolean) => void;
  onToggleRouteRequestLog: (checked: boolean) => void;
  onOpenUsageAnalysis: (trigger: HTMLElement) => void;
  onSaveRoute: (route: Profile) => Promise<boolean>;
  onSetRouteEnabled: (routeId: string, enabled: boolean) => Promise<boolean>;
  onReorderRoute: (sourceId: string, targetId: string) => Promise<void>;
  onReorderRouteModels: (routeId: string, models: string[]) => Promise<void>;
  onDeleteRoute: (routeId: string) => void;
  onFetchRouteModels: (route: Profile) => void;
  onOfficialAccountsChanged: (result: OfficialAccountsResult) => void;
  onNotice: (notice: { tone: "success" | "info" | "error"; text: string }) => void;
  onSaveOfficialRouteSettings?: (
    routeId: string,
    models: string[],
    showAccountUsageInHeader: boolean,
    enabled: boolean,
    modelContexts: Record<string, ModelContextConfig>,
    upstreamProxy?: string,
    routeSettings?: {
      accountId: string;
      routeName: string;
      routeShortName: string;
      baseUrl: string;
    },
  ) => Promise<boolean>;
  onSetDefaultModel: (routeId: string, model: string) => void;
  onConfigChange?: (config: Config) => void;
  onRequestConfirmation?: (confirmation: Confirmation) => void;
};

type RouteModelRef = { routeId: string; model: string };

type RouteModelGroup = {
  profile: Profile;
  providerId: string;
  models: string[];
  defaultModel: string;
  official: boolean;
};

function newRouteName(profiles: Profile[]) {
  let index = profiles.length + 1;
  const names = new Set(profiles.map((profile) => profile.name));
  while (names.has(`新线路 ${index}`)) index += 1;
  return `新线路 ${index}`;
}

function createRoute(profiles: Profile[]): Profile {
  const id = `route-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  return {
    id,
    enabled: true,
    name: newRouteName(profiles),
    shortName: "",
    baseUrl: "",
    apiKey: "",
    upstreamProtocol: "openaiResponses",
    authMode: "apiKey",
    apiKeyConfigured: false,
    modelRequestHeaders: {},
    upstreamProxy: "",
    clearApiKey: false,
    officialAccount: false,
    supportsRemoteCompaction: false,
    supportsWebsockets: false,
    supportsNativeWebSearch: false,
  };
}

type RouteDraftErrors = {
  name: string;
  shortName: string;
  baseUrl: string;
  apiKey: string;
  upstreamProxy: string;
};

// 官方线路的编辑入口只改线路名、短名称、网关和代理，模型列举交给同步入口。
type OfficialRouteDialogScope = "settings" | "models";

export { MAX_ROUTE_NAME_CHARACTERS };

const UPSTREAM_PROXY_TOOLTIP_CONTENT = (
  <div className="space-y-1.5 text-xs text-left leading-relaxed">
    <div className="font-semibold">上游代理格式与提示</div>
    <div>
      支持协议：<code className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/10 px-1 py-0.5 font-mono text-[11px] dark:bg-[var(--codey-surface,#fff)]/15">http://</code>、<code className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/10 px-1 py-0.5 font-mono text-[11px] dark:bg-[var(--codey-surface,#fff)]/15">https://</code>、<code className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/10 px-1 py-0.5 font-mono text-[11px] dark:bg-[var(--codey-surface,#fff)]/15">socks5://</code>、<code className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/10 px-1 py-0.5 font-mono text-[11px] dark:bg-[var(--codey-surface,#fff)]/15">socks5h://</code>
    </div>
    <div>
      支持代理认证：允许携带用户名与密码（如 <code className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/10 px-1 py-0.5 font-mono text-[11px] dark:bg-[var(--codey-surface,#fff)]/15">user:pass@host:port</code>）。
    </div>
    <div className="pt-0.5">
      <div className="font-semibold text-[11px] opacity-80">常见示例：</div>
      <div className="mt-0.5 space-y-0.5 font-mono text-[11px] opacity-90">
        <div>http://127.0.0.1:7890</div>
        <div>socks5://127.0.0.1:7890</div>
        <div>http://user:pass@192.168.1.100:8080</div>
      </div>
    </div>
    <div className="border-t border-current/15 pt-1 text-[11px] opacity-80">
      留空使用系统代理；设置后该线路改用流式 HTTP 传输。
    </div>
  </div>
);

function validateRouteDraft(route: Profile, profiles: readonly Profile[]): RouteDraftErrors {
  if (route.authMode === "officialAccount") {
    return {
      name: "",
      shortName: "",
      baseUrl: "",
      apiKey: "",
      upstreamProxy: validateOutboundProxyUrl(route.upstreamProxy || ""),
    };
  }
  const errors: RouteDraftErrors = {
    name: !route.name.trim()
      ? "请输入线路名称"
      : Array.from(route.name.trim()).length > MAX_ROUTE_NAME_CHARACTERS
        ? `线路名最多 ${MAX_ROUTE_NAME_CHARACTERS} 个字符`
        : "",
    shortName: validateThirdPartyRouteShortName(route.shortName, profiles, route.id),
    baseUrl: "",
    apiKey: "",
    upstreamProxy: validateOutboundProxyUrl(route.upstreamProxy || ""),
  };
  errors.baseUrl = validateOutboundApiUrl(route.baseUrl);
  if (route.apiKey.trim() === "" && !route.apiKeyConfigured) errors.apiKey = "请输入 API Key";
  return errors;
}

const routeProtocolOptions: Array<{
  label: string;
  value: Profile["upstreamProtocol"];
}> = [
  { label: "OpenAI Responses", value: "openaiResponses" },
  { label: "OpenAI Chat Completions", value: "openaiChatCompletions" },
  { label: "Anthropic Messages", value: "anthropicMessages" },
];

function ModelSectionComponent({
  config,
  currentProvider,
  officialAccountAvailable,
  popupContainer,
  modelState,
  dirty,
  canSyncCurrentProvider,
  isBusy,
  busy,
  showAccountUsageInHeader,
  subagentModelOptions = [],
  onToggleLocalRouter,
  onToggleRouteRequestLog,
  onOpenUsageAnalysis,
  onSaveRoute,
  onSetRouteEnabled,
  onReorderRoute,
  onReorderRouteModels,
  onDeleteRoute,
  onFetchRouteModels,
  onOfficialAccountsChanged,
  onNotice,
  onSaveOfficialRouteSettings,
  onSetDefaultModel,
  onConfigChange,
  onRequestConfirmation,
}: ModelSectionProps) {
  const [routeDialogOpen, setRouteDialogOpen] = useState(false);
  const [draggedRouteId, setDraggedRouteId] = useState<string | null>(null);
  const [dropRouteId, setDropRouteId] = useState<string | null>(null);
  // 模型只能在所属线路内拖动排序，因此拖动状态同时记录线路和模型。
  const [draggedModel, setDraggedModel] = useState<RouteModelRef | null>(null);
  const [dropModel, setDropModel] = useState<RouteModelRef | null>(null);
  const [pendingRouteToggle, setPendingRouteToggle] = useState<{
    id: string;
    enabled: boolean;
  } | null>(null);
  const [routeDraft, setRouteDraft] = useState<Profile | null>(null);
  const [routeValidationAttempted, setRouteValidationAttempted] = useState(false);
  const [routeApiKeyVisible, setRouteApiKeyVisible] = useState(false);
  const [routeHeadersText, setRouteHeadersText] = useState("{}");
  const [headerDialogProfile, setHeaderDialogProfile] = useState<Profile | null>(null);
  const [headerError, setHeaderError] = useState("");
  const [officialModelDraft, setOfficialModelDraft] = useState<string[]>([]);
  const [officialAccounts, setOfficialAccounts] = useState<OfficialAccount[] | null>(null);
  // 脱敏只作用于当前页面显示，每次进入页面默认关闭。
  const [maskSensitive, setMaskSensitive] = useState(false);
  const [officialRouteDraft, setOfficialRouteDraft] =
    useState<OfficialRouteSettingsDraft | null>(null);
  const [officialDialogScope, setOfficialDialogScope] =
    useState<OfficialRouteDialogScope | null>(null);
  const routeConfigReadOnly = !config.localRouterEnabled;

  useEffect(() => {
    if (!routeConfigReadOnly) return;
    setRouteDialogOpen(false);
    setRouteDraft(null);
    setOfficialRouteDraft(null);
    setOfficialDialogScope(null);
    setRouteApiKeyVisible(false);
  }, [routeConfigReadOnly]);

  // 官方线路的线路名、短名称、网关和代理保存在所属账号记录里，保存入口在线路卡片上。
  const refreshOfficialAccounts = useCallback(async () => {
    try {
      const result = await listOfficialAccounts();
      setOfficialAccounts(result.accounts ?? []);
    } catch {
      setOfficialAccounts(null);
    }
  }, []);

  const handleOfficialAccountsChanged = useCallback(
    (result: OfficialAccountsResult) => {
      if (Array.isArray(result.accounts)) {
        rememberOfficialAccounts(result);
        setOfficialAccounts(result.accounts);
      }
      onOfficialAccountsChanged(result);
    },
    [onOfficialAccountsChanged],
  );

  // 账号面板只在开启本地路由时挂载，只读模式下线路卡片仍要显示所属账号邮箱。
  useEffect(() => {
    if (!routeConfigReadOnly) return;
    void refreshOfficialAccounts();
  }, [refreshOfficialAccounts, routeConfigReadOnly]);

  const defaultOfficialAccount = useMemo(
    () => officialAccounts?.find((account) => account.isDefault) ?? null,
    [officialAccounts],
  );
  // 每条官方线路对应一个账号记录，线路卡片和编辑弹窗都显示所属账号。
  const accountForRoute = useCallback(
    (profile: Profile | null | undefined): OfficialAccount | null => {
      if (!profile) return null;
      if (profile.officialAccountId) {
        return officialAccounts?.find((account) => account.id === profile.officialAccountId) ?? null;
      }
      return defaultOfficialAccount;
    },
    [defaultOfficialAccount, officialAccounts],
  );
  const displayedEmail = useCallback(
    (account: OfficialAccount | null) => {
      const email = account?.email?.trim();
      if (!email) return "";
      return maskSensitive ? maskEmail(email) : email;
    },
    [maskSensitive],
  );
  const officialLoginLabelFor = useCallback(
    (account: OfficialAccount | null) => {
      const email = displayedEmail(account);
      return email ? `官方账号登录 · ${email}` : "官方账号登录";
    },
    [displayedEmail],
  );
  const hideUrl = useCallback(
    (value: string) => (maskSensitive ? maskUrl(value) : value),
    [maskSensitive],
  );

  const nativeProfile = useMemo<Profile | null>(() => {
    if (!routeConfigReadOnly || !currentProvider) return null;
    const matchingProfile = config.profiles.find(
      (profile) =>
        profile.id === currentProvider.id ||
        routeProviderId(profile) === currentProvider.id,
    );
    const official = currentProvider.official;
    if (matchingProfile?.enabled === false) return null;
    return {
      id: matchingProfile?.id || currentProvider.id,
      name:
        currentProvider.name.trim() ||
        matchingProfile?.name.trim() ||
        currentProvider.id,
      shortName: matchingProfile?.shortName || (official ? "官" : ""),
      baseUrl: currentProvider.baseUrl || matchingProfile?.baseUrl || "",
      apiKey: "",
      upstreamProtocol:
        matchingProfile?.upstreamProtocol ||
        (official ? "official" : "openaiResponses"),
      authMode: official ? "officialAccount" : "apiKey",
      apiKeyConfigured: matchingProfile?.apiKeyConfigured === true,
      sourceProviderId: currentProvider.id,
      officialAccount: official,
      supportsRemoteCompaction: matchingProfile?.supportsRemoteCompaction,
      supportsWebsockets: matchingProfile?.supportsWebsockets,
      supportsNativeWebSearch: matchingProfile?.supportsNativeWebSearch,
      supportsAutoReview: matchingProfile?.supportsAutoReview,
      modelRequestHeaders: matchingProfile?.modelRequestHeaders || {},
      upstreamProxy: matchingProfile?.upstreamProxy || "",
    };
  }, [config.profiles, currentProvider, routeConfigReadOnly]);
  const visibleProfiles = useMemo(
    () => {
      if (routeConfigReadOnly) return nativeProfile ? [nativeProfile] : [];
      const profiles = config.profiles.filter(
        (profile) =>
          profile.enabled === false ||
          profile.authMode !== "officialAccount" ||
          officialAccountAvailable ||
          // 存储账号的官方线路自带凭据，默认登录缺失时仍由本地路由提供服务。
          Boolean(profile.officialAccountId),
      );
      return sortRoutesByEnabled(pendingRouteToggle
        ? profiles.map((profile) => profile.id === pendingRouteToggle.id
          ? { ...profile, enabled: pendingRouteToggle.enabled }
          : profile)
        : profiles);
    },
    [config.profiles, nativeProfile, officialAccountAvailable, routeConfigReadOnly, pendingRouteToggle],
  );
  const officialDisplayNames = useMemo(
    () =>
      new Map(
        modelState.officialModels.map((model) => [
          modelKey(model.slug),
          model.displayName,
        ]),
      ),
    [modelState.officialModels],
  );
  const officialCatalog = useMemo(
    () =>
      uniqueModelIds([
        ...modelState.officialModelIds,
        ...modelState.officialModels.map((model) => model.slug),
      ]),
    [modelState.officialModelIds, modelState.officialModels],
  );
  const officialModelDraftKeys = useMemo(
    () => new Set(officialModelDraft.map(modelKey)),
    [officialModelDraft],
  );
  const modelGroups = useMemo<RouteModelGroup[]>(
    () => {
      const nativeOfficialModels = routeConfigReadOnly
        ? modelState.officialModels.filter((model) => model.supported).map((model) => model.slug)
        : [];
      return visibleProfiles.filter((profile) => profile.enabled !== false).map((profile) => {
        const providerId = routeProviderId(profile);
        const official = profile.authMode === "officialAccount";
        const configuredModels = config.selectedModelsByProvider[providerId] || [];
        const models = routeConfigReadOnly
          ? official
            ? uniqueModelIds(
                nativeOfficialModels.length > 0
                  ? nativeOfficialModels
                  : modelState.officialModelIds,
              )
            : modelState.thirdPartyModels
            : official
              ? uniqueModelIds(
                  configuredModels.length > 0 ? configuredModels : officialCatalog,
                )
            : uniqueModelIds([
                ...configuredModels,
                ...(config.declaredOfficialModelsByProvider[providerId] || []),
              ]);
        return {
          profile,
          providerId,
          models,
          defaultModel: routeConfigReadOnly
            ? modelState.defaultModel
            : globalDefaultForRoute(config, profile, models),
          official,
        };
      });
    },
    [config, modelState, officialCatalog, routeConfigReadOnly, visibleProfiles],
  );
  const modelGroupByProviderId = useMemo(
    () => new Map(modelGroups.map((group) => [group.providerId, group])),
    [modelGroups],
  );

  const totalModelCount = useMemo(
    () => modelGroups.reduce((count, group) => count + group.models.length, 0),
    [modelGroups],
  );
  const routeDraftErrors = useMemo(
    () => routeDraft ? validateRouteDraft(routeDraft, config.profiles) : null,
    [config.profiles, routeDraft],
  );
  const routeDraftHasErrors = Boolean(
    routeDraftErrors && Object.values(routeDraftErrors).some(Boolean),
  );
  const officialRouteDraftErrors = useMemo(
    () =>
      officialRouteDraft
        ? validateOfficialRouteSettings(
            officialRouteDraft,
            config.profiles,
            officialAccounts,
            routeDraft?.officialAccountId ?? defaultOfficialAccount?.id ?? "",
          )
        : null,
    [config.profiles, defaultOfficialAccount, officialAccounts, officialRouteDraft, routeDraft],
  );

  const openNewRouteDialog = () => {
    setRouteDraft(createRoute(config.profiles));
    setRouteValidationAttempted(false);
    setRouteApiKeyVisible(false);
    setRouteHeadersText(JSON.stringify({}, null, 2));
    setHeaderError("");
    setOfficialModelDraft([]);
    setOfficialDialogScope(null);
    setRouteDialogOpen(true);
  };
  const openRouteDialog = (
    profile: Profile,
    officialScope: OfficialRouteDialogScope | null = null,
  ) => {
    const official = profile.authMode === "officialAccount";
    setRouteDraft({ ...profile });
    setRouteValidationAttempted(false);
    setRouteApiKeyVisible(false);
    setRouteHeadersText(headersTextFromMap(profile.modelRequestHeaders));
    setHeaderError("");
    const routeAccount = official ? accountForRoute(profile) : null;
    setOfficialRouteDraft(
      official && officialScope === "settings"
        ? {
            routeName: routeAccount?.routeName ?? "",
            routeShortName: routeAccount?.routeShortName ?? "",
            baseUrl: routeAccount?.baseUrl ?? "",
            upstreamProxy: profile.upstreamProxy ?? "",
          }
        : null,
    );
    if (official && officialScope === "models") {
      const providerId = routeProviderId(profile);
      const configuredModels = config.selectedModelsByProvider[providerId] || [];
      const catalogKeys = new Set(officialCatalog.map(modelKey));
      const enabledModels = configuredModels.filter((model) => catalogKeys.has(modelKey(model)));
      setOfficialModelDraft(
        uniqueModelIds(enabledModels.length > 0 ? enabledModels : officialCatalog),
      );
    } else {
      setOfficialModelDraft([]);
    }
    setOfficialDialogScope(official ? officialScope : null);
    setRouteDialogOpen(true);
  };
  const updateRouteDraft = (patch: Partial<Profile>) => {
    setRouteDraft((current) => current ? { ...current, ...patch } : current);
  };
  const updateOfficialRouteDraft = (patch: Partial<OfficialRouteSettingsDraft>) => {
    setOfficialRouteDraft((current) => current ? { ...current, ...patch } : current);
  };
  const openHeadersDialog = (profile: Profile) => {
    setHeaderDialogProfile(profile);
    setRouteHeadersText(headersTextFromMap(profile.modelRequestHeaders));
    setHeaderError("");
  };
  const saveHeaders = async () => {
    if (!headerDialogProfile) return;
    let modelRequestHeaders: Record<string, string>;
    try {
      modelRequestHeaders = parseHeadersText(routeHeadersText);
    } catch (error) {
      setHeaderError((error as Error).message);
      return;
    }
    setHeaderError("");
    if (!await onSaveRoute({ ...headerDialogProfile, modelRequestHeaders })) return;
    setHeaderDialogProfile(null);
  };
  const toggleRouteApiKeyVisibility = () => {
    setRouteApiKeyVisible((visible) => !visible);
  };
  const saveRouteDraft = async () => {
    if (!routeDraft) return;
    let modelRequestHeaders: Record<string, string>;
    try {
      modelRequestHeaders = parseHeadersText(routeHeadersText);
    } catch (error) {
      setHeaderError((error as Error).message);
      setRouteValidationAttempted(true);
      return;
    }
    if (routeDraft.authMode === "officialAccount") {
      if (officialDialogScope === "models") {
        if (officialModelDraft.length === 0) return;
      } else if (officialRouteDraftErrors && Object.values(officialRouteDraftErrors).some(Boolean)) {
        setRouteValidationAttempted(true);
        requestAnimationFrame(() => {
          const firstInvalid = document.querySelector<HTMLInputElement>(
            ".official-route-editor [aria-invalid='true']",
          );
          firstInvalid?.focus();
        });
        return;
      }
    } else if (routeDraftHasErrors) {
      setRouteValidationAttempted(true);
      requestAnimationFrame(() => {
        const firstInvalid = document.querySelector<HTMLInputElement>(
          ".route-editor-form [aria-invalid='true']",
        );
        firstInvalid?.focus();
      });
      return;
    }
    const accountId = routeDraft.officialAccountId ?? defaultOfficialAccount?.id ?? "";
    const officialProviderId = routeProviderId(routeDraft);
    const configuredOfficialModels = config.selectedModelsByProvider[officialProviderId] || [];
    const currentOfficialModels = configuredOfficialModels.length > 0
      ? configuredOfficialModels
      : officialCatalog;
    const savingOfficialSettings =
      routeDraft.authMode === "officialAccount" && officialDialogScope === "settings";
    // 同步入口只改模型，代理等设置保持现状，因此不传代理值。
    const upstreamProxy = savingOfficialSettings
      ? officialRouteDraft?.upstreamProxy ?? routeDraft.upstreamProxy ?? ""
      : undefined;
    const saved = routeDraft.authMode === "officialAccount"
      ? (onSaveOfficialRouteSettings
          ? await onSaveOfficialRouteSettings(
              routeDraft.id,
              officialDialogScope === "models" ? officialModelDraft : currentOfficialModels,
              showAccountUsageInHeader,
              routeDraft.enabled !== false,
              {},
              upstreamProxy,
              savingOfficialSettings && accountId && officialRouteDraft
                ? {
                    accountId,
                    routeName: officialRouteDraft.routeName.trim(),
                    routeShortName: officialRouteDraft.routeShortName.trim(),
                    baseUrl: officialRouteDraft.baseUrl.trim(),
                  }
                : undefined,
            )
          : true)
      : await onSaveRoute({ ...routeDraft, modelRequestHeaders });
    if (saved) {
      if (routeDraft.authMode === "officialAccount") void refreshOfficialAccounts();
      setRouteDialogOpen(false);
      setRouteDraft(null);
      setOfficialRouteDraft(null);
      setOfficialDialogScope(null);
    }
  };

  const handleToggleRouteEnabled = async (profile: Profile, enabled: boolean) => {
    if (isBusy || dirty || routeConfigReadOnly || pendingRouteToggle) return;
    setPendingRouteToggle({ id: profile.id, enabled });
    try {
      await onSetRouteEnabled(profile.id, enabled);
    } finally {
      // 待保存状态仅用于显示，失败后自然恢复后端配置，避免改动设置草稿。
      setPendingRouteToggle(null);
    }
  };

  const draftOfficialAccount = accountForRoute(routeDraft);
  const draggedProfile = draggedRouteId
    ? visibleProfiles.find((profile) => profile.id === draggedRouteId)
    : undefined;
  const draftOfficialAccountLabel =
    displayedEmail(draftOfficialAccount) || draftOfficialAccount?.id || "";

  const preferredProfile =
    config.profiles.find((profile) => profile.id === config.activeProfileId) ??
    config.profiles[0];
  const preferredProviderId = preferredProfile
    ? routeProviderId(preferredProfile)
    : undefined;
  const miscModelDisabled = isBusy || subagentModelOptions.length === 0;
  const selectedMiscModelKey = config.miscModel.trim().toLowerCase();
  const miscModelUnavailable =
    selectedMiscModelKey !== "" &&
    !subagentModelOptions.some(
      (option) => option.value.toLowerCase() === selectedMiscModelKey,
    );

  const miscModelTooltip = (
    <div className="flex flex-col gap-1 text-xs leading-relaxed max-w-[360px]">
      <div className="font-semibold text-foreground">
        统一指定会话命名、Git 提交消息、环境建议与自动复核回退使用的模型
      </div>
      <div className="text-muted">
        留空时沿用默认选择：会话命名优先使用官方 Luna，第三方线路使用已启用的 Luna 或默认模型；Git 提交消息和环境建议沿用内置模型。选择模型后，这些功能及自动复核回退使用所选模型；只要任一线路声明支持 codex-auto-review，自动复核仍使用专用模型。自动复核回退在保存后生效，其余功能需要重启 Codex，且依赖当前版本的主进程补丁支持。
      </div>
      {miscModelUnavailable && (
        <div className="text-warning font-medium">
          当前选择不在可用模型列表中，请重新选择；保存后若仍无法解析，将沿用默认行为，自动复核回退不可用。
        </div>
      )}
      {!config.localRouterEnabled && (
        <div className="text-muted italic">
          本地路由已关闭，自动复核回退不会生效。
        </div>
      )}
    </div>
  );

  return (
    <section className="route-section" aria-labelledby="route-title">
      <SettingsPageHeader
        id="route-title"
        title="线路与模型"
        icon={<IconRoute size={15} />}
        badge={
          <button
            type="button"
            className="route-mask-toggle-btn"
            aria-label={maskSensitive ? "显示线路 URL 与邮箱" : "隐藏线路 URL 与邮箱"}
            title={maskSensitive ? "显示线路 URL 与邮箱" : "隐藏线路 URL 与邮箱"}
            onClick={() => setMaskSensitive((previous) => !previous)}
          >
            {maskSensitive ? (
              <IconEyeOff size={16} aria-hidden="true" />
            ) : (
              <IconEye size={16} aria-hidden="true" />
            )}
          </button>
        }
        description={routeConfigReadOnly
          ? "查看 Codex 当前线路并同步原始模型目录"
          : "统一管理供应商线路与模型目录"}
        actions={
          <div className="route-header-controls">
            <div className="route-header-switch-item">
              <span className="route-header-switch-label">本地路由</span>
              <Switch
                size="sm"
                checked={config.localRouterEnabled}
                disabled={isBusy}
                onCheckedChange={onToggleLocalRouter}
                aria-label="启用本地路由"
              />
            </div>
            {config.localRouterEnabled && (
              <>
                <span className="route-header-divider" aria-hidden="true" />
                <div className="route-header-switch-item">
                  <span className="route-header-switch-label">日志记录</span>
                  <Switch
                    size="sm"
                    checked={config.routeRequestLog.enabled}
                    disabled={isBusy}
                    onCheckedChange={onToggleRouteRequestLog}
                    aria-label="开启请求日志记录"
                  />
                </div>
                <span className="route-header-divider" aria-hidden="true" />
                <div className="route-header-btn-group">
                  <Button
                    color="primary"
                    variant="filled"
                    size="sm"
                    onClick={(event) => {
                      if (event.currentTarget instanceof HTMLElement) onOpenUsageAnalysis(event.currentTarget);
                    }}
                  >
                    <IconChartDonut size={14} aria-hidden="true" />
                    <span>用量分析</span>
                  </Button>
                  <Button
                    color="primary"
                    variant="filled"
                    size="sm"
                    onClick={() => void invoke("open_route_request_logs", { theme: readHostTheme() })}
                  >
                    <IconFileText size={14} aria-hidden="true" />
                    <span>查看请求日志</span>
                  </Button>
                </div>
              </>
            )}
          </div>
        }
      />

      <div className="route-content">
        <div className={`route-manager${routeConfigReadOnly ? " route-manager-current" : ""}`}>
          <div className="route-catalog-pane">
            {!routeConfigReadOnly && (
              <OfficialAccountsPanel
                officialAccountAvailable={officialAccountAvailable}
                isBusy={isBusy}
                maskSensitive={maskSensitive}
                popupContainer={popupContainer}
                onAccountsLoaded={setOfficialAccounts}
                onAccountsChanged={handleOfficialAccountsChanged}
                onNotice={onNotice}
                onRequestConfirmation={onRequestConfirmation}
              />
            )}

            <div className="catalog-aggregate-heading">
              <div className="catalog-aggregate-title-wrap">
                <div className="catalog-aggregate-title">
                  <strong>{routeConfigReadOnly ? "当前线路模型" : "供应商与模型"}</strong>
                  <Badge variant="info">
                    {visibleProfiles.length} 条线路 · {totalModelCount} 个模型
                  </Badge>
                </div>
                <small>
                  {routeConfigReadOnly
                    ? "模型请求由 Codex 当前 Provider 直接处理"
                    : "点击模型设为全局默认；拖动线路或模型左侧手柄调整顺序"}
                </small>
              </div>
              {!routeConfigReadOnly && (
                <Button
                  size="sm"
                  variant="default"
                  disabled={isBusy || dirty}
                  onClick={openNewRouteDialog}
                >
                  <Plus size={13} strokeWidth={2.2} aria-hidden="true" />
                  <span>新增线路</span>
                </Button>
              )}
            </div>

            <div id="provider-model-groups" className="provider-model-groups" role="region" aria-label="供应商与模型列表" tabIndex={0}>
              {visibleProfiles.length === 0 && (
                <div className="provider-model-empty">
                  <div className="provider-empty-content">
                    <IconCpu size={16} className="provider-empty-icon" aria-hidden="true" />
                    <span>{routeConfigReadOnly ? "尚未读取到 Codex 当前线路" : "暂无线路，添加后即可配置模型"}</span>
                  </div>
                </div>
              )}
              {visibleProfiles.map((profile) => {
                const providerId = routeProviderId(profile);
                const group = modelGroupByProviderId.get(providerId);
                const isOfficial = profile.authMode === "officialAccount";
                const disabled = profile.enabled === false;
                const acceptsRouteDrop = draggedProfile && draggedProfile.id !== profile.id
                  && (draggedProfile.enabled === false) === disabled;
                const officialLoginLabel = officialLoginLabelFor(
                  isOfficial ? accountForRoute(profile) : null,
                );
                const syncModels = () => onFetchRouteModels(profile);
                return (
                  <section
                    className={`provider-model-group${disabled ? " is-disabled" : ""}${dropRouteId === profile.id ? " is-drop-target" : ""}`}
                    key={profile.id}
                    aria-labelledby={`provider-model-${profile.id}`}
                    onDragOver={(event) => {
                      if (routeConfigReadOnly || !acceptsRouteDrop || isBusy || dirty) return;
                      event.preventDefault();
                      event.dataTransfer.dropEffect = "move";
                      setDropRouteId(profile.id);
                    }}
                    onDrop={(event) => {
                      event.preventDefault();
                      if (!routeConfigReadOnly && !isBusy && !dirty && draggedRouteId && acceptsRouteDrop) {
                        void onReorderRoute(draggedRouteId, profile.id);
                      }
                      setDraggedRouteId(null);
                      setDropRouteId(null);
                    }}
                  >
                    <div className="provider-card-header">
                      <div className="provider-heading-main">
                        {!routeConfigReadOnly && (
                          <button
                            type="button"
                            className="route-item-drag-handle cursor-grab text-gray-400 dark:text-gray-400 hover:text-gray-600 active:cursor-grabbing disabled:cursor-default"
                            disabled={isBusy || dirty}
                            draggable={!isBusy && !dirty}
                            aria-label={`调整线路 ${profile.name} 的顺序`}
                            title="在相同启用状态的线路间拖动排序，也可按上下方向键调整"
                            onDragStart={(event) => {
                              event.dataTransfer.setData("text/plain", profile.id);
                              event.dataTransfer.effectAllowed = "move";
                              setDraggedRouteId(profile.id);
                            }}
                            onDragEnd={() => {
                              setDraggedRouteId(null);
                              setDropRouteId(null);
                            }}
                            onKeyDown={(event) => {
                              if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
                              event.preventDefault();
                              const index = visibleProfiles.findIndex((route) => route.id === profile.id);
                              const target = visibleProfiles[index + (event.key === "ArrowUp" ? -1 : 1)];
                              if (target && (target.enabled === false) === disabled) void onReorderRoute(profile.id, target.id);
                            }}
                          >
                            <IconGripVertical size={15} aria-hidden="true" />
                          </button>
                        )}
                        <div className={`provider-avatar-pill ${isOfficial ? "official" : "custom"}`} aria-hidden="true">
                          {isOfficial ? <IconShieldCheck size={14} /> : <Server size={14} />}
                        </div>
                        <div className="provider-heading-text">
                          <div className="provider-heading-title-row">
                            {!routeConfigReadOnly && (
                              <span
                                title={disabled ? `点击启用线路「${profile.name}」` : `点击停用线路「${profile.name}」`}
                                className="provider-route-toggle-wrap"
                              >
                                <Switch
                                  size="xs"
                                  checked={!disabled}
                                  disabled={isBusy || dirty || pendingRouteToggle !== null}
                                  aria-busy={pendingRouteToggle?.id === profile.id}
                                  onCheckedChange={(checked) => void handleToggleRouteEnabled(profile, checked)}
                                  aria-label={`${disabled ? "启用" : "停用"}线路 ${profile.name}`}
                                  className="route-status-switch"
                                />
                              </span>
                            )}
                            <strong id={`provider-model-${profile.id}`} title={profile.name}>{profile.name || "未命名线路"}</strong>
                            <div className="route-item-badges">
                              {pendingRouteToggle?.id === profile.id && <Badge variant="secondary">保存中…</Badge>}
                              {disabled ? <Badge variant="destructive">已禁用</Badge> : (
                                <>
                                  <Badge variant="info">{group?.models.length || 0} 模型</Badge>
                                  {!routeConfigReadOnly && !isOfficial && !group?.models.length && (
                                    <Badge variant="secondary">待配置模型</Badge>
                                  )}
                                  {(isOfficial || profile.supportsWebsockets) && <Badge variant="brand">WS</Badge>}
                                </>
                              )}
                              {disabled && <span className="route-disabled-hint">启用后可使用此线路的模型</span>}
                            </div>
                          </div>
                          <small
                            title={isOfficial ? officialLoginLabel : hideUrl(profile.baseUrl)}
                          >
                            {isOfficial
                              ? officialLoginLabel
                              : profile.baseUrl
                                ? hideUrl(profile.baseUrl)
                                : "待填写 URL"}
                          </small>
                        </div>
                      </div>

                      <div className="provider-card-toolbar">
                        {!disabled && (
                          <Button
                            color="primary"
                            variant="filled"
                            size="xs"
                            disabled={!canSyncCurrentProvider || isBusy}
                            onClick={syncModels}
                            aria-label={`同步 ${profile.name} 模型`}
                            title={`同步 ${profile.name} 模型`}
                          >
                            <RefreshCw size={12} className={busy === "fetch-route-models" && (routeConfigReadOnly || profile.id === config.activeProfileId) ? "animate-spin" : ""} aria-hidden="true" />
                            <span>同步</span>
                          </Button>
                        )}
                        {!routeConfigReadOnly && (
                          <div className="route-item-manage-actions">
                            <Button
                              variant="link"
                              color="primary"
                              size="icon-sm"
                              disabled={isBusy || dirty}
                              onClick={() => openHeadersDialog(profile)}
                              aria-label={`编辑线路 ${profile.name} 的请求头`}
                              title="编辑上游请求头"
                            >
                              <IconListDetails size={14} aria-hidden="true" />
                            </Button>
                            <Button
                              variant="link"
                              color="primary"
                              size="icon-sm"
                              disabled={isBusy || dirty}
                              onClick={() =>
                                openRouteDialog(profile, isOfficial ? "settings" : null)}
                              aria-label={`编辑线路 ${profile.name}`}
                              title={`编辑线路 ${profile.name}`}
                            >
                              <Edit size={14} aria-hidden="true" />
                            </Button>
                            {!isOfficial && (
                              <Button
                                variant="link"
                                color="danger"
                                size="icon-sm"
                                disabled={routeConfigReadOnly || isBusy || dirty || config.profiles.length <= 1}
                                onClick={() => onDeleteRoute(profile.id)}
                                aria-label={`删除线路 ${profile.name}`}
                                title={config.profiles.length <= 1 ? "至少需要保留一条线路" : `删除线路 ${profile.name}`}
                              >
                                <Trash size={14} aria-hidden="true" />
                              </Button>
                            )}
                          </div>
                        )}
                      </div>
                    </div>

                    <div className="provider-card-body">
                      {group && (group.models.length > 0 ? (
                        <div className="provider-model-tags">
                          {group.models.map((model) => {
                            const isDefault = !routeConfigReadOnly && modelIdsEqual(group.defaultModel, model);
                            const displayName = group.official ? officialDisplayNames.get(modelKey(model)) || model : model;
                            const isDraggedModel = draggedModel?.routeId === profile.id
                              && modelIdsEqual(draggedModel.model, model);
                            const acceptsModelDrop = Boolean(draggedModel)
                              && draggedModel?.routeId === profile.id
                              && !isDraggedModel;
                            const isModelDropTarget = acceptsModelDrop
                              && dropModel?.routeId === profile.id
                              && modelIdsEqual(dropModel.model, model);
                            const reorderModels = (source: string, target: string) => {
                              const models = moveModelId(group.models, source, target);
                              if (models) void onReorderRouteModels(profile.id, models);
                            };
                            return (
                              <div
                                key={`${group.providerId}:${model}`}
                                className={`model-tag-item${isModelDropTarget ? " is-drop-target" : ""}`}
                                onDragOver={(event) => {
                                  if (!acceptsModelDrop || isBusy || dirty) return;
                                  event.preventDefault();
                                  event.dataTransfer.dropEffect = "move";
                                  setDropModel({ routeId: profile.id, model });
                                }}
                                onDrop={(event) => {
                                  if (!draggedModel) return;
                                  event.preventDefault();
                                  if (acceptsModelDrop && !isBusy && !dirty) {
                                    reorderModels(draggedModel.model, model);
                                  }
                                  setDraggedModel(null);
                                  setDropModel(null);
                                }}
                              >
                                {!routeConfigReadOnly && (
                                  <button
                                    type="button"
                                    className="model-tag-drag-handle cursor-grab text-gray-400 dark:text-gray-400 hover:text-gray-600 active:cursor-grabbing disabled:cursor-default"
                                    disabled={isBusy || dirty}
                                    draggable={!isBusy && !dirty}
                                    aria-label={`调整模型 ${displayName} 的顺序`}
                                    title="在同一线路内拖动排序，也可按方向键调整"
                                    onDragStart={(event) => {
                                      event.dataTransfer.setData("text/plain", model);
                                      event.dataTransfer.effectAllowed = "move";
                                      setDraggedModel({ routeId: profile.id, model });
                                    }}
                                    onDragEnd={() => {
                                      setDraggedModel(null);
                                      setDropModel(null);
                                    }}
                                    onKeyDown={(event) => {
                                      const step = event.key === "ArrowLeft" || event.key === "ArrowUp"
                                        ? -1
                                        : event.key === "ArrowRight" || event.key === "ArrowDown"
                                          ? 1
                                          : 0;
                                      if (step === 0) return;
                                      event.preventDefault();
                                      const index = group.models.findIndex((candidate) => modelIdsEqual(candidate, model));
                                      const target = group.models[index + step];
                                      if (target) reorderModels(model, target);
                                    }}
                                  >
                                    <IconGripVertical size={13} aria-hidden="true" />
                                  </button>
                                )}
                                <button
                                  type="button"
                                  className={`model-tag-pill${isDefault ? " is-default" : ""}`}
                                  disabled={routeConfigReadOnly || isBusy || dirty || isDefault}
                                  onClick={() => onSetDefaultModel(profile.id, model)}
                                  title={routeConfigReadOnly ? displayName : isDefault ? `${displayName}（当前默认模型）` : `点击设为默认模型：${displayName}`}
                                  aria-label={routeConfigReadOnly ? displayName : isDefault ? `${displayName}，当前默认模型` : `设 ${displayName} 为默认模型`}
                                >
                                  <span className="model-tag-indicator" aria-hidden="true">
                                    {isDefault ? <Check size={11} strokeWidth={2.5} /> : <span className="model-tag-dot" />}
                                  </span>
                                  <span className="model-tag-name">{displayName}</span>
                                  {isDefault && <span className="model-tag-badge">默认</span>}
                                </button>
                              </div>
                            );
                          })}
                        </div>
                      ) : (
                        <div className="provider-model-empty">
                          <div className="provider-empty-content">
                            <IconCpu size={16} className="provider-empty-icon" aria-hidden="true" />
                            <span>尚未配置模型</span>
                          </div>
                          <Button
                            variant="secondary"
                            size="xs"
                            disabled={!canSyncCurrentProvider || isBusy}
                            onClick={syncModels}
                          >
                            <Plus size={12} aria-hidden="true" />
                            <span>{routeConfigReadOnly ? "同步模型" : isOfficial ? "配置官方模型" : "同步或手动添加"}</span>
                          </Button>
                        </div>
                      ))}
                    </div>
                  </section>
                );
              })}
            </div>
          </div>
        </div>

        <div className="route-auxiliary-bar">
          <div className="route-auxiliary-header">
            <div className="route-auxiliary-title-wrap">
              <span className="route-auxiliary-title">高级路由与重试设置</span>
              <small className="route-auxiliary-subtitle">配置辅助任务专用模型与长会话中断后的自动恢复策略</small>
            </div>
          </div>
          <div className="route-auxiliary-grid">
            <div className="route-auxiliary-misc">
              <Tooltip content={miscModelTooltip} position="top">
                <span className="route-auxiliary-label cursor-help">
                  <IconSparkles size={14} className="route-auxiliary-icon" aria-hidden="true" />
                  <strong>杂事模型</strong>
                  <IconInfoCircle size={13} className="route-auxiliary-help" aria-hidden="true" />
                </span>
              </Tooltip>
              <div className="route-auxiliary-combobox">
                <ModelCombobox
                  aria-label="杂事模型"
                  value={config.miscModel}
                  placeholder={
                    subagentModelOptions.length === 0
                      ? "所有线路均暂无模型"
                      : "请选择模型"
                  }
                  disabled={miscModelDisabled}
                  options={subagentModelOptions}
                  preferredProviderId={preferredProviderId}
                  onChange={(value) => {
                    if (!subagentModelOptions.some((option) => option.value === value)) {
                      return;
                    }
                    onConfigChange?.({ ...config, miscModel: value });
                  }}
                />
              </div>
              <Button
                variant="ghost"
                size="sm"
                disabled={isBusy || config.miscModel.trim() === ""}
                onClick={() => onConfigChange?.({ ...config, miscModel: "" })}
                className="route-misc-model-reset"
              >
                恢复默认
              </Button>
            </div>

            <div className="route-auxiliary-retry">
              <div className="local-router-toggle route-retry-toggle">
                <Tooltip content="流式会话中断后自动重新连接的次数，保存并重启 Codex 后生效" position="top">
                  <span className="route-retry-label cursor-help">
                    <strong>会话重试</strong>
                    <IconInfoCircle size={13} className="route-auxiliary-help" aria-hidden="true" />
                  </span>
                </Tooltip>
                <NumberInput
                  size="sm"
                  value={config.streamMaxRetries}
                  minValue={0}
                  maxValue={100}
                  disabled={isBusy}
                  onChange={(value) => {
                    if (Number.isInteger(value) && value >= 0 && value <= 100 && value !== config.streamMaxRetries) {
                      onConfigChange?.({ ...config, streamMaxRetries: value });
                    }
                  }}
                  aria-label="会话错误重试次数"
                />
              </div>
            </div>
          </div>
        </div>

        <div className="readonly-note">
          <IconInfoCircle size={14} className="readonly-note-icon" aria-hidden="true" />
          <span className="readonly-note-text">
            {routeConfigReadOnly
              ? "本地路由已关闭；仅展示 Codex 当前线路，可同步模型，线路地址、密钥和协议保持只读"
              : "所有已启用线路同时生效，模型请求会自动分发到所属供应商"}
          </span>
          <Badge
            variant={routeConfigReadOnly ? "secondary" : "brand"}
            className="readonly-note-tag"
          >
            {routeConfigReadOnly ? "当前线路" : "统一路由"}
          </Badge>
        </div>
      </div>

      <Dialog
        open={routeDialogOpen}
        onOpenChange={(open) => {
          if (!isBusy) {
            setRouteDialogOpen(open);
            if (!open) {
              setRouteDraft(null);
              setOfficialRouteDraft(null);
              setOfficialDialogScope(null);
              setRouteApiKeyVisible(false);
            }
          }
        }}
      >
        {routeDialogOpen && routeDraft && (
          <DialogContent
            className="route-editor-dialog"
            container={popupContainer ?? undefined}
            onEscapeKeyDown={(event) => {
              if (isBusy) event.preventDefault();
            }}
            onPointerDownOutside={(event) => {
              if (isBusy) event.preventDefault();
            }}
          >
            <DialogHeader>
              <div className="route-editor-dialog-title-row">
                <DialogTitle>
                  {routeDraft.authMode === "officialAccount"
                    ? officialDialogScope === "models"
                      ? "同步官方模型"
                      : "编辑官方线路"
                    : config.profiles.some((profile) => profile.id === routeDraft.id)
                      ? "编辑线路"
                      : "新增线路"}
                </DialogTitle>
              </div>
              <DialogDescription>
                {routeDraft.authMode === "officialAccount"
                  ? officialDialogScope === "models"
                    ? draftOfficialAccount
                      ? `当前官方账号：${draftOfficialAccountLabel}。未勾选的模型不会在模型目录和选择器中显示。`
                      : "未勾选的模型不会在模型目录和选择器中显示。"
                    : draftOfficialAccount
                      ? `当前官方账号：${draftOfficialAccountLabel}。此处只调整线路名、短名称、网关地址和上游代理，模型列表请使用线路卡片上的同步按钮。`
                      : "此处只调整线路名、短名称、网关地址和上游代理，模型列表请使用线路卡片上的同步按钮。"
                  : "配置第三方服务的接入信息。保存后可在模型目录中同步模型。"}
              </DialogDescription>
            </DialogHeader>

            {routeDraft.authMode === "officialAccount" ? (
              <div className="official-route-editor">
                {officialDialogScope !== "models" && (
                  <>
                    <div className="route-editor-row route-editor-row-names">
                      <label className="route-field">
                        <span>线路名</span>
                        <Input
                          id="official-route-name-input"
                          aria-label="线路名"
                          maxLength={MAX_ROUTE_NAME_CHARACTERS}
                          aria-invalid={Boolean(
                            officialRouteDraftErrors?.routeName &&
                            (routeValidationAttempted ||
                              (officialRouteDraft?.routeName.length ?? 0) > 0),
                          )}
                          aria-describedby={
                            officialRouteDraftErrors?.routeName &&
                            (routeValidationAttempted ||
                              (officialRouteDraft?.routeName.length ?? 0) > 0)
                              ? "official-route-name-error"
                              : undefined
                          }
                          value={officialRouteDraft?.routeName ?? ""}
                          disabled={isBusy || !draftOfficialAccount}
                          placeholder={routeDraft.name || "OpenAI 官方直登"}
                          onChange={(event) =>
                            updateOfficialRouteDraft({ routeName: event.target.value })}
                        />
                        {officialRouteDraftErrors?.routeName &&
                        (routeValidationAttempted ||
                          (officialRouteDraft?.routeName.length ?? 0) > 0) ? (
                          <small
                            id="official-route-name-error"
                            className="text-[var(--codey-red,#d70015)]"
                            role="alert"
                          >
                            {officialRouteDraftErrors.routeName}
                          </small>
                        ) : !draftOfficialAccount ? (
                          <small className="route-field-hint">
                            未找到该线路对应的官方账号记录。
                          </small>
                        ) : null}
                      </label>
                      <label className="route-field">
                        <span>短名称</span>
                        <Input
                          id="official-route-short-name-input"
                          aria-label="短名称"
                          error={Boolean(
                            officialRouteDraftErrors?.shortName &&
                            (routeValidationAttempted ||
                              (officialRouteDraft?.routeShortName.length ?? 0) > 0),
                          )}
                          aria-errormessage={
                            officialRouteDraftErrors?.shortName &&
                            (routeValidationAttempted ||
                              (officialRouteDraft?.routeShortName.length ?? 0) > 0)
                              ? "official-route-short-name-error"
                              : undefined
                          }
                          value={officialRouteDraft?.routeShortName ?? ""}
                          disabled={isBusy || !draftOfficialAccount}
                          placeholder="官"
                          maxLength={MAX_ROUTE_SHORT_NAME_CHARACTERS}
                          onChange={(event) =>
                            updateOfficialRouteDraft({ routeShortName: event.target.value })}
                        />
                        {officialRouteDraftErrors?.shortName &&
                        (routeValidationAttempted ||
                          (officialRouteDraft?.routeShortName.length ?? 0) > 0) ? (
                          <small
                            id="official-route-short-name-error"
                            className="text-[var(--codey-red,#d70015)]"
                            role="alert"
                          >
                            {officialRouteDraftErrors.shortName}
                          </small>
                        ) : null}
                      </label>
                    </div>

                    <label className="route-field">
                      <span>网关地址（可选）</span>
                      <Input
                        id="official-route-base-url-input"
                        aria-label="网关地址（可选）"
                        aria-invalid={Boolean(officialRouteDraftErrors?.baseUrl)}
                        aria-describedby={
                          officialRouteDraftErrors?.baseUrl
                            ? "official-route-base-url-error"
                            : undefined
                        }
                        value={officialRouteDraft?.baseUrl ?? ""}
                        disabled={isBusy || !draftOfficialAccount}
                        placeholder="留空使用官方默认网关"
                        onChange={(event) =>
                          updateOfficialRouteDraft({ baseUrl: event.target.value })}
                      />
                      {officialRouteDraftErrors?.baseUrl ? (
                        <small
                          id="official-route-base-url-error"
                          className="text-[var(--codey-red,#d70015)]"
                          role="alert"
                        >
                          {officialRouteDraftErrors.baseUrl}
                        </small>
                      ) : (
                        <small className="route-field-hint">
                          默认 https://chatgpt.com/backend-api/codex
                        </small>
                      )}
                    </label>

                    <label className="route-field">
                      <span className="route-option-title-group">
                        <span>上游代理（可选）</span>
                        <Tooltip content={UPSTREAM_PROXY_TOOLTIP_CONTENT}>
                          <span
                            className="route-option-info-trigger"
                            aria-label="上游代理格式与提示"
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                            }}
                          >
                            <IconHelpCircle size={13} />
                          </span>
                        </Tooltip>
                      </span>
                      <Input
                        id="official-route-proxy-input"
                        aria-label="上游代理（可选）"
                        aria-invalid={Boolean(officialRouteDraftErrors?.upstreamProxy)}
                        aria-describedby={
                          officialRouteDraftErrors?.upstreamProxy
                            ? "official-route-proxy-error"
                            : undefined
                        }
                        value={officialRouteDraft?.upstreamProxy ?? routeDraft.upstreamProxy ?? ""}
                        disabled={isBusy}
                        placeholder="留空使用系统代理"
                        onChange={(event) =>
                          updateOfficialRouteDraft({ upstreamProxy: event.target.value })}
                      />
                      {officialRouteDraftErrors?.upstreamProxy ? (
                        <small id="official-route-proxy-error" className="text-[var(--codey-red,#d70015)]" role="alert">
                          {officialRouteDraftErrors.upstreamProxy}
                        </small>
                      ) : null}
                    </label>
                  </>
                )}

                {officialDialogScope !== "settings" && (
                  <>
                    <div className="official-model-editor">
                      <div className="official-model-editor-heading">
                        <span>
                          <strong>支持的模型</strong>
                          <small>已启用 {officialModelDraft.length} 个，至少保留一个。</small>
                        </span>
                        <Badge variant="secondary">
                          {officialModelDraft.length} / {officialCatalog.length}
                        </Badge>
                      </div>
                      <div className="official-model-options">
                        {officialCatalog.map((model) => {
                          const checked = officialModelDraftKeys.has(modelKey(model));
                          return (
                            <div className="official-model-option" style={{ flexWrap: "wrap" }} key={model}>
                              <Checkbox
                                checked={checked}
                                disabled={isBusy || (checked && officialModelDraft.length <= 1)}
                                onCheckedChange={(nextChecked) => {
                                  setOfficialModelDraft((current) =>
                                    nextChecked === true
                                      ? uniqueModelIds([...current, model])
                                      : current.filter(
                                          (candidate) => !modelIdsEqual(candidate, model),
                                        ),
                                  );
                                }}
                                aria-label={`${checked ? "停用" : "启用"}官方模型 ${model}`}
                              />
                              <span>
                                <strong>
                                  {officialDisplayNames.get(modelKey(model)) || model}
                                </strong>
                                <small>{model}</small>
                              </span>

                            </div>
                          );
                        })}
                      </div>
                    </div>
                  </>
                )}
              </div>
            ) : (
              <div className="route-editor-form">
                <div className="route-editor-row route-editor-row-names">
                  <label className="route-field">
                    <span>线路名</span>
                    <Input
                      id="route-name-input"
                      aria-label="线路名"
                      maxLength={MAX_ROUTE_NAME_CHARACTERS}
                      aria-invalid={Boolean(
                        routeDraftErrors?.name &&
                        (routeValidationAttempted || routeDraft.name.length > 0),
                      )}
                      aria-describedby={
                        routeDraftErrors?.name &&
                        (routeValidationAttempted || routeDraft.name.length > 0)
                          ? "route-name-error"
                          : undefined
                      }
                      value={routeDraft.name}
                      disabled={isBusy}
                      placeholder="如：主线路、备用中转"
                      onChange={(event) => updateRouteDraft({ name: event.target.value })}
                    />
                    {routeDraftErrors?.name &&
                    (routeValidationAttempted || routeDraft.name.length > 0) ? (
                      <small id="route-name-error" className="text-[var(--codey-red,#d70015)]" role="alert">
                        {routeDraftErrors.name}
                      </small>
                    ) : null}
                  </label>
                  <label className="route-field">
                    <span>短名称</span>
                    <Input
                      id="route-short-name-input"
                      aria-label="短名称"
                      error={Boolean(
                        routeDraftErrors?.shortName &&
                        (routeValidationAttempted || routeDraft.shortName.length > 0),
                      )}
                      aria-errormessage={
                        routeDraftErrors?.shortName &&
                        (routeValidationAttempted || routeDraft.shortName.length > 0)
                          ? "route-short-name-error"
                          : undefined
                      }
                      value={routeDraft.shortName}
                      disabled={isBusy}
                      placeholder="如：主、备"
                      maxLength={MAX_ROUTE_SHORT_NAME_CHARACTERS}
                      onChange={(event) =>
                        updateRouteDraft({ shortName: event.target.value })}
                    />
                    {routeDraftErrors?.shortName &&
                    (routeValidationAttempted || routeDraft.shortName.length > 0) ? (
                      <small
                        id="route-short-name-error"
                        className="text-[var(--codey-red,#d70015)]"
                        role="alert"
                      >
                        {routeDraftErrors.shortName}
                      </small>
                    ) : null}
                  </label>
                </div>

                <div className="route-field">
                  <span id="route-protocol-label">上游协议</span>
                  <Select
                    aria-label="上游协议"
                    aria-labelledby="route-protocol-label"
                    value={routeDraft.upstreamProtocol}
                    disabled={isBusy}
                    onChange={(value) => {
                      if (value == null) return;
                      const upstreamProtocol = value as Profile["upstreamProtocol"];
                      updateRouteDraft({
                        upstreamProtocol,
                        supportsWebsockets:
                          upstreamProtocol === "openaiResponses"
                            ? Boolean(routeDraft.supportsWebsockets)
                            : false,
                        supportsNativeWebSearch:
                          upstreamProtocol === "openaiResponses"
                            ? Boolean(routeDraft.supportsNativeWebSearch)
                            : false,
                      });
                    }}
                    optionList={routeProtocolOptions}
                  />
                </div>

                {routeDraft.upstreamProtocol === "openaiResponses" && (
                  <div className="route-protocol-options route-editor-span-all">
                    <div className="route-option-item">
                      <div className="route-option-header">
                        <div className="route-option-title-group">
                          <strong className="route-option-title">WebSocket</strong>
                          <Tooltip content="优先尝试复用长连接；使用代理或连接失败时转为流式 HTTP。能力变更需重启 Codex，实际速度取决于上游和网络。">
                            <span className="route-option-info-trigger" aria-label="WebSocket 详细说明">
                              <IconInfoCircle size={13} />
                            </span>
                          </Tooltip>
                        </div>
                        <Switch
                          size="sm"
                          checked={Boolean(routeDraft.supportsWebsockets)}
                          disabled={isBusy}
                          onCheckedChange={(checked) =>
                            updateRouteDraft({ supportsWebsockets: checked })}
                          aria-label="WebSocket"
                        />
                      </div>
                      <small className="route-field-hint">
                        优先长连接，失败转流式 HTTP
                      </small>
                    </div>
                    <div className="route-option-item">
                      <div className="route-option-header">
                        <div className="route-option-title-group">
                          <strong className="route-option-title">原生网页搜索</strong>
                          <Tooltip content="仅在上游和所选模型都明确支持时开启。">
                            <span className="route-option-info-trigger" aria-label="原生网页搜索详细说明">
                              <IconInfoCircle size={13} />
                            </span>
                          </Tooltip>
                        </div>
                        <Switch
                          size="sm"
                          checked={Boolean(routeDraft.supportsNativeWebSearch)}
                          disabled={isBusy}
                          onCheckedChange={(checked) =>
                            updateRouteDraft({ supportsNativeWebSearch: checked })}
                          aria-label="原生网页搜索"
                        />
                      </div>
                      <small className="route-field-hint">
                        仅在上游与模型支持时开启
                      </small>
                    </div>
                  </div>
                )}

                <label className="route-field">
                  <span>URL</span>
                  <Input
                    id="route-url-input"
                    aria-label="URL"
                    aria-invalid={Boolean(
                      routeDraftErrors?.baseUrl &&
                      (routeValidationAttempted || routeDraft.baseUrl.trim()),
                    )}
                    aria-describedby={
                      routeDraftErrors?.baseUrl &&
                      (routeValidationAttempted || routeDraft.baseUrl.trim())
                        ? "route-url-error"
                        : undefined
                    }
                    value={routeDraft.baseUrl}
                    disabled={isBusy}
                    placeholder={
                      routeDraft.upstreamProtocol === "anthropicMessages"
                        ? "https://api.anthropic.com"
                        : "https://api.example.com/v1"
                    }
                    onChange={(event) => updateRouteDraft({ baseUrl: event.target.value })}
                  />
                  {routeDraftErrors?.baseUrl &&
                  (routeValidationAttempted || routeDraft.baseUrl.trim()) ? (
                    <small id="route-url-error" className="text-[var(--codey-red,#d70015)]" role="alert">
                      {routeDraftErrors.baseUrl}
                    </small>
                  ) : null}
                </label>

                <label className="route-field">
                  <span>Key</span>
                  <PasswordInput
                    id="route-key-input"
                    aria-label="Key"
                    aria-invalid={Boolean(
                      routeValidationAttempted && routeDraftErrors?.apiKey,
                    )}
                    aria-describedby={
                      routeValidationAttempted && routeDraftErrors?.apiKey
                        ? "route-key-error"
                        : undefined
                    }
                    autoComplete="new-password"
                    visibility={routeApiKeyVisible}
                    onVisibilityChange={toggleRouteApiKeyVisibility}
                    value={routeDraft.apiKey}
                    disabled={isBusy}
                    placeholder={
                      routeDraft.apiKeyConfigured
                        ? "已保存（输入新 Key 替换）"
                        : routeDraft.upstreamProtocol === "anthropicMessages"
                          ? "sk-ant-..."
                          : "sk-..."
                    }
                    onChange={(event) => {
                      updateRouteDraft({ apiKey: event.target.value });
                    }}
                  />
                  {routeValidationAttempted && routeDraftErrors?.apiKey ? (
                    <small id="route-key-error" className="text-[var(--codey-red,#d70015)]" role="alert">
                      {routeDraftErrors.apiKey}
                    </small>
                  ) : null}
                </label>

                <label className="route-field">
                  <span className="route-option-title-group">
                    <span>上游代理（可选）</span>
                    <Tooltip content={UPSTREAM_PROXY_TOOLTIP_CONTENT}>
                      <span
                        className="route-option-info-trigger"
                        aria-label="上游代理格式与提示"
                        onClick={(e) => {
                          e.preventDefault();
                          e.stopPropagation();
                        }}
                      >
                        <IconHelpCircle size={13} />
                      </span>
                    </Tooltip>
                  </span>
                  <Input
                    id="route-proxy-input"
                    aria-label="上游代理（可选）"
                    aria-invalid={Boolean(
                      routeDraftErrors?.upstreamProxy &&
                      (routeValidationAttempted || (routeDraft.upstreamProxy || "").trim()),
                    )}
                    aria-describedby={
                      routeDraftErrors?.upstreamProxy &&
                      (routeValidationAttempted || (routeDraft.upstreamProxy || "").trim())
                        ? "route-proxy-error"
                        : undefined
                    }
                    value={routeDraft.upstreamProxy || ""}
                    disabled={isBusy}
                    placeholder="留空使用系统代理"
                    onChange={(event) =>
                      updateRouteDraft({ upstreamProxy: event.target.value })}
                  />
                  {routeDraftErrors?.upstreamProxy &&
                  (routeValidationAttempted || (routeDraft.upstreamProxy || "").trim()) ? (
                    <small id="route-proxy-error" className="text-[var(--codey-red,#d70015)]" role="alert">
                      {routeDraftErrors.upstreamProxy}
                    </small>
                  ) : null}
                </label>
              </div>
            )}

            {headerError && <small className="text-[var(--codey-red,#d70015)]" role="alert">{headerError}；请先在线路的请求头编辑器中修正。</small>}
            <DialogFooter className="route-editor-footer">
              <Button
                variant="outline"
                disabled={isBusy}
                onClick={() => {
                  setRouteDialogOpen(false);
                  setRouteDraft(null);
                  setOfficialRouteDraft(null);
                  setOfficialDialogScope(null);
                  setRouteValidationAttempted(false);
                  setRouteApiKeyVisible(false);
                }}
              >
                取消
              </Button>
              <Button
                disabled={isBusy || (
                  routeDraft.authMode === "officialAccount"
                    ? officialDialogScope === "models" && officialModelDraft.length === 0
                    : routeValidationAttempted && routeDraftHasErrors
                )}
                onClick={() => void saveRouteDraft()}
              >
                <Check aria-hidden="true" />
                {routeDraft.authMode === "officialAccount" && officialDialogScope === "models"
                  ? "保存模型"
                  : "保存线路"}
              </Button>
            </DialogFooter>
          </DialogContent>
        )}
      </Dialog>
      <Dialog open={headerDialogProfile !== null} onOpenChange={(open) => { if (!open && !isBusy) setHeaderDialogProfile(null); }}>
        {headerDialogProfile && (
          <DialogContent className="route-editor-dialog">
            <DialogHeader>
              <DialogTitle>编辑上游请求头</DialogTitle>
              <DialogDescription>{headerDialogProfile.name} 的 Codey → 上游请求头，使用 JSON 对象表示；null 或空字符串删除普通请求头，认证和协议必需请求头受保护。</DialogDescription>
            </DialogHeader>
            <label className="route-field">
              <span>请求头（JSON）</span>
              <textarea
                aria-label="请求头（JSON）"
                aria-invalid={Boolean(headerError)}
                aria-describedby={headerError ? "request-headers-error" : undefined}
                value={routeHeadersText}
                disabled={isBusy}
                rows={10}
                className="min-h-48 rounded-lg border border-[rgb(var(--codey-ink-rgb,0,0,0))]/10 bg-[var(--codey-surface,#fff)] p-2 font-mono text-xs"
                onChange={(event) => { setRouteHeadersText(event.target.value); setHeaderError(""); }}
                placeholder={'{"X-Custom-Header": "value"}'}
              />
              {headerError && <small id="request-headers-error" className="text-[var(--codey-red,#d70015)]" role="alert">{headerError}</small>}
            </label>
            <DialogFooter className="route-editor-footer">
              <Button variant="outline" disabled={isBusy} onClick={() => setHeaderDialogProfile(null)}>取消</Button>
              <Button disabled={isBusy} onClick={() => void saveHeaders()}><Check aria-hidden="true" />保存请求头</Button>
            </DialogFooter>
          </DialogContent>
        )}
      </Dialog>
    </section>
  );
}

export const ModelSection = memo(ModelSectionComponent);
