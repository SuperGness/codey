// The renderer UI is kept in codey-inject.js for backwards compatibility.
// This wrapper lets the CDP launcher expose the documented module name.
(() => {
  if (window.__codeyRendererInjectLoaded) return;
  // The launcher injects codey-inject.js immediately after this module. This
  // marker lets diagnostics distinguish a fresh page from an older injection.
  window.__codeyRendererModuleReady = true;
})();
