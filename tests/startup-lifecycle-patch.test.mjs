import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const delay = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));
const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");
const standaloneNodeRepl = (pid, overrides = {}) => ({
  appServerPid: 10,
  command: "/Codex/Resources/cua_node/bin/node_repl",
  depth: 1,
  kind: "other",
  parentPid: 10,
  pid,
  rootChildPid: pid,
  startedAtMs: 1000,
  ...overrides,
});

async function waitFor(predicate, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) {
      assert.fail(`condition was not met within ${timeoutMs}ms`);
    }
    await delay(5);
  }
}

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
    const nativeAvatarManagerSource = [
      "class AvatarOverlayManager{",
      "async prewarm(e){",
      "if(this.window!=null||this.openingWindowPromise!=null||this.isAppQuitting)return;",
      "let t=this.windowVisibilitySequence,n=await this.ensureWindow(t);",
      "n==null||t!==this.windowVisibilitySequence}",
      "}",
    ].join("");
    assert.equal(
      globalThis.__CODEY_PATCH_CODEX_AVATAR_OVERLAY_PREWARM__(
        nativeAvatarManagerSource,
      ),
      nativeAvatarManagerSource,
    );
  } finally {
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
  }
});

test("checkout WebView attachment stays inside the unique checkout handler", () => {
  const source = [
    "function kI({getAuthToken:e,owner:t}){",
    "x.on(`will-attach-webview`,()=>{});x.on(`did-attach-webview`,()=>{});",
    "let i=n.shift();i!=null&&AI({webContents:d})}",
    "function bL({getAuthToken:e,onBusinessCheckoutComplete:t,",
    "onPersonalCheckoutComplete:n,onPurchaseCheckoutComplete:r,",
    "onReturnToCodex:i,owner:a}){",
    "x.on(`will-attach-webview`,()=>{});x.on(`did-attach-webview`,()=>{});",
    "let s=q.shift();if(s==null)return;inspect({webContents:d});",
    "AI(()=>{},{webContents:g})},cleanup()}",
    "function SL(){AI(()=>{},{webContents:c})},cleanup()}",
  ].join("");

  const patched =
    globalThis.__CODEY_PATCH_CHECKOUT_WEBVIEW_ATTACHMENT__(source);

  assert.match(
    patched,
    /track\(a,s\.partition,g\);inspect\(\{webContents:d\}\)/,
  );
  assert.doesNotMatch(patched, /track\(t,/);
  assert.throws(
    () => globalThis.__CODEY_PATCH_CHECKOUT_WEBVIEW_ATTACHMENT__(
      source.replace("AI(()=>{},{webContents:g})},", "AI(),"),
    ),
    /guest reference not found/,
  );
});

test("execution snapshot normalizes app-server ownership and root process trees", () => {
  const lifecycle = globalThis.__CODEY_EXECUTION_PROCESS_LIFECYCLE__;
  const normalized = lifecycle.normalizeSnapshot([
    { command: "codex app-server", depth: 1, kind: "app_server", parentPid: 9, pid: 10 },
    { command: "node fastctx.js --mcp", depth: 2, kind: "mcp", parentPid: 10, pid: 11 },
    { command: "node fastctx.js --mcp", depth: 2, kind: "mcp", parentPid: 10, pid: 11 },
    { command: "node fastctx-worker.js", depth: 3, kind: "other", parentPid: 11, pid: 12 },
    { command: "unrelated", depth: 1, kind: "other", parentPid: 9, pid: 13 },
  ]);

  assert.deepEqual(
    normalized.map(({ appServerPid, depth, pid, rootChildPid }) => ({
      appServerPid,
      depth,
      pid,
      rootChildPid,
    })),
    [
      { appServerPid: 10, depth: 1, pid: 11, rootChildPid: 11 },
      { appServerPid: 10, depth: 2, pid: 12, rootChildPid: 11 },
    ],
  );
});

test("execution terminator rejects MCP targets before attempting a signal", async () => {
  const lifecycle = globalThis.__CODEY_EXECUTION_PROCESS_LIFECYCLE__;
  const attemptsBefore = lifecycle.status.terminationAttempts;
  const terminated = await lifecycle.terminate(999_999, {
    appServerPid: 10,
    command: "/Codex/Resources/cua_node/bin/node_repl --mcp",
    depth: 1,
    kind: "mcp",
    parentPid: 10,
    pid: 999_999,
    rootChildPid: 999_999,
    startedAtMs: 1000,
  });

  assert.equal(terminated, false);
  assert.equal(lifecycle.status.terminationAttempts, attemptsBefore);
});

test("startup lifecycle patch preserves unique MCP servers and waits for all turns before cleanup", async () => {
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
      { command: "git-mcp-server --stdio", depth: 2, kind: "mcp", pid: 41 },
      standaloneNodeRepl(42),
      { command: "npm run vite:dev", depth: 2, kind: "other", pid: 43 },
      { command: "node ./mcp/server.mjs", depth: 2, pid: 44 },
      { command: "node codegraph.js serve --mcp", depth: 2, pid: 45 },
    ],
    completionGraceMs: 10,
  });
  notificationHandler({ method: "turn/started", params: { threadId: "a", turn: { id: "1" } } });
  notificationHandler({ method: "turn/started", params: { threadId: "b", turn: { id: "2" } } });
  notificationHandler({ method: "turn/completed", params: { threadId: "a", turn: { id: "1" } } });
  await delay(20);
  assert.deepEqual(killed, []);
  notificationHandler({ method: "turn/completed", params: { threadId: "b", turn: { id: "2" } } });
  await waitFor(() => killed.length === 1);
  assert.deepEqual(killed, [42]);
  dispose();
});

