import assert from "node:assert/strict";
import test from "node:test";
import ts from "typescript";

import { readSource } from "./helpers/read-source.mjs";

test("request log provider labels use route short names and distinguish official accounts", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
  const routeNames = await readSource("src/routeShortNames.ts");
  const source = [
    routeNames.match(/export function fallbackRouteShortName\([\s\S]*?\n\}/)?.[0],
    routeNames.match(/export function formatRouteName\([\s\S]*?\n\}/)?.[0],
    viewer.match(/type RequestLogProviderIndex = \{[\s\S]*?\n\};/)?.[0],
    viewer.match(/function buildRequestLogProviderIndex\([\s\S]*?\n\}/)?.[0],
    viewer.match(/function requestLogProviderLabel\([\s\S]*?\n\}/)?.[0],
  ];
  source.forEach((part) => assert.ok(part));
  const compiled = ts.transpileModule(source.join("\n").replaceAll("export function", "function"), {}).outputText;
  const { buildIndex, label } = new Function(`${compiled}; return { buildIndex: buildRequestLogProviderIndex, label: requestLogProviderLabel };`)();
  const profiles = [
    { id: "local-a", sourceProviderId: "route-a", name: "线路甲", shortName: "甲" },
    { id: "route-b", name: "", shortName: "乙" },
    { id: "official-a", sourceProviderId: "official", name: "官方甲", shortName: "官甲", officialAccountId: "account-a" },
    { id: "official-b", sourceProviderId: "official", name: "官方乙", shortName: "官乙", officialAccountId: "account-b" },
  ];
  const index = buildIndex(profiles);
  for (const args of [[profiles], [profiles, index]]) {
    assert.equal(label({ provider: "route-a", providerName: "route-a" }, ...args), "[甲] 线路甲");
    assert.equal(label({ provider: "local-a" }, ...args), "[甲] 线路甲");
    assert.equal(label({ provider: "route-b" }, ...args), "乙");
    assert.equal(label({ provider: "official", officialAccountId: "account-b" }, ...args), "[官乙] 官方乙");
    assert.equal(label({ provider: "removed", officialAccountId: "account-b" }, ...args), "[官乙] 官方乙");
    assert.equal(label({ provider: "removed", providerName: "历史线路" }, ...args), "历史线路");
    assert.equal(label({ provider: "removed" }, ...args), "removed");
    assert.equal(label({}, ...args), "—");
  }
  assert.equal(label({ provider: "legacy" }, [{ id: "legacy", name: "自建" }]), "自建");
  assert.equal(label({ provider: "empty", providerName: "历史线路" }, [{ id: "empty", name: "", shortName: "" }]), "历史线路");
});

