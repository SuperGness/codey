import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { reconcileConfigApply, configApplyNotice, configApplyPending } = await loadTypeScriptModule(new URL("../src/configApply.ts", import.meta.url));
const { reconcileRuntimeStatus } = await loadTypeScriptModule(new URL("../src/runtimeStatusSnapshot.ts", import.meta.url));
const { createStatusPollScheduler, createStatusPollTask } = await loadTypeScriptModule(new URL("../src/runtimeStatusPollScheduler.ts", import.meta.url));

test("late save and polling responses cannot replace newer or completed application state", () => {
  const applied = { revision: 5, state: "applied" };
  assert.equal(reconcileConfigApply(applied, { revision: 5, state: "pending" }), applied);
  assert.equal(reconcileConfigApply(applied, { revision: 4, state: "failed" }), applied);
  assert.equal(reconcileConfigApply(applied, null), applied);
  const newer = { revision: 6, state: "pending" };
  assert.equal(reconcileConfigApply(applied, newer), newer);
  const status = reconcileRuntimeStatus({ running: true, configApply: applied, restartRequired: false },
    { running: true, configApply: { revision: 4, state: "pending" }, restartRequired: true });
  assert.equal(status.configApply, applied);
  assert.equal(status.restartRequired, false);
});

test("pending application does not report restart until its outcome is known", () => {
  const pending = { revision: 2, state: "pending" };
  const status = reconcileRuntimeStatus(
    { running: true, restartRequired: false },
    { running: true, configApply: pending, restartRequired: true },
  );
  assert.equal(status.configApply, pending);
  assert.equal(status.restartRequired, false);

  const terminal = reconcileRuntimeStatus(status, {
    running: true,
    configApply: { revision: 2, state: "restart_required" },
    restartRequired: true,
  });
  assert.equal(terminal.restartRequired, true);
  const cleared = reconcileRuntimeStatus(terminal, {
    running: true,
    configApply: terminal.configApply,
    restartRequired: false,
  });
  assert.equal(cleared.restartRequired, false, "a fresh runtime snapshot can clear an earlier restart requirement");
  const confirmed = reconcileRuntimeStatus(terminal, {
    running: true,
    configApply: { revision: 3, state: "applied" },
    restartRequired: true,
  });
  assert.equal(
    confirmed.restartRequired,
    true,
    "a newer terminal response must keep its own restart confirmation",
  );
});

test("successful restart accepts a newer confirmation and rejects the old terminal result", () => {
  for (const state of ["failed", "restart_required", "saved"]) {
    const previous = { revision: 7, state };
    const confirmed = { revision: 8, state: "applied" };
    const status = reconcileRuntimeStatus(
      { running: true, configApply: previous, restartRequired: true },
      { running: true, configApply: confirmed, restartRequired: false },
    );
    assert.equal(status.configApply, confirmed);
    assert.equal(status.restartRequired, false);
    assert.equal(reconcileConfigApply(status.configApply, previous), confirmed);
  }
});

test("pending, failure and restart messages distinguish saving from applying", () => {
  assert.match(configApplyNotice({ revision: 1, state: "pending" }).text, /已保存，正在应用/);
  assert.equal(configApplyNotice({ revision: 1, state: "failed", error: "连接中断" }).tone, "error");
  assert.match(configApplyNotice({ revision: 1, state: "failed", error: "连接中断" }).text, /连接中断/);
  assert.match(configApplyNotice({ revision: 1, state: "restart_required" }).text, /重启/);
  assert.equal(configApplyNotice({ revision: 1, state: "saved" }).text, "配置已保存");
});

function clockHarness() {
  let now = 0, id = 0;
  const timers = new Map();
  return { clock: { now: () => now, setTimeout: (fn, delay) => { timers.set(++id, { fn, at: now + delay }); return id; }, clearTimeout: (key) => timers.delete(key) },
    async tick() { const entry = [...timers].sort((a, b) => a[1].at - b[1].at)[0]; if (!entry) return false; timers.delete(entry[0]); now = entry[1].at; entry[1].fn(); await new Promise(setImmediate); return true; },
    timers };
}

test("application polling is lightweight, ignores older terminal states and stops on completion", async () => {
  const harness = clockHarness(), requests = [];
  const states = [{ revision: 1, state: "applied" }, { revision: 2, state: "pending" }, { revision: 2, state: "applied" }];
  const scheduler = createStatusPollScheduler(async (probe) => { requests.push(probe); return { running: true, configApply: states.shift() }; }, harness.clock);
  scheduler.add(createStatusPollTask({ kind: "configApply", delays: [10], refreshesInjectionStatus: false, pending: (next) => configApplyPending(next.configApply, 2) }, 100, 0));
  while (await harness.tick()) {}
  assert.deepEqual(requests, [false, false, false]);
});

for (const failure of [false, true]) test(`application polling is bounded on ${failure ? "query failures" : "timeout"} and can be restarted`, async () => {
  const harness = clockHarness();
  let exhausted = 0;
  const scheduler = createStatusPollScheduler(async () => { if (failure) throw Error("offline"); return { running: true, configApply: { revision: 2, state: "pending" } }; }, harness.clock);
  const task = () => createStatusPollTask({ kind: "configApply", delays: [10], refreshesInjectionStatus: false, pending: () => true, onExhausted: () => exhausted++ }, failure ? 1000 : 30, harness.clock.now());
  scheduler.add(task());
  while (await harness.tick()) {}
  assert.equal(exhausted, 1);
  scheduler.add(task());
  assert.equal(harness.timers.size, 1);
  scheduler.clear();
  assert.equal(harness.timers.size, 0);
});
