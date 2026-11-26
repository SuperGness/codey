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
    key = null,
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
      // Only plugin lists receive local entries; string arrays such as
      // capabilities or screenshots and the marketplace list stay intact.
      if (depth > 0 && key !== "plugins") return current;
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
    for (const [childKey, child] of Object.entries(output)) {
      if (!child || typeof child !== "object") continue;
      if (budget.remaining <= 0) break;
      budget.remaining -= 1;
      output[childKey] = mergePlugins(child, depth + 1, seen, budget, childKey);
    }
    return output;
  };
  const patchResponse = (value) => mergePlugins(value);
  window.__codeyPatchPluginResponse = patchResponse;

  // Codex 26.1120.50957 插件通道：
  // 1) AppServer RPC 经 mcp-request 包络或直接请求发送 plugin/{list,installed,read,install,uninstall}；
  // 2) HTTP 目录查询请求 /ps/plugins/{list,search,workspace/created}。
  const pluginMethods = new Map([
    ["plugin/list", { list: true, mutation: false }],
    ["plugin/installed", { list: true, mutation: false }],
    ["plugin/read", { list: false, mutation: false }],
    ["plugin/install", { list: false, mutation: true }],
    ["plugin/uninstall", { list: false, mutation: true }],
  ]);
  const requestPath = (value) => value.trim().toLowerCase()
    .split(/[?#]/, 1)[0].replace(/^(?:[a-z][a-z\d+.-]*:)?\/\/[^/]+/, "");
  const pluginRoute = (value) => {
    if (typeof value !== "string") return null;
    const path = requestPath(value);
    if (
      /(?:^|\/)(?:ps\/)?plugins\/(?:list|installed|search|workspace\/created)(?:\/|$)/.test(path)
      || /(?:^|\/)marketplace\/list(?:\/|$)/.test(path)
    ) {
      return { list: true, mutation: false };
    }
    return null;
  };
  const isPlainRequestObject = (value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return false;
    try {
      return Object.prototype.toString.call(value) === "[object Object]";
    } catch {
      return false;
    }
  };
  const pluginRequest = (value) => {
    if (typeof value === "string") {
      if (!value.includes("plugin/")) return null;
      try { value = JSON.parse(value); } catch { return null; }
    }
    if (!isPlainRequestObject(value)) return null;
    let type;
    let method;
    try {
      type = value.type;
      method = value.method;
    } catch {
      return null;
    }
    if (typeof method === "string") {
      return pluginMethods.get(method) || null;
    }
    if (type === "mcp-request") {
      try {
        const inner = value.request;
        if (isPlainRequestObject(inner) && typeof inner.method === "string") {
          return pluginMethods.get(inner.method) || null;
        }
      } catch {}
    }
    return null;
  };
  const pluginRequestArgs = (args) => {
    const first = args[0];
    if (first === "mcp-request") {
      const second = args[1];
      if (!isPlainRequestObject(second)) return null;
      try {
        if (typeof second.method === "string") return pluginMethods.get(second.method) || null;
        const inner = second.request;
        if (isPlainRequestObject(inner) && typeof inner.method === "string") {
          return pluginMethods.get(inner.method) || null;
        }
      } catch {}
      return null;
    }
    return pluginRequest(first);
  };

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
      let request = null;
      try {
        request = pluginRequestArgs(args);
      } catch {}
      // Every IPC message passes through here; unrelated requests must not
      // pay for an extra promise hop or a second argument walk.
      if (!request) return original.apply(this, args);
      const result = original.apply(this, args);
      if (!result || typeof result.then !== "function") {
        if (request.mutation) refreshLocalPlugins(true);
        return result;
      }
      const localRefresh = request.list ? waitForLocalPlugins() : Promise.resolve();
      return Promise.all([result, localRefresh]).then(([response]) => {
        let patched = response;
        try {
          patched = patchResponse(response);
        } catch {}
        if (request.mutation) {
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
      const routeRequest = pluginRoute(url);
      const request = routeRequest || pluginRequest(body);
      if (!request) return next(...args);
      const patchesPluginResponse = Boolean(routeRequest);

      const responsePromise = next(...args);
      const ready = request.list
        ? Promise.all([responsePromise, waitForLocalPlugins()]).then(([response]) => response)
        : responsePromise;
      return ready.then(async (response) => {
        if (request.mutation) refreshLocalPlugins(true);
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
