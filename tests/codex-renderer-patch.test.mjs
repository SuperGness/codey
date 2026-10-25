import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { spawnSync } from "node:child_process";

import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

// Keep the native draft merge and persistence branches in the fixture. The
// optional extracted production asset runs through the same behavior harness.
const newDraftFixture = [
  "let Hva=wo(U,e=>({isManuallyChanged:!1,modelSettings:null,serviceTier:null}));",
  "function Jbo({userSavedModelString:e,userSavedReasoningEffort:t,listModelsData:n}){let r=n?.models.find(m=>m.model===e),i=r?.supportedReasoningEfforts?.map(e=>e.reasoningEffort),a=t!=null&&i!=null&&i.includes(t)?t:r?.defaultReasoningEffort;return{model:r?r.model:e,reasoningEffort:a??t??`medium`,profile:null,isLoading:!1}}",
  "function nxo(e){let{managedNewThreadModelSettings:s,waitForModelList:c}=e;let{data:N,isLoading:P}=Za(M);let{data:R,isLoading:z}=wvo(L),B=Xbo(T?.settings,R),ne=Jbo({userSavedModelString:V,userSavedReasoningEffort:H,listModelsData:R});return ne}",
  "function uxo(e,t){let n=(0,fxo.c)(29),r=e===void 0?null:e,d={enabled:r==null};let{draftSettings:f,isNewThreadDraft:p,updateDraftSettings:m}=Lva(d),h=sxo(r,p),{isLunaReserveActive:g}=h,_=r==null&&p,v=_&&h.hasManagedNewThreadSettings,y=_&&h.isUsingCopilotApi,b=_&&f.modelSettings!=null,x=h.modelSettings;if(b){x={...x,...f.modelSettings}}if(g)x={...x,model:YNt};let C=async(e,t,n)=>{let i=e,a=_&&n?.persistAsDefault===!1;if(a||v||y&&t===`ultra`?m(e=>({...e,isManuallyChanged:!0,modelSettings:{model:i,profile:h.modelSettings.profile,reasoningEffort:t},modelSettingsPersistAsDefault:!a,serviceTier:n?.serviceTier===void 0?e.serviceTier:{value:xir(n.serviceTier)}})):_&&f.modelSettings!=null&&m(dxo),a)return!0;return h.setModelAndReasoningEffort(i,t,n)};return{...h,modelSettings:x,setModelAndReasoningEffort:C}}function dxo(e){return{...e,modelSettings:null,modelSettingsPersistAsDefault:void 0}}",
  "function vwo(e,t){let r=e===void 0?null:e,i=t===void 0?null:t,a=hi(U),{draftSettings:o,isNewThreadDraft:s,updateDraftSettings:c}=Lva(),l=_wo(r,uxo(r),o.serviceTier,s,i),{serviceTierAccess:d,...u}=l;return{...u,setServiceTier:async(e,t)=>{if(!We(e,d))return!1;let n=r==null&&s&&(u.hasManagedNewThreadSettings||i!=null||o.serviceTier!=null);n&&c(t=>({...t,isManuallyChanged:!0,serviceTier:{value:xir(e)}}));let[l]=await Promise.all([u.setServiceTier(e,t),n?Vh(a,u.hostId).clearPrewarmedThreads():void 0]);return l}}}function afterSpeed(){}",
].join("");

