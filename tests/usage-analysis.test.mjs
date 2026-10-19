import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
import {
  autoStubModule,
  collectElements,
  createModuleGraph,
  elementProps,
  elementType,
  textContent,
} from "./helpers/jsx-tree.mjs";

const { usageQuery, usageSlices, usageCoverage, formatUsageNumber, observeUsageRequest, usageTrend, usageHeatmap } = await loadTypeScriptModule(new URL("../src/usageAnalysis.ts", import.meta.url));
const group = (key, tokens, total = 1) => ({ key, totalTokensSum: tokens, total });

test("每日热力图遵循UTC边界、闰日与首尾部分日，不包含结束日", () => {
  const result = usageHeatmap({ fromUnixMs: Date.UTC(2024, 1, 28, 12), toUnixMs: Date.UTC(2024, 2, 2), dailyTrend: [] });
  assert.deepEqual(result.days.map((day) => new Date(day.timestampUnixMs).toISOString().slice(0, 10)), ["2024-02-28", "2024-02-29", "2024-03-01"]);
  assert.deepEqual(result.days.map((day) => day.partial), [true, false, false]);
  assert.equal(usageHeatmap({ fromUnixMs: Date.UTC(2024, 1, 28), toUnixMs: Date.UTC(2024, 1, 28, 1), dailyTrend: [] }).days[0].partial, true);
});

test("每日热力图区分无请求、全部未知、已知零与部分已知，并按可见年计算色阶", () => {
  const day = 86400000;
  const result = usageHeatmap({ fromUnixMs: 0, toUnixMs: 5 * day, dailyTrend: [
    { timestampUnixMs: day, total: 2, totalTokensSum: null, totalTokensKnownCount: 0 },
    { timestampUnixMs: 2 * day, total: 1, totalTokensSum: 0, totalTokensKnownCount: 1 },
    { timestampUnixMs: 3 * day, total: 2, totalTokensSum: 25, totalTokensKnownCount: 1 },
    { timestampUnixMs: 4 * day, total: 1, totalTokensSum: 100, totalTokensKnownCount: 1 },
  ] });
  assert.deepEqual(result.days.map((day) => day.level), ["empty", "unknown", "zero", "1", "4"]);
  assert.equal(result.days[3].totalTokensKnownCount, 1);
  assert.equal(result.peak, 100);
  assert.equal(usageHeatmap({ fromUnixMs: 0, toUnixMs: day, dailyTrend: [{ timestampUnixMs: 0, total: 1, totalTokensSum: null, totalTokensKnownCount: 0 }] }).peak, null);
});

test("全历史按年限制最多366天，UTC跨年与无每日合同不伪造数据", () => {
  const stats = { fromUnixMs: Date.UTC(2023, 11, 31, 20), toUnixMs: Date.UTC(2025, 0, 1), dailyTrend: [] };
  const result = usageHeatmap(stats);
  assert.deepEqual(result.years, [2024, 2023]);
  assert.equal(result.days.length, 366);
  assert.equal(usageHeatmap(stats, 2023).days.length, 1);
  assert.equal(usageHeatmap(stats, 2020).year, 2024);
  assert.equal(usageHeatmap({ ...stats, dailyTrend: undefined }).available, false);
  assert.equal(usageHeatmap(stats).available, true);
  assert.equal(usageHeatmap({ ...stats, toUnixMs: stats.fromUnixMs }).days.length, 0);
  assert.equal(usageHeatmap({ ...stats, fromUnixMs: NaN }).available, false);
  const shortCrossYear = usageHeatmap({ fromUnixMs: Date.UTC(2023, 11, 28, 12), toUnixMs: Date.UTC(2024, 0, 4, 12), dailyTrend: [] });
  assert.equal(shortCrossYear.days.length, 8);
  assert.equal(shortCrossYear.years.length, 1);
  assert.equal(new Date(shortCrossYear.days[0].timestampUnixMs).getUTCFullYear(), 2023);
});

test("各时间范围固定按模型与Token排序；全部历史不传时间或游标限制", () => {
  const now = 2_000_000_000_000;
  assert.deepEqual(usageQuery("7d", now), { groupBy: "model", groupSort: "tokens", includeDailyTrend: true, fromUnixMs: now - 7 * 86400000, toUnixMs: now });
  assert.deepEqual(usageQuery("30d", now), { groupBy: "model", groupSort: "tokens", includeDailyTrend: true, fromUnixMs: now - 30 * 86400000, toUnixMs: now });
  assert.deepEqual(usageQuery("all", now), { groupBy: "model", groupSort: "tokens", includeDailyTrend: true, allTime: true });
});

