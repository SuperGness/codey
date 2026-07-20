import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

async function loadVoicePatchExpression() {
  const source = await readFile(
    new URL("../backend/src/codex_startup_patch.rs", import.meta.url),
    "utf8",
  );
  const template = source.match(
    /const STARTUP_PATCH_TEMPLATE: &str = r#"\n([\s\S]*?)\n"#;/,
  )?.[1];
  assert.ok(template);
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__DISABLE_VOICE__", "true");
}

test("voice startup patch blocks native listeners and Dictation windows", async () => {
  const Module = process.getBuiltinModule("module");
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
    assert.equal((0, eval)(await loadVoicePatchExpression()), "codey-startup-patch-installed-v4");

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
    assert.throws(
      () => new electron.BrowserWindow({ appearance: "globalDictation", title: "Dictation" }),
      (error) => error?.code === "CODEY_VOICE_DISABLED",
    );
    assert.ok(new electron.BrowserWindow({ title: "Settings" }) instanceof FakeBrowserWindow);
    assert.equal(globalThis.__CODEY_DISABLED_VOICE_MANAGER__, undefined);
  } finally {
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});

test("startup lifecycle patch closes temporary WebViews and waits for all turns before cleanup", async () => {
  const lifecycle = globalThis.__CODEY_TEMP_WEBVIEW_LIFECYCLE__;
  const owner = {};
  let destroyedListener = null;
  let closed = 0;
  const guest = {
    close: () => { closed += 1; },
    isDestroyed: () => false,
    once: (name, listener) => {
      if (name === "destroyed") destroyedListener = listener;
    },
  };
  lifecycle.track(owner, "codex-checkout", guest);
  lifecycle.close(owner, "codex-checkout");
  assert.equal(closed, 1);
  destroyedListener?.();

  let notificationHandler = null;
  const killed = [];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => [
      { command: "node ./mcp/server.mjs", depth: 2, kind: "mcp", pid: 41 },
      { command: "/Codex/Resources/cua_node/bin/node_repl", depth: 2, pid: 42 },
      { command: "npm run vite:dev", depth: 2, kind: "other", pid: 43 },
    ],
  });
  notificationHandler({ method: "turn/started", params: { threadId: "a", turn: { id: "1" } } });
  notificationHandler({ method: "turn/started", params: { threadId: "b", turn: { id: "2" } } });
  notificationHandler({ method: "turn/completed", params: { threadId: "a", turn: { id: "1" } } });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(killed, []);
  notificationHandler({ method: "turn/completed", params: { threadId: "b", turn: { id: "2" } } });
  await new Promise((resolve) => setTimeout(resolve, 1100));
  assert.deepEqual(killed, [41, 42]);
  dispose();
});
