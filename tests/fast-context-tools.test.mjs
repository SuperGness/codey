import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("user FastCtx blocks embedded tools across the backend and settings", async () => {
  const [
    appSource,
    appTypesSource,
    sectionsSource,
    configSource,
    commandSource,
    runtimeCommandSource,
    runtimeStatusPresentation,
  ] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/App.types.ts", root), "utf8"),
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readFile(new URL("backend/src/config.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("backend/src/commands/runtime.rs", root), "utf8"),
    loadTypeScriptModule(new URL("src/runtimeStatusPresentation.ts", root)),
  ]);
  const uiSource = `${appSource}\n${sectionsSource}`;

  assert.match(configSource, /pub fast_context_tools: bool/);
  assert.match(configSource, /fast_context_tools: false/);
  assert.match(appTypesSource, /export type FastContextToolsStatus = \{[\s\S]*userConfigured: boolean;[\s\S]*detectionFailed: boolean;[\s\S]*serverId\?: string;/);
  assert.match(commandSource, /fastContextToolsStatus/);
  assert.match(commandSource, /embedded_fast_context_tools_enabled\([\s\S]*config_input\.fast_context_tools/);
  assert.doesNotMatch(commandSource, /current_fast_context_tools_status\(\)\?/);
  assert.match(appSource, /setFastContextToolsStatus\([\s\S]*result\.fastContextToolsStatus/);
  assert.match(appSource, /fastContextToolsStatus=\{fastContextToolsStatus\}/);
  assert.match(runtimeCommandSource, /"fastContextToolsActive": fast_context_tools_active/);
  const [fastctxFeature] = runtimeStatusPresentation.buildEnabledOptimizationFeatures(
    { running: true, fastContextToolsActive: true },
    { userConfigured: false, detectionFailed: false },
  );
  assert.equal(fastctxFeature.id, "fastctx-context-tools");
  assert.equal(fastctxFeature.name, "FastCtx 上下文加速");
  assert.equal(
    fastctxFeature.detail,
    "Codey 内置 FastCtx 已随当前运行实例加载",
  );
  assert.match(uiSource, /const fastContextToolsEnabled =\s*config\.fastContextTools && !fastctxStatusBlocksEmbedded/);
  assert.match(uiSource, /checked=\{fastContextToolsEnabled\}/);
  assert.match(uiSource, /disabled=\{isBusy \|\| fastctxStatusBlocksEmbedded\}/);
  assert.match(uiSource, /aria-label="启用 FastCtx 上下文工具"/);
  assert.match(
    uiSource,
    /<Tooltip[\s\S]*content=\{fastctxBlockedReason\}[\s\S]*position="top"/,
  );
  assert.match(uiSource, /className="fastctx-disabled-switch-tooltip"[\s\S]*tabIndex=\{0\}/);
});

test("Codey keeps FastCtx in the dedicated sidecar", async () => {
  const [manifest, sidecarSource, mainSource, libSource, configSource, fastctxSource] = await Promise.all([
    readFile(new URL("backend/Cargo.toml", root), "utf8"),
    readFile(new URL("backend/src/bin/codey-fastctx.rs", root), "utf8"),
    readFile(new URL("backend/src/main.rs", root), "utf8"),
    readFile(new URL("backend/src/lib.rs", root), "utf8"),
    readFile(new URL("backend/src/codex_config.rs", root), "utf8"),
    readFile(new URL("backend/src/codex_config/fastctx.rs", root), "utf8"),
  ]);
  const configPatchSource = `${configSource}\n${fastctxSource}`;

  assert.match(manifest, /name = "codey-fastctx"/);
  assert.match(sidecarSource, /fastctx::cli::run_server/);
  assert.match(sidecarSource, /fastctx::cli::run\(\)/);
  assert.match(sidecarSource, /runtime-bootstrap/);
  assert.match(sidecarSource, /runtime-host/);
  // 主程序既不链接 FastCtx，也不再充当启动 sidecar 的兼容代理。
  assert.doesNotMatch(mainSource, /fastctx::/);
  assert.doesNotMatch(libSource, /fastctx::/);
  assert.doesNotMatch(mainSource, /--codey-fastctx-mcp/);
  assert.doesNotMatch(mainSource, /codey-fastctx(?:\.exe)?/);
  assert.match(configPatchSource, /--codey-fastctx-mcp/);
  assert.match(configPatchSource, /CODEY_FASTCTX_SERVER_ID: &str = "codey_fastctx"/);
  assert.match(configPatchSource, /CODEY_FASTCTX_NAMESPACE: &str = "mcp__codey_fastctx"/);
  assert.match(configPatchSource, /FASTCTX_TOKEN_BUDGET/);
  assert.match(configPatchSource, /configured_user_fastctx_server_id\(doc\)\.is_some\(\)[\s\S]*disable_fast_context_tools\(doc\);[\s\S]*return Ok\(None\)/);
});
