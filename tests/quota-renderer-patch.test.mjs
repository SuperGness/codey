import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const patchSource = await readFile(new URL("../backend/src/renderer_asset_patch.js", import.meta.url), "utf8");
const url = "app://-/assets/app-primary-test.js";
const marker = "/*codey-local-quota-gate*/";

// Keep the semantic anchors and data flow of the shipped composer, while
// making the quota and all other disabled conditions independently executable.
const fixture = `function render(model,host,quotaBlocked,otherDisabled){
const zt={settings:{model}},bt=host,XP=quotaBlocked,X=value=>value;
const dn={},Ib=()=>({}),G=null,Ht=false,Tv=()=>null;
let{agentMode:fn,permissionProfileId:pn,shouldSendPermissionOverrides:mn}=Ib(dn),hn=X(null),gn=G!=null&&(Ht||(zt.settings.model===\`gpt-reserve\`?hn!==G.model:zt.settings.model!==G.model)),_n=Tv(),vn=X(bt===\`local\`&&XP),yn=null;
const submission={activeCollaborationMode:zt,agentMode:fn,resolvedHostId:bt,useAsbDirect:false};
let Zn=otherDisabled||gn||vn,Qn;
const view={submitDisabled:Zn,onSubmitLocal:null};
return view.submitDisabled;
}
// codexComposerAdapter.CodexComposerAdapter.interruptThreadError
`;

// Codex 26.1029 numeric chunk: the model identity moved behind A.qz,
// the local quota hook uses a double-quoted host check, and submitDisabled ends
// with a semicolon. The remaining disabled conditions stay in their real place.
const numericChunkFixture = `function render(model,host,quotaBlocked,otherDisabled,hardBlocked=false){
const n1={settings:{model}},nB=host,R={U:quotaBlocked},i={Ul:value=>value,_U:()=>hardBlocked},ei={r9:()=>0},A={qz:"reserve-model"},tq={model};
const nO=(0,i._U)(A.F9,nB),tm=otherDisabled;
let{agentMode:os}={},ou=n1.settings.model!==tq.model&&(n1.settings.model===A.qz?n1.settings.model!==tq.model:n1.settings.model!==tq.model),om=(0,ei.r9)(),op=(0,i.Ul)("local"===nB&&R.U),nN=false,nA=false,nP=false,nK=false,ot=true,n3=false,or=null;
const submission={activeCollaborationMode:n1,agentMode:os,resolvedHostId:nB,useAsbDirect:false};
let oT=void 0!==tm&&tm||ou||nN||nA&&nP||nK||ot&&n3||or?.isLoading===!0||nO||op;
const view={submitDisabled:oT,onSubmitLocal:null};
return view.submitDisabled;
}
// codexComposerAdapter.CodexComposerAdapter.interruptThreadError
`;

function harness(helper) {
  const failures = [];
  const context = vm.createContext({
    URL,
    __codeyQuotaUnlock: helper,
    __codeyPatchLog: record => failures.push(JSON.parse(record)),
    console: { error() {} },
  });
  vm.runInContext(patchSource, context);
  return {
    context,
    failures,
    patch: (source, assetUrl = url) => context.__codeyRendererPatchEntry(assetUrl, source) ?? source,
  };
}

test("only confirmed local third-party quota can stop disabling submission", () => {
  const helper = { allowsLocalSend: ({ hostId, model }) => hostId === "local" && model === "third-party" };
  const h = harness(helper);
  vm.runInContext(h.patch(fixture), h.context);
  const render = h.context.render;
  assert.equal(render("third-party", "local", true, false), false);
  assert.equal(render("gpt-official", "local", true, false), true);
  assert.equal(render("unknown", "local", true, false), true);
  assert.equal(render("third-party", "local", true, true), true);
  assert.equal(render("gpt-official", "local", false, false), false);
  assert.equal(render("third-party", "remote", true, true), true);
  // Sequential calls use the current model and helper state, without caching.
  assert.equal(render("third-party", "local", true, false), false);
  assert.equal(render("gpt-official", "local", true, false), true);
  helper.allowsLocalSend = () => false;
  assert.equal(render("third-party", "local", true, false), true);
});

test("missing, throwing, and non-boolean helpers preserve the official quota condition", () => {
  for (const helper of [undefined, {}, { allowsLocalSend() { throw Error("unavailable"); } }, { allowsLocalSend: () => "true" }]) {
    const h = harness(helper);
    vm.runInContext(h.patch(fixture), h.context);
    assert.equal(h.context.render("third-party", "local", true, false), true);
  }
});

