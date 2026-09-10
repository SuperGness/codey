import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const root = new URL("../", import.meta.url);

test("request log cache hit rate uses input tokens and preserves unknown usage", async () => {
  const viewer = await readFile(new URL("src/RequestLogDialog.tsx", root), "utf8");
  const source = viewer.match(/function formatCacheHitRate\([\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const compiled = ts.transpileModule(source, {}).outputText;
  const format = new Function(`${compiled}; return formatCacheHitRate;`)();
  assert.equal(format(64268, 32000), "49.8%");
  assert.equal(format(100, 0), "0.0%");
  assert.equal(format(100, 100), "100.0%");
  for (const [input, cached] of [[null, 0], [100, null], [undefined, undefined], [0, 0], [-1, 0], [100, -1], [100, 101], [Infinity, 1], [100, NaN]]) {
    assert.equal(format(input, cached), "—");
  }
});

test("request log controls are scoped to built-in routing and preserve logger settings", async () => {
  const [app, modelSection, types, preview] = await Promise.all([
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/ModelSection.tsx", root), "utf8"),
    readFile(new URL("src/App.types.ts", root), "utf8"),
    readFile(new URL("src/dev/mockApi.ts", root), "utf8"),
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
  assert.match(modelSection, /invoke\("open_route_request_logs"\)/);
  assert.doesNotMatch(modelSection, /<RequestLogDialog/);
  assert.match(preview, /routeRequestLog:\s*\{/);
  assert.match(preview, /command === "query_route_request_logs"/);
  assert.match(preview, /codexSessionId:/);
  assert.match(preview, /codexSessionIsParent,/);
  assert.match(preview, /item\.codexSessionId,/);
  assert.doesNotMatch(preview, /retryCount/);
  assert.match(preview, /upstreamTransport: protocol === "sse" \? "http_sse" : protocol/);
  assert.match(preview, /protocol && item\.upstreamTransport !== protocol/);
  assert.doesNotMatch(preview, /protocol && item\.requestProtocol/);
});

test("request log viewer is hosted by the local router for the system browser", async () => {
  const [api, overlay] = await Promise.all([
    readFile(new URL("src/api.ts", root), "utf8"),
    readFile(new URL("src/overlay.tsx", root), "utf8"),
  ]);

  assert.match(api, /"open_route_request_logs"/);
  assert.match(overlay, /const REQUEST_LOG_PATH = "\/codey\/request-logs"/);
  assert.match(overlay, /sessionStorage\.setItem\(REQUEST_LOG_TOKEN_KEY, hashToken\)/);
  assert.match(overlay, /fetch\(`\/codey\/api\/\$\{command\}`/);
  assert.match(overlay, /<RequestLogDialog[\s\S]*standalone/);
});

test("request log viewer uses a full-screen server-paginated searchable table", async () => {
  const viewer = await readFile(
    new URL("src/RequestLogDialog.tsx", root),
    "utf8",
  );

  assert.doesNotMatch(viewer, /<Modal/);
  assert.doesNotMatch(viewer, /antd|ant-/);
  assert.match(viewer, /className="[^"]*flex h-full min-h-0 flex-1 flex-col/);
  assert.match(viewer, /invoke<RouteRequestLogQueryPage>\("query_route_request_logs", \{/);
  assert.match(viewer, /pageSize/);
  assert.match(viewer, /window\.setTimeout\([\s\S]*300/);
  assert.match(viewer, /按供应商筛选请求日志/);
  assert.match(viewer, /按实际模型筛选请求日志/);
  assert.match(viewer, /按状态筛选请求日志/);
  assert.match(viewer, /按上游协议筛选请求日志/);
  assert.match(viewer, /label: "SSE", value: "http_sse"/);
  assert.match(viewer, /item\.upstreamTransport === "http_sse" \? "SSE" : \(item\.upstreamTransport \|\| "—"\)\.toUpperCase\(\)/);
  assert.match(viewer, /protocolTagClass\(item\.upstreamTransport\)/);
  assert.doesNotMatch(viewer, /item\.requestProtocol/);
  assert.match(viewer, /<Pagination[^>]*className="w-auto"/);
  const styles = await readFile(new URL("src/styles.request-log.css", root), "utf8");
  assert.match(styles, /\.request-log-protocol-http/);
  assert.match(styles, /\.request-log-protocol-sse/);
  assert.match(styles, /\.request-log-protocol-ws/);
  assert.match(styles, /\.request-log-pagination \.pagination[\s\S]*width:\s*auto/);
  assert.match(viewer, /<Drawer[\s\S]*onOpenChange=\{[^}]*setSelectedItem\(null\)/);
  assert.match(viewer, /onRowAction=\{\(key\) =>[\s\S]*setSelectedItem\(record\)/);
  assert.match(viewer, /aria-label=\{`复制请求 ID：\$\{item\.requestId\}`\}/);
  assert.match(viewer, /cursorMode: true/);
  assert.match(viewer, /result\.nextCursor/);
  assert.match(viewer, /query_route_request_log_stats/);
  assert.doesNotMatch(viewer, /for \(const item of result\.items\)/);
  assert.match(viewer, /总量已知/);
  assert.match(viewer, /开始时间/);
  assert.match(viewer, /recordingHealth/);
  assert.match(viewer, />\s*删除请求日志\s*</);
  assert.match(viewer, /"clear_route_request_logs"/);
  assert.match(viewer, /删除全部请求日志？/);
  assert.match(viewer, /删除全部历史请求日志，且不可恢复/);
  assert.match(viewer, /确认删除全部日志/);
  assert.match(viewer, /container=\{standalone \? document\.body : container\}/);
  assert.match(viewer, /disabled=\{clearing\}/);
  assert.doesNotMatch(viewer, /disabled=\{loading \|\| clearing \|\| result\?\.queryable !== true\}/);
  assert.match(viewer, /if \(clearInFlight\.current\) return/);
  assert.match(viewer, /setPage\(1\)/);
  assert.match(viewer, /total:\s*0/);
  assert.match(viewer, /items:\s*\[\]/);
  assert.match(viewer, /<Alert\.Title>\{actionNotice\.tone === "success" \? "删除成功" : "删除失败"\}<\/Alert\.Title>/);
  assert.match(viewer, /result\?\.status === "unavailable"/);
  assert.match(viewer, /请求日志加载失败/);
  assert.match(viewer, /没有匹配的请求日志/);
  for (const heading of [
    "时间 / 请求 ID",
    "会话 ID",
    "供应商 / 上游",
    "模型",
    "思考强度",
    "上游协议",
    "状态",
    "耗时",
    "Token 用量",
    "缓存 Token",
  ]) {
    assert.match(viewer, new RegExp(`title: "${heading}", width: \\d+`));
  }
  assert.doesNotMatch(viewer, />重试</);
  assert.doesNotMatch(viewer, /retryCount/);
  assert.match(viewer, /item\.upstreamAuthority/);
  assert.match(viewer, /downstreamFirstContentMs\?: number \| null/);
  assert.match(viewer, /item\.downstreamFirstContentMs \?\? item\.ttftMs/);
  assert.match(viewer, /端到端首内容/);
  assert.match(viewer, /路由前置/);
  assert.match(viewer, /上游首包/);
  assert.match(viewer, /upstreamErrorSummary\?: string \| null/);
  assert.match(viewer, /\[\s*item\.statusCode,\s*item\.upstreamStatusCode\s*\]\.some/);
  assert.match(viewer, /statusCode < 200 \|\| statusCode >= 300/);
  assert.match(viewer, /<IconQuestionMark/);
  assert.match(viewer, /查看上游错误信息/);
  assert.match(viewer, /cancelled: \{ label: "已中断"/);
  assert.match(viewer, /downstream_event_write_failed/);
  assert.match(viewer, /completionReason === "scope_dropped"/);
  assert.match(viewer, /查看中断原因/);
  assert.match(viewer, /`HTTP \$\{item\.statusCode\}`/);
  assert.match(viewer, /<Tooltip[\s\S]*position="top"/);
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
  assert.match(viewer, /formatTokens\(item\.reasoningOutputTokens\)/);
  assert.match(viewer, /formatCacheHitRate\(item\.inputTokens, item\.cachedInputTokens\)/);
  assert.match(viewer, /formatTimestamp\(item\.timestampUnixMs\)/);
  assert.match(viewer, /item\.requestId/);
  assert.match(viewer, /codexSessionId\?: string \| null/);
  assert.match(viewer, /codexSessionIsParent\?: boolean \| null/);
  assert.match(viewer, /item\.codexSessionIsParent[\s\S]*父/);
  assert.match(viewer, /flex w-36 max-w-36 items-center gap-1\.5 overflow-hidden/);
  assert.match(viewer, /className="shrink-0 whitespace-nowrap"/);
  assert.match(viewer, /onClick=\{\(\) => handleCopyId\(item\.codexSessionId!\)\}/);
  assert.match(viewer, /navigator\.clipboard\.writeText\(requestId\)\.then\([\s\S]*setCopiedId\(requestId\)/);
  assert.match(viewer, /复制\$\{item\.codexSessionIsParent \? "父会话" : "会话"\} ID/);
  assert.match(viewer, /setCopyToast\(\{/);
  assert.match(viewer, /role="status"/);
  assert.match(viewer, /aria-live="polite"/);
  assert.match(viewer, /总请求数/);
  assert.match(viewer, /请求成功率/);
  assert.match(viewer, /平均首字耗时 \(TTFT\)/);
  assert.match(viewer, /Token 消耗/);
  assert.doesNotMatch(viewer, /item\.providerName && item\.provider \?/);
});

test("request log preview supports clearing all history", async () => {
  const preview = await readFile(new URL("src/dev/mockApi.ts", root), "utf8");

  assert.match(preview, /command === "clear_route_request_logs"/);
  assert.match(preview, /previewRouteRequestLogs\.length = 0/);
  assert.match(preview, /removedFileCount: hadLogs \? 1 : 0/);
  assert.match(preview, /recordingRestarted: true/);
});