test("execution reaper unsubscribes completed subagents without reaping an active root", async () => {
  let notificationHandler = null;
  let snapshotCalls = 0;
  const killed = [];
  const unsubscribed = [];
  const processes = [
    {
      ageSeconds: 120,
      appServerPid: 10,
      command: "/Codey/codey-fastctx --codey-fastctx-mcp",
      depth: 1,
      kind: "mcp",
      parentPid: 10,
      pid: 101,
      rootChildPid: 101,
      startedAtMs: 1000,
    },
    {
      ageSeconds: 5,
      appServerPid: 10,
      command: "/Codey/codey-fastctx --codey-fastctx-mcp",
      depth: 1,
      kind: "mcp",
      parentPid: 10,
      pid: 102,
      rootChildPid: 102,
      startedAtMs: 116000,
    },
  ];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
      async unsubscribeThread(threadId) {
        unsubscribed.push(threadId);
        return { status: "unsubscribed" };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return processes;
    },
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "root", turn: { id: "root-turn" } },
  });
  notificationHandler({
    method: "item/completed",
    params: {
      threadId: "root",
      turnId: "root-turn",
      item: {
        type: "collabAgentToolCall",
        tool: "spawnAgent",
        status: "completed",
        receiverThreadIds: ["root", "child"],
      },
    },
  });
  notificationHandler({
    method: "turn/started",
    params: { threadId: "child", turn: { id: "child-turn" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "child", turn: { id: "child-turn" } },
  });
  await waitFor(() => unsubscribed.length === 1);

  assert.deepEqual(unsubscribed, ["child"]);
  assert.equal(snapshotCalls, 0);
  assert.deepEqual(killed, []);

  notificationHandler({
    method: "turn/completed",
    params: { threadId: "root", turn: { id: "root-turn" } },
  });
  await waitFor(() => snapshotCalls === 1);
  assert.deepEqual(unsubscribed, ["child"]);
  assert.deepEqual(killed, []);
  dispose();
});

