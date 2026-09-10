import assert from "node:assert/strict";
import test from "node:test";

import { flushMicrotasks } from "./helpers/flush.mjs";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

function createFakeClock() {
  let currentTime = 0;
  let nextTimerId = 1;
  const timers = new Map();
  const clock = {
    now: () => currentTime,
    setTimeout(callback, delay) {
      const id = nextTimerId;
      nextTimerId += 1;
      timers.set(id, {
        at: currentTime + Math.max(0, delay),
        callback,
      });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  };

  return {
    clock,
    async runNext() {
      const next = [...timers.entries()].sort(
        ([leftId, left], [rightId, right]) =>
          left.at - right.at || leftId - rightId,
      )[0];
      if (!next) return false;
      const [id, timer] = next;
      timers.delete(id);
      currentTime = timer.at;
      timer.callback();
      await flushMicrotasks();
      return true;
    },
  };
}

test("runtime feature presentation exposes only confirmed user-facing behavior", async () => {
  const { buildEnabledOptimizationFeatures } = await loadTypeScriptModule(
    new URL("src/runtimeStatusPresentation.ts", root),
  );
  const features = buildEnabledOptimizationFeatures(
    {
      running: true,
      injectionScripts: [
        {
          id: "internal-ready",
          name: "内部控制",
          source: "builtin",
          visibility: "internal",
          status: "effective",
        },
        {
          id: "builtin-ready",
          name: "内置功能",
          source: "builtin",
          visibility: "feature",
          status: "effective",
          detail: "已确认",
        },
        {
          id: "user-ready",
          name: "用户功能",
          source: "user",
          visibility: "feature",
          status: "effective",
        },
        {
          id: "not-confirmed",
          name: "等待确认",
          source: "builtin",
          visibility: "feature",
          status: "executed",
        },
      ],
      subagentOptimizationActive: true,
      notificationChannelsActive: true,
      activeNotificationChannelCount: 2,
      traceLogWriteProtectionActive: true,
      crashpadDiskProtectionActive: true,
    },
    {
      userConfigured: true,
      detectionFailed: false,
      serverId: "fastctx-local",
    },
  );

  assert.deepEqual(
    features.map(({ id, icon, sourceLabel }) => ({ id, icon, sourceLabel })),
    [
      {
        id: "fastctx-context-tools",
        icon: "fastctx",
        sourceLabel: "外部配置",
      },
      { id: "builtin-ready", icon: "code", sourceLabel: "内置" },
      { id: "user-ready", icon: "code", sourceLabel: "用户脚本" },
      {
        id: "subagent-optimization",
        icon: "subagent",
        sourceLabel: "Codey",
      },
      {
        id: "notification-channels",
        icon: "notifications",
        sourceLabel: "Codey",
      },
      {
        id: "disk-write-protection",
        icon: "database",
        sourceLabel: "Codey",
      },
    ],
  );
  assert.equal(features[0].detail, "Codex 已配置 FastCtx（fastctx-local）");
  assert.equal(features.at(-1).detail, "Codex Trace 日志与 Crashpad 磁盘保护均已生效");
  assert.equal(features.some((feature) => feature.id === "internal-ready"), false);
  assert.equal(features.some((feature) => feature.id === "not-confirmed"), false);
});

test("injection status summary separates internal failures from feature verification", async () => {
  const { summarizeInjectionScripts } = await loadTypeScriptModule(
    new URL("src/runtimeStatusPresentation.ts", root),
  );
  const summary = summarizeInjectionScripts([
    {
      id: "internal-pending",
      name: "内部等待",
      source: "builtin",
      visibility: "internal",
      status: "executed",
    },
    {
      id: "internal-failed",
      name: "内部失败",
      source: "builtin",
      visibility: "internal",
      status: "unknown",
    },
    {
      id: "feature-pending",
      name: "功能等待",
      source: "builtin",
      visibility: "feature",
      status: "executed",
    },
    {
      id: "feature-failed",
      name: "功能失败",
      source: "builtin",
      visibility: "feature",
      status: "failed",
    },
    {
      id: "feature-unknown",
      name: "功能未知",
      source: "user",
      visibility: "feature",
      status: "unknown",
    },
    {
      id: "feature-ready",
      name: "功能可用",
      source: "builtin",
      visibility: "feature",
      status: "effective",
    },
  ]);

  assert.deepEqual(summary, {
    failedInjectionScriptCount: 2,
    internalInjectionError: true,
    internalInjectionPending: true,
    unverifiedInjectionScriptCount: 1,
  });
});

test("status poll scheduler coalesces due work and stops after bounded failures", async () => {
  const {
    createStatusPollScheduler,
    createStatusPollTask,
    STATUS_POLL_MAX_CONSECUTIVE_ERRORS,
  } = await loadTypeScriptModule(
    new URL("src/runtimeStatusPollScheduler.ts", root),
  );

  {
    const fake = createFakeClock();
    const requests = [];
    const scheduler = createStatusPollScheduler(async (refreshesInjectionStatus) => {
      requests.push(refreshesInjectionStatus);
      return { running: true };
    }, fake.clock);
    scheduler.add(createStatusPollTask({
      kind: "injection",
      delays: [10],
      pending: () => false,
      refreshesInjectionStatus: true,
    }, 1_000, fake.clock.now()));
    scheduler.add(createStatusPollTask({
      kind: "restart",
      delays: [10],
      pending: () => false,
      refreshesInjectionStatus: false,
    }, 1_000, fake.clock.now()));

    assert.equal(await fake.runNext(), true);
    assert.deepEqual(requests, [true]);
    assert.equal(await fake.runNext(), false);
  }

  {
    const fake = createFakeClock();
    let requests = 0;
    const scheduler = createStatusPollScheduler(async () => {
      requests += 1;
      throw new Error("runtime unavailable");
    }, fake.clock);
    scheduler.add(createStatusPollTask({
      kind: "injection",
      delays: [1],
      pending: () => true,
      refreshesInjectionStatus: true,
    }, 10_000, fake.clock.now()));

    for (let index = 0; index < 10 && await fake.runNext(); index += 1) {
      // Drain scheduled retries.
    }
    assert.equal(requests, STATUS_POLL_MAX_CONSECUTIVE_ERRORS);
    assert.equal(await fake.runNext(), false);
  }
});

test("status poll scheduler never overlaps requests and clear cancels queued work", async () => {
  const { createStatusPollScheduler, createStatusPollTask } =
    await loadTypeScriptModule(
      new URL("src/runtimeStatusPollScheduler.ts", root),
    );
  const fake = createFakeClock();
  let requests = 0;
  let releaseRequest;
  const scheduler = createStatusPollScheduler(
    () => {
      requests += 1;
      return new Promise((resolve) => {
        releaseRequest = resolve;
      });
    },
    fake.clock,
  );
  scheduler.add(createStatusPollTask({
    kind: "restart",
    delays: [1],
    pending: () => true,
    refreshesInjectionStatus: false,
  }, 10_000, fake.clock.now()));

  assert.equal(await fake.runNext(), true);
  assert.equal(requests, 1);
  assert.equal(await fake.runNext(), false);

  scheduler.clear();
  releaseRequest({ running: true });
  await flushMicrotasks();
  assert.equal(await fake.runNext(), false);
  assert.equal(requests, 1);
});

test("restart polling reports exhaustion and can resume after a manual status query", async () => {
  const { createStatusPollScheduler, createStatusPollTask } =
    await loadTypeScriptModule(new URL("src/runtimeStatusPollScheduler.ts", root));
  for (const outcome of ["errors", "deadline", "completed"]) {
    const fake = createFakeClock();
    let exhausted = 0;
    let completed = outcome === "completed";
    const scheduler = createStatusPollScheduler(async () => {
      if (outcome === "errors" && !completed) throw new Error("disconnected");
      return { running: true, restartInProgress: !completed };
    }, fake.clock);
    const addRestart = () => scheduler.add(createStatusPollTask({
      kind: "restart",
      delays: [500],
      pending: (next) => next.restartInProgress,
      refreshesInjectionStatus: false,
      onExhausted: () => { exhausted += 1; },
    }, outcome === "errors" ? 300_000 : 1_000, fake.clock.now()));
    addRestart();
    for (let index = 0; index < 10 && await fake.runNext(); index += 1) {}
    assert.equal(exhausted, outcome === "completed" ? 0 : 1, outcome);
    assert.equal(await fake.runNext(), false);

    completed = true;
    addRestart();
    assert.equal(await fake.runNext(), true);
    assert.equal(await fake.runNext(), false);
    assert.equal(exhausted, outcome === "completed" ? 0 : 1);
  }
});

test("a timed-out status request cannot commit a late response", async () => {
  const { withTimeout } = await loadTypeScriptModule(new URL("src/appUtils.ts", root));
  const fake = createFakeClock();
  const previousWindow = globalThis.window;
  globalThis.window = fake.clock;
  try {
    let resolveRequest;
    let committed = false;
    const request = withTimeout(new Promise((resolve) => {
      resolveRequest = resolve;
    }), 10_000, "status timeout").then(() => { committed = true; });
    const rejection = assert.rejects(request, /status timeout/);
    await fake.runNext();
    await rejection;
    resolveRequest({ restartInProgress: true });
    await flushMicrotasks();
    assert.equal(committed, false);
  } finally {
    if (previousWindow === undefined) delete globalThis.window;
    else globalThis.window = previousWindow;
  }
});
