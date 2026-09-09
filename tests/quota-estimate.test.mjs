import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const quota = await loadTypeScriptModule(new URL("../src/quotaEstimate.ts", import.meta.url));
const near = (actual, expected) => assert.ok(Math.abs(actual - expected) < 1e-9, `${actual} != ${expected}`);

const usageRows = (...items) => {
  const rows = new Map();
  for (const item of items) quota.addQuotaUsage(rows, { serviceTier: "default", ...item });
  return [...rows.values()];
};
test("official prices split cache and context rates without double charging", () => {
  const rows = usageRows(
    { model: "gpt-5.6-sol", inputTokens: 200_000, cachedInputTokens: 100_000,
      cacheCreationInputTokens: 20_000, outputTokens: 10_000, totalTokens: 210_000 },
    { model: "GPT-5.6-SOL", inputTokens: 300_000, cachedInputTokens: 100_000,
      cacheCreationInputTokens: 20_000, outputTokens: 10_000, totalTokens: 310_000 },
    { model: "unknown", inputTokens: 10, outputTokens: 2, totalTokens: 12 },
    { model: "gpt-5.4", inputTokens: null, outputTokens: 1000 });
  const [row, long, unknown, missing] = rows;
  near(row.cost, 0.66); near(row.cacheCost, 0.14); near(row.cacheSaving, 0.34);
  near(row.readCost, .04); near(row.writeCost, .1); near(row.inputCost, .32); near(row.outputCost, .2);
  near(long.cost, 2.02); assert.equal(long.context, ">272K"); assert.notEqual(row.key, long.key);
  near(missing.cost, 0.015); assert.equal(unknown.cost, 0); assert.equal(unknown.unpriced, 1);
  const total = quota.sumQuotaRows(rows);
  near(total.cost, 2.695); assert.equal(total.tokens, 520_012); assert.equal(total.missing, 1);
  near(total.cost, total.inputCost + total.outputCost + total.readCost + total.writeCost);
});

test("cache without separate price is display only; malformed counts cannot create negative charges", () => {
  const [row, snapshot, unknown, prototype] = usageRows(
    { model: "gpt-5.4-pro", inputTokens: 100, cachedInputTokens: 500,
      cacheCreationInputTokens: 100, outputTokens: -10, totalTokens: NaN },
    { model: "gpt-4o-2024-05-13", inputTokens: 1000, outputTokens: 1000 },
    { model: "gpt-5.4-unreleased", inputTokens: 1000 }, { model: "constructor", inputTokens: 1000 });
  near(row.cost, 0.003); near(row.cacheSaving, 0); assert.equal(row.tokens, 0);
  near(snapshot.cost, 0.02); assert.equal(unknown.unpriced, 1); assert.equal(prototype.unpriced, 1);
});

test("cache writes distinguish missing records from explicit zero and preserve billed totals", () => {
  const base = { model: "gpt-5.6-sol", inputTokens: 1000, outputTokens: 0, totalTokens: 1000 };
  const rows = usageRows(base, { ...base, cacheCreationInputTokens: null },
    { ...base, cacheCreationInputTokens: 0 }, { ...base, cacheCreationInputTokens: 200 });
  const total = quota.sumQuotaRows(rows);
  assert.equal(total.calls, 4); assert.equal(total.missingWrites, 2);
  assert.equal(total.writes, 200); assert.equal(total.missing, 0);
  near(total.writeCost, .001); near(total.inputCost, .0152); near(total.cost, .0162);
});

test("tiers have independent prices; actual fallback wins and unconfirmed requests stay separate", () => {
  const base = { model: "gpt-5.5", inputTokens: 100_000, outputTokens: 10_000, totalTokens: 110_000 };
  const rows = usageRows(
    base, { ...base, serviceTier: "priority" }, { ...base, serviceTier: "fast" },
    { ...base, requestedServiceTier: "priority" },
    { ...base, serviceTier: null, requestedServiceTier: "fast" },
    { ...base, serviceTier: null, requestedServiceTier: "auto" },
    { ...base, serviceTier: null }, { ...base, serviceTier: "future", requestedServiceTier: "fast" });
  assert.equal(rows.length, 5);
  const standard = rows.find(r => r.tier === "Standard" && r.source === "响应确认");
  const fast = rows.find(r => r.tier === "Fast" && r.source === "响应确认");
  near(standard.cost, 1.6); assert.equal(standard.calls, 2);
  near(fast.cost, 4); assert.equal(fast.calls, 2);
  const assumed = rows.find(r => r.assumed);
  near(assumed.cost, 2); assert.notEqual(assumed.key, fast.key);
  const defaulted = rows.find(r => r.source === "默认档位（未记录）");
  assert.equal(defaulted.tier, "Standard"); assert.equal(defaulted.calls, 2);
  near(defaulted.cost, 1.6); assert.equal(defaulted.assumed, 2);
  assert.equal(quota.sumQuotaRows(rows).unpriced, 1);
  const [mini] = usageRows({ model: "gpt-5-mini", serviceTier: "fast", inputTokens: 100_000 });
  near(mini.cost, .045); // 1.8x, not a universal 2x Fast multiplier.
});