test("execution reaper cancels subagent unsubscribe when that thread starts another turn", async () => {
  let notificationHandler = null;
  let snapshotCalls = 0;
  const killed = [];
  const unsubscribed = [];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
      async unsubscribeThread(threadId) {
        unsubscribed.push(threadId);
        return { result: { status: "notSubscribed" } };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return [];
    },
    completionGraceMs: 20,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "root", turn: { id: "root-turn" } },
  });
  notificationHandler({
    method: "thread/started",
    params: { thread: { id: "child", source: { subAgent: {} } } },
  });
  notificationHandler({
    method: "turn/started",
    params: { threadId: "child", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "child", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/started",
    params: { threadId: "child", turn: { id: "second" } },
  });
  await delay(30);

  assert.deepEqual(unsubscribed, []);
  assert.equal(snapshotCalls, 0);
  assert.deepEqual(killed, []);

  notificationHandler({
    method: "turn/completed",
    params: { threadId: "child", turn: { id: "second" } },
  });
  await waitFor(() => unsubscribed.length === 1);
  assert.deepEqual(unsubscribed, ["child"]);
  dispose();
});

test("execution reaper never terminates MCP processes even when globally idle", async () => {
  let notificationHandler = null;
  let snapshotCalls = 0;
  const killed = [];
  const processes = [
    {
      appServerPid: 10,
      command: "node fastctx.js --mcp",
      depth: 1,
      kind: "mcp",
      parentPid: 10,
      pid: 101,
      rootChildPid: 101,
      startedAtMs: 1000,
    },
    {
      appServerPid: 10,
      command: "/Codex/Resources/cua_node/bin/node_repl",
      depth: 2,
      kind: "other",
      parentPid: 101,
      pid: 102,
      rootChildPid: 101,
      startedAtMs: 1000,
    },
    {
      appServerPid: 10,
      command: "node fastctx.js --mcp",
      depth: 1,
      kind: "mcp",
      parentPid: 10,
      pid: 103,
      rootChildPid: 103,
      startedAtMs: 116000,
    },
    {
      appServerPid: 10,
      command: "/ThirdParty/node_repl --mcp",
      depth: 1,
      kind: "mcp",
      parentPid: 10,
      pid: 105,
      rootChildPid: 105,
      startedAtMs: 31000,
    },
    {
      appServerPid: 10,
      command: "/ThirdParty/node_repl",
      depth: 1,
      kind: "other",
      parentPid: 10,
      pid: 107,
      rootChildPid: 107,
      startedAtMs: 31000,
    },
    standaloneNodeRepl(106, { startedAtMs: 91000 }),
  ];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return processes;
    },
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "idle-safety", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "idle-safety", turn: { id: "first" } },
  });
  await waitFor(() => killed.length === 1);

  assert.equal(snapshotCalls, 2);
  assert.deepEqual(killed, [106]);
  dispose();
});

test("execution reaper rejects a standalone node_repl whose identity changed", async () => {
  let notificationHandler = null;
  let snapshotCalls = 0;
  const killed = [];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return [standaloneNodeRepl(203, {
        startedAtMs: snapshotCalls === 1 ? 1000 : 9000,
      })];
    },
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "node-repl-pid-reuse", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "node-repl-pid-reuse", turn: { id: "first" } },
  });
  await waitFor(() => snapshotCalls === 2);
  await delay(15);

  assert.equal(snapshotCalls, 2);
  assert.deepEqual(killed, []);
  dispose();
});

test("execution reaper fails closed when a candidate has no process start identity", async () => {
  let notificationHandler = null;
  let snapshotCalls = 0;
  const killed = [];
  const candidates = [standaloneNodeRepl(211, { startedAtMs: null })];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return candidates;
    },
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "missing-start", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "missing-start", turn: { id: "first" } },
  });
  await waitFor(() => snapshotCalls === 2);
  await delay(15);

  assert.deepEqual(killed, []);
  dispose();
});

