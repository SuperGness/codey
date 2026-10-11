import type { Config, ModelState, Profile, ProviderStatus } from "./App.types";
import { modelKey, uniqueModelIds } from "./modelIds";
import { routeModelAlias, routeProviderId } from "./modelRoutes";
import { fallbackRouteShortName, routeDisplayPrefix } from "./routeShortNames";
import { resolveModelReasoningEfforts } from "./modelReasoningEfforts";

const THIRD_PARTY_REASONING_EFFORTS = ["low", "medium", "high", "xhigh"];
const THIRD_PARTY_REASONING_EFFORT_ALLOWLIST = [
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

function thirdPartyReasoningEfforts(efforts?: readonly string[]) {
  const supported = (efforts || []).filter((effort) =>
    THIRD_PARTY_REASONING_EFFORT_ALLOWLIST.includes(effort)
  );
  return supported.length > 0 ? supported : THIRD_PARTY_REASONING_EFFORTS;
}

function metadataForModel<T>(metadata: ReadonlyMap<string, T>, modelId: string) {
  const exact = metadata.get(modelKey(modelId));
  if (exact) return exact;
  const separator = modelId.indexOf("/");
  return separator >= 0
    ? metadata.get(modelKey(modelId.slice(separator + 1)))
    : undefined;
}

export type SubagentModelOption = {
  value: string;
  label: string;
  modelId: string;
  routeId: string;
  providerId: string;
  routeName: string;
  routePrefix: string;
  official: boolean;
  supportedReasoningEfforts: string[];
  defaultReasoningEffort: string;
};

function enabledModelsForRoute(
  config: Config,
  modelState: ModelState,
  profile: Profile,
) {
  const providerId = routeProviderId(profile);
  const configuredModels = config.selectedModelsByProvider[providerId] || [];
  if (profile.authMode !== "officialAccount") {
    return uniqueModelIds([
      ...configuredModels,
      ...(config.declaredOfficialModelsByProvider[providerId] || []),
    ]);
  }

  return uniqueModelIds(
    configuredModels.length > 0
      ? configuredModels
      : [...modelState.officialModelIds, ...modelState.officialModels.map((model) => model.slug)],
  );
}

export function buildSubagentModelOptions(
  config: Config | null,
  modelState: ModelState,
  officialAccountAvailable: boolean,
  currentProvider: ProviderStatus["provider"] | null = null,
) {
  if (!config) return [];

  const officialMetadata = new Map(
    modelState.officialModels.map((model) => [modelKey(model.slug), model]),
  );
  const thirdPartyMetadata = new Map(
    (modelState.thirdPartyModelMetadata || []).map((model) => [
      modelKey(model.slug),
      model,
    ]),
  );
  const seenAliases = new Set<string>();
  const options: SubagentModelOption[] = [];

  const appendOption = ({
    modelId,
    value,
    routeId,
    providerId,
    routeName,
    routePrefix,
    official,
  }: {
    modelId: string;
    value: string;
    routeId: string;
    providerId: string;
    routeName: string;
    routePrefix: string;
    official: boolean;
  }) => {
    const valueKey = modelKey(value);
    if (seenAliases.has(valueKey)) return;
    seenAliases.add(valueKey);

    const officialModelMetadata = metadataForModel(officialMetadata, modelId);
    const thirdPartyModelMetadata = metadataForModel(thirdPartyMetadata, modelId);
    const efforts = official && officialModelMetadata
      ? officialModelMetadata.supportedReasoningEfforts
      : thirdPartyReasoningEfforts(
        thirdPartyModelMetadata?.supportedReasoningEfforts ??
          officialModelMetadata?.supportedReasoningEfforts,
      );
    const profile = config.profiles.find((candidate) => candidate.id === routeId);
    const capability = !official && config.localRouterEnabled && profile?.pluginOwnerId
      ? Object.entries(profile.pluginRouteSpec?.modelReasoningEfforts ?? {}).find(
          ([model]) => modelKey(model) === modelKey(modelId),
        )?.[1]
      : undefined;
    const stored = Object.entries(config.modelReasoningEffortsByProvider?.[providerId] ?? {})
      .find(([model]) => modelKey(model) === modelKey(modelId))?.[1];
    const boundedEfforts = capability
      ? resolveModelReasoningEfforts(efforts, stored, capability).efforts.map((effort) => effort.value)
      : efforts;
    const supportedReasoningEfforts = boundedEfforts.length > 0 ? boundedEfforts : ["low"];
    const requestedDefaultEffort =
      official && officialModelMetadata
        ? officialModelMetadata.defaultReasoningEffort || supportedReasoningEfforts[0]
        : thirdPartyModelMetadata?.defaultReasoningEffort || "low";
    options.push({
      value,
      label: official && officialModelMetadata
        ? officialModelMetadata.displayName
        : modelId,
      modelId,
      routeId,
      providerId,
      routeName,
      routePrefix,
      official,
      supportedReasoningEfforts,
      defaultReasoningEffort:
        supportedReasoningEfforts.includes(requestedDefaultEffort)
          ? requestedDefaultEffort
          : supportedReasoningEfforts[0],
    });
  };

  if (!config.localRouterEnabled) {
    const matchingProfile = currentProvider
      ? config.profiles.find((profile) =>
        profile.id === currentProvider.id ||
        routeProviderId(profile) === currentProvider.id)
      : config.profiles.find((profile) => profile.id === config.activeProfileId);
    if (matchingProfile?.enabled === false) return [];
    const providerId = currentProvider?.id ||
      (matchingProfile ? routeProviderId(matchingProfile) : "");
    if (!providerId) return [];
    const official = currentProvider?.official ??
      matchingProfile?.authMode === "officialAccount";
    const routeName = matchingProfile
      ? matchingProfile.name.trim()
      : currentProvider?.name.trim() || providerId;
    const routePrefix = matchingProfile?.shortName?.trim() || fallbackRouteShortName(routeName);
    const officialModels = modelState.officialModels
      .filter((model) => model.supported)
      .map((model) => model.slug);
    const models = official
      ? uniqueModelIds(
        officialModels.length > 0 ? officialModels : modelState.officialModelIds,
      )
      : modelState.thirdPartyModels;
    for (const modelId of models) {
      appendOption({
        modelId,
        value: modelId,
        routeId: matchingProfile?.id || providerId,
        providerId,
        routeName,
        routePrefix,
        official,
      });
    }
    return options;
  }

  for (const profile of config.profiles) {
    if (profile.enabled === false) continue;
    const official = profile.authMode === "officialAccount";
    // 存储账号的官方线路自带凭据，默认登录缺失时仍能由本地路由提供模型。
    if (official && !officialAccountAvailable && !profile.officialAccountId) continue;

    const providerId = routeProviderId(profile);
    for (const modelId of enabledModelsForRoute(config, modelState, profile)) {
      appendOption({
        modelId,
        value: routeModelAlias(profile, modelId),
        routeId: profile.id,
        providerId,
        routeName: profile.name.trim(),
        routePrefix: routeDisplayPrefix(profile),
        official,
      });
    }
  }

  return options;
}

export function resolveSubagentModelOption(
  options: readonly SubagentModelOption[],
  requestedValue: string,
  preferredProviderId?: string,
) {
  const requested = requestedValue.trim();
  if (!requested) return undefined;
  const requestedKey = modelKey(requested);

  let preferredRouteMatch: SubagentModelOption | undefined;
  let legacyMatch: SubagentModelOption | undefined;
  let legacyMatchIsUnique = true;
  for (const option of options) {
    if (modelKey(option.value) === requestedKey) return option;
    if (modelKey(option.modelId) !== requestedKey) continue;
    if (!preferredRouteMatch && option.providerId === preferredProviderId) {
      preferredRouteMatch = option;
    }
    if (legacyMatch) {
      legacyMatchIsUnique = false;
    } else {
      legacyMatch = option;
    }
  }
  return preferredRouteMatch || (legacyMatchIsUnique ? legacyMatch : undefined);
}
