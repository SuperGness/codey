import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadPatchExpression() {
  const template = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.js", import.meta.url),
    "utf8",
  ));
  assert.ok(template, "startup patch template should be readable by the regression test");
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__", "false");
}

test("Windows worker source signature cache is bounded", async () => {
  const source = await loadPatchExpression();

  assert.match(source, /maximumWmiWorkerSourceCacheEntries = 256/);
  assert.match(source, /const rememberWorkerSourceMatch = \(key, value\) =>/);
  assert.match(
    source,
    /workerSourceMatchCache\.size > maximumWmiWorkerSourceCacheEntries/,
  );
  assert.match(source, /workerSourceMatchCache\.delete\(oldestKey\)/);
  assert.match(source, /stats\.mtimeNs/);
  assert.match(source, /stats\.ctimeNs/);
});

async function withWindowsPlatform(run) {
  const descriptor = Object.getOwnPropertyDescriptor(process, "platform");
  assert.ok(descriptor?.configurable, "the Node test process platform should be configurable");
  Object.defineProperty(process, "platform", { ...descriptor, value: "win32" });
  try {
    await run();
  } finally {
    Object.defineProperty(process, "platform", descriptor);
  }
}

test("Windows lag patch bypasses only the recurring WMI snapshot worker", async () => {
  await withWindowsPlatform(async () => {
    const Module = process.getBuiltinModule("module");
    const workerThreads = process.getBuiltinModule("worker_threads");
    const originalLoad = Module._load;
    const NativeWorker = workerThreads.Worker;
    const esmWorkerThreads = await import("node:worker_threads");
    assert.equal(esmWorkerThreads.Worker, NativeWorker);
    const temporaryDirectory = await mkdtemp(
      join(tmpdir(), "codey-wmi-worker-"),
    );

    try {
      const expression = await loadPatchExpression();
      assert.equal((0, eval)(expression), "codey-startup-patch-installed-v37");
      const initialSampler =
        globalThis.__CODEY_CODEX_STARTUP_PATCH__.windowsWmiSampler;
      assert.equal(initialSampler.version, 4);
      assert.equal(initialSampler.selfTestPassed, true);
      assert.equal(initialSampler.selfTestError, "");
      assert.equal(initialSampler.blocked, 0);
      assert.equal(initialSampler.workersObserved, 0);

      const blocked = new workerThreads.Worker(
        "C:\\Codex\\resources\\app\\.vite\\build\\child-process-snapshot-worker.js",
        { workerData: 42 },
      );
      assert.equal(blocked.threadId, -1);
      assert.deepEqual((await once(blocked, "message"))[0], { type: "ok", value: [] });

      assert.notEqual(esmWorkerThreads.Worker, NativeWorker);
      const esmBlocked = new esmWorkerThreads.Worker(
        "C:\\Codex\\resources\\app\\.vite\\build\\child-process-snapshot-worker-esm.js",
      );
      assert.equal(esmBlocked.threadId, -1);
      assert.deepEqual((await once(esmBlocked, "message"))[0], {
        type: "ok",
        value: [],
      });

      const hashedKnownWorker = new workerThreads.Worker(
        new URL(
          "file:///C:/Codex/resources/app/.vite/build/child-process-snapshot-worker-A1B2.js?cache=1#worker",
        ),
      );
      assert.equal(hashedKnownWorker.threadId, -1);
      assert.deepEqual((await once(hashedKnownWorker, "message"))[0], {
        type: "ok",
        value: [],
      });

      const semanticNamedBlocked = new workerThreads.Worker(
        "C:\\Codex\\resources\\app\\.vite\\build\\src-A1B2.js",
        { name: "child-process-snapshot", workerData: 42 },
      );
      assert.equal(semanticNamedBlocked.threadId, -1);
      assert.deepEqual((await once(semanticNamedBlocked, "message"))[0], {
        type: "ok",
        value: [],
      });
      assert.equal(
        globalThis.__CODEY_CODEX_STARTUP_PATCH__.windowsWmiSampler.lastMatchReason,
        "worker-option-name",
      );

      const renamedWmiWorkerPath = join(
        temporaryDirectory,
        "process-telemetry-A1B2.mjs",
      );
      await writeFile(
        renamedWmiWorkerPath,
        [
          'import { parentPort } from "node:worker_threads";',
          'const executable = "powershell.exe";',
          'const command = "Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
          "parentPort.postMessage({ executable, command });",
        ].join("\n"),
      );
      const renamedBlocked = new workerThreads.Worker(
        pathToFileURL(renamedWmiWorkerPath),
      );
      assert.equal(renamedBlocked.threadId, -1);
      assert.deepEqual((await once(renamedBlocked, "message"))[0], {
        type: "ok",
        value: [],
      });

      const pwshWorkerPath = join(
        temporaryDirectory,
        "process-telemetry-pwsh.mjs",
      );
      await writeFile(
        pwshWorkerPath,
        [
          'import { parentPort } from "node:worker_threads";',
          'const executable = "pwsh.exe";',
          'const command = "Get-CimInstance Win32_Process Win32_PerfRawData_PerfProc_Process";',
          "parentPort.postMessage({ executable, command });",
        ].join("\n"),
      );
      const pwshBlocked = new workerThreads.Worker(pwshWorkerPath);
      assert.equal(pwshBlocked.threadId, -1);
      assert.deepEqual((await once(pwshBlocked, "message"))[0], {
        type: "ok",
        value: [],
      });

      const evalBlocked = new workerThreads.Worker(
        [
          'const { parentPort } = require("node:worker_threads");',
          'const executable = "powershell.exe";',
          'const command = "Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
          "parentPort.postMessage({ executable, command });",
        ].join("\n"),
        { eval: true },
      );
      assert.equal(evalBlocked.threadId, -1);
      assert.deepEqual((await once(evalBlocked, "message"))[0], {
        type: "ok",
        value: [],
      });

      const dataWorkerSource = [
        'import { parentPort } from "node:worker_threads";',
        'const executable = "powershell.exe";',
        'const command = "Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
        "parentPort.postMessage({ executable, command });",
      ].join("\n");
      const dataBlocked = new workerThreads.Worker(
        new URL(
          `data:text/javascript,${encodeURIComponent(dataWorkerSource)}`,
        ),
      );
      assert.equal(dataBlocked.threadId, -1);
      assert.deepEqual((await once(dataBlocked, "message"))[0], {
        type: "ok",
        value: [],
      });

      const harmlessWorkerPath = join(
        temporaryDirectory,
        "process-snapshot-helper.mjs",
      );
      await writeFile(
        harmlessWorkerPath,
        [
          'import { parentPort } from "node:worker_threads";',
          'parentPort.postMessage("harmless-worker-ran");',
        ].join("\n"),
      );
      const harmless = new workerThreads.Worker(harmlessWorkerPath);
      assert.equal((await once(harmless, "message"))[0], "harmless-worker-ran");
      await harmless.terminate();

      await writeFile(
        harmlessWorkerPath,
        [
          'import { parentPort } from "node:worker_threads";',
          'const executable = "powershell.exe";',
          'const command = "Get-WmiObject Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
          "parentPort.postMessage({ executable, command, replaced: true });",
        ].join("\n"),
      );
      const replacedAtSamePath = new workerThreads.Worker(harmlessWorkerPath);
      assert.equal(replacedAtSamePath.threadId, -1);
      assert.deepEqual((await once(replacedAtSamePath, "message"))[0], {
        type: "ok",
        value: [],
      });

      const normal = new workerThreads.Worker(
        'require("node:worker_threads").parentPort.postMessage("normal-worker-ran")',
        { eval: true, name: "child-process-snapshot-preview" },
      );
      assert.equal((await once(normal, "message"))[0], "normal-worker-ran");
      await normal.terminate();

      const sampler =
        globalThis.__CODEY_CODEX_STARTUP_PATCH__.windowsWmiSampler;
      assert.equal(sampler.installed, true);
      assert.equal(sampler.workerWrapperPatched, true);
      assert.equal(sampler.esmExportsSynchronized, true);
      assert.equal(sampler.selfTestPassed, true);
      assert.equal(sampler.blocked, 9);
      assert.equal(sampler.sourceSignatureMatches, 5);
      assert.equal(sampler.lastMatchReason, "source-signature");
      assert.equal(sampler.lastObservedWorkerName, "eval-worker");
      assert.equal(
        sampler.lastObservedThreadName,
        "child-process-snapshot-preview",
      );
      assert.deepEqual(sampler.lastObservedSourceSignals, ["workerMessaging"]);
    } finally {
      workerThreads.Worker = NativeWorker;
      Module.syncBuiltinESMExports?.();
      Module._load = originalLoad;
      await rm(temporaryDirectory, { recursive: true, force: true });
    }
  });
});

