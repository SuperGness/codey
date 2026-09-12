import assert from "node:assert/strict";
import { once } from "node:events";
import test from "node:test";
import vm from "node:vm";

import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

function evalStartupPatchInIsolatedProcess(source, envOverrides = {}) {
  const isolatedProcess = Object.create(process);
  isolatedProcess.env = { ...process.env };
  for (const [name, value] of Object.entries(envOverrides)) {
    if (value == null) delete isolatedProcess.env[name];
    else isolatedProcess.env[name] = value;
  }
  isolatedProcess.execArgv = process.execArgv.slice();
  isolatedProcess.argv = process.argv.slice();
  const context = {
    console,
    process: isolatedProcess,
    setImmediate,
    setTimeout,
    clearTimeout,
    Promise,
  };
  context.globalThis = context;
  return {
    process: isolatedProcess,
    result: vm.runInNewContext(source, context),
  };
}

test("main bundle detection accepts renamed CommonJS entry chunks by signature", async () => {
  const source = await loadStartupPatchTemplate();

  assert.match(source, /const hasMainBundleSignature =/);
  assert.match(source, /source\.includes\("checkout-webview-presentation-changed"\)/);
  assert.match(source, /source\.includes\("will-attach-webview"\)/);
  assert.match(source, /source\.includes\("did-attach-webview"\)/);
  assert.match(source, /\(\?:cjs\|js\)/);
  assert.match(source, /get mainBundleSourcePatch\(\)/);
});

test("startup patch preserves native child processes and ordinary BrowserWindows", async () => {
  const Module = process.getBuiltinModule("module");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const NativeWorker = workerThreads.Worker;
  const childProcessModule = process.getBuiltinModule("child_process");
  const nativeSpawn = childProcessModule.spawn;
  const platformDescriptor = Object.getOwnPropertyDescriptor(process, "platform");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  class FakeBrowserWindow {}
  const fakeElectron = { BrowserWindow: FakeBrowserWindow };
  const nativeChildSpawns = [];
  const fakeChildProcess = {
    spawn(command, args) {
      const child = { command, args, passedThrough: true };
      nativeChildSpawns.push(child);
      return child;
    },
    spawnSync(command, args) {
      nativeChildSpawns.push({ command, args, passedThrough: true });
      return { status: 17 };
    },
  };
  Module._load = function testElectronLoader(request) {
    if (request === "electron") return fakeElectron;
    if (request === "child_process" || request === "node:child_process") {
      return fakeChildProcess;
    }
    return Reflect.apply(nativeLoad, this, arguments);
  };

  try {
    Object.defineProperty(process, "platform", { ...platformDescriptor, value: "win32" });
    assert.equal(
      (0, eval)(await loadStartupPatchTemplate()),
      "codey-startup-patch-installed-v40",
    );

    const childProcess = Module._load("node:child_process", undefined, false);
    const bareMonitor = childProcess.spawn(
      "/Applications/Codex.app/Contents/Resources/native/bare-modifier-monitor",
      ["--key", "DoubleCommand"],
    );
    assert.equal(bareMonitor.passedThrough, true);
    assert.equal(nativeChildSpawns.length, 1);

    const releaseWatcher = childProcess.spawn("powershell.exe", [
      "-Command",
      "[CodexKeyboardState]::IsDown(17); GetAsyncKeyState",
    ]);
    assert.equal(releaseWatcher.passedThrough, true);
    assert.equal(nativeChildSpawns.length, 2);

    const electron = Module._load("electron", undefined, false);
    assert.ok(new electron.BrowserWindow({ title: "Settings" }) instanceof FakeBrowserWindow);
    const overlaySource = [
      "class AvatarOverlayManager{",
      "async prewarm(e){",
      "if(this.window!=null||this.openingWindowPromise!=null||this.isAppQuitting)return;",
      "let t=this.windowVisibilitySequence,n=await this.ensureWindow(t);",
      "n==null||t!==this.windowVisibilitySequence||this.positionWindow(n,e)}",
      "}",
    ].join("");
    assert.equal(
      globalThis.__CODEY_PATCH_CODEX_AVATAR_OVERLAY_PREWARM__(overlaySource),
      overlaySource,
    );
    const worker = new workerThreads.Worker([
      'const { parentPort } = require("node:worker_threads");',
      'const query = "powershell.exe Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
      'parentPort.postMessage({ executed: true, query });',
    ].join("\n"), { eval: true, name: "child-process-snapshot" });
    try {
      assert.equal(worker.threadId, -1);
      const [message] = await once(worker, "message");
      assert.deepEqual(message, { type: "ok", value: [] });
    } finally {
      await worker.terminate();
    }
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.windowsWmiSampler.lastMatchReason,
      "worker-option-name",
    );
  } finally {
    Object.defineProperty(process, "platform", platformDescriptor);
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
    childProcessModule.spawn = nativeSpawn;
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});

test("NODE_OPTIONS require path writes a marker and clears inherited options", async () => {
  const fs = process.getBuiltinModule("fs");
  const os = process.getBuiltinModule("os");
  const path = process.getBuiltinModule("path");
  const Module = process.getBuiltinModule("module");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  const NativeWorker = workerThreads.Worker;
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "codey-startup-require-"));
  const markerPath = path.join(tempDir, "marker.json");

  try {
    const isolated = evalStartupPatchInIsolatedProcess(
      await loadStartupPatchTemplate(),
      {
        NODE_OPTIONS: "--require=/tmp/codey-should-not-leak.js",
        CODEY_STARTUP_PATCH_MARKER: markerPath,
      },
    );
    assert.equal(isolated.result, "codey-startup-patch-installed-v40");
    assert.ok(!isolated.process.env.NODE_OPTIONS);
    assert.ok(!isolated.process.env.CODEY_STARTUP_PATCH_MARKER);
    const payload = JSON.parse(fs.readFileSync(markerPath, "utf8"));
    assert.equal(payload.status, "executed");
    assert.equal(payload.pid, process.pid);
    assert.equal(typeof payload.timestamp_ms, "number");
  } finally {
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("inspector eval does not clear NODE_OPTIONS without the require marker", async () => {
  const Module = process.getBuiltinModule("module");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  const NativeWorker = workerThreads.Worker;

  try {
    const isolated = evalStartupPatchInIsolatedProcess(
      await loadStartupPatchTemplate(),
      {
        NODE_OPTIONS: "--require=/tmp/codey-keep-node-options.js",
        CODEY_STARTUP_PATCH_MARKER: null,
      },
    );
    assert.equal(isolated.result, "codey-startup-patch-installed-v40");
    assert.equal(
      isolated.process.env.NODE_OPTIONS,
      "--require=/tmp/codey-keep-node-options.js",
    );
  } finally {
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});
