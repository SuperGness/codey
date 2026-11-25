import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const patchSource = await readFile(new URL("../backend/src/renderer_asset_patch.js", import.meta.url), "utf8");
const url = "app://-/assets/async/76344.88a5c21495.js";
const marker = "/*codey-local-quota-gate*/";

// Codex 26.1120+ CodexComposerAdapter: usage-limit 与 reserve-limit 合并进共享
// 额度原子 nQ，经 React Compiler 缓存汇总为 submitDisabledReasons。
const composerFixture = `function createRender(){
const e7=Array(200).fill(Symbol.for("react.memo_cache_sentinel"));
return function render(model,host,quotaReason,otherDisabled=false){
let nK=host,ty=otherDisabled,nG=false,nH=false,ok=false,nz=false,nN=false,nE=false,n1=false,oc=false,on=false,og=null;
let nQ=quotaReason;
const{activeMode:n8,modelSettings:n9}={activeMode:{settings:{model}},modelSettings:null};
let{agentMode:ov}={agentMode:"default"};
const submission={activeCollaborationMode:n8,agentMode:ov,resolvedHostId:nK,useAsbDirect:false};
let e2;
let oF=void 0!==ty&&ty&&"caller-disabled",oW=nG&&"read-only-ssh-chat",oV=nH&&"provider-blocked",oK=ok&&"model-selection-unavailable",oG=nz&&"environment-busy",oH=nN&&nE&&"draft-tabs-restoring",oz=n1&&"policy-violation",oX=oc&&on&&"collaboration-mode-loading",oQ=og?.isLoading&&"service-tier-loading";e7[170]!==oF||e7[171]!==oW||e7[172]!==oV||e7[173]!==oK||e7[174]!==oG||e7[175]!==oH||e7[176]!==oz||e7[177]!==oX||e7[178]!==oQ||e7[179]!==nQ?(e2=[oF,oW,oV,oK,oG,oH,oz,oX,oQ,nQ].filter(Boolean),e7[170]=oF,e7[171]=oW,e7[172]=oV,e7[173]=oK,e7[174]=oG,e7[175]=oH,e7[176]=oz,e7[177]=oX,e7[178]=oQ,e7[179]=nQ,e7[180]=e2):e2=e7[180];let oJ=e2.join(",");
const view={submitDisabledReasons:oJ,onSubmitLocal:null};
return view.submitDisabledReasons;
};
}
// CodexComposerAdapter
`;

function harness(helper) {
  const failures = [];
  const context = vm.createContext({
    URL,
    __codeyQuotaUnlock: helper,
    __codeyPatchLog: (record) => failures.push(JSON.parse(record)),
    console: { error() {} },
  });
  vm.runInContext(patchSource, context);
  return {
    context,
    failures,
    patch: (source, assetUrl = url) => context.__codeyRendererPatchEntry(assetUrl, source) ?? source,
  };
}

test("only confirmed local third-party quota clears usage-limit and reserve-limit reasons", () => {
  const helper = { allowsLocalSend: ({ hostId, model }) => hostId === "local" && model === "third-party" };
  const h = harness(helper);
  const patched = h.patch(composerFixture);
  assert.equal(patched.split(marker).length - 1, 1);
  assert.equal(h.patch(patched), patched);
  assert.deepEqual(h.failures, []);

  vm.runInContext(patched, h.context);
  const render = h.context.createRender();
  assert.equal(render("third-party", "local", "usage-limit", false), "");
  assert.equal(render("third-party", "local", "reserve-limit", false), "");
  assert.equal(render("third-party", "local", "reserve-limit,usage-limit", false), "");
  assert.equal(render("third-party", "local", "usage-limit", true), "caller-disabled");
  assert.equal(render("gpt-official", "local", "usage-limit", false), "usage-limit");
  assert.equal(render("gpt-official", "local", "reserve-limit", false), "reserve-limit");
  assert.equal(render("gpt-official", "local", "reserve-limit,usage-limit", false), "reserve-limit,usage-limit");
  assert.equal(render("unknown", "local", "usage-limit", false), "usage-limit");
  assert.equal(render("third-party", "remote", "reserve-limit", false), "reserve-limit");
  assert.equal(render("gpt-official", "local", "", false), "");
  // React Compiler 缓存槽随模型切换自然失效，不复用上一次模型的放行判断。
  assert.equal(render("third-party", "local", "usage-limit", false), "");
  assert.equal(render("gpt-official", "local", "usage-limit", false), "usage-limit");
  helper.allowsLocalSend = () => false;
  assert.equal(render("third-party", "local", "usage-limit", false), "usage-limit");
});

