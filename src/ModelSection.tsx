import { memo, useEffect, useMemo, useState, type KeyboardEvent } from "react";
import {
  IconCheck as Check,
  IconCpu,
  IconEdit as Edit,
  IconEye,
  IconEyeOff,
  IconInfoCircle,
  IconListDetails,
  IconPlus as Plus,
  IconRefresh as RefreshCw,
  IconServer as Server,
  IconShieldCheck,
  IconTrash as Trash,
  IconWorld,
} from "@tabler/icons-react";

import type { Config, ModelState, Profile, ProviderStatus } from "./App.types";
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
} from "./components/mantine";
import { modelIdsEqual, modelKey, uniqueModelIds } from "./modelIds";
import { globalDefaultForRoute, routeProviderId } from "./modelRoutes";
import { SETTINGS_OVERLAY_Z_INDEX } from "./overlay.constants";
import { validateThirdPartyRouteShortName } from "./routeShortNames";
import { flushCardClass } from "./uiClasses";
import { validateOutboundApiUrl } from "./urlValidation";
import { invoke } from "./api";

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
  onDeleteRoute: (routeId: string) => void;
  onFetchRouteModels: (route: Profile) => void;
  onToggleAccountUsage?: (checked: boolean) => void;
  onSaveOfficialRouteSettings?: (
    routeId: string,
    models: string[],
    showAccountUsageInHeader: boolean,
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
  const hasApiKey =
    route.apiKey.trim() !== "" ||
    Boolean(route.apiKeyConfigured);
  if (!hasApiKey) errors.apiKey = "请输入 API Key";
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
  onDeleteRoute,
  onFetchRouteModels,
  onToggleAccountUsage,
  onSaveOfficialRouteSettings,
  onSetDefaultModel,
}: ModelSectionProps) {
  const [routeDialogOpen, setRouteDialogOpen] = useState(false);
  const [routeDraft, setRouteDraft] = useState<Profile | null>(null);
  const [routeValidationAttempted, setRouteValidationAttempted] = useState(false);
  const [routeApiKeyVisible, setRouteApiKeyVisible] = useState(false);
  const [officialModelDraft, setOfficialModelDraft] = useState<string[]>([]);
  const [selectedProviderFilter, setSelectedProviderFilter] = useState<string>("all");
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
          profile.authMode !== "officialAccount" || officialAccountAvailable,
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
    () =>
      visibleProfiles.map((profile) => {
        const providerId = routeProviderId(profile);
        const official = profile.authMode === "officialAccount";
        const configuredModels = config.selectedModelsByProvider[providerId] || [];
        const nativeOfficialModels = modelState.officialModels
          .filter((model) => model.supported)
          .map((model) => model.slug);
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
      }),
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
  const providerFilterIds = useMemo(
    () => ["all", ...modelGroups.map((group) => group.providerId)],
    [modelGroups],
  );
  useEffect(() => {
    if (!providerFilterIds.includes(selectedProviderFilter)) {
      setSelectedProviderFilter("all");
    }
  }, [providerFilterIds, selectedProviderFilter]);
  const routeDraftErrors = useMemo(
    () => routeDraft ? validateRouteDraft(routeDraft, config.profiles) : null,
    [config.profiles, routeDraft],
  );
  const routeDraftHasErrors = Boolean(
    routeDraftErrors && Object.values(routeDraftErrors).some(Boolean),
  );

  const handleProviderTabKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
  ) => {
    const currentIndex = providerFilterIds.indexOf(selectedProviderFilter);
    let nextIndex = currentIndex < 0 ? 0 : currentIndex;
    if (event.key === "ArrowRight") {
      nextIndex = (nextIndex + 1) % providerFilterIds.length;
    } else if (event.key === "ArrowLeft") {
      nextIndex = (nextIndex - 1 + providerFilterIds.length) % providerFilterIds.length;
    } else if (event.key === "Home") {
      nextIndex = 0;
    } else if (event.key === "End") {
      nextIndex = providerFilterIds.length - 1;
    } else {
      return;
    }
    event.preventDefault();
    setSelectedProviderFilter(providerFilterIds[nextIndex]);
    event.currentTarget
      .parentElement
      ?.querySelectorAll<HTMLButtonElement>('[role="tab"]')
      .item(nextIndex)
      .focus();
  };

  const displayedGroups = useMemo(() => {
    return selectedProviderFilter === "all"
      ? modelGroups
      : modelGroups.filter((group) => group.providerId === selectedProviderFilter);
  }, [modelGroups, selectedProviderFilter]);

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
                variant="outline"
                size="sm"
                onClick={() => void invoke("open_route_request_logs")}
              >
                <IconListDetails aria-hidden="true" />
                查看请求日志
              </Button>
            </>
          )}
        </div>
      </div>

      <Card className={`route-card ${flushCardClass}`}>
        <div
          className={`route-manager route-manager-balanced${
            routeConfigReadOnly ? " route-manager-current" : ""
          }`}
        >
          {!routeConfigReadOnly && (
            <aside className="route-list-pane" aria-label="线路列表">
            <div className="route-list-heading">
              <div>
                <div className="route-list-heading-title">
                  <strong>供应商线路</strong>
                  <Badge variant="secondary" size="xs">
                    {visibleProfiles.length}
                  </Badge>
                </div>
                <small>
                  {routeConfigReadOnly
                    ? "已保存线路以只读方式加载"
                    : "第三方线路同时接入统一路由"}
                </small>
              </div>
              <Button
                size="xs"
                variant="secondary"
                disabled={routeConfigReadOnly || isBusy || dirty}
                onClick={openNewRouteDialog}
              >
                <Plus size={13} aria-hidden="true" />
                新增线路
              </Button>
            </div>
            <div className="route-list">
              {visibleProfiles.map((profile) => {
                const providerId = routeProviderId(profile);
                const group = modelGroupByProviderId.get(providerId);
                const isOfficial = profile.authMode === "officialAccount";
                return (
                  <div
                    className="route-list-item"
                    key={profile.id}
                  >
                    <div className="route-item-header">
                      <div className="route-item-title-wrap">
                        <div
                          className={`route-item-icon-pill ${isOfficial ? "official" : "custom"}`}
                          aria-hidden="true"
                        >
                          {isOfficial ? (
                            <IconShieldCheck size={15} />
                          ) : (
                            <IconWorld size={15} />
                          )}
                        </div>
                        <div className="route-item-names">
                          <strong title={profile.name || "未命名线路"}>
                            {profile.name || "未命名线路"}
                          </strong>
                          <small
                            title={
                              isOfficial
                                ? "官方账号登录"
                                : profile.baseUrl || "待填写 URL"
                            }
                          >
                            {isOfficial
                              ? "官方账号登录"
                              : profile.baseUrl || "待填写 URL"}
                          </small>
                        </div>
                      </div>
                      {!isOfficial && (
                        <div className="route-item-actions">
                          <Button
                            className="route-action-button route-edit-button"
                            variant="ghost"
                            size="xs"
                            disabled={routeConfigReadOnly || isBusy || dirty}
                            onClick={() => openEditRouteDialog(profile)}
                            aria-label={`编辑线路 ${profile.name}`}
                            title={`编辑线路 ${profile.name}`}
                          >
                            <Edit size={13} aria-hidden="true" />
                          </Button>
                          <Button
                            className="route-action-button route-delete-button"
                            variant="ghost"
                            size="xs"
                            disabled={
                              routeConfigReadOnly ||
                              isBusy ||
                              dirty ||
                              config.profiles.length <= 1
                            }
                            onClick={() => onDeleteRoute(profile.id)}
                            aria-label={`删除线路 ${profile.name}`}
                            title={
                              config.profiles.length <= 1
                                ? "至少需要保留一条线路"
                                : `删除线路 ${profile.name}`
                            }
                          >
                            <Trash size={13} aria-hidden="true" />
                          </Button>
                        </div>
                      )}
                    </div>
                    <div className="route-item-footer">
                      <div className="route-item-badges">
                        <Badge variant="secondary" size="xs">
                          {group?.models.length || 0} 模型
                        </Badge>
                        {!isOfficial && (
                          <Badge
                            variant={
                              routeConfigReadOnly
                                ? "secondary"
                                : group?.models.length
                                  ? "brand"
                                  : "secondary"
                            }
                            size="xs"
                          >
                            {routeConfigReadOnly
                              ? "配置只读"
                              : group?.models.length
                                ? "已接入路由"
                                : "待配置模型"}
                          </Badge>
                        )}
                        {(isOfficial || profile.supportsWebsockets) && (
                          <Badge variant="brand" size="xs">
                            WS
                          </Badge>
                        )}
                      </div>
                      {isOfficial && (
                        <div className="route-item-usage-toggle">
                          <span className="route-item-usage-label">额度显示</span>
                          <Switch
                            size="xs"
                            checked={showAccountUsageInHeader}
                            disabled={isBusy}
                            onCheckedChange={(checked) =>
                              onToggleAccountUsage?.(checked)}
                            aria-label="在账户区域显示额度"
                          />
                        </div>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
            </aside>
          )}

          <div className="route-catalog-pane">
            <div className="catalog-aggregate-heading">
              <div className="catalog-aggregate-title-wrap">
                <div className="catalog-aggregate-title">
                  <strong>
                    {routeConfigReadOnly ? "当前线路模型" : "统一模型目录"}
                  </strong>
                  <Badge variant="secondary" size="xs">
                    {totalModelCount} 个
                  </Badge>
                </div>
                <small>
                  {routeConfigReadOnly
                    ? "模型请求由 Codex 当前 Provider 直接处理"
                    : "选择模型时，本地路由会自动分发到所属供应商"}
                </small>
              </div>
            </div>

            {modelGroups.length > 1 && (
              <div
                className="provider-tabs-bar"
                role="tablist"
                aria-label="按供应商筛选模型"
              >
                <button
                  type="button"
                  id="provider-filter-tab-0"
                  role="tab"
                  aria-controls="provider-model-groups"
                  aria-selected={selectedProviderFilter === "all"}
                  tabIndex={selectedProviderFilter === "all" ? 0 : -1}
                  className={`provider-tab-pill ${selectedProviderFilter === "all" ? "active" : ""}`}
                  onClick={() => setSelectedProviderFilter("all")}
                  onKeyDown={handleProviderTabKeyDown}
                >
                  <span>全部</span>
                  <span className="tab-pill-count">{totalModelCount}</span>
                </button>
                {modelGroups.map((g, index) => (
                  <button
                    type="button"
                    key={g.providerId}
                    id={`provider-filter-tab-${index + 1}`}
                    role="tab"
                    aria-controls="provider-model-groups"
                    aria-selected={selectedProviderFilter === g.providerId}
                    tabIndex={selectedProviderFilter === g.providerId ? 0 : -1}
                    className={`provider-tab-pill ${selectedProviderFilter === g.providerId ? "active" : ""}`}
                    onClick={() => setSelectedProviderFilter(g.providerId)}
                    onKeyDown={handleProviderTabKeyDown}
                  >
                    <span className="tab-pill-icon" aria-hidden="true">
                      {g.official ? <IconShieldCheck size={12} /> : <Server size={12} />}
                    </span>
                    <span>{g.profile.name}</span>
                    <span className="tab-pill-count">{g.models.length}</span>
                  </button>
                ))}
              </div>
            )}

            <div
              id="provider-model-groups"
              className="provider-model-groups"
              role={modelGroups.length > 1 ? "tabpanel" : undefined}
            >
              {routeConfigReadOnly && displayedGroups.length === 0 ? (
                <div className="provider-model-empty">
                  <div className="provider-empty-content">
                    <IconCpu size={16} className="provider-empty-icon" aria-hidden="true" />
                    <span>尚未读取到 Codex 当前线路</span>
                  </div>
                </div>
              ) : null}
              {displayedGroups.map((group) => (
                <section
                  className="provider-model-group"
                  key={group.providerId}
                  aria-labelledby={`provider-model-${group.profile.id}`}
                >
                    <div className="provider-model-group-heading">
                      <div className="provider-heading-main">
                        <div
                          className={`provider-avatar-pill ${group.official ? "official" : "custom"}`}
                          aria-hidden="true"
                        >
                          {group.official ? (
                            <IconShieldCheck size={14} />
                          ) : (
                            <Server size={14} />
                          )}
                        </div>
                        <div className="provider-heading-text">
                          <strong id={`provider-model-${group.profile.id}`}>
                            {group.profile.name}
                          </strong>
                          <small>
                            {group.official ? "官方账号" : "第三方 API Key"}
                          </small>
                        </div>
                      </div>
                      <div className="provider-model-group-actions">
                        {routeConfigReadOnly && group.official && (
                          <div className="route-item-usage-toggle provider-model-usage-toggle">
                            <span className="route-item-usage-label">额度显示</span>
                            <Switch
                              size="xs"
                              checked={showAccountUsageInHeader}
                              disabled={isBusy}
                              onCheckedChange={(checked) =>
                                onToggleAccountUsage?.(checked)}
                              aria-label="在账户区域显示额度"
                            />
                          </div>
                        )}
                        <Badge variant={group.official ? "info" : "brand"} size="xs">
                          {group.models.length} 模型
                        </Badge>
                        <Button
                          variant="ghost"
                          size="xs"
                          disabled={!canSyncCurrentProvider || isBusy}
                          onClick={() => {
                            if (group.official && !routeConfigReadOnly) {
                              openEditRouteDialog(group.profile);
                            } else {
                              onFetchRouteModels(group.profile);
                            }
                          }}
                        >
                          <RefreshCw
                            size={12}
                            className={
                              busy === "fetch-route-models" &&
                              (routeConfigReadOnly ||
                                group.profile.id === config.activeProfileId)
                                ? "animate-spin"
                                : ""
                            }
                            aria-hidden="true"
                          />
                          同步模型
                        </Button>
                      </div>
                    </div>

                    {group.models.length > 0 ? (
                      <div className="provider-model-tags">
                        {group.models.map((model) => {
                          const isDefault = !routeConfigReadOnly && modelIdsEqual(group.defaultModel, model);
                          const displayName = group.official
                            ? officialDisplayNames.get(modelKey(model)) || model
                            : model;
                          return (
                            <button
                              type="button"
                              key={`${group.providerId}:${model}`}
                              className={`model-tag-pill${isDefault ? " is-default" : ""}`}
                              disabled={routeConfigReadOnly || isBusy || dirty || isDefault}
                              onClick={() => onSetDefaultModel(group.profile.id, model)}
                              title={
                                routeConfigReadOnly
                                  ? displayName
                                  : isDefault
                                  ? `${displayName}（当前默认模型）`
                                  : `点击设为默认模型：${displayName}`
                              }
                              aria-label={
                                routeConfigReadOnly
                                  ? displayName
                                  : isDefault
                                  ? `${displayName}，当前默认模型`
                                  : `设 ${displayName} 为默认模型`
                              }
                            >
                              <span className="model-tag-indicator" aria-hidden="true">
                                {isDefault ? (
                                  <Check size={11} strokeWidth={2.5} />
                                ) : (
                                  <span className="model-tag-dot" />
                                )}
                              </span>
                              <span className="model-tag-name">{displayName}</span>
                              {isDefault && (
                                <span className="model-tag-badge">默认</span>
                              )}
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
                          variant="outline"
                          size="xs"
                          disabled={!canSyncCurrentProvider || isBusy}
                          onClick={() => {
                            if (group.official && !routeConfigReadOnly) {
                              openEditRouteDialog(group.profile);
                            } else {
                              onFetchRouteModels(group.profile);
                            }
                          }}
                        >
                          <RefreshCw size={12} aria-hidden="true" />
                          {routeConfigReadOnly
                            ? "同步模型"
                            : group.official
                              ? "配置官方模型"
                              : "同步或手动添加"}
                        </Button>
                      </div>
                    )}
                </section>
              ))}
            </div>
          </div>
        </div>

        <div className="readonly-note">
          <IconInfoCircle size={14} className="readonly-note-icon" aria-hidden="true" />
          <span className="readonly-note-text">
            {routeConfigReadOnly
              ? "本地路由已关闭；仅展示 Codex 当前线路，可同步模型，线路地址、密钥和协议保持只读"
              : "所有第三方线路通过 Codey 本地路由同时生效，线路列表仅用于管理配置"}
          </span>
          <Badge
            variant={routeConfigReadOnly ? "secondary" : "brand"}
            className="readonly-note-tag"
          >
            {routeConfigReadOnly ? "当前线路" : "一次性"}
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
            zIndex={SETTINGS_OVERLAY_Z_INDEX}
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
                        <label className="official-model-option" key={model}>
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
                        </label>
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
                    getPopupContainer={() => popupContainer ?? document.body}
                    zIndex={SETTINGS_OVERLAY_Z_INDEX}
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
                          启用 WebSocket 双向长连接，降低流式首字延迟
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
                    visible={routeApiKeyVisible}
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
                    visibilityToggleIcon={({ reveal }) =>
                      reveal ? (
                        <IconEyeOff size={16} aria-hidden="true" />
                      ) : (
                        <IconEye size={16} aria-hidden="true" />
                      )
                    }
                    visibilityToggleButtonProps={{
                      disabled: isBusy || !routeDraft.apiKey.trim(),
                      title: routeApiKeyVisible ? "隐藏 API Key" : "显示 API Key",
                      "aria-label": routeApiKeyVisible
                        ? "隐藏线路 API Key"
                        : "显示线路 API Key",
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
