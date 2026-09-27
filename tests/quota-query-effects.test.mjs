import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const quota = await loadTypeScriptModule(new URL("../src/quotaEstimate.ts", import.meta.url));
const source = await readFile(new URL("../src/QuotaEstimateDialog.tsx", import.meta.url), "utf8");
const tree = ts.createSourceFile("QuotaEstimateDialog.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let effect;
const helpers = [];
function visit(node) {
  if (ts.isCallExpression(node) && node.expression.getText(tree) === "useEffect") effect = node.getText(tree);
  if (ts.isFunctionDeclaration(node) && ["estimateTargets", "officialAccountLabel"].includes(node.name?.text)) helpers.push(node.getText(tree));
  ts.forEachChild(node, visit);
}
visit(tree);
const compiled = ts.transpileModule(`${helpers.join("\n")}\n${effect}`, { compilerOptions: { target: ts.ScriptTarget.ES2020 } }).outputText;
const now = 1788969600000;
const snapshot = (extra = {}) => ({ status: "ok", fetchedAt: now / 1000 - 30,
  secondary: { windowMinutes: 10080, usedPercent: 50, resetsAt: now / 1000 + 86400 }, ...extra });
const bucket = { model: "gpt-5.6-sol", serviceTier: "default", requestedServiceTier: null, longContext: false,
  calls: 10000, inputTokens: 1000000, outputTokens: 0, totalTokens: 1000000,
  cachedInputTokens: 0, cacheCreationInputTokens: 0, cacheHits: 0, missingUsage: 0, missingCacheCreation: 0,
  billedCachedInputTokens: 0, billedCacheCreationInputTokens: 0 };
const result = (calls = 10000) => ({ queryable: true, totalCalls: calls, groups: calls ? [{ ...bucket, calls }] : [] });
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };

function run(options = {}) {
  const calls = [], states = [], delays = [];
  let cleanup;
  let queryCount = 0;
  let settled = false;
  const record = (key, value) => { states.push({ key, value }); if (key === "loading" && value === false) settled = true; };
  const gate = async (stage, value) => {
    if (stage === options.pause) { options.reached.resolve(); await options.release.promise; }
    return value;
  };
  const context = {
    WEEK_MS: quota.WEEK_MS, Date: { now: () => now }, revision: options.revision ?? 0,
    USAGE_QUERY_STAGGER_MS: 200, errorText: cause => cause.message || String(cause), maskEmail: value => value,
    quotaPeriod: value => quota.quotaPeriod(value, now), quotaAggregateRows: quota.quotaAggregateRows,
    estimateQuotaRows: quota.estimateQuotaRows, sumQuotaRows: quota.sumQuotaRows,
    useEffect: callback => { cleanup = callback(); },
    setLoading: value => record("loading", value), setGroups: value => record("groups", value),
    setError: value => record("error", value), setLoaded: value => record("loaded", value),
    setHealthWarning: value => record("health", value), setRangeEnd: value => record("range", value),
    listOfficialAccounts: () => gate("list", { accounts: options.accounts ?? [{ id: "a", isDefault: true }] }),
    invoke: async (command, args) => {
      assert.equal(command, "query_route_request_log_quota_usage");
      calls.push({ command, args });
      const recent = args.fromUnixMs === now - quota.WEEK_MS && args.toUnixMs === now;
      queryCount++;
      return gate(recent ? "recent" : "exact", options.query?.(args, recent, queryCount) ?? result());
    },
    readAccountUsage: async (accountId, forceRefresh) => {
      calls.push({ command: "official", accountId, forceRefresh });
      const value = await gate("official", options.snapshot ?? snapshot());
      if (options.officialError?.(accountId)) throw new Error("quota offline");
      return value;
    },
    setTimeout: (resolve, duration) => {
      calls.push({ command: "delay", duration }); delays.push(duration);
      void gate("delay").then(resolve);
    },
  };
  new Function(...Object.keys(context), compiled)(...Object.values(context));
  return { calls, states, delays, cancel: () => cleanup(),
    groups: () => states.filter(state => state.key === "groups").at(-1)?.value,
    async done() { for (let i = 0; i < 20 && !settled; i++) await tick(); assert.equal(settled, true); } };
}

test("10000 requests require two aggregate queries with exact account period bounds", async () => {
  const task = run({ revision: 1 }); await task.done();
  assert.deepEqual(task.calls.map(call => call.command), ["query_route_request_log_quota_usage", "official", "query_route_request_log_quota_usage"]);
  const [recent, official, exact] = task.calls;
  assert.deepEqual(recent.args, { officialAccountId: "a", fromUnixMs: now - quota.WEEK_MS, toUnixMs: now, unassignedOnly: false });
  const period = quota.quotaPeriod(snapshot(), now);
  assert.deepEqual(exact.args, { officialAccountId: "a", fromUnixMs: period.fromUnixMs, toUnixMs: period.toUnixMs, unassignedOnly: false });
  assert.equal(official.forceRefresh, true);
  assert.equal(task.groups()[0].total.calls, 10000);
  assert.equal(task.groups()[0].estimate.result.limit, 8);
});

