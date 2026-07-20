(() => {
  if (window.__codeyBridgeHelpersInstalled) return;
  window.__codeyBridgeHelpersInstalled = true;
  window.__codeyCall = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };
  window.__codeyRefreshSession = (detail = {}) => window.dispatchEvent(new CustomEvent("codey-session-refresh", { detail }));
})();
