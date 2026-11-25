import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const patchSource = await readFile(
  new URL("../backend/src/renderer_asset_patch.js", import.meta.url),
  "utf8",
);
const assetUrl = "app://-/assets/async/76344.88a5c21495.js";
const quotaFixture = [
  "function render(model,host,quotaReason){",
  "let nK=host,ty=false,nQ=quotaReason;",
  "const{activeMode:n8,modelSettings:n9}={activeMode:{settings:{model}},modelSettings:null};",
  "let{agentMode:ov}={agentMode:\"default\"};",
  "const submission={activeCollaborationMode:n8,agentMode:ov,resolvedHostId:nK,useAsbDirect:false};",
  "let oF=void 0!==ty&&ty&&\"caller-disabled\";",
  "let e2=[oF,nQ].filter(Boolean);let oJ=e2.join(\",\");",
  "const view={submitDisabledReasons:oJ,onSubmitLocal:null};",
  "return view.submitDisabledReasons;",
  "}",
  "// CodexComposerAdapter",
  "",
].join("\n");

function loadPatch() {
  const maps = [];
  const logs = [];
  class TrackingMap extends Map {
    constructor(...args) {
      super(...args);
      maps.push(this);
    }
  }
  const context = vm.createContext({
    URL,
    Map: TrackingMap,
    __codeyPatchLog: (record) => logs.push(JSON.parse(record)),
    console: { error() {} },
  });
  vm.runInContext(patchSource, context);
  return {
    logs,
    maps,
    patch(source) {
      return context.__codeyRendererPatchEntry(assetUrl, source) ?? source;
    },
  };
}

test("large rewritten assets are not retained in the patch script maps", () => {
  const harness = loadPatch();
  const bodyMiB = 4;
  const filler = "x".repeat(bodyMiB * 1024 * 1024);
  const results = [];
  for (let version = 0; version < 4; version += 1) {
    const source = `${quotaFixture}const version=${version};const filler=${JSON.stringify(filler)};`;
    const patched = harness.patch(source);
    assert.notEqual(patched, source);
    assert.match(patched, /\/\*codey-local-quota-gate\*\//);
    results.push(patched);
  }

  assert.equal(harness.patch(results[0]), results[0]);
  assert.ok(harness.maps.length > 0);
  for (const map of harness.maps) {
    for (const value of map.values()) {
      assert.ok(
        typeof value !== "string" || value.length < 4096,
        "asset bodies must not be retained in a patch script map",
      );
    }
  }
  assert.deepEqual(harness.logs, []);
});

test("failed gate deduplication is isolated by source and repeated patches stay identical", () => {
  const harness = loadPatch();
  const incompatible = "supportsIndependentSpeedModes ultrafast_mode";
  assert.equal(harness.patch(incompatible), incompatible);
  assert.equal(harness.patch(incompatible), incompatible);
  assert.deepEqual(
    harness.logs.map(({ operation }) => operation),
    ["renderer_patch:service tier access"],
  );

  const changedSource = `${incompatible}\nconst changed=!0;`;
  assert.equal(harness.patch(changedSource), changedSource);
  assert.deepEqual(
    harness.logs.map(({ operation }) => operation),
    [
      "renderer_patch:service tier access",
      "renderer_patch:service tier access",
    ],
  );

  const first = harness.patch(quotaFixture);
  const second = harness.patch(quotaFixture);
  assert.equal(first, second);
  assert.match(first, /\/\*codey-local-quota-gate\*\//);
  assert.equal(harness.logs.length, 2);
});
