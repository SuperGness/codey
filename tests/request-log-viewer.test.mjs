import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("request log controls are scoped to built-in routing and preserve logger settings", async () => {
  const [app, modelSection, types, preview] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/ModelSection.tsx", root), "utf8"),
    readFile(new URL("src/App.types.ts", root), "utf8"),
    readFile(new URL("src/main.tsx", root), "utf8"),
  ]);

  assert.match(types, /export type RouteRequestLogConfig/);
  assert.match(types, /backend: "ndjson" \| "sqlite"/);
  assert.match(app, /routeRequestLog:\s*\{\s*\.\.\.config\.routeRequestLog/);
  assert.match(app, /enabled: checked/);
  assert.match(app, /checked \? \{ backend: "sqlite" as const \} : \{\}/);
  assert.match(app, /请求日志记录已实时开启，无需重启/);
  assert.match(app, /保存后将实时关闭请求日志记录，无需重启 Codex/);
  assert.match(modelSection, /\{config\.localRouterEnabled && \([\s\S]*开启日志记录/);
  assert.match(modelSection, /aria-label="开启请求日志记录"/);
  assert.match(modelSection, /查看请求日志/);
  assert.match(modelSection, /<RequestLogDialog/);
  assert.match(preview, /routeRequestLog:\s*\{/);
  assert.match(preview, /command === "query_route_request_logs"/);
  assert.match(preview, /codexSessionId:/);
  assert.match(preview, /codexSessionIsParent,/);
  assert.match(preview, /item\.codexSessionId,/);
  assert.doesNotMatch(preview, /retryCount/);
});

test("request log viewer uses a full-screen server-paginated searchable table", async () => {
  const viewer = await readFile(
    new URL("src/RequestLogDialog.tsx", root),
    "utf8",
  );

  assert.match(viewer, /<Modal[\s\S]*fullScreen/);
  assert.match(viewer, /invoke<RouteRequestLogQueryPage>\("query_route_request_logs", \{/);
  assert.match(viewer, /pageSize/);
  assert.match(viewer, /window\.setTimeout\([\s\S]*300/);
  assert.match(viewer, /按供应商筛选请求日志/);
  assert.match(viewer, /按模型筛选请求日志/);
  assert.match(viewer, /按状态筛选请求日志/);
  assert.match(viewer, /按协议筛选请求日志/);
  assert.match(viewer, /<Pagination/);
  assert.match(viewer, />\s*删除请求日志\s*</);
  assert.match(viewer, /"clear_route_request_logs"/);
  assert.match(viewer, /删除全部请求日志？/);
  assert.match(viewer, /删除全部历史请求日志，且不可恢复/);
  assert.match(viewer, /确认删除全部日志/);
  assert.match(viewer, /disabled=\{clearing\}/);
  assert.doesNotMatch(viewer, /disabled=\{loading \|\| clearing \|\| result\?\.queryable !== true\}/);
  assert.match(viewer, /if \(clearInFlight\.current\) return/);
  assert.match(viewer, /setPage\(1\)/);
  assert.match(viewer, /total:\s*0/);
  assert.match(viewer, /items:\s*\[\]/);
  assert.match(viewer, /title=\{actionNotice\.tone === "success" \? "删除成功" : "删除失败"\}/);
  assert.match(viewer, /result\?\.status === "unavailable"/);
  assert.match(viewer, /请求日志加载失败/);
  assert.match(viewer, /没有匹配的请求日志/);
  for (const heading of [
    "时间 / 请求 ID",
    "会话 ID",
    "供应商 / 上游",
    "模型",
    "思考强度",
    "协议",
    "状态",
    "TTFT / 总耗时",
    "输入 Token",
    "输出 Token",
    "缓存 Token",
    "总 Token",
  ]) {
    assert.match(viewer, new RegExp(`>${heading}<`));
  }
  assert.doesNotMatch(viewer, />重试</);
  assert.doesNotMatch(viewer, /retryCount/);
  assert.match(viewer, /item\.upstreamAuthority/);
  assert.match(viewer, /upstreamErrorSummary\?: string \| null/);
  assert.match(viewer, /\[\s*item\.statusCode,\s*item\.upstreamStatusCode,[\s\S]*\.some/);
  assert.match(viewer, /statusCode < 200 \|\| statusCode >= 300/);
  assert.match(viewer, /<IconQuestionMark/);
  assert.match(viewer, /查看上游错误信息/);
  assert.match(viewer, /cancelled: \{ label: "已中断"/);
  assert.match(viewer, /downstream_event_write_failed/);
  assert.match(viewer, /completionReason === "scope_dropped"/);
  assert.match(viewer, /查看中断原因/);
  assert.match(viewer, /`HTTP \$\{item\.statusCode\}`/);
  assert.match(viewer, /<Tooltip[\s\S]*autoAdjustOverflow/);
  for (const reason of [
    "not_reported_by_upstream",
    "response_tap_limit_exceeded",
    "observer_queue_full",
    "response_observer_queue_full",
    "usage_projection_failed",
    "usage_projection_limit_exceeded",
    "request_not_completed",
  ]) {
    assert.match(viewer, new RegExp(`${reason}:`));
  }
  assert.match(viewer, /item\.totalTokens == null[\s\S]*usageUnavailable\.label/);
  assert.match(viewer, /Token 使用量不可用：\$\{usageUnavailable\.message\}/);
  assert.match(viewer, /formatTokens\(item\.totalTokens\)/);
  assert.match(viewer, /formatTimestamp\(item\.timestampUnixMs\)/);
  assert.match(viewer, /item\.requestId/);
  assert.match(viewer, /codexSessionId\?: string \| null/);
  assert.match(viewer, /codexSessionIsParent\?: boolean \| null/);
  assert.match(viewer, /item\.codexSessionIsParent[\s\S]*父/);
  assert.match(viewer, /w-40 max-w-40 whitespace-nowrap">会话 ID/);
  assert.match(viewer, /flex w-36 max-w-36 items-center gap-1\.5 overflow-hidden/);
  assert.match(viewer, /className="shrink-0 whitespace-nowrap"/);
  assert.match(viewer, /onClick=\{\(\) => handleCopyId\(item\.codexSessionId!\)\}/);
  assert.match(viewer, /复制\$\{item\.codexSessionIsParent \? "父会话" : "会话"\} ID/);
  assert.match(viewer, /<span className="text-\[#8e8e93\]">—<\/span>/);
  assert.doesNotMatch(viewer, /codexSubagentSessionId/);
  assert.doesNotMatch(viewer, /item\.providerName && item\.provider \?/);
});

test("request log preview supports clearing all history", async () => {
  const preview = await readFile(new URL("src/main.tsx", root), "utf8");

  assert.match(preview, /command === "clear_route_request_logs"/);
  assert.match(preview, /previewRouteRequestLogs\.length = 0/);
  assert.match(preview, /removedFileCount: hadLogs \? 1 : 0/);
  assert.match(preview, /recordingRestarted: true/);
});
