import { memo, useEffect, useMemo, useState } from "react";
import {
  IconCheck as Check,
  IconChevronRight,
  IconCpu,
  IconEdit as Edit,
  IconGripVertical,
  IconInfoCircle,
  IconListDetails,
  IconPlus as Plus,
  IconRefresh as RefreshCw,
  IconServer as Server,
  IconShieldCheck,
  IconTrash as Trash,
} from "@tabler/icons-react";

import type { Config, ModelContextConfig, ModelState, Profile, ProviderStatus } from "./App.types";
import {
  Badge,
  Button,
  Card,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  PasswordInput,
  Select,
  Switch,
} from "./components/antd";
import { modelIdsEqual, modelKey, uniqueModelIds } from "./modelIds";
import { globalDefaultForRoute, routeProviderId } from "./modelRoutes";
import { validateThirdPartyRouteShortName } from "./routeShortNames";
import { flushCardClass } from "./uiClasses";
import { validateOutboundApiUrl } from "./urlValidation";
import { invoke } from "./api";

export function ModelContextFields({ model, policy, disabled, onChange }: {
  model: string;
  policy?: ModelContextConfig;
  disabled: boolean;
  onChange: (policy: ModelContextConfig | undefined) => void;
}) {
  return (
    <details className="group model-context-details w-full text-xs">
      <summary className="model-context-summary flex cursor-pointer select-none list-none items-center gap-1.5 py-0.5 pl-6 text-[11px] text-[#6e6e73] transition-colors hover:text-[#1d1d1f] outline-none [&::-webkit-details-marker]:hidden">
        <IconChevronRight
          size={12}
          className="shrink-0 text-[#86868b] transition-transform duration-150 group-open:rotate-90"
          aria-hidden="true"
        />
        <span className="font-medium">上下文预算</span>
        {policy?.contextWindowTokens ? (
          <span className="inline-flex items-center rounded border border-blue-500/20 bg-blue-500/10 px-1.5 py-0.2 text-[10px] font-semibold text-[#007aff]">
            {policy.contextWindowTokens.toLocaleString()} Token
          </span>
        ) : (
          <span className="inline-flex items-center rounded border border-black/5 bg-black/[0.04] px-1.5 py-0.2 text-[10px] font-normal text-[#86868b]">
            默认
          </span>
        )}
      </summary>
      <div className="mt-1.5 rounded-[9px] border border-black/[0.08] bg-[#f8f8fa] p-3 shadow-[0_1px_2px_rgba(0,0,0,0.02)]">
        <div className="mb-2.5 flex items-start justify-between gap-2">
          <p className="text-[11px] leading-[1.45] text-[#6e6e73]">
            自定义值优先于 1M；清空窗口恢复默认。未知模型默认使用 32768 Token 保守预算，不代表服务端容量。修改后重启 Codex 生效。
          </p>
          {policy && (
            <button
              type="button"
              disabled={disabled}
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onChange(undefined);
              }}
              className="shrink-0 text-[10.5px] font-medium text-[#007aff] transition-colors hover:text-[#d70015] hover:underline disabled:opacity-40"
            >
              恢复默认
            </button>
          )}
        </div>
        <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-3">
          {([
            ["contextWindowTokens", "窗口", 1024, "默认 (32768)"],
            ["autoCompactTokenLimit", "压缩阈值", 1, "自动"],
            ["reserveOutputTokens", "输出预留", 1, "不单独预留"],
          ] as const).map(([field, label, min, placeholder]) => (
            <label key={field} className="flex flex-col gap-1">
              <span className="text-[11px] font-medium text-[#4b5563]">
                {label} <span className="text-[10px] text-[#86868b]">（Token）</span>
              </span>
              <Input
                type="number"
                min={min}
                max={10_000_000}
                step={1}
                disabled={disabled}
                className="h-7 rounded-md border-black/10 bg-white text-xs focus:border-[#007aff]"
                aria-label={`${model} ${label} Token`}
                placeholder={placeholder}
                value={policy?.[field] ?? ""}
                onChange={(event) => {
                  const raw = event.target.value;
                  if (field === "contextWindowTokens" && raw === "") {
                    onChange(undefined);
                    return;
                  }
                  onChange({
                    contextWindowTokens: 32768,
                    ...policy,
                    [field]: raw === "" ? undefined : Number(raw),
                  });
                }}
              />
            </label>
          ))}
        </div>
        <p className="mt-2 text-[10px] leading-relaxed text-[#86868b]">
          阈值不能超过窗口的 90% 和预留后的有效空间；预留按整百分比向下取整，不是输出长度上限。
        </p>
      </div>
    </details>
  );
}

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
  onToggleLocalRouter: (checked: boolean) => void;
  onToggleRouteRequestLog: (checked: boolean) => void;
  onSaveRoute: (route: Profile) => Promise<boolean>;
  onReorderRoute: (sourceId: string, targetId: string) => Promise<void>;
  onDeleteRoute: (routeId: string) => void;
  onFetchRouteModels: (route: Profile) => void;
  onToggleAccountUsage?: (checked: boolean) => void;
  onSaveOfficialRouteSettings?: (
    routeId: string,
    models: string[],
    showAccountUsageInHeader: boolean,
    supports1MContextModels: string[],
    enabled: boolean,
    modelContexts: Record<string, ModelContextConfig>,
  ) => Promise<boolean>;
  onSetDefaultModel: (routeId: string, model: string) => void;
};

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
};

