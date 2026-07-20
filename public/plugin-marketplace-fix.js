(() => {
  if (window.__codeyPluginMarketplaceFixInstalled) return;
  window.__codeyPluginMarketplaceFixInstalled = true;
  const bridge = (path, payload = {}) => {
    const call = window.__codeyCall || window.__codeyBridge;
    return typeof call === "function" ? call(path, payload) : Promise.resolve({ status: "failed" });
  };
  window.__codeyLocalPlugins = [];
  const refreshLocalPlugins = () => {
    void bridge("/plugins/list", {}).then((result) => {
      if (result?.status === "failed") return;
      window.__codeyLocalPlugins = Array.isArray(result?.plugins) ? result.plugins : [];
      window.dispatchEvent(new CustomEvent("codey-plugin-marketplace-refresh", { detail: result }));
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
  const mergePlugins = (value) => {
    if (Array.isArray(value)) {
      const current = value.map(mergePlugins);
      const existing = new Set(current.filter(pluginLike).map((plugin) => plugin.id || `${plugin.name}@${plugin.marketplaceName || ""}`));
      for (const plugin of window.__codeyLocalPlugins || []) {
        const normalized = normalizePlugin(plugin);
        const key = normalized.id || `${normalized.name}@${normalized.marketplaceName || ""}`;
        if (!existing.has(key)) current.push(normalized);
      }
      return current;
    }
    if (!value || typeof value !== "object") return value;
    const output = normalizePlugin(value);
    for (const [key, child] of Object.entries(output)) {
      if (child && typeof child === "object") output[key] = mergePlugins(child);
    }
    return output;
  };
  const patchResponse = (value) => mergePlugins(value);
  window.__codeyPatchPluginResponse = patchResponse;
  const normalizeRequest = (value) => {
    if (Array.isArray(value)) return value.map(normalizeRequest);
    if (!value || typeof value !== "object") return value;
    const output = { ...value };
    if ("includeHidden" in output) output.includeHidden = true;
    if ("includeRemote" in output) output.includeRemote = true;
    for (const [key, child] of Object.entries(output)) {
      if (child && typeof child === "object") output[key] = normalizeRequest(child);
    }
    return output;
  };
  const normalizeRequestArg = (value) => {
    if (typeof value !== "string") return normalizeRequest(value);
    try { return JSON.stringify(normalizeRequest(JSON.parse(value))); } catch { return value; }
  };

  const patchElectronBridge = () => {
    const electronBridge = window.electronBridge;
    if (!electronBridge || typeof electronBridge.sendMessageFromView !== "function" || electronBridge.sendMessageFromView.__codeyPatched) return;
    const original = electronBridge.sendMessageFromView;
    const wrapped = function (...args) {
      let requestText = "";
      try { requestText = JSON.stringify(args); } catch { /* ignore */ }
      const isPluginRequest = /plugin|marketplace|list-plugins|install-plugin|uninstall-plugin/i.test(requestText);
      const normalizedArgs = isPluginRequest ? args.map(normalizeRequestArg) : args;
      const result = original.apply(this, normalizedArgs);
      if (!result || typeof result.then !== "function") return result;
      return result.then((response) => {
        if (!isPluginRequest) return response;
        const patched = patchResponse(response);
        if (/install-plugin|uninstall-plugin/i.test(requestText)) {
          window.__codeyPluginCacheVersion = (window.__codeyPluginCacheVersion || 0) + 1;
          refreshLocalPlugins();
        }
        return patched;
      });
    };
    wrapped.__codeyPatched = true;
    electronBridge.sendMessageFromView = wrapped;
  };

  const originalFetch = window.fetch;
  if (typeof originalFetch === "function") {
    window.fetch = async (...args) => {
      const response = await originalFetch(...args);
      const url = typeof args[0] === "string" ? args[0] : args[0]?.url || "";
      const contentType = response.headers.get("content-type") || "";
      if (!/plugin|marketplace/i.test(url) || !contentType.includes("application/json")) return response;
      try {
        const patched = patchResponse(await response.clone().json());
        const headers = new Headers(response.headers);
        headers.delete("content-length");
        return new Response(JSON.stringify(patched), { status: response.status, statusText: response.statusText, headers });
      } catch {
        return response;
      }
    };
  }
  patchElectronBridge();
  refreshLocalPlugins();
  new MutationObserver(patchElectronBridge).observe(document.documentElement, { childList: true, subtree: true });
})();
