import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(
  new URL("../public/git-request-guard.js", import.meta.url),
  "utf8",
);
const cdpSource = await readFile(
  new URL("../backend/src/cdp.rs", import.meta.url),
  "utf8",
);

const gitRequest = (id, method, params = {}) => ({
  type: "worker-request",
  workerId: "git",
  request: { id, method, params },
});

function createRuntime({
  platform = "Win32",
  send,
  bridgeReady = true,
  freezeBridge = false,
  mainGuardReady = null,
} = {}) {
  let currentTime = 10_000;
  let nextTimerId = 0;
  const timers = new Map();
  const nativeCalls = [];
  const events = [];
  const statusRequests = [];
  const subscriptions = new Map();
  const messageListeners = new Set();
  const sendImpl =
    send ??
    ((workerId, message) =>
      Promise.resolve({ delivered: true, workerId, requestId: message?.request?.id }));
  const window = {
    navigator: {
      platform,
      userAgent: platform === "Win32" ? "Windows" : "Macintosh",
    },
    __codeyInjectionStatus: {
      "git-request-guard": {
        status: "pending",
        detail: null,
        error: null,
      },
    },
    addEventListener(type, listener) {
      if (type === "message") messageListeners.add(listener);
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    setTimeout(callback, delay = 0) {
      const id = ++nextTimerId;
      timers.set(id, {
        callback,
        dueAt: currentTime + Math.max(0, Number(delay) || 0),
      });
      return id;
    },
    removeEventListener(type, listener) {
      if (type === "message") messageListeners.delete(listener);
    },
    dispatchEvent(event) {
      events.push(event);
      if (event.type === "message") {
        for (const listener of [...messageListeners]) listener(event);
      }
      return true;
    },
  };
  const electronBridge = {
    sendWorkerMessageFromView(workerId, message, ...rest) {
      nativeCalls.push({ workerId, message, rest });
      return sendImpl(workerId, message, ...rest);
    },
    subscribeToWorkerMessages(workerId, listener) {
      subscriptions.set(workerId, listener);
      return () => subscriptions.delete(workerId);
    },
  };
  if (mainGuardReady !== null) {
    electronBridge.sendMessageFromView = (message) => {
      statusRequests.push(message);
      const guard = {
        enabled: true,
        gitHandlerPatched: true,
        matched: mainGuardReady === "unobserved" ? 0 : 1,
        statusHandlerPatched: true,
        strategy: "main-process-ipc",
        tokenRefillMs: 1_000,
      };
      if (mainGuardReady === "event") {
        window.dispatchEvent({
          type: "message",
          data: {
            type: "codey-git-request-guard-status-response",
            requestId: message.requestId,
            status: "ok",
            guard,
          },
        });
        return Promise.resolve(undefined);
      }
      if (mainGuardReady === "no-return") {
        return Promise.resolve(undefined);
      }
      if (!mainGuardReady) {
        return Promise.resolve({
          status: "ok",
          guard: {
            enabled: true,
            gitHandlerPatched: false,
          },
        });
      }
      return Promise.resolve({
        status: "ok",
        guard,
      });
    };
  }
  if (freezeBridge) Object.freeze(electronBridge);
  if (bridgeReady) window.electronBridge = electronBridge;
  window.window = window;
  const context = {
    CustomEvent: class CustomEvent {
      constructor(type, init = {}) {
        this.type = type;
        this.detail = init.detail;
      }
    },
    Date: { now: () => currentTime },
    console,
    window,
  };

  const run = () => vm.runInNewContext(source, context);
  const flush = async () => {
    await Promise.resolve();
    await Promise.resolve();
    await new Promise((resolve) => setImmediate(resolve));
    await Promise.resolve();
    await Promise.resolve();
  };
  const advance = async (milliseconds) => {
    currentTime += milliseconds;
    for (let iteration = 0; iteration < 50; iteration += 1) {
      const due = [...timers.entries()]
        .filter(([, timer]) => timer.dueAt <= currentTime)
        .sort((left, right) => left[1].dueAt - right[1].dueAt);
      if (due.length === 0) break;
      for (const [id, timer] of due) {
        if (!timers.delete(id)) continue;
        timer.callback();
        await flush();
      }
    }
  };

  run();
  return {
    advance,
    context,
    connectBridge() {
      window.electronBridge = electronBridge;
    },
    disconnectBridge() {
      delete window.electronBridge;
    },
    emit(workerId, message) {
      subscriptions.get(workerId)?.(message);
    },
    events,
    flush,
    nativeCalls,
    run,
    statusRequests,
    timers,
    window,
  };
}

test("Git request guard is registered as an independently probed CDP script", () => {
  assert.match(
    cdpSource,
    /include_str!\("\.\.\/\.\.\/dist-overlay\/inject\/git-request-guard\.js"\)/,
  );
  assert.match(
    cdpSource,
    /"git-request-guard",\s*"Windows Git 请求保护",\s*GIT_REQUEST_GUARD_SCRIPT/,
  );
  assert.match(cdpSource, /window\.__codeyGitRequestGuard/);
  assert.match(cdpSource, /Windows Git 请求限流已由主进程接管/);
});

test("Git request guard leaves unknown and mutating worker requests untouched", async () => {
  const runtime = createRuntime();
  const unknown = gitRequest("unknown", "stable-metadata", { cwd: "C:\\repo" });
  const mutation = gitRequest("commit", "commit", { cwd: "C:\\repo" });

  await runtime.window.electronBridge.sendWorkerMessageFromView("git", unknown);
  await runtime.window.electronBridge.sendWorkerMessageFromView("git", mutation);
  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "computer-use",
    gitRequest("other-worker", "status-summary", { cwd: "C:\\repo" }),
  );

  assert.equal(runtime.nativeCalls.length, 3);
  assert.equal(runtime.nativeCalls[0].message, unknown);
  assert.equal(runtime.nativeCalls[1].message, mutation);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().matched, 0);
});

