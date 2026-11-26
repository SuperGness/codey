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
    'enqueueRequest(e,t,n,r=t=>{this.dispatchMessage?.("mcp-request",{request:t,hostId:this.hostId,retainResponse:void 0,source:n?.source,timeoutMs:n?.timeoutMs,widget:n?.widget})},i=null,a){if(a?.aborted)return Promise.reject(new DOMException("App-server request cancelled","AbortError"));let s=1;log("mcp_request_enqueued");let{request:o,promise:l}=this.createRequest(e,t,n,i);r(o);return l}',
    'onResult(e,t){let r=this.requestPromises.get(e);r.resolve(t),this.emitRequestLifecycleEvent({type:"completed",hostId:this.hostId,method:r.method,id:e})}}',
  ].join("");
  const patched = patch(source);
  for (const marker of [
    "let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage",
    "globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage",
    "globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult",
    "globalThis.__codeyModelRequestSourceGateInstalled=true",
  ]) assert.ok(patched.includes(marker), marker);
  assert.equal(patch(patched), null, "the patch is idempotent");
  // Pre-aborted requests reject before running route preflight.
  let guardCalled = false;
  globalThis.__codeyModelWhitelistPatch = {
    guardOutgoingRequest: () => { guardCalled = true; return null; },
  };
  const C = Function("log", `${patched};return C`)(() => {});
  const client = new C();
  client.hostId = "local";
  await assert.rejects(
    client.enqueueRequest("model/list", {}, undefined, undefined, null, { aborted: true }),
    /cancelled/,
  );
  assert.equal(guardCalled, false);
  assert.deepEqual(logs, []);
});

test("rspack hosted manager marker follows reversed null checks", async () => {
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
  assert.deepEqual(logs, []);
});

test("rspack service tier gates grant tiers declared by the model catalog", async () => {
  const { logs, patch } = await installPatch();
  const access = 'let o="default";function i(e){let t=e.requirements?.featureRequirements;return{ultrafast:"supportsIndependentSpeedModes"in e?t?.ultrafast_mode!==!1:!1}}function s(e,t){if(null==e||e===o)return!0;if(null==t)return!1;let n=l(e);return"flex"===e||t[n??"fast"]}function l(e){return e}';
  const patchedAccess = patch(access);
  assert.equal(patch(patchedAccess), null, "the service tier access patch is idempotent");
  const accessCheck = Function(`${patchedAccess};return s`)();
  assert.equal(accessCheck("fast", { fast: false }), true);
  assert.equal(accessCheck("ultrafast", null), true);

  const state = 'function U(c,_,p,g,h,s,r,n){let t,a,u;let{data:m,isPending:x}=n(s.b9,t),b=!!_?.isLoading||g&&x;c[3]!==m||c[4]!==b||c[5]!==g?(a=g&&!b&&null!=m?(0,r.W6)(m):null,c[3]=m,c[4]=b,c[5]=g,c[6]=a):a=c[6];let y=a;return c[7]!==b||c[8]!==y?(u={serviceTierAccess:y,isLoading:b},c[7]=b,c[8]=y,c[9]=u):u=c[9],u}';
  const patchedState = patch(state);
  assert.equal(patch(patchedState), null, "the service tier state patch is idempotent");
  const hook = Function(`${patchedState};return U`)();
  const result = hook([], { isLoading: false }, null, false, null, {}, { W6: () => "native" }, () => ({ data: null, isPending: false }));
  assert.deepEqual({ ...result.serviceTierAccess }, { fast: true, ultrafast: true }, "API key logins keep the speed control");
  assert.deepEqual(logs, []);
});

test("rspack Ultra reasoning gate unlocks custom providers from computed local flag", async () => {
  const { logs, patch } = await installPatch();
  const source = 'const q=({hostId:l,includeUltraReasoningEffort:r=!0,limit:i,teamCatalog:d},{get:h,queryClient:g})=>{let E=(0,p.cc)(l,d),W=r&&h(M.JW,"1186680773");return{queryKey:[l],select:()=>({includeUltraReasoningEffort:W,isCustomModelProvider:E})}};';
  const patched = patch(source);
  assert.match(patched, /W=r&&\(E\|\|h\(M\.JW,"1186680773"\)\)/);
  const query = Function("p", "M", `${patched};return q`)(
    { cc: (_host, custom) => Boolean(custom) },
    { JW: "gate" },
  );
  const get = () => false;
  assert.equal(query({ hostId: "local", includeUltraReasoningEffort: true, teamCatalog: true }, { get }).select().includeUltraReasoningEffort, true);
  assert.equal(query({ hostId: "local", includeUltraReasoningEffort: true, teamCatalog: false }, { get }).select().includeUltraReasoningEffort, false);
  assert.equal(query({ hostId: "local", includeUltraReasoningEffort: false, teamCatalog: true }, { get }).select().includeUltraReasoningEffort, false);

  // Host services chunk passes the Ultra gate into dynamic tools without a teamCatalog model query; leave it untouched.
  const hostServicesSource = [
    "function Hs(t,e,c){return Sm({hostId:e.hostId,includeUltraReasoningEffort:Kl(t,`1186680773`),",
    "isAuthLoading:c.isLoading,isCustomModelProvider:e.isCustomModelProvider})}",
  ].join("");
  assert.equal(
    patch(hostServicesSource, "app://-/assets/async/49792.d8d33ace73.js"),
    null,
  );
  assert.deepEqual(logs, [], "a gate argument without the catalog query is not a drift");
});

test("rspack telemetry patches skip heartbeat snapshots and interaction sampling", async () => {
  const { logs, patch } = await installPatch();
  const snapshot = 'async function h(e,t){switch(e.type){case"electron-app-state-snapshot-request":{let a=await t.collectAppStateSnapshot(e.hostId,e.reason);if(null==a)break;t.dispatchMessage("electron-app-state-snapshot-response",{requestId:e.requestId,snapshot:a});break}default:return"other"}return"replied"}';
  const patchedSnapshot = patch(snapshot);
  assert.equal(patch(patchedSnapshot), null, "the heartbeat snapshot patch is idempotent");
  const handle = Function(`${patchedSnapshot};return h`)();
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
  assert.deepEqual(logs, []);
});

