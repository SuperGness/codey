// Keep Codex's native model allowlist aligned with the current Codey channel.
(() => {
  const patchVersion = "1";
  const existingPatch = window.__codeyModelWhitelistPatch;
  if (existingPatch?.version === patchVersion) {
    void existingPatch.refresh();
    return;
  }
  existingPatch?.dispose?.();

  const modelConfigId = "107580212";
  const modelCatalogPath = "/codex-model-catalog";
  const interactionEvents = ["pointerdown", "focusin"];
  let catalog = {
    loaded: false,
    models: [],
    defaultModel: "",
  };
  let refreshTimer = 0;
  let refreshUntil = 0;
  let catalogLoadPromise = null;
  let disposed = false;

  const uniqueModelNames = (values) => Array.from(new Set(
    (Array.isArray(values) ? values : [])
      .filter((value) => typeof value === "string")
      .map((value) => value.trim())
      .filter(Boolean),
  ));

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
    ) {
      return null;
    }
    const models = uniqueModelNames(value.models);
    const requestedDefault = [value.default_model, value.model]
      .find((model) => typeof model === "string" && models.includes(model.trim()));
    return {
      loaded: true,
      models,
      defaultModel: requestedDefault?.trim() || models[0] || "",
    };
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
      && value.default_model === catalog.defaultModel
    ) {
      return config;
    }
    const nextConfig = {
      ...config,
      value: {
        ...value,
        available_models: [...catalog.models],
        default_model: catalog.defaultModel,
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
          && current?.value?.default_model === catalog.defaultModel
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
        && current?.value?.default_model === catalog.defaultModel
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

  const statsigClients = () => {
    const root = window.__STATSIG__ || globalThis.__STATSIG__;
    if (!root || typeof root !== "object") return [];
    let currentInstance = null;
    try {
      currentInstance = typeof root.instance === "function" ? root.instance() : null;
    } catch {
      currentInstance = null;
    }
    return [
      root.firstInstance,
      currentInstance,
      ...(root.instances && typeof root.instances === "object"
        ? Object.values(root.instances)
        : []),
    ].filter((client, index, clients) => (
      client
      && typeof client === "object"
      && clients.indexOf(client) === index
    ));
  };

  const applyModelWhitelist = () => {
    if (!catalog.loaded || disposed) return false;
    let changed = false;
    statsigClients().forEach((client) => {
      if (patchStatsigClient(client)) changed = true;
    });
    return changed;
  };

  const scheduleRefresh = (durationMs = 5000) => {
    if (disposed) return;
    refreshUntil = Math.max(refreshUntil, Date.now() + durationMs);
    if (refreshTimer) return;
    const tick = () => {
      refreshTimer = 0;
      if (catalog.loaded) {
        applyModelWhitelist();
      } else {
        void loadModelCatalog();
      }
      if (!disposed && Date.now() < refreshUntil) {
        refreshTimer = window.setTimeout(tick, 120);
      }
    };
    refreshTimer = window.setTimeout(tick, 0);
  };

  const loadModelCatalog = () => {
    if (catalogLoadPromise) return catalogLoadPromise;
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
        catalog = nextCatalog;
        applyModelWhitelist();
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

  const handleInteraction = () => {
    applyModelWhitelist();
  };
  const handleFocus = () => {
    void loadModelCatalog();
  };
  interactionEvents.forEach((eventName) => {
    document.addEventListener(eventName, handleInteraction, true);
  });
  window.addEventListener?.("focus", handleFocus);

  const api = {
    version: patchVersion,
    apply: applyModelWhitelist,
    refresh: loadModelCatalog,
    snapshot: () => ({
      loaded: catalog.loaded,
      models: [...catalog.models],
      defaultModel: catalog.defaultModel,
    }),
    dispose() {
      disposed = true;
      window.clearTimeout(refreshTimer);
      refreshTimer = 0;
      interactionEvents.forEach((eventName) => {
        document.removeEventListener(eventName, handleInteraction, true);
      });
      window.removeEventListener?.("focus", handleFocus);
    },
  };
  window.__codeyModelWhitelistPatch = api;
  void loadModelCatalog();
})();