test("missing, throwing, and non-boolean helpers preserve the official quota reasons", () => {
  for (const helper of [undefined, {}, { allowsLocalSend() { throw Error("unavailable"); } }, { allowsLocalSend: () => "true" }]) {
    const h = harness(helper);
    vm.runInContext(h.patch(composerFixture), h.context);
    const render = h.context.createRender();
    assert.equal(render("third-party", "local", "reserve-limit,usage-limit", false), "reserve-limit,usage-limit");
  }
});

test("renamed minified variables still match the semantic anchors exactly once", () => {
  const renamed = composerFixture
    .replace(/\bn8\b/g, "selectedMode")
    .replace(/\bnK\b/g, "resolvedHost")
    .replace(/\bnQ\b/g, "quotaReasons")
    .replace(/\boJ\b/g, "joinedReasons");
  const h = harness({ allowsLocalSend: ({ hostId, model }) => hostId === "local" && model === "third-party" });
  const patched = h.patch(renamed, "app://-/assets/async/914060.1e5b8f6646.js");
  assert.equal(patched.split(marker).length - 1, 1);
  assert.equal(h.patch(patched, "app://-/assets/async/914060.1e5b8f6646.js"), patched);
  vm.runInContext(patched, h.context);
  const render = h.context.createRender();
  assert.equal(render("third-party", "local", "usage-limit", false), "");
  assert.equal(render("gpt-official", "local", "usage-limit", false), "usage-limit");
  assert.deepEqual(h.failures, []);
});

test("duplicate or drifted quota gates are left unchanged and reported", () => {
  for (const [source, count] of [
    [composerFixture + composerFixture, 2],
    [composerFixture.replace("caller-disabled", "caller-other"), 0],
    [composerFixture.replace(".filter(Boolean)", ".map(Boolean)"), 0],
  ]) {
    const h = harness({ allowsLocalSend: () => true });
    assert.equal(h.patch(source), source);
    assert.equal(h.failures.length, 1);
    assert.equal(h.failures[0].operation, "renderer_patch:local third-party composer quota");
    assert.equal(h.failures[0].context.matchCount, count);
  }
});

test("unrelated assets and non-Codex code are untouched", () => {
  const h = harness();
  assert.equal(h.patch(composerFixture, "app://-/assets/chatgpt-composer.js"), composerFixture);
  assert.equal(h.patch(composerFixture, "https://example.com/assets/async/76344.88a5c21495.js"), composerFixture);
  const unrelated = composerFixture.replace("CodexComposerAdapter", "unrelated");
  assert.equal(h.patch(unrelated), unrelated);
  assert.equal(h.failures.length, 0);
});

test("numeric renderer URLs require a bounded hashed chunk name", () => {
  const accepted = harness({ allowsLocalSend: () => true });
  assert.notEqual(accepted.patch(composerFixture, "app://-/assets/async/634303.855e57c3e7.js?cache=1"), composerFixture);
  assert.notEqual(accepted.patch(composerFixture, "app://-/assets/async/0513.ABCDEF01.js"), composerFixture);
  assert.notEqual(accepted.patch(composerFixture, "app://-/assets/634303.855e57c3e7.js"), composerFixture);
  assert.notEqual(accepted.patch(composerFixture, `app://-/assets/async/1234567890.${"a".repeat(32)}.js`), composerFixture);

  for (const rejectedUrl of [
    "app://-/assets/app-primary-855e57c3e7.js",
    "app://-/assets/codex-composer-adapter-8f303ad0f240.js",
    "app://-/assets/async/composer.855e57c3e7.js",
    "app://-/assets/async/12345678901.855e57c3e7.js",
    "app://-/assets/async/634303.js",
    "app://-/assets/async/634303.abcdefg1.js",
    `app://-/assets/async/634303.${"a".repeat(33)}.js`,
    "app://-/assets/async/634303.855e57c3e7.cjs",
    "app://-/assets/async/634303.855e57c3e7.mjs",
    "app://-/assets/async/634303.not-a-hash.js",
    "app://-/assets/async/634303.855e57c3e7.js.map",
    "app://-/assets/634303.js",
    "app://-/webview/assets/async/634303.855e57c3e7.js",
    "app://-/assets/other/634303.855e57c3e7.js",
    "https://codex.example/assets/async/634303.855e57c3e7.js",
  ]) {
    const h = harness({ allowsLocalSend: () => true });
    assert.equal(h.patch(composerFixture, rejectedUrl), composerFixture);
  }
});
