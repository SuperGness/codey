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
  assert.doesNotMatch(launcherSource, /pet_status\.status = "failed"/);
});
