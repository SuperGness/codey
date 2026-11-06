import assert from "node:assert/strict";
import test from "node:test";

import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

// Codex 26.1029 switched to rspack: every asset is a numeric chunk, string
// literals use double quotes, comparisons put the literal first and imports are
// called as `(0,module.export)(...)`. Fixtures mirror those shapes.
const chunkUrl = "app://-/assets/async/24191.589dcc9878.js";

async function installPatch({ disablePet = false } = {}) {
  delete globalThis.__codeyRendererPatchInstalled;
  (0, eval)(await loadStartupPatchTemplate({ disablePet }));
  const logs = [];
  globalThis.__codeyPatchLog = (record) => logs.push(JSON.parse(record));
  return {
    logs,
    patch(source, url = chunkUrl) {
      return globalThis.__codeyRendererPatchEntry(url, source);
    },
  };
}

test.afterEach(() => {
  delete globalThis.__codeyPatchLog;
  delete globalThis.__codeyModelWhitelistPatch;
});

test("rspack view bridge sends through the model route preflight", async () => {
  const { logs, patch } = await installPatch();
  const source = 'const k="codex-message-from-view";function send(e,r,n){r?.sendMessageFromView&&(r.sendMessageFromView(e).catch(t=>{"log-message"!==e.type&&n.push(["Failed to send message from view",e,t])}))}';
  const patched = patch(source);
  assert.ok(patched, "the short-circuit bridge send is rewritten");
  assert.equal(patch(patched), null, "the patch is idempotent");
  const send = Function(`${patched};return send`)();

  const sent = [];
  const bridge = { sendMessageFromView: async (message) => { sent.push(message); } };
  globalThis.__codeyModelWhitelistPatch = {
    rewriteOutgoingMessage: (message) => ({ ...message, routed: true }),
    isBlockedOutgoingMessage: (message) => message.type === "blocked",
    notifyBlockedOutgoingMessage: () => sent.push("blocked-notice"),
    guardDirectBridgeMessage: () => null,
  };
  send({ type: "turn" }, bridge, []);
  send({ type: "blocked" }, bridge, []);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(sent, [{ type: "turn", routed: true }, "blocked-notice"]);

  const failures = [];
  send({ type: "turn" }, { sendMessageFromView: async () => { throw Error("transport"); } }, failures);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(failures.length, 1, "transport failures keep the native catch");
  send({ type: "turn" }, undefined, failures);
  assert.deepEqual(logs, []);
});

test("rspack request client keeps route, identity and model result gates in one chunk", async () => {
  const { logs, patch } = await installPatch();
  const source = [
    'class C{async sendRequest(e,t,n){if(null==this.dispatchMessage)throw Error("AppServerRequestClient is missing a message dispatcher");return this.enqueueRequest(e,t,n)}',
    'enqueueRequest(e,t,n,r=t=>{this.dispatchMessage?.("mcp-request",{request:t,hostId:this.hostId,retainResponse:void 0,source:n?.source,timeoutMs:n?.timeoutMs,widget:n?.widget})},i=null){let a=1;log("mcp_request_enqueued");let{request:o,promise:l}=this.createRequest(e,t,n,i);r(o);return l}',
    'onResult(e,t){let r=this.requestPromises.get(e);r.resolve(t),this.emitRequestLifecycleEvent({type:"completed",hostId:this.hostId,method:r.method,id:e})}}',
  ].join("");
  const patched = patch(source);
  for (const marker of [
    "let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage",
    "globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage",
    "globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult",
    "globalThis.__codeyModelRequestSourceGateInstalled=true",
  ]) assert.ok(patched.includes(marker), marker);
  Function(patched);
  assert.deepEqual(logs, []);
});