function validateRouteDraft(route: Profile, profiles: readonly Profile[]): RouteDraftErrors {
  if (route.authMode === "officialAccount") {
    return { name: "", shortName: "", baseUrl: "", apiKey: "" };
  }
  const errors: RouteDraftErrors = {
    name: route.name.trim() ? "" : "请输入线路名称",
    shortName: validateThirdPartyRouteShortName(route.shortName, profiles, route.id),
    baseUrl: "",
    apiKey: "",
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
  onToggleLocalRouter,
  onToggleRouteRequestLog,
  onSaveRoute,
  onReorderRoute,
  onDeleteRoute,
  onFetchRouteModels,
  onToggleAccountUsage,
  onSaveOfficialRouteSettings,
  onSetDefaultModel,
}: ModelSectionProps) {
  const [routeDialogOpen, setRouteDialogOpen] = useState(false);
  const [draggedRouteId, setDraggedRouteId] = useState<string | null>(null);
  const [dropRouteId, setDropRouteId] = useState<string | null>(null);
  const [routeDraft, setRouteDraft] = useState<Profile | null>(null);
  const [routeValidationAttempted, setRouteValidationAttempted] = useState(false);
  const [routeApiKeyVisible, setRouteApiKeyVisible] = useState(false);
  const [officialModelDraft, setOfficialModelDraft] = useState<string[]>([]);
  const routeConfigReadOnly = !config.localRouterEnabled;

  useEffect(() => {
    if (!routeConfigReadOnly) return;
    setRouteDialogOpen(false);
    setRouteDraft(null);
    setRouteApiKeyVisible(false);
  }, [routeConfigReadOnly]);

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
    };
  }, [config.profiles, currentProvider, routeConfigReadOnly]);
  const visibleProfiles = useMemo(
    () => {
      if (routeConfigReadOnly) return nativeProfile ? [nativeProfile] : [];
      return config.profiles.filter(
        (profile) =>
          profile.enabled === false || profile.authMode !== "officialAccount" || officialAccountAvailable,
      );
    },
    [config.profiles, nativeProfile, officialAccountAvailable, routeConfigReadOnly],
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
            ? configuredModels.length > 0
              ? configuredModels
              : officialCatalog
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

  const openNewRouteDialog = () => {
    setRouteDraft(createRoute(config.profiles));
    setRouteValidationAttempted(false);
    setRouteApiKeyVisible(false);
    setOfficialModelDraft([]);
    setRouteDialogOpen(true);
  };
  const openEditRouteDialog = (profile: Profile) => {
    const official = profile.authMode === "officialAccount";
    setRouteDraft({ ...profile });
    setRouteValidationAttempted(false);
    setRouteApiKeyVisible(false);
    if (official) {
      const providerId = routeProviderId(profile);
      const configuredModels = config.selectedModelsByProvider[providerId] || [];
      setOfficialModelDraft(
        configuredModels.length > 0
          ? configuredModels
          : officialCatalog,
      );
    }
    setRouteDialogOpen(true);
  };
  const updateRouteDraft = (patch: Partial<Profile>) => {
    setRouteDraft((current) => current ? { ...current, ...patch } : current);
  };
  const toggleRouteApiKeyVisibility = () => {
    setRouteApiKeyVisible((visible) => !visible);
  };
  const saveRouteDraft = async () => {
    if (!routeDraft) return;
    if (routeDraft.authMode !== "officialAccount" && routeDraftHasErrors) {
      setRouteValidationAttempted(true);
      requestAnimationFrame(() => {
        const firstInvalid = document.querySelector<HTMLInputElement>(
          ".route-editor-form [aria-invalid='true']",
        );
        firstInvalid?.focus();
      });
      return;
    }
    const saved = routeDraft.authMode === "officialAccount"
      ? (onSaveOfficialRouteSettings
          ? await onSaveOfficialRouteSettings(
              routeDraft.id,
              officialModelDraft,
              showAccountUsageInHeader,
              [],
              routeDraft.enabled !== false,
              {},
            )
          : true)
      : await onSaveRoute(routeDraft);
    if (saved) {
      setRouteDialogOpen(false);
      setRouteDraft(null);
    }
  };

  return (
    <section className="route-section" aria-labelledby="route-title">
      <div className="section-title">
        <div className="section-heading">
          <span className="section-icon" aria-hidden="true">
            <Server size={15} />
          </span>
          <div>
            <h2 id="route-title">线路与模型</h2>
            <p>
              {routeConfigReadOnly
                ? "查看 Codex 当前线路并同步原始模型目录"
                : "统一管理供应商线路与模型目录"}
            </p>
          </div>
        </div>
        <div className="route-heading-actions">
          <div className="local-router-toggle">
            <strong>本地路由</strong>
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
              <div className="local-router-toggle route-request-log-toggle">
                <strong>开启日志记录</strong>
                <Switch
                  size="sm"
                  checked={config.routeRequestLog.enabled}
                  disabled={isBusy}
                  onCheckedChange={onToggleRouteRequestLog}
                  aria-label="开启请求日志记录"
                />
              </div>
              <Button
                color="primary"
                variant="filled"
                onClick={() => void invoke("open_route_request_logs")}
              >
                <IconListDetails size={14} aria-hidden="true" />
                <span>查看请求日志</span>
              </Button>
            </>
          )}
        </div>
      </div>

      <Card className={`route-card ${flushCardClass}`}>
        <div className={`route-manager${routeConfigReadOnly ? " route-manager-current" : ""}`}>
          <div className="route-catalog-pane">
            <div className="catalog-aggregate-heading">
              <div className="catalog-aggregate-title-wrap">
                <div className="catalog-aggregate-title">
                  <strong>{routeConfigReadOnly ? "当前线路模型" : "供应商与模型"}</strong>
                  <Badge variant="secondary">{visibleProfiles.length} 条线路</Badge>
                  <Badge variant="secondary">{totalModelCount} 个模型</Badge>
                </div>
                <small>
                  {routeConfigReadOnly
                    ? "模型请求由 Codex 当前 Provider 直接处理"
                    : "点击模型设为全局默认；拖动线路左侧手柄调整顺序"}
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
                const syncModels = () => {
                  if (isOfficial && !routeConfigReadOnly) openEditRouteDialog(profile);
                  else onFetchRouteModels(profile);
                };
                return (
                  <section
                    className={`provider-model-group${disabled ? " is-disabled" : ""}${dropRouteId === profile.id ? " is-drop-target" : ""}`}
                    key={profile.id}
                    aria-labelledby={`provider-model-${profile.id}`}
                    onDragOver={(event) => {
                      if (routeConfigReadOnly || !draggedRouteId || draggedRouteId === profile.id || isBusy || dirty) return;
                      event.preventDefault();
                      event.dataTransfer.dropEffect = "move";
                      setDropRouteId(profile.id);
                    }}
                    onDrop={(event) => {
                      event.preventDefault();
                      if (!routeConfigReadOnly && !isBusy && !dirty && draggedRouteId && draggedRouteId !== profile.id) {
                        void onReorderRoute(draggedRouteId, profile.id);
                      }
                      setDraggedRouteId(null);
                      setDropRouteId(null);
                    }}
                  >
                    <div className="provider-model-group-left">
                      <div className="provider-heading-main">
                        {!routeConfigReadOnly && (
                          <button
                            type="button"
                            className="route-item-drag-handle cursor-grab text-gray-400 hover:text-gray-600 active:cursor-grabbing disabled:cursor-default"
                            disabled={isBusy || dirty}
                            draggable={!isBusy && !dirty}
                            aria-label={`调整线路 ${profile.name} 的顺序`}
                            title="拖动排序，也可按上下方向键调整"
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
                              if (target) void onReorderRoute(profile.id, target.id);
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
                            <strong id={`provider-model-${profile.id}`} title={profile.name}>{profile.name || "未命名线路"}</strong>
                            <div className="route-item-badges">
                              {disabled ? <Badge variant="destructive">已禁用</Badge> : (
                                <>
                                  <Badge variant="secondary">{group?.models.length || 0} 模型</Badge>
                                  {!routeConfigReadOnly && !isOfficial && (
                                    <Badge variant={group?.models.length ? "brand" : "secondary"}>
                                      {group?.models.length ? "已接入路由" : "待配置模型"}
                                    </Badge>
                                  )}
                                  {(isOfficial || profile.supportsWebsockets) && <Badge variant="brand">WS</Badge>}
                                </>
                              )}
                              {disabled && <span className="route-disabled-hint">启用后可使用此线路的模型</span>}
                            </div>
                          </div>
                          <small title={isOfficial ? "官方账号登录" : profile.baseUrl}>
                            {isOfficial ? "官方账号登录" : profile.baseUrl || "待填写 URL"}
                          </small>
                        </div>
                      </div>
                      {group && (group.models.length > 0 ? (
                        <div className="provider-model-tags">
                          {group.models.map((model) => {
                            const isDefault = !routeConfigReadOnly && modelIdsEqual(group.defaultModel, model);
                            const displayName = group.official ? officialDisplayNames.get(modelKey(model)) || model : model;
                            return (
                              <button
                                type="button"
                                key={`${group.providerId}:${model}`}
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
                    <div className="provider-model-group-actions">
                      <div className="provider-model-group-actions-top">
                        {isOfficial && !disabled && (
                          <div className="route-item-usage-toggle provider-model-usage-toggle">
                            <span className="route-item-usage-label">额度显示</span>
                            <Switch size="xs" checked={showAccountUsageInHeader} disabled={isBusy} onCheckedChange={(checked) => onToggleAccountUsage?.(checked)} aria-label="在账户区域显示额度" />
                          </div>
                        )}
                        {!routeConfigReadOnly && !isOfficial && (
                          <div className="route-item-manage-actions">
                            <Button
                              variant="link"
                              color="primary"
                              size="xs"
                              disabled={isBusy || dirty}
                              onClick={() => openEditRouteDialog(profile)}
                              aria-label={`编辑线路 ${profile.name}`}
                              title={`编辑线路 ${profile.name}`}
                            >
                              <Edit size={14} aria-hidden="true" />
                            </Button>
                            <Button
                              variant="link"
                              color="danger"
                              size="xs"
                              disabled={routeConfigReadOnly || isBusy || dirty || config.profiles.length <= 1}
                              onClick={() => onDeleteRoute(profile.id)}
                              aria-label={`删除线路 ${profile.name}`}
                              title={config.profiles.length <= 1 ? "至少需要保留一条线路" : `删除线路 ${profile.name}`}
                            >
                              <Trash size={14} aria-hidden="true" />
                            </Button>
                          </div>
                        )}
                        {!routeConfigReadOnly && isOfficial && disabled && (
                          <div className="route-item-manage-actions">
                            <Button
                              variant="link"
                              color="primary"
                              size="xs"
                              disabled={isBusy || dirty}
                              onClick={() => openEditRouteDialog(profile)}
                              aria-label={`编辑线路 ${profile.name}`}
                              title={`编辑线路 ${profile.name}`}
                            >
                              <Edit size={14} aria-hidden="true" />
                            </Button>
                          </div>
                        )}
                      </div>
                      <div className="provider-model-group-actions-bottom">
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
                      </div>
                    </div>
                  </section>
                );
              })}
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
      </Card>

      <Dialog
        open={routeDialogOpen}
        onOpenChange={(open) => {
          if (!isBusy) {
            setRouteDialogOpen(open);
            if (!open) {
              setRouteDraft(null);
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
              <DialogTitle>
                {routeDraft.authMode === "officialAccount"
                  ? "配置官方账号模型"
                  : config.profiles.some((profile) => profile.id === routeDraft.id)
                    ? "编辑线路"
                    : "新增线路"}
              </DialogTitle>
              <DialogDescription>
                {routeDraft.authMode === "officialAccount"
                  ? "选择允许在 Codex 中使用的官方候选模型。未勾选的模型不会在模型目录和选择器中出现。"
                  : "配置第三方服务的接入信息。保存后可在模型目录中同步模型。"}
              </DialogDescription>
            </DialogHeader>

            <div className="route-option-item">
              <strong>启用线路</strong>
              <Switch
                checked={routeDraft.enabled !== false}
                disabled={isBusy}
                onCheckedChange={(enabled) => updateRouteDraft({ enabled })}
                aria-label="启用线路"
              />
            </div>

            {routeDraft.authMode === "officialAccount" ? (
              <div className="official-route-editor">
                <div className="official-route-summary">
                  <span>
                    <strong>{routeDraft.name}</strong>
                    <small>使用当前 Codex 官方账号登录状态</small>
                  </span>
                  <Badge variant="info">官方账号</Badge>
                </div>

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
              </div>
            ) : (
              <div className="route-editor-form">
                <div className="route-editor-row route-editor-row-names">
                  <label className="route-field">
                    <span>线路名</span>
                    <Input
                      id="route-name-input"
                      aria-label="线路名"
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
                      <small id="route-name-error" className="text-[#d70015]" role="alert">
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
                      onChange={(event) =>
                        updateRouteDraft({ shortName: event.target.value })}
                    />
                    {routeDraftErrors?.shortName &&
                    (routeValidationAttempted || routeDraft.shortName.length > 0) ? (
                      <small
                        id="route-short-name-error"
                        className="text-[#d70015]"
                        role="alert"
                      >
                        {routeDraftErrors.shortName}
                      </small>
                    ) : null}
                  </label>
                  <small id="route-short-name-hint" className="route-field-hint route-editor-span-all">
                    最多 2 个字符且不可重复，模型名称前会显示为 [短名称]
                  </small>
                </div>

                <div className="route-field">
                  <span id="route-protocol-label">上游协议</span>
                  <Select
                    aria-label="上游协议"
                    aria-labelledby="route-protocol-label"
                    value={routeDraft.upstreamProtocol}
                    disabled={isBusy}
                    zIndex={1100}
                    getPopupContainer={() => popupContainer ?? document.body}
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
                  <small className="route-field-hint">
                    请选择上游实际支持的接口协议；Chat Completions 与 Anthropic Messages 会由本地路由适配为 Codex 可用格式。
                  </small>
                </div>

                {routeDraft.upstreamProtocol === "openaiResponses" && (
                  <div className="route-protocol-options route-editor-span-all">
                    <div className="route-option-item">
                      <div className="route-option-content">
                        <strong className="route-option-title">WebSocket</strong>
                        <small className="route-field-hint">
                          优先尝试复用长连接；使用代理或连接失败时转为流式 HTTP。能力变更需重启 Codex，实际速度取决于上游和网络。
                        </small>
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
                    <div className="route-option-item">
                      <div className="route-option-content">
                        <strong className="route-option-title">原生网页搜索</strong>
                        <small className="route-field-hint">
                          仅在上游和所选模型都明确支持时开启
                        </small>
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
                    <small id="route-url-error" className="text-[#d70015]" role="alert">
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
                    <small id="route-key-error" className="text-[#d70015]" role="alert">
                      {routeDraftErrors.apiKey}
                    </small>
                  ) : null}
                </label>
              </div>
            )}

            <DialogFooter className="route-editor-footer">
              <Button
                variant="outline"
                disabled={isBusy}
                onClick={() => {
                  setRouteDialogOpen(false);
                  setRouteDraft(null);
                  setRouteValidationAttempted(false);
                  setRouteApiKeyVisible(false);
                }}
              >
                取消
              </Button>
              <Button
                disabled={isBusy || (
                  routeDraft.authMode === "officialAccount"
                    ? officialModelDraft.length === 0
                    : routeValidationAttempted && routeDraftHasErrors
                )}
                onClick={() => void saveRouteDraft()}
              >
                <Check aria-hidden="true" />
                {routeDraft.authMode === "officialAccount" ? "保存模型" : "保存线路"}
              </Button>
            </DialogFooter>
          </DialogContent>
        )}
      </Dialog>
    </section>
  );
}

export const ModelSection = memo(ModelSectionComponent);