test("rspack stylesheet assets rewrite data-state :has() rules without touching unrelated css", async () => {
  const { logs, patch } = await installPatch();
  const mainCss = [
    ".group-has-\\[\\[data-state\\=open\\]\\]\\:opacity-100:is(:where(.group):has([data-state=open]) *){opacity:1}",
    ".group-has-\\[\\[data-state\\=open\\]\\]\\/project-source\\:opacity-100:is(:where(.group\\/project-source):has([data-state=open]) *){opacity:1}",
    ":where(.threadCard-Zx98Ab > *):has([data-state=open]){content-visibility:visible}",
    ".has-\\[\\[data-state\\=open\\]\\]\\:opacity-100:has([data-state=open]){opacity:1}",
    ".has-\\[button\\[data-state\\=open\\]\\]\\:opacity-100:has(:is(button[data-state=open])){opacity:1}",
  ].join("");
  const patchedMain = patch(mainCss, "app://-/assets/async/e77f816560c6.css");
  assert.ok(patchedMain, "hashed async stylesheet should be patched");
  assert.doesNotMatch(patchedMain, /:has\([^)]*data-state/);
  assert.match(patchedMain, /:where\(\.group\)\[data-codey-open-within\]/);
  assert.match(patchedMain, /:where\(\.group\\\/project-source\)\[data-codey-open-within\]/);
  assert.match(patchedMain, /\.has-\\\[\\\[data-state\\=open\\\]\\\]\\:opacity-100\[data-codey-open-within\]/);
  assert.match(patchedMain, /\.has-\\\[button\\\[data-state\\=open\\\]\\\]\\:opacity-100\[data-codey-open-within\]/);
  const markerMatch = patchedMain.match(
    /:root\{--codey-css-open-state-has-patched:1;--codey-open-within-classes:"([^"]+)"\}/,
  );
  assert.ok(markerMatch, "patched stylesheet emits open-within class metadata");
  const harvestedClasses = JSON.parse(decodeURIComponent(markerMatch[1]));
  assert.deepEqual(harvestedClasses.d, ["threadCard-Zx98Ab"]);
  assert.deepEqual(
    harvestedClasses.a.sort(),
    ["group", "group/project-source", "has-[[data-state=open]]:opacity-100", "threadCard-Zx98Ab"],
  );
  assert.deepEqual(
    harvestedClasses.b,
    ["has-[button[data-state=open]]:opacity-100"],
  );
  assert.equal(
    patch(patchedMain, "app://-/assets/async/e77f816560c6.css"),
    null,
    "patched stylesheet is idempotent",
  );

  assert.equal(
    patch(".plain{color:red}[data-state=open]{opacity:1}", "app://-/assets/async/9012f383f623.css"),
    null,
    "stylesheets without :has(...data-state...) pass through unchanged",
  );
  assert.equal(
    patch(mainCss, "app://-/assets/unhashed.css"),
    null,
    "unhashed css urls are ignored",
  );
  assert.deepEqual(logs, []);
});

test("rspack CheckoutPersonalWebviewHost skips background prewarm and renders only presented checkout", async () => {
  const { logs, patch } = await installPatch();
  const source = [
    'const r={c:(n)=>Array(n).fill(Symbol.for("react.memo_cache_sentinel"))},a={Fragment:"Fragment",jsx:(type,props,key)=>({type,props,key}),jsxs:(type,props,key)=>({type,props,key})},v={O1:{dispatchMessage(){}}};',
    'function M(searchArg,useCheckout,useCredit){let s=(0,r.c)(10),W=searchArg,N=useCheckout,z=useCredit,v="https://chatgpt.com",R="u1",k="a1";let F=JSON.stringify([R,k,v]),G=null!=W||N||z;return(0,a.jsx)(A,{chatGptOrigin:v,enabled:G,search:W},F)}',
    'function A(e){let u=(0,r.c)(15),{chatGptOrigin:c,enabled:d,search:h}=e,m="part",f="prewarm=1";if(v.O1.dispatchMessage("checkout-webview-presentation-changed",{partition:m,url:h}),!d)return null;let i=(0,a.jsx)("div",{"aria-hidden":!0,inert:!0,className:"pointer-events-none invisible fixed inset-0",children:(0,a.jsx)(W,{isPrewarm:!0,partition:m,search:f})}),l=null==h?null:(0,a.jsx)("div",{className:"fixed inset-0 z-30",children:(0,a.jsx)(W,{partition:m,search:h},h)});return(0,a.jsxs)(a.Fragment,{children:[i,l]})}',
    'function W(props){return props}',
  ].join("");
  const url = "app://-/assets/async/162421.5b98cfa6cf.js";
  const patched = patch(source, url);
  assert.ok(patched, "CheckoutPersonalWebviewHost chunk should be patched");
  assert.ok(patched.includes("/*codey-personal-checkout-prewarm*/"));
  assert.equal(patch(patched, url), null, "the patch is idempotent");
  const { M, A } = Function(`${patched};return{M,A}`)();
  const hiddenHost = M(null, true, true);
  assert.equal(hiddenHost.props.enabled, false, "embedded checkout experiments no longer enable background prewarm");
  assert.equal(A(hiddenHost.props), null, "disabled host mounts nothing");

  const activeHost = M("?accountId=personal&url=https%3A%2F%2Fchatgpt.com%2F%23pricing", true, true);
  assert.equal(activeHost.props.enabled, true, "explicit checkout navigation still enables the host");
  const rendered = A(activeHost.props);
  assert.equal(rendered.props.children[0], null, "hidden isPrewarm webview wrapper is skipped");
  assert.equal(rendered.props.children[1].props.children.props.search, activeHost.props.search, "presented checkout webview still renders");
  assert.deepEqual(logs, []);
});