test("rspack hosted manager marker and completed thread reconciliation follow reversed null checks", async () => {
  const { logs, patch } = await installPatch();
  const hosted = 'function E(e){let O=v.appServices.appServerManagers;if(null==O)throw Error("Desktop host did not provide its AppServerManager service");return O.dup()}';
  const hostedPatched = patch(hosted);
  delete globalThis.__codeyAppServerManagerHosted;
  const connect = Function("v", `${hostedPatched};return E`);
  assert.throws(() => connect({ appServices: {} })(), /did not provide/);
  assert.equal(globalThis.__codeyAppServerManagerHosted, undefined);
  assert.equal(connect({ appServices: { appServerManagers: { dup: () => "dup" } } })(), "dup");
  assert.equal(globalThis.__codeyAppServerManagerHosted, true);
  delete globalThis.__codeyAppServerManagerHosted;

  const manager = [
    "class M{getHostId(){return\"local\"}getConversation(e){return this.threadStore.getConversation(e)}getStreamRole(e){}updateConversationState(e,t){}getConversationStreamRevision(e){}",
    "clear(){this.inactiveThreadUnsubscriber.clearConversationStreamOwnership(1)}",
    "getHasInProgressLocalConversation({exceptConversationId:e}={}){for(let t of this.threadStore.getRecentConversationIds()){if(t===e)continue;let n=this.conversations.get(t);if(null!=n&&el(n))return!0}return!1}}",
  ].join("");
  const managerPatched = patch(manager);
  assert.match(managerPatched, /async codeyReconcileCompletedConversation\(/);
  assert.match(managerPatched, /&&el\(__codeyConversation\)&&/, "the native in-progress predicate is reused");
  Function(managerPatched);
  assert.deepEqual(logs, []);
});

test("rspack service tier gates grant tiers declared by the model catalog", async () => {
  const { logs, patch } = await installPatch();
  const access = 'let o="default";function i(e){let t=e.requirements?.featureRequirements;return{ultrafast:"supportsIndependentSpeedModes"in e?t?.ultrafast_mode!==!1:!1}}function s(e,t){if(null==e||e===o)return!0;if(null==t)return!1;let n=l(e);return"flex"===e||t[n??"fast"]}function l(e){return e}';
  const accessCheck = Function(`${patch(access)};return s`)();
  assert.equal(accessCheck("fast", { fast: false }), true);
  assert.equal(accessCheck("ultrafast", null), true);

  const state = 'function U(c,_,p,g,h,s,r,n){let t,a,u;let{data:m,isPending:x}=n(s.b9,t),b=!!_?.isLoading||g&&x;c[3]!==m||c[4]!==b||c[5]!==g?(a=g&&!b&&null!=m?(0,r.W6)(m):null,c[3]=m,c[4]=b,c[5]=g,c[6]=a):a=c[6];let y=a;return c[7]!==b||c[8]!==y?(u={serviceTierAccess:y,isLoading:b},c[7]=b,c[8]=y,c[9]=u):u=c[9],u}';
  const hook = Function(`${patch(state)};return U`)();
  const result = hook([], { isLoading: false }, null, false, null, {}, { W6: () => "native" }, () => ({ data: null, isPending: false }));
  assert.deepEqual({ ...result.serviceTierAccess }, { fast: true, ultrafast: true }, "API key logins keep the speed control");
  assert.deepEqual(logs, []);
});

test("rspack Ultra reasoning gate accepts module-member feature ids", async () => {
  const { logs, patch } = await installPatch();
  const source = 'const q=({includeUltraReasoningEffort:l,isCustomModelProvider:i,limit:u,modelCatalogPath:c},{get:v,queryClient:I})=>{let k=null!=c,O=l&&v(b.JW,"1186680773");return{queryKey:[k],ultra:O}};';
  const patched = patch(source);
  assert.match(patched, /O=l&&\(i\|\|v\(b\.JW,"1186680773"\)\)/);
  const query = Function("b", `${patched};return q`)({ JW: "gate" });
  const get = () => false;
  assert.equal(query({ includeUltraReasoningEffort: true, isCustomModelProvider: true }, { get }).ultra, true);
  assert.equal(query({ includeUltraReasoningEffort: true, isCustomModelProvider: false }, { get }).ultra, false);
  assert.equal(query({ includeUltraReasoningEffort: false, isCustomModelProvider: true }, { get }).ultra, false);
  assert.deepEqual(logs, []);
});

test("rspack telemetry patches skip heartbeat snapshots and interaction sampling", async () => {
  const { logs, patch } = await installPatch();
  const snapshot = 'async function h(e,t){switch(e.type){case"electron-app-state-snapshot-request":{let a=await t.collectAppStateSnapshot(e.hostId,e.reason);if(null==a)break;t.dispatchMessage("electron-app-state-snapshot-response",{requestId:e.requestId,snapshot:a});break}default:return"other"}return"replied"}';
  const handle = Function(`${patch(snapshot)};return h`)();
  const calls = [];
  const host = { collectAppStateSnapshot: async (...args) => (calls.push(args), {}), dispatchMessage: (...args) => calls.push(args) };
  assert.equal(await handle({ type: "electron-app-state-snapshot-request", hostId: "local", reason: "heartbeat" }, host), "replied");
  assert.deepEqual(calls, [], "the 30 s heartbeat no longer collects a snapshot");
  await handle({ type: "electron-app-state-snapshot-request", hostId: "local", reason: "feedback" }, host);
  assert.equal(calls.length, 2, "other diagnostic reasons still reply");

  const interactions = 'class R{constructor(){this.activeInteractions=new Map}begin(E,n){return{cpuSampling:"dropped"===E||!0===n.backfilled?null:this.beginCpuSampling()}}beginCpuSampling(){return"sampling"}ensureHeartbeat(){null==this.heartbeatTimer&&(this.heartbeatTimer=setInterval(()=>{let e=this.now(),t=this.wallNow();for(let n of this.activeInteractions.values())this.recordHeartbeat(n,e,t)},1e3))}}';
  const Recorder = Function(`${patch(interactions)};return R`)();
  const recorder = new Recorder();
  assert.equal(recorder.begin("sampled", {}).cpuSampling, null);
  recorder.ensureHeartbeat();
  assert.equal(recorder.heartbeatTimer, undefined, "no 1 Hz heartbeat timer is started");
  assert.deepEqual(logs, []);
});

test("rspack checkout prewarm and avatar overlay prewarm stay off until shown", async () => {
  const { logs, patch } = await installPatch({ disablePet: true });
  const pricing = 'function ue(K,Z,q,et,Q,ee,j,P,y){let m;m=!(0,P.$t)()&&"chatgpt"===K&&!Z&&null!=q&&!et&&Q?.structure==="personal"&&(0,y.XM)(ee,j,{disableExposureLog:!0}).get("use_embedded_checkout",!1)?function({currentPlan:e,url:t}){return new URL(t).href}:null;return{forcePricingRefreshInBackground:!0,search:m}}';
  const credit = 'function U(H,F){let c,g,u={useLayoutEffect(){}};if((0,u.useLayoutEffect)(c,g),!H&&(!F||!({get:()=>F}).get("use_embedded_credit_checkout",!1)))return null;return{isPrewarm:!H}}';
  const patched = patch(`${pricing}${credit}`);
  const pricingHook = Function(`${patched};return ue`)();
  const eligible = ["chatgpt", false, {}, false, { structure: "personal" }, null, null, { $t: () => false }, { XM: () => ({ get: () => true }) }];
  assert.equal(pricingHook(...eligible).search, null, "no hidden pricing page is preloaded");
  const creditHook = Function(`${patched};return U`)();
  assert.equal(creditHook(false, true), null, "no hidden credit page is mounted");
  assert.deepEqual({ ...creditHook(true, true) }, { isPrewarm: false }, "a presented credit page still loads");

  const runtime = '(0,u.useEffect)(()=>{l||t0.O1.dispatchMessage("electron-avatar-overlay-restore-ready",{onDemandEnabled:k,shouldPrewarmAvatarOverlay:t&&eP})},[k,t,l,eP]);';
  assert.match(patch(runtime), /\{onDemandEnabled:k,shouldPrewarmAvatarOverlay:!1\}/);
  assert.deepEqual(logs, []);
});

test("rspack draft defaults patch the split factory and every settings hook chunk", async () => {
  const { logs, patch } = await installPatch();
  const factory = 'let c=(0,a.createContext)(void 0),p=(0,i.bD)(l.u,e=>({isManuallyChanged:!1,modelSettings:null,serviceTier:null}));function h(e){return{isNewThreadDraft:e}}';
  const factoryPatched = patch(factory);
  const makeDraft = Function("a", "i", "l", `${factoryPatched};return p`)(
    { createContext: () => null },
    { bD: (_atom, create) => create },
    { u: null },
  );
  assert.deepEqual(makeDraft(), { isManuallyChanged: false, modelSettings: null, serviceTier: { value: "default" } });

  const hook = [
    'let s={modelSettingsPersistAsDefault:!0};function ed(e){var t,n;let r,a,p=(0,l.c)(22),{hostId:m,managedNewThreadModelSettings:M,draftModelSettings:D,isNewThreadDraft:T}=e,P=void 0!==T&&T,',
    'ey=(0,et.Zw)({userSavedModelString:"saved",userSavedReasoningEffort:"high",listModelsData:{models:[{model:"saved"}]}});',
    "return ey}function after(){return{isManuallyChanged:!0}}",
  ].join("");
  const hookPatched = patch(hook, "app://-/assets/async/328669.bfa87d955e.js");
  assert.equal(patch(hookPatched, "app://-/assets/async/328669.bfa87d955e.js"), null, "the patch is idempotent");
  const settingsHook = Function("l", "et", `${hookPatched};return ed`)(
    { c: () => [] },
    { Zw: ({ userSavedModelString: model, userSavedReasoningEffort: reasoningEffort }) => ({ model, reasoningEffort }) },
  );
  assert.equal(settingsHook({ isNewThreadDraft: true }).reasoningEffort, "medium", "a fresh draft starts at medium");
  assert.equal(settingsHook({ isNewThreadDraft: false }).reasoningEffort, "high", "existing threads keep the saved effort");
  assert.equal(settingsHook({ isNewThreadDraft: true, draftModelSettings: { model: "x" } }).reasoningEffort, "high", "manual draft choices are kept");
  assert.equal(settingsHook({ isNewThreadDraft: true, managedNewThreadModelSettings: {} }).reasoningEffort, "high", "managed settings are kept");
  assert.deepEqual(logs, []);
});

test("unrelated rspack chunks are skipped by the patch anchors", async () => {
  const { logs, patch } = await installPatch();
  assert.equal(patch('export const __rspack_esm_id=1;export const __webpack_modules__={a(e,t){t.A="icon"}};'), null);
  assert.equal(
    patch('(0,u.useEffect)(()=>{l||t0.O1.dispatchMessage("electron-avatar-overlay-restore-ready",{onDemandEnabled:k,shouldPrewarmAvatarOverlay:t&&eP})},[k,t,l,eP]);'),
    null,
    "pet-only anchors stay inactive when pet slimming is disabled",
  );
  assert.deepEqual(logs, []);
});
