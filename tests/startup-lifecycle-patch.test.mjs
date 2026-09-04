import assert from "node:assert/strict";
import { once } from "node:events";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadStartupPatchExpression() {
  const template = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.js", import.meta.url),
    "utf8",
  ));
  assert.ok(template);
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__", "false");
}

test("main bundle detection accepts renamed CommonJS entry chunks by signature", async () => {
  const source = await loadStartupPatchExpression();

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
      (0, eval)(await loadStartupPatchExpression()),
      "codey-startup-patch-installed-v38",
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
    const worker = new workerThreads.Worker([
      'const { parentPort } = require("node:worker_threads");',
      'const query = "powershell.exe Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
      'parentPort.postMessage({ executed: true, query });',
    ].join("\n"), { eval: true, name: "child-process-snapshot" });
    try {
      const [message] = await once(worker, "message");
      assert.equal(message.executed, true);
    } finally {
      await worker.terminate();
    }
  } finally {
    Object.defineProperty(process, "platform", platformDescriptor);
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
    childProcessModule.spawn = nativeSpawn;
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});
