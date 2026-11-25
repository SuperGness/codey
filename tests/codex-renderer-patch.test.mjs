import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { spawnSync } from "node:child_process";

import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

// Codex 26.1120+ stores manual draft choices natively and passes them to the
// settings hook as draftModelSettings; only the initial reasoning changes.
const nativeDraftFixture = [
  "let Dfa=bn(V,e=>({isManuallyChanged:!1,modelSettings:null,serviceTier:null}));",
  "function Cfa(e){return{...e,modelSettings:null,modelSettingsPersistAsDefault:void 0}}",
  "function klo({userSavedModelString:e,userSavedReasoningEffort:t,listModelsData:n}){let r=n?.models.find(m=>m.model===e),i=r?.supportedReasoningEfforts?.map(e=>e.reasoningEffort),a=t!=null&&i!=null&&i.includes(t)?t:r?.defaultReasoningEffort;return{model:r?r.model:e,reasoningEffort:a??t??`medium`,profile:null,isLoading:!1}}",
  "function Llo(e){let t=(0,A2.c)(32),{hostId:n,managedNewThreadModelSettings:s,draftModelSettings:c,waitForModelList:l,isNewThreadDraft:d}=e,z=Cat(n),I=s?.model??null,ne=I==null?Saved().reasoningEffort:s?.modelReasoningEffort??null,ae=klo({userSavedModelString:I??Saved().model,userSavedReasoningEffort:ne,listModelsData:l&&z!=null?{...z,defaultModel:z.models[0]}:z}),me={...ae,isLoading:!1};c!=null&&(me={...me,model:c.model,reasoningEffort:c.reasoningEffort});return{modelSettings:me}}",
  "function afterLlo(){}",
].join("");

test("native draft settings start at supported medium reasoning and ordinary speed", async (t) => {
  (0, eval)(await loadStartupPatchTemplate());
  const errors = [];
  const savedLog = globalThis.__codeyPatchLog;
  globalThis.__codeyPatchLog = (record) => errors.push(JSON.parse(record));
  const draftUrl = "app://-/assets/async/164925.2d4f802116.js";
  let patched;
  try {
    patched = globalThis.__codeyRendererPatchEntry(draftUrl, nativeDraftFixture);
  } finally {
    globalThis.__codeyPatchLog = savedLog;
  }
  assert.deepEqual(errors, []);
  assert.ok(patched?.includes("/*codey-new-thread-draft-defaults*/"));
  const syntax = spawnSync(process.execPath, ["--check", "--input-type=module"], { input: patched, encoding: "utf8" });
  assert.equal(syntax.status, 0, syntax.stderr);
  const factory = patched.match(/=>\(\{isManuallyChanged:!1,modelSettings:null,serviceTier:\{value:`default`\}\}\)/)?.[0];
  assert.ok(factory, "each draft starts with ordinary speed");

  const state = {
    saved: { model: "chosen-model", reasoningEffort: "high" },
    catalog: {
      models: [
        { model: "chosen-model", defaultReasoningEffort: "high", supportedReasoningEfforts: [{ reasoningEffort: "medium" }, { reasoningEffort: "high" }] },
        { model: "high-only", defaultReasoningEffort: "high", supportedReasoningEfforts: [{ reasoningEffort: "high" }] },
      ],
    },
  };
  const start = patched.indexOf("function Llo(");
  const useSettings = Function("A2", "Cat", "Saved", `${patched.slice(start, patched.indexOf("function afterLlo"))}${patched.slice(patched.indexOf("function klo("), patched.indexOf("function Llo("))}return Llo`)(
    { c: (size) => Array(size) },
    () => state.catalog,
    () => state.saved,
  );
  const effort = (options) => useSettings({ hostId: "local", waitForModelList: true, ...options }).modelSettings.reasoningEffort;

  await t.test("a new draft ignores the saved reasoning", () => {
    assert.equal(effort({ isNewThreadDraft: true }), "medium");
    assert.equal(useSettings({ hostId: "local", isNewThreadDraft: true }).modelSettings.model, "chosen-model");
  });
  await t.test("manual draft choices, managed settings and existing threads stay native", () => {
    assert.equal(effort({ isNewThreadDraft: true, draftModelSettings: { model: "chosen-model", reasoningEffort: "high" } }), "high");
    assert.equal(effort({ isNewThreadDraft: true, managedNewThreadModelSettings: { model: "chosen-model", modelReasoningEffort: "high" } }), "high");
    assert.equal(effort({ isNewThreadDraft: false }), "high");
    assert.equal(effort({}), "high");
  });
  await t.test("unsupported medium and unknown models use the native result", () => {
    state.saved = { model: "high-only", reasoningEffort: "high" };
    assert.equal(effort({ isNewThreadDraft: true }), "high");
    state.saved = { model: "not-in-catalog", reasoningEffort: "low" };
    assert.equal(effort({ isNewThreadDraft: true }), "low");
    state.catalog = undefined;
    assert.equal(effort({ isNewThreadDraft: true }), "low");
  });

  const patch = (source) => globalThis.__codeyRendererPatchEntry(draftUrl, source) ?? source;
  assert.equal(patch(patched), patched, "reapplying is idempotent");
  globalThis.__codeyPatchLog = () => {};
  const savedError = console.error;
  console.error = () => {};
  try {
    for (const source of [
      `${nativeDraftFixture.replace("serviceTier:null", "serviceTier:void 0")}function Mlo(e){let t=(0,A2.c)(1),{managedNewThreadModelSettings:s,draftModelSettings:c,isNewThreadDraft:d}=e,x=1}`,
      nativeDraftFixture.replace("serviceTier:null", "serviceTier:void 0").replace("me={...ae,", "me={...klo({userSavedModelString:null}),...ae,"),
    ]) assert.equal(patch(source), source, "drifted anchors leave the asset untouched");
  } finally {
    globalThis.__codeyPatchLog = savedLog;
    console.error = savedError;
  }
});

