import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("bulk model selection includes every filtered page and preserves unrelated selections", async () => {
  const ids = await loadTypeScriptModule(new URL("src/modelIds.ts", root));
  const { filterModelOptions } = await loadTypeScriptModule(new URL("src/modelPickerPagination.ts", root));
  const state = [];
  let cursor = 0;
  const react = {
    useCallback: (callback) => callback,
    useMemo: (factory) => factory(),
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
  render().toggleDraft1MModel("provider-0", true);
  const matching = filterModelOptions(render().thirdPartyModelOptions, " PROVIDER- ");
  render().toggleDraftModel(matching, true);
  assert.equal(render().draftModelSet.size, 452);
  render().toggleDraftModel(["PROVIDER-0"], true);
  assert.equal(render().draftModelSet.size, 452);
  render().toggleDraftModel(matching, false);
  assert.deepEqual([...render().draftModelSet], ["official", "manual"]);
  assert.ok(render().draft1MModelSet.has("provider-0"));
  assert.ok(render().draftManualThirdPartyModelKeys.has("manual"));
  render().toggleDraftModel("manual", false);
  assert.ok(!render().draftManualThirdPartyModelKeys.has("manual"));
});

const readModelCommandSources = async () => {
  const dir = new URL("backend/src/commands/models/", root);
  const { readdir } = await import("node:fs/promises");
  const names = (await readdir(dir)).filter((name) => name.endsWith(".rs")).sort();
  const sources = await Promise.all(names.map((name) => readFile(new URL(name, dir), "utf8")));
  return sources.join("\n");
};

test("third-party model sync can fall back to manual model support configuration", async () => {
  const [dialogSource, hookSource, commandSource, modelCommandSource] = await Promise.all([
    readFile(new URL("src/AppDialogs.tsx", root), "utf8"),
    readFile(new URL("src/useModelSelection.ts", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readModelCommandSources(),
  ]);

  assert.match(dialogSource, /modelState\.officialModels\.length > 0/);
  assert.match(dialogSource, /本次官方账号登录可用的模型/);
  assert.match(dialogSource, /filteredOfficialModels\.map/);
  assert.match(dialogSource, /placeholder="搜索模型，或输入模型 ID 添加/);
  assert.match(dialogSource, /当前线路支持 auto-review/);
  assert.match(dialogSource, /<Switch/);
  assert.match(dialogSource, /manualThirdPartyModelKeys\.has/);
  assert.match(dialogSource, /aria-label=\{`删除其他模型 \$\{model\}`\}/);
  assert.match(dialogSource, /onDeleteThirdPartyModel\(model\)/);
  assert.match(hookSource, /modelEditorState\.officialModelIds\.find/);
  assert.match(hookSource, /已在上方官方模型列表中，请直接勾选，不可重复输入/);
  assert.match(hookSource, /deleteDraftThirdPartyModel/);
  assert.match(hookSource, /manualThirdPartyModels/);
  assert.match(hookSource, /supportsAutoReview/);
  assert.match(hookSource, /AUTO_REVIEW_MODEL.*线路能力/s);
  assert.match(hookSource, /deletedThirdPartyModels: deletedModels/);
  assert.match(
    hookSource,
    /"save_selected_models",\s*\{\s*officialModels,\s*thirdPartyModels,/,
  );
  assert.match(commandSource, /argument::<Vec<String>>\(&args, "officialModels"\)/);
  assert.match(commandSource, /argument::<Vec<String>>\(&args, "thirdPartyModels"\)/);
  assert.match(commandSource, /optional_argument::<Vec<String>>\(&args, "manualThirdPartyModels"\)/);
  assert.match(commandSource, /optional_argument::<Vec<String>>\(&args, "deletedThirdPartyModels"\)/);
  assert.match(commandSource, /optional_argument::<bool>\(&args, "supportsAutoReview"\)/);
  assert.match(
    modelCommandSource,
    /已在官方模型列表中，请直接勾选，不可作为其他模型手动添加/,
  );
  assert.match(modelCommandSource, /官方模型 \{model\} 不能作为其他模型删除/);
  assert.match(modelCommandSource, /不是手动添加的其他模型，不能删除/);
  assert.match(modelCommandSource, /validate_manual_third_party_model_sources/);
  assert.match(modelCommandSource, /validate_regular_route_model_list/);
  assert.match(modelCommandSource, /preserve_selected_third_party_models_except/);
  assert.match(
    modelCommandSource,
    /refreshed_model_state_async\(&config, false\)\.await\?/,
  );
  assert.match(modelCommandSource, /tokio::task::spawn_blocking/);
  assert.match(modelCommandSource, /rollback_model_catalog_after_config_save/);
  assert.match(
    modelCommandSource,
    /let model_catalog_fallback = catalog_refresh[\s\S]*?\.is_some_and\(\|refresh\| refresh\.fallback\)/,
  );
  assert.match(
    modelCommandSource,
    /"modelCatalogFallback":model_catalog_fallback/,
  );
  assert.match(
    modelCommandSource,
    /startup_model_sync_models_or_fallback\([\s\S]*saved_models/,
  );
  assert.match(modelCommandSource, /cdp::refresh_model_whitelist/);
  assert.match(modelCommandSource, /"modelHotReloaded"/);
  assert.match(modelCommandSource, /"modelHotReloadDeferred"/);
  assert.match(hookSource, /setNotice\(modelSelectionNotice\(result, summary\)\)/);
});

test("model save notices distinguish delivery, pending restart and subagent errors", async () => {
  const { modelSelectionNotice } = await loadTypeScriptModule(new URL("src/modelSelectionNotice.ts", root));
  const summary = "已保存 3 个当前线路模型";
  for (const [result, tone, suffix] of [
    [{}, "success", ""],
    [{ modelHotReloaded: true }, "success", "；Codex 模型列表已立即更新"],
    [{ modelHotReloaded: true, modelHotReloadDeferred: true }, "info", "；Codex 模型列表将在打开模型选择器时更新"],
    [{ modelHotReloaded: true, restartRequired: true }, "info", "；Codex 模型列表已立即更新；模型能力或其他设置需重启 Codex 后生效"],
    [{ modelHotReloaded: true, modelHotReloadDeferred: true, restartRequired: true }, "info", "；Codex 模型列表将在打开模型选择器时更新；模型能力或其他设置需重启 Codex 后生效"],
    [{ modelHotReloaded: false, restartRequired: true }, "info", "；线路运行配置需重启，Codex 模型列表将在重启后更新"],
    [{ modelHotReloaded: false, modelHotReloadError: "CDP failed" }, "info", "；Codex 模型列表刷新失败，重启 Codex 后生效"],
    [{ modelHotReloaded: false, subagentConfigHotReloadError: "reload failed" }, "info", "；子代理配置暂未能更新，重启 Codex 后生效"],
    [{ modelHotReloaded: true, subagentConfigHotReloadError: "reload failed" }, "info", "；Codex 模型列表已立即更新；子代理配置暂未能更新，重启 Codex 后生效"],
    [{ modelHotReloaded: true, subagentConfigRepaired: true }, "success", "；Codex 模型列表已立即更新；已校验并修复受影响的子代理运行配置"],
    [{ modelHotReloaded: true, subagentConfigHotReloaded: true }, "success", "；Codex 模型列表已立即更新；受影响的子代理角色也已同步"],
  ]) {
    assert.deepEqual(modelSelectionNotice(result, summary), { tone, text: summary + suffix });
  }
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
