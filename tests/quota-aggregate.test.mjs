import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
const quota = await loadTypeScriptModule(new URL("../src/quotaEstimate.ts", import.meta.url));

// Encode the backend wire contract independently of the frontend pricing routine.
function aggregate(items) {
  const groups = new Map();
  const count = value => typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.trunc(value) : 0;
  for (const item of items) {
    const model = item.model?.trim() || item.requestedModel?.trim() || "未知模型";
    const input = count(item.inputTokens), cached = count(item.cachedInputTokens), writes = count(item.cacheCreationInputTokens);
    const serviceTier = item.serviceTier?.trim() || null, requestedServiceTier = item.requestedServiceTier?.trim() || null;
    const longContext = input > 272000;
    const key = JSON.stringify([model.toLowerCase(), serviceTier, requestedServiceTier, longContext]);
    const group = groups.get(key) ?? { model, serviceTier, requestedServiceTier, longContext, calls: 0,
      inputTokens: 0, outputTokens: 0, totalTokens: 0, cachedInputTokens: 0, cacheCreationInputTokens: 0,
      cacheHits: 0, missingUsage: 0, missingCacheCreation: 0, billedCachedInputTokens: 0, billedCacheCreationInputTokens: 0 };
    group.calls++; group.inputTokens += input; group.outputTokens += count(item.outputTokens);
    group.totalTokens += count(item.totalTokens); group.cachedInputTokens += cached; group.cacheCreationInputTokens += writes;
    group.cacheHits += Number(cached > 0);
    group.missingUsage += Number(item.inputTokens == null || item.outputTokens == null || item.totalTokens == null);
    group.missingCacheCreation += Number(item.cacheCreationInputTokens == null);
    group.billedCachedInputTokens += Math.min(input, cached);
    group.billedCacheCreationInputTokens += Math.min(input - Math.min(input, cached), writes);
    groups.set(key, group);
  }
  return [...groups.values()];
}

test("aggregate pricing equals raw oracle without reclamping or using summed context size", () => {
  const items = [
    ...Array.from({ length: 10000 }, () => ({ model: "gpt-5.6-sol", serviceTier: "default",
      inputTokens: 100, outputTokens: 5, totalTokens: 105, cachedInputTokens: 200, cacheCreationInputTokens: 50 })),
    { model: "gpt-5.6-sol", serviceTier: "default", inputTokens: 100, outputTokens: 0, totalTokens: 100, cacheCreationInputTokens: 200 },
    ...[272000, 272001].map(inputTokens => ({ model: "gpt-5.6-sol", inputTokens })),
    ...[null, 0].map(value => ({ model: "gpt-5.6-sol", inputTokens: value, outputTokens: value, totalTokens: value, cacheCreationInputTokens: value })),
    ...["gpt-5.4-2026-03-05", "gpt-5.5", "unknown", "constructor", "gpt-4o-mini"].flatMap(model =>
      [1, 300000].flatMap(inputTokens => ["fast", "auto", "future", null].map(serviceTier =>
        ({ model, inputTokens, serviceTier, requestedServiceTier: "priority", cachedInputTokens: 100, cacheCreationInputTokens: 100 })))),
    { model: "  ", requestedModel: "gpt-5.6-sol", serviceTier: "DEFAULT", inputTokens: 1 },
    { model: "gpt-5.6-sol", serviceTier: " auto ", requestedServiceTier: " future ", inputTokens: 10 },
  ];
  const raw = quota.quotaRows(items);
  const grouped = quota.quotaAggregateRows(aggregate(items));
  const byKey = new Map(grouped.map(row => [row.key, row]));
  assert.equal(grouped.length, raw.length);
  for (const row of raw) {
    const actual = byKey.get(row.key);
    assert.ok(actual, row.key);
    for (const [key, value] of Object.entries(row)) {
      if (typeof value === "number") assert.ok(Math.abs(actual[key] - value) < 1e-9, `${key}: ${actual[key]} != ${value}`);
      else assert.equal(actual[key], value);
    }
  }
  const short = grouped.find(row => row.model === "gpt-5.6-sol" && row.source === "响应确认");
  assert.equal(short.context, "≤272K");
  assert.equal(short.calls, 10002);
  assert.equal(short.writeCost, .0005);
  assert.equal(grouped.filter(row => row.model === "gpt-4o-mini" && row.tier === "Fast" && row.source === "响应确认").length, 1);
});

test("aggregate rows retain every model and the newest display spelling", () => {
  const items = [
    { model: " GPT-5.6-SOL ", serviceTier: "default", inputTokens: 10 },
    { model: "gpt-5.6-sol", serviceTier: "standard", inputTokens: 20 },
    ...Array.from({ length: 60 }, (_, index) => ({ model: `unknown-${index}`, inputTokens: 1 })),
  ];
  const rows = quota.quotaAggregateRows(aggregate(items));
  assert.equal(rows.length, 61);
  assert.equal(rows.find(row => row.model === "GPT-5.6-SOL").calls, 2);
  assert.deepEqual(rows, quota.quotaRows(items));
});
