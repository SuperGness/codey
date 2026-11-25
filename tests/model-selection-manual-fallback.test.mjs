import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("bulk model selection includes every filtered page and preserves unrelated selections", async () => {
  const ids = await loadTypeScriptModule(new URL("src/modelIds.ts", root));
  const reasoningEfforts = await loadTypeScriptModule(new URL("src/modelReasoningEfforts.ts", root));
  const { filterModelOptions } = await loadTypeScriptModule(new URL("src/modelPickerPagination.ts", root));
  const state = [];
  let cursor = 0;
  const react = {
    useCallback: (callback) => callback,
    useMemo: (factory) => factory(),
    useRef: (initial) => ({ current: initial }),
    useState(initial) {
      const index = cursor++;
      if (!(index in state)) state[index] = initial;
      return [state[index], (value) => { state[index] = typeof value === "function" ? value(state[index]) : value; }];
    },
  };
  const source = await readFile(new URL("src/useModelSelection.ts", root), "utf8");
  const exports = {};
  new Function("require", "exports", ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  }).outputText)((name) => {
    if (name === "react") return react;
    if (name === "./modelIds") return ids;
    if (name === "./modelReasoningEfforts") return reasoningEfforts;
    if (name === "./subagentModels") return { buildSubagentModelOptions: () => [] };
    return {};
  }, exports);
  const render = () => {
    cursor = 0;
    return exports.useModelSelection({ config: null, currentProvider: null });
  };
  const upstream = Array.from({ length: 450 }, (_, index) => `provider-${index}`);
  render().openModelPicker({
    officialModels: [{ slug: "official", supported: true }], officialModelIds: ["official"],
    upstreamModels: upstream, thirdPartyModels: ["manual"], manualThirdPartyModels: ["manual"],
  });
  render().updateDraftReasoningEffort("provider-0", [{ level: "high", value: "high" }]);
  const matching = filterModelOptions(render().thirdPartyModelOptions, " PROVIDER- ");
  render().toggleDraftModel(matching, true);
  assert.equal(render().draftModelSet.size, 452);
  render().toggleDraftModel(["PROVIDER-0"], true);
  assert.equal(render().draftModelSet.size, 452);
  render().toggleDraftModel(matching, false);
  assert.deepEqual([...render().draftModelSet], ["official", "manual"]);
  assert.deepEqual(render().draftReasoningEfforts["provider-0"], [
    { level: "high", value: "high" },
  ]);
  assert.ok(render().draftManualThirdPartyModelKeys.has("manual"));
  render().toggleDraftModel("manual", false);
  assert.ok(!render().draftManualThirdPartyModelKeys.has("manual"));
});

