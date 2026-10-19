import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
import { autoStubModule, collectElements, createModuleGraph, elementProps, elementType, textContent } from "./helpers/jsx-tree.mjs";

const { usageTimelineView } = await loadTypeScriptModule(new URL("../src/usageTimelineView.ts", import.meta.url));
const bucket = (timestampUnixMs, totalTokensSum, total = 1) => ({ timestampUnixMs, totalTokensSum, total });

test("时间窗口按 UTC 分桶并裁剪首尾，不包括结束边界上的记录", () => {
  const view = usageTimelineView({ fromUnixMs: 50, toUnixMs: 300, bucketMs: 100, trend: [bucket(0, 5), bucket(100, 10), bucket(300, 99)] });
  assert.equal(view.count, 3);
  assert.deepEqual(view.windowAt(0, 3).buckets.map(({ start, end }) => [start, end]), [[50, 100], [100, 200], [200, 300]]);
  assert.equal(view.latestIndex, 1);
  assert.equal(view.peak, 10);
  assert.equal(view.peakIndex, 1);
});

test("未上报、已知零与未返回的无请求分桶保持不同状态", () => {
  const view = usageTimelineView({ fromUnixMs: 0, toUnixMs: 400, bucketMs: 100, trend: [bucket(100, null, 2), bucket(200, 0), bucket(300, 30, 4)] });
  assert.deepEqual(view.windowAt(0, 4).buckets.map(({ total, tokens }) => [total, tokens]), [[0, null], [2, null], [1, 0], [4, 30]]);
  assert.equal(view.peak, 30);
  assert.equal(usageTimelineView({ fromUnixMs: 0, toUnixMs: 100, bucketMs: 100, trend: [bucket(0, null)] }).peak, null);
  assert.equal(usageTimelineView({ fromUnixMs: 0, toUnixMs: 100, bucketMs: 100, trend: [bucket(0, 0)] }).peak, 0);
});

test("全未知时仍定位最近请求，并列峰值定位首个实际分桶", () => {
  const input = { fromUnixMs: 0, toUnixMs: 1000, bucketMs: 100 };
  const unknown = usageTimelineView({ ...input, trend: [bucket(200, null), bucket(700, null)] });
  assert.equal(unknown.latestIndex, 7);
  assert.equal(unknown.peakIndex, null);
  const tied = usageTimelineView({ ...input, trend: [bucket(700, 30), bucket(200, 30), bucket(500, 0)] });
  assert.equal(tied.peakIndex, 2);
  assert.equal(tied.peakCount, 2);
});

test("稀疏数据使用小窗口，窗口平移和越界选择始终限制在查询范围", () => {
  const view = usageTimelineView({ fromUnixMs: 0, toUnixMs: 10000, bucketMs: 100, trend: [bucket(9700, 50), bucket(9800, 10)] });
  assert.equal(view.focusLength, 8);
  assert.equal(view.windowAt(200, 8).offset, 92);
  assert.equal(view.windowAt(-10, 8).offset, 0);
  assert.equal(view.windowAt(200, 1000).length, 100);
  assert.equal(view.bucketAt(-100).start, 0);
  assert.equal(view.bucketAt(1000).end, 10000);
});

test("空查询不制造用量，无效时间范围安全返回", () => {
  const input = { fromUnixMs: 50, toUnixMs: 250, bucketMs: 100, trend: [] };
  const view = usageTimelineView(input);
  assert.equal(view.peak, null);
  assert.equal(view.peakIndex, null);
  assert.ok(view.windowAt(0, 24).buckets.every(({ total, tokens }) => total === 0 && tokens === null));
  assert.equal(usageTimelineView({ ...input, toUnixMs: 50 }), null);
  assert.equal(usageTimelineView({ ...input, bucketMs: 0 }), null);
  assert.equal(usageTimelineView({ ...input, fromUnixMs: NaN }), null);
  assert.equal(usageTimelineView({ ...input, fromUnixMs: Date.UTC(2024, 11, 31, 23), toUnixMs: Date.UTC(2025, 0, 1), bucketMs: 3600000 }).spansYears, true, "跨年结束边界也需显示年份");
  assert.equal(usageTimelineView({ ...input, fromUnixMs: Date.UTC(2024, 0, 1), toUnixMs: Date.UTC(2024, 11, 31), bucketMs: 86400000 }).spansYears, false);
});

test("鼠标悬停临时预览，点击固定时段，离开图表恢复固定选择", () => {
  const states = [];
  let cursor = 0;
  const graph = createModuleGraph(new URL("../src/UsageTimeline.tsx", import.meta.url), { autoStub: true, stubs: {
    react: { useMemo(factory) { return factory(); }, useState(initial) { const index = cursor++; if (!(index in states)) states[index] = initial; return [states[index], (value) => { states[index] = value; }]; }, useRef() { return { current: null }; } },
    "./components/ui": autoStubModule("ui"),
  } });
  const stats = { fromUnixMs: 0, toUnixMs: 400, bucketMs: 100, trend: [bucket(100, null), bucket(200, 0), bucket(300, 99)] };
  const render = () => { cursor = 0; return graph.exports.UsageTimeline({ stats }); };
  const buttons = (tree) => collectElements(tree, (element) => elementType(element) === "button" && elementProps(element)["data-bucket-index"] != null);
  let tree = render();
  assert.match(textContent(tree), /选中时段/);
  elementProps(buttons(tree)[1]).onPointerEnter({ pointerType: "mouse" });
  tree = render();
  assert.match(textContent(tree), /预览时段/);
  assert.equal(elementProps(buttons(tree)[3])["aria-pressed"], true, "悬停不改变固定选择");
  elementProps(buttons(tree)[2]).onClick();
  tree = render();
  const [plot] = collectElements(tree, (element) => elementProps(element).className === "usage-timeline-plot");
  elementProps(plot).onPointerLeave();
  tree = render();
  assert.equal(elementProps(buttons(tree)[2])["aria-pressed"], true);
  assert.match(textContent(tree), /已知用量为零/);
  assert.doesNotMatch(textContent(tree), /预览时段/);
});
