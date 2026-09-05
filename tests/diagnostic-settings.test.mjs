import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

test("settings reports startup health without exposing implementation modes", async () => {
  const [
    sectionsSource,
    typesSource,
    commandsSource,
    launcherRootSource,
    launcherProcessSource,
    runtimeStatusPresentation,
  ] = await Promise.all([
    readFile(new URL("../src/OperationsPanel.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/App.types.ts", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/commands/runtime.rs", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8"),
    readFile(
      new URL("../backend/src/launcher/process.rs", import.meta.url),
      "utf8",
    ),
    loadTypeScriptModule(
      new URL("../src/runtimeStatusPresentation.ts", import.meta.url),
    ),
  ]);
  const launcherSource = `${launcherRootSource}\n${launcherProcessSource}`;

  assert.match(commandsSource, /"clientPlatform": current_update_platform\(\)/);
  assert.doesNotMatch(commandsSource, /injection_statuses_for_display/);
  assert.match(typesSource, /clientPlatform\?: string/);
  assert.match(
    sectionsSource,
    /const performanceError = maintenance\?\.performanceStatus === "error"/,
  );
  assert.match(
    sectionsSource,
    /const startupNeedsAttention = maintenance\?\.performanceStatus === "degraded"/,
  );
  assert.match(sectionsSource, /startupNeedsAttention\s*\? "需检查"\s*: "正常"/);
  assert.doesNotMatch(sectionsSource, /兼容模式|主进程增强|已优化/);
  const failedSummary = runtimeStatusPresentation.summarizeInjectionScripts([
    {
      id: "windows-internal-failure",
      name: "Windows 内部保护",
      source: "builtin",
      visibility: "internal",
      status: "failed",
    },
  ]);
  assert.equal(failedSummary.internalInjectionError, true);
  assert.equal(failedSummary.failedInjectionScriptCount, 0);
  assert.doesNotMatch(sectionsSource, /injection-script-state/);
  assert.doesNotMatch(sectionsSource, /id: "opt-patch"/);
  assert.doesNotMatch(launcherSource, /fn mark_pet_slim_startup_failure/);
  assert.doesNotMatch(launcherSource, /pet_status\.status = "failed"/);
});

test("diagnostic storage guards and pet remain user-configurable", async () => {
  const [appSource, sectionsSource, configSource, traceSource, launcherSource, commandsSource] = await Promise.all([
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/FeaturePolicyCard.tsx", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/config.rs", import.meta.url), "utf8"),
    readFile(new URL("../src/TraceLogModule.tsx", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8"),
  ]);
  const uiSource = `${appSource}\n${sectionsSource}`;

  assert.match(uiSource, /disableTraceLogWrites/);
  assert.match(configSource, /pub disable_trace_log_writes: bool/);
  assert.match(uiSource, /protectCrashpadPending/);
  assert.match(configSource, /pub protect_crashpad_pending: bool/);
  assert.match(traceSource, /traceProtectionEnabled/);
  assert.match(traceSource, /crashpadProtectionEnabled/);
  assert.match(traceSource, /刷新统计/);
  assert.match(traceSource, /日志总条数/);
  assert.match(traceSource, /Trace 磁盘占用/);
  assert.match(traceSource, /内容字节估算/);
  assert.match(traceSource, /Crashpad 报告/);
  assert.match(traceSource, /Crashpad 占用/);
  assert.doesNotMatch(traceSource, /近 7 天写入/);
  assert.doesNotMatch(traceSource, /SSD 写入寿命粗略估算/);
  assert.doesNotMatch(traceSource, /级别分布|高占用 Targets/);
  assert.match(appSource, /refresh_diagnostic_storage_stats/);
  assert.match(appSource, /clear_diagnostic_storage/);
  assert.match(appSource, /crashpadPendingStats: result\.crashpadPendingStats/);
  assert.doesNotMatch(appSource, /可手动刷新统计/);
  assert.match(commandsSource, /"refresh_diagnostic_storage_stats"/);
  assert.match(commandsSource, /"clear_diagnostic_storage"/);
  assert.match(launcherSource, /spawn_crashpad_guard_watcher/);
  assert.doesNotMatch(launcherSource, /spawn_startup_trace_stats_refresh/);
  assert.match(uiSource, /slimCodexPet/);
});