test("execution reaper recognizes terminal notifications and thread closure", async () => {
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
    snapshot: async () => [standaloneNodeRepl(51)],
    completionGraceMs: 5,
  });

  const terminalStates = [
    "completed",
    "aborted",
    "cancelled",
    "canceled",
    "failed",
    "error",
    "errored",
    "closed",
    "stopped",
    "interrupted",
  ];
  for (const [index, state] of terminalStates.entries()) {
    const id = String(index);
    notificationHandler({
      method: "turn/started",
      params: { threadId: "terminal-events", turn: { id } },
    });
    notificationHandler({
      method: `turn/${state}`,
      params: { thread_id: "terminal-events", turn_id: id },
    });
  }
  notificationHandler({
    method: "turn/started",
    params: { threadId: "closed-thread", turn: { id: "remaining" } },
  });
  notificationHandler({
    method: "thread/closed",
    params: { threadId: "closed-thread" },
  });

  await waitFor(() => killed.length === 1);
  assert.deepEqual(killed, [51]);
  dispose();
  assert.equal(notificationHandler, null);
});

test("execution reaper rearms when a grace timer fires before the quiet window", async () => {
  let notificationHandler = null;
  let acceleratedTimer = false;
  let now = 1000;
  let snapshotCalls = 0;
  const nativeDateNow = Date.now;
  const nativeSetTimeout = globalThis.setTimeout;
  const killed = [];

  Date.now = () => now;
  globalThis.setTimeout = (listener, milliseconds, ...args) => {
    if (!acceleratedTimer && milliseconds === 20) {
      acceleratedTimer = true;
      return nativeSetTimeout(listener, 0, ...args);
    }
    return nativeSetTimeout(listener, milliseconds, ...args);
  };

  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return [standaloneNodeRepl(58)];
    },
    completionGraceMs: 20,
  });

  try {
    notificationHandler({
      method: "turn/started",
      params: { threadId: "early-timer", turn: { id: "first" } },
    });
    notificationHandler({
      method: "turn/completed",
      params: { threadId: "early-timer", turn: { id: "first" } },
    });

    await new Promise((resolve) => nativeSetTimeout(resolve, 10));
    assert.equal(snapshotCalls, 0);
    assert.deepEqual(killed, []);

    now = 1020;
    Date.now = nativeDateNow;
    globalThis.setTimeout = nativeSetTimeout;
    await waitFor(() => killed.length === 1);
    assert.equal(snapshotCalls, 2);
    assert.deepEqual(killed, [58]);
  } finally {
    Date.now = nativeDateNow;
    globalThis.setTimeout = nativeSetTimeout;
    dispose();
  }
});

test("execution reaper ignores unknown lifecycle state until an observed turn finishes", async () => {
  let notificationHandler = null;
  let snapshotCalls = 0;
  const killed = [];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      return [standaloneNodeRepl(52)];
    },
    completionGraceMs: 5,
  });

  await delay(30);
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "preexisting", turn: { id: "unknown" } },
  });
  await delay(30);
  assert.equal(snapshotCalls, 0);
  assert.deepEqual(killed, []);

  notificationHandler({
    method: "turn/started",
    params: { threadId: "observed", turn: { id: "known" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "observed", turn: { id: "known" } },
  });
  await waitFor(() => killed.length === 1);
  assert.equal(snapshotCalls, 2);
  assert.deepEqual(killed, [52]);
  dispose();
});

test("execution reaper never evicts a silent long-running turn", async () => {
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
    snapshot: async () => [standaloneNodeRepl(53)],
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "active", turn: { id: "silent" } },
  });
  await delay(80);
  assert.deepEqual(killed, []);
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "active", turn: { id: "silent" } },
  });
  await waitFor(() => killed.length === 1);
  assert.deepEqual(killed, [53]);
  dispose();
});

