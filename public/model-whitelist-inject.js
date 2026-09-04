// Keep Codex's native model allowlist aligned with the current Codey channel.
(() => {
  const patchVersion = "47";
  const nativeSelectionOnly = window.__codeyNativeModelSelectionOnly === true;
  const officialProviderId = "openai";
  const localRouterProviderId = "codey_router";
  const legacyOfficialRouteProviderIds = new Set([
    officialProviderId,
    "local-official",
  ]);
  const gatewayProviderIds = new Set([
    officialProviderId,
    localRouterProviderId,
  ]);
  const existingPatch = window.__codeyModelWhitelistPatch;
  if (existingPatch?.version === patchVersion
    && existingPatch.nativeSelectionOnly === nativeSelectionOnly) {
    void existingPatch.refresh();
    return;
  }
  existingPatch?.dispose?.();

  const modelConfigId = "107580212";
  const modelCatalogPath = "/codex-model-catalog";
  const fastServiceTierId = "priority";
  const fastSpeedTierId = "fast";
  const interactionEvents = ["pointerdown", "click", "focusin"];
  const routeSelectionEvents = new Set(["pointerdown", "click"]);
  const groupedMenuStyleId = "codey-model-route-menu-style";
  const groupedMenuSelector = "[role='menu'], [role='listbox']";
  const groupedMenuItemSelector = "[role='menuitem'], [role='menuitemradio'], [role='option']";
  const modelQueryKey = ["models", "list"];
  const modelResponseEvent = "message";
  const modelRequestEvent = "codex-message-from-view";
  const routableOutgoingMessageTypes = new Set([
    "mcp-request",
    "thread-prewarm-start",
  ]);
  const modelBoundRequestMethods = new Set([
    "thread/start",
    "thread/resume",
    "thread/fork",
    "thread/settings/update",
    "turn/start",
  ]);
  const threadProviderRequestMethods = new Set([
    "thread/start",
    "thread/resume",
    "thread/fork",
  ]);
  const providerBoundExistingThreadMethods = new Set([
    "thread/resume",
    "thread/settings/update",
    "turn/start",
  ]);
  const routeMetadataParam = "responsesapiClientMetadata";
  const routeMetadataKey = "codey_route";
  const persistedThreadRoutesKey = "codey.thread-route-bindings.v1";
  let catalog = {
    loaded: false,
    models: [],
    defaultModel: "",
    modelMetadata: {},
    routeMetadata: {},
    modelNamesByKey: new Map(),
    routes: [],
    routeBySelectorKey: new Map(),
    routesBySourceKey: new Map(),
    routeByRouteProviderSource: new Map(),
    routeByAnyProviderSource: new Map(),
  };
  let refreshTimer = 0;
  let refreshUntil = 0;
  let refreshRetryDelay = 120;
  let refreshDeliveryInFlight = false;
  let catalogLoadPromise = null;
  let catalogRevision = 0;
  let disposed = false;
  const fullReactDiscoveryIntervalMs = 10_000;
  const maxTrackedModelListRequests = 256;
  const maxKnownModelQueryClients = 8;
  let nextFullReactDiscoveryAt = 0;
  const modelListRequestIds = new Set();
  const knownModelQueryClients = new Set();
  let originalDispatchEvent = null;
  let patchedDispatchEvent = null;
  let groupedMenuTimer = 0;
  let groupedMenuObserver = null;
  const groupedMenuTextObservers = new Map();
  const patchedProviderKey = Symbol("codeyPatchedModelProvider");
  const patchedRouteKey = Symbol("codeyPatchedRoute");
  const blockedProviderRequestKey = Symbol("codeyBlockedProviderRequest");
  // Codex persists a thread's original provider in rollout data, while Codey
  // can resume that same thread through the local router for this process.
  // Keep the two identities separate so a later thread/list response cannot
  // erase a successful runtime migration.
  const threadPersistedProviders = new Map();
  const threadRuntimeProviders = new Map();
  const threadRoutes = new Map();
  const pendingThreadRequests = new Map();
  const maxTrackedThreadProviders = 2048;
  const maxPendingThreadRequests = 256;
  const pendingRouteIntentMaxAgeMs = 5 * 60 * 1000;
  let pendingRouteIntent = null;
  const supersededModelMenuLabels = new Map();
  const maxSupersededModelMenuLabels = 512;
  const supersededDefaultRoutes = [];
  const maxSupersededDefaultRoutes = 8;
  let providerMismatchNoticeTimer = 0;
  let deliveryState = {
    revision: 0,
    statsigClients: 0,
    notifiedClients: 0,
    queryClients: 0,
    queryEntries: 0,
    reactContainers: 0,
    responsePatchInstalled: false,
  };

  const rememberBounded = (set, value, limit) => {
    set.delete(value);
    set.add(value);
    while (set.size > limit) {
      set.delete(set.values().next().value);
    }
  };
  const rememberBoundedMap = (map, key, value, limit) => {
    map.delete(key);
    map.set(key, value);
    while (map.size > limit) {
      map.delete(map.keys().next().value);
    }
  };
  const rememberBoundedThreadPersistedProvider = (threadId, providerId) => {
    rememberBoundedMap(
      threadPersistedProviders,
      threadId,
      providerId,
      maxTrackedThreadProviders,
    );
  };
  const rememberBoundedThreadRuntimeProvider = (threadId, providerId) => {
    rememberBoundedMap(
      threadRuntimeProviders,
      threadId,
      providerId,
      maxTrackedThreadProviders,
    );
  };
  const persistThreadRoutes = () => {
    try {
      window.localStorage?.setItem(
        persistedThreadRoutesKey,
        JSON.stringify(Array.from(threadRoutes.entries())),
      );
    } catch {
      // Routing remains safe for this launch when renderer storage is unavailable.
    }
  };
  const rememberBoundedThreadRoute = (threadId, route) => {
    const routeProviderId = requestProviderId(route?.routeProviderId);
    const sourceModel = typeof route?.sourceModel === "string"
      ? route.sourceModel.trim()
      : "";
    if (!threadId || !routeProviderId || !sourceModel) return;
    const previous = threadRoutes.get(threadId);
    const changed = !previous
      || modelKey(previous.routeProviderId) !== modelKey(routeProviderId)
      || modelKey(previous.sourceModel) !== modelKey(sourceModel);
    rememberBoundedMap(
      threadRoutes,
      threadId,
      { routeProviderId, sourceModel },
      maxTrackedThreadProviders,
    );
    // Refresh the in-memory LRU on every turn, but avoid synchronously
    // serializing the entire binding table when the persisted value is unchanged.
    if (changed) persistThreadRoutes();
  };
  const restoreThreadRoutes = () => {
    try {
      const entries = JSON.parse(window.localStorage?.getItem(persistedThreadRoutesKey) || "[]");
      if (!Array.isArray(entries)) return;
      for (const entry of entries.slice(-maxTrackedThreadProviders)) {
        if (!Array.isArray(entry) || entry.length !== 2) continue;
        const threadId = typeof entry[0] === "string" ? entry[0].trim() : "";
        const route = entry[1];
        const routeProviderId = requestProviderId(route?.routeProviderId);
        const sourceModel = typeof route?.sourceModel === "string"
          ? route.sourceModel.trim()
          : "";
        if (threadId && routeProviderId && sourceModel) {
          threadRoutes.set(threadId, { routeProviderId, sourceModel });
        }
      }
    } catch {
      // Ignore stale or user-cleared renderer storage.
    }
  };

  const modelKey = (value) => String(value || "").trim().toLowerCase();
  const routeFromNestedIndex = (index, providerId, sourceModel) => {
    const providerKey = modelKey(providerId);
    const sourceKey = modelKey(sourceModel);
    return providerKey && sourceKey
      ? index.get(providerKey)?.get(sourceKey) || null
      : null;
  };
  const addRouteToNestedIndex = (index, providerId, sourceModel, route) => {
    const providerKey = modelKey(providerId);
    const sourceKey = modelKey(sourceModel);
    if (!providerKey || !sourceKey) return;
    let bySource = index.get(providerKey);
    if (!bySource) {
      bySource = new Map();
      index.set(providerKey, bySource);
    }
    if (!bySource.has(sourceKey)) bySource.set(sourceKey, route);
  };
  const uniqueModelNames = (values) => {
    const seen = new Set();
    return (Array.isArray(values) ? values : []).reduce((models, value) => {
      if (typeof value !== "string") return models;
      const model = value.trim();
      const key = modelKey(model);
      if (!key || seen.has(key)) return models;
      seen.add(key);
      models.push(model);
      return models;
    }, []);
  };
  const canonicalModelName = (models, value) => {
    const key = modelKey(value);
    return key ? models.find((model) => modelKey(model) === key) || "" : "";
  };
  const requestProviderId = (providerId) => (
    typeof providerId === "string" ? providerId.trim() : ""
  );
  const paramsProviderId = (params) => {
    if (!params || typeof params !== "object") return "";
    for (const value of [params.modelProvider, params.model_provider]) {
      const providerId = requestProviderId(value);
      if (providerId) return providerId;
    }
    return "";
  };
  const isGatewayProviderId = (providerId) => (
    gatewayProviderIds.has(modelKey(providerId))
  );
  const isOfficialRoute = (route) => (
    route?.officialAccount === true
    || legacyOfficialRouteProviderIds.has(modelKey(route?.routeProviderId))
  );
  const providersAreCompatible = (method, currentProviderId, targetProviderId) => (
    modelKey(currentProviderId) === modelKey(targetProviderId)
    || (
      method === "thread/resume"
      && modelKey(targetProviderId) === localRouterProviderId
    )
  );
  const markPatchedProvider = (params, providerId) => {
    try {
      Object.defineProperty(params, patchedProviderKey, {
        value: providerId,
        configurable: true,
      });
    } catch {
      // Ignore non-extensible request objects; the serialized request payload is unchanged.
    }
    return params;
  };
  const markPatchedRoute = (params, route) => {
    try {
      Object.defineProperty(params, patchedRouteKey, {
        value: route,
        configurable: true,
      });
    } catch {
      // Ignore non-extensible request objects; the serialized request payload is unchanged.
    }
    return params;
  };
  const threadIdFromParams = (params) => (
    typeof params?.threadId === "string" ? params.threadId.trim() : ""
  );
  const knownThreadProvider = (params) => {
    const threadId = threadIdFromParams(params);
    return requestProviderId(threadRuntimeProviders.get(threadId))
      || requestProviderId(threadPersistedProviders.get(threadId))
      || paramsProviderId(params);
  };
  const markBlockedProviderRequest = (params, detail) => {
    try {
      Object.defineProperty(params, blockedProviderRequestKey, {
        value: detail,
        configurable: true,
      });
    } catch {
      // Frozen payloads are cloned by routedRequestParams before this point.
    }
    return params;
  };
  const routedRequestParams = (method, source, model, providerId, route) => {
    const usesCodeyRoute = Boolean(requestProviderId(route?.routeProviderId));
    const routeProviderId = requestProviderId(route?.routeProviderId);
    const threadId = threadIdFromParams(source);
    const currentProviderId = providerBoundExistingThreadMethods.has(method)
      ? (threadId ? knownThreadProvider(source) : paramsProviderId(source))
      : "";
    // A task that has not yet resumed through Codey's carrier can still use
    // its original OpenAI provider for an official model. Keep that request
    // direct and translate the renderer-only selector back to the real model
    // id. Other cross-provider choices first migrate through `thread/resume`.
    const preservesLegacyOfficialCarrier = method !== "thread/resume"
      && modelKey(currentProviderId) === officialProviderId
      && isOfficialRoute(route);
    const routedProviderId = preservesLegacyOfficialCarrier
      ? officialProviderId
      : (
          usesCodeyRoute
          || (method === "thread/resume" && Boolean(currentProviderId))
        )
        ? localRouterProviderId
        : providerId;
    // Route-qualified ids are UI selectors, not valid ChatGPT-account model
    // ids. Keep the route in metadata while every official request uses the
    // upstream model id.
    const routedModel = isOfficialRoute(route)
      ? cleanText(route?.sourceModel) || model
      : model;
    const next = { ...source };
    if (routedModel || Object.hasOwn(source, "model")) next.model = routedModel;
    delete next.model_provider;
    if (threadProviderRequestMethods.has(method)) {
      if (routedProviderId) next.modelProvider = routedProviderId;
      else delete next.modelProvider;
    } else {
      // `turn/start` has no modelProvider field. New, forked, and resumed
      // threads may use the HTTP-only Codey carrier; the persisted rollout
      // metadata remains untouched by a resume-time override.
      delete next.modelProvider;
    }
    if (method === "turn/start" && routeProviderId && !preservesLegacyOfficialCarrier) {
      const existingMetadata = source[routeMetadataParam];
      next[routeMetadataParam] = {
        ...(existingMetadata && typeof existingMetadata === "object"
          ? existingMetadata
          : {}),
        [routeMetadataKey]: routeProviderId,
      };
    } else if (preservesLegacyOfficialCarrier) {
      const existingMetadata = source[routeMetadataParam];
      if (existingMetadata && typeof existingMetadata === "object") {
        const nextMetadata = { ...existingMetadata };
        delete nextMetadata[routeMetadataKey];
        if (Object.keys(nextMetadata).length > 0) next[routeMetadataParam] = nextMetadata;
        else delete next[routeMetadataParam];
      }
    }
    let blocked = false;
    if (providerBoundExistingThreadMethods.has(method) && routedProviderId) {
      // `turn/start` has no modelProvider field, so provider migration must
      // happen during `thread/resume`. Resume may move any persisted provider
      // onto Codey's runtime router; later turns can then switch routes safely.
      if (
        threadId
        && currentProviderId
        && !providersAreCompatible(method, currentProviderId, routedProviderId)
      ) {
        blocked = true;
        markBlockedProviderRequest(next, {
          method,
          threadId,
          model,
          targetProviderId: routedProviderId,
          currentProviderId,
          routeName: cleanText(route?.routeName),
          reason: "provider_migration_required",
        });
      }
    }
    if (!blocked && threadId && routeProviderId) {
      rememberBoundedThreadRoute(threadId, route);
    }
    markPatchedRoute(next, route);
    return markPatchedProvider(next, routedProviderId);
  };
  const cleanText = (value) => (
    typeof value === "string" ? value.trim().replace(/\s+/g, " ") : ""
  );
  const metadataText = (metadata, key) => cleanText(
    metadata && typeof metadata === "object" ? metadata[key] : "",
  );
  const metadataBoolean = (metadata, key) => (
    Boolean(metadata && typeof metadata === "object" && metadata[key] === true)
  );
  const displayNameParts = (displayName) => {
    const separator = displayName.indexOf(" / ");
    return separator > 0
      ? {
        routeName: cleanText(displayName.slice(0, separator)),
        modelName: cleanText(displayName.slice(separator + 3)),
      }
      : { routeName: "", modelName: cleanText(displayName) };
  };
  const modelFallbackName = (modelName) => {
    const separator = modelName.indexOf("/");
    return cleanText(separator > 0 ? modelName.slice(separator + 1) : modelName);
  };

  const sameModelNames = (left, right) => (
    Array.isArray(left)
    && left.length === right.length
    && left.every((value, index) => value === right[index])
  );

  const normalizedCatalog = (value) => {
    if (
      !value
      || typeof value !== "object"
      || !["ok", "not_configured"].includes(value.status)
      || (value.native_selection_only === true) !== nativeSelectionOnly
    ) {
      return null;
    }
    const models = uniqueModelNames(value.models);
    if (nativeSelectionOnly && models.length === 0) return null;
    const requestedDefault = [value.default_model, value.model]
      .map((model) => canonicalModelName(models, model))
      .find(Boolean);
    const modelMetadata = Object.fromEntries(
      (Array.isArray(value.model_metadata) ? value.model_metadata : [])
        .flatMap((metadata) => {
          if (
            !metadata
            || typeof metadata !== "object"
            || typeof metadata.model !== "string"
          ) return [];
          const model = canonicalModelName(models, metadata.model);
          return model ? [[model, metadata]] : [];
        }),
    );
    const routeMetadata = nativeSelectionOnly ? {} : Object.fromEntries(
      Object.entries(modelMetadata).map(([model, metadata]) => {
        const providerId = typeof metadata.provider_id === "string"
          ? metadata.provider_id.trim()
          : "";
        let sourceModel = [metadata.upstream_model, metadata.source_model]
          .find((candidate) => typeof candidate === "string" && candidate.trim())
          ?.trim() || "";
        const explicitRouteProviderId = typeof metadata.route_provider_id === "string"
          ? metadata.route_provider_id.trim()
          : "";
        const selectorSeparator = model.indexOf("/");
        const routeProviderId = explicitRouteProviderId
          || (!isGatewayProviderId(providerId) ? providerId : "")
          || (selectorSeparator > 0 ? model.slice(0, selectorSeparator).trim() : "")
          || (modelKey(providerId) === officialProviderId ? officialProviderId : "");
        const selectorPrefix = `${routeProviderId}/`;
        if (
          !metadataText(metadata, "upstream_model")
          && modelKey(sourceModel) === modelKey(model)
          && sourceModel.toLowerCase().startsWith(selectorPrefix.toLowerCase())
        ) {
          sourceModel = sourceModel.slice(selectorPrefix.length).replace(/#\d+$/, "").trim();
        }
        const routeName = metadataText(metadata, "route_name")
          || displayNameParts(metadataText(metadata, "display_name")).routeName
          || providerId;
        return [model, {
          selectorModel: model,
          providerId,
          routeProviderId,
          sourceModel,
          routeName,
          officialAccount: metadataBoolean(metadata, "official_account"),
        }];
      }).filter(([, route]) => (
        route.providerId && route.routeProviderId && route.sourceModel
      )),
    );
    for (const model of models) {
      if (nativeSelectionOnly) break;
      if (routeMetadata[model]) continue;
      const separator = model.indexOf("/");
      if (separator <= 0) continue;
      const providerId = model.slice(0, separator).trim();
      const sourceModel = model.slice(separator + 1).replace(/#\d+$/, "").trim();
      if (providerId && sourceModel) {
        routeMetadata[model] = {
          selectorModel: model,
          providerId: localRouterProviderId,
          routeProviderId: providerId,
          sourceModel,
          routeName: providerId,
        };
      }
    }
    const modelNamesByKey = new Map(models.map((model) => [modelKey(model), model]));
    const routes = [];
    const routeBySelectorKey = new Map();
    const routesBySourceKey = new Map();
    const routeByRouteProviderSource = new Map();
    const routeByAnyProviderSource = new Map();
    for (const model of models) {
      const route = routeMetadata[model];
      if (!route) continue;
      routes.push(route);
      routeBySelectorKey.set(modelKey(model), route);
      const sourceKey = modelKey(route.sourceModel);
      if (sourceKey) {
        const sourceRoutes = routesBySourceKey.get(sourceKey) || [];
        sourceRoutes.push(route);
        routesBySourceKey.set(sourceKey, sourceRoutes);
      }
      addRouteToNestedIndex(
        routeByRouteProviderSource,
        route.routeProviderId,
        route.sourceModel,
        route,
      );
      addRouteToNestedIndex(
        routeByAnyProviderSource,
        route.routeProviderId,
        route.sourceModel,
        route,
      );
      addRouteToNestedIndex(
        routeByAnyProviderSource,
        route.providerId,
        route.sourceModel,
        route,
      );
    }
    return {
      loaded: true,
      models,
      defaultModel: requestedDefault?.trim() || models[0] || "",
      modelMetadata,
      routeMetadata,
      modelNamesByKey,
      routes,
      routeBySelectorKey,
      routesBySourceKey,
      routeByRouteProviderSource,
      routeByAnyProviderSource,
    };
  };

  const reasoningEffortName = (value) => (
    typeof value === "string"
      ? value.trim()
      : typeof value?.reasoningEffort === "string"
        ? value.reasoningEffort.trim()
        : ""
  );

  const reasoningEffortDescriptors = (values) => uniqueModelNames(
    (Array.isArray(values) ? values : []).map(reasoningEffortName),
  ).map((reasoningEffort) => ({
    reasoningEffort,
    description: `${reasoningEffort} effort`,
  }));

  const sameReasoningEffortNames = (left, right) => (
    Array.isArray(left)
    && left.length === right.length
    && left.every((value, index) => (
      reasoningEffortName(value) === reasoningEffortName(right[index])
    ))
  );

  const fallbackReasoningEfforts = () => reasoningEffortDescriptors([
    "minimal",
    "low",
    "medium",
    "high",
    "xhigh",
  ]);

  const fastServiceTier = () => ({
    id: fastServiceTierId,
    name: "Fast",
    description: "1.5x speed, increased usage",
  });

  const nativeFastServiceTiers = (value) => {
    const tiers = Array.isArray(value) ? value : [];
    const fastTierIndex = tiers.findIndex((tier) => tier?.id === fastServiceTierId);
    if (fastTierIndex < 0) return [...tiers, fastServiceTier()];
    const currentFastTier = tiers[fastTierIndex];
    if (
      !currentFastTier
      || typeof currentFastTier !== "object"
      || !Object.hasOwn(currentFastTier, "iconKind")
    ) return tiers;
    const nativeFastTier = { ...currentFastTier };
    delete nativeFastTier.iconKind;
    const nextTiers = [...tiers];
    nextTiers[fastTierIndex] = nativeFastTier;
    return nextTiers;
  };

  const nativeFastSpeedTiers = (value) => {
    const tiers = Array.isArray(value) ? value : [];
    return tiers.includes(fastSpeedTierId) ? tiers : [...tiers, fastSpeedTierId];
  };

  const modelPresentationFromCatalog = (sourceCatalog, modelName, current = null) => {
    const metadata = sourceCatalog.modelMetadata[modelName];
    const route = sourceCatalog.routeMetadata[modelName];
    const metadataDisplayName = metadataText(metadata, "display_name");
    const displayParts = displayNameParts(metadataDisplayName);
    const routeName = metadataText(metadata, "route_name")
      || route?.routeName
      || displayParts.routeName
      || cleanText(route?.providerId)
      || "";
    const sourceModel = metadataText(metadata, "source_model") || route?.sourceModel || "";
    const modelLabel = metadataText(metadata, "model_display_name")
      || sourceModel
      || displayParts.modelName
      || modelFallbackName(modelName);
    const currentDisplayName = cleanText(current?.displayName);
    const displayName = metadataDisplayName
      || (routeName && modelLabel ? `${routeName} / ${modelLabel}` : "")
      || currentDisplayName
      || modelName;
    return {
      routeName,
      modelName: modelLabel,
      displayName,
      providerId: cleanText(route?.providerId) || metadataText(metadata, "provider_id"),
      sourceModel: sourceModel || modelName,
    };
  };

  const modelPresentation = (modelName, current = null) => (
    modelPresentationFromCatalog(catalog, modelName, current)
  );

  const modelDescriptor = (modelName, current = null) => {
    const metadata = catalog.modelMetadata[modelName];
    const presentation = modelPresentation(modelName, current);
    const displayName = presentation.displayName;
    const supportedReasoningEfforts = reasoningEffortDescriptors(
      metadata?.supported_reasoning_efforts,
    );
    if (nativeSelectionOnly && current && supportedReasoningEfforts.length === 0) {
      return current.hidden === true ? { ...current, hidden: false } : current;
    }
    const currentReasoningEfforts = reasoningEffortDescriptors(
      current?.supportedReasoningEfforts,
    );
    const resolvedReasoningEfforts = supportedReasoningEfforts.length > 0
      ? supportedReasoningEfforts
      : currentReasoningEfforts.length > 0
        ? currentReasoningEfforts
        : fallbackReasoningEfforts();
    const supportedNames = resolvedReasoningEfforts.map(reasoningEffortName);
    const requestedDefault = [
      metadata?.default_reasoning_effort,
      current?.defaultReasoningEffort,
      "medium",
      "low",
      supportedNames[0],
    ].find((effort) => (
      typeof effort === "string" && supportedNames.includes(effort.trim())
    ));
    const defaultReasoningEffort = requestedDefault?.trim() || "medium";
    if (nativeSelectionOnly) {
      if (current) {
        return (
          current.hidden !== true
          && current.defaultReasoningEffort === defaultReasoningEffort
          && sameReasoningEffortNames(
            current.supportedReasoningEfforts,
            resolvedReasoningEfforts,
          )
        ) ? current : {
          ...current,
          hidden: false,
          defaultReasoningEffort,
          supportedReasoningEfforts: resolvedReasoningEfforts,
        };
      }
      return {
        model: modelName,
        id: modelName,
        slug: modelName,
        name: metadata?.display_name || modelName,
        displayName: metadata?.display_name || modelName,
        hidden: false,
        isDefault: false,
        defaultReasoningEffort,
        supportedReasoningEfforts: resolvedReasoningEfforts,
      };
    }
    return {
      ...(current && typeof current === "object" ? current : {}),
      model: modelName,
      id: typeof current?.id === "string" && current.id ? current.id : modelName,
      slug: typeof current?.slug === "string" && current.slug ? current.slug : modelName,
      name: displayName
        || (typeof current?.name === "string" && current.name ? current.name : modelName),
      displayName,
      routeName: presentation.routeName,
      providerName: presentation.routeName,
      providerId: presentation.providerId,
      sourceModel: presentation.sourceModel,
      codeyRouteName: presentation.routeName,
      codeyModelName: presentation.modelName,
      description: typeof current?.description === "string" && current.description
        ? current.description
        : presentation.routeName || "Custom model",
      hidden: false,
      isDefault: modelName === catalog.defaultModel,
      defaultReasoningEffort,
      supportedReasoningEfforts: resolvedReasoningEfforts,
      serviceTiers: nativeFastServiceTiers(current?.serviceTiers),
      additionalSpeedTiers: nativeFastSpeedTiers(current?.additionalSpeedTiers),
      defaultServiceTier: Object.hasOwn(current || {}, "defaultServiceTier")
        ? current.defaultServiceTier
        : null,
    };
  };

  const sameReasoningEfforts = (left, right) => (
    Array.isArray(left)
    && left.every((value) => (
      value
      && typeof value === "object"
      && typeof value.reasoningEffort === "string"
      && value.reasoningEffort.trim()
    ))
    && sameReasoningEffortNames(left, right)
  );

  const sameModelMetadata = (left, right, models) => models.every((modelName) => {
    const leftMetadata = left[modelName];
    const rightMetadata = right[modelName];
    if (!leftMetadata || !rightMetadata) return leftMetadata === rightMetadata;
    return (
      leftMetadata.default_reasoning_effort === rightMetadata.default_reasoning_effort
      && leftMetadata.display_name === rightMetadata.display_name
      && leftMetadata.route_name === rightMetadata.route_name
      && leftMetadata.route_prefix === rightMetadata.route_prefix
      && leftMetadata.model_display_name === rightMetadata.model_display_name
      && leftMetadata.provider_id === rightMetadata.provider_id
      && leftMetadata.source_model === rightMetadata.source_model
      && leftMetadata.route_provider_id === rightMetadata.route_provider_id
      && leftMetadata.official_account === rightMetadata.official_account
      && leftMetadata.upstream_model === rightMetadata.upstream_model
      && sameReasoningEffortNames(
        leftMetadata.supported_reasoning_efforts,
        rightMetadata.supported_reasoning_efforts,
      )
    );
  });

  const sameCatalog = (left, right) => (
    left.loaded
    && sameModelNames(left.models, right.models)
    && left.defaultModel === right.defaultModel
    && sameModelMetadata(left.modelMetadata, right.modelMetadata, right.models)
  );

  const modelArrayLooksPatchable = (value, allowEmpty = false) => (
    Array.isArray(value)
    && (allowEmpty || value.length > 0)
    && Array.from(value).every((item) => (
      item
      && typeof item === "object"
      && typeof item.model === "string"
    ))
  );

  const patchedModelArray = (models, allowEmpty = false) => {
    if (!catalog.loaded || !modelArrayLooksPatchable(models, allowEmpty)) return null;
    const existing = new Map(models.map((item) => [modelKey(item.model), item]));
    const nextModels = catalog.models.map((modelName) => (
      modelDescriptor(modelName, existing.get(modelKey(modelName)))
    ));
    if (nativeSelectionOnly) {
      return models.length === nextModels.length
        && models.every((model, index) => model === nextModels[index])
        ? null : nextModels;
    }
    const unchanged = (
      models.length === nextModels.length
      && models.every((model, index) => (
        model?.model === nextModels[index]?.model
        && model?.name === nextModels[index]?.name
        && model?.displayName === nextModels[index]?.displayName
        && model?.routeName === nextModels[index]?.routeName
        && model?.providerName === nextModels[index]?.providerName
        && model?.providerId === nextModels[index]?.providerId
        && model?.sourceModel === nextModels[index]?.sourceModel
        && model?.codeyRouteName === nextModels[index]?.codeyRouteName
        && model?.codeyModelName === nextModels[index]?.codeyModelName
        && model?.hidden === false
        && model?.isDefault === nextModels[index]?.isDefault
        && model?.defaultReasoningEffort === nextModels[index]?.defaultReasoningEffort
        && sameReasoningEfforts(
          model?.supportedReasoningEfforts,
          nextModels[index]?.supportedReasoningEfforts,
        )
        && model?.serviceTiers === nextModels[index]?.serviceTiers
        && model?.additionalSpeedTiers === nextModels[index]?.additionalSpeedTiers
        && model?.defaultServiceTier === nextModels[index]?.defaultServiceTier
      ))
    );
    return unchanged ? null : nextModels;
  };

  const patchedModelPayload = (value) => {
    if (!catalog.loaded || !value || typeof value !== "object") {
      return { changed: false, value };
    }
    if (Array.isArray(value)) {
      const models = patchedModelArray(value);
      return models
        ? { changed: true, value: models }
        : { changed: false, value };
    }

    let changed = false;
    const next = { ...value };
    for (const key of ["data", "models"]) {
      const allowEmpty = key === "data"
        ? ("nextCursor" in value || "next_cursor" in value)
        : (
          "defaultModel" in value
          || "default_model" in value
          || "hasModelSupportingMaxReasoningEffort" in value
        );
      const models = patchedModelArray(value[key], allowEmpty);
      if (!models) continue;
      next[key] = models;
      changed = true;
    }
    for (const key of ["result", "message"]) {
      if (!value[key] || typeof value[key] !== "object") continue;
      const nested = patchedModelPayload(value[key]);
      if (!nested.changed) continue;
      next[key] = nested.value;
      changed = true;
    }
    if (
      Array.isArray(value.availableModels)
      && !sameModelNames(value.availableModels, catalog.models)
    ) {
      next.availableModels = [...catalog.models];
      changed = true;
    }
    if (
      Array.isArray(value.available_models)
      && !sameModelNames(value.available_models, catalog.models)
    ) {
      next.available_models = [...catalog.models];
      changed = true;
    }
    if (!nativeSelectionOnly && "defaultModel" in value && catalog.defaultModel) {
      if (typeof value.defaultModel === "string" && value.defaultModel !== catalog.defaultModel) {
        next.defaultModel = catalog.defaultModel;
        changed = true;
      } else if (
        value.defaultModel
        && typeof value.defaultModel === "object"
        && value.defaultModel.model !== catalog.defaultModel
      ) {
        const models = next.models || value.models;
        next.defaultModel = Array.isArray(models)
          ? models.find((model) => model?.model === catalog.defaultModel)
            || modelDescriptor(catalog.defaultModel)
          : modelDescriptor(catalog.defaultModel);
        changed = true;
      }
    }
    return { changed, value: changed ? next : value };
  };

  const patchedModelConfig = (config) => {
    if (
      !catalog.loaded
      || !config
      || typeof config !== "object"
      || !config.value
      || typeof config.value !== "object"
    ) {
      return config;
    }
    const value = config.value;
    if (
      sameModelNames(value.available_models, catalog.models)
      && (nativeSelectionOnly || value.default_model === catalog.defaultModel)
    ) {
      return config;
    }
    const nextConfig = {
      ...config,
      value: {
        ...value,
        available_models: [...catalog.models],
        ...(nativeSelectionOnly ? {} : { default_model: catalog.defaultModel }),
      },
    };
    try {
      config.value = nextConfig.value;
      if (config.value === nextConfig.value) return config;
    } catch {
      // Frozen Statsig results are returned as a shallow copy by the wrapper.
    }
    return nextConfig;
  };

  const addConfigReference = (references, parent, key) => {
    if (!parent || typeof parent !== "object" || !(key in parent)) return;
    references.push({ parent, key });
  };

  const statsigModelConfigReferences = (client) => {
    const references = [];
    const memoCache = client?._memoCache;
    if (memoCache && typeof memoCache === "object") {
      Object.keys(memoCache)
        .filter((key) => key.includes(modelConfigId))
        .forEach((key) => addConfigReference(references, memoCache, key));
    }
    [
      client?._store?._valuesForExternalUse?.dynamic_configs,
      client?._store?._values?._values?.dynamic_configs,
      client?._store?._values?.dynamic_configs,
    ].forEach((configs) => addConfigReference(references, configs, modelConfigId));
    return references;
  };

  const patchStatsigClient = (client) => {
    if (!client || typeof client !== "object") return false;
    let changed = false;
    const memoCache = client._memoCache;
    if (memoCache instanceof Map) {
      for (const [key, current] of memoCache.entries()) {
        if (!String(key).includes(modelConfigId)) continue;
        const alreadyPatched = (
          sameModelNames(current?.value?.available_models, catalog.models)
          && (nativeSelectionOnly || current?.value?.default_model === catalog.defaultModel)
        );
        const next = patchedModelConfig(current);
        if (next !== current) {
          try {
            memoCache.set(key, next);
          } catch {
            // The getDynamicConfig wrapper still fixes immutable cache entries.
          }
        }
        if (!alreadyPatched) changed = true;
      }
    }
    for (const { parent, key } of statsigModelConfigReferences(client)) {
      const current = parent[key];
      const alreadyPatched = (
        sameModelNames(current?.value?.available_models, catalog.models)
        && (nativeSelectionOnly || current?.value?.default_model === catalog.defaultModel)
      );
      const next = patchedModelConfig(current);
      if (next !== current) {
        try {
          parent[key] = next;
        } catch {
          // The getDynamicConfig wrapper still fixes immutable cache entries.
        }
      }
      if (!alreadyPatched) changed = true;
    }

    const currentGetter = client.getDynamicConfig;
    if (
      typeof currentGetter === "function"
      && currentGetter.__codeyModelWhitelistPatchVersion !== patchVersion
    ) {
      const originalGetter = currentGetter.bind(client);
      const wrappedGetter = (name, options) => {
        const result = originalGetter(name, options);
        return String(name) === modelConfigId ? patchedModelConfig(result) : result;
      };
      Object.defineProperty(wrappedGetter, "__codeyModelWhitelistPatchVersion", {
        value: patchVersion,
      });
      try {
        client.getDynamicConfig = wrappedGetter;
        changed = client.getDynamicConfig === wrappedGetter || changed;
      } catch {
        // A later refresh retries if Statsig temporarily exposes a readonly API.
      }
    }
    return changed;
  };

  const statsigClients = window.__codeySharedRuntime.statsigClients;

  const notifyStatsigClients = () => {
    let notified = 0;
    for (const client of statsigClients()) {
      if (typeof client.$emt !== "function") continue;
      try {
        client.$emt({ name: "values_updated" });
        notified += 1;
      } catch {
        // A later refresh retries transient Statsig subscription failures.
      }
    }
    return notified;
  };

  const applyModelWhitelist = () => {
    if (!catalog.loaded || disposed) return false;
    let changed = false;
    statsigClients().forEach((client) => {
      if (patchStatsigClient(client)) changed = true;
    });
    return changed;
  };

  const ensureGroupedMenuStyles = () => {
    if (
      document.getElementById?.(groupedMenuStyleId)
      || typeof document.createElement !== "function"
    ) return;
    const style = document.createElement("style");
    if (!style) return;
    style.id = groupedMenuStyleId;
    style.textContent = `
      .codey-model-route-heading {
        display: flex;
        align-items: center;
        gap: 8px;
        margin: 8px 8px 4px;
        padding: 0 2px;
        color: #8b949e;
        font-size: 11px;
        font-weight: 600;
        line-height: 16px;
        pointer-events: none;
        user-select: none;
      }
      .codey-model-route-heading::after {
        content: "";
        flex: 1 1 auto;
        border-top: 1px solid rgba(139, 148, 158, 0.24);
      }
    `;
    (document.head || document.documentElement || document.body)?.appendChild?.(style);
  };

  const replaceTextOnce = (element, from, to) => {
    const source = cleanText(from);
    const target = cleanText(to);
    if (!source || !target || source === target) return false;
    if (typeof document.createTreeWalker === "function") {
      const walker = document.createTreeWalker(element, 4);
      let node = walker.nextNode();
      while (node) {
        const text = node.nodeValue || "";
        if (text.includes(source)) {
          node.nodeValue = text.replace(source, target);
          return true;
        }
        node = walker.nextNode();
      }
    }
    if (cleanText(element.textContent) === source) {
      element.textContent = target;
      return true;
    }
    return false;
  };

  const createRouteHeading = (routeName) => {
    if (typeof document.createElement !== "function") return null;
    const heading = document.createElement("div");
    heading.className = "codey-model-route-heading";
    heading.textContent = routeName;
    heading.setAttribute("role", "presentation");
    heading.setAttribute("aria-hidden", "true");
    heading.dataset.codeyRouteHeading = routeName;
    return heading;
  };

  const directRouteHeadings = (parent) => Array.from(parent?.children || [])
    .filter((child) => Boolean(child?.dataset?.codeyRouteHeading));

  const restoreSupersededModelMenuItem = (item) => {
    if (!item?.dataset?.codeySupersededModel) return;
    item.removeAttribute?.("hidden");
    delete item.dataset.codeySupersededModel;
    delete item.dataset.codeySupersededLabel;
  };

  const hideSupersededModelMenuItem = (item, modelName, itemText) => {
    item.dataset.codeySupersededModel = modelName;
    item.dataset.codeySupersededLabel = itemText;
    item.setAttribute?.("hidden", "");
  };

  const reconcileRouteHeadings = (parent, items) => {
    const routeStarts = [];
    let previousRoute = "";
    for (const { item, routeName } of items) {
      if (routeName === previousRoute) continue;
      routeStarts.push({ item, routeName });
      previousRoute = routeName;
    }
    const headings = directRouteHeadings(parent);
    const children = Array.from(parent?.children || []);
    const alreadyGrouped = headings.length === routeStarts.length
      && routeStarts.every(({ item, routeName }) => {
        const itemIndex = children.indexOf(item);
        return itemIndex > 0
          && children[itemIndex - 1]?.dataset?.codeyRouteHeading === routeName;
      });
    if (alreadyGrouped) return false;
    headings.forEach((heading) => heading.remove?.());
    for (const { item, routeName } of routeStarts) {
      const heading = createRouteHeading(routeName);
      if (heading) parent.insertBefore?.(heading, item);
    }
    return true;
  };

  const enhanceGroupedModelMenus = () => {
    if (nativeSelectionOnly) return;
    if (!catalog.loaded || disposed || typeof document.querySelectorAll !== "function") return;
    ensureGroupedMenuStyles();
    const byMenuLabel = new Map();
    for (const modelName of catalog.models) {
      const presentation = modelPresentation(modelName);
      const displayName = cleanText(presentation.displayName);
      if (!displayName || !presentation.routeName || !presentation.modelName) continue;
      for (const menuLabel of [displayName, cleanText(modelName)]) {
        if (menuLabel) byMenuLabel.set(menuLabel, { modelName, presentation, menuLabel });
      }
    }
    const containers = Array.from(document.querySelectorAll(groupedMenuSelector) || []);
    for (const container of containers) {
      const items = Array.from(container.querySelectorAll?.(groupedMenuItemSelector) || []);
      const enhancedItems = [];
      const existingHeadings = directRouteHeadings(container);
      const looksLikeModelMenu = existingHeadings.length > 0 || items.some((item) => (
        Boolean(item.dataset?.codeyRouteModel)
        || Boolean(item.dataset?.codeySupersededModel)
        || byMenuLabel.has(cleanText(item.textContent))
      ));
      if (!looksLikeModelMenu) continue;
      const itemParents = new Set(existingHeadings.length > 0 ? [container] : []);
      for (const item of items) {
        const itemText = cleanText(item.textContent);
        const supersededLabel = cleanText(item.dataset?.codeySupersededLabel);
        if (supersededLabel && supersededLabel !== itemText) {
          restoreSupersededModelMenuItem(item);
          delete item.dataset.codeyRouteModel;
          delete item.dataset.codeyRouteName;
          item.classList?.remove?.("codey-model-route-item");
          item.removeAttribute?.("aria-label");
        }
        const existingModel = item.dataset?.codeyRouteModel || "";
        const existingPresentation = existingModel ? modelPresentation(existingModel) : null;
        const existingPresentationStillMatches = existingPresentation?.routeName
          && catalog.modelNamesByKey.has(modelKey(existingModel))
          && [existingPresentation.displayName, existingPresentation.modelName]
            .map(cleanText)
            .includes(itemText);
        const matched = existingPresentationStillMatches
          ? { modelName: existingModel, presentation: existingPresentation, menuLabel: itemText }
          : byMenuLabel.get(itemText);
        if (!matched?.presentation?.routeName || !matched.presentation.modelName) {
          const supersededModel = (
            existingModel && !catalog.modelNamesByKey.has(modelKey(existingModel))
              ? existingModel
              : supersededModelMenuLabels.get(itemText)
          );
          if (supersededModel) {
            hideSupersededModelMenuItem(item, supersededModel, itemText);
            itemParents.add(item.parentElement || container);
          } else {
            restoreSupersededModelMenuItem(item);
          }
          continue;
        }
        restoreSupersededModelMenuItem(item);
        item.dataset.codeyRouteModel = matched.modelName;
        item.dataset.codeyRouteName = matched.presentation.routeName;
        item.classList?.add?.("codey-model-route-item");
        item.setAttribute?.(
          "aria-label",
          `${matched.presentation.routeName} / ${matched.presentation.modelName}`,
        );
        replaceTextOnce(
          item,
          matched.menuLabel,
          matched.presentation.modelName,
        );
        enhancedItems.push({ item, routeName: matched.presentation.routeName });
        itemParents.add(item.parentElement || container);
      }
      const itemsByParent = new Map();
      for (const entry of enhancedItems) {
        const { item } = entry;
        const parent = item.parentElement || container;
        const siblings = itemsByParent.get(parent) || [];
        siblings.push(entry);
        itemsByParent.set(parent, siblings);
      }
      for (const parent of itemParents) {
        reconcileRouteHeadings(parent, itemsByParent.get(parent) || []);
      }
    }
  };

  const scheduleGroupedModelMenuEnhancement = () => {
    if (nativeSelectionOnly) return;
    if (disposed || groupedMenuTimer || !catalog.loaded) return;
    groupedMenuTimer = window.setTimeout(() => {
      groupedMenuTimer = 0;
      enhanceGroupedModelMenus();
    }, 0);
  };

  const groupedMenuElement = (node) => {
    if (node && typeof node.matches === "function") return node;
    const parent = node?.parentElement;
    return parent && typeof parent.matches === "function" ? parent : null;
  };

  const groupedMenuContainer = (node) => {
    const element = groupedMenuElement(node);
    if (!element) return null;
    if (element.matches?.(groupedMenuSelector)) return element;
    return element.closest?.(groupedMenuSelector) || null;
  };

  const menusWithin = (node) => {
    const element = groupedMenuElement(node);
    if (!element) return [];
    const menus = [];
    if (element.matches?.(groupedMenuSelector)) menus.push(element);
    if (typeof element.querySelectorAll === "function") {
      menus.push(...element.querySelectorAll(groupedMenuSelector));
    }
    return menus;
  };

  const stopGroupedMenuTextObserver = (container) => {
    const observer = groupedMenuTextObservers.get(container);
    if (!observer) return;
    observer.disconnect?.();
    groupedMenuTextObservers.delete(container);
  };

  const observeGroupedMenuText = (container) => {
    if (!container || disposed || groupedMenuTextObservers.has(container)) return;
    const MutationObserver = window.MutationObserver || globalThis.MutationObserver;
    if (typeof MutationObserver !== "function") return;
    const observer = new MutationObserver(scheduleGroupedModelMenuEnhancement);
    // Route saves can rewrite only a menu row's text node. CharacterData has
    // to stay on the open picker, otherwise the short-name label stays
    // ungrouped until the menu is reopened.
    observer.observe(container, {
      childList: true,
      characterData: true,
      subtree: true,
    });
    groupedMenuTextObservers.set(container, observer);
  };

  const syncGroupedMenuTextObservers = (roots = []) => {
    if (disposed) return;
    const menus = roots.length > 0
      ? roots.flatMap(menusWithin)
      : Array.from(document.querySelectorAll?.(groupedMenuSelector) || []);
    for (const menu of menus) observeGroupedMenuText(menu);
  };

  const handleGroupedMenuMutations = (mutations) => {
    if (disposed) return;
    const discoveredMenus = [];
    let relevant = false;
    for (const mutation of mutations) {
      const targetMenu = groupedMenuContainer(mutation.target);
      if (targetMenu) {
        relevant = true;
        discoveredMenus.push(targetMenu);
      }
      if (mutation.type === "characterData") continue;
      for (const node of mutation.addedNodes || []) {
        const menus = menusWithin(node);
        if (menus.length === 0) continue;
        relevant = true;
        discoveredMenus.push(...menus);
      }
      for (const node of mutation.removedNodes || []) {
        for (const menu of menusWithin(node)) {
          stopGroupedMenuTextObserver(menu);
        }
      }
    }
    if (discoveredMenus.length) syncGroupedMenuTextObservers(discoveredMenus);
    for (const container of [...groupedMenuTextObservers.keys()]) {
      if (container.isConnected === false) stopGroupedMenuTextObserver(container);
    }
    if (relevant) scheduleGroupedModelMenuEnhancement();
  };

  const installGroupedModelMenuObserver = () => {
    if (groupedMenuObserver || !document.body) return;
    const dispatcher = window.__codeyMutationDispatcher;
    if (typeof dispatcher?.subscribe === "function") {
      const unsubscribe = dispatcher.subscribe(handleGroupedMenuMutations, {
        childList: true,
      });
      if (dispatcher.snapshot?.().observerInstalled) {
        groupedMenuObserver = { disconnect: unsubscribe };
        syncGroupedMenuTextObservers();
        return;
      }
      unsubscribe?.();
    }
    const MutationObserver = window.MutationObserver || globalThis.MutationObserver;
    if (typeof MutationObserver !== "function") return;
    groupedMenuObserver = new MutationObserver(handleGroupedMenuMutations);
    groupedMenuObserver.observe(document.body, {
      childList: true,
      subtree: true,
    });
    syncGroupedMenuTextObservers();
  };

  const reactFiberKeys = (element) =>
    window.__codeySharedRuntime.reactInternalKeys(element, { includeContainer: true });

  const reactModelStateNodes = (forceScan = false) => {
    const nodes = [
      document.body,
      document.documentElement,
      document.getElementById?.("root"),
      ...Array.from(document.querySelectorAll?.(
        "[role='menu'], [role='dialog'], [role='listbox'], [data-radix-popper-content-wrapper]",
      ) || []),
    ].filter(Boolean);
    if (forceScan) {
      nodes.push(...Array.from(document.querySelectorAll?.("*") || []).slice(0, 600));
    }
    return nodes.filter((node, index, all) => all.indexOf(node) === index);
  };

  const scanReactObjectGraph = (forceScan = false, discover = false) => {
    if (!forceScan && !discover && knownModelQueryClients.size > 0) {
      return {
        queryClients: [...knownModelQueryClients],
        reactContainers: 0,
      };
    }
    const queryClients = new Set(knownModelQueryClients);
    const visited = new WeakSet();
    let visitedCount = 0;
    let reactContainers = 0;

    const visit = (value, depth = 0) => {
      if (
        !value
        || (typeof value !== "object" && typeof value !== "function")
        || visited.has(value)
        || visitedCount >= 30_000
        // Current renderer builds wrap the query client in deep provider
        // stacks; the visited cap above keeps the wider hop budget bounded.
        || depth > 12
      ) return;
      visited.add(value);
      visitedCount += 1;

      try {
        if (
          typeof value.getQueriesData === "function"
          && typeof value.setQueryData === "function"
          && typeof value.invalidateQueries === "function"
        ) {
          queryClients.add(value);
          rememberBounded(
            knownModelQueryClients,
            value,
            maxKnownModelQueryClients,
          );
        }
      } catch {
        // Ignore proxy-backed values that reject capability probes.
      }

      // Native selection must update model queries through QueryClient so its
      // observers receive a new result. Mutating a React result object first
      // makes the later cache pass look unchanged and leaves mounted pickers
      // memoized on the old model list.
      if (!nativeSelectionOnly) {
        const patched = patchedModelPayload(value);
        if (patched.changed && patched.value !== value) {
          for (const key of ["data", "models", "result", "message", "availableModels", "available_models", "defaultModel"]) {
            if (!(key in patched.value) || patched.value[key] === value[key]) continue;
            try {
              value[key] = patched.value[key];
              reactContainers += 1;
            } catch {
              // QueryClient.setQueryData handles immutable cached results below.
            }
          }
        }
      }

      let keys = [];
      try {
        keys = Object.keys(value).slice(0, 120);
      } catch {
        return;
      }
      for (const key of keys) {
        if (
          key === "ownerDocument"
          || key === "parentElement"
          || key === "parentNode"
          || key === "children"
          || key === "childNodes"
        ) continue;
        let child;
        try {
          child = value[key];
        } catch {
          continue;
        }
        visit(child, depth + 1);
      }
    };

    for (const node of reactModelStateNodes(forceScan)) {
      for (const key of reactFiberKeys(node)) {
        let root;
        try {
          root = node[key];
        } catch {
          continue;
        }
        visit(root);
      }
    }
    return { queryClients: [...queryClients], reactContainers };
  };

  const scanReactObjectGraphWhenDue = (forceScan = false, discover = false) => {
    const now = Date.now();
    const runFullScan = forceScan || now >= nextFullReactDiscoveryAt;
    if (!runFullScan && knownModelQueryClients.size === 0 && !discover) {
      return { queryClients: [], reactContainers: 0 };
    }
    if (runFullScan) {
      nextFullReactDiscoveryAt = now + fullReactDiscoveryIntervalMs;
    }
    return scanReactObjectGraph(runFullScan, discover);
  };

  const patchModelQueryClients = async ({
    forceScan = false,
    discover = false,
    invalidate = false,
  } = {}) => {
    const scan = scanReactObjectGraphWhenDue(forceScan, discover);
    let queryEntries = 0;
    let changedEntries = 0;
    const invalidations = [];

    for (const client of scan.queryClients) {
      let entries = [];
      try {
        entries = client.getQueriesData({
          queryKey: modelQueryKey,
          exact: false,
        }) || [];
      } catch {
        knownModelQueryClients.delete(client);
        continue;
      }
      queryEntries += entries.length;
      for (const [queryKey, current] of entries) {
        const patched = patchedModelPayload(current);
        if (!patched.changed) continue;
        try {
          client.setQueryData(queryKey, patched.value);
          changedEntries += 1;
        } catch {
          // The response interceptor still patches the next active refetch.
        }
      }
      if (invalidate) {
        try {
          invalidations.push(Promise.resolve(client.invalidateQueries({
            queryKey: modelQueryKey,
            exact: false,
            refetchType: "active",
          })));
        } catch {
          // A later scheduled pass retries discovery and refresh.
        }
      }
    }
    if (invalidations.length > 0) {
      void Promise.allSettled(invalidations).then(async () => {
        if (disposed || !catalog.loaded) return;
        const settledPass = await patchModelQueryClients({
          forceScan: false,
          invalidate: false,
        });
        const notifiedClients = notifyStatsigClients();
        updateDeliveryState({
          statsigClients: statsigClients().length,
          notifiedClients,
          queryClients: settledPass.queryClients,
          queryEntries: settledPass.queryEntries,
          reactContainers: settledPass.reactContainers,
        });
      });
    }
    return {
      queryClients: scan.queryClients.length,
      queryEntries,
      changedEntries,
      reactContainers: scan.reactContainers,
    };
  };

  const updateDeliveryState = (report) => {
    if (deliveryState.revision !== catalogRevision) {
      deliveryState = {
        revision: catalogRevision,
        statsigClients: 0,
        notifiedClients: 0,
        queryClients: 0,
        queryEntries: 0,
        reactContainers: 0,
        responsePatchInstalled: true,
      };
    }
    deliveryState.statsigClients = Math.max(
      deliveryState.statsigClients,
      report.statsigClients || 0,
    );
    deliveryState.notifiedClients = Math.max(
      deliveryState.notifiedClients,
      report.notifiedClients || 0,
    );
    deliveryState.queryClients = Math.max(
      deliveryState.queryClients,
      report.queryClients || 0,
    );
    deliveryState.queryEntries = Math.max(
      deliveryState.queryEntries,
      report.queryEntries || 0,
    );
    deliveryState.reactContainers = Math.max(
      deliveryState.reactContainers,
      report.reactContainers || 0,
    );
  };

  const deliverModelCatalog = async ({
    discoverQueryClients = false,
    invalidate = true,
  } = {}) => {
    if (!catalog.loaded || disposed) return false;
    const statsigChanged = applyModelWhitelist();
    const firstPass = await patchModelQueryClients({
      forceScan: invalidate,
      discover: discoverQueryClients,
      invalidate,
    });
    const shouldNotify = (
      invalidate
      || statsigChanged
      || firstPass.changedEntries > 0
      || firstPass.reactContainers > 0
    );
    const firstNotifications = shouldNotify ? notifyStatsigClients() : 0;
    const secondPass = invalidate
      ? await patchModelQueryClients({ forceScan: false, invalidate: false })
      : firstPass;
    updateDeliveryState({
      statsigClients: statsigClients().length,
      notifiedClients: firstNotifications,
      queryClients: Math.max(firstPass.queryClients, secondPass.queryClients),
      queryEntries: Math.max(firstPass.queryEntries, secondPass.queryEntries),
      reactContainers: firstPass.reactContainers + secondPass.reactContainers,
    });
    scheduleGroupedModelMenuEnhancement();
    return true;
  };

  const scheduleRefresh = (durationMs = 5000) => {
    if (disposed) return;
    refreshUntil = Math.max(refreshUntil, Date.now() + durationMs);
    if (refreshTimer) return;
    refreshRetryDelay = 120;
    const tick = () => {
      // Keep the fired handle truthy while the tick body runs: the
      // bridge-missing path inside loadModelCatalog calls scheduleRefresh
      // synchronously, and a cleared handle here would let it start a second
      // timer chain that can never be cancelled.
      if (disposed) {
        refreshTimer = 0;
        return;
      }
      if (catalog.loaded) {
        if (!refreshDeliveryInFlight) {
          refreshDeliveryInFlight = true;
          void deliverModelCatalog({ invalidate: false }).then(
            () => {
              refreshDeliveryInFlight = false;
            },
            (error) => {
              refreshDeliveryInFlight = false;
              console.warn("[Codey] scheduled model delivery failed", error);
            },
          );
        }
      } else {
        void loadModelCatalog();
      }
      if (Date.now() < refreshUntil) {
        const nextDelay = refreshRetryDelay;
        refreshRetryDelay = Math.min(
          refreshRetryDelay * 2,
          catalog.loaded ? 1000 : 2000,
        );
        refreshTimer = window.setTimeout(tick, nextDelay);
      } else {
        refreshTimer = 0;
      }
    };
    refreshTimer = window.setTimeout(tick, 0);
  };

  const loadModelCatalog = () => {
    if (catalogLoadPromise) return catalogLoadPromise;
    const requestedRevision = catalogRevision;
    catalogLoadPromise = (async () => {
      if (disposed || typeof window.__codexSessionDeleteBridge !== "function") {
        scheduleRefresh();
        return false;
      }
      try {
        const result = await window.__codexSessionDeleteBridge(modelCatalogPath, {});
        const nextCatalog = normalizedCatalog(result);
        if (!nextCatalog) {
          if (!catalog.loaded) scheduleRefresh();
          return false;
        }
        if (requestedRevision !== catalogRevision) return false;
        const unchanged = sameCatalog(catalog, nextCatalog);
        if (unchanged) {
          // Window-focus reloads land here when nothing changed upstream:
          // skip the invalidating re-delivery (full client scan plus query
          // invalidation) and keep only a short non-invalidating window.
          scheduleRefresh(1000);
          return true;
        }
        rememberSupersededModelMenuItems(catalog, nextCatalog);
        rememberSupersededDefaultRoute(catalog, nextCatalog);
        catalogRevision += 1;
        catalog = nextCatalog;
        await deliverModelCatalog();
        scheduleRefresh();
        return true;
      } catch (error) {
        console.warn("[Codey] model whitelist refresh failed", error);
        if (!catalog.loaded) scheduleRefresh();
        return false;
      }
    })().finally(() => {
      catalogLoadPromise = null;
    });
    return catalogLoadPromise;
  };

  const setModelCatalog = (value) => {
    if (disposed) return false;
    const nextCatalog = normalizedCatalog(value);
    if (!nextCatalog) return false;
    if (sameCatalog(catalog, nextCatalog)) {
      scheduleRefresh(1000);
      return Promise.resolve(true);
    }
    rememberSupersededModelMenuItems(catalog, nextCatalog);
    rememberSupersededDefaultRoute(catalog, nextCatalog);
    catalogRevision += 1;
    catalog = nextCatalog;
    return deliverModelCatalog().then((delivered) => {
      scheduleRefresh();
      return delivered;
    });
  };

  const routeForModel = (modelName) => {
    const key = modelKey(modelName);
    const canonicalModel = catalog.modelNamesByKey.get(key) || modelName;
    const route = catalog.routeBySelectorKey.get(key)
      || catalog.routeMetadata[canonicalModel];
    if (route) return route;
    const metadata = catalog.modelMetadata[canonicalModel];
    const providerId = typeof metadata?.provider_id === "string"
      ? metadata.provider_id.trim()
      : "";
    const sourceModel = typeof metadata?.source_model === "string"
      ? metadata.source_model.trim()
      : "";
    const routeProviderId = typeof metadata?.route_provider_id === "string"
      ? metadata.route_provider_id.trim()
      : "";
    return providerId && routeProviderId && sourceModel
      ? {
          selectorModel: canonicalModel,
          providerId,
          routeProviderId,
          sourceModel,
          officialAccount: metadataBoolean(metadata, "official_account"),
        }
      : null;
  };

  const rememberSupersededModelMenuItems = (previousCatalog, nextCatalog) => {
    if (!previousCatalog?.loaded) return;
    const nextModelKeys = new Set(nextCatalog.models.map(modelKey));
    for (const modelName of previousCatalog.models) {
      if (nextModelKeys.has(modelKey(modelName))) continue;
      const displayName = cleanText(
        modelPresentationFromCatalog(previousCatalog, modelName).displayName,
      );
      if (!displayName) continue;
      supersededModelMenuLabels.delete(displayName);
      supersededModelMenuLabels.set(displayName, modelName);
    }
    for (const modelName of nextCatalog.models) {
      const displayName = cleanText(
        modelPresentationFromCatalog(nextCatalog, modelName).displayName,
      );
      if (displayName) supersededModelMenuLabels.delete(displayName);
    }
    while (supersededModelMenuLabels.size > maxSupersededModelMenuLabels) {
      supersededModelMenuLabels.delete(supersededModelMenuLabels.keys().next().value);
    }
  };

  const rememberSupersededDefaultRoute = (previousCatalog, nextCatalog) => {
    if (
      !previousCatalog?.loaded
      || !previousCatalog.defaultModel
      || modelKey(previousCatalog.defaultModel) === modelKey(nextCatalog?.defaultModel)
    ) return;
    const route = previousCatalog.routeMetadata?.[previousCatalog.defaultModel];
    const record = {
      selectorModel: previousCatalog.defaultModel,
      routeProviderId: requestProviderId(route?.routeProviderId),
      sourceModel: typeof route?.sourceModel === "string"
        ? route.sourceModel.trim()
        : previousCatalog.defaultModel,
    };
    const key = modelKey(record.selectorModel);
    const duplicate = supersededDefaultRoutes.findIndex(
      (candidate) => modelKey(candidate.selectorModel) === key,
    );
    if (duplicate >= 0) supersededDefaultRoutes.splice(duplicate, 1);
    supersededDefaultRoutes.push(record);
    while (supersededDefaultRoutes.length > maxSupersededDefaultRoutes) {
      supersededDefaultRoutes.shift();
    }
  };

  const requestUsesSupersededDefault = (requestedModel) => {
    const requestedKey = modelKey(requestedModel);
    if (!requestedKey || requestedKey === modelKey(catalog.defaultModel)) return false;
    return supersededDefaultRoutes.some((record) => (
      requestedKey === modelKey(record.selectorModel)
      || (
        requestedKey === modelKey(record.sourceModel)
        && modelKey(record.sourceModel) !== modelKey(routeForModel(catalog.defaultModel)?.sourceModel)
      )
    ));
  };

  const routeForProviderAlias = (modelName) => {
    const model = typeof modelName === "string" ? modelName.trim() : "";
    const separator = model.indexOf("/");
    if (separator <= 0) return null;
    const providerId = model.slice(0, separator).trim();
    const sourceModel = model.slice(separator + 1).trim();
    if (!providerId || !sourceModel) return null;
    const catalogAlias = catalog.modelNamesByKey.get(modelKey(model)) || "";
    if (catalogAlias) {
      const catalogRoute = routeForModel(catalogAlias);
      if (catalogRoute) return catalogRoute;
      const catalogSeparator = catalogAlias.indexOf("/");
      if (catalogSeparator > 0) {
        return {
          selectorModel: catalogAlias,
          providerId: localRouterProviderId,
          routeProviderId: catalogAlias.slice(0, catalogSeparator).trim(),
          sourceModel: catalogAlias.slice(catalogSeparator + 1).replace(/#\d+$/, "").trim(),
        };
      }
    }
    return routeFromNestedIndex(
      catalog.routeByAnyProviderSource,
      providerId,
      sourceModel,
    );
  };

  const routeMatchesModel = (route, model) => (
    modelKey(route?.selectorModel) === modelKey(model)
    || modelKey(route?.sourceModel) === modelKey(model)
  );
  const routeForThread = (threadId) => {
    const binding = threadRoutes.get(threadId);
    if (!binding) return null;
    const route = routeFromNestedIndex(
      catalog.routeByRouteProviderSource,
      binding.routeProviderId,
      binding.sourceModel,
    );
    if (!route && threadId && threadRoutes.delete(threadId)) persistThreadRoutes();
    return route;
  };
  const routeForThreadModel = (threadId, model) => {
    const route = routeForThread(threadId);
    return route && routeMatchesModel(route, model) ? route : null;
  };
  const uniqueRouteForRawModel = (model) => {
    const matches = catalog.routesBySourceKey.get(modelKey(model)) || [];
    return matches.length === 1 ? matches[0] : null;
  };
  const uniqueRouteForProviderModel = (providerId, model) => {
    const providerKey = modelKey(providerId);
    const sourceKey = modelKey(model);
    if (!providerKey || !sourceKey) return null;
    const matches = catalog.routes.filter((route) => (
      modelKey(route?.sourceModel) === sourceKey
      && (
        modelKey(route?.routeProviderId) === providerKey
        || modelKey(route?.providerId) === providerKey
      )
    ));
    const uniqueMatches = new Map(
      matches.map((route) => [modelKey(route.selectorModel), route]),
    );
    return uniqueMatches.size === 1 ? uniqueMatches.values().next().value : null;
  };
  const routeForHintedRawModel = (routeProviderId, model) => routeFromNestedIndex(
    catalog.routeByRouteProviderSource,
    routeProviderId,
    model,
  );
  const routeForThreadDisplayModel = (thread) => {
    if (!catalog.loaded || !thread || typeof thread !== "object") return null;
    const model = typeof thread.model === "string" ? thread.model.trim() : "";
    if (!model || catalog.modelNamesByKey.has(modelKey(model))) return null;
    const threadId = typeof thread.id === "string" ? thread.id.trim() : "";
    const providerId = providerFromThread(thread);
    const routeHint = requestProviderId(thread[routeMetadataParam]?.[routeMetadataKey]);
    if (routeHint) {
      const hintedRoute = routeForHintedRawModel(routeHint, model);
      if (hintedRoute?.selectorModel) return hintedRoute;
    }
    const threadRoute = routeForThreadModel(threadId, model);
    if (threadRoute?.selectorModel) return threadRoute;
    const providerModel = (() => {
      if (!providerId || !model) return model;
      const prefix = `${providerId}/`;
      return model.toLowerCase().startsWith(prefix.toLowerCase())
        ? model.slice(prefix.length).trim()
        : model;
    })();
    const providerRoute = providerId && modelKey(providerId) !== localRouterProviderId
      ? uniqueRouteForProviderModel(providerId, providerModel)
      : null;
    if (providerRoute?.selectorModel) return providerRoute;
    const aliasRoute = routeForProviderAlias(model);
    if (aliasRoute?.selectorModel) return aliasRoute;
    const rawRoute = uniqueRouteForRawModel(model);
    return rawRoute?.selectorModel ? rawRoute : null;
  };
  const patchedThreadDisplayModel = (thread) => {
    const route = routeForThreadDisplayModel(thread);
    const selectorModel = typeof route?.selectorModel === "string"
      ? route.selectorModel.trim()
      : "";
    if (
      !selectorModel
      || !thread
      || typeof thread !== "object"
      || modelKey(thread.model) === modelKey(selectorModel)
    ) return thread;
    return { ...thread, model: selectorModel };
  };
  const dataLooksLikeThreadList = (value) => (
    Array.isArray(value)
    && value.length > 0
    && value.every((item) => (
      item
      && typeof item === "object"
      && typeof item.id === "string"
    ))
    && value.some((item) => typeof item.model === "string")
    && !value.some((item) => (
      item
      && typeof item === "object"
      && (
        Array.isArray(item.supportedReasoningEfforts)
        || Array.isArray(item.serviceTiers)
        || Object.hasOwn(item, "displayName")
      )
    ))
  );
  const patchedThreadDisplayPayload = (value) => {
    if (!catalog.loaded || !value || typeof value !== "object") {
      return { changed: false, value };
    }
    let changed = false;
    const next = { ...value };
    if (value.thread && typeof value.thread === "object") {
      const thread = patchedThreadDisplayModel(value.thread);
      if (thread !== value.thread) {
        next.thread = thread;
        changed = true;
      }
    }
    if (dataLooksLikeThreadList(value.data)) {
      const data = value.data.map((thread) => patchedThreadDisplayModel(thread));
      if (data.some((thread, index) => thread !== value.data[index])) {
        next.data = data;
        changed = true;
      }
    }
    return { changed, value: changed ? next : value };
  };
  const applyThreadDisplayPatch = (value) => {
    const patched = patchedThreadDisplayPayload(value);
    if (!patched.changed || patched.value === value) return false;
    let applied = false;
    for (const key of ["thread", "data"]) {
      if (!(key in patched.value) || patched.value[key] === value[key]) continue;
      try {
        value[key] = patched.value[key];
        applied = true;
      } catch {
        // Immutable bridge payloads remain unchanged; later requests still route safely.
      }
    }
    return applied;
  };
  const replaceThreadDisplayPayload = (parent, key) => {
    const value = parent?.[key];
    const patched = patchedThreadDisplayPayload(value);
    if (!patched.changed || patched.value === value) return false;
    try {
      parent[key] = patched.value;
      if (parent[key] === patched.value) return true;
    } catch {
      // A mutable nested payload can still be repaired in place below.
    }
    return applyThreadDisplayPatch(value);
  };

  const rememberMenuRouteIntent = (event) => {
    if (!routeSelectionEvents.has(event?.type) || !catalog.loaded) return;
    const item = event?.target?.closest?.(groupedMenuItemSelector);
    const selectorModel = typeof item?.dataset?.codeyRouteModel === "string"
      ? item.dataset.codeyRouteModel.trim()
      : "";
    const route = selectorModel ? routeForModel(selectorModel) : null;
    if (!route) return;
    pendingRouteIntent = {
      selectorModel,
      routeProviderId: route.routeProviderId,
      sourceModel: route.sourceModel,
      selectedAt: Date.now(),
    };
  };
  const pendingIntentRouteForRequest = () => {
    const intent = pendingRouteIntent;
    if (!intent) return null;
    if (Date.now() - intent.selectedAt > pendingRouteIntentMaxAgeMs) {
      pendingRouteIntent = null;
      return null;
    }
    const route = routeForHintedRawModel(intent.routeProviderId, intent.sourceModel);
    if (!route) {
      pendingRouteIntent = null;
      return null;
    }
    // The capture-phase menu event is the freshest user intent. Codex can
    // enqueue a prewarm/settings request carrying the previous model before
    // its React state commits, so requiring the payload to already match the
    // clicked selector makes the first click appear to do nothing.
    return route;
  };

  const paramsWithoutUnverifiedRouteMetadata = (source, requestedModel) => {
    const metadata = source?.[routeMetadataParam];
    const routeHint = requestProviderId(metadata?.[routeMetadataKey]);
    if (!routeHint) return source;
    const threadId = threadIdFromParams(source);
    const binding = threadRoutes.get(threadId);
    const matchesBinding = binding
      && modelKey(binding.routeProviderId) === modelKey(routeHint)
      && (
        !requestedModel
        || modelKey(binding.sourceModel) === modelKey(requestedModel)
      );
    if (matchesBinding) return source;
    const nextMetadata = { ...metadata };
    delete nextMetadata[routeMetadataKey];
    const next = { ...source };
    if (Object.keys(nextMetadata).length > 0) next[routeMetadataParam] = nextMetadata;
    else delete next[routeMetadataParam];
    return next;
  };

  const patchedRequestParams = (method, params) => {
    if (!modelBoundRequestMethods.has(method)) return params;
    const source = params && typeof params === "object" ? params : {};
    const hasModelOverride = Object.hasOwn(source, "model");
    const requestedModel = typeof source.model === "string"
      ? source.model.trim()
      : "";
    if (method === "thread/settings/update") {
      if (!hasModelOverride) return params;
      if (source.model === null) {
        const threadId = threadIdFromParams(source);
        if (threadId && threadRoutes.delete(threadId)) persistThreadRoutes();
        pendingRouteIntent = null;
        return params;
      }
      // Preserve an explicit invalid value so app-server can report it instead
      // of silently replacing it with a route default.
      if (!requestedModel) return params;
    }
    const requestedProvider = paramsProviderId(source);
    if (!catalog.loaded) {
      // Before the catalog arrives there is no safe way to distinguish a
      // legacy route alias from a legitimate upstream model containing `/`.
      // Preserve the model byte-for-byte, but keep every new, forked, or resumed
      // thread on Codey's carrier so a route alias cannot reach the old provider.
      const safeSource = method === "turn/start"
        ? paramsWithoutUnverifiedRouteMetadata(source, requestedModel)
        : source;
      const providerId = threadProviderRequestMethods.has(method)
        ? localRouterProviderId
        : requestedProvider;
      return providerId && threadProviderRequestMethods.has(method)
        ? routedRequestParams(
            method,
            safeSource,
            requestedModel,
            providerId,
            null,
          )
        : safeSource === source ? params : safeSource;
    }
    const requestedModelForProvider = (() => {
      if (!requestedProvider || !requestedModel) return requestedModel;
      const prefix = `${requestedProvider}/`;
      return requestedModel.toLowerCase().startsWith(prefix.toLowerCase())
        ? requestedModel.slice(prefix.length).trim()
        : requestedModel;
    })();
    const canonicalRequestedModel = catalog.modelNamesByKey.get(modelKey(requestedModel)) || "";
    const canonicalRoute = canonicalRequestedModel
      ? routeForModel(canonicalRequestedModel)
      : null;
    const metadataRouteProviderId = requestProviderId(
      source[routeMetadataParam]?.[routeMetadataKey],
    );
    const metadataRoute = metadataRouteProviderId
      ? routeForHintedRawModel(metadataRouteProviderId, requestedModel)
      : null;
    const previouslyPatchedRoute = source[patchedRouteKey]
      && routeMatchesModel(source[patchedRouteKey], requestedModel)
      ? routeForHintedRawModel(
          source[patchedRouteKey].routeProviderId,
          requestedModel,
        )
      : null;
    const threadId = threadIdFromParams(source);
    const userIntentRoute = pendingIntentRouteForRequest();
    const threadRoute = requestedModel
      ? routeForThreadModel(threadId, requestedModel)
      : null;
    const stickyThreadRoute = (
      method === "turn/start"
      && !hasModelOverride
    ) ? routeForThread(threadId) : null;
    const existingRoute = requestedProvider
      && !isGatewayProviderId(requestedProvider)
      && requestedModel
      ? routeFromNestedIndex(
          catalog.routeByAnyProviderSource,
          requestedProvider,
          requestedModelForProvider,
        )
      : null;
    const aliasRoute = routeForProviderAlias(requestedModel);
    const uniqueRawRoute = requestedModel
      ? uniqueRouteForRawModel(requestedModel)
      : null;
    const defaultRoute = routeForModel(catalog.defaultModel);
    const matchingDefaultRoute = (
      requestedModel
      && threadProviderRequestMethods.has(method)
      && routeMatchesModel(defaultRoute, requestedModel)
    ) ? defaultRoute : null;
    const refreshedDefaultRoute = (
      method === "thread/start"
      && !userIntentRoute
      && requestUsesSupersededDefault(requestedModel)
    ) ? defaultRoute : null;
    const staleExplicitAliasRoute = (() => {
      if (!requestedProvider || isGatewayProviderId(requestedProvider)) return null;
      const prefix = `${requestedProvider}/`;
      if (!requestedModel.toLowerCase().startsWith(prefix.toLowerCase())) return null;
      return {
        selectorModel: requestedModel,
        providerId: localRouterProviderId,
        routeProviderId: requestedProvider,
        sourceModel: requestedModel,
        routeName: requestedProvider,
      };
    })();
    const route = userIntentRoute
      || refreshedDefaultRoute
      || metadataRoute
      || previouslyPatchedRoute
      || aliasRoute
      // `thread/settings/update` is the model picker's explicit new sticky
      // choice. Its selector must replace the previous thread binding. Later
      // raw `turn/start` requests can then reuse that stored route safely.
      || (method === "thread/settings/update" ? canonicalRoute : threadRoute)
      || (method === "thread/settings/update" ? threadRoute : canonicalRoute)
      || existingRoute
      || uniqueRawRoute
      || stickyThreadRoute
      || matchingDefaultRoute
      || staleExplicitAliasRoute;
    if (route) {
      const routed = routedRequestParams(
        method,
        source,
        route.selectorModel || route.sourceModel,
        requestProviderId(route.providerId),
        route,
      );
      if (userIntentRoute) pendingRouteIntent = null;
      return routed;
    }
    if (canonicalRequestedModel) {
      return routedRequestParams(
        method,
        source,
        canonicalRequestedModel,
        requestedProvider,
        null,
      );
    }
    // Any task with a known persisted provider can be resumed through the
    // HTTP-only Codey carrier without rewriting its rollout. Preserve an
    // unknown model exactly so the gateway can report it rather than falling
    // back to an unrelated default.
    if (method === "thread/resume") {
      const currentProviderId = knownThreadProvider(source);
      return currentProviderId
        ? routedRequestParams(
            method,
            source,
            requestedModel,
            localRouterProviderId,
            null,
          )
        : params;
    }
    // An explicit unknown or deleted model must never be silently replaced by
    // an unrelated default. Preserve it so the caller can surface the exact
    // invalid selection instead of sending a different model than the user chose.
    if (requestedModel) return params;
    if (!catalog.defaultModel) return params;
    const providerId = requestProviderId(defaultRoute?.providerId || "");
    return routedRequestParams(
      method,
      source,
      defaultRoute?.selectorModel || catalog.defaultModel,
      providerId,
      defaultRoute,
    );
  };

  const outgoingRequestParts = (request) => {
    const wrappedMethod = request?.method === "send-cli-request-for-host"
      && typeof request.params?.method === "string"
      ? request.params.method
      : "";
    return {
      wrappedMethod,
      method: wrappedMethod || String(request?.method || ""),
      params: wrappedMethod ? request.params?.params : request?.params,
    };
  };

  const requestIdKey = (request) => (
    request?.id == null ? "" : String(request.id)
  );
  const rememberOutgoingThreadRequest = (detail) => {
    const request = detail?.request;
    const requestId = requestIdKey(request);
    if (!requestId) return;
    const { method, params } = outgoingRequestParts(request);
    if (!["thread/start", "thread/resume", "thread/fork", "thread/read"].includes(method)) {
      return;
    }
    rememberBoundedMap(
      pendingThreadRequests,
      requestId,
      {
        method,
        threadId: threadIdFromParams(params),
        providerId: paramsProviderId(params),
        route: params?.[patchedRouteKey] || null,
      },
      maxPendingThreadRequests,
    );
  };

  const providerFromThread = (thread) => requestProviderId(
    thread?.modelProvider || thread?.model_provider,
  );
  const rememberThreadPersistedProvider = (thread, fallbackProvider = "") => {
    const threadId = typeof thread?.id === "string" ? thread.id.trim() : "";
    const fallback = requestProviderId(fallbackProvider);
    const providerId = providerFromThread(thread)
      || fallback;
    if (!threadId || !providerId) return;
    rememberBoundedThreadPersistedProvider(threadId, providerId);
  };
  const rememberThreadRuntimeProvider = (thread, providerId) => {
    const threadId = typeof thread === "string"
      ? thread.trim()
      : typeof thread?.id === "string"
        ? thread.id.trim()
        : "";
    const provider = requestProviderId(providerId);
    if (!threadId || !provider) return;
    rememberBoundedThreadRuntimeProvider(threadId, provider);
  };
  const rememberThreadRoute = (thread, fallbackRoute = null) => {
    const threadId = typeof thread?.id === "string" ? thread.id.trim() : "";
    if (!threadId) return;
    const model = typeof thread?.model === "string" ? thread.model.trim() : "";
    const route = fallbackRoute
      || (model ? routeForThreadModel(threadId, model) : null)
      || (model ? uniqueRouteForRawModel(model) : null);
    rememberBoundedThreadRoute(threadId, route);
  };
  const rememberThreadProvidersFromResponse = (data, message) => {
    if (!message || typeof message !== "object") return;
    const notificationThread = message?.params?.thread;
    if (message.method === "thread/started") {
      rememberThreadPersistedProvider(notificationThread);
      rememberThreadRuntimeProvider(notificationThread, providerFromThread(notificationThread));
      rememberThreadRoute(notificationThread);
    }
    const requestId = message.id == null ? "" : String(message.id);
    const pending = requestId ? pendingThreadRequests.get(requestId) : null;
    if (requestId) pendingThreadRequests.delete(requestId);
    const result = message.result;
    const resultThread = result?.thread;
    const fallbackProvider = pending?.method === "thread/start"
      || pending?.method === "thread/resume"
      || pending?.method === "thread/fork"
      ? pending.providerId
      : pending?.threadId
        ? threadPersistedProviders.get(pending.threadId)
        : "";
    const resultProvider = requestProviderId(result?.modelProvider) || fallbackProvider;
    const runtimeProvider = (
      pending?.method === "thread/start"
      || pending?.method === "thread/resume"
      || pending?.method === "thread/fork"
    ) ? pending.providerId : pending?.threadId
      ? threadRuntimeProviders.get(pending.threadId)
      : resultProvider;
    rememberThreadPersistedProvider(resultThread, resultProvider);
    rememberThreadRuntimeProvider(resultThread || pending?.threadId, runtimeProvider);
    rememberThreadRoute(resultThread, pending?.route);
    for (const thread of Array.isArray(result?.data) ? result.data : []) {
      rememberThreadPersistedProvider(thread);
      rememberThreadRoute(thread);
    }
    const directThread = data?.thread;
    rememberThreadPersistedProvider(directThread, resultProvider);
    rememberThreadRuntimeProvider(directThread || pending?.threadId, runtimeProvider);
    rememberThreadRoute(directThread, pending?.route);
  };

  const rememberOutgoingModelListRequest = (detail) => {
    const request = detail?.request;
    if (
      !request
      || typeof request !== "object"
      || request.id == null
    ) return;
    const { method } = outgoingRequestParts(request);
    if (method !== "model/list") return;
    rememberBounded(
      modelListRequestIds,
      String(request.id),
      maxTrackedModelListRequests,
    );
  };

  const rewrittenOutgoingMessage = (detail) => {
    const request = detail?.request;
    rememberOutgoingModelListRequest(detail);
    if (
      !routableOutgoingMessageTypes.has(detail?.type)
      || !request
      || typeof request !== "object"
    ) return detail;
    if (nativeSelectionOnly) return detail;

    const { wrappedMethod, method, params } = outgoingRequestParts(request);
    const nextParams = patchedRequestParams(method, params);
    if (nextParams === params) {
      rememberOutgoingThreadRequest(detail);
      return detail;
    }
    const nextRequest = { ...request };
    if (wrappedMethod) {
      nextRequest.params = {
        ...(request.params || {}),
        params: nextParams,
      };
    } else {
      nextRequest.params = nextParams;
    }
    const rewritten = { ...detail, request: nextRequest };
    rememberOutgoingThreadRequest(rewritten);
    return rewritten;
  };

  const blockedProviderRequest = (detail) => {
    const request = detail?.request;
    if (!request || typeof request !== "object") return null;
    const { params } = outgoingRequestParts(request);
    return params?.[blockedProviderRequestKey] || null;
  };

  const showBlockedProviderNotice = (detail) => {
    const blocked = blockedProviderRequest(detail);
    if (!blocked) return false;
    const target = blocked.routeName || blocked.targetProviderId || "所选线路";
    const current = blocked.currentProviderId
      ? `当前任务仍绑定在 ${blocked.currentProviderId}`
      : "当前任务的运行时供应商身份尚未确认";
    const message = `${current}，尚未完成迁入 Codey 统一路由，暂时不能安全发送到「${target}」。请重新打开该任务后重试；恢复完成后即可跨供应商切换。本次消息已在本地拦截，未请求任何上游。`;
    console.warn("[Codey] provider migration required before routed turn", blocked);
    try {
      const noticeId = "codey-provider-mismatch-notice";
      let notice = document.getElementById?.(noticeId);
      if (!notice && document.createElement && document.body?.appendChild) {
        notice = document.createElement("div");
        notice.id = noticeId;
        notice.setAttribute("role", "alert");
        Object.assign(notice.style, {
          position: "fixed",
          left: "50%",
          bottom: "88px",
          transform: "translateX(-50%)",
          zIndex: "2147483647",
          maxWidth: "min(680px, calc(100vw - 32px))",
          padding: "12px 16px",
          border: "1px solid rgba(245, 158, 11, 0.45)",
          borderRadius: "12px",
          background: "rgba(24, 24, 27, 0.96)",
          color: "#fafafa",
          boxShadow: "0 12px 36px rgba(0, 0, 0, 0.28)",
          fontSize: "13px",
          lineHeight: "1.55",
        });
        document.body.appendChild(notice);
      }
      if (notice) notice.textContent = message;
      window.clearTimeout(providerMismatchNoticeTimer);
      providerMismatchNoticeTimer = window.setTimeout(() => notice?.remove?.(), 9000);
    } catch {
      // Console warning remains available if the renderer DOM is unavailable.
    }
    return true;
  };

  const patchOutgoingModelRequest = (detail) => {
    const rewritten = rewrittenOutgoingMessage(detail);
    if (rewritten === detail) return false;
    try {
      detail.request = rewritten.request;
    } catch {
      // The startup renderer gate uses the returned clone when the event detail
      // is immutable; the later CustomEvent remains observational only.
    }
    return true;
  };

  const handleModelRequest = (event) => {
    patchOutgoingModelRequest(event?.detail);
  };

  const installModelRequestDispatchPatch = () => {
    if (typeof window.dispatchEvent !== "function") return;
    if (window.dispatchEvent.__codeyModelRequestPatchVersion === patchVersion) return;
    originalDispatchEvent = window.dispatchEvent;
    patchedDispatchEvent = function codeyModelRequestDispatchEvent(event) {
      try {
        if (event?.type === modelRequestEvent) {
          patchOutgoingModelRequest(event.detail);
        }
      } catch (error) {
        console.warn("[Codey] model request repair failed", error);
      }
      return originalDispatchEvent.call(this, event);
    };
    Object.defineProperty(patchedDispatchEvent, "__codeyModelRequestPatchVersion", {
      value: patchVersion,
    });
    window.dispatchEvent = patchedDispatchEvent;
  };

  const handleModelResponse = (event) => {
    const data = event?.data;
    if (data?.type !== "mcp-response") return;
    const message = data.message || data.response;
    if (!nativeSelectionOnly) rememberThreadProvidersFromResponse(data, message);
    const requestId = message?.id == null ? "" : String(message.id);
    const isModelListResponse = (
      modelListRequestIds.has(requestId)
      || data.requestMethod === "model/list"
      || message?.requestMethod === "model/list"
    );
    if (!isModelListResponse) {
      if (nativeSelectionOnly) return;
      replaceThreadDisplayPayload(message, "result");
      applyThreadDisplayPatch(data);
      return;
    }
    modelListRequestIds.delete(requestId);
    const patched = patchedModelPayload(message?.result);
    if (!patched.changed) return;
    try {
      message.result = patched.value;
    } catch {
      // Immutable bridge messages fall back to cached-query patching.
    }
    scheduleRefresh(1000);
  };

  // The wrapped getDynamicConfig already patches results on read, so the
  // interaction-driven re-apply is only a safety net for clients created
  // between events. Rescanning every Statsig memo cache on every pointerdown
  // and focusin is far more often than that safety net needs.
  let lastInteractionApply = 0;
  const interactionApplyIntervalMs = 2_000;
  const handleInteraction = (event) => {
    const pickerInteraction = Boolean(
      event?.target?.closest?.(`${groupedMenuSelector}, [aria-haspopup]`),
    );
    if (nativeSelectionOnly && !pickerInteraction) return;
    if (!nativeSelectionOnly) {
      installGroupedModelMenuObserver();
      rememberMenuRouteIntent(event);
    }
    scheduleGroupedModelMenuEnhancement();
    const now = Date.now();
    if (!pickerInteraction && now - lastInteractionApply < interactionApplyIntervalMs) return;
    lastInteractionApply = now;
    // A new or reopened task can mount a fresh QueryClient while the previous
    // client remains callable. Probe the current React roots on picker
    // interaction so the visible cache is repaired immediately without the
    // expensive full-document discovery pass.
    void deliverModelCatalog({
      discoverQueryClients: true,
      invalidate: false,
    });
  };
  const handleFocus = () => {
    void loadModelCatalog();
  };
  interactionEvents.forEach((eventName) => {
    document.addEventListener(eventName, handleInteraction, true);
  });
  if (!nativeSelectionOnly) {
    installGroupedModelMenuObserver();
    restoreThreadRoutes();
  }
  window.addEventListener?.("focus", handleFocus);
  if (!nativeSelectionOnly) installModelRequestDispatchPatch();
  if (typeof window.addEventListener === "function") {
    window.addEventListener(modelRequestEvent, handleModelRequest, true);
    window.addEventListener(modelResponseEvent, handleModelResponse, true);
    deliveryState.responsePatchInstalled = true;
  }

  const api = {
    version: patchVersion,
    nativeSelectionOnly,
    apply: applyModelWhitelist,
    refresh: loadModelCatalog,
    setCatalog: setModelCatalog,
    // The Codex renderer calls electronBridge before emitting its diagnostic
    // CustomEvent. The startup source gate invokes this synchronous hook at the
    // real transport boundary so thread/start receives modelProvider in time.
    rewriteOutgoingMessage: rewrittenOutgoingMessage,
    rewriteIncomingResult: (method, result) => (
      method === "model/list" ? patchedModelPayload(result).value : result
    ),
    trackOutgoingMessage: (detail) => {
      // AppServerRequestClient runs rewriteOutgoingMessage before createRequest
      // assigns an id. Track the concrete request too, otherwise a later native
      // model/list reply can bypass the response patch and replace Codey's hot
      // catalog after a completed turn.
      rememberOutgoingModelListRequest(detail);
      if (!nativeSelectionOnly) rememberOutgoingThreadRequest(detail);
      return detail;
    },
    isBlockedOutgoingMessage: (detail) => Boolean(blockedProviderRequest(detail)),
    notifyBlockedOutgoingMessage: showBlockedProviderNotice,
    enhanceModelMenus: enhanceGroupedModelMenus,
    delivery: () => ({ ...deliveryState }),
    snapshot: () => ({
      loaded: catalog.loaded,
      models: [...catalog.models],
      defaultModel: catalog.defaultModel,
    }),
    dispose() {
      disposed = true;
      window.clearTimeout(refreshTimer);
      refreshTimer = 0;
      window.clearTimeout(groupedMenuTimer);
      groupedMenuTimer = 0;
      window.clearTimeout(providerMismatchNoticeTimer);
      providerMismatchNoticeTimer = 0;
      document.getElementById?.("codey-provider-mismatch-notice")?.remove?.();
      groupedMenuObserver?.disconnect?.();
      groupedMenuObserver = null;
      for (const observer of groupedMenuTextObservers.values()) {
        observer.disconnect?.();
      }
      groupedMenuTextObservers.clear();
      interactionEvents.forEach((eventName) => {
        document.removeEventListener(eventName, handleInteraction, true);
      });
      window.removeEventListener?.("focus", handleFocus);
      window.removeEventListener?.(modelRequestEvent, handleModelRequest, true);
      window.removeEventListener?.(modelResponseEvent, handleModelResponse, true);
      if (patchedDispatchEvent && window.dispatchEvent === patchedDispatchEvent) {
        window.dispatchEvent = originalDispatchEvent;
      }
      originalDispatchEvent = null;
      patchedDispatchEvent = null;
      knownModelQueryClients.clear();
      modelListRequestIds.clear();
      pendingThreadRequests.clear();
      threadPersistedProviders.clear();
      threadRuntimeProviders.clear();
      threadRoutes.clear();
      pendingRouteIntent = null;
    },
  };
  window.__codeyModelWhitelistPatch = api;
  void loadModelCatalog();
})();
