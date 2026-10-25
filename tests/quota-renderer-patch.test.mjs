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