test("前12组按Token排序，其他包含后端截断残量且不改变输入", () => {
  const groups = Array.from({ length: 50 }, (_, index) => group(`model-${index}`, index + 1));
  const original = structuredClone(groups);
  const slices = usageSlices({ groups, totalTokensSum: 2000, total: 75, groupsTruncated: true });
  assert.equal(slices.length, 13);
  assert.equal(slices[0].key, "model-49");
  assert.equal(slices.at(-1).other, true);
  assert.equal(slices.at(-1).requests, 63);
  assert.equal(slices.reduce((sum, slice) => sum + slice.tokens, 0), 2000);
  assert.deepEqual(groups, original);
});

test("未上报不是零，未知的其他组不伪造为零；已知零仍保留", () => {
  const groups = Array.from({ length: 13 }, (_, index) => group(`m${index}`, null));
  assert.ok(usageSlices({ groups, totalTokensSum: null, total: 13, groupsTruncated: false }).every((slice) => slice.tokens === null));
  assert.equal(usageSlices({ groups: [group("zero", 0)], totalTokensSum: 0, total: 1, groupsTruncated: false })[0].tokens, 0);
  assert.equal(formatUsageNumber(null), "—");
  assert.equal(formatUsageNumber(NaN), "—");
  assert.equal(formatUsageNumber(0), "0");
  assert.equal(usageCoverage(0, 0), "—");
  assert.equal(usageCoverage(4, 3), "75.0%");
});

test("切换范围清空旧状态，过期成功与失败均不覆盖新结果", async () => {
  let resolveOld;
  const old = new Promise((resolve) => { resolveOld = resolve; });
  const events = [];
  const cancel = observeUsageRequest(() => old, (state) => events.push(state));
  assert.deepEqual(events[0], { loading: true, data: null, error: "" });
  cancel();
  observeUsageRequest(() => Promise.resolve({ total: 30 }), (state) => events.push(state));
  await new Promise(setImmediate);
  resolveOld({ total: 7 });
  await new Promise(setImmediate);
  assert.equal(events.at(-1).data.total, 30);
  assert.equal(events.length, 3);
  const ignored = [];
  const stop = observeUsageRequest(() => Promise.reject(new Error("旧错误")), (state) => ignored.push(state));
  stop();
  await new Promise(setImmediate);
  assert.equal(ignored.length, 1);
});

test("趋势按真实时间定位并裁剪首尾分桶，不重叠或超出范围", () => {
  const result = usageTrend({ fromUnixMs: 50, toUnixMs: 250, bucketMs: 100,
    trend: [0, 100, 200, 300].map((timestampUnixMs) => ({ timestampUnixMs, totalTokensSum: 10 })) });
  assert.equal(result.peak, 10);
  assert.deepEqual(result.bars.map(({ x, width, height }) => ({ x, width, height })), [
    { x: 0, width: 199, height: 128 }, { x: 200, width: 399, height: 128 }, { x: 600, width: 199, height: 128 },
  ]);
});

test("趋势峰值区分未上报与已知零，空数据和无效区间安全返回", () => {
  const input = { fromUnixMs: 0, toUnixMs: 100, bucketMs: 100, trend: [{ timestampUnixMs: 0, totalTokensSum: null }] };
  assert.equal(usageTrend(input).peak, null);
  assert.equal(usageTrend({ ...input, trend: [{ timestampUnixMs: 0, totalTokensSum: 0 }] }).peak, 0);
  assert.deepEqual(usageTrend({ ...input, trend: [] }), { peak: null, bars: [] });
  assert.deepEqual(usageTrend({ ...input, toUnixMs: 0 }), { peak: null, bars: [] });
  assert.deepEqual(usageTrend({ ...input, bucketMs: 0 }), { peak: null, bars: [] });
});

test("趋势把未上报用量的分桶标记为 unknown，不画成零用量", () => {
  const result = usageTrend({
    fromUnixMs: 0, toUnixMs: 300, bucketMs: 100,
    trend: [
      { timestampUnixMs: 0, total: 1, totalTokensSum: 10 },
      { timestampUnixMs: 100, total: 3, totalTokensSum: null },
      { timestampUnixMs: 200, total: 2, totalTokensSum: 0 },
    ],
  });
  assert.equal(result.peak, 10, "峰值只按已知值计算");
  assert.deepEqual(
    result.bars.map(({ tokensUnknown, height }) => ({ tokensUnknown, height })),
    [
      { tokensUnknown: false, height: 128 },
      { tokensUnknown: true, height: 0 },
      { tokensUnknown: false, height: 0 },
    ],
    "有请求但未上报用量的分桶必须带 unknown 标记，与已知零用量区分",
  );
});

test("同步异常与后端失败终止loading且可重新加载；不可查询保留reason", async () => {
  const events = [];
  observeUsageRequest(() => { throw new Error("统计失败"); }, (state) => events.push(state));
  await new Promise(setImmediate);
  assert.deepEqual(events.at(-1), { loading: false, data: null, error: "统计失败" });
  observeUsageRequest(() => Promise.resolve({ queryable: false, reason: "ndjson_not_queryable" }), (state) => events.push(state));
  await new Promise(setImmediate);
  assert.equal(events.at(-1).data.reason, "ndjson_not_queryable");
  assert.equal(events.at(-1).loading, false);
});

