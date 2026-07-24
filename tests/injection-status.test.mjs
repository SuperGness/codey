import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("script injection diagnostics report runtime evidence without continuous polling", async () => {
  const [cdp, launcher, commands, app, types, sections, pluginFix, styles] = await Promise.all([
    readFile(new URL("backend/src/cdp.rs", root), "utf8"),
    readFile(new URL("backend/src/launcher.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/App.types.ts", root), "utf8"),
    readFile(new URL("src/AppSections.tsx", root), "utf8"),
    readFile(new URL("public/plugin-marketplace-fix.js", root), "utf8"),
    readFile(new URL("src/styles.css", root), "utf8"),
  ]);

  assert.match(cdp, /window\.__codeyInjectionStatus/);
  assert.match(cdp, /MAX_INJECTION_ERROR_CHARS:\s*usize\s*=\s*500/);
  assert.match(cdp, /read_injection_statuses\(&websocket_url, scripts\)/);
  assert.match(launcher, /injection_statuses:\s*Arc<RwLock<Arc<\[cdp::InjectionScriptStatus\]>>>/);
  assert.match(launcher, /watchdog_injection_statuses\.write\(\)\.await/);
  assert.match(commands, /runtime\.injection_statuses\.read\(\)\.await\.clone\(\)/);
  assert.match(commands, /"injectionScripts"/);
  assert.match(commands, /"refresh_injection_status"\s*=>\s*refresh_injection_status/);
  assert.match(app, /invoke\("refresh_injection_status"\)/);
  assert.equal(app.match(/invoke\("refresh_injection_status"\)/g)?.length, 1);
  assert.match(app, /codey-injection-status-changed/);
  assert.match(app, /injectionStatusRefreshRef/);
  assert.match(cdp, /completedEntry\.status === \\"pending\\"/);
  assert.match(pluginFix, /markPluginBridgeEffective/);
  assert.match(pluginFix, /entry\.status = "effective"/);
  assert.match(pluginFix, /codey-injection-status-changed/);
  assert.match(types, /status:\s*"effective"\s*\|\s*"executed"\s*\|\s*"failed"\s*\|\s*"unknown"/);
  assert.match(sections, /脚本生效状态/);
  assert.match(sections, /生效探针通过/);
  assert.match(sections, /脚本已执行，但没有生效证据/);
  assert.match(sections, /Codex 启动后将记录每个脚本的注入结果/);
  assert.match(sections, /性能策略已生效：采样与泄漏修复/);
  assert.doesNotMatch(sections, /setInterval/);
  assert.match(styles, /text-wrap:\s*balance/);
  assert.match(styles, /word-break:\s*normal/);
  assert.match(cdp, /for \(const delay of \[50, 200\]\)/);
});
