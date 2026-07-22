import assert from "node:assert/strict";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadPatchExpression() {
  const source = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.rs", import.meta.url),
    "utf8",
  ));
  const template = source.match(
    /const STARTUP_PATCH_TEMPLATE: &str = r#"\n([\s\S]*?)\n"#;/,
  )?.[1];
  assert.ok(template, "startup patch template should be readable by the regression test");
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__DISABLE_VOICE__", "false");
}

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

    try {
      const expression = await loadPatchExpression();
      assert.equal((0, eval)(expression), "codey-startup-patch-installed-v5");

      const blocked = new workerThreads.Worker(
        "C:\\Codex\\resources\\app\\.vite\\build\\child-process-snapshot-worker.js",
        { workerData: 42 },
      );
      assert.equal(blocked.threadId, -1);
      assert.deepEqual((await once(blocked, "message"))[0], { type: "ok", value: [] });

      const normal = new workerThreads.Worker(
        'require("node:worker_threads").parentPort.postMessage("normal-worker-ran")',
        { eval: true },
      );
      assert.equal((await once(normal, "message"))[0], "normal-worker-ran");
      await normal.terminate();
    } finally {
      workerThreads.Worker = NativeWorker;
      Module._load = originalLoad;
    }
  });
});

test("settings exposes the Windows optimization patch status only on Windows clients", async () => {
  const [sectionsSource, typesSource, commandsSource] = await Promise.all([
    readFile(new URL("../src/AppSections.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/App.types.ts", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8"),
  ]);

  assert.match(commandsSource, /"clientPlatform": current_update_platform\(\)/);
  assert.match(typesSource, /clientPlatform\?: string/);
  assert.match(sectionsSource, /status\.clientPlatform === "windows"/);
  assert.match(sectionsSource, /\{isWindowsClient && \(/);
  assert.match(sectionsSource, /Windows 优化补丁/);
  assert.match(sectionsSource, /maintenance\?\.performanceStatus === "ready"/);
  assert.match(sectionsSource, /windowsPatchReady[\s\S]*?"已启用"/);
  assert.match(sectionsSource, /windowsPatchFailed[\s\S]*?"未生效"/);
});

test("trace guard, stats, pet, and voice remain user-configurable", async () => {
  const [appSource, sectionsSource, configSource, traceSource, launcherSource, commandsSource] = await Promise.all([
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/AppSections.tsx", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/config.rs", import.meta.url), "utf8"),
    readFile(new URL("../src/TraceLogModule.tsx", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8"),
  ]);
  const uiSource = `${appSource}\n${sectionsSource}`;

  assert.doesNotMatch(uiSource, /disableCodexMicro/);
  assert.doesNotMatch(configSource, /pub disable_codex_micro/);
  assert.match(uiSource, /disableTraceLogWrites/);
  assert.match(configSource, /pub disable_trace_log_writes: bool/);
  assert.match(traceSource, /onProtectionChange|protectionEnabled/);
  assert.match(traceSource, /刷新统计/);
  assert.match(traceSource, /SSD 写入寿命粗略估算/);
  assert.match(traceSource, /统计范围：/);
  assert.match(traceSource, /已清理、轮转或覆盖的历史记录/);
  assert.match(traceSource, /REFERENCE_SSD_TBW_BYTES/);
  assert.match(traceSource, /MAX_WRITE_AMPLIFICATION/);
  assert.match(appSource, /refresh_trace_log_stats/);
  assert.match(commandsSource, /"refresh_trace_log_stats"/);
  assert.doesNotMatch(launcherSource, /spawn_startup_trace_stats_refresh/);
  assert.match(uiSource, /slimCodexPet/);
  assert.match(uiSource, /slimCodexVoice/);
  assert.match(configSource, /pub slim_codex_voice: bool/);
});