test("Git request guard leaves unknown live queries without a duplicated worker id untouched", async () => {
  const runtime = createRuntime();
  const liveRequest = (id) => ({
    type: "worker-request",
    request: {
      id,
      method: "subscribe-live-query",
      params: {
        query: {
          method: "worktree-health-v2",
          params: { cwd: "C:\\repo" },
        },
      },
    },
  });

  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    liveRequest("live-1"),
  );
  const secondDelivery = runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    liveRequest("live-2"),
  );

  await secondDelivery;
  assert.equal(runtime.nativeCalls.length, 2);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().matched, 0);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().queued, 0);
});

test("Git request guard spaces duplicate read requests and cancels queued work", async () => {
  const runtime = createRuntime();
  const first = gitRequest("status-1", "status-summary", {
    cwd: "C:\\repo",
    includeUntrackedFiles: true,
    operationSource: "local_conversation_git_actions",
  });
  const second = gitRequest("status-2", "status-summary", {
    cwd: "C:\\repo",
    includeUntrackedFiles: true,
    operationSource: "local_conversation_git_actions",
  });
  const cancelled = gitRequest("status-3", "status-summary", {
    cwd: "C:\\repo",
    includeUntrackedFiles: true,
    operationSource: "local_conversation_git_actions",
  });

  await runtime.window.electronBridge.sendWorkerMessageFromView("git", first);
  const secondDelivery =
    runtime.window.electronBridge.sendWorkerMessageFromView("git", second);
  const cancelledDelivery =
    runtime.window.electronBridge.sendWorkerMessageFromView("git", cancelled);

  assert.equal(runtime.nativeCalls.length, 1);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().queued, 2);

  await runtime.window.electronBridge.sendWorkerMessageFromView("git", {
    type: "worker-request-cancel",
    workerId: "git",
    id: "status-3",
  });
  await cancelledDelivery;
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().queued, 1);
  assert.equal(
    runtime.window.__codeyGitRequestGuard.snapshot().cancelledBeforeSend,
    1,
  );

  await runtime.advance(1_999);
  assert.equal(runtime.nativeCalls.length, 1);
  await runtime.advance(1);
  await secondDelivery;
  assert.equal(runtime.nativeCalls.length, 2);
  assert.equal(runtime.nativeCalls[1].message, second);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().sent, 2);
});

