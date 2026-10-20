import assert from "node:assert/strict";
import test from "node:test";
import { createModuleGraph, collectElements, elementProps, elementType, textContent } from "./helpers/jsx-tree.mjs";

const { exports: { RequestLogRouteDiagnostics } } = createModuleGraph(new URL("../src/RequestLogRouteDiagnostics.tsx", import.meta.url));
const render = (item) => RequestLogRouteDiagnostics({ item });
const decision = {
  settingsRevision: 0, reason: "explicit_alias", candidateCount: 2,
  historicalAlias: false, defaultedModel: false, rejectedHint: null, rejectedBinding: null,
};
const attempt = {
  sequence: 1, transport: "http_sse", trigger: "initial", startedMs: 0,
  durationMs: null, headerMs: null, statusCode: null, outcome: "pending", phase: "send",
};

test("legacy logs hide diagnostics while unresolved new logs remain explicit", () => {
  assert.equal(render({}), null);
  assert.equal(render({ routeDecision: null, upstreamAttempts: [], attemptsOmitted: 0 }), null);
  const text = textContent(render({ routeDecision: { ...decision, reason: "not_resolved", candidateCount: 0 } }));
  assert.ok(text.includes("尚未完成线路选择"));
  assert.ok(text.includes("配置版本0"));
  assert.ok(text.includes("匹配候选线路0 条"));
  assert.ok(!text.includes("成功"));
});

test("route decision explains alias recovery and actual rejected candidates", () => {
  const text = textContent(render({ routeDecision: {
    ...decision, historicalAlias: true, defaultedModel: true,
    rejectedHint: "model_not_enabled", rejectedBinding: "route_missing",
  } }));
  for (const label of ["按模型别名指定的线路选择", "已还原历史模型别名", "已使用默认模型", "未采用请求线路提示", "线路未启用该模型", "未沿用会话绑定", "线路不存在或不可用"]) assert.ok(text.includes(label));
  const cleanText = textContent(render({ routeDecision: decision }));
  assert.ok(!cleanText.includes("未采用请求线路提示"));
  assert.ok(!cleanText.includes("未沿用会话绑定"));
});

test("successful retry preserves the earlier rejection and request order", () => {
  const tree = render({ upstreamAttempts: [
    { ...attempt, statusCode: 400, outcome: "rejected", phase: "response_body", durationMs: 15 },
    { ...attempt, sequence: 2, trigger: "reasoning_repair", statusCode: 200, outcome: "succeeded", phase: "complete", durationMs: 42 },
  ], attemptsOmitted: 3 });
  const rows = collectElements(tree, node => elementType(node) === "li");
  assert.equal(rows.length, 2);
  assert.match(textContent(rows[0]), /第 1 次.*上游拒绝.*HTTP 400/);
  assert.match(textContent(rows[1]), /第 2 次.*成功.*补全思考内容后重试.*HTTP 200/);
  assert.ok(textContent(tree).includes("另有 3 次请求超出记录上限"));
});

test("zero timings are visible and absent timings or outcomes never imply success", () => {
  const text = textContent(render({ upstreamAttempts: [attempt] }));
  assert.ok(text.includes("请求开始后0 ms"));
  assert.ok(text.includes("本次耗时未记录"));
  assert.ok(text.includes("收到响应头未记录"));
  assert.ok(text.includes("结果未记录"));
  assert.ok(!text.includes("成功"));
  const zero = textContent(render({ upstreamAttempts: [{ ...attempt, headerMs: 0, durationMs: 0 }] }));
  assert.ok(zero.includes("本次耗时0 ms"));
  assert.ok(zero.includes("收到响应头0 ms"));
});

test("unknown enums and markup remain plain text with neutral status", () => {
  const future = "<img src=x onerror=alert(1)>";
  const tree = render({ routeDecision: { ...decision, reason: future, rejectedHint: "__proto__" }, upstreamAttempts: [{ ...attempt, outcome: future, phase: "future_phase", trigger: "future_trigger", transport: "future_transport" }] });
  const text = textContent(tree);
  for (const label of [future, "__proto__", "future_phase", "future_trigger", "future_transport"]) assert.ok(text.includes(label));
  assert.ok(!text.includes("成功"));
  assert.equal(collectElements(tree, node => elementType(node) === "img").length, 0);
  assert.equal(collectElements(tree, node => elementProps(node).dangerouslySetInnerHTML != null).length, 0);
});
