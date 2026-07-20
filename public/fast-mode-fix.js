// Remove Codey's former custom Fast button/request interceptor. Fast is now
// supplied by Codex's native service-tier picker through the model catalog.
(() => {
  const version = "native-1";
  if (window.__codeyFastModeFixInstalled === version) return;

  const previousCleanup = window.__codeyFastModeFixCleanup;
  try {
    if (typeof previousCleanup === "function") previousCleanup();
  } catch {
    // A stale injector must not prevent the native control from taking over.
  }

  const cleanupLegacyFastMode = () => {
    document
      .querySelectorAll('[data-codey-fast-mode-toggle="true"]')
      .forEach((node) => node.remove());
    document.getElementById("codey-fast-mode-style")?.remove();
    try {
      window.localStorage?.removeItem("codey-fast-mode-enabled");
    } catch {
      // Cleanup is still useful when storage is unavailable.
    }

    delete window.__codeyGetFastMode;
    delete window.__codeySetFastMode;
    delete window.__codeyPatchFastModeRequest;
    delete window.__codeyFastModeTestHooks;
    delete window.__codeyFastModeLastRequest;
    delete window.__codeyFastModeDispatcherStatus;
  };

  cleanupLegacyFastMode();
  window.__codeyFastModeFixInstalled = version;
  window.__codeyFastModeFixCleanup = cleanupLegacyFastMode;
})();
