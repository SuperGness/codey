(() => {
  if (window.__codeyPluginMarketplaceFixInstalled) {
    if (typeof window.__codeyEnsurePluginBridge === "function") {
      window.__codeyEnsurePluginBridge();
      return;
    }
    window.__codeyPluginMarketplaceFixInstalled = false;
  }
  window.__codeyPluginMarketplaceFixInstalled = true;
  const bridge = (path, payload = {}) => {
    const call = window.__codeyCall || window.__codeyBridge;
    return typeof call === "function" ? call(path, payload) : Promise.resolve({ status: "failed" });
  };
  window.__codeyLocalPlugins = [];
  let pluginRefreshPromise = null;
  let pluginRefreshQueued = false;
  const refreshLocalPlugins = (queueAfterInflight = false) => {
    if (pluginRefreshPromise) {
      if (queueAfterInflight) pluginRefreshQueued = true;
      return pluginRefreshPromise;
    }
    pluginRefreshPromise = Promise.resolve()
      .then(() => bridge("/plugins/list", {}))
      .then((result) => {
        if (result?.status === "failed") return;
        window.__codeyLocalPlugins = Array.isArray(result?.plugins) ? result.plugins : [];
      })
      .catch(() => {})
      .finally(() => {
        pluginRefreshPromise = null;
        if (pluginRefreshQueued) {
          pluginRefreshQueued = false;
          void refreshLocalPlugins();
        }
      });
    return pluginRefreshPromise;
  };
  const waitForLocalPlugins = () => {
    const refresh = refreshLocalPlugins();
    return new Promise((resolve) => {
      let settled = false;
      const finish = () => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timer);
        resolve();
      };
      const timer = window.setTimeout(finish, 2_000);
      Promise.resolve(refresh).then(finish, finish);
    });
  };
  const pluginLike = (value) => value && typeof value === "object" && ("name" in value || "id" in value) && ("marketplace" in value || "marketplaceName" in value || "marketplacePath" in value || "hidden" in value);
  const normalizePlugin = (plugin) => {
    if (!pluginLike(plugin)) return plugin;
    const output = { ...plugin };
    if (output.hidden === true) output.hidden = false;
    if (!output.marketplaceName) output.marketplaceName = output.marketplace || output.remoteName || "openai-curated";
    if (!output.marketplacePath) output.marketplacePath = output.path || output.localPath || output.marketplaceName;
    return output;
  };
  const mergePlugins = (
    value,
    depth = 0,
    seen = new WeakMap(),
    budget = { remaining: 512 },
  ) => {
    if (!value || typeof value !== "object") return value;
    if (seen.has(value)) return seen.get(value);
    if (depth >= 12 || budget.remaining <= 0) return value;
    if (Array.isArray(value)) {
      const current = [];
      seen.set(value, current);
      for (const child of value) {
        if (budget.remaining <= 0) {
          current.push(child);
          continue;
        }
        budget.remaining -= 1;
        current.push(mergePlugins(child, depth + 1, seen, budget));
      }
      const existing = new Set(current.filter(pluginLike).map((plugin) => plugin.id || `${plugin.name}@${plugin.marketplaceName || ""}`));
      for (const plugin of window.__codeyLocalPlugins || []) {
        const normalized = normalizePlugin(plugin);
        const key = normalized.id || `${normalized.name}@${normalized.marketplaceName || ""}`;
        if (!existing.has(key)) current.push(normalized);
      }
      return current;
    }
    const output = normalizePlugin(value);
    seen.set(value, output);
    for (const [key, child] of Object.entries(output)) {
      if (!child || typeof child !== "object") continue;
      if (budget.remaining <= 0) break;
      budget.remaining -= 1;
      output[key] = mergePlugins(child, depth + 1, seen, budget);
    }
    return output;
  };
  const patchResponse = (value) => mergePlugins(value);
  window.__codeyPatchPluginResponse = patchResponse;
  const normalizeRequest = (
    value,
    depth = 0,
    seen = new WeakMap(),
    budget = { remaining: 128 },
  ) => {
    if (!value || typeof value !== "object" || depth >= 8 || budget.remaining <= 0) return value;
    if (seen.has(value)) return seen.get(value);
    let entries;
    try {
      entries = Object.entries(value);
    } catch {
      return value;
    }
    const output = Array.isArray(value) ? [] : {};
    seen.set(value, output);
    for (const [key, child] of entries) {
      if (budget.remaining <= 0) {
        output[key] = child;
        continue;
      }
      budget.remaining -= 1;
      if (key === "includeHidden" || key === "includeRemote") {
        output[key] = true;
      } else {
        output[key] = normalizeRequest(child, depth + 1, seen, budget);
      }
    }
    return output;
  };
  const normalizeRequestArg = (value) => {
    if (typeof value !== "string") {
      try { return normalizeRequest(value); } catch { return value; }
    }
    try { return JSON.stringify(normalizeRequest(JSON.parse(value))); } catch { return value; }
  };

  const pluginRequestPattern = /plugin|marketplace|list-plugins|install-plugin|uninstall-plugin/i;
  const pluginMutationPattern = /install-plugin|uninstall-plugin/i;
  const directRequestKeys = ["channel", "command", "method", "action", "type", "path", "topic", "url"];
  // requestValueMatchesMethod only ever matches a string that ends with the
  // method name, so the raw text must contain it literally. Scanning for the
  // substring is orders of magnitude cheaper than parsing a large body just to
  // discover it was irrelevant.
  const mayContainMethod = (text, method) => (
    typeof text === "string" && text.toLowerCase().includes(method.toLowerCase())
  );
  const isStructuredRequestBody = (value) => {
    if (!value || typeof value !== "object") return false;
    if (Array.isArray(value)) return true;
    try {
      return Object.prototype.toString.call(value) === "[object Object]";
    } catch {
      return true;
    }
  };
  const requestValueMatchesMethod = (value, method) => {
    if (typeof value !== "string") return false;
    const normalized = value.trim().toLowerCase().split(/[?#]/, 1)[0];
    const expected = method.toLowerCase();
    return normalized === expected
      || normalized.endsWith(`/${expected}`)
      || normalized.endsWith(`:${expected}`)
      || normalized.endsWith(`.${expected}`);
  };
  const requestHasMethod = (
    value,
    method,
    depth = 0,
    seen = new WeakSet(),
    budget = { remaining: 24 },
  ) => {
    if (!value || typeof value !== "object" || depth >= 4 || seen.has(value) || budget.remaining <= 0) {
      return false;
    }
    seen.add(value);
    for (const key of directRequestKeys) {
      let marker;
      try {
        marker = value[key];
      } catch {
        continue;
      }
      if (requestValueMatchesMethod(marker, method)) return true;
    }
    let entries;
    try {
      entries = Object.entries(value);
    } catch {
      return false;
    }
    for (const [key, child] of entries) {
      budget.remaining -= 1;
      if (child && typeof child === "object" && requestHasMethod(child, method, depth + 1, seen, budget)) {
        return true;
      }
      if (key === "body" && typeof child === "string" && mayContainMethod(child, method)) {
        try {
          if (requestHasMethod(JSON.parse(child), method, depth + 1, seen, budget)) return true;
        } catch {}
      }
      if (budget.remaining <= 0) break;
    }
    return false;
  };
  const argsHaveRequestMethod = (args, method) => args.some((value) => requestHasMethod(value, method));
  const requestHasMarker = (value, pattern, depth = 0, seen = new WeakSet(), budget = { remaining: 24 }) => {
    if (typeof value === "string") return pattern.test(value);
    if (!value || typeof value !== "object" || depth >= 3 || seen.has(value) || budget.remaining <= 0) {
      return false;
    }
    seen.add(value);
    let entries;
    try {
      entries = Object.entries(value);
    } catch {
      return false;
    }
    for (const [key, child] of entries) {
      budget.remaining -= 1;
      if (pattern.test(key) || requestHasMarker(child, pattern, depth + 1, seen, budget)) {
        return true;
      }
      if (budget.remaining <= 0) break;
    }
    return false;
  };
  const requestMatches = (value, pattern) => {
    if (typeof value === "string") return pattern.test(value);
    if (!value || typeof value !== "object") return false;
    for (const key of directRequestKeys) {
      let marker;
      try {
        marker = value[key];
      } catch {
        continue;
      }
      if (typeof marker !== "string") continue;
      if (pattern.test(marker)) return true;
    }
    try {
      return requestHasMarker(value, pattern);
    } catch {
      return false;
    }
  };
  const argsMatch = (args, pattern) => args.some((value) => requestMatches(value, pattern));

  let bridgeRetryTimer = 0;
  let bridgeRetryDelay = 50;
  let bridgeRetryDeadline = Date.now() + 30_000;
  // 慢速重试（30s 周期）封顶：桥长期缺席时不再无限期探测。
  // __codeyEnsurePluginBridge 会重置计数并重新打开 30s 快速窗口。
  const MAX_SLOW_BRIDGE_RETRIES = 20;
  let bridgeSlowRetries = 0;
  const markPluginBridgeEffective = () => {
    const entry = window.__codeyInjectionStatus?.["plugin-marketplace-compatibility"];
    if (!entry || entry.status === "failed") return;
    const changed = entry.status !== "effective" || entry.detail !== "插件市场桥接已接管";
    entry.status = "effective";
    entry.detail = "插件市场桥接已接管";
    entry.error = null;
    if (changed) {
      window.dispatchEvent(new CustomEvent("codey-injection-status-changed", {
        detail: { id: "plugin-marketplace-compatibility", status: "effective" },
      }));
    }
  };
  const patchElectronBridge = () => {
    const electronBridge = window.electronBridge;
    if (!electronBridge || typeof electronBridge.sendMessageFromView !== "function") return false;
    if (electronBridge.sendMessageFromView.__codeyPatched) {
      window.clearTimeout(bridgeRetryTimer);
      markPluginBridgeEffective();
      return true;
    }
    const original = electronBridge.sendMessageFromView;
    const wrapped = function (...args) {
      let isPluginRequest = false;
      let isPluginListRequest = false;
      try {
        isPluginRequest = argsMatch(args, pluginRequestPattern);
        isPluginListRequest = argsHaveRequestMethod(args, "list-plugins");
      } catch {}
      // Every IPC message passes through here; unrelated requests must not
      // pay for an extra promise hop or a second argument walk.
      if (!isPluginRequest && !isPluginListRequest) return original.apply(this, args);
      const normalizedArgs = isPluginRequest ? args.map(normalizeRequestArg) : args;
      const result = original.apply(this, normalizedArgs);
      if (!result || typeof result.then !== "function") return result;
      const localRefresh = isPluginListRequest ? waitForLocalPlugins() : Promise.resolve();
      return Promise.all([result, localRefresh]).then(([response]) => {
        if (!isPluginRequest) return response;
        let patched = response;
        try {
          patched = patchResponse(response);
        } catch {}
        if (argsMatch(args, pluginMutationPattern)) {
          refreshLocalPlugins(true);
        }
        return patched;
      });
    };
    wrapped.__codeyPatched = true;
    electronBridge.sendMessageFromView = wrapped;
    window.clearTimeout(bridgeRetryTimer);
    markPluginBridgeEffective();
    return true;
  };
  const retryPatchElectronBridge = () => {
    bridgeRetryTimer = 0;
    if (patchElectronBridge()) return;
    const fastRetry = Date.now() < bridgeRetryDeadline;
    if (!fastRetry) {
      if (bridgeSlowRetries >= MAX_SLOW_BRIDGE_RETRIES) return;
      bridgeSlowRetries += 1;
    }
    const delay = fastRetry ? bridgeRetryDelay : 30_000;
    if (fastRetry) bridgeRetryDelay = Math.min(bridgeRetryDelay * 2, 2_000);
    bridgeRetryTimer = window.setTimeout(retryPatchElectronBridge, delay);
  };
  window.__codeyEnsurePluginBridge = () => {
    bridgeRetryDeadline = Date.now() + 30_000;
    bridgeRetryDelay = 50;
    bridgeSlowRetries = 0;
    if (patchElectronBridge()) return;
    window.clearTimeout(bridgeRetryTimer);
    bridgeRetryTimer = window.setTimeout(retryPatchElectronBridge, bridgeRetryDelay);
  };

  const registerFetchInterceptor = window.__codeySharedRuntime?.registerFetchInterceptor;
  if (typeof registerFetchInterceptor === "function") {
    registerFetchInterceptor("plugin-marketplace", (next, ...args) => {
      const url = typeof args[0] === "string" ? args[0] : args[0]?.url || "";
      const body = args[1]?.body;
      const patchesPluginResponse = /plugin|marketplace/i.test(url);
      const urlRequestsPluginList = requestValueMatchesMethod(url, "list-plugins");
      const bodyIsStructuredRequest = isStructuredRequestBody(body);
      const bodyMayRequestPluginList =
        bodyIsStructuredRequest || mayContainMethod(body, "list-plugins");
      if (
        !patchesPluginResponse &&
        !urlRequestsPluginList &&
        !bodyMayRequestPluginList
      ) {
        return next(...args);
      }

      let isPluginListRequest = urlRequestsPluginList;
      if (!isPluginListRequest && bodyIsStructuredRequest) {
        isPluginListRequest = requestHasMethod(body, "list-plugins");
      } else if (!isPluginListRequest && mayContainMethod(body, "list-plugins")) {
        try {
          isPluginListRequest = requestHasMethod(JSON.parse(body), "list-plugins");
        } catch {}
      }

      const responsePromise = next(...args);
      const ready = isPluginListRequest
        ? Promise.all([responsePromise, waitForLocalPlugins()]).then(([response]) => response)
        : responsePromise;
      return ready.then(async (response) => {
        const contentType = response.headers.get("content-type") || "";
        if (!patchesPluginResponse || !contentType.includes("application/json")) return response;
        try {
          const patched = patchResponse(await response.clone().json());
          const headers = new Headers(response.headers);
          headers.delete("content-length");
          return new Response(JSON.stringify(patched), { status: response.status, statusText: response.statusText, headers });
        } catch {
          return response;
        }
      });
    }, 30);
  }
  const bridgePatched = patchElectronBridge();
  if (!bridgePatched) {
    bridgeRetryTimer = window.setTimeout(retryPatchElectronBridge, bridgeRetryDelay);
  }
})();
