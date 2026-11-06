// 只测真实组件生成元素树的 CPU 时间，不包含 React 提交、布局、绘制或网络。
// 用现有测试桩替代 UI 库，TypeScript 编译和依赖加载均在计时之外。
import assert from "node:assert/strict";
import { cpus, platform, release } from "node:os";
import { performance } from "node:perf_hooks";
import {
  autoStubModule,
  collectElements,
  createModuleGraph,
  elementProps,
} from "../tests/helpers/jsx-tree.mjs";

const samples = 100;
const percentile = (values, p) => [...values].sort((a, b) => a - b)[Math.ceil(values.length * p) - 1];
console.log(JSON.stringify({ environment: { node: process.version, platform: platform(), release: release(), cpu: cpus()[0].model }, samples }));

for (const count of [24, 168]) {
  const graph = createModuleGraph(new URL("../src/UsageTimeline.tsx", import.meta.url), {
    autoStub: true,
    stubs: { "./components/ui": autoStubModule("ui") },
  });
  const hour = 3_600_000;
  const from = Date.UTC(2026, 10, 1);
  const stats = {
    fromUnixMs: from, toUnixMs: from + count * hour, bucketMs: hour,
    trend: Array.from({ length: count }, (_, index) => ({ timestampUnixMs: from + index * hour, total: 3, totalTokensSum: (index + 1) * 1234 })),
  };
  const render = () => { graph.restart(); return graph.exports.UsageTimeline({ stats }); };
  const started = performance.now();
  let tree = render();
  const firstRenderMs = performance.now() - started;
  if (count > 24) {
    const [all] = collectElements(tree, node => elementProps(node)["aria-label"] === undefined && elementProps(node).onClick && elementProps(node).children === "查看全程");
    assert.ok(all);
    elementProps(all).onClick();
    tree = render();
  }
  let buttons = collectElements(tree, node => elementProps(node)["data-bucket-index"] != null);
  assert.equal(buttons.length, count);
  const times = [];
  for (let index = 0; index < samples + 10; index++) {
    const begin = performance.now();
    elementProps(buttons[index % count]).onPointerEnter({ pointerType: "mouse" });
    tree = render();
    const elapsed = performance.now() - begin;
    if (index >= 10) times.push(elapsed);
    buttons = collectElements(tree, node => elementProps(node)["data-bucket-index"] != null);
  }
  console.log(JSON.stringify({ scenario: "usage_timeline_element_tree", buckets: count, first_render_ms: firstRenderMs, hover_p50_ms: percentile(times, .5), hover_p95_ms: percentile(times, .95) }));
}