test("account quotas stay isolated and serial with 200ms delay, empty accounts skip official reads", async () => {
  const task = run({ accounts: [{ id: "a" }, { id: "empty" }, { id: "b" }],
    query: args => result(args.officialAccountId === "empty" ? 0 : 10000) });
  await task.done();
  assert.deepEqual(task.calls.filter(call => call.command === "official").map(call => call.accountId), ["a", "b"]);
  assert.deepEqual(task.delays, [200]);
  assert.equal(task.calls.filter(call => call.command === "query_route_request_log_quota_usage").length, 5);
  assert.deepEqual(task.groups().map(group => group.total.calls), [10000, 0, 10000]);
  assert.ok(task.calls.findIndex(call => call.command === "delay") > task.calls.findIndex(call => call.command === "official"));
});

test("quota failure keeps recent aggregate and allows following accounts to finish", async () => {
  const task = run({ accounts: [{ id: "a" }, { id: "b" }], officialError: accountId => accountId === "a" });
  await task.done();
  const [first, second] = task.groups();
  assert.equal(first.error, "quota offline"); assert.equal(first.estimate, null); assert.equal(first.total.calls, 10000);
  assert.ok(second.estimate); assert.equal(second.error, "");
});

test("stale quota keeps snapshot cutoff, warning and health from aggregate response", async () => {
  const task = run({ snapshot: snapshot({ stale: true, message: "cached quota", fetchedAt: now / 1000 - 3600 }),
    query: () => ({ ...result(), recordingHealth: { active: false, sampleRatePerMillion: 1000000, droppedFull: 0, droppedClosed: 0, writeDropped: 0, writeFailures: 0 } }) });
  await task.done();
  assert.equal(task.calls.at(-1).args.toUnixMs, now - 3600000);
  assert.equal(task.groups()[0].usageWarning, "cached quota");
  assert.ok(task.states.some(state => state.key === "health" && state.value));
});

test("empty and unavailable logs do not read official quota", async () => {
  for (const response of [result(0), { queryable: false }]) {
    const task = run({ query: () => response }); await task.done();
    assert.equal(task.calls.length, 1);
    if (!response.queryable) assert.match(task.states.findLast(state => state.key === "error").value, /暂不可查询/);
  }
});

test("no stored accounts retain provider fallback and clamp future snapshot cutoff", async () => {
  const task = run({ accounts: [], snapshot: snapshot({ fetchedAt: now / 1000 + .5 }) });
  await task.done();
  assert.equal(task.calls[0].args.provider, "openai");
  assert.equal(task.calls.at(-1).args.toUnixMs, now);
  assert.equal(task.calls[1].accountId, undefined);
});

test("empty official period intersection is handled locally without a second query", async () => {
  const task = run({ snapshot: snapshot({ fetchedAt: now / 1000 + .5,
    secondary: { windowMinutes: 10080, usedPercent: 50, resetsAt: (now + quota.WEEK_MS + 100) / 1000 } }) });
  await task.done();
  assert.equal(task.calls.length, 2);
  assert.equal(task.groups()[0].total.calls, 0);
  assert.equal(task.groups()[0].estimate.result, null);
});

test("invalid quota and exact aggregation failure preserve recent rows without a projection", async () => {
  for (const options of [
    { snapshot: snapshot({ status: "error", message: "invalid quota" }) },
    { query: (_args, recent) => recent ? result() : { queryable: false } },
    { query: (_args, recent) => { if (!recent) throw new Error("aggregate offline"); return result(); } },
  ]) {
    const task = run(options); await task.done();
    assert.equal(task.groups()[0].total.calls, 10000);
    assert.equal(task.groups()[0].estimate, null);
    assert.ok(task.groups()[0].error);
  }
});

for (const stage of ["list", "recent", "official", "delay", "exact"]) {
  for (const reject of stage === "delay" ? [false] : [false, true]) test(`cancelled ${stage} ${reject ? "failure" : "success"} cannot write state or launch later work`, async () => {
    const reached = deferred(), release = deferred();
    const task = run({ pause: stage, reached, release, accounts: [{ id: "a" }, { id: "b" }] });
    await reached.promise;
    task.cancel();
    const states = task.states.length, calls = task.calls.length;
    // Timers do not reject; releasing them still checks the effect's cancellation guard.
    if (reject && stage !== "delay") release.reject(new Error("late failure")); else release.resolve();
    await tick(); await tick();
    assert.equal(task.states.length, states);
    assert.equal(task.calls.length, calls);
  });
}