// 渲染层：未知用量的分桶必须与零用量区分，用低矮虚线柱加明确文案呈现。
const panelHooks = { effects: [], hooks: [], cursor: 0 };
const panelReact = {
  useMemo(factory) { return factory(); },
  useState(initial) {
    const index = panelHooks.cursor++;
    if (!(index in panelHooks.hooks))
      panelHooks.hooks[index] = typeof initial === "function" ? initial() : initial;
    return [panelHooks.hooks[index], (next) => {
      panelHooks.hooks[index] = typeof next === "function" ? next(panelHooks.hooks[index]) : next;
    }];
  },
  useRef(initial) {
    const index = panelHooks.cursor++;
    if (!(index in panelHooks.hooks)) panelHooks.hooks[index] = { current: initial };
    return panelHooks.hooks[index];
  },
  useCallback(callback) { return callback; },
  useEffect(effect, deps) {
    const index = panelHooks.cursor++;
    const previous = panelHooks.hooks[index];
    if (previous && deps.every((value, i) => Object.is(value, previous.deps[i]))) return;
    panelHooks.effects.push(() => { previous?.cleanup?.(); panelHooks.hooks[index] = { deps, cleanup: effect() }; });
  },
};

const panelStats = {
  queryable: true, fromUnixMs: 0, toUnixMs: 300, bucketMs: 100, total: 6,
  succeededCount: 6, failedCount: 0, incompleteCount: 0, cancelledCount: 0, successRate: 1,
  avgDuration: null, avgTtft: null, avgRouterPreUpstream: null, avgUpstreamHeader: null,
  avgUpstreamFirstByte: null, avgDownstreamFirstContent: null, avgQueueDelay: null,
  inputTokensSum: null, outputTokensSum: null, totalTokensSum: 10, cachedTokensSum: null,
  usageReportedCount: 1, totalTokensKnownCount: 1, groupsTruncated: false,
  groups: [{ ...group("gpt-5", 10, 1) }], dailyTrend: [], recordingHealth: null,
  trend: [0, 100, 200].map((timestampUnixMs, index) => ({
    timestampUnixMs, total: index + 1,
    totalTokensSum: [10, null, 0][index],
    avgDuration: null, avgTtft: null, avgDownstreamFirstContent: null,
  })),
};

const uiStubs = autoStubModule("ui");
const timelineGraph = createModuleGraph(new URL("../src/UsageTimeline.tsx", import.meta.url), {
  autoStub: true,
  stubs: { react: panelReact, "./components/ui": uiStubs },
});
const panelGraph = createModuleGraph(new URL("../src/UsageAnalysisPanel.tsx", import.meta.url), {
  autoStub: true,
  stubs: {
    react: panelReact,
    "./api": { invoke: async () => panelStats },
    "./appUtils": { withTimeout: (promise) => promise },
    "./components/ui": uiStubs,
    "./UsageHeatmap": autoStubModule("heatmap"),
    "./UsageTimeline": timelineGraph.exports,
  },
});

test("用量趋势渲染把未上报的分桶画成虚线标记并标注用量未上报", async () => {
  panelHooks.cursor = 0;
  panelHooks.effects.length = 0;
  const props = { onBack: () => {} };
  panelGraph.exports.UsageAnalysisPanel(props);
  panelHooks.effects.splice(0).forEach((effect) => effect());
  await new Promise(setImmediate);

  const render = () => {
    panelHooks.cursor = 0;
    return panelGraph.exports.UsageAnalysisPanel(props);
  };
  let tree = render();
  const [barsButton] = collectElements(
    tree,
    (element) => elementType(element) === uiStubs.Button && textContent(element) === "直方图",
  );
  assert.ok(barsButton, "必须能切到直方图");
  elementProps(barsButton).onPress?.();
  tree = render();

  const [timeline] = collectElements(tree, (element) => elementType(element) === timelineGraph.exports.UsageTimeline);
  assert.ok(timeline, "直方图入口必须渲染交互时间轴");
  const chartTree = timelineGraph.exports.UsageTimeline(elementProps(timeline));
  const bars = collectElements(chartTree, (element) => elementType(element) === "button" && elementProps(element)["data-bucket-index"] != null);
  assert.equal(bars.length, 3);
  const [known, unknown, zero] = bars.map((bar) => ({
    className: elementProps(bar).className,
    title: elementProps(bar)["aria-label"],
  }));
  assert.match(known.title, /10 Token/);
  assert.match(unknown.className, /is-unknown/, "未上报分桶有独立的虚线状态");
  assert.match(unknown.title, /用量未上报/);
  assert.match(zero.className, /is-zero/, "已知零有独立标记");
  assert.match(zero.title, /0 Token/);
  assert.doesNotMatch(zero.title, /用量未上报/);
});