test("settings keeps Windows optimization checks without a standalone banner", async () => {
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
  assert.match(sectionsSource, /status\.clientPlatform === "windows"/);
  assert.doesNotMatch(sectionsSource, /Windows 优化补丁/);
  assert.doesNotMatch(sectionsSource, /windows-patch-status/);
  assert.doesNotMatch(sectionsSource, /windowsStartupPatchInstalled/);
  assert.doesNotMatch(sectionsSource, /windowsWmiSamplerConfirmed/);
  assert.doesNotMatch(sectionsSource, /script\.id === "windows-wmi-sampler"/);
  assert.match(
    sectionsSource,
    /const performanceError = maintenance\?\.performanceStatus === "error"/,
  );
  assert.match(
    sectionsSource,
    /const performanceDegraded = maintenance\?\.performanceStatus === "degraded"/,
  );
  assert.match(sectionsSource, /performanceDegraded[\s\S]*?"兼容模式"/);
  assert.doesNotMatch(sectionsSource, /windowsPatchReady/);
  assert.doesNotMatch(sectionsSource, /windowsPatchFailed/);
  assert.doesNotMatch(
    sectionsSource,
    /WMI 周期采样、临时 WebView 残留与执行环境泄漏修复已生效/,
  );
  assert.match(
    launcherSource,
    /WMI 周期采样保护等待运行时确认/,
  );
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
