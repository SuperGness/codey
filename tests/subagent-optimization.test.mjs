import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("subagent settings expose the three supported role controls", async () => {
  const [featurePolicySource, modelHookSource, modelOptionsSource, comboboxSource] = await Promise.all([
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readFile(new URL("src/useModelSelection.ts", root), "utf8"),
    readFile(new URL("src/subagentModels.ts", root), "utf8"),
    readFile(new URL("src/components/ModelCombobox.tsx", root), "utf8"),
  ]);

  assert.match(featurePolicySource, /checked=\{config\.subagentOptimization\}/);
  assert.match(
    featurePolicySource,
    /onCheckedChange=\{\(checked\) =>\s*onSubagentOptimizationChange\(checked\)\s*\}/,
  );
  for (const [id, name] of [
    ["codey_explorer", "检索"],
    ["codey_reviewer", "复核"],
    ["codey_worker", "开发"],
  ]) {
    assert.match(featurePolicySource, new RegExp(`id: "${id}"`));
    assert.match(featurePolicySource, new RegExp(`name: "${name}"`));
  }
  assert.match(featurePolicySource, /config\.subagentRoles\[task\.id\]/);
  assert.match(featurePolicySource, /checked=\{selection\.enabled\}/);
  assert.match(featurePolicySource, /onCheckedChange=\{\(enabled\) => updateRole\(\{ enabled \}\)\}/);
  assert.match(featurePolicySource, /各角色工具权限一致/);
  assert.doesNotMatch(featurePolicySource, /默认只读|默认可写|不产生写入操作/);
  assert.match(featurePolicySource, /roleDisabled/);
  assert.match(featurePolicySource, /kind: "analysis"/);
  assert.match(featurePolicySource, /kind: "implementation"/);
  assert.match(featurePolicySource, /<ModelCombobox/);
  assert.match(
    modelHookSource,
    /buildSubagentModelOptions\(\s*config,\s*modelState,\s*officialAccountAvailable/,
  );
  assert.match(modelHookSource, /officialAccountAvailable,\s*currentProvider/);
  assert.match(modelOptionsSource, /for \(const profile of config\.profiles\)/);
  assert.match(modelOptionsSource, /value: routeModelAlias\(profile, modelId\)/);
  assert.match(modelOptionsSource, /if \(!config\.localRouterEnabled\)/);
  assert.match(modelOptionsSource, /currentProvider\?\.id/);
  assert.match(modelOptionsSource, /value: modelId/);
  assert.match(modelOptionsSource, /: modelState\.thirdPartyModels/);
  assert.match(modelOptionsSource, /official && officialModelMetadata/);
  assert.match(
    modelOptionsSource,
    /THIRD_PARTY_REASONING_EFFORTS\s*=\s*\["low",\s*"medium",\s*"high",\s*"xhigh"\]/,
  );
  assert.match(
    modelOptionsSource,
    /THIRD_PARTY_REASONING_EFFORT_ALLOWLIST\s*=\s*\[\s*"low",\s*"medium",\s*"high",\s*"xhigh",\s*"max",\s*"ultra"/,
  );
  assert.match(modelOptionsSource, /modelState\.thirdPartyModelMetadata/);
  assert.match(modelOptionsSource, /resolveSubagentModelOption/);
  assert.match(comboboxSource, /<ComboBox/);
  assert.match(comboboxSource, /<ListBox\.Section/);
  assert.match(comboboxSource, /没有匹配的模型或线路/);
  assert.match(comboboxSource, /items=\{groups\}/);
});