test("model save notices distinguish delivery, pending restart and subagent errors", async () => {
  const { modelSelectionNotice } = await loadTypeScriptModule(new URL("src/modelSelectionNotice.ts", root));
  const summary = "已保存 3 个模型";
  for (const [result, tone, suffix] of [
    [{}, "success", ""],
    [{ modelHotReloaded: true }, "success", ""],
    [{ modelHotReloaded: true, modelHotReloadDeferred: true }, "success", ""],
    [{ modelHotReloaded: true, restartRequired: true }, "info", "，需重启 Codex 后生效"],
    [{ modelHotReloaded: true, modelHotReloadDeferred: true, restartRequired: true }, "info", "，需重启 Codex 后生效"],
    [{ modelHotReloaded: false, restartRequired: true }, "info", "，需重启 Codex 后生效"],
    [{ modelHotReloaded: false, modelHotReloadError: "CDP failed" }, "info", "；模型刷新失败，请重试同步"],
    [{ modelHotReloadError: "CDP failed", restartRequired: true }, "info", "；模型刷新失败，请重试同步；部分设置需重启 Codex 后生效"],
    [{ modelHotReloaded: false, subagentConfigHotReloadError: "reload failed" }, "info", "；子代理配置未能实时更新：reload failed"],
    [{ modelHotReloaded: true, subagentConfigHotReloadError: "reload failed" }, "info", "；子代理配置未能实时更新：reload failed"],
    [{ subagentConfigHealth: "pending_restart", restartRequired: true }, "info", "；当前 Codex 尚未注册新增的子代理角色，请重启一次以启用动态角色配置"],
    [{ subagentConfigHealth: "pending_restart", subagentConfigHotReloadError: "角色变化，等待重启" }, "info", "；角色变化，等待重启"],
    [{ subagentConfigHealth: "superseded", subagentConfigHotReloadError: "较新的配置已保存" }, "info", "；较新的配置已保存"],
    [{ modelHotReloaded: true, subagentConfigRepaired: true }, "success", ""],
    [{ modelHotReloaded: true, subagentConfigHotReloaded: true }, "success", ""],
  ]) {
    assert.deepEqual(modelSelectionNotice(result, summary), { tone, text: summary + suffix });
  }
  assert.deepEqual(
    modelSelectionNotice({ customContextsRestored: true, modelHotReloaded: true }, summary),
    {
      tone: "info",
      text: `${summary}；本机 Codex 模型缓存不完整，自定义上下文预算已恢复为默认值`,
    },
  );
  assert.deepEqual(
    modelSelectionNotice({ customContextsRestored: true, restartRequired: true }, summary),
    {
      tone: "info",
      text: `${summary}，需重启 Codex 后生效；本机 Codex 模型缓存不完整，自定义上下文预算已恢复为默认值`,
    },
  );
  assert.deepEqual(
    modelSelectionNotice({ customContextsRestored: false }, summary),
    { tone: "success", text: summary },
  );
});

test("model IDs compare case-insensitively while preserving first spelling", async () => {
  const {
    includesModelId,
    modelIdsEqual,
    modelKey,
    partitionModelIdsByKey,
    uniqueModelIds,
    withoutModelId,
  } = await loadTypeScriptModule(new URL("src/modelIds.ts", root));

  assert.equal(modelKey(" Provider-Coder "), "provider-coder");
  assert.equal(modelIdsEqual("Provider-Coder", " provider-coder "), true);
  assert.equal(
    includesModelId(["Provider-Coder", "Provider-Reasoner"], "PROVIDER-CODER"),
    true,
  );
  assert.deepEqual(
    uniqueModelIds([
      " Provider-Coder ",
      "provider-coder",
      "Provider-Reasoner",
      "",
    ]),
    ["Provider-Coder", "Provider-Reasoner"],
  );
  assert.deepEqual(
    withoutModelId(
      ["Provider-Coder", "Provider-Reasoner", "provider-coder"],
      " PROVIDER-CODER ",
    ),
    ["Provider-Reasoner"],
  );
  assert.deepEqual(
    partitionModelIdsByKey(
      ["Provider-Coder", "other-model", "PROVIDER-REASONER"],
      new Set(["provider-coder", "provider-reasoner"]),
    ),
    {
      matching: ["Provider-Coder", "PROVIDER-REASONER"],
      remaining: ["other-model"],
    },
  );
});

test("model picker filters case-insensitively and pages bounded results", async () => {
  const {
    MODEL_PICKER_PAGE_SIZE,
    filterModelOptions,
    nextVisibleModelCount,
    visibleModelOptions,
  } = await loadTypeScriptModule(new URL("src/modelPickerPagination.ts", root));
  const models = Array.from({ length: 450 }, (_, index) =>
    index % 2 === 0 ? `Provider-${index}` : `Other-${index}`
  );

  assert.equal(MODEL_PICKER_PAGE_SIZE, 200);
  assert.equal(filterModelOptions(models, "  PROVIDER-2  ")[0], "Provider-2");
  assert.equal(filterModelOptions(models, "provider").length, 225);
  assert.equal(filterModelOptions(models, ""), models);

  const firstPage = visibleModelOptions(models, MODEL_PICKER_PAGE_SIZE);
  assert.equal(firstPage.length, 200);
  assert.equal(nextVisibleModelCount(firstPage.length, models.length), 400);
  assert.equal(nextVisibleModelCount(400, models.length), 450);
  assert.equal(nextVisibleModelCount(450, models.length), 450);
});