test("an incompatible optional renderer patch never blocks the Codex module response", async () => {
  const patchRecords = [];
  const patchErrors = [];
  const nativePatchLog = globalThis.__codeyPatchLog;
  const nativeConsoleError = console.error;
  globalThis.__codeyPatchLog = (record) => {
    patchRecords.push(JSON.parse(record));
  };
  console.error = (...args) => {
    patchErrors.push(args);
  };

  try {
    (0, eval)(await loadStartupPatchTemplate());
    assert.equal(globalThis.__codeyRendererPatchInstalled, true);
    const patch = (url, source) =>
      globalThis.__codeyRendererPatchEntry(url, source) ?? source;

    const incompatibleSource = [
      "supportsIndependentSpeedModes",
      "ultrafast_mode",
      "case`electron-app-state-snapshot-request`:",
    ].join(" ");
    const response = patch(
      "app://-/assets/async/49792.d8d33ace73.js",
      incompatibleSource,
    );
    assert.equal(response, incompatibleSource);
    assert.ok(patchErrors.length >= 1);
    for (const [message] of patchErrors) {
      assert.match(String(message), /incompatible Codex renderer patch/);
    }
    assert.deepEqual(
      patchRecords.map(({ operation }) => operation),
      [
        "renderer_patch:service tier access",
        "renderer_patch:app state heartbeat snapshot",
      ],
    );
    for (const record of patchRecords) {
      assert.equal(record.context.matchCount, 0);
      assert.ok(
        Array.isArray(record.context.excerpts) &&
          record.context.excerpts.length > 0 &&
          record.context.excerpts.every((excerpt) => excerpt.length > 0),
        `${record.operation} must carry anchor excerpts for field diagnosis`,
      );
      assert.match(
        record.context.excerpts.join("\n"),
        /supportsIndependentSpeedModes|electron-app-state-snapshot-request/,
      );
    }

    const repeatedResponse = patch(
      "app://-/assets/async/49792.d8d33ace73.js",
      incompatibleSource,
    );
    assert.equal(repeatedResponse, incompatibleSource);
    assert.equal(patchErrors.length, 2, "the same incompatible source must not rerun failed renderer gates");
    assert.equal(patchRecords.length, 2, "the same incompatible source must not report the same gates again");

    const routeBridgeSource = [
      "const routeLog={warning(){}};",
      "const routeTransport={postMessage:e=>{let t=!1,n=window.electronBridge;",
      "n?.sendMessageFromView&&(n.sendMessageFromView(e).catch(t=>{",
      "e.type!==`log-message`&&routeLog.warning(`Failed to send message from view`,{message:e,error:t})",
      "}),t=!0);",
      "let r=new CustomEvent(`codex-message-from-view`,{detail:e});",
      "t&&(r.__codexForwardedViaBridge=!0),window.dispatchEvent(r)}};",
    ].join("");
    const patchedRouteBridgeSource = patch(
      "app://-/assets/348410.f0081c7d18.js",
      routeBridgeSource,
    );
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
    let releaseDirect;
    routeGlobal.__codeyModelWhitelistPatch.guardDirectBridgeMessage = (_detail, send) => new Promise((resolve) => { releaseDirect = () => resolve(send()); });
    routeTransport.postMessage({ type: "mcp-request", request: { id: "direct" } });
    assert.equal(sentMessages.length, 1);
    releaseDirect();
    await new Promise(setImmediate);
    assert.equal(sentMessages.length, 2);
    assert.equal(sentMessages[1].request.id, "direct");
    assert.equal(patchErrors.length, 2, "the current Codex bridge preflight must patch without compatibility errors");

    const appServerRequestSource = [
      "class AppServerRequestClient{",
      "constructor(){this.hostId=`local`;this.sent=[];this.queuedRequests=[];this.requestPromises=new Map();this.useHostRequestScheduler=!1;this.dispatchMessage=(e,t)=>this.sent.push({type:e,payload:t})}",
      "createRequest(e,t,n,r=null){return{request:{id:`req-1`,method:e,params:t},promise:Promise.resolve({ok:!0})}}",
      "startRequest(){} onError(){} emitRequestLifecycleEvent(){} pumpQueue(){let e=this.queuedRequests.shift();e?.dispatch()}",
      "onResult(e,t,n){let r=this.requestPromises.get(e);if(r){this.requestPromises.delete(e),r.resolve(t),this.emitRequestLifecycleEvent({type:`completed`,hostId:this.hostId,method:r.method});return}}",
      "enqueueRequest(e,t,n,r=t=>{this.dispatchMessage?.(`mcp-request`,{request:t,hostId:this.hostId,...t.trace==null?{}:{dispatchedAtMs:Date.now()},priority:Mjt(e,n),source:Ejt(e,n?.source),timeoutMs:n?.timeoutMs,expiresAtMs:n?.timeoutMs!=null&&n.timeoutMs>0?Date.now()+n.timeoutMs:void 0,widget:n?.widget})},i=null,a){if(a?.aborted)return Promise.reject(new DOMException(`App-server request cancelled`,`AbortError`));let s=Mjt(e,n),o=Ejt(e,n?.source);let{request:u,promise:c}=this.createRequest(e,t,n,i);return this.queuedRequests.push({dispatch:()=>{this.startRequest(u);try{r(u)}catch(e){this.onError(u.id,e)}},priority:s}),this.pumpQueue(),c}",
      "async sendRequest(e,t,n){return this.enqueueRequest(e,t,n)}",
      "}",
      "function Mjt(){return `critical`}function Ejt(){return `source`}",
      "const appServerPatchSignals=`AppServerRequestClient is missing a message dispatcher mcp_request_enqueued`;",
    ].join("");
    const patchedAppServerRequestSource = patch(
      "app://-/assets/async/49792.d8d33ace73.js",
      appServerRequestSource,
    );
    assert.match(patchedAppServerRequestSource, /__codeyModelWhitelistPatch\?\.rewriteOutgoingMessage/);
    const blockedAppServerMessages = [];
    const routedAppServerTypes = [];
    const trackedAppServerMessages = [];
    const appServerGlobal = {
      __codeyModelWhitelistPatch: {
        rewriteOutgoingMessage(detail) {
          if (detail.hostId !== "local") return detail;
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
        rewriteIncomingResult(method, result, hostId) {
          if (hostId !== "local") return result;
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
    assert.equal(appServerGlobal.__codeyModelRequestSourceGateInstalled, true);
    const requestClient = new AppServerRequestClient();
    await requestClient.enqueueRequest("thread/start", {
      model: "route-mt6lv4lx-i2bfax/gpt-5.5",
      model_provider: "openai",
    }, {});
    assert.equal(appServerGlobal.__codeyAppServerRequestClients.get("local"), requestClient);
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
      hostId: "local",
      request: requestClient.sent[0].payload.request,
    });
    let releasePortable;
    let reentering = false;
    appServerGlobal.__codeyModelWhitelistPatch.guardOutgoingRequest = (_client, method, _params, proceed) => {
      if (method !== "turn/start" || reentering) return null;
      return new Promise((resolve) => { releasePortable = resolve; }).then(() => {
        reentering = true;
        try { return proceed(); } finally { reentering = false; }
      });
    };
    const portableSend = requestClient.enqueueRequest("turn/start", { threadId: "portable", model: "route/model" }, {});
    assert.equal(requestClient.sent.length, 2, "source gate waits before rewrite or transport");
    releasePortable();
    await portableSend;
    assert.equal(requestClient.sent.length, 3, "source gate dispatches exactly once after preparation");
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
    const remoteClient = new AppServerRequestClient();
    remoteClient.hostId = "remote-host";
    await remoteClient.enqueueRequest("thread/start", { model: "remote-model" }, {});
    assert.deepEqual(remoteClient.sent[0].payload.request.params, { model: "remote-model" });
    assert.equal(trackedAppServerMessages.at(-1).hostId, "remote-host");
    let remoteResult;
    remoteClient.requestPromises.set("remote-list", { method: "model/list", resolve(value) { remoteResult = value; } });
    remoteClient.onResult("remote-list", { data: [{ model: "remote-model" }] });
    assert.deepEqual(remoteResult, { data: [{ model: "remote-model" }] });
    assert.equal(patchErrors.length, 2, "the AppServerRequestClient route preflight must patch without compatibility errors");

    // Assets outside the numeric chunk set must pass through untouched.
    const unrelatedChunk = patch(
      "app://-/webview/assets/unrelated-feature-6f60e9ac4103.js",
      appServerRequestSource,
    );
    assert.equal(unrelatedChunk, appServerRequestSource);

    const localePropsSource = [
      "function provider(E){return E.localeOverride}",
      "function resolveProps(a,o,r,H3){const dynamicConfigId=`72216192`;",
      "let s=o?.get(`enable_i18n`,!1);",
      "let c=s,l=o?.get(`locale_source`,`IDE`),u=a?.ideLocale,d=a?.systemLocale,f=H3(r),p;",
      "bb0:{if(r){p=r;break bb0}if(l===`SYSTEM`){p=d;break bb0}p=u}",
      "return {enabled:c,source:l,locale:p,english:f}}",
    ].join("");
    const localePropsResult = patch(
      "app://-/assets/async/49792.d8d33ace73.js",
      localePropsSource,
    );
    assert.equal(localePropsResult, localePropsSource);

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
      "function unrelated(){return beginCpuSampling()}",
    ].join("");
    const patchedInteractionPerformance = patch(
      "app://-/assets/async/366996.9782070574.js",
      interactionPerformanceSource,
    );
    assert.match(patchedInteractionPerformance, /cpuSampling:null/);
    assert.match(patchedInteractionPerformance, /ensureHeartbeat\(\)\{\}/);
    assert.doesNotMatch(patchedInteractionPerformance, /heartbeatTimer\?\?=setInterval/);
    assert.doesNotMatch(patchedInteractionPerformance, /cpuSampling:[^,}]*this\.beginCpuSampling\(\)/);
    assert.match(patchedInteractionPerformance, /function unrelated\(\)\{return beginCpuSampling\(\)\}/);

    const hostServicesSnapshotSource = [
      "async function onHostMessage(e,t,tt){switch(e.type){",
      "case`electron-app-state-snapshot-request`:{let n=t.get(`host`,e.hostId),r=t.get(`sampler`),i=null;",
      "if(n!=null&&r!=null){let[a]=await Promise.all([r.collectAppStateSnapshot(e.hostId,e.reason)]);i=a}",
      "let a=t.get(`route`);if(i==null&&a==null)break;",
      "tt.dispatchMessage(`electron-app-state-snapshot-response`,{requestId:e.requestId,fields:i,workRoute:a});break}",
      "case`other`:break}}",
    ].join("");
    const onHostMessage = Function(
      `${patch("app://-/assets/async/49792.d8d33ace73.js", hostServicesSnapshotSource)};return onHostMessage`,
    )();
    const collectedReasons = [];
    const snapshotResponses = [];
    const snapshotStore = {
      get: (key) => key === "host" ? {} : {
        async collectAppStateSnapshot(_hostId, reason) {
          collectedReasons.push(reason);
          return { reason };
        },
      },
    };
    const snapshotBridge = {
      dispatchMessage: (_type, payload) => snapshotResponses.push(payload.requestId),
    };
    for (const [requestId, reason] of [["beat", "heartbeat"], ["jank", "main_thread_jank"]]) {
      await onHostMessage({
        type: "electron-app-state-snapshot-request",
        hostId: "local",
        requestId,
        reason,
      }, snapshotStore, snapshotBridge);
    }
    assert.deepEqual(collectedReasons, ["main_thread_jank"]);
    assert.deepEqual(snapshotResponses, ["jank"]);

    // 只有首个不兼容模块触发两项跳过，其余夹具全部按现行形态改写。
    assert.equal(patchErrors.length, 2);
    assert.equal(patchRecords.length, 2);

    const productionRendererAsset = process.env.CODEY_RENDERER_ASSET;
    if (productionRendererAsset) {
      const productionSource = await readFile(productionRendererAsset, "utf8");
      const previousErrorCount = patchErrors.length;
      const patchedProductionSource = patch(
        "app://-/assets/async/49792.d8d33ace73.js",
        productionSource,
      );
      assert.notEqual(
        patchedProductionSource,
        productionSource,
        "the production renderer asset should receive compatible Codey gates",
      );
      const currentGateFailures = patchErrors
        .slice(previousErrorCount)
        .map(([message]) => String(message))
        .filter((message) =>
          /service tier access|app state heartbeat snapshot/.test(message),
        );
      assert.deepEqual(
        currentGateFailures,
        [],
        "the current production renderer shapes must not log known compatibility failures",
      );
    }
  } finally {
    console.error = nativeConsoleError;
    if (nativePatchLog === undefined) {
      delete globalThis.__codeyPatchLog;
    } else {
      globalThis.__codeyPatchLog = nativePatchLog;
    }
  }
});