test("rspack performance gates cap pierre workers and skip orbit preload, route prefetch, LoAF observers, and pending sampler", async () => {
  const { logs, patch } = await installPatch();

  // 1. Pierre syntax highlight worker pool: poolSize 2/4 -> 1 and totalASTLRUCacheSize 100 -> 32; Shiki tokenizeTimeLimit 0 -> 250.
  const pierreUrl = "app://-/assets/async/660852.c4b8f73f76.js";
  const pierreSource = "function k(){return{}}function render(){let t={workerFactory:k,poolSize:2,totalASTLRUCacheSize:100};return t}";
  const patchedPierre = patch(pierreSource, pierreUrl);
  assert.ok(patchedPierre?.includes("/*codey-pierre-pool-size*/1"));
  assert.equal(patch(patchedPierre, pierreUrl), null);
  const pierreCfg = Function(`${patchedPierre};return render()`)();
  assert.equal(pierreCfg.poolSize, 1);
  assert.equal(pierreCfg.totalASTLRUCacheSize, 32);
  assert.equal(
    patch("function k(){return{}}function render(){let t={workerFactory:k,poolSize:1,totalASTLRUCacheSize:32};return t}", pierreUrl),
    null,
  );

  for (const [shikiUrl, shikiSource] of [
    [
      "app://-/assets/async/625546.e2f9da89f8.js",
      'function K(e,t,i,n){return{lang:e,..."string"==typeof t?{theme:t}:{themes:t},defaultColor:!1,cssVariablePrefix:(0,n.D)("token"),tokenizeMaxLineLength:i,tokenizeTimeLimit:0}}',
    ],
    [
      "app://-/assets/async/76322.eaa934f410.js",
      'function K(e,t,i,N){return{lang:e,..."string"==typeof t?{theme:t}:{themes:t},defaultColor:!1,cssVariablePrefix:N("token"),tokenizeMaxLineLength:i,tokenizeTimeLimit:0}}',
    ],
  ]) {
    const patchedShiki = patch(shikiSource, shikiUrl);
    assert.ok(patchedShiki?.includes("/*codey-shiki-tokenize-time-limit*/250"), shikiUrl);
    assert.equal(patch(patchedShiki, shikiUrl), null);
    const opts = Function(
      `${patchedShiki};return K("ts","dark",20000,Object.assign((s)=>s,{D:(s)=>s}))`,
    )();
    assert.equal(opts.tokenizeTimeLimit, 250);
  }

  // 2. Orbit character preload iframe: d() becomes a callable no-op while preserving Oz/iE.
  const orbitUrl = "app://-/assets/async/198194.4d5c9b6c90.js";
  const orbitSource = 'var o={a:()=>"/frame.html"},c={prefetch(){spy.prefetch+=1}},s={s(){spy.s+=1}},l=!1,a={d(_t,_e,exports){exports.Oz=function(){d(),(0,s.s)()},exports.iE=d}};function d(){let e=(0,o.a)();if("u"<typeof document||null==e||(c.prefetch(),l))return;l=!0;let t=document.createElement("iframe"),a=new URL(e,document.baseURI);a.searchParams.set("preload",""),t.src=a.href,t.hidden=!0,t.setAttribute("sandbox","allow-scripts allow-same-origin"),t.allow="cross-origin-isolated";let n=new AbortController,r=window.setTimeout(i,3e4);function i(){window.clearTimeout(r),n.abort(),t.remove()}window.addEventListener("message",e=>{e.source===t.contentWindow&&e.origin===window.location.origin&&"orbit-character-preloaded"===e.data&&i()},{signal:n.signal}),window.addEventListener("pagehide",i,{once:!0,signal:n.signal}),document.body.append(t)}const out={};a.d(null,null,out);';
  const patchedOrbit = patch(orbitSource, orbitUrl);
  assert.ok(patchedOrbit?.includes("/*codey-orbit-preload-skip*/"));
  assert.equal(patch(patchedOrbit, orbitUrl), null);
  const appended = [];
  const spy = { prefetch: 0, s: 0 };
  const api = Function(
    "spy",
    "document",
    "window",
    `${patchedOrbit};return out`,
  )(
    spy,
    { baseURI: "https://example.invalid/", createElement: () => ({}), body: { append: (el) => appended.push(el) } },
    { setTimeout: () => 1, clearTimeout() {}, addEventListener() {}, location: { origin: "https://example.invalid" } },
  );
  api.Oz();
  api.iE();
  assert.equal(spy.prefetch, 0);
  assert.equal(spy.s, 1);
  assert.equal(appended.length, 0);

  // 3. Startup secondary route prefetch: e_() becomes a no-op without prefetching settings/dialog chunks.
  const prefetchUrl = "app://-/assets/async/55935.5583618e49.js";
  const prefetchSource = 'function e_(){ea.x.prefetch(),function(){for(let e of(O.B.prefetch(),["general-settings","appearance","keyboard-shortcuts"]))(0,en.OU)(e).catch(()=>void 0);Promise.all([t.e(1),t.e(2)]).then(t.bind(t,"njHN")).catch(()=>void 0)}()}';
  const patchedPrefetch = patch(prefetchSource, prefetchUrl);
  assert.ok(patchedPrefetch?.includes("/*codey-startup-prefetch-skip*/"));
  assert.equal(patch(patchedPrefetch, prefetchUrl), null);
  let prefetchCalls = 0;
  Function("ea", "O", "en", "t", `${patchedPrefetch};e_();`)(
    { x: { prefetch() { prefetchCalls += 1; } } },
    { B: { prefetch() { prefetchCalls += 1; } } },
    { OU: async () => { prefetchCalls += 1; } },
    { e() { prefetchCalls += 1; return Promise.resolve(); }, bind() {} },
  );
  assert.equal(prefetchCalls, 0);

  // 4. Orbit navigation startup prefetch: effect body becomes a no-op while preserving hook slots.
  const orbitNavSource = 'function e_(){ea.x.prefetch(),function(){for(let e of(O.B.prefetch(),["general-settings","appearance","keyboard-shortcuts"]))(0,en.OU)(e).catch(()=>void 0);Promise.all([t.e(1)]).then(t.bind(t,"njHN")).catch(()=>void 0)}()}function run(eU,l,c,b,n,V){let O,H;let eZ=(0,l._U)(c.G,{name:"orbit.navigation"})?.isCapable;eU[18]!==eZ?(O=()=>{eZ&&(b.$O.prefetch(),n.e(271496).then(n.bind(n,"gjBn")).catch(()=>void 0),(0,V.Wy)().catch(()=>void 0))},H=[eZ],eU[18]=eZ,eU[19]=O,eU[20]=H):(O=eU[19],H=eU[20]);O();return H}';
  const patchedOrbitNav = patch(orbitNavSource, "app://-/assets/async/554595.c9f05dac4c.js");
  assert.ok(patchedOrbitNav?.includes("/*codey-orbit-nav-prefetch-skip*/"));
  assert.ok(patchedOrbitNav?.includes("/*codey-startup-prefetch-skip*/"));
  assert.equal(patch(patchedOrbitNav, "app://-/assets/async/554595.c9f05dac4c.js"), null);
  let orbitNavCalls = 0;
  const orbitDeps = Function(`${patchedOrbitNav};return run`)()(
    Array(25).fill(Symbol.for("react.memo_cache_sentinel")),
    { _U: () => ({ isCapable: true }) },
    { G: {} },
    { $O: { prefetch() { orbitNavCalls += 1; } } },
    { e() { orbitNavCalls += 1; return Promise.resolve(); }, bind() {} },
    { Wy: async () => { orbitNavCalls += 1; } },
  );
  assert.equal(orbitNavCalls, 0);
  assert.deepEqual(orbitDeps, [true]);

  // 5. Frame gap / event observer (jgoj): Z() returns inert handle without starting rAF or PerformanceObserver.
  const frameGapUrl = "app://-/assets/async/366996.9782070574.js";
  const frameGapSource = 'let i,r,o,a=new Set;function s(e,t){return new PerformanceObserver(t)}function l(e){for(let t of e)if(t instanceof PerformanceEventTiming&&t.interactionId>0)return}function c(){}function u(){r=requestAnimationFrame(u)}const mod={R:s,Z:function(e){return 0===a.size&&(i=s("event",l),document.addEventListener("visibilitychange",u),u()),a.add(e),{eventsSupported:null!=i,stop:()=>{a.has(e)&&(null!=i&&l(i.takeRecords()),a.delete(e),0===a.size&&(i?.disconnect(),i=void 0,document.removeEventListener("visibilitychange",u),c()))}}}};const cfg={durationThreshold:16};';
  const patchedFrameGap = patch(frameGapSource, frameGapUrl);
  assert.ok(patchedFrameGap?.includes("/*codey-frame-gap-observer-skip*/"));
  assert.equal(patch(patchedFrameGap, frameGapUrl), null);
  let rafCalls = 0;
  let poCalls = 0;
  const frameGapMod = Function("PerformanceObserver", "requestAnimationFrame", "document", `${patchedFrameGap};return mod`)(
    class { constructor() { poCalls += 1; } },
    () => { rafCalls += 1; return 1; },
    { addEventListener() { rafCalls += 1; }, removeEventListener() {} },
  );
  const handle = frameGapMod.Z({ event() {}, frame() {} });
  assert.equal(handle.eventsSupported, false);
  assert.doesNotThrow(() => handle.stop());
  assert.equal(rafCalls, 0);
  assert.equal(poCalls, 0);

  // 6. Startup event observer (HPUJ): skips 30s 16ms PerformanceObserver while preserving deadline lifecycle.
  const startupEventSource = 'class Startup{constructor(){this.interactions={supported:!1};try{"u">typeof PerformanceObserver&&PerformanceObserver.supportedEntryTypes?.includes("event")&&(this.eventObserver=new PerformanceObserver(e=>this.addEvents(e.getEntries())),this.eventObserver.observe({type:"event",buffered:!0,durationThreshold:16}),this.interactions.supported=!0)}catch{this.eventObserver?.disconnect(),this.eventObserver=void 0}this.deadline=1}}';
  const patchedStartupEvent = patch(startupEventSource, frameGapUrl);
  assert.ok(patchedStartupEvent?.includes("/*codey-startup-event-observer-skip*/"));
  assert.equal(patch(patchedStartupEvent, frameGapUrl), null);
  let startupPoCalls = 0;
  const Startup = Function("PerformanceObserver", `${patchedStartupEvent};return Startup`)(
    Object.assign(class { constructor() { startupPoCalls += 1; } observe() {} }, { supportedEntryTypes: ["event"] }),
  );
  const startup = new Startup();
  assert.equal(startup.deadline, 1);
  assert.equal(startup.interactions.supported, false);
  assert.equal(startupPoCalls, 0);

  // 7. Windows Sentry EventLoopBlockRenderer hang detection: skips 500ms IPC heartbeat integration.
  const hangUrl = "app://-/assets/async/49792.d8d33ace73.js";
  const hangSource = 'function init(e,added){let n;e.rendererHangDetectionEnabled&&function(i){added.push(i.name)}((n=500,{name:"EventLoopBlockRenderer",setup(){}}))}';
  const patchedHang = patch(hangSource, hangUrl);
  assert.ok(patchedHang?.includes("/*codey-hang-detection-skip*/"));
  assert.equal(patch(patchedHang, hangUrl), null);
  const addedIntegrations = [];
  Function(`${patchedHang};return init`)()({ rendererHangDetectionEnabled: true }, addedIntegrations);
  assert.deepEqual(addedIntegrations, []);

  // 8. Interaction LoAF + longtask observers: ensureObserver returns false cleanly.
  const interactionUrl = "app://-/assets/async/956370.aff5cce818.js";
  const interactionSource = 'class Tracker{start(){let n=this.ensureObserver("longtask"),i=this.ensureObserver("long-animation-frame");return{longAnimationFrameSupported:i,longTaskSupported:n}}ensureObserver(e){try{if(!this.isSupported(e))return!1}catch{return!1}if(null!=("longtask"===e?this.longTaskObserver:this.longAnimationFrameObserver))return!0;try{let t="longtask"===e?this.longTaskEntries:this.longAnimationFrameEntries,n=this.createObserver(e,e=>{t.push(...e)});return n.observe({buffered:!0,type:e}),"longtask"===e?this.longTaskObserver=n:this.longAnimationFrameObserver=n,!0}catch{return!1}}}';
  const patchedInteraction = patch(interactionSource, interactionUrl);
  assert.ok(patchedInteraction?.includes("/*codey-interaction-loaf-skip*/"));
  assert.equal(patch(patchedInteraction, interactionUrl), null);
  let createdObservers = 0;
  const Tracker = Function(`${patchedInteraction};return Tracker`)();
  const tracker = new Tracker();
  tracker.isSupported = () => true;
  tracker.createObserver = () => { createdObservers += 1; return { observe() {} }; };
  assert.deepEqual(tracker.start(), { longAnimationFrameSupported: false, longTaskSupported: false });
  assert.equal(createdObservers, 0);

  // 9. Codex pages pending observation sampler: 10s interval & doc listeners skipped, sendHeartbeat preserved.
  const pagesSource = 'function startObs(s,e,C){let r,n,l=()=>{},u=()=>{},d=()=>{};return n=C,s.add(n),1===s.size&&(r=setInterval(()=>{for(let e of[...s])e.sample()},1e4),document.addEventListener("visibilitychange",l),document.addEventListener("freeze",u),window.addEventListener("pagehide",d,!0)),e.count("codex_pages",`${p}_started`)}function afterObs(){return {p_pending_age_ms:"pending_age_ms",m:"_pending_observations"}}';
  const pagesWithHeartbeat = `${pagesSource}class Sync{sendHeartbeat=()=>{this.metrics.count("codex_pages","edit_pending_observations",{},0)}}`;
  const patchedPages = patch(pagesWithHeartbeat, hangUrl);
  assert.ok(patchedPages?.includes("/*codey-pending-observations-sampler-skip*/"));
  assert.match(patchedPages, /sendHeartbeat=\(\)=>\{this\.metrics\.count/);
  assert.equal(patch(patchedPages, hangUrl), null);
  let intervals = 0;
  let listeners = 0;
  const events = [];
  Function("p", "document", "window", "setInterval", "events", `${patchedPages};startObs(new Set,{count:(a,b)=>events.push([a,b])},{sample(){}})`)(
    "open",
    { addEventListener() { listeners += 1; } },
    { addEventListener() { listeners += 1; } },
    () => { intervals += 1; return 1; },
    events,
  );
  assert.equal(intervals, 0);
  assert.equal(listeners, 0);
  assert.deepEqual(events, [["codex_pages", "open_started"]]);

  assert.deepEqual(logs, []);
});

test("rspack Statsig startup gates bound post-login bootstrap, skip remote identity stall, and cap async client init", async () => {
  const { logs, patch } = await installPatch();
  const url = "app://-/assets/async/907538.0572f09d40.js";

  // 1. Post-login bootstrap timeout: 62500ms deadline -> 12000ms (12e3), 1e4 attempt timeout -> 8000ms (8e3).
  const bootstrapSource =
    'async function runBootstrap(){let n,r={auth:0,request:0},i=new AbortController,a=setTimeout(()=>{i.abort(new Ny("req_failed",Error("Reached the post-login Statsig bootstrap deadline")))},62500);try{return await async function(u){let d,h=new AbortController,_=u?AbortSignal.any([h.signal,u]):h.signal;function m(){clearTimeout(d),d=setTimeout(()=>{h.abort(Error("Timed out while fetching post-login Statsig bootstrap"))},1e4)}return m(),_.aborted}(i.signal)}finally{clearTimeout(n),clearTimeout(a)}}';
  const patchedBootstrap = patch(bootstrapSource, url);
  assert.ok(patchedBootstrap?.includes("/*codey-statsig-bootstrap-deadline*/12e3"));
  assert.ok(patchedBootstrap?.includes("/*codey-statsig-bootstrap-attempt*/8e3"));
  assert.equal(patch(patchedBootstrap, url), null, "bootstrap timeout patch is idempotent");
  const recordedDelays = [];
  await Function(
    "Ny",
    "c",
    "setTimeout",
    "clearTimeout",
    `${patchedBootstrap};return runBootstrap();`,
  )(
    class extends Error {},
    {},
    (fn, ms) => {
      recordedDelays.push(ms);
      return { fn, ms };
    },
    () => {},
  );
  assert.deepEqual(recordedDelays, [12000, 8000]);

  // 2. Fallback identity non-blocking: skips remote accounts/check wait while keeping local token wait.
  const identitySource =
    'function nJ(e){let t,n,r,i=(0,l.c)(17),{currentAccount:c,children:v}=e;let{data:y,isLoading:I,hasEverErrored:T}=useAccounts(void 0,t);let{isLoading:C}=useAccountInfo(n),A=c??y;if(C||I&&!T){let e;return i[2]===S?(e=jsx(Loader,{debugName:"CodexStatsigProvider.async.identity"}),i[2]=e):e=i[2],e}return i[3]=jsx(Fallback,{currentAccount:A,children:v}),i[3]}';
  const patchedIdentity = patch(identitySource, url);
  assert.ok(patchedIdentity?.includes("/*codey-statsig-identity-nonblocking*/"));
  assert.equal(patch(patchedIdentity, url), null, "identity non-blocking patch is idempotent");
  const runIdentity = ({ localLoading, accountsLoading, errored, accountsData }) =>
    Function(
      "l",
      "S",
      "jsx",
      "Loader",
      "Fallback",
      "useAccounts",
      "useAccountInfo",
      `${patchedIdentity};return nJ({currentAccount:undefined,children:"routes"});`,
    )(
      { c: (n) => Array(n).fill(Symbol.for("react.memo_cache_sentinel")) },
      Symbol.for("react.memo_cache_sentinel"),
      (type, props) => ({ type, props }),
      "Loader",
      "Fallback",
      () => ({ data: accountsData, isLoading: accountsLoading, hasEverErrored: errored }),
      () => ({ isLoading: localLoading }),
    );
  const localWaiting = runIdentity({
    localLoading: true,
    accountsLoading: false,
    errored: false,
    accountsData: undefined,
  });
  assert.equal(localWaiting.type, "Loader");
  assert.equal(localWaiting.props.debugName, "CodexStatsigProvider.async.identity");
  const remoteStalled = runIdentity({
    localLoading: false,
    accountsLoading: true,
    errored: false,
    accountsData: { id: "cached-account" },
  });
  assert.equal(remoteStalled.type, "Fallback");
  assert.deepEqual(remoteStalled.props.currentAccount, { id: "cached-account" });
  assert.equal(remoteStalled.props.children, "routes");

  // 3. Fallback async Statsig client init cap: fast init finishes cleanly; stalled init finalizes Ready at 3000ms.
  const asyncInitSource =
    'const t={};t.useStatsigInternalClientFactoryAsync=function(e,t){let n=(0,i.useMemo)(()=>{var n;return null!=(n=(0,r._getInstance)(t.sdkKey))?n:e(t)},[]),[a,s]=(0,i.useState)("Ready"!==n.loadingStatus);return(0,i.useMemo)(()=>{"Ready"!==n.loadingStatus&&n.initializeAsync().catch(r.Log.error).finally(()=>s(!1))},[]),{client:n,isLoading:a}}';
  const patchedAsync = patch(asyncInitSource, url);
  assert.ok(patchedAsync?.includes("/*codey-statsig-async-init-cap*/"));
  assert.equal(patch(patchedAsync, url), null, "async init cap patch is idempotent");

  const runAsyncFactory = ({ resolveImmediately }) => {
    let resolveInit;
    const finalizedWith = [];
    const loadingChanges = [];
    const timers = new Map();
    const clearedTimers = [];
    let nextTimerId = 1;
    const client = {
      loadingStatus: "Loading",
      initializeAsync: () =>
        new Promise((resolve) => {
          resolveInit = () => {
            client.loadingStatus = "Ready";
            resolve({ success: true });
          };
          if (resolveImmediately) resolveInit();
        }),
      _finalizeUpdate(values) {
        finalizedWith.push(values);
        client.loadingStatus = "Ready";
      },
    };
    const api = Function(
      "client",
      "i",
      "r",
      "setTimeout",
      "clearTimeout",
      `${patchedAsync};return t.useStatsigInternalClientFactoryAsync(()=>client,{sdkKey:"k"});`,
    )(
      client,
      {
        useMemo: (fn) => fn(),
        useState: (initial) => [
          initial,
          (next) => {
            loadingChanges.push(next);
          },
        ],
      },
      {
        _getInstance: () => null,
        Log: { error() {} },
      },
      (fn, ms) => {
        const id = nextTimerId++;
        timers.set(id, { fn, ms });
        return id;
      },
      (id) => {
        clearedTimers.push(id);
        timers.delete(id);
      },
    );
    return { api, client, finalizedWith, loadingChanges, timers, clearedTimers, resolveInit: () => resolveInit() };
  };

  const fast = runAsyncFactory({ resolveImmediately: true });
  assert.equal(fast.api.client, fast.client);
  assert.equal(fast.api.isLoading, true);
  await Promise.resolve();
  await Promise.resolve();
  assert.deepEqual(fast.finalizedWith, []);
  assert.deepEqual(fast.loadingChanges, [false]);
  assert.equal(fast.timers.size, 0);
  assert.deepEqual(fast.clearedTimers, [1]);

  const stalled = runAsyncFactory({ resolveImmediately: false });
  assert.equal(stalled.timers.get(1)?.ms, 3000);
  assert.deepEqual(stalled.finalizedWith, []);
  assert.deepEqual(stalled.loadingChanges, []);
  stalled.timers.get(1).fn();
  assert.deepEqual(stalled.finalizedWith, [null]);
  assert.equal(stalled.client.loadingStatus, "Ready");
  assert.deepEqual(stalled.loadingChanges, [false]);
  stalled.resolveInit();
  await Promise.resolve();
  await Promise.resolve();
  assert.deepEqual(stalled.finalizedWith, [null]);
  assert.deepEqual(stalled.loadingChanges, [false]);

  assert.deepEqual(logs, []);
});

test("incompatible optional renderer patches log anchor excerpts once without blocking responses", async () => {
  const { logs, patch } = await installPatch();
  const patchErrors = [];
  const nativeConsoleError = console.error;
  console.error = (...args) => {
    patchErrors.push(args);
  };
  try {
    const incompatibleSource = [
      "supportsIndependentSpeedModes",
      "ultrafast_mode",
      'case"electron-app-state-snapshot-request":',
      "collectAppStateSnapshot(",
    ].join(" ");
    const url = "app://-/assets/async/49792.d8d33ace73.js";
    assert.equal(patch(incompatibleSource, url), null);
    assert.equal(patchErrors.length, 2);
    for (const [message] of patchErrors) {
      assert.match(String(message), /incompatible Codex renderer patch/);
    }
    assert.deepEqual(
      logs.map(({ operation }) => operation),
      [
        "renderer_patch:service tier access",
        "renderer_patch:app state heartbeat snapshot",
      ],
    );
    for (const record of logs) {
      assert.equal(record.context.matchCount, 0);
      assert.ok(
        Array.isArray(record.context.excerpts)
          && record.context.excerpts.length > 0
          && record.context.excerpts.every((excerpt) => excerpt.length > 0),
        `${record.operation} must carry anchor excerpts for field diagnosis`,
      );
      assert.match(
        record.context.excerpts.join("\n"),
        /supportsIndependentSpeedModes|electron-app-state-snapshot-request/,
      );
    }

    assert.equal(patch(incompatibleSource, url), null);
    assert.equal(patchErrors.length, 2, "the same incompatible source must not rerun failed renderer gates");
    assert.equal(logs.length, 2, "the same incompatible source must not report the same gates again");

    // Non-numeric chunk URLs pass through untouched.
    assert.equal(
      patch(incompatibleSource, "app://-/webview/assets/unrelated-feature-6f60e9ac4103.js"),
      null,
    );
    assert.equal(logs.length, 2);
  } finally {
    console.error = nativeConsoleError;
  }
});

test("hosted composer sends pass through the portable handoff gate", async () => {
  const { logs, patch } = await installPatch();
  const followUp = "async function f(e,n,i,o){let l=d(e,n),m=null;return f=await l.sendFollowUpMessage(i,{...o,useUserMessageAdmission:c(e,n),writingBlockContextPrepared:m!=null}),f}";
  const turnStart = "async function f(e,n,r,a){let c=d(e,n);return await c.startTurn(r,{...a,context:{...a.context,onRequestDispatched:a.context?.onRequestDispatched},request:{...a.request}})}async function p(e,n,r,a){let s=d(e,n);await s.startEmptyTurn(r,a)}";
  const cases = [
    ["app://-/assets/async/49792.d8d33ace73.js", followUp, "sendFollowUpMessage", { prompt: "hi" }],
    ["app://-/assets/async/102734.cdd4b920b9.js", turnStart, "startTurn", { request: { input: [] } }],
  ];
  for (const [url, source, method, options] of cases) {
    const patched = patch(source, url);
    assert.ok(patched?.includes(`"${method}",`), method);
    assert.equal(patch(patched, url), null, `${method} patch is idempotent`);
    const calls = [];
    const rpc = { [method]: async (...args) => { calls.push(["native", ...args]); return "sent"; } };
    const run = Function("d", "c", `${patched};return f`)(() => rpc, () => true);
    delete globalThis.__codeyModelWhitelistPatch;
    assert.equal(await run({}, "local", "thread", options), "sent", `${method} works without the gate`);
    globalThis.__codeyModelWhitelistPatch = {
      hostedTurnCall: async (target, name, threadId, value) => {
        calls.push(["gate", name, threadId]);
        return target[name](threadId, value);
      },
    };
    try {
      assert.equal(await run({}, "local", "thread", options), "sent");
    } finally {
      delete globalThis.__codeyModelWhitelistPatch;
    }
    assert.deepEqual(
      calls.map(([kind, name]) => (kind === "gate" ? `gate:${name}` : kind)),
      ["native", `gate:${method}`, "native"],
    );
  }
  const lock = "function t(e){return!r.has(e)&&(r.add(e),!0)}";
  assert.equal(patch(lock, "app://-/assets/async/999999.be4319076981.js"), null);
  assert.deepEqual(logs, []);
});

test("rspack 26.1120.50957 patches fix breadcrumb ResizeObserver loop, turn-diff telemetry, customAvatars slimming, paste cap, project sort, Bluetooth dictation, browser sidebar disposal, and question auto-resolve", async () => {
  const { logs, patch } = await installPatch();
  const mainChunkUrl = "app://-/assets/async/49792.d8d33ace73.js";

  // (a) File breadcrumb ResizeObserver subpixel guard + post-recovery showRoot (#62669)
  const breadcrumbSource = [
    'const label="review.fileSource.breadcrumb.ariaLabel";',
    "function compute(avail,trailWidths,ellipsisW,sepW,zoom){",
    "let e={currentCSSZoom:zoom,style:{paddingInlineEnd:\"\"}};",
    "let state={first:0,showRoot:!1,empty:!1};",
    "let i=10,a=avail,s=[...trailWidths,ellipsisW,sepW],[o,l]=s.splice(-2),u=s.at(-1)??0,d=a<Math.min(u,l),c=a<s.reduce((x,y)=>x+y,0)?s.length-1:0,p=a-u-o,h=c>0&&p>=0;for(;c>0&&p>=Math.min(s[c-1],o+l);)c-=1,p-=s[c];let f=`${i/(e.currentCSSZoom||1)}px`;e.style.paddingInlineEnd!==f&&(e.style.paddingInlineEnd=f),(t=>{state=t(state)})(e=>e.first===c&&e.showRoot===h&&e.empty===d?e:{first:c,showRoot:h,empty:d});",
    "return state}",
  ].join("");
  const patchedBreadcrumb = patch(breadcrumbSource, mainChunkUrl);
  assert.ok(patchedBreadcrumb?.includes("/*codey-breadcrumb-subpixel-guard*/"));
  assert.equal(patch(patchedBreadcrumb, mainChunkUrl), null);
  const computeBreadcrumb = Function(`${patchedBreadcrumb};return compute`)();
  assert.deepEqual(
    computeBreadcrumb(99.8, [40, 60], 12, 8, 1),
    { first: 0, showRoot: false, empty: false },
    "0.2px subpixel rounding difference does not trigger false overflow",
  );
  assert.deepEqual(
    computeBreadcrumb(84, [40, 45], 12, 8, 1),
    { first: 0, showRoot: false, empty: false },
    "when recovery loop restores all segments (c===0), showRoot must be false",
  );
  assert.deepEqual(
    computeBreadcrumb(70, [35, 30, 30], 12, 8, 1),
    { first: 1, showRoot: false, empty: false },
    "when restored segment borrows ellipsis space (p < 0), showRoot must turn off to prevent overflow",
  );
  assert.deepEqual(
    computeBreadcrumb(75, [35, 30, 30], 12, 8, 1),
    { first: 1, showRoot: true, empty: false },
    "genuinely truncated path with room for ellipsis displays showRoot",
  );

  // (c) Turn diff analytics full-repo Git snapshot short-circuit (#54955)
  const turnDiffSource = [
    "async function runTurnDiff(gitReq,visStart,visDone,analytics,retain){",
    'let ctx={capture:async function(e,t,n,r,i,a){try{return await (0,gitReq.V)("git").request({method:"turn-diff-capture-start",params:{baseTurnId:i,checkpointKey:r,cwd:n,hostConfig:t,operationSource:"turn_diff_analytics",turnId:a}})}catch(e){return console.warn("Failed to start turn diff analytics capture",{safe:{},sensitive:{error:e}}),null}}(1,2,"/repo","k","b","t1"),visualizationSnapshot:(async function(e,t){return await visStart()})(1,2).catch(e=>(console.warn("Failed to start visualization turn diff snapshot",{safe:{},sensitive:{error:e}}),null))};',
    'let[u,c]=await Promise.all([ctx.capture,ctx.visualizationSnapshot]);let[p,h]=await Promise.all([(0,gitReq.V)("git").request({method:"turn-diff-capture-complete",params:{capture:u??"forced",hostConfig:2,operationSource:"turn_diff_analytics",retainCheckpoint:retain()}}),null==c||!retain()?null:(async function(e,t,n=null,r){return await visDone()})(1,c).catch(e=>(console.warn("Failed to complete visualization turn diff snapshot",{safe:{},sensitive:{error:e}}),null))]);if(!retain())return"not-retained";if("error"===p.diff.type)return analytics();return"sent"}',
  ].join("");
  const patchedTurnDiff = patch(turnDiffSource, mainChunkUrl);
  assert.ok(patchedTurnDiff?.includes("/*codey-turn-diff-analytics-skip*/"));
  assert.ok(patchedTurnDiff?.includes("/*codey-turn-diff-complete-skip*/"));
  assert.equal(patch(patchedTurnDiff, mainChunkUrl), null);
  let gitCalls = 0;
  let visCalls = 0;
  const runTurnDiff = Function(`${patchedTurnDiff};return runTurnDiff`)();
  await runTurnDiff(
    { V: () => ({ request: async () => { gitCalls += 1; return { diff: { type: "ok" } }; } }) },
    async () => { visCalls += 1; return {}; },
    async () => { visCalls += 1; return "diff"; },
    () => { gitCalls += 1; },
    () => true,
  );
  assert.equal(gitCalls, 0);
  assert.equal(visCalls, 0);

  // (d) Slim pet customAvatars.load() short-circuit (#61498)
  const avatarsSource =
    'function buildQuery(r,i,a,s){let o=["custom-avatars"];return r(a,()=>({queryKey:o,queryFn:()=>i.appServices.customAvatars.load(),enabled:!0,networkMode:"always",refetchOnMount:!1,refetchOnWindowFocus:!1,staleTime:s}))}';
  const patchedAvatars = patch(avatarsSource, mainChunkUrl);
  assert.ok(patchedAvatars?.includes("/*codey-custom-avatars-slim-skip*/"));
  assert.equal(patch(patchedAvatars, mainChunkUrl), null);
  let rpcLoads = 0;
  const queryDef = Function(`${patchedAvatars};return buildQuery`)()(
    (_atom, factory) => factory(),
    { appServices: { customAvatars: { load: async () => { rpcLoads += 1; return [{ id: "custom:1" }]; } } } },
    null,
    Infinity,
  );
  try {
    globalThis.__codeyPetControlShield = { enabled: true };
    assert.deepEqual(await queryDef.queryFn(), []);
    assert.equal(rpcLoads, 0);
    globalThis.__codeyPetControlShield = { enabled: false };
    assert.deepEqual(await queryDef.queryFn(), [{ id: "custom:1" }]);
    assert.equal(rpcLoads, 1);
  } finally {
    delete globalThis.__codeyPetControlShield;
  }

  // (e) Paste attachment threshold 5,000 -> 500,000 chars (#25144)
  const pasteUrl = "app://-/assets/async/976185.d6427dfaa7.js";
  const pasteSource = "const exports={hqW:()=>1,huD:()=>5e3,i0d:()=>2};";
  const patchedPaste = patch(pasteSource, pasteUrl);
  assert.ok(patchedPaste?.includes("/*codey-paste-attachment-threshold*/5e5"));
  assert.equal(patch(patchedPaste, pasteUrl), null);
  assert.equal(Function(`${patchedPaste};return exports.huD()`)(), 500_000);

  // (f) Sidebar projectGroups updated_at recency sort (#31836)
  const projectSortSource = [
    "const k='SIDEBAR_PROJECT_THREAD_ORDERS',modeKey='chatgpt-work-flat-project-sidebar-mode-v2';",
    "function computeGroups(groups,recencyMap,U,x){",
    "const F=(key)=>recencyMap[key]??0,z=({getRecencyAt,items,sortMode})=>sortMode==='manual'?items.map(i=>i.key):[...items].sort((a,b)=>getRecencyAt(b.key)-getRecencyAt(a.key)).map(i=>i.key),R={vJ:(_old,next)=>next};",
    "let er=groups,ei=[];",
    'er=er.map(e=>{let t=z({getRecencyAt:F,items:e.threadKeys.map(k=>({key:k})),manualThreadIds:[],sortMode:U}),n=(0,R.vJ)(e.threadKeys,t);return n===e.threadKeys?e:{...e,threadKeys:n}}),"manual"===x&&(ei=[]);',
    "return{pinnedProjectThreadKeys:[],projectSortMode:U,projectGroups:er}}",
  ].join("");
  const patchedProjectSort = patch(projectSortSource, mainChunkUrl);
  assert.ok(patchedProjectSort?.includes("/*codey-project-groups-updated-sort*/"));
  assert.equal(patch(patchedProjectSort, mainChunkUrl), null);
  const computeGroups = Function(`${patchedProjectSort};return computeGroups`)();
  const sampleGroups = [
    { projectId: "old", threadKeys: ["t1"] },
    { projectId: "empty", threadKeys: [] },
    { projectId: "newest", threadKeys: ["t2", "t3"] },
  ];
  assert.deepEqual(
    computeGroups(sampleGroups, { t1: 100, t2: 50, t3: 900 }, "updated_at", "updated_at").projectGroups.map((g) => g.projectId),
    ["newest", "old", "empty"],
  );
  assert.deepEqual(
    computeGroups(sampleGroups, { t1: 100, t2: 50, t3: 900 }, "manual", "manual").projectGroups.map((g) => g.projectId),
    ["old", "empty", "newest"],
  );

  // (g.1) AirPods dictation clock=stopped fallback (#63267)
  const btUrl = "app://-/assets/async/919295.2d97f3afdf.js";
  const btSource = [
    'const msg="Microphone input ended";',
    "async function checkFrame(clockFn,E,e,T,t){",
    "let R=1,g=2,d=!1,o=clockFn,i,n={y:(p)=>p},p={signal:{}},f=()=>clockFn();",
    'if(R=g,g=R,d=!1,null!=o){let r=t;if(null==r)t=f(),d=!1;else if(e>=T){let[a,o]=await (0,n.y)(Promise.all([r,f()]),p.signal),s=a===o&&("recognized"===o||"hands_free"===o);d=s&&E>=3||"number"==typeof a&&"number"==typeof o&&o>a,i="number"==typeof o||"recognized"===o?o:void 0,t=Promise.resolve(o),s||(E=0),d||(T=e+50)}else d=!1}let A=E>0?"bluetooth_signal_ready":"bluetooth_silence_ready";return{d,E,A}}',
  ].join("");
  const patchedBt = patch(btSource, btUrl);
  assert.ok(patchedBt?.includes("/*codey-bluetooth-clock-stopped-guard*/"));
  assert.equal(patch(patchedBt, btUrl), null);
  const checkFrame = Function(`${patchedBt};return checkFrame`)();
  assert.equal((await checkFrame(async () => null, 3, 100, 0, Promise.resolve(null))).d, true, "clock=stopped (null) with 3 live signal frames resolves ready");
  assert.equal((await checkFrame(async () => null, 0, 100, 0, Promise.resolve(null))).d, false, "clock=stopped with pure silence still waits");
  assert.equal((await checkFrame(null, 3, 100, 0, null)).d, true, "missing native clock helper falls back to live frame streak");

  // (g.2) Conversation eviction disposes browser sidebar tabs (#61121)
  const disposeSource = [
    "class Sidebar{constructor(bus){this.removed=[];this.mediaRefreshed=0;bus.subscribe(\"browser-sidebar-destroy-webview\",()=>{})}removeConversationBrowserStateIfEmpty(e){this.ownerRoutePathsByConversation.delete(e)}removeConversationTabs(e){this.removed.push(e)}emitMediaChange(){this.mediaRefreshed+=1}}",
    'class Evictor{evictConversation(e,t){switch(t.reason){case"archive":case"archive-notification":break;case"delete":break;case"discard":break}}}',
  ].join("");
  const patchedDispose = patch(disposeSource, mainChunkUrl);
  assert.ok(patchedDispose?.includes("/*codey-browser-sidebar-dispose*/"));
  assert.equal(patch(patchedDispose, mainChunkUrl), null);
  try {
    const { Sidebar, Evictor } = Function(`${patchedDispose};return{Sidebar,Evictor}`)();
    const sidebar = new Sidebar({ subscribe() {} });
    const evictor = new Evictor();
    evictor.evictConversation("conv-del", { reason: "delete" });
    evictor.evictConversation("conv-arch", { reason: "archive" });
    evictor.evictConversation("conv-empty", { reason: "empty-thread" });
    assert.deepEqual(sidebar.removed, ["conv-del", "conv-arch"]);
    assert.equal(sidebar.mediaRefreshed, 2);
  } finally {
    delete globalThis.__codeyDisposeBrowserSidebarConversation;
  }

  // (g.3) AI question card autoResolve timeout snooze & state cap (#29702/#31651)
  const autoResolveSource = [
    "function mount(nh,tl,eY,t,atom){",
    "nh.appServices.owlBrowserCrashCounter.subscribe(()=>{});",
    "function applyBgXp(e,h,n){let r={conversationId:n.conversationId,hostId:h};switch(n.kind){case\"removed\":{let x=e.get(atom,r);x?.requestId===n.requestId&&e.set(atom,r,null);break}case\"updated\":e.set(atom,r,{requestId:n.requestId,resolutionState:n.resolutionState})}}",
    "tl.S=applyBgXp;",
    'return function handle(e){switch(e.type){case"request-user-input-auto-resolution-changed":if("timed-out"===e.change.kind){(0,eY.we)(t,{hostId:e.hostId,requestId:e.change.requestId},e.change.timeoutMs);break}(0,tl.S)(t,e.hostId,e.change),"removed"===e.change.kind&&(0,eY.cC)(t,{hostId:e.hostId,requestId:e.change.requestId});break;case"request-user-input-auto-resolution-snapshot":for(let{conversationId:n,requestId:r,resolutionState:i}of e.pendingRequests)(0,tl.S)(t,e.hostId,{kind:"updated",conversationId:n,requestId:r,resolutionState:i});break}}}',
  ].join("");
  const patchedAutoResolve = patch(autoResolveSource, mainChunkUrl);
  assert.ok(patchedAutoResolve?.includes("/*codey-user-input-auto-resolve-disable*/"));
  assert.equal(patch(patchedAutoResolve, mainChunkUrl), null);
  try {
    const snoozedCalls = [];
    const storeState = new Map();
    const store = {
      get: (_a, k) => storeState.get(`${k.hostId}:${k.conversationId}`) ?? null,
      set: (_a, k, v) => storeState.set(`${k.hostId}:${k.conversationId}`, v),
    };
    const handle = Function(`${patchedAutoResolve};return mount`)()(
      {
        appServices: {
          owlBrowserCrashCounter: { subscribe() {} },
          requestUserInputAutoResolution: { snooze: (arg) => snoozedCalls.push(arg) },
        },
      },
      {},
      { we() {}, cC() {} },
      store,
      "atom",
    );
    handle({
      type: "request-user-input-auto-resolution-changed",
      hostId: "local",
      change: {
        kind: "updated",
        conversationId: "conv-1",
        requestId: "req-1",
        resolutionState: { status: "scheduled", deadlineMs: 99999 },
      },
    });
    // Duplicate scheduled updates for the same request must not loop IPC snooze calls.
    handle({
      type: "request-user-input-auto-resolution-changed",
      hostId: "local",
      change: {
        kind: "updated",
        conversationId: "conv-1",
        requestId: "req-1",
        resolutionState: { status: "scheduled", deadlineMs: 99999 },
      },
    });
    assert.deepEqual(snoozedCalls, [{ conversationId: "conv-1", hostId: "local", requestId: "req-1" }]);
    assert.deepEqual(storeState.get("local:conv-1"), {
      requestId: "req-1",
      resolutionState: { status: "snoozed" },
    });
  } finally {
    delete globalThis.__codeyAppServices;
    delete globalThis.__codeySnoozedInputRequests;
  }

  assert.deepEqual(logs, []);
});