function exerciseDraftHooks(source) {
  const takeFunction = (name) => {
    const start = source.indexOf(`function ${name}(`);
    assert.ok(start >= 0, `${name} exists`);
    const end = Math.min(...[source.indexOf("}function ", start), source.indexOf("}var ", start)].filter((index) => index >= 0));
    assert.ok(end > start, `${name} has a bounded next declaration`);
    return source.slice(start, end + 1);
  };
  const factory = source.match(/=>\(\{isManuallyChanged:!1,modelSettings:null,serviceTier:\{value:`default`\}\}\)/)?.[0];
  assert.ok(factory, "each draft has its own ordinary-speed initializer");
  const makeDraft = Function(`return e${factory}`)();
  const state = {
    draft: makeDraft(),
    isDraft: true,
    cache: [],
    speedCache: [],
    savedSpeed: "fast",
    existingSpeed: "fast",
    catalog: {
      models: [
        { model: "chosen-model", defaultReasoningEffort: "high", supportedReasoningEfforts: [{ reasoningEffort: "medium" }, { reasoningEffort: "high" }] },
        { model: "high-only", defaultReasoningEffort: "high", supportedReasoningEfforts: [{ reasoningEffort: "high" }] },
      ],
    },
    settings: { model: "chosen-model", reasoningEffort: "high", profile: "saved-profile", isLoading: false },
    writes: [],
    hooks: 0,
  };
  const scope = { set() {}, get() {} };
  const base = {
    hostId: "local",
    hasManagedNewThreadSettings: false,
    isUsingCopilotApi: false,
    isLunaReserveActive: false,
    setModelAndReasoningEffort: async (...args) => { state.writes.push(args); return true; },
    setModelAndReasoningEffortForNextTurn: async () => true,
  };
  const bindings = {
    fxo: { c: (size) => {
      if (state.cache.length === 0) state.cache = Array(size).fill(Symbol.for("react.memo_cache_sentinel"));
      return state.cache;
    } },
    ywo: { c: (size) => {
      if (state.speedCache.length === 0) state.speedCache = Array(size).fill(Symbol.for("react.memo_cache_sentinel"));
      return state.speedCache;
    } },
    Lva: () => ({ draftSettings: state.draft, isNewThreadDraft: state.isDraft, updateDraftSettings: (update) => { state.draft = update(state.draft); } }),
    sxo: () => ({ ...base, modelSettings: state.settings }),
    wvo: () => { state.hooks += 1; return { data: state.catalog }; },
    hi: () => scope,
    U: {},
    J_o() {},
    IJ: {},
    Vh: () => ({ clearPrewarmedThreads: async () => {} }),
    xir: (tier) => tier ?? "default",
    qbo: (models, model) => models?.find((entry) => entry.model === model),
    YNt: "gpt-reserve",
    We: () => true,
    _wo: (threadId, _model, draftTier) => ({
      hostId: "local",
      hasManagedNewThreadSettings: false,
      serviceTierAccess: {},
      // Native speed resolver gives existing-thread settings precedence, then
      // the draft value, then saved global preferences.
      serviceTierSettings: { selectedServiceTier: threadId != null ? state.existingSpeed : draftTier?.value ?? state.savedSpeed },
      setServiceTier: async (tier) => { state.savedSpeed = tier; return true; },
    }),
  };
  const { useModel, useSpeed } = Function(...Object.keys(bindings), `${takeFunction("Jbo")}${takeFunction("uxo")}${takeFunction("vwo")}function dxo(e){return{...e,modelSettings:null,modelSettingsPersistAsDefault:void 0}}return{useModel:uxo,useSpeed:vwo}`)(...Object.values(bindings));
  return { state, makeDraft, useModel, useSpeed };
}