test("Batch and Flex keep distinct cache prices, unsupported long Fast never falls back", () => {
  const [batch, flex, boundary, long, unavailable, dated] = usageRows(
    { model: "o4-mini", serviceTier: "batch", inputTokens: 100_000, cachedInputTokens: 100_000 },
    { model: "o4-mini", serviceTier: "flex", inputTokens: 100_000, cachedInputTokens: 100_000 },
    { model: "gpt-5.6-sol", serviceTier: "fast", inputTokens: 272_000 },
    { model: "gpt-5.6-sol", serviceTier: "fast", inputTokens: 272_001 },
    { model: "gpt-5.5", serviceTier: "fast", inputTokens: 300_000 },
    { model: "gpt-5.4-2026-03-05", serviceTier: "flex", inputTokens: 100_000, cachedInputTokens: 100_000 });
  near(batch.readCost, .055); near(flex.readCost, .0138);
  near(boundary.cost, 272_000 * 8 / 1e6); near(long.cost, 272_001 * 16 / 1e6);
  assert.equal(unavailable.unpriced, 1); assert.equal(unavailable.cost, 0);
  near(dated.readCost, .013);
});

test("upstream regions do not split usage or add any charge", () => {
  const base = { model: "gpt-5.6-sol", inputTokens: 100_000, cachedInputTokens: 50_000, outputTokens: 1000 };
  const rows = usageRows(
    { ...base, upstreamAuthority: "us.api.openai.com:443" },
    { ...base, upstreamAuthority: "api.openai.com" },
    { ...base, upstreamAuthority: "eu.api.openai.com" });
  assert.equal(rows.length, 1); near(rows[0].cost, .72);
  near(rows[0].cacheSaving, .54);
  assert.equal("regionCost" in rows[0], false);
});

test("official weekly window determines log bounds and rejects missing or expired data", () => {
  const now = 1788969600000;
  const fetchedAt = now / 1000 - 30;
  const weekly = { windowMinutes: 10080, usedPercent: 80.5, resetsAt: now / 1000 + 86400 };
  const snapshot = { status: "ok", fetchedAt, primary: { windowMinutes: 300, usedPercent: 10 }, secondary: weekly };
  const period = quota.quotaPeriod(snapshot, now);
  assert.deepEqual(period, { fromUnixMs: weekly.resetsAt * 1000 - quota.WEEK_MS,
    toUnixMs: fetchedAt * 1000, resetsAt: weekly.resetsAt * 1000, usedPercent: 80.5 });
  assert.equal(quota.quotaPeriod({ ...snapshot, primary: weekly, secondary: null }, now).usedPercent, 80.5);
  for (const invalid of [
    { status: "error", message: "offline" }, { ...snapshot, secondary: null },
    { ...snapshot, fetchedAt: undefined }, { ...snapshot, fetchedAt: now / 1000 + 10 },
    { ...snapshot, fetchedAt: weekly.resetsAt - 604800 },
    ...[null, undefined, -1, 101, NaN, "80"].map(usedPercent => ({ ...snapshot, secondary: { ...weekly, usedPercent } })),
    ...[undefined, 0, now / 1000, NaN].map(resetsAt => ({ ...snapshot, secondary: { ...weekly, resetsAt } })),
  ]) assert.throws(() => quota.quotaPeriod(invalid, now));
  assert.equal(quota.quotaPeriod({ ...snapshot, secondary: { ...weekly, usedPercent: 0 } }, now).usedPercent, 0);
});

test("weekly limit uses actual consumed percentage rather than projected weekly spend", () => {
  assert.deepEqual(quota.projectQuota(80, quota.WEEK_MS, 80), { weekly: 80, limit: 100, remaining: 20 });
  assert.deepEqual(quota.projectQuota(80, quota.WEEK_MS / 7, 80), { weekly: 560, limit: 100, remaining: 20 });
  assert.equal(quota.projectQuota(80, quota.WEEK_MS, 0).limit, null);
  assert.equal(quota.projectQuota(80.5, quota.WEEK_MS / 2, 80.5).limit, 100);
  assert.equal(quota.projectQuota(100, quota.WEEK_MS / 2, 100).remaining, 0);
  for (const value of [-1, 100.01, NaN, Infinity, "80", null]) assert.throws(() => quota.projectQuota(80, quota.WEEK_MS, value));
  assert.throws(() => quota.projectQuota(80, 0, 80));
});

test("read all cursor pages, retain every model, reject unavailable/failed/stuck pagination and cancel", async () => {
  let calls = 0;
  const rows = await quota.loadQuotaRows(async cursor => {
    calls++;
    if (!cursor) return { queryable: true, items: Array.from({ length: 60 }, (_, i) => ({ model: `model-${i}` })),
      hasMore: true, nextCursor: { timestampUnixMs: 100, requestId: "a" } };
    return { queryable: true, items: [{ model: "gpt-5.4", inputTokens: 1000 }], hasMore: false };
  }, () => true, () => {});
  assert.equal(calls, 2); assert.equal(rows.length, 61);
  await assert.rejects(quota.loadQuotaRows(async () => ({ queryable: false }), () => true, () => {}));
  await assert.rejects(quota.loadQuotaRows(async () => { throw new Error("offline"); }, () => true, () => {}), /offline/);
  await assert.rejects(quota.loadQuotaRows(async () => ({ queryable: true, items: [{}], hasMore: true,
    nextCursor: { timestampUnixMs: 100, requestId: "a" } }), () => true, () => {}), /分页异常/);
  let active = true;
  assert.equal(await quota.loadQuotaRows(async () => { active = false; return {}; }, () => active, () => assert.fail()), null);
});