test("execution reaper rechecks turn state after its process snapshot", async () => {
  let notificationHandler = null;
  let releaseFirstSnapshot = null;
  let firstSnapshotSeen = false;
  let snapshotCalls = 0;
  const killed = [];
  const candidates = [standaloneNodeRepl(54)];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      if (snapshotCalls !== 1) return candidates;
      firstSnapshotSeen = true;
      return new Promise((resolve) => { releaseFirstSnapshot = () => resolve(candidates); });
    },
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "race", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "race", turn: { id: "first" } },
  });
  await waitFor(() => firstSnapshotSeen);
  notificationHandler({
    method: "turn/started",
    params: { threadId: "race", turn: { id: "second" } },
  });
  releaseFirstSnapshot();
  await delay(20);
  assert.deepEqual(killed, []);

  notificationHandler({
    method: "turn/completed",
    params: { threadId: "race", turn: { id: "second" } },
  });
  await waitFor(() => killed.length === 1);
  assert.ok(snapshotCalls >= 2);
  assert.deepEqual(killed, [54]);
  dispose();
});

test("execution reaper cancels cleanup when a turn starts after the snapshot", async () => {
  let notificationHandler = null;
  let firstSnapshotSeen = false;
  let snapshotCalls = 0;
  const killed = [];
  const candidates = [standaloneNodeRepl(55)];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      if (snapshotCalls === 1) {
        firstSnapshotSeen = true;
        setTimeout(() => {
          notificationHandler({
            method: "turn/started",
            params: { threadId: "snapshot-race", turn: { id: "second" } },
          });
        }, 0);
      }
      return candidates;
    },
    completionGraceMs: 20,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "snapshot-race", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "snapshot-race", turn: { id: "first" } },
  });
  await waitFor(() => firstSnapshotSeen);
  await delay(40);
  assert.deepEqual(killed, []);

  notificationHandler({
    method: "turn/completed",
    params: { threadId: "snapshot-race", turn: { id: "second" } },
  });
  await waitFor(() => killed.length === 1);
  assert.ok(snapshotCalls >= 2);
  assert.deepEqual(killed, [55]);
  dispose();
});

test("execution reaper retries after snapshot failure when a newer turn finished", async () => {
  let notificationHandler = null;
  let rejectFirstSnapshot = null;
  let firstSnapshotSeen = false;
  let snapshotCalls = 0;
  const killed = [];
  const candidates = [standaloneNodeRepl(56)];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      if (snapshotCalls !== 1) return candidates;
      firstSnapshotSeen = true;
      return new Promise((_resolve, reject) => {
        rejectFirstSnapshot = () => reject(new Error("snapshot unavailable"));
      });
    },
    completionGraceMs: 25,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "retry", turn: { id: "first" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "retry", turn: { id: "first" } },
  });
  await waitFor(() => firstSnapshotSeen);
  notificationHandler({
    method: "turn/started",
    params: { threadId: "retry", turn: { id: "newer" } },
  });
  notificationHandler({
    method: "turn/failed",
    params: { threadId: "retry", turn: { id: "newer" } },
  });
  rejectFirstSnapshot();
  await delay(10);
  assert.deepEqual(killed, []);

  await waitFor(() => killed.length === 1);
  assert.ok(snapshotCalls >= 2);
  assert.deepEqual(killed, [56]);
  dispose();
});

test("disposing execution reaper cancels pending cleanup", async () => {
  let notificationHandler = null;
  let releaseSnapshot = null;
  let firstSnapshotSeen = false;
  let snapshotCalls = 0;
  const killed = [];
  const dispose = globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({
    connection: {
      registerInternalNotificationHandler(handler) {
        notificationHandler = handler;
        return () => { notificationHandler = null; };
      },
    },
    kill: async (pid) => { killed.push(pid); },
    snapshot: async () => {
      snapshotCalls += 1;
      firstSnapshotSeen = true;
      return new Promise((resolve) => {
        releaseSnapshot = () => resolve([standaloneNodeRepl(57)]);
      });
    },
    completionGraceMs: 5,
  });

  notificationHandler({
    method: "turn/started",
    params: { threadId: "dispose", turn: { id: "pending" } },
  });
  notificationHandler({
    method: "turn/completed",
    params: { threadId: "dispose", turn: { id: "pending" } },
  });
  await waitFor(() => firstSnapshotSeen);
  dispose();
  assert.equal(notificationHandler, null);
  releaseSnapshot();
  await delay(20);
  assert.deepEqual(killed, []);
  dispose();
});