test("new drafts initialize ordinary speed and supported medium reasoning without replacing manual choices", async (t) => {
  (0, eval)(await loadStartupPatchTemplate());
  const sources = [["fixture", newDraftFixture]];
  if (process.env.CODEY_NEW_CHAT_RENDERER_ASSET) {
    sources.push(["installed renderer", await readFile(process.env.CODEY_NEW_CHAT_RENDERER_ASSET, "utf8")]);
  }
  for (const [label, source] of sources) {
    await t.test(label, async (t) => {
      const errors = [];
      const savedLog = globalThis.__codeyPatchLog;
      const savedError = console.error;
      let patched;
      globalThis.__codeyPatchLog = (record) => errors.push(JSON.parse(record));
      console.error = () => {};
      try {
        patched = globalThis.__codeyRendererPatchEntry("app://-/assets/app-initial-new-drafts.js", source) ?? source;
      } finally {
        globalThis.__codeyPatchLog = savedLog;
        console.error = savedError;
      }
      assert.ok(patched.includes("/*codey-new-thread-draft-defaults*/"));
      assert.deepEqual(errors.filter(({ operation }) => operation === "renderer_patch:new thread draft defaults"), []);
      const syntax = spawnSync(process.execPath, ["--check", "--input-type=module"], { input: patched, encoding: "utf8" });
      assert.equal(syntax.status, 0, syntax.stderr);
      const { state, makeDraft, useModel, useSpeed } = exerciseDraftHooks(patched);
      await t.test("two new drafts reset independently and keep the chosen default model", async () => {
        const first = useModel();
        assert.equal(first.modelSettings.reasoningEffort, "medium");
        assert.equal(first.modelSettings.model, "chosen-model");
        assert.equal(first.modelSettings.profile, "saved-profile");
        assert.equal(state.draft.serviceTier.value, "default");
        assert.equal(useModel().modelSettings, first.modelSettings, "default object stays memoized");
        await first.setModelAndReasoningEffort("chosen-model", "high", { serviceTier: "fast" });
        assert.equal(useModel().modelSettings.reasoningEffort, "high");
        assert.equal(state.draft.serviceTier.value, "fast");
        assert.equal(state.writes.length, 1, "native default-model persistence is preserved");
        const previousDraft = state.draft;
        state.draft = makeDraft();
        state.cache = [];
        assert.equal(useModel().modelSettings.reasoningEffort, "medium");
        assert.equal(state.draft.serviceTier.value, "default");
        assert.equal(previousDraft.serviceTier.value, "fast");
      });
      await t.test("independent speed choices remain local to the current draft", async () => {
        assert.equal(useSpeed().serviceTierSettings.selectedServiceTier, "default");
        await useSpeed().setServiceTier("fast", "picker");
        assert.equal(useSpeed().serviceTierSettings.selectedServiceTier, "fast");
        assert.equal(useModel().modelSettings.reasoningEffort, "medium", "a speed change does not restore old reasoning");
        state.draft = makeDraft();
        assert.equal(useSpeed().serviceTierSettings.selectedServiceTier, "default");
        assert.equal(useSpeed("existing-thread").serviceTierSettings.selectedServiceTier, "fast");
      });
      await t.test("unsupported medium uses the native model fallback", () => {
        state.settings = { ...state.settings, model: "high-only" };
        assert.equal(useModel().modelSettings.reasoningEffort, "high");
      });
      await t.test("missing or loading catalogs retain valid settings and keep hook order", () => {
        state.settings = { ...state.settings, model: "not-in-catalog", reasoningEffort: "low" };
        assert.equal(useModel().modelSettings, state.settings);
        state.catalog = undefined;
        const before = state.hooks;
        assert.equal(useModel().modelSettings, state.settings);
        assert.equal(state.hooks, before + 1);
      });
      await t.test("existing and restoring threads retain reasoning and ignore draft speed", async () => {
        state.isDraft = false;
        state.settings = { ...state.settings, reasoningEffort: "high", isLoading: true };
        assert.equal(useModel("existing-thread").modelSettings, state.settings);
        const previous = state.draft;
        await useModel("existing-thread").setModelAndReasoningEffort("high-only", "high");
        assert.equal(state.draft, previous);
      });
    });
  }
});

test("draft default patch is atomic when anchors drift and idempotent when reapplied", async () => {
  (0, eval)(await loadStartupPatchTemplate());
  const patch = (source) => globalThis.__codeyRendererPatchEntry("app://-/assets/app-initial-new-drafts.js", source) ?? source;
  const patched = patch(newDraftFixture);
  assert.equal(patch(patched), patched);
  const savedLog = globalThis.__codeyPatchLog;
  const savedError = console.error;
  globalThis.__codeyPatchLog = () => {};
  console.error = () => {};
  try {
    for (const source of [
      newDraftFixture.replace("serviceTier:null", "serviceTier:void 0"),
      newDraftFixture.replace("waitForModelList:c", "fetchModels:c"),
      newDraftFixture.replace("a||v||y&&t===`ultra`?", "a||v?"),
      `${newDraftFixture}let duplicate=wo(U,e=>({isManuallyChanged:!1,modelSettings:null,serviceTier:null}));`,
    ]) assert.equal(patch(source), source);
  } finally {
    globalThis.__codeyPatchLog = savedLog;
    console.error = savedError;
  }
});

