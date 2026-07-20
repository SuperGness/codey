import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("FastCtx optimization is opt-in and exposed through the settings switch", async () => {
  const [appSource, configSource, commandSource] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("backend/src/config.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
  ]);

  assert.match(configSource, /pub fast_context_tools: bool/);
  assert.match(configSource, /fast_context_tools: false/);
  assert.match(commandSource, /config\.fast_context_tools = config_input\.fast_context_tools/);
  assert.match(appSource, /checked=\{config\.fastContextTools\}/);
  assert.match(appSource, /aria-label="启用 FastCtx 上下文工具"/);
  assert.match(appSource, /下次启动提供分页读取、搜索、文件发现与批量替换/);
});

test("Codey embeds FastCtx and only dispatches it through the dedicated MCP mode", async () => {
  const [manifest, runtimeSource, configPatchSource] = await Promise.all([
    readFile(new URL("backend/Cargo.toml", root), "utf8"),
    readFile(new URL("backend/src/lib.rs", root), "utf8"),
    readFile(new URL("backend/src/codex_config.rs", root), "utf8"),
  ]);

  assert.match(manifest, /fastctx = \{ git = "https:\/\/github\.com\/yc-duan\/fastctx", rev = "64a6a45f88e65a2c0305e36673fa5e3f99d95384", default-features = false \}/);
  assert.match(runtimeSource, /--codey-fastctx-mcp/);
  assert.match(runtimeSource, /fastctx::cli::run_server/);
  assert.match(configPatchSource, /CODEY_FASTCTX_SERVER_ID: &str = "codey_fastctx"/);
  assert.match(configPatchSource, /CODEY_FASTCTX_NAMESPACE: &str = "mcp__codey_fastctx"/);
  assert.match(configPatchSource, /FASTCTX_TOKEN_BUDGET/);
});