test("Git request guard only protects explicitly supported live-query reads", async () => {
  const runtime = createRuntime();
  const protectedSubscription = gitRequest("live-review", "subscribe-live-query", {
    operationSource: "review_model",
    subscriptionId: "subscription-1",
    query: {
      method: "review-summary",
      params: {
        cwd: "C:\\repo",
        includeUntrackedFiles: true,
        operationSource: "review_model",
      },
    },
  });
  const unrelatedSubscription = gitRequest("live-branch", "subscribe-live-query", {
    subscriptionId: "subscription-2",
    query: {
      method: "branch-commits",
      params: { cwd: "C:\\repo" },
    },
  });

  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    protectedSubscription,
  );
  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    unrelatedSubscription,
  );

  assert.equal(runtime.nativeCalls.length, 2);
  assert.equal(runtime.nativeCalls[1].message, unrelatedSubscription);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().matched, 1);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().lastMethod, "review-summary");
});

test("Git request guard retains renderer protection until main observes a Git request", async () => {
  const runtime = createRuntime({ mainGuardReady: "unobserved" });
  await runtime.flush();
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().mainProcessProtected, false);
  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git", gitRequest("status", "status-summary", { cwd: "C:\\repo" }),
  );
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().matched, 1);
});

test("Git request guard allows a small burst and caps the sustained global rate", async () => {
  const runtime = createRuntime();
  const deliveries = [
    runtime.window.electronBridge.sendWorkerMessageFromView(
      "git",
      gitRequest("origins-burst", "git-origins", { dirs: ["C:\\one"] }),
    ),
    runtime.window.electronBridge.sendWorkerMessageFromView(
      "git",
      gitRequest("status-burst", "status-summary", { cwd: "C:\\two" }),
    ),
    runtime.window.electronBridge.sendWorkerMessageFromView(
      "git",
      gitRequest("review-burst", "review-summary", { cwd: "C:\\three" }),
    ),
    runtime.window.electronBridge.sendWorkerMessageFromView(
      "git",
      gitRequest("diff-queued", "branch-diff-stats", { cwd: "C:\\four" }),
    ),
  ];

  assert.equal(runtime.nativeCalls.length, 3);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().queued, 1);
  await runtime.advance(999);
  assert.equal(runtime.nativeCalls.length, 3);
  await runtime.advance(1);
  await Promise.all(deliveries);
  assert.equal(runtime.nativeCalls.length, 4);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().queued, 0);
});

test("Git request guard probe can finish installation after the bridge appears", async () => {
  const runtime = createRuntime({ bridgeReady: false });

  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().bridgePatched, false);
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].status,
    "pending",
  );
  runtime.connectBridge();

  assert.equal(runtime.window.__codeyGitRequestGuard.ensureInstalled(), true);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().bridgePatched, true);
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].status,
    "effective",
  );
});

test("Git request guard can patch configurable bridge methods", async () => {
  const runtime = createRuntime({ bridgeReady: false });
  let send = runtime.context.window.electronBridge?.sendWorkerMessageFromView;
  runtime.connectBridge();
  send = runtime.window.electronBridge.sendWorkerMessageFromView;
  Object.defineProperty(runtime.window.electronBridge, "sendWorkerMessageFromView", {
    configurable: true,
    get: () => send,
    set: () => {},
  });

  assert.equal(runtime.window.__codeyGitRequestGuard.ensureInstalled(), true);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().bridgePatched, true);
  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    gitRequest("status-accessor", "status-summary", { cwd: "C:\\repo" }),
  );
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().matched, 1);
});

test("Git request guard accepts main-process protection when contextBridge is frozen", async () => {
  const runtime = createRuntime({
    freezeBridge: true,
    mainGuardReady: true,
  });
  await runtime.flush();

  const snapshot = runtime.window.__codeyGitRequestGuard.snapshot();
  assert.equal(snapshot.version, 3);
  assert.equal(snapshot.bridgePatched, false);
  assert.equal(snapshot.mainProcessProtected, true);
  assert.equal(snapshot.mainProcessProbeTransport, "invoke-return");
  assert.equal(snapshot.installed, true);
  assert.equal(snapshot.strategy, "main-process-ipc");
  assert.equal(runtime.statusRequests.length, 1);
  assert.equal(
    runtime.statusRequests[0].type,
    "codey-git-request-guard-status",
  );
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].status,
    "effective",
  );
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].detail,
    "Windows Git 请求限流已由主进程接管",
  );

  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    gitRequest("main-process-status", "status-summary", { cwd: "C:\\repo" }),
  );
  assert.equal(runtime.nativeCalls.length, 1);
  assert.equal(snapshot.matched, 0);
});

