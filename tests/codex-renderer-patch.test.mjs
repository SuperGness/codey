import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadStartupPatchExpression(
  disablePet = true,
  errorLoggerExecutable = null,
) {
  const template = normalizeLineEndings(
    await readFile(
      new URL("../backend/src/codex_startup_patch.js", import.meta.url),
      "utf8",
    ),
  );
  assert.ok(template);
  const expression = template.replaceAll(
    "__DISABLE_PET__",
    disablePet ? "true" : "false",
  ).replaceAll("__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__", "false");
  return errorLoggerExecutable == null
    ? expression
    : expression.replaceAll(
        '"__CODEY_ERROR_LOGGER_EXECUTABLE__"',
        JSON.stringify(errorLoggerExecutable),
      );
}

test("an incompatible optional renderer patch never blocks the Codex module response", async () => {
  const Module = process.getBuiltinModule("module");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  let installedHandler = null;
  class FakeEmitter {
    constructor() {
      this.listeners = new Map();
    }

    on(name, listener) {
      const listeners = this.listeners.get(name) || [];
      listeners.push({ listener, once: false });
      this.listeners.set(name, listeners);
      return this;
    }

    once(name, listener) {
      const listeners = this.listeners.get(name) || [];
      listeners.push({ listener, once: true });
      this.listeners.set(name, listeners);
      return this;
    }

    removeListener(name, listener) {
      const listeners = this.listeners.get(name) || [];
      this.listeners.set(
        name,
        listeners.filter((entry) => entry.listener !== listener),
      );
      return this;
    }

    emit(name, ...args) {
      const listeners = [...(this.listeners.get(name) || [])];
      this.listeners.set(
        name,
        listeners.filter((entry) => !entry.once),
      );
      listeners.forEach((entry) => entry.listener(...args));
    }
  }
  class FakeWebContents extends FakeEmitter {
    constructor() {
      super();
      this.currentUrl = "";
      this.loadedUrls = [];
      this.destroyed = false;
      this.backgroundThrottling = [];
    }

    getURL() {
      return this.currentUrl;
    }

    loadURL(url) {
      this.currentUrl = url;
      this.loadedUrls.push(url);
      this.emit("did-start-navigation", {}, url);
      return Promise.resolve();
    }

    setBackgroundThrottling(enabled) {
      this.backgroundThrottling.push(enabled);
    }
  }
  class FakeBrowserWindow extends FakeEmitter {
    constructor(options = {}) {
      super();
      this.options = options;
      this.webContents = new FakeWebContents();
      this.destroyed = false;
      this.destroyCalls = 0;
    }

    destroy() {
      if (this.destroyed) return;
      this.destroyed = true;
      this.destroyCalls += 1;
      this.webContents.destroyed = true;
      this.webContents.emit("destroyed");
      this.emit("closed");
    }

    isDestroyed() {
      return this.destroyed;
    }

    loadURL(url) {
      return this.webContents.loadURL(url);
    }
  }
  const fakeElectron = {
    app: { getVersion: () => "26.826.1724" },
    BrowserWindow: FakeBrowserWindow,
    protocol: {
      handle(scheme, handler) {
        assert.equal(scheme, "app");
        installedHandler = handler;
      },
    },
  };
  const fakeAvatarOverlayNative = { createController: () => ({}) };
  Module._load = function testElectronLoader(request) {
    if (request === "electron") return fakeElectron;
    if (request === "C:\\Codex\\avatar_overlay.node") {
      return fakeAvatarOverlayNative;
    }
    return Reflect.apply(nativeLoad, this, arguments);
  };

  const nativeConsoleError = console.error;
  const childProcess = process.getBuiltinModule("child_process");
  const nativeSpawn = childProcess.spawn;
  const nativeSpawnSync = childProcess.spawnSync;
  const asyncLogSpawns = [];
  const syncLogSpawns = [];
  childProcess.spawn = (command, args, options) => {
    const child = new FakeEmitter();
    const stdin = new FakeEmitter();
    const call = { command, args, options, input: null, encoding: null };
    stdin.end = (input, encoding) => {
      call.input = input;
      call.encoding = encoding;
      queueMicrotask(() => child.emit("exit", 0));
    };
    child.stdin = stdin;
    child.kill = () => true;
    child.unref = () => child;
    asyncLogSpawns.push(call);
    return child;
  };
  childProcess.spawnSync = (command, args, options) => {
    syncLogSpawns.push({ command, args, options });
    return { status: 0, stderr: "" };
  };
  const patchErrors = [];
  console.error = (...args) => {
    patchErrors.push(args);
  };

  try {
    assert.equal(
      (0, eval)(await loadStartupPatchExpression(true, "C:\\Codey\\codey.exe")),
      "codey-startup-patch-installed-v38",
    );
    const electron = Module._load("electron", undefined, false);
    const petSurface = new electron.BrowserWindow({ title: "Pet Surface test" });
    assert.equal(petSurface.destroyed, false);
    const avatarOverlayWindow = new electron.BrowserWindow({
      width: 356,
      height: 320,
      alwaysOnTop: true,
      transparent: true,
      focusable: false,
      show: false,
      frame: false,
      skipTaskbar: true,
      webPreferences: { backgroundThrottling: false },
    });
    assert.equal(avatarOverlayWindow.destroyed, false);
    assert.equal(
      avatarOverlayWindow.options.webPreferences.backgroundThrottling,
      false,
    );
    avatarOverlayWindow.emit("show");
    avatarOverlayWindow.emit("hide");
    assert.deepEqual(
      avatarOverlayWindow.webContents.backgroundThrottling,
      [],
    );
    assert.equal(petSurface.options.webPreferences, undefined);
    assert.equal(
      Module._load("C:\\Codex\\avatar_overlay.node", undefined, false),
      fakeAvatarOverlayNative,
    );
    const routeWindow = new electron.BrowserWindow({ title: "Codex" });
    await routeWindow.webContents.loadURL(
      "app://-/index.html?initialRoute=%2Favatar-overlay",
    );
    assert.equal(routeWindow.destroyed, false);
    assert.deepEqual(routeWindow.webContents.loadedUrls, [
      "app://-/index.html?initialRoute=%2Favatar-overlay",
    ]);
    assert.equal(globalThis.__CODEY_CODEX_STARTUP_PATCH__.disablePet, true);
    assert.equal(
      Object.hasOwn(globalThis.__CODEY_CODEX_STARTUP_PATCH__, "petManagerSourceRemoved"),
      false,
    );
    const upstreamHandler = async () =>
      new Response(
        [
          "useHiddenModels:",
          "availableModels:",
          "includeUltraReasoningEffort",
          "amazonBedrock",
        ].join(" "),
      );
    electron.protocol.handle("app", upstreamHandler);
    assert.equal(typeof installedHandler, "function");

    const response = await installedHandler({
      url: "app://-/assets/app-initial-new-codex-build.js",
    });
    assert.equal(response.ok, true);
    assert.match(await response.text(), /useHiddenModels:/);
    // Each incompatible gate is skipped independently (and logged) instead of one
    // throw discarding every gate on the asset. The response is never blocked and
    // the source is returned unchanged when nothing matched.
    assert.ok(patchErrors.length >= 1);
    for (const [message] of patchErrors) {
      assert.match(String(message), /incompatible Codex renderer patch/);
    }
    assert.equal(syncLogSpawns.length, 0);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(asyncLogSpawns.length, 1);
    assert.deepEqual(
      JSON.parse(asyncLogSpawns[0].input).map(({ operation }) => operation),
      [
        "renderer_patch:model allowlist",
        "renderer_patch:model visibility",
      ],
    );
    // Every skipped gate must carry printable excerpts around its neighborhood
    // anchors so an incompatible field bundle can be adapted without access to
    // that exact build.
    for (const record of JSON.parse(asyncLogSpawns[0].input)) {
      assert.equal(record.versions.codex, "26.826.1724");
      assert.equal(record.context.matchCount, 0);
      assert.ok(
        Array.isArray(record.context.excerpts) &&
          record.context.excerpts.length > 0 &&
          record.context.excerpts.every((excerpt) => excerpt.length > 0),
        `${record.operation} must carry anchor excerpts for field diagnosis`,
      );
      assert.match(
        record.context.excerpts.join("\n"),
        /useHiddenModels|includeUltraReasoningEffort/,
      );
    }

    const repeatedResponse = await installedHandler({
      url: "app://-/assets/app-initial-new-codex-build.js",
    });
    assert.match(await repeatedResponse.text(), /useHiddenModels:/);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(
      patchErrors.length,
      2,
      "the same incompatible source must not rerun failed renderer gates",
    );
    assert.equal(
      asyncLogSpawns.length,
      1,
      "the same incompatible source must not spawn another patch logger",
    );

    const currentRendererSource = [
      "const includeUltraReasoningEffort=!0,isServiceTierAllowed=!0;",
      "function currentModelFilter({additionalAvailableModels:e,authMethod:t,availableModels:n,isCustomModelProvider:r,model:i,useHiddenModels:a}){",
      "return e?.has(i.model)===!0||i.model!==`codex-auto-review`&&",
      "(a&&!r&&t!==`amazonBedrock`?n.has(i.model):!i.hidden)}",
      "function currentComposer(){",
      "let w=!1,F=!0,K=!1,xe=`fast`,M={availableOptions:[{value:`fast`}]};",
      "let Ee=!w&&F&&M.availableOptions.length>1;",
      "let Re=!w&&F&&!K&&xe!=null,ze={enabled:Re};",
      "OQ(`composer.toggleFastMode`,()=>{},ze);",
      "let de=!0,r=!1,pe=de&&!r,Se=`fast`,V=()=>{},H=()=>{},U=()=>{},z=!1,B={},te=[];",
      "let Ze=pe?{labelCandidates:te,onBlur:V,onPointerDown:H,onPointerLeave:U,",
      "selectedServiceTierIconKind:Se,showFastServiceTierIndicator:!0,tooltipOpen:z,triggerRef:B}:void 0;",
      "let view={modelPickerTriggerConfig:Ze,selectedServiceTierIconKind:Se};",
      "if(de&&Ze!=null)view.ready=!0;return {Ee,Re,pe,view}}",
      "`composer.intelligenceDropdown.model.title`;",
      "`composer.intelligenceDropdown.model.rowLabel`;",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(currentRendererSource),
    );
    const currentRendererResponse = await installedHandler({
      url: "app://-/assets/app-initial-current-codex-build.js",
    });
    const patchedCurrentRendererSource = await currentRendererResponse.text();
    assert.match(
      patchedCurrentRendererSource,
      /Ee=!w&&M\.availableOptions\.length>1/,
    );
    assert.match(patchedCurrentRendererSource, /Re=!w&&!K&&xe!=null/);
    assert.match(patchedCurrentRendererSource, /pe=!r/);
    assert.match(patchedCurrentRendererSource, /if\(Ze!=null\)/);
    assert.equal(
      patchErrors.length,
      2,
      "native-compatible model access and current Fast controls must not log skips",
    );

    const serviceTierControlGateOrderings = [
      "settings.availableOptions.length>1&&!draft&&allowed",
      "settings.availableOptions.length>1&&allowed&&!draft",
      "!draft&&settings.availableOptions.length>1&&allowed",
      "!draft&&allowed&&settings.availableOptions.length>1",
      "allowed&&!draft&&settings.availableOptions.length>1",
      "allowed&&settings.availableOptions.length>1&&!draft",
    ];
    for (const [index, gate] of serviceTierControlGateOrderings.entries()) {
      const reorderedServiceTierControlSource = [
        "const isServiceTierAllowed=!0;",
        "function nextComposer(){",
        "let draft=!1,allowed=!0,settings={availableOptions:[{value:`fast`},{value:`auto`}]};",
        "let loading=!1,fastOption=`fast`;",
        `let show=${gate};`,
        "OQ(`composer.toggleFastMode`,()=>{},{enabled:allowed&&!loading&&fastOption!=null});",
        "return show}",
      ].join("");
      electron.protocol.handle(
        "app",
        async () => new Response(reorderedServiceTierControlSource),
      );
      const reorderedServiceTierControlResponse = await installedHandler({
        url: `app://-/assets/app-initial-reordered-service-tier-control-${index}.js`,
      });
      const patchedReorderedServiceTierControlSource =
        await reorderedServiceTierControlResponse.text();
      assert.match(
        patchedReorderedServiceTierControlSource,
        /show=(?:settings\.availableOptions\.length>1&&!draft|!draft&&settings\.availableOptions\.length>1)/,
      );
      assert.doesNotMatch(
        patchedReorderedServiceTierControlSource,
        /show=[^;]*allowed/,
      );
      assert.match(
        patchedReorderedServiceTierControlSource,
        /enabled:!loading&&fastOption!=null/,
      );
      assert.equal(
        patchErrors.length,
        2,
        "reordered service-tier control gates must patch without compatibility errors",
      );
    }

    // Electron 151 hoists the toggle gate into a memoized variable and can
    // place over 2KB of memo-cache code between the `availableOptions`
    // assignment and the `composer.toggleFastMode` registration.
    const memoizedGap = "t[99]===e?t[100]:(d(e),t[99]=e,t[100]=1),".repeat(90);
    const electron151ServiceTierSource = [
      "const isServiceTierAllowed=!0;",
      "let{isServiceTierAllowed:I}=G7o(F),N={availableOptions:[{value:`fast`}]};",
      "let w=!1,q=!1,Se=`fast`,Re=()=>{};",
      `let De=!w&&I&&N.availableOptions.length>1,${memoizedGap}`,
      "let ze=!w&&I&&!q&&Se!=null,Be;",
      "t[45]===ze?Be=t[46]:(Be={enabled:ze},t[45]=ze,t[46]=Be),",
      "U$(`composer.toggleFastMode`,Re,Be);",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(electron151ServiceTierSource),
    );
    const electron151Response = await installedHandler({
      url: "app://-/assets/app-initial-electron151-service-tier.js",
    });
    const patchedElectron151Source = await electron151Response.text();
    assert.match(
      patchedElectron151Source,
      /De=!w&&N\.availableOptions\.length>1/,
    );
    assert.doesNotMatch(
      patchedElectron151Source,
      /De=[^,]*&&I&&/,
      "the entitlement flag must be dropped from the model-aware gate",
    );
    assert.match(patchedElectron151Source, /ze=!w&&!q&&Se!=null/);
    assert.match(patchedElectron151Source, /\{enabled:ze\}/);
    assert.equal(
      patchErrors.length,
      2,
      "Electron 151 memoized service-tier gates must patch without compatibility errors",
    );

    // The wider Electron 151 window can also contain an unrelated, earlier
    // assignment with the same minified shape. Only the gate nearest the unique
    // toggle registration belongs to that control.
    const scopedMemoizedGap =
      "cache[99]===model?cache[100]:(touch(model),cache[99]=model,cache[100]=1);"
        .repeat(48);
    assert.ok(scopedMemoizedGap.length > 2048);
    const scopedServiceTierSource = [
      "const isServiceTierAllowed=!1;",
      "function unrelatedComposer(){",
      "let draft=!1,allowed=!1,settings={availableOptions:[1,2]};",
      "let unrelated=!draft&&allowed&&settings.availableOptions.length>1;",
      "return unrelated}",
      "function scopedComposer(OQ){",
      "let draft=!1,allowed=isServiceTierAllowed,settings={availableOptions:[1,2]};",
      "let show=!draft&&allowed&&settings.availableOptions.length>1;",
      "const cache=[],model={},touch=()=>{};",
      scopedMemoizedGap,
      "OQ(`composer.toggleFastMode`,()=>{},{enabled:show});return show}",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(scopedServiceTierSource),
    );
    const scopedServiceTierResponse = await installedHandler({
      url: "app://-/assets/app-initial-scoped-service-tier.js",
    });
    const patchedScopedServiceTierSource =
      await scopedServiceTierResponse.text();
    assert.match(
      patchedScopedServiceTierSource,
      /unrelated=!draft&&allowed&&settings\.availableOptions\.length>1/,
    );
    assert.match(
      patchedScopedServiceTierSource,
      /show=!draft&&settings\.availableOptions\.length>1/,
    );
    assert.doesNotThrow(() => Function(patchedScopedServiceTierSource));
    const scopedServiceTierResults = Function(
      `${patchedScopedServiceTierSource};return [` +
        "unrelatedComposer(),scopedComposer(()=>{})]",
    )();
    assert.deepEqual(scopedServiceTierResults, [false, true]);
    assert.equal(
      patchErrors.length,
      2,
      "an earlier same-shaped gate must not make the scoped Fast patch ambiguous",
    );

    // Electron 151 minifies `return!1` without a space and interleaves the
    // entitlement cache write between the chatgpt check and the fast_mode
    // requirement lookup.
    const electron151EntitlementSource = [
      "zp.error(`Failed to read service tier for request`,{safe:{},sensitive:{}});",
      "async function YHr(e,t){",
      "let n=await KHr(e,t);",
      "if(n!==`chatgpt`)return!1;",
      "let r=await N2t(e,t,{priority:`critical`});",
      "return e.query.setData(rx,{authMethod:n,hostId:t},r),",
      "r.requirements?.featureRequirements?.fast_mode!==!1}",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(electron151EntitlementSource),
    );
    const entitlementResponse = await installedHandler({
      url: "app://-/assets/app-initial-electron151-entitlement.js",
    });
    const patchedEntitlementSource = await entitlementResponse.text();
    assert.match(
      patchedEntitlementSource,
      /async function YHr\(e,t\)\{return!0\}/,
    );
    assert.doesNotMatch(patchedEntitlementSource, /chatgpt/);
    assert.equal(
      patchErrors.length,
      2,
      "the minified entitlement probe must patch without compatibility errors",
    );

    const routeBridgeSource = [
      "const routeLog={warning(){}};",
      "const routeTransport={postMessage:e=>{let t=!1,n=window.electronBridge;",
      "if(n?.sendMessageFromView){let r=e;n.sendMessageFromView(r).catch(t=>{",
      "r.type!==`log-message`&&routeLog.warning(`Failed to send message from view`,{message:e,error:t})",
      "}),t=!0}",
      "let r=new CustomEvent(`codex-message-from-view`,{detail:e});",
      "t&&(r.__codexForwardedViaBridge=!0),window.dispatchEvent(r)}};",
    ].join("");
    electron.protocol.handle("app", async () => new Response(routeBridgeSource));
    const routeBridgeResponse = await installedHandler({
      url: "app://-/assets/app-initial-route-bridge.js",
    });
    const patchedRouteBridgeSource = await routeBridgeResponse.text();
    assert.match(
      patchedRouteBridgeSource,
      /globalThis\.__codeyModelWhitelistPatch\?\.rewriteOutgoingMessage\?\.\(e\)\?\?e/,
    );
    const sentMessages = [];
    const blockedMessages = [];
    const testRouteWindow = {
      electronBridge: {
        async sendMessageFromView(message) {
          sentMessages.push(message);
        },
      },
      dispatchEvent() {},
    };
    const routeGlobal = {
      __codeyModelWhitelistPatch: {
        rewriteOutgoingMessage(message) {
          return { ...message, routed: true };
        },
        isBlockedOutgoingMessage(message) {
          return message.blocked === true;
        },
        notifyBlockedOutgoingMessage(message) {
          blockedMessages.push(message);
        },
      },
    };
    const routeTransport = Function(
      "window",
      "globalThis",
      "CustomEvent",
      `${patchedRouteBridgeSource};return routeTransport`,
    )(
      testRouteWindow,
      routeGlobal,
      class CustomEvent {
        constructor(type, init) {
          this.type = type;
          this.detail = init.detail;
        }
      },
    );
    routeTransport.postMessage({ type: "mcp-request" });
    await Promise.resolve();
    assert.deepEqual(sentMessages, [{ type: "mcp-request", routed: true }]);
    routeTransport.postMessage({ type: "mcp-request", blocked: true });
    await Promise.resolve();
    assert.deepEqual(sentMessages, [{ type: "mcp-request", routed: true }]);
    assert.deepEqual(blockedMessages, [{
      type: "mcp-request",
      blocked: true,
      routed: true,
    }]);
    assert.equal(
      patchErrors.length,
      2,
      "the current Codex bridge preflight must patch without compatibility errors",
    );

    const appServerRequestSource = [
      "class AppServerRequestClient{",
      "constructor(){this.hostId=`local`;this.sent=[];this.queuedRequests=[];this.requestPromises=new Map();this.useHostRequestScheduler=!1;this.dispatchMessage=(e,t)=>this.sent.push({type:e,payload:t})}",
      "createRequest(e,t,n,r=null){return{request:{id:`req-1`,method:e,params:t},promise:Promise.resolve({ok:!0})}}",
      "startRequest(){} onError(){} emitRequestLifecycleEvent(){} pumpQueue(){let e=this.queuedRequests.shift();e?.dispatch()}",
      "onResult(e,t,n){let r=this.requestPromises.get(e);if(r){this.requestPromises.delete(e),r.resolve(t),this.emitRequestLifecycleEvent({type:`completed`,hostId:this.hostId,method:r.method});return}}",
      "enqueueRequest(e,t,n,r=t=>{this.dispatchMessage?.(`mcp-request`,{request:t,hostId:this.hostId,...t.trace==null?{}:{dispatchedAtMs:Date.now()},priority:Mjt(e,n),source:Ejt(e,n?.source),timeoutMs:n?.timeoutMs,expiresAtMs:n?.timeoutMs!=null&&n.timeoutMs>0?Date.now()+n.timeoutMs:void 0,widget:n?.widget})},i=null){let a=Mjt(e,n),o=Ejt(e,n?.source);let{request:s,promise:c}=this.createRequest(e,t,n,i);return this.queuedRequests.push({dispatch:()=>{this.startRequest(s);try{r(s)}catch(e){this.onError(s.id,e)}},priority:a}),this.pumpQueue(),c}",
      "async sendRequest(e,t,n){return this.enqueueRequest(e,t,n)}",
      "}",
      "function Mjt(){return `critical`}function Ejt(){return `source`}",
      "const appServerPatchSignals=`AppServerRequestClient is missing a message dispatcher mcp_request_enqueued`;",
    ].join("");
    electron.protocol.handle("app", async () => new Response(appServerRequestSource));
    const appServerRequestResponse = await installedHandler({
      url: "app://-/assets/app-initial-app-server-request-current-build.js",
    });
    const patchedAppServerRequestSource = await appServerRequestResponse.text();
    assert.match(
      patchedAppServerRequestSource,
      /__codeyModelWhitelistPatch\?\.rewriteOutgoingMessage/,
    );
    const blockedAppServerMessages = [];
    const routedAppServerTypes = [];
    const trackedAppServerMessages = [];
    const appServerGlobal = {
      __codeyModelWhitelistPatch: {
        rewriteOutgoingMessage(detail) {
          routedAppServerTypes.push(detail.type);
          if (detail.request.params.model === "blocked-route/model") {
            return { ...detail, blocked: true };
          }
          return {
            ...detail,
            request: {
              ...detail.request,
              params: {
                model: "route-mt6lv4lx-i2bfax/gpt-5.5",
                modelProvider: "codey_router",
              },
            },
          };
        },
        isBlockedOutgoingMessage(detail) {
          return detail.blocked === true;
        },
        notifyBlockedOutgoingMessage(detail) {
          blockedAppServerMessages.push(detail);
        },
        trackOutgoingMessage(detail) {
          trackedAppServerMessages.push(detail);
        },
        rewriteIncomingResult(method, result) {
          return method === "model/list"
            ? { ...result, data: [...result.data, { model: "route/model" }] }
            : result;
        },
      },
    };
    const AppServerRequestClient = Function(
      "globalThis",
      "Date",
      `${patchedAppServerRequestSource};return AppServerRequestClient`,
    )(appServerGlobal, Date);
    const requestClient = new AppServerRequestClient();
    await requestClient.enqueueRequest("thread/start", {
      model: "route-mt6lv4lx-i2bfax/gpt-5.5",
      model_provider: "openai",
    }, {});
    assert.deepEqual(requestClient.sent[0].payload.request.params, {
      model: "route-mt6lv4lx-i2bfax/gpt-5.5",
      modelProvider: "codey_router",
    });
    await requestClient.enqueueRequest("thread/start", {
      model: "route-mt6lv4lx-i2bfax/gpt-5.5",
      model_provider: "openai",
    }, {}, (request) => {
      requestClient.dispatchMessage?.("thread-prewarm-start", {
        request,
        hostId: requestClient.hostId,
      });
    });
    assert.equal(routedAppServerTypes.at(-1), "mcp-request");
    assert.deepEqual(requestClient.sent[1].payload.request.params, {
      model: "route-mt6lv4lx-i2bfax/gpt-5.5",
      modelProvider: "codey_router",
    });
    await assert.rejects(
      requestClient.enqueueRequest("thread/start", { model: "blocked-route/model" }, {}),
      /Codey blocked cross-provider model request/,
    );
    assert.equal(requestClient.sent.length, 2);
    assert.equal(blockedAppServerMessages.length, 1);
    assert.equal(trackedAppServerMessages.length, 2);
    assert.deepEqual(trackedAppServerMessages[0], {
      type: "mcp-request",
      request: requestClient.sent[0].payload.request,
    });
    const modelListResults = [];
    requestClient.requestPromises.set("model-list", {
      method: "model/list",
      resolve(result) {
        modelListResults.push(result);
      },
    });
    requestClient.onResult("model-list", { data: [{ model: "gpt-5.6-sol" }] });
    assert.deepEqual(modelListResults, [{
      data: [{ model: "gpt-5.6-sol" }, { model: "route/model" }],
    }]);
    assert.equal(
      patchErrors.length,
      2,
      "the AppServerRequestClient route preflight must patch without compatibility errors",
    );

    const hookStatsSource = [
      "const hookLabel=`assistantMessage.hookStats.label`;",
      "const hookTitle=`assistantMessage.hookStats.title`;",
      "function renderHookStats(r,l,d){",
      "return (0,R.jsx)(r,{tooltipContent:l,tooltipClassName:`px-3 py-2`,",
      "tooltipMaxWidth:`min(32rem, var(--radix-tooltip-content-available-width), calc(100vw - 16px))`,",
      "children:d})}",
    ].join("");
    electron.protocol.handle("app", async () => new Response(hookStatsSource));
    const hookStatsResponse = await installedHandler({
      url: "app://-/assets/subagent-activity-chip-group-current-build.js",
    });
    const patchedHookStatsSource = await hookStatsResponse.text();
    assert.match(
      patchedHookStatsSource,
      /\{interactive:!0,tooltipContent:l,tooltipClassName:`px-3 py-2`/,
    );
    assert.equal(
      patchErrors.length,
      2,
      "the compatible hook tooltip patch must not log a skipped renderer gate",
    );


    const petSettingsSource = [
      "import{AvatarPreview as P,builtInPets as L}",
      "from\"./codex-avatar-BpKnWN_W.js\";",
      "const petSettingsId=`settings.appearance.pets.title`;",
      "function renderPetSettings(){return [P(),L.map(()=>1),petSettingsId]}",
    ].join("");
    electron.protocol.handle("app", async () => new Response(petSettingsSource));
    const petSettingsResponse = await installedHandler({
      url: "app://-/assets/general-settings-current-build.js",
    });
    const patchedPetSettingsSource = await petSettingsResponse.text();
    assert.doesNotMatch(patchedPetSettingsSource, /codex-avatar-/);
    assert.match(
      patchedPetSettingsSource,
      /const P=\(\(\)=>\{const target=function\(\)\{return null\}/,
    );
    const renderPetSettings = Function(
      `${patchedPetSettingsSource};return renderPetSettings`,
    )();
    assert.deepEqual(renderPetSettings(), [
      null,
      [],
      "settings.appearance.pets.title",
    ]);

    const sideEffectPetSettingsSource = [
      "import\"./codex-avatar-next-build.js\";",
      "const petSettingsId=`settings.pets.title`;",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(sideEffectPetSettingsSource),
    );
    const sideEffectPetSettingsResponse = await installedHandler({
      url: "app://-/assets/pet-settings-next-build.js",
    });
    const patchedSideEffectPetSettingsSource =
      await sideEffectPetSettingsResponse.text();
    assert.doesNotMatch(patchedSideEffectPetSettingsSource, /codex-avatar-/);
    assert.match(
      patchedSideEffectPetSettingsSource,
      /const petSettingsId=`settings\.pets\.title`/,
    );

    const localeSource = [
      "function resolveLocale(a,bp,Au){",
      "const dynamicConfigId=`72216192`,enableI18n=`enable_i18n`;",
      "let o=a?.get(enableI18n,!1);",
      "let s=o,c=a?.get(`locale_source`,`IDE`),l=bp(Au.localeOverride);",
      "return {enabled:s,source:c,locale:l}}",
    ].join("");
    electron.protocol.handle("app", async () => new Response(localeSource));
    const localeResponse = await installedHandler({
      url: "app://-/assets/app-initial-BHB6SClA.js",
    });
    const patchedLocaleSource = await localeResponse.text();
    assert.match(
      patchedLocaleSource,
      /__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__=!0/,
    );
    assert.doesNotMatch(
      patchedLocaleSource,
      /let s=o,c=a\?\.get\(`locale_source`,`IDE`\),l=bp\(Au\.localeOverride\)/,
    );
    delete globalThis.__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__;
    const resolveLocale = Function(`${patchedLocaleSource};return resolveLocale`)();
    assert.deepEqual(
      resolveLocale(
        { get: () => false },
        () => "en-US",
        { localeOverride: {} },
      ),
      { enabled: true, source: "SYSTEM", locale: "zh-CN" },
    );
    assert.equal(
      globalThis.__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__,
      true,
    );
    delete globalThis.__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__;

    const ownerDiscoverySource = [
      "async function maybeResume(Bm,f,n,t){",
      "if(t.followExistingOwner===!0&&f===`local`&&Bm?.clientCoordination!=null){",
      "let owner=null;",
      "try{owner=await Bm.clientCoordination.findThreadOwner({hostId:f,conversationId:n})}",
      "catch(error){console.warn(`maybe_resume_owner_discovery_failed`,error)}",
      "return owner}",
      "return null}",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(ownerDiscoverySource),
    );
    const ownerDiscoveryResponse = await installedHandler({
      url: "app://-/assets/app-initial-BHB6SClA.js",
    });
    const patchedOwnerDiscoverySource = await ownerDiscoveryResponse.text();
    assert.match(
      patchedOwnerDiscoverySource,
      /__CODEY_THREAD_OWNER_DISCOVERY_V2__/,
    );
    assert.match(
      patchedOwnerDiscoverySource,
      /setTimeout\(\(\)=>\{if\(settled\)return;settled=true;resolve\(null\)\},150\)/,
    );
    assert.doesNotMatch(patchedOwnerDiscoverySource, /expiresAt|\.cache/);
    assert.doesNotMatch(
      patchedOwnerDiscoverySource,
      /owner=await Bm\.clientCoordination\.findThreadOwner/,
    );
    delete globalThis.__CODEY_THREAD_OWNER_DISCOVERY_V2__;
    const maybeResume = Function(
      `${patchedOwnerDiscoverySource};return maybeResume`,
    )();
    const ownerNativeSetTimeout = globalThis.setTimeout;
    const ownerNativeClearTimeout = globalThis.clearTimeout;
    let scheduledOwnerTimers = 0;
    globalThis.setTimeout = (callback, delay, ...args) => {
      scheduledOwnerTimers += 1;
      return ownerNativeSetTimeout(callback, delay, ...args);
    };
    globalThis.clearTimeout = (timer) => ownerNativeClearTimeout(timer);
    let ownerLookupCalls = 0;
    let currentOwner = "existing-owner";
    const primaryCoordination = {
      async findThreadOwner() {
        ownerLookupCalls += 1;
        return currentOwner;
      },
    };
    try {
      assert.equal(
        await maybeResume(
          { clientCoordination: primaryCoordination },
          "local",
          "thread-1",
          { followExistingOwner: true },
        ),
        "existing-owner",
      );
      assert.equal(ownerLookupCalls, 1);
      assert.equal(scheduledOwnerTimers, 1);

      // A settled positive answer must not be reused. The owner may have
      // disconnected before the next hydration attempt, and returning its
      // stale client ID would skip local thread hydration indefinitely.
      currentOwner = null;
      assert.equal(
        await maybeResume(
          { clientCoordination: primaryCoordination },
          "local",
          "thread-1",
          { followExistingOwner: true },
        ),
        null,
      );
      assert.equal(ownerLookupCalls, 2);
      assert.equal(scheduledOwnerTimers, 2);

      // A separate window/client never shares in-flight discovery state.
      let overlayLookupCalls = 0;
      assert.equal(
        await maybeResume(
          {
            clientCoordination: {
              async findThreadOwner() {
                overlayLookupCalls += 1;
                return "overlay-owner";
              },
            },
          },
          "local",
          "thread-1",
          { followExistingOwner: true },
        ),
        "overlay-owner",
      );
      assert.equal(overlayLookupCalls, 1);
      assert.equal(scheduledOwnerTimers, 3);
    } finally {
      globalThis.setTimeout = ownerNativeSetTimeout;
      globalThis.clearTimeout = ownerNativeClearTimeout;
      delete globalThis.__CODEY_THREAD_OWNER_DISCOVERY_V2__;
    }

    // Concurrent hydration attempts in the same renderer share one discovery.
    let resolveSharedOwner;
    let sharedLookupCalls = 0;
    const sharedOwner = new Promise((resolve) => {
      resolveSharedOwner = resolve;
    });
    const sharedCoordination = {
      findThreadOwner() {
        sharedLookupCalls += 1;
        return sharedOwner;
      },
    };
    const sharedOwnerFirst = maybeResume(
      { clientCoordination: sharedCoordination },
      "local",
      "thread-shared",
      { followExistingOwner: true },
    );
    const sharedOwnerSecond = maybeResume(
      { clientCoordination: sharedCoordination },
      "local",
      "thread-shared",
      { followExistingOwner: true },
    );
    await Promise.resolve();
    assert.equal(sharedLookupCalls, 1);
    resolveSharedOwner("shared-owner");
    assert.deepEqual(
      await Promise.all([sharedOwnerFirst, sharedOwnerSecond]),
      ["shared-owner", "shared-owner"],
    );
    delete globalThis.__CODEY_THREAD_OWNER_DISCOVERY_V2__;

    // A timeout is uncertainty, not a negative cache entry. The next attempt
    // must retry discovery and can immediately observe a newly available owner.
    const timeoutCallbacks = [];
    let timeoutLookupCalls = 0;
    globalThis.setTimeout = (callback, delay) => {
      const timer = { callback, delay, cleared: false };
      timeoutCallbacks.push(timer);
      return timer;
    };
    globalThis.clearTimeout = (timer) => {
      timer.cleared = true;
    };
    const timeoutCoordination = {
      findThreadOwner() {
        timeoutLookupCalls += 1;
        if (timeoutLookupCalls === 1) return new Promise(() => {});
        return Promise.resolve("owner-after-timeout");
      },
    };
    try {
      const timedOutOwner = maybeResume(
        { clientCoordination: timeoutCoordination },
        "local",
        "thread-timeout",
        { followExistingOwner: true },
      );
      assert.equal(timeoutCallbacks.length, 1);
      assert.equal(timeoutCallbacks[0].delay, 150);
      timeoutCallbacks[0].callback();
      assert.equal(await timedOutOwner, null);

      assert.equal(
        await maybeResume(
          { clientCoordination: timeoutCoordination },
          "local",
          "thread-timeout",
          { followExistingOwner: true },
        ),
        "owner-after-timeout",
      );
      assert.equal(timeoutLookupCalls, 2);
      assert.equal(timeoutCallbacks.length, 2);
      assert.equal(timeoutCallbacks[1].cleared, true);
    } finally {
      globalThis.setTimeout = ownerNativeSetTimeout;
      globalThis.clearTimeout = ownerNativeClearTimeout;
      delete globalThis.__CODEY_THREAD_OWNER_DISCOVERY_V2__;
    }

    const completedReconciliationSource = [
      "class RunningManager{",
      "constructor(){this.conversation={inProgress:!0,resumeState:`resumed`};",
      "this.role={role:`owner`};this.revision=1;this.statuses=[`idle`,`idle`];this.events=[];",
      "this.productPolicy={runtimePolicy:{isLocalConversationInProgress:e=>e.inProgress===!0}};",
      "this.inactiveThreadUnsubscriber={clearConversationStreamOwnership:e=>{this.events.push(`clear:${e}`);this.role=null}}}",
      "getConversation(){return this.conversation}",
      "getConversationStreamRevision(){return this.revision}",
      "getStreamRole(){return this.role}",
      "async sendRequest(e,t){this.events.push(`read:${e}:${t.threadId}:${t.includeTurns}`);return{thread:{status:this.statuses.shift()??`idle`}}}",
      "updateConversationState(e,t){this.events.push(`state:${e}`);t(this.conversation)}",
      "async maybeResumeConversation(e){this.events.push(`resume:${e.conversationId}`);this.conversation.inProgress=!1;this.role={role:`owner`}}",
      "async resumeConversation(e){await this.maybeResumeConversation(e);return{activeTurnId:null}}",
      "discardConversationFromCache(e){return this.productPolicy.runtimePolicy.isLocalConversationInProgress(this.conversation)&&this.inactiveThreadUnsubscriber.clearConversationStreamOwnership(e)}",
      "}",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(completedReconciliationSource),
    );
    const completedReconciliationResponse = await installedHandler({
      url: "app://-/assets/app-initial-BHB6SClA.js",
    });
    const patchedCompletedReconciliationSource = await completedReconciliationResponse.text();
    assert.match(
      patchedCompletedReconciliationSource,
      /async codeyReconcileCompletedConversation\(/,
    );
    const RunningManager = Function(
      `${patchedCompletedReconciliationSource};return RunningManager`,
    )();
    const reconcileNativeSetTimeout = globalThis.setTimeout;
    const reconcileDelays = [];
    globalThis.setTimeout = (callback, delay) => {
      reconcileDelays.push(delay);
      callback();
      return 1;
    };
    try {
      const completedManager = new RunningManager();
      assert.equal(
        await completedManager.codeyReconcileCompletedConversation({
          conversationId: "thread-completed",
        }),
        true,
      );
      assert.deepEqual(completedManager.events, [
        "read:thread/read:thread-completed:false",
        "read:thread/read:thread-completed:false",
        "clear:thread-completed",
        "state:thread-completed",
        "resume:thread-completed",
      ]);
      assert.deepEqual(reconcileDelays, [250]);
      assert.equal(completedManager.conversation.resumeState, "needs_resume");
      assert.deepEqual(completedManager.role, { role: "owner" });

      const activeManager = new RunningManager();
      activeManager.statuses = ["running"];
      assert.equal(
        await activeManager.codeyReconcileCompletedConversation({
          conversationId: "thread-running",
        }),
        false,
      );
      assert.deepEqual(activeManager.events, [
        "read:thread/read:thread-running:false",
      ]);

      const changedRevisionManager = new RunningManager();
      const sendRequest = changedRevisionManager.sendRequest.bind(changedRevisionManager);
      changedRevisionManager.sendRequest = async (...args) => {
        const response = await sendRequest(...args);
        if (changedRevisionManager.events.length === 2) {
          changedRevisionManager.revision += 1;
        }
        return response;
      };
      assert.equal(
        await changedRevisionManager.codeyReconcileCompletedConversation({
          conversationId: "thread-updated",
        }),
        false,
      );
      assert.deepEqual(changedRevisionManager.events, [
        "read:thread/read:thread-updated:false",
        "read:thread/read:thread-updated:false",
      ]);

      const idleManager = new RunningManager();
      idleManager.conversation.inProgress = false;
      assert.equal(
        await idleManager.codeyReconcileCompletedConversation({ conversationId: "thread-idle" }),
        false,
      );
      assert.deepEqual(idleManager.events, []);

      const followerManager = new RunningManager();
      followerManager.role = { role: "follower", ownerClientId: "other-window" };
      assert.equal(
        await followerManager.codeyReconcileCompletedConversation({
          conversationId: "thread-followed",
        }),
        false,
      );
      assert.deepEqual(followerManager.events, []);
    } finally {
      globalThis.setTimeout = reconcileNativeSetTimeout;
    }

    const interactionPerformanceSource = [
      "Hcn=class{activeInteractions=new Map;beginCpuSampling;",
      "start(e,n,u){let d={activeKey:e,",
      "cpuSampling:u===`dropped`||n.backfilled===!0?null:this.beginCpuSampling(),",
      "name:e};return this.activeInteractions.set(e,d),this.ensureHeartbeat(),d}",
      "ensureHeartbeat(){this.heartbeatTimer??=setInterval(()=>{",
      "let e=this.now(),t=this.wallNow();",
      "for(let n of this.activeInteractions.values())",
      "this.recordHeartbeat(n,e,t)},Vcn)}",
      "recordHeartbeat(e,t,n){return [e,t,n]}};",
      "const rendererProcessCpuPercentAvg=true;",
      "function unrelated(){return beginCpuSampling()}",
    ].join("");
    electron.protocol.handle(
      "app",
      async () => new Response(interactionPerformanceSource),
    );
    const interactionPerformanceResponse = await installedHandler({
      url: "app://-/assets/app-initial-BHB6SClA.js",
    });
    const patchedInteractionPerformance =
      await interactionPerformanceResponse.text();
    assert.match(patchedInteractionPerformance, /cpuSampling:null/);
    assert.match(patchedInteractionPerformance, /ensureHeartbeat\(\)\{\}/);
    assert.doesNotMatch(
      patchedInteractionPerformance,
      /heartbeatTimer\?\?=setInterval/,
    );
    assert.doesNotMatch(
      patchedInteractionPerformance,
      /cpuSampling:[^,}]*this\.beginCpuSampling\(\)/,
    );
    assert.match(
      patchedInteractionPerformance,
      /function unrelated\(\)\{return beginCpuSampling\(\)\}/,
    );

    // Only the first (fully incompatible) bundle logged skips — two gates whose
    // anchors were present but whose shapes did not match. The interaction
    // bundle patched cleanly, so no further skips were logged.
    assert.equal(patchErrors.length, 2);

    const productionRendererAsset = process.env.CODEY_RENDERER_ASSET;
    if (productionRendererAsset) {
      const productionSource = await readFile(productionRendererAsset, "utf8");
      const previousErrorCount = patchErrors.length;
      electron.protocol.handle("app", async () => new Response(productionSource));
      const productionResponse = await installedHandler({
        url: "app://-/assets/app-initial-production-build.js",
      });
      const patchedProductionSource = await productionResponse.text();
      await new Promise((resolve) => setImmediate(resolve));
      assert.notEqual(
        patchedProductionSource,
        productionSource,
        "the production renderer asset should receive compatible Codey gates",
      );
      const currentGateFailures = patchErrors
        .slice(previousErrorCount)
        .map(([message]) => String(message))
        .filter((message) =>
          /model allowlist|model visibility|model-aware service tier control|model-aware Fast toggle|fast model trigger availability/.test(
            message,
          ),
        );
      assert.deepEqual(
        currentGateFailures,
        [],
        "the current production renderer shapes must not log known compatibility failures",
      );
    }
  } finally {
    console.error = nativeConsoleError;
    childProcess.spawn = nativeSpawn;
    childProcess.spawnSync = nativeSpawnSync;
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});