test("the hosted AppServerManager service leaves a diagnostic marker when it connects", async () => {
  delete globalThis.__codeyRendererPatchInstalled;
  (0, eval)(await loadStartupPatchTemplate());
  const url = "app://-/assets/async/49792.d8d33ace73.js";
  const source = "function im(e){let t=A.appServerManagers;if(t==null)throw Error(`Desktop host did not provide its AppServerManager service`);return tm(e,t)}";
  const patched = globalThis.__codeyRendererPatchEntry(url, source);
  const connect = (service) => Function("A", "tm", `${patched};return im`)({ appServerManagers: service }, () => "connected");
  delete globalThis.__codeyAppServerManagerHosted;
  assert.throws(() => connect(null)(), /did not provide/);
  assert.equal(globalThis.__codeyAppServerManagerHosted, undefined, "a missing host service is not marked");
  assert.equal(connect({})(), "connected");
  assert.equal(globalThis.__codeyAppServerManagerHosted, true);
  delete globalThis.__codeyAppServerManagerHosted;
  assert.equal(globalThis.__codeyRendererPatchEntry(url, patched), null, "the patch is idempotent");
});

test("hosted composer sends pass through the portable handoff gate", async () => {
  delete globalThis.__codeyRendererPatchInstalled;
  (0, eval)(await loadStartupPatchTemplate());
  const followUp = "async function f(e,n,i,o){let l=d(e,n),m=null;return f=await l.sendFollowUpMessage(i,{...o,useUserMessageAdmission:c(e,n),writingBlockContextPrepared:m!=null}),f}";
  const turnStart = "async function f(e,n,r,a){let c=d(e,n);return await c.startTurn(r,{...a,context:{...a.context,onRequestDispatched:a.context?.onRequestDispatched},request:{...a.request}})}async function p(e,n,r,a){let s=d(e,n);await s.startEmptyTurn(r,a)}";
  const cases = [
    ["app://-/assets/async/49792.d8d33ace73.js", followUp, "sendFollowUpMessage", { prompt: "hi" }],
    ["app://-/assets/async/102734.cdd4b920b9.js", turnStart, "startTurn", { request: { input: [] } }],
  ];
  for (const [url, source, method, options] of cases) {
    const patched = globalThis.__codeyRendererPatchEntry(url, source);
    assert.ok(patched?.includes(`"${method}",`), method);
    assert.equal(globalThis.__codeyRendererPatchEntry(url, patched), null, `${method} patch is idempotent`);
    const calls = [];
    const rpc = { [method]: async (...args) => { calls.push(["native", ...args]); return "sent"; } };
    const run = Function("d", "c", `${patched};return f`)(() => rpc, () => true);
    delete globalThis.__codeyModelWhitelistPatch;
    assert.equal(await run({}, "local", "thread", options), "sent", `${method} works without the gate`);
    globalThis.__codeyModelWhitelistPatch = { hostedTurnCall: async (target, name, threadId, value) => {
      calls.push(["gate", name, threadId]);
      return target[name](threadId, value);
    } };
    try {
      assert.equal(await run({}, "local", "thread", options), "sent");
    } finally {
      delete globalThis.__codeyModelWhitelistPatch;
    }
    assert.deepEqual(calls.map(([kind, name]) => kind === "gate" ? `gate:${name}` : kind), ["native", `gate:${method}`, "native"]);
  }
  const lock = "function t(e){return!r.has(e)&&(r.add(e),!0)}";
  assert.equal(globalThis.__codeyRendererPatchEntry("app://-/assets/async/999999.be4319076981.js", lock), null);
});
