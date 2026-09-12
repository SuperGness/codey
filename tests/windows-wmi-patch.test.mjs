import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import vm from "node:vm";

import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

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

test("Windows worker source signature cache is bounded", async () => {
  const source = await loadStartupPatchTemplate();
  assert.match(source, /maximumWmiWorkerSourceCacheEntries = 256/);
  assert.match(source, /const rememberWorkerSourceMatch = \(key, value\) =>/);
  assert.match(
    source,
    /workerSourceMatchCache\.size > maximumWmiWorkerSourceCacheEntries/,
  );
  assert.match(source, /workerSourceMatchCache\.delete\(oldestKey\)/);
  assert.match(source, /stats\.mtimeNs/);
  assert.match(source, /stats\.ctimeNs/);
  assert.match(source, /Win32_ComputerSystem/);
  assert.doesNotMatch(source, /__codeyWindowsWmiSamplerGuard/);
});

test("WMI inspection skips oversized files and reuses small worker results", async () => {
  const source = await loadStartupPatchTemplate();
  const limit = 2 * 1024 * 1024;
  const prefix = 'powershell Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process parentPort';
  const reads = [];
  const fakeFs = {
    statSync: (filename) => ({ dev: 1, ino: 1,
      size: filename.includes("large") ? limit * 20 : limit,
      mtimeNs: 1, ctimeNs: 1, isFile: () => !filename.endsWith("directory.cjs"),
    }),
    readFileSync(filename) {
      reads.push(filename);
      if (filename === "/small-worker.cjs") return prefix;
      throw new Error("read failed");
    },
  };
  class NativeWorker { threadId = 123; }
  const workerThreads = { Worker: NativeWorker };
  const Module = { _load() {}, _extensions: { ".js"() {} }, syncBuiltinESMExports() {} };
  const sandboxProcess = {
    ...process, platform: "win32", env: {}, execArgv: [], argv: [],
    getBuiltinModule(name) {
      if (name === "fs") return fakeFs;
      if (name === "module") return Module;
      if (name === "worker_threads") return workerThreads;
      if (name === "child_process") return { ...process.getBuiltinModule(name) };
      return process.getBuiltinModule(name);
    },
  };
  vm.runInNewContext(source, { process: sandboxProcess, Buffer, console, setTimeout, clearTimeout, setImmediate });
  reads.length = 0;
  assert.equal(new workerThreads.Worker("/large-worker.cjs").threadId, 123);
  assert.equal(new workerThreads.Worker("/app.asar/large-worker.cjs").threadId, 123);
  assert.equal(new workerThreads.Worker("/directory.cjs").threadId, 123);
  assert.equal(new workerThreads.Worker("/child-process-snapshot-worker-large.cjs").threadId, -1);
  assert.deepEqual(reads, []);
  assert.equal(new workerThreads.Worker("/small-worker.cjs").threadId, -1);
  assert.equal(new workerThreads.Worker("/small-worker.cjs").threadId, -1);
  assert.deepEqual(reads, ["/small-worker.cjs"]);
  assert.equal(new workerThreads.Worker("/unreadable-worker.cjs").threadId, 123);
  assert.deepEqual(reads, ["/small-worker.cjs", "/unreadable-worker.cjs"]);
});

test("Windows lag patch bypasses only the recurring WMI snapshot worker", async () => {
  await withWindowsPlatform(async () => {
    const Module = process.getBuiltinModule("module");
    const workerThreads = process.getBuiltinModule("worker_threads");
    const originalLoad = Module._load;
    const nativeJsExtension = Module._extensions[".js"];
    const NativeWorker = workerThreads.Worker;
    const esmWorkerThreads = await import("node:worker_threads");
    assert.equal(esmWorkerThreads.Worker, NativeWorker);
    const temporaryDirectory = await mkdtemp(
      join(tmpdir(), "codey-wmi-worker-"),
    );

    try {
      assert.equal(
        (0, eval)(await loadStartupPatchTemplate()),
        "codey-startup-patch-installed-v40",
      );
      const initialSampler =
        globalThis.__CODEY_CODEX_STARTUP_PATCH__.windowsWmiSampler;
      assert.equal(initialSampler.version, 4);
      assert.equal(initialSampler.enabled, true);
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

      // Current Codex also contains a ComputerSystem query. Merely mentioning
      // PowerShell/CIM must not identify it as the recurring process sampler.
      const systemInfo = new workerThreads.Worker(
        'const command = "powershell -NoProfile Get-CimInstance Win32_ComputerSystem"; require("node:worker_threads").parentPort.postMessage(command)',
        { eval: true },
      );
      assert.match((await once(systemInfo, "message"))[0], /Win32_ComputerSystem/);
      await systemInfo.terminate();

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
      Module._extensions[".js"] = nativeJsExtension;
      await rm(temporaryDirectory, { recursive: true, force: true });
    }
  });
});