test("request log cache hit rate uses input tokens and preserves unknown usage", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
  const source = viewer.match(/function cacheRateValue\([\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const compiled = ts.transpileModule(source, {}).outputText;
  const cacheRateValue = new Function(`${compiled}; return cacheRateValue;`)();
  assert.equal(cacheRateValue(64268, 32000).toFixed(1), "49.8");
  assert.equal(cacheRateValue(100, 0).toFixed(1), "0.0");
  assert.equal(cacheRateValue(100, 100).toFixed(1), "100.0");
  for (const [input, cached] of [[null, 0], [100, null], [undefined, undefined], [0, 0], [-1, 0], [100, -1], [100, 101], [Infinity, 1], [100, NaN]]) {
    assert.equal(cacheRateValue(input, cached), null);
  }
});

test("request log output speed uses one decimal and preserves unknown values", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
  const source = viewer.match(/function formatTokensPerSecondValue\([\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const compiled = ts.transpileModule(source, {}).outputText;
  const formatValue = new Function(`${compiled}; return formatTokensPerSecondValue;`)();
  assert.equal(formatValue(123.44), "123.4");
  assert.equal(formatValue(123.46), "123.5");
  assert.equal(formatValue(1), "1.0");
  for (const value of [null, undefined, 0, -1, NaN, Infinity, -Infinity]) {
    assert.equal(formatValue(value), null);
  }
});

test("request log preview measures all output after the upstream first byte and aggregates by duration", async () => {
  const preview = await readSource("src/dev/mockApi.ts");
  const durationSource = preview.match(/function eligiblePreviewOutputDurationMs\([\s\S]*?\n\}\n/)?.[0];
  const rowsSource = preview.match(/const previewRouteRequestLogs = Array\.from\([\s\S]*?\n    \}\);/)?.[0];
  const aggregateSource = preview.match(/const aggregate = \(rows: typeof filtered\) => \{[\s\S]*?\n          \};/)?.[0];
  for (const source of [durationSource, rowsSource, aggregateSource]) assert.ok(source);
  const compiled = ts.transpileModule([
    "const previewOfficialAccounts = [];",
    durationSource, rowsSource, aggregateSource,
  ].join("\n"), {}).outputText;
  const { duration, rows, aggregate } = new Function(`${compiled}; return {
    duration: eligiblePreviewOutputDurationMs, rows: previewRouteRequestLogs, aggregate,
  };`)();
  const buffered = rows[0];
  assert.equal(duration(buffered), 110_465);
  assert.equal(buffered.outputTokensPerSecond.toFixed(1), "169.5");
  assert.ok(buffered.reasoningOutputTokens > 0);
  assert.equal(buffered.outputTokensPerSecond, buffered.outputTokens * 1_000 / duration(buffered));
  assert.equal(rows[7].outputTokensPerSecond, undefined);
  assert.equal(rows[8].outputTokensPerSecond, undefined);
  const base = { outputTokens: 100, totalDurationMs: 300, routerPreUpstreamMs: 50, upstreamFirstByteMs: 150 };
  assert.equal(duration(base), 100);
  assert.equal(duration({ ...base, downstreamFirstContentMs: null, ttftMs: 299 }), 100);
  for (const changes of [
    { outputTokens: 99 }, { outputTokens: null }, { outputTokens: NaN },
    { routerPreUpstreamMs: null }, { upstreamFirstByteMs: undefined },
    { routerPreUpstreamMs: -1 }, { upstreamFirstByteMs: -1 },
    { totalDurationMs: -1 }, { totalDurationMs: Infinity },
    { routerPreUpstreamMs: NaN }, { upstreamFirstByteMs: Infinity },
    { totalDurationMs: 299 }, { upstreamFirstByteMs: 301 },
  ]) assert.equal(duration({ ...base, ...changes }), undefined);
  const short = { ...rows[1], ...base, outputTokensPerSecond: 1_000 };
  const invalid = { ...short, upstreamFirstByteMs: null, outputTokens: 999_999 };
  const expected = (buffered.outputTokens + short.outputTokens) * 1_000 / (110_465 + 100);
  assert.equal(aggregate([buffered, short, invalid]).avgOutputTokensPerSecond, expected);
  assert.notEqual(expected, (buffered.outputTokensPerSecond + short.outputTokensPerSecond) / 2);
  assert.equal(aggregate([invalid]).avgOutputTokensPerSecond, null);
  assert.equal(aggregate([]).avgOutputTokensPerSecond, null);
});

test("request log detail labels request body shape and flags empty input arrays", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
  const source = viewer.match(/function requestShapeText\([\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const compiled = ts.transpileModule(source, {}).outputText;
  const shape = new Function(`${compiled}; return requestShapeText;`)();
  assert.equal(shape("array", 2, false), "数组 2 项");
  assert.equal(shape("array", 0, true), "空数组（0 项）");
  assert.equal(shape("absent", 0, false), "无 input 字段");
  assert.equal(shape("null", 0, false), "input 为 null");
  assert.equal(shape("array", 3, true), "数组 3 项 · 带 previous_response_id");
  assert.equal(shape(null, 0, true), null);
});

test("request log viewer is split into its own standalone browser entry", async () => {
  const [entry, overlay] = await Promise.all([
    readSource("src/request-logs-entry.tsx"),
    readSource("src/overlay.tsx"),
  ]);
  assert.match(entry, /sessionStorage\.setItem\(REQUEST_LOG_TOKEN_KEY, hashToken\)/);
  assert.match(entry, /fetch\(`\/codey\/api\/\$\{command\}`/);
  assert.match(entry, /<RequestLogDialog[\s\S]*standalone/);
  assert.doesNotMatch(overlay, /RequestLogDialog/);
});