test("Git request guard verifies a no-return preload through a renderer event", async () => {
  const runtime = createRuntime({
    freezeBridge: true,
    mainGuardReady: "event",
  });
  await runtime.flush();

  const snapshot = runtime.window.__codeyGitRequestGuard.snapshot();
  assert.equal(snapshot.bridgePatched, false);
  assert.equal(snapshot.mainProcessProtected, true);
  assert.equal(snapshot.mainProcessProbeTransport, "renderer-event");
  assert.equal(snapshot.mainProcessSnapshot.gitHandlerPatched, true);
  assert.equal(snapshot.installed, true);
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].status,
    "effective",
  );
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].detail,
    "Windows Git 请求限流已由主进程接管",
  );
});

test("Git request guard does not treat a missing status response as verified", async () => {
  const runtime = createRuntime({
    freezeBridge: true,
    mainGuardReady: "no-return",
  });
  await runtime.flush();

  assert.equal(
    runtime.window.__codeyGitRequestGuard.snapshot().mainProcessProtected,
    false,
  );
  await runtime.advance(1_000);
  await runtime.flush();
  assert.equal(
    runtime.window.__codeyGitRequestGuard.snapshot().mainProcessProtected,
    false,
  );
  assert.notEqual(
    runtime.window.__codeyInjectionStatus["git-request-guard"].status,
    "effective",
  );
});

test("Git request guard re-arms bridge retries when an existing guard is re-injected", async () => {
  const runtime = createRuntime();
  const entry = runtime.window.__codeyInjectionStatus["git-request-guard"];
  await runtime.advance(31_000);
  runtime.events.length = 0;
  runtime.disconnectBridge();
  entry.status = "pending";
  entry.detail = null;
  entry.error = null;

  runtime.run();
  entry.status = "executed";

  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().bridgePatched, false);
  assert.equal(runtime.timers.size, 1);
  await runtime.advance(1_000);
  runtime.connectBridge();
  await runtime.advance(100);

  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().bridgePatched, true);
  assert.equal(entry.status, "effective");
  assert.equal(runtime.timers.size, 0);
  assert.ok(
    runtime.events.some(
      (event) =>
        event.type === "codey-injection-status-changed" &&
        event.detail?.id === "git-request-guard",
    ),
  );
});

test("Git request guard observes worker failures and remains idempotent", async () => {
  const runtime = createRuntime();
  const wrapper = runtime.window.electronBridge.sendWorkerMessageFromView;
  runtime.window.electronBridge.subscribeToWorkerMessages("git", () => {});

  await runtime.window.electronBridge.sendWorkerMessageFromView(
    "git",
    gitRequest("origins-1", "git-origins", {
      dirs: ["C:\\repo"],
      operationSource: "sidebar_workspace_task_groups_task_dirs",
    }),
  );
  runtime.emit("git", {
    type: "worker-response",
    workerId: "git",
    response: {
      id: "origins-1",
      result: { type: "error", error: "repository unavailable" },
    },
  });

  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().observedFailures, 1);
  runtime.run();
  assert.equal(runtime.window.electronBridge.sendWorkerMessageFromView, wrapper);
  assert.equal(runtime.window.__codeyGitRequestGuard.snapshot().version, 3);
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].status,
    "effective",
  );
  assert.ok(
    runtime.events.some(
      (event) =>
        event.type === "codey-injection-status-changed" &&
        event.detail?.id === "git-request-guard",
    ),
  );
});

test("Git request guard is a no-op outside Windows", async () => {
  const runtime = createRuntime({ platform: "MacIntel" });
  const native = runtime.window.electronBridge.sendWorkerMessageFromView;

  await native(
    "git",
    gitRequest("status-mac", "status-summary", { cwd: "/tmp/repo" }),
  );

  const snapshot = runtime.window.__codeyGitRequestGuard.snapshot();
  assert.equal(snapshot.enabled, false);
  assert.equal(snapshot.installed, true);
  assert.equal(snapshot.bridgePatched, false);
  assert.equal(snapshot.matched, 0);
  assert.equal(runtime.nativeCalls.length, 1);
  assert.equal(
    runtime.window.__codeyInjectionStatus["git-request-guard"].detail,
    "Git 请求保护已就绪，当前平台无需启用",
  );
});