// Codex 26.1021+ stores manual draft choices natively and passes them to the
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
  let patched;
  try {
    patched = globalThis.__codeyRendererPatchEntry("app://-/assets/app-initial-native-drafts.js", nativeDraftFixture);
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

  const patch = (source) => globalThis.__codeyRendererPatchEntry("app://-/assets/app-initial-native-drafts.js", source) ?? source;
  assert.equal(patch(patched), patched, "reapplying is idempotent");
  globalThis.__codeyPatchLog = () => {};
  const savedError = console.error;
  console.error = () => {};
  try {
    for (const source of [
      nativeDraftFixture.replace("serviceTier:null", "serviceTier:void 0"),
      `${nativeDraftFixture}function Mlo(e){let t=(0,A2.c)(1),{managedNewThreadModelSettings:s,draftModelSettings:c,isNewThreadDraft:d}=e,x=1}`,
      nativeDraftFixture.replace("me={...ae,", "me={...klo({userSavedModelString:null}),...ae,"),
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
    (0, eval)(await loadStartupPatchTemplate({ disablePet: true }));
    assert.equal(globalThis.__codeyRendererPatchInstalled, true);
    // Fetch interception entry: patched text means the response is rewritten,
    // null means the paused asset is fulfilled with its original body.
    const patch = (url, source) =>
      globalThis.__codeyRendererPatchEntry(url, source) ?? source;

    const incompatibleSource = [
      "supportsIndependentSpeedModes",
      "ultrafast_mode",
      "case`electron-app-state-snapshot-request`:",
    ].join(" ");
    const response = patch(
      "app://-/assets/app-initial-new-codex-build.js",
      incompatibleSource,
    );
    // Each incompatible gate is skipped independently (and logged) instead of one
    // throw discarding every gate on the asset. The response is never blocked and
    // the source is returned unchanged when nothing matched.
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
    // Every skipped gate must carry printable excerpts around its neighborhood
    // anchors so an incompatible field bundle can be adapted without access to
    // that exact build.
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
      "app://-/assets/app-initial-new-codex-build.js",
      incompatibleSource,
    );
    assert.equal(repeatedResponse, incompatibleSource);
    assert.equal(
      patchErrors.length,
      2,
      "the same incompatible source must not rerun failed renderer gates",
    );
    assert.equal(
      patchRecords.length,
      2,
      "the same incompatible source must not report the same gates again",
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
    const patchedRouteBridgeSource = patch(
      "app://-/assets/app-initial-route-bridge.js",
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
    const patchedAppServerRequestSource = patch(
      "app://-/assets/app-initial-app-server-request-current-build.js",
      appServerRequestSource,
    );
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
    assert.equal(
      appServerGlobal.__codeyAppServerRequestClients.get("local"),
      requestClient,
    );
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
    assert.equal(
      patchErrors.length,
      2,
      "the AppServerRequestClient route preflight must patch without compatibility errors",
    );

    // Codex 26.1007 moved AppServerRequestClient and the Electron bridge
    // forwarder into app-shared-*.js. That chunk must stay in the patch filter
    // or the model-request source gate never installs and startup aborts at
    // the request-gate verification stage.
    const patchedSharedChunk = patch(
      "app://-/webview/assets/app-shared-8bd9d0399038.js",
      appServerRequestSource,
    );
    assert.match(
      patchedSharedChunk,
      /__codeyModelRequestSourceGateInstalled=true/,
    );
    assert.match(
      patchedSharedChunk,
      /__codeyAppServerRequestClients/,
    );
    // Current Codex puts composer and request gates in app-primary too.
    assert.equal(
      patch("app://-/webview/assets/app-primary-6f60e9ac4103.js", appServerRequestSource),
      patchedSharedChunk,
    );
    // Assets outside the patch set must pass through untouched.
    const unrelatedChunk = patch(
      "app://-/webview/assets/unrelated-feature-6f60e9ac4103.js",
      appServerRequestSource,
    );
    assert.equal(unrelatedChunk, appServerRequestSource);

    const petSettingsSource = [
      "import{AvatarPreview as P,builtInPets as L}",
      "from\"./codex-avatar-BpKnWN_W.js\";",
      "const petSettingsId=`settings.appearance.pets.title`;",
      "function renderPetSettings(){return [P(),L.map(()=>1),petSettingsId]}",
    ].join("");
    const patchedPetSettingsSource = patch(
      "app://-/assets/pets-settings-route-current-build.js",
      petSettingsSource,
    );
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
    const patchedSideEffectPetSettingsSource = patch(
      "app://-/assets/pets-settings-next-build.js",
      sideEffectPetSettingsSource,
    );
    assert.doesNotMatch(patchedSideEffectPetSettingsSource, /codex-avatar-/);
    assert.match(
      patchedSideEffectPetSettingsSource,
      /const petSettingsId=`settings\.pets\.title`/,
    );

    // Current Codex builds split the pets/Mini preview across three chunks and
    // drop the semicolon after each import, so every one of them has to be
    // stubbed from the same settings route chunk.
    const currentPetSettingsSource = [
      "import{f as Rt,m as zt,r as Bt,t as Vt}from\"./avatar-mascot-button-7586001b9adf.js\"",
      "import{n as Ht,t as Ut}from\"./codex-pet-assets-5f01c10f2955.js\"",
      "import{n as Yt,t as Xt}from\"./use-avatar-options-fda035de15fb.js\"",
      "const miniSettingsId=`settings.mini.show`;",
      "function renderMiniSettings(){return [Rt(),Ht(),Yt(),miniSettingsId]}",
    ].join("\n");
    const patchedCurrentPetSettingsSource = patch(
      "app://-/assets/pets-settings-route-551771c3ecb9.js",
      currentPetSettingsSource,
    );
    assert.doesNotMatch(
      patchedCurrentPetSettingsSource,
      /(avatar-mascot-button|codex-pet-assets|use-avatar-options)-/,
    );
    const renderMiniSettings = Function(
      `${patchedCurrentPetSettingsSource};return renderMiniSettings`,
    )();
    assert.deepEqual(renderMiniSettings(), [
      null,
      null,
      null,
      "settings.mini.show",
    ]);

    const localePropsSource = [
      "function provider(E){return E.localeOverride}",
      "function resolveProps(a,o,r,H3){const dynamicConfigId=`72216192`;",
      "let s=o?.get(`enable_i18n`,!1);",
      "let c=s,l=o?.get(`locale_source`,`IDE`),u=a?.ideLocale,d=a?.systemLocale,f=H3(r),p;",
      "bb0:{if(r){p=r;break bb0}if(l===`SYSTEM`){p=d;break bb0}p=u}",
      "return {enabled:c,source:l,locale:p,english:f}}",
    ].join("");
    // Codex owns the UI language, so its locale resolver must reach the renderer unchanged.
    const localePropsResult = patch(
      "app://-/assets/app-initial-26903.js",
      localePropsSource,
    );
    assert.equal(localePropsResult, localePropsSource);

    // Codex 26.1013 shape: the in-progress predicate is a module function used
    // by getHasInProgressLocalConversation; resume runs through the public
    // resumeConversation entry.
    const completedReconciliationSource = [
      "function isRunning(e){return e.inProgress===!0}",
      "class RunningManager{",
      "constructor(){this.conversation={inProgress:!0,resumeState:`resumed`,cwd:`/repo`};",
      "this.threadStore={getConversation:()=>this.conversation,getRecentConversationIds:()=>[`thread`]};",
      "this.role={role:`owner`};this.revision=1;this.statuses=[{type:`idle`},{type:`idle`}];this.events=[];",
      "this.inactiveThreadUnsubscriber={clearConversationStreamOwnership:e=>{this.events.push(`clear:${e}`);this.role=null}}}",
      "get conversations(){return new Map([[`thread`,this.conversation]])}",
      "getConversation(e){return this.threadStore.getConversation(e)}",
      "getConversationStreamRevision(){return this.revision}",
      "getStreamRole(){return this.role}",
      "async sendRequest(e,t){this.events.push(`read:${e}:${t.threadId}:${t.includeTurns}`);return{thread:{status:this.statuses.shift()??{type:`idle`}}}}",
      "updateConversationState(e,t,n){this.events.push(`state:${e}:${n}`);t(this.conversation)}",
      "async resumeConversation(e){this.events.push(`resume:${e.conversationId}:${e.workspaceRoots.join()}`);this.conversation.inProgress=!1;this.role={role:`owner`};return{status:`ready`}}",
      "getHasInProgressLocalConversation({exceptConversationId:e}={}){for(let t of this.threadStore.getRecentConversationIds()){if(t===e)continue;let n=this.conversations.get(t);if(n!=null&&isRunning(n))return!0}return!1}",
      "releaseOwnedThread(e){this.inactiveThreadUnsubscriber.clearConversationStreamOwnership(e)}",
      "}",
    ].join("");
    const patchedCompletedReconciliationSource = patch(
      "app://-/assets/app-initial-BHB6SClA.js",
      completedReconciliationSource,
    );
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
      assert.equal(completedManager.getHasInProgressLocalConversation(), true);
      assert.equal(
        await completedManager.codeyReconcileCompletedConversation({
          conversationId: "thread-completed",
          workspaceRoots: [],
        }),
        true,
      );
      assert.deepEqual(completedManager.events, [
        "read:thread/read:thread-completed:false",
        "read:thread/read:thread-completed:false",
        "clear:thread-completed",
        "state:thread-completed:false",
        "resume:thread-completed:/repo",
      ]);
      assert.deepEqual(reconcileDelays, [250]);
      assert.equal(completedManager.conversation.resumeState, "needs_resume");
      assert.deepEqual(completedManager.role, { role: "owner" });

      const activeManager = new RunningManager();
      activeManager.statuses = [{ type: "active" }];
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

      // A submitted turn the server has not confirmed yet is not stale.
      const submittingManager = new RunningManager();
      submittingManager.conversation.unconfirmedTurnSubmissions = [{ terminal: false }];
      assert.equal(
        await submittingManager.codeyReconcileCompletedConversation({
          conversationId: "thread-submitting",
        }),
        false,
      );
      assert.deepEqual(submittingManager.events, []);

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
      "function unrelated(){return beginCpuSampling()}",
    ].join("");
    const patchedInteractionPerformance = patch(
      "app://-/assets/app-initial-BHB6SClA.js",
      interactionPerformanceSource,
    );
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

    const appStateSnapshotSource = [
      "async function onMessage(e,r,hs){bb467:switch(e.type){",
      "case`electron-app-state-snapshot-request`:{let t=r.get(`host`,e.hostId);if(t==null)break bb467;",
      "let n=r.get(`sampler`),i=await n?.collectAppStateSnapshot(e.hostId,e.reason);",
      "if(i==null)break bb467;",
      "hs.dispatchMessage(`electron-app-state-snapshot-response`,{requestId:e.requestId,fields:i});break bb467}",
      "case`other`:break bb467}}",
    ].join("");
    const onAppMessage = Function(
      `${patch("app://-/assets/app-initial-app-state.js", appStateSnapshotSource)};return onMessage`,
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
      await onAppMessage({
        type: "electron-app-state-snapshot-request",
        hostId: "local",
        requestId,
        reason,
      }, snapshotStore, snapshotBridge);
    }
    assert.deepEqual(collectedReasons, ["main_thread_jank"]);
    assert.deepEqual(snapshotResponses, ["jank"]);

    // Codex 26.1021 moved the handler into the host services chunk and uses an
    // unlabelled switch break; a skipped heartbeat must not send workRoute.
    const hostServicesSnapshotSource = [
      "async function onHostMessage(e,t,tt){switch(e.type){",
      "case`electron-app-state-snapshot-request`:{let n=t.get(`host`,e.hostId),r=t.get(`sampler`),i=null;",
      "if(n!=null&&r!=null){let[a]=await Promise.all([r.collectAppStateSnapshot(e.hostId,e.reason)]);i=a}",
      "let a=t.get(`route`);if(i==null&&a==null)break;",
      "tt.dispatchMessage(`electron-app-state-snapshot-response`,{requestId:e.requestId,fields:i,workRoute:a});break}",
      "case`other`:break}}",
    ].join("");
    const onHostMessage = Function(
      `${patch("app://-/assets/initialize-app-host-services-5891c906a171.js", hostServicesSnapshotSource)};return onHostMessage`,
    )();
    collectedReasons.length = 0;
    snapshotResponses.length = 0;
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
        "app://-/assets/app-initial-production-build.js",
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
          /service tier access|app state heartbeat snapshot|completed thread reconciliation|app server manager registry/.test(
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
    if (nativePatchLog === undefined) {
      delete globalThis.__codeyPatchLog;
    } else {
      globalThis.__codeyPatchLog = nativePatchLog;
    }
  }
});
