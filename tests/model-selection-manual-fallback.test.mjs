import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

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
  assert.match(dialogSource, /modelState\.officialModels\.map/);
  assert.match(dialogSource, /placeholder="输入当前线路模型 ID/);
  assert.match(dialogSource, /当前线路支持 auto-review/);
  assert.match(dialogSource, /<Switch/);
  assert.match(dialogSource, /manualThirdPartyModelKeys\.has/);
  assert.match(dialogSource, /aria-label=\{`删除其他模型 \$\{model\}`\}/);
  assert.match(dialogSource, /onDeleteThirdPartyModel\(model\)/);
  assert.match(hookSource, /modelEditorState\.officialModelIds\.find/);
  assert.match(hookSource, /已在上方官方模型列表中，请直接勾选，不可重复输入/);
  assert.match(hookSource, /deleteDraftThirdPartyModel/);
  assert.doesNotMatch(hookSource, /fetchCurrentModels/);
  assert.doesNotMatch(hookSource, /deleteThirdPartyModel/);
  assert.doesNotMatch(hookSource, /setDefaultModel/);
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