test("renamed minified variables still match the semantic anchors exactly once", () => {
  const renamed = fixture.replace(/\bzt\b/g, "selectedMode").replace(/\bbt\b/g, "resolvedHost").replace(/\bvn\b/g, "quotaDisabled").replace(/\bZn\b/g, "submitDisabledValue");
  const h = harness({ allowsLocalSend: () => true });
  const patched = h.patch(renamed);
  assert.equal(patched.split(marker).length - 1, 1);
  vm.runInContext(patched, h.context);
  assert.equal(h.context.render("third-party", "local", true, false), false);
  assert.equal(h.patch(patched), patched);
  assert.equal(h.patch(renamed), patched);
});

test("Codex 26.1021 composer adapter chunk passes a derived collaboration mode", () => {
  // The submission now receives J=H!=null&&cr&&!In?null:K while the quota
  // check still reads K.settings.model, so the mode binding may differ.
  const derived = fixture
    .replace("const submission=", "const activeMode=zt;const submission=")
    .replace("activeCollaborationMode:zt", "activeCollaborationMode:activeMode");
  const h = harness({ allowsLocalSend: ({ model }) => model === "third-party" });
  const patched = h.patch(derived, "app://-/assets/codex-composer-adapter-8f303ad0f240.js");
  assert.equal(patched.split(marker).length - 1, 1);
  vm.runInContext(patched, h.context);
  assert.equal(h.context.render("third-party", "local", true, false), false);
  assert.equal(h.context.render("gpt-official", "local", true, false), true);
  assert.deepEqual(h.failures, []);
});

test("Codex 26.1021 workspace hard block follows the same local third-party gate", () => {
  // Team workspaces without credits add a per-host hard block next to the
  // quota term; it previously kept third-party models disabled.
  const hardBlock = fixture
    .replace("function render(model,host,quotaBlocked,otherDisabled){", "function render(model,host,quotaBlocked,otherDisabled,hardBlocked=quotaBlocked){")
    .replace(",yn=null;", ",HB=(blocked,id)=>blocked,Rn=HB(hardBlocked,bt),yn=null;")
    .replace("||gn||vn,Qn", "||gn||Rn||vn,Qn");
  const hardMarker = "/*codey-local-hard-block-gate*/";
  const helper = { allowsLocalSend: ({ hostId, model }) => hostId === "local" && model === "third-party" };
  const h = harness(helper);
  const patched = h.patch(hardBlock, "app://-/assets/codex-composer-adapter-8f303ad0f240.js");
  assert.equal(patched.split(marker).length - 1, 1);
  assert.equal(patched.split(hardMarker).length - 1, 1);
  assert.equal(h.patch(patched), patched);
  vm.runInContext(patched, h.context);
  const render = h.context.render;
  assert.equal(render("third-party", "local", true, false), false);
  assert.equal(render("third-party", "local", false, false, true), false);
  assert.equal(render("gpt-official", "local", false, false, true), true);
  assert.equal(render("third-party", "remote", false, false, true), true);
  assert.equal(render("third-party", "local", false, true, true), true);
  assert.equal(render("gpt-official", "local", false, false, false), false);
  assert.deepEqual(h.failures, []);

  for (const unavailable of [undefined, { allowsLocalSend() { throw Error("unavailable"); } }]) {
    const fallback = harness(unavailable);
    vm.runInContext(fallback.patch(hardBlock), fallback.context);
    assert.equal(fallback.context.render("third-party", "local", false, false, true), true);
  }
});

