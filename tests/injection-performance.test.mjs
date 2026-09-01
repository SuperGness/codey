import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const root = new URL("../", import.meta.url);
const readSource = async (path) =>
  (await readFile(new URL(path, root), "utf8")).replace(/\r\n/g, "\n");

test("renderer core keeps session tools lazy until sidebar use or an active task", async () => {
  const [inject, sessionTools, bridge, petShield, securityShield, promptOptimize] = await Promise.all([
    readSource("public/renderer-inject.js"),
    readSource("public/codey-inject.js"),
    readSource("public/codey-bridge.js"),
    readSource("public/pet-control-shield.js"),
    readSource("public/security-warning-shield.js"),
    readSource("public/prompt-optimize.js"),
  ]);

  assert.match(inject, /const queryWithin = \(root, selector\)/);
  assert.match(inject, /const sessionToolsLoadPath = "\/internal\/codey\/session-tools\/load"/);
  assert.match(inject, /const sidebarSelector = \[/);
  assert.match(inject, /const loadSessionTools = \(\) =>/);
  assert.ok(
    inject.lastIndexOf("window.__codeyRendererCoreLoaded = true")
      > inject.indexOf("bootstrapObserver.observe"),
    "renderer loaded state must be committed only after bootstrap succeeds",
  );
  assert.doesNotMatch(inject, /const sidebarDetected =/);
  assert.match(
    inject,
    /armSessionToolsInteraction\(\);\s*armSessionToolsRunningProbe\(\);\s*loadSessionToolsForRunningTask\(\);\s*scan\(\);\s*void hydrateUpdateAvailability\(\)/,
  );
  assert.match(inject, /const backendStatusPath = "\/backend\/status"/);
  assert.doesNotMatch(inject, /showUpdateDialog|codey-update-check-status/);
  assert.match(inject, /document\.addEventListener\("pointerover", loadSessionToolsFromInteraction/);
  assert.match(inject, /document\.addEventListener\("focusin", loadSessionToolsFromInteraction/);
  assert.match(inject, /const sessionToolsRunningProbeIntervalMs = 5_000/);
  assert.match(inject, /const nativeTaskControlSelector = "button\[aria-label\], button\[title\], button\[data-testid\]"/);
  assert.match(inject, /armSessionToolsRunningProbe\(\);\s*loadSessionToolsForRunningTask\(\);\s*scan\(\)/);
  assert.match(inject, /window\.setInterval\(\(\) => \{\s*loadSessionToolsForRunningTask\(\);\s*\}, sessionToolsRunningProbeIntervalMs\)/);
  assert.match(inject, /disarmSessionToolsRunningProbe\(\);\s*bootstrapObserver\?\.disconnect/);
  assert.match(inject, /bootstrapObserver\?\.disconnect\(\)/);
  assert.match(inject, /mutationDispatcher\.subscribe\(\s*handleBootstrapMutations/);
  assert.match(inject, /new MutationObserver\(handleBootstrapMutations\)/);
  assert.match(inject, /scheduleScan\(element\)/);
  assert.match(inject, /const mountedButtonIsUsable = \(button\) =>/);
  assert.match(inject, /if \(mountedButtonIsUsable\(existingButton\)\) return;/);
  assert.match(inject, /button\.nextElementSibling === button\.__codeyHeaderAnchor/);
  assert.match(inject, /const isTopChromeMountTarget = \(element\) =>/);
  assert.match(inject, /const visibleMountRect = \(element\) =>/);
  assert.match(inject, /return \{ control, right: rect\.right \};/);
  assert.doesNotMatch(
    inject,
    /control\.getBoundingClientRect\(\)\.right > rightmost\.getBoundingClientRect\(\)\.right/,
  );
  assert.doesNotMatch(inject, /querySelector\("main"\)/);
  assert.match(inject, /headerMountDirty = true/);
  assert.match(inject, /window\.__codeyRendererInvalidateHeaderMount = invalidateHeaderMount/);
  assert.doesNotMatch(inject, /new MutationObserver\(\(\) => \{[\s\S]*setTimeout\(scan,/);
  assert.doesNotMatch(inject, /characterData:\s*true/);
  assert.doesNotMatch(inject, /mutation\.type === "characterData"/);
  assert.doesNotMatch(inject, /const sidebarTitleCache = new Map\(\)/);
  assert.doesNotMatch(inject, /callBridge\("\/session\/wake-watcher"\)/);
  assert.match(sessionTools, /const sidebarTitleCache = new Map\(\)/);
  assert.match(sessionTools, /syncSidebarTitles\(root\)/);
  assert.match(sessionTools, /callBridge\("\/session\/wake-watcher"\)/);
  assert.match(sessionTools, /document\.addEventListener\("pointerdown", wakeSessionWatcher/);
  assert.match(sessionTools, /document\.addEventListener\("keydown", wakeSessionWatcherFromKey/);
  assert.doesNotMatch(sessionTools, /const mountedButtonIsUsable = \(button\) =>/);
  assert.doesNotMatch(sessionTools, /const isTopChromeMountTarget = \(element\) =>/);
  assert.doesNotMatch(sessionTools, /const mountButton = \(\) =>/);
  assert.doesNotMatch(sessionTools, /const settingsIcon = `/);
  assert.doesNotMatch(sessionTools, /querySelector\("main"\)/);
  assert.match(sessionTools, /fallbackSessionExportMaxBytes = 64 \* 1024 \* 1024/);
  assert.match(sessionTools, /exportSize > fallbackSessionExportMaxBytes/);
  assert.match(sessionTools, /watcherWakeTimer = window\.setTimeout\(\(\) => \{[\s\S]*\}, 30_000\)/);
  assert.match(sessionTools, /const stuckCompletionGraceMs = 30_000/);
  assert.match(sessionTools, /const nativeTaskControlSelector = "button\[aria-label\], button\[title\], button\[data-testid\]"/);
  assert.match(sessionTools, /const stuckCompletionProbeIntervalMs = 15_000/);
  assert.match(sessionTools, /const stuckCompletionProbeTimeoutMs = 10_000/);
  assert.match(sessionTools, /const stuckCompletionRecoveryRetryMs = 30_000/);
  assert.match(sessionTools, /const stuckCompletionRecoveryCooldownMs = 60_000/);
  assert.match(sessionTools, /const stuckCompletionRecoveryResetMs = 5 \* 60_000/);
  assert.match(sessionTools, /const stuckCompletionRecoveryMaxAttempts = 3/);
  assert.match(sessionTools, /const stuckRunningStreamGraceMs = 30_000/);
  assert.match(sessionTools, /const stuckRunningStreamMinActivityAdvances = 2/);
  assert.match(sessionTools, /const stuckCompletionBridgePath = "\/session\/completion-state"/);
  assert.match(sessionTools, /const completionRecoveryStateByKey = new Map\(\)/);
  assert.match(sessionTools, /const currentTurnRenderFingerprint = \(turnId\) =>/);
  assert.doesNotMatch(sessionTools, /recoveredCompletionKeys|completionRecoveryCooldownUntil/);
  assert.doesNotMatch(sessionTools, /characterData:\s*true/);
  assert.doesNotMatch(sessionTools, /mutation\.type === "characterData"/);
  assert.match(
    sessionTools,
    /\{ timeoutMs: stuckCompletionProbeTimeoutMs \}/,
  );
  assert.match(
    sessionTools,
    /window\.setInterval\(\(\) => \{\s*void probeStuckTaskCompletion\(\);\s*\}, stuckCompletionProbeIntervalMs\)/,
  );
  assert.match(sessionTools, /window\.addEventListener\("focus", probeStuckTaskCompletion\)/);
  assert.match(sessionTools, /window\.addEventListener\("pageshow", probeStuckTaskCompletion\)/);
  assert.match(sessionTools, /window\.__codeyRendererInvalidateHeaderMount\?\.\(root\)/);
  assert.doesNotMatch(sessionTools, /headerMountDirty/);
  assert.match(sessionTools, /const threadUpdatedAtRows = new Set\(\)/);
  assert.match(sessionTools, /window\.__codeySessionToolsInjectLoading = true/);
  assert.match(sessionTools, /if \(window\.__codeySessionToolsInjectLoading\) return/);
  assert.match(sessionTools, /const scheduleInitialScan = \(\) =>/);
  assert.match(sessionTools, /window\.requestIdleCallback\(run\)/);
  assert.match(
    sessionTools,
    /window\.__codeySessionToolsInjectLoaded = true;\s*window\.__codeySessionToolsInjectLoading = false;\s*void probeStuckTaskCompletion\(\);\s*scheduleInitialScan\(\)/,
  );
  assert.doesNotMatch(sessionTools, /addStyle\(\);\s*scan\(\)/);
  assert.doesNotMatch(sessionTools, /installThreadUpdatedTimes\(document(?:, true)?\)/);
  assert.doesNotMatch(sessionTools, /pendingScanRoots\.add\(document\.documentElement\)/);
  const addPendingScanRootBody = sessionTools.match(
    /const addPendingScanRoot = \(root\) => \{([\s\S]*?)\n  \};/,
  )?.[1] ?? "";
  assert.ok(addPendingScanRootBody.length > 0);
  assert.ok(
    addPendingScanRootBody.indexOf("__codeyRendererInvalidateHeaderMount")
      < addPendingScanRootBody.indexOf("maxPendingScanRoots"),
    "header mount invalidation must run before the pending-root budget check",
  );
  const sessionObserverFilter = sessionTools.match(
    /attributeFilter:\s*\[([\s\S]*?)\],\s*childList:\s*true/,
  )?.[1] ?? "";
  assert.match(sessionObserverFilter, /"class"/);
  assert.match(sessionObserverFilter, /"aria-describedby"/);
  assert.doesNotMatch(sessionObserverFilter, /"style"/);
  assert.doesNotMatch(
    sessionTools,
    /flushThreadUpdatedAtFetch[\s\S]*queryWithin\(document, "\[data-app-action-sidebar-thread-row\]"\)/,
  );
  const sessionObserverBody = sessionTools.match(
    /const handleSessionToolMutations = \(mutations\) => \{([\s\S]*?)\n  \};\n  const sessionToolMutationOptions/,
  )?.[1] ?? "";
  assert.match(sessionObserverBody, /addPendingScanRoot\(threadRow\)/);
  assert.match(sessionObserverBody, /syncConversationRichTooltipOpen\(target\)/);
  assert.doesNotMatch(sessionObserverBody, /syncSidebarThreadTimeState\(threadRow\)/);
  assert.doesNotMatch(sessionObserverBody, /probeStuckTaskCompletion/);
  assert.match(sessionTools, /mutationDispatcher\.subscribe\(\s*handleSessionToolMutations/);
  assert.match(sessionTools, /new MutationObserver\(handleSessionToolMutations\)/);
  assert.match(promptOptimize, /mutationDispatcher\.subscribe\(\s*handleComposerMutations/);
  const modelWhitelist = await readSource("public/model-whitelist-inject.js");
  assert.match(modelWhitelist, /const maxTrackedModelListRequests = 256/);
  assert.match(modelWhitelist, /const maxKnownModelQueryClients = 8/);
  assert.match(modelWhitelist, /knownModelQueryClients\.delete\(client\)/);
  assert.match(modelWhitelist, /dispatcher\.subscribe\(handleGroupedMenuMutations/);
  assert.match(modelWhitelist, /groupedMenuObserver\.observe\(document\.body, \{/);
  assert.doesNotMatch(
    modelWhitelist,
    /groupedMenuObserver\.observe\(document\.body, \{[\s\S]*?characterData:\s*true/,
  );
  assert.match(modelWhitelist, /characterData:\s*true/);
  assert.doesNotMatch(inject, /__codeyBlockNativePetControls/);
  assert.match(petShield, /const block = \(root = document\)/);
  assert.match(petShield, /if \(!enabled\) \{/);
  assert.match(bridge, /window\.__codeyMutationDispatcher = Object\.freeze/);
  assert.match(bridge, /const createShieldLifecycle = \(\{/);
  assert.match(bridge, /const controlsWithin = \(root, selector\) =>/);
  assert.doesNotMatch(petShield, /const controlsWithin = \(root, selector\) =>/);
  assert.match(petShield, /__codeyMutationDispatcher\?\.createShieldLifecycle/);
  assert.match(securityShield, /__codeyMutationDispatcher\.subscribe/);
  assert.doesNotMatch(petShield, /new MutationObserver/);
  assert.doesNotMatch(securityShield, /new MutationObserver/);
});

test("locale bootstrap patches navigator and Statsig independently from renderer controls", async () => {
  const [localeSource, rendererSource] = await Promise.all([
    readSource("public/default-chinese-locale.js"),
    readSource("public/renderer-inject.js"),
  ]);
  assert.doesNotMatch(rendererSource, /installDefaultChineseLocale/);

  function Navigator() {}
  const dynamicConfig = {
    value: {},
    get(key, fallback) {
      return this.value[key] ?? fallback;
    },
  };
  const statsigClient = {
    getDynamicConfig() {
      return dynamicConfig;
    },
  };
  const window = {
    __codeySharedRuntime: {
      statsigClients: () => [statsigClient],
    },
    addEventListener() {},
    navigator: Object.create(Navigator.prototype),
  };
  window.window = window;
  const sandbox = {
    console: { warn() {} },
    Navigator,
    window,
  };

  vm.runInNewContext(localeSource, sandbox);
  const firstState = window.__codeyDefaultChineseLocale;
  assert.equal(window.navigator.language, "zh-CN");
  assert.deepEqual([...window.navigator.languages], ["zh-CN", "zh", "en-US", "en"]);
  assert.equal(dynamicConfig.get("enable_i18n", false), true);
  assert.equal(dynamicConfig.get("locale_source", ""), "SYSTEM");
  assert.equal(firstState.snapshot().locale, "zh-CN");
  assert.equal(firstState.snapshot().statsigClientsPatched, 1);
  assert.match(localeSource, /window\.setTimeout\?\.\(scanStatsigUntilReady, 250\)/);
  assert.doesNotMatch(localeSource, /elapsed < 1000 \? 50/);
  assert.match(localeSource, /wrapStatsigRootInstances/);

  vm.runInNewContext(localeSource, sandbox);
  assert.equal(window.__codeyDefaultChineseLocale, firstState);
  assert.equal(firstState.snapshot().statsigClientsPatched, 1);
});

test("locale bootstrap patches Statsig instances added in place without a 50ms burst", async () => {
  const localeSource = await readSource("public/default-chinese-locale.js");

  function Navigator() {}
  const dynamicConfig = {
    value: {},
    get(key, fallback) {
      return this.value[key] ?? fallback;
    },
  };
  const statsigClient = {
    getDynamicConfig() {
      return dynamicConfig;
    },
  };
  const lateClient = {
    getDynamicConfig() {
      return dynamicConfig;
    },
  };
  const instances = {};
  const window = {
    __codeySharedRuntime: {
      statsigClients: () => [statsigClient, ...Object.values(instances).filter(Boolean)],
    },
    addEventListener() {},
    navigator: Object.create(Navigator.prototype),
    __STATSIG__: {
      firstInstance: statsigClient,
      instances,
    },
  };
  window.window = window;
  const sandbox = {
    console: { warn() {} },
    Navigator,
    Proxy,
    window,
  };

  vm.runInNewContext(localeSource, sandbox);
  window.__STATSIG__.instances.late = lateClient;
  assert.equal(lateClient.__codeyDefaultChineseLocalePatched, true);
  assert.equal(window.__codeyDefaultChineseLocale.snapshot().statsigClientsPatched, 2);
});

test("plugin bridge fast-paths unrelated IPC payloads without a DOM observer", async () => {
  const source = await readSource("public/plugin-marketplace-fix.js");
  const nativeCalls = [];
  const localCalls = [];
  const window = {
    __codeyCall: async (...args) => {
      localCalls.push(args);
      return {
        plugins: [{
          id: "local-tool@local",
          marketplaceName: "local",
          name: "local-tool",
        }],
      };
    },
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView(...args) {
        nativeCalls.push(args);
        return Promise.resolve({
          plugins: [{
            hidden: true,
            id: "remote-tool@remote",
            marketplace: "remote",
            name: "remote-tool",
          }],
        });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {
      constructor(type, options = {}) {
        this.type = type;
        this.detail = options.detail;
      }
    },
    console,
    window,
  });
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(localCalls.length, 0);

  const cyclicPayload = { channel: "thread-update" };
  cyclicPayload.self = cyclicPayload;
  await window.electronBridge.sendMessageFromView(cyclicPayload);
  assert.equal(nativeCalls[0][0], cyclicPayload);
  assert.equal(localCalls.length, 0);

  const response = await window.electronBridge.sendMessageFromView({
    channel: "list-plugins",
    options: { includeHidden: false, includeRemote: false },
  });
  assert.equal(localCalls.length, 1);
  assert.equal(localCalls[0][0], "/plugins/list");
  assert.equal(nativeCalls[1][0].options.includeHidden, true);
  assert.equal(nativeCalls[1][0].options.includeRemote, true);
  assert.equal(response.plugins[0].hidden, false);
  assert.equal(response.plugins.some((plugin) => plugin.id === "local-tool@local"), true);

  await window.electronBridge.sendMessageFromView({
    channel: "invoke",
    payload: {
      method: "list-plugins",
      options: { includeHidden: false, includeRemote: false },
    },
  });
  assert.equal(localCalls.length, 2);
  assert.equal(nativeCalls[2][0].payload.options.includeHidden, true);
  assert.equal(nativeCalls[2][0].payload.options.includeRemote, true);

  await window.electronBridge.sendMessageFromView({
    type: "invoke",
    payload: {
      request: {
        method: "list-plugins",
        options: { includeHidden: false, includeRemote: false },
      },
    },
  });
  assert.equal(localCalls.length, 3);
  assert.equal(nativeCalls[3][0].payload.request.options.includeHidden, true);
  assert.equal(nativeCalls[3][0].payload.request.options.includeRemote, true);

  const cyclicPluginPayload = {
    channel: "list-plugins",
    options: { includeHidden: false },
  };
  cyclicPluginPayload.self = cyclicPluginPayload;
  await window.electronBridge.sendMessageFromView(cyclicPluginPayload);
  assert.equal(localCalls.length, 4);
  assert.equal(nativeCalls[4][0].options.includeHidden, true);
  assert.equal(nativeCalls[4][0].self, nativeCalls[4][0]);

  const throwingPayload = {};
  Object.defineProperty(throwingPayload, "channel", {
    enumerable: true,
    get() {
      throw new Error("hostile getter");
    },
  });
  await window.electronBridge.sendMessageFromView(throwingPayload);
  assert.equal(nativeCalls[5][0], throwingPayload);
  assert.equal(localCalls.length, 4);

  assert.doesNotMatch(source, /JSON\.stringify\(args\)/);
  assert.doesNotMatch(source, /new MutationObserver/);
  assert.match(source, /directRequestKeys/);
  assert.match(source, /bridgeRetryDelay = Math\.min\(bridgeRetryDelay \* 2, 2_000\)/);
  assert.match(source, /const delay = fastRetry \? bridgeRetryDelay : 30_000/);

  const replacementCalls = [];
  window.electronBridge.sendMessageFromView = (...args) => {
    replacementCalls.push(args);
    return Promise.resolve({ plugins: [] });
  };
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });
  await window.electronBridge.sendMessageFromView({
    channel: "list-plugins",
    options: { includeHidden: false },
  });
  assert.equal(localCalls.length, 5);
  assert.equal(replacementCalls[0][0].options.includeHidden, true);
});

test("plugin fetch wrapper returns unrelated native requests without promise or header work", async () => {
  const [bridgeSource, source] = await Promise.all([
    readSource("public/codey-bridge.js"),
    readSource("public/plugin-marketplace-fix.js"),
  ]);
  const nativeResponse = {
    headers: {
      get() {
        throw new Error("unrelated response headers must not be inspected");
      },
    },
  };
  const nativePromise = Promise.resolve(nativeResponse);
  const fetchCalls = [];
  const window = {
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ status: "ok" });
      },
    },
    fetch(...args) {
      fetchCalls.push(args);
      return nativePromise;
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  const sandbox = {
    CustomEvent: class {},
    console,
    globalThis: window,
    window,
  };
  vm.runInNewContext(bridgeSource, sandbox);
  vm.runInNewContext(source, sandbox);
  assert.equal(window.__codeySharedRuntime.fetchSnapshot().interceptorCount, 1);

  const result = window.fetch("https://api.example/conversation", {
    body: JSON.stringify({ message: "hello" }),
    method: "POST",
  });

  assert.equal(result, nativePromise);
  assert.equal(await result, nativeResponse);

  const objectBodyResult = window.fetch("https://api.example/upload", {
    body: new URLSearchParams({ message: "hello" }),
    method: "POST",
  });
  assert.equal(objectBodyResult, nativePromise);
  assert.equal(await objectBodyResult, nativeResponse);
  assert.equal(fetchCalls.length, 2);
});

test("plugin bridge reports effective only after the runtime method is patched", async () => {
  const source = await readSource("public/plugin-marketplace-fix.js");
  const events = [];
  const window = {
    __codeyInjectionStatus: {
      "plugin-marketplace-compatibility": {
        status: "pending",
        detail: null,
        error: null,
      },
    },
    clearTimeout() {},
    dispatchEvent(event) {
      events.push(event);
    },
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ status: "ok" });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;

  vm.runInNewContext(source, {
    CustomEvent: class {
      constructor(type, options = {}) {
        this.type = type;
        this.detail = options.detail;
      }
    },
    console,
    window,
  });

  const status = window.__codeyInjectionStatus["plugin-marketplace-compatibility"];
  assert.equal(window.electronBridge.sendMessageFromView.__codeyPatched, true);
  assert.equal(status.status, "effective");
  assert.equal(status.detail, "插件市场桥接已接管");
  assert.equal(status.error, null);
  assert.equal(events.length, 1);
  assert.equal(events[0].type, "codey-injection-status-changed");
  assert.equal(events[0].detail.status, "effective");
});

test("a stalled local plugin refresh cannot block the native marketplace list", async () => {
  const source = await readSource("public/plugin-marketplace-fix.js");
  let timeoutCallback;
  const window = {
    __codeyCall() {
      return new Promise(() => {});
    },
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ plugins: [{ id: "native-tool", hidden: true }] });
      },
    },
    setTimeout(callback) {
      timeoutCallback = callback;
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });

  const responsePromise = window.electronBridge.sendMessageFromView({
    channel: "list-plugins",
  });
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(typeof timeoutCallback, "function");
  timeoutCallback();
  const response = await responsePromise;
  assert.equal(response.plugins[0].hidden, false);
});

test("ordinary conversation app requests do not refresh the local plugin marketplace", async () => {
  const source = await readSource("public/plugin-marketplace-fix.js");
  const localCalls = [];
  const window = {
    __codeyCall(...args) {
      localCalls.push(args);
      return Promise.resolve({ plugins: [] });
    },
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ status: "ok" });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });

  await window.electronBridge.sendMessageFromView({
    type: "mcp-request",
    request: {
      id: "tool-call-1",
      method: "tools/call",
      params: { name: "calendar_lookup" },
    },
  });
  await window.electronBridge.sendMessageFromView({
    channel: "thread-update",
    payload: { text: "please use the installed app" },
  });
  assert.equal(localCalls.length, 0);
});

test("plugin response normalization handles cyclic bridge payloads", async () => {
  const source = await readSource("public/plugin-marketplace-fix.js");
  const window = {
    __codeyLocalPlugins: [],
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ status: "ok" });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });
  const response = { plugins: [{ id: "local", hidden: true }] };
  response.self = response;

  const patched = window.__codeyPatchPluginResponse(response);

  assert.equal(patched.self, patched);
  assert.equal(patched.plugins[0].hidden, false);
});

test("plugin mutations queue one trailing list refresh while a refresh is in flight", async () => {
  const source = await readSource("public/plugin-marketplace-fix.js");
  const listResolvers = [];
  let listCalls = 0;
  const window = {
    __codeyCall() {
      listCalls += 1;
      return new Promise((resolve) => listResolvers.push(resolve));
    },
    clearTimeout() {},
    dispatchEvent() {},
    electronBridge: {
      sendMessageFromView() {
        return Promise.resolve({ status: "ok" });
      },
    },
    setTimeout() {
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    CustomEvent: class {},
    console,
    window,
  });

  await window.electronBridge.sendMessageFromView({ method: "install-plugin" });
  assert.equal(listCalls, 1);
  await window.electronBridge.sendMessageFromView({ method: "uninstall-plugin" });
  assert.equal(listCalls, 1);
  listResolvers.shift()({ plugins: [] });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(listCalls, 2);

  listResolvers.shift()({ plugins: [] });
  await Promise.resolve();
});