test("Codex 26.1029 numeric composer chunk preserves quota and hard blocks", () => {
  const h = harness({ allowsLocalSend: ({ hostId, model }) => hostId === "local" && model === "third-party" });
  const numericUrl = "app://codex/assets/async/634303.855e57c3e7.js";
  const patched = h.patch(numericChunkFixture, numericUrl);
  const hardMarker = "/*codey-local-hard-block-gate*/";
  assert.equal(patched.split(marker).length - 1, 1);
  assert.equal(patched.split(hardMarker).length - 1, 1);
  assert.match(patched, /op=\(0,i\.Ul\)\("local"===nB&&R\.U\)&&\/\*codey-local-quota-gate\*\//);
  assert.equal(h.patch(patched, numericUrl), patched);
  vm.runInContext(patched, h.context);
  const render = h.context.render;
  assert.equal(render("third-party", "local", true, false), false);
  assert.equal(render("gpt-official", "local", true, false), true);
  assert.equal(render("unknown", "local", true, false), true);
  assert.equal(render("third-party", "remote", true, false), false);
  assert.equal(render("third-party", "local", true, true), true);
  assert.equal(render("gpt-official", "local", false, false, true), true);
  assert.equal(render("third-party", "local", false, false, true), false);
  assert.equal(render("third-party", "local", false, true, true), true);
  assert.equal(render("third-party", "remote", false, false, true), true);
  assert.equal(render("gpt-official", "local", false, false, false), false);
  assert.deepEqual(h.failures, []);
});

test("Codex 26.1103 split composer adapter and interrupt helper chunks patch without false failures", () => {
  const splitComposerChunk = numericChunkFixture.replace(
    "// codexComposerAdapter.CodexComposerAdapter.interruptThreadError",
    "const exports={CodexComposerAdapter:()=>render};",
  );
  const splitInterruptHelperChunk = `function interruptThread(n){
try{return n()}catch(e){return{id:"codexComposerAdapter.CodexComposerAdapter.interruptThreadError",error:e}}
}`;
  const h = harness({ allowsLocalSend: ({ hostId, model }) => hostId === "local" && model === "third-party" });
  const composerUrl = "app://-/assets/async/634303.fe5bdc3dcb.js";
  const helperUrl = "app://-/assets/async/182735.bb2cb0d2af.js";
  const patchedComposer = h.patch(splitComposerChunk, composerUrl);
  assert.equal(patchedComposer.split(marker).length - 1, 1);
  assert.equal(patchedComposer.split("/*codey-local-hard-block-gate*/").length - 1, 1);
  assert.equal(h.patch(splitInterruptHelperChunk, helperUrl), splitInterruptHelperChunk);
  vm.runInContext(patchedComposer, h.context);
  assert.equal(h.context.render("third-party", "local", true, false), false);
  assert.equal(h.context.render("third-party", "local", false, false, true), false);
  assert.equal(h.context.render("gpt-official", "local", true, false), true);
  assert.equal(h.context.render("gpt-official", "local", false, false, true), true);
  assert.deepEqual(h.failures, []);
});

test("numeric composer matching follows renamed bindings and verifies the host", () => {
  const renamed = numericChunkFixture
    .replace(/\bom\b/g, "workMode")
    .replace(/\bn1\b/g, "selectedMode")
    .replace(/\bnB\b/g, "resolvedHost")
    .replace(/\bqz\b/g, "reserveModel");
  const h = harness({ allowsLocalSend: () => true });
  vm.runInContext(h.patch(renamed), h.context);
  assert.equal(h.context.render("third-party", "local", true, false, true), false);
  for (const source of [numericChunkFixture + numericChunkFixture,
    numericChunkFixture.replace("resolvedHostId:nB", "resolvedHostId:anotherHost")]) {
    const incompatible = harness({ allowsLocalSend: () => true });
    assert.equal(incompatible.patch(source), source);
    assert.equal(incompatible.failures.length, 1);
  }
  for (const helper of [undefined, { allowsLocalSend() { throw Error("unavailable"); } }]) {
    const unavailable = harness(helper);
    vm.runInContext(unavailable.patch(numericChunkFixture), unavailable.context);
    assert.equal(unavailable.context.render("third-party", "local", true, false), true);
    assert.equal(unavailable.context.render("third-party", "local", false, false, true), true);
  }
});

test("duplicate or drifted quota gates are left unchanged and reported", () => {
  for (const [source, count] of [[fixture + fixture, 2], [fixture.replace("resolvedHostId:bt", "resolvedHostId:anotherHost"), 0], [fixture.replace("||vn,Qn", "||anotherQuota,Qn"), 0]]) {
    const h = harness();
    assert.equal(h.patch(source), source);
    assert.equal(h.failures.length, 1);
    assert.equal(h.failures[0].operation, "renderer_patch:local third-party composer quota");
    assert.equal(h.failures[0].context.matchCount, count);
  }
});

test("unrelated assets and non-Codex code are untouched", () => {
  const h = harness();
  assert.equal(h.patch(fixture, "app://-/assets/chatgpt-composer.js"), fixture);
  assert.equal(h.patch(fixture, "https://example.com/assets/app-primary-test.js"), fixture);
  const unrelated = fixture.replace("codexComposerAdapter.CodexComposerAdapter.interruptThreadError", "unrelated");
  assert.equal(h.patch(unrelated), unrelated);
  assert.equal(h.failures.length, 0);
});

test("numeric renderer URLs require a bounded hashed chunk name", () => {
  const sourceUrl = "app://-/assets/async/634303.855e57c3e7.js?cache=1";
  const accepted = harness({ allowsLocalSend: () => true });
  assert.notEqual(accepted.patch(numericChunkFixture, sourceUrl), numericChunkFixture);
  assert.notEqual(accepted.patch(numericChunkFixture, "app://-/assets/async/0513.ABCDEF01.js"), numericChunkFixture);
  assert.notEqual(accepted.patch(numericChunkFixture, "app://-/assets/634303.855e57c3e7.js"), numericChunkFixture);
  assert.notEqual(accepted.patch(numericChunkFixture, `app://-/assets/async/1234567890.${"a".repeat(32)}.js`), numericChunkFixture);

  for (const rejectedUrl of [
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
    assert.equal(h.patch(numericChunkFixture, rejectedUrl), numericChunkFixture);
  }
});
