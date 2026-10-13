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
    viewer.match(/function requestLogProviderLabel\([\s\S]*?\n\}/)?.[0],
  ];
  source.forEach((part) => assert.ok(part));
  const compiled = ts.transpileModule(source.join("\n").replaceAll("export function", "function"), {}).outputText;
  const label = new Function(`${compiled}; return requestLogProviderLabel;`)();
  const profiles = [
    { id: "local-a", sourceProviderId: "route-a", name: "线路甲", shortName: "甲" },
    { id: "route-b", name: "", shortName: "乙" },
    { id: "official-a", sourceProviderId: "official", name: "官方甲", shortName: "官甲", officialAccountId: "account-a" },
    { id: "official-b", sourceProviderId: "official", name: "官方乙", shortName: "官乙", officialAccountId: "account-b" },
  ];
  assert.equal(label({ provider: "route-a", providerName: "route-a" }, profiles), "[甲] 线路甲");
  assert.equal(label({ provider: "local-a" }, profiles), "[甲] 线路甲");
  assert.equal(label({ provider: "route-b" }, profiles), "乙");
  assert.equal(label({ provider: "official", officialAccountId: "account-b" }, profiles), "[官乙] 官方乙");
  assert.equal(label({ provider: "removed", providerName: "历史线路" }, profiles), "历史线路");
  assert.equal(label({ provider: "removed" }, profiles), "removed");
  assert.equal(label({}, profiles), "—");
  assert.equal(label({ provider: "legacy" }, [{ id: "legacy", name: "自建" }]), "自建");
  assert.equal(label({ provider: "empty", providerName: "历史线路" }, [{ id: "empty", name: "", shortName: "" }]), "历史线路");
});

test("request log cache hit rate uses input tokens and preserves unknown usage", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
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

test("request log output speed uses one decimal and preserves unknown values", async () => {
  const [viewer, usage] = await Promise.all([
    readSource("src/RequestLogDialog.tsx"),
    readSource("src/usageAnalysis.ts"),
  ]);
  const source = viewer.match(/function formatTokensPerSecond\([\s\S]*?\n\}/)?.[0];
  assert.ok(source);
  const compiled = ts.transpileModule(source, {}).outputText;
  const format = new Function(`${compiled}; return formatTokensPerSecond;`)();
  assert.equal(format(123.44), "123.4 tok/s");
  assert.equal(format(123.46), "123.5 tok/s");
  assert.equal(format(1), "1.0 tok/s");
  for (const value of [null, undefined, 0, -1, NaN, Infinity, -Infinity]) {
    assert.equal(format(value), "—");
  }
  assert.match(viewer, /outputTokensPerSecond\?: number \| null;/);
  assert.match(viewer, /速度<\/span>[\s\S]*?formatTokensPerSecond\(item\.outputTokensPerSecond\)/);
  assert.match(viewer, /输出速度<\/span>[\s\S]*?formatTokensPerSecond\(selectedItem\.outputTokensPerSecond\)/);
  assert.match(viewer, /平均总耗时 \{formatDuration\(stats\.avgDuration\)\} · <span[^>]*>平均输出 \{formatTokensPerSecond\(stats\.avgOutputTokensPerSecond\)\}/);
  assert.match(viewer, /hidden md:inline"[^>]*>速度:[\s\S]*?formatTokensPerSecond\(stats\.avgOutputTokensPerSecond\)/);
  assert.equal((usage.match(/avgOutputTokensPerSecond: number \| null/g) ?? []).length, 2);
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

test("request log model cell shows the model sent upstream and the upstream model when it differs", async () => {
  const [viewer, preview] = await Promise.all([
    readSource("src/RequestLogDialog.tsx"),
    readSource("src/dev/mockApi.ts"),
  ]);

  assert.ok(viewer.includes('import { modelIdsEqual } from "./modelIds";'));
  assert.ok(viewer.includes('const sentModel = (item.model ?? "").trim() || item.requestedModel.trim();'));
  assert.ok(viewer.includes('const upstreamModel = (item.upstreamResponseModel ?? "").trim();'));
  assert.ok(viewer.includes("const upstreamModelDiffers = Boolean(upstreamModel) && !modelIdsEqual(upstreamModel, sentModel);"));
  assert.ok(viewer.includes('{sentModel || "—"}'));
  assert.ok(viewer.includes("{upstreamModelDiffers ? ("));
  assert.ok(viewer.includes("请求模型（发往上游）：${sentModel}"));
  assert.ok(viewer.includes("上游实际使用模型：${upstreamModel}"));
  assert.ok(viewer.includes("upstreamResponseModel?: string | null;"));
  assert.ok(viewer.includes("Codex 请求 Codey 时选择的模型 ID，带线路前缀"));
  // 请求模型取发往上游的模型，实际模型取上游响应回报的模型。
  assert.match(viewer, /请求模型<\/dt>[\s\S]*?selectedItem\.model \|\| selectedItem\.requestedModel/);
  assert.match(viewer, /实际使用模型<\/dt>[\s\S]*?selectedItem\.upstreamResponseModel/);
  // 请求模型不展示带线路前缀的 Codex 选择器，只在详情里单列一行。
  assert.ok(viewer.includes("{selectedItem.requestedModel}"));
  // 预览覆盖实际模型与请求模型一致、不一致以及上游未回报三种形态。
  assert.ok(preview.includes('primary ? "primary/provider-fast-coder" : "claude-sonnet-4-5"'));
  assert.ok(preview.includes('model: account ? "gpt-5.6-sol" : primary ? "provider-fast-coder" : "claude-sonnet-4-5"'));
  assert.ok(preview.includes("upstreamResponseModel: failed"));
  assert.ok(preview.includes('"deepseek/deepseek-v4.1-flash"'));
  assert.ok(preview.includes('"claude-sonnet-4-5-20250929"'));
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
  assert.match(viewer, /请求体 input 形态（客户端 \/ 发往上游）/);
  assert.match(viewer, /isEmptyInputArray\(selectedItem\.upstreamInputState, selectedItem\.upstreamInputItems\)/);
});

test("request log controls are scoped to built-in routing and preserve logger settings", async () => {
  const [app, modelSection, types, preview] = await Promise.all([
    readSource("src/App.tsx"),
    readSource("src/ModelSection.tsx"),
    readSource("src/App.types.ts"),
    readSource("src/dev/mockApi.ts"),
  ]);

  assert.match(types, /export type RouteRequestLogConfig/);
  assert.match(types, /backend: "ndjson" \| "sqlite"/);
  assert.match(app, /routeRequestLog:\s*\{\s*\.\.\.config\.routeRequestLog/);
  assert.match(app, /enabled: checked/);
  assert.match(app, /checked \? \{ backend: "sqlite" as const \} : \{\}/);
  assert.match(app, /请求日志记录已实时开启，无需重启/);
  assert.match(app, /保存后关闭日志记录，无需重启/);
  assert.match(modelSection, /\{config\.localRouterEnabled && \([\s\S]*(开启)?日志记录/);
  assert.match(modelSection, /aria-label="开启请求日志记录"/);
  assert.match(modelSection, /查看请求日志/);
  assert.match(modelSection, /invoke\("open_route_request_logs", \{ theme: readHostTheme\(\) \}\)/);
  assert.doesNotMatch(modelSection, /<RequestLogDialog/);
  assert.match(preview, /routeRequestLog:\s*\{/);
  assert.match(preview, /command === "query_route_request_logs"/);
  assert.match(preview, /codexSessionId:/);
  assert.match(preview, /codexSessionIsParent,/);
  assert.match(preview, /item\.codexSessionId,/);
  assert.doesNotMatch(preview, /retryCount/);
  assert.match(preview, /upstreamTransport: protocol === "sse" \? "http_sse" : protocol/);
  assert.match(preview, /requestInputState: "array",/);
  assert.match(preview, /upstreamInputItems: failed \? 0 : 12 \+ index,/);
  assert.match(preview, /protocol && item\.upstreamTransport !== protocol/);
  assert.doesNotMatch(preview, /protocol && item\.requestProtocol/);
});

test("request log viewer is hosted by the local router for the system browser", async () => {
  const [api, overlay] = await Promise.all([
    readSource("src/api.ts"),
    readSource("src/overlay.tsx"),
  ]);

  assert.match(api, /"open_route_request_logs"/);
  assert.match(overlay, /const REQUEST_LOG_PATH = "\/codey\/request-logs"/);
  assert.match(overlay, /sessionStorage\.setItem\(REQUEST_LOG_TOKEN_KEY, hashToken\)/);
  assert.match(overlay, /fetch\(`\/codey\/api\/\$\{command\}`/);
  assert.match(overlay, /<RequestLogDialog[\s\S]*standalone/);
});

test("request log viewer uses a full-screen server-paginated searchable table", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");

  assert.doesNotMatch(viewer, /<Modal/);
  assert.doesNotMatch(viewer, /antd|ant-/);
  assert.match(viewer, /className="[^"]*flex h-full min-h-0 flex-1 flex-col/);
  assert.match(viewer, /invoke<RouteRequestLogQueryPage>\("query_route_request_logs", \{/);
  assert.match(viewer, /pageSize/);
  assert.match(viewer, /window\.setTimeout\([\s\S]*300/);
  assert.match(viewer, /按供应商筛选请求日志/);
  assert.match(viewer, /按请求模型筛选请求日志/);
  assert.match(viewer, /按状态筛选请求日志/);
  assert.match(viewer, /按上游协议筛选请求日志/);
  assert.match(viewer, /按官方账号筛选请求日志/);
  assert.match(viewer, /officialAccountId: officialAccount/);
  assert.match(viewer, /\{ label: "按官方账号统计", value: "official_account" \}/);
  assert.match(viewer, /official_account: "官方账号"/);
  assert.match(viewer, /官方账号：\$\{officialAccountLabel\(item\.officialAccountId\)\}/);
  assert.match(viewer, />\s*官\s*<\/span>/);
  assert.doesNotMatch(viewer, />官方账号<\/span>/);
  // 独立页面拿不到启动期能力标志，存在官方线路时仍要读取账号列表。
  assert.match(viewer, /profile\.officialAccount \|\| Boolean\(profile\.officialAccountId\)/);
  assert.match(viewer, /label: "SSE", value: "http_sse"/);
  assert.match(viewer, /item\.upstreamTransport === "http_sse" \? "SSE" : \(item\.upstreamTransport \|\| "—"\)\.toUpperCase\(\)/);
  assert.match(viewer, /protocolTagClass\(item\.upstreamTransport\)/);
  assert.doesNotMatch(viewer, /item\.requestProtocol/);
  assert.match(viewer, /<Pagination[^>]*className="w-auto"/);
  const styles = await readSource("src/styles.request-log.css");
  assert.match(styles, /\.request-log-protocol-http/);
  assert.match(styles, /\.request-log-protocol-sse/);
  assert.match(styles, /\.request-log-protocol-ws/);
  assert.match(styles, /\.request-log-pagination \.pagination[\s\S]*width:\s*auto/);
  assert.match(viewer, /<Drawer[\s\S]*onOpenChange=\{[^}]*setSelectedItem\(null\)/);
  assert.match(viewer, /onRowAction=\{\(key\) =>[\s\S]*setSelectedItem\(record\)/);
  assert.match(viewer, /aria-label=\{`复制请求 ID：\$\{item\.requestId\}`\}/);
  assert.match(viewer, /cursorMode: true/);
  assert.match(viewer, /nextResult\.nextCursor/);
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
    "时间 / 标识",
    "供应商 / 上游",
    "模型 / 思考",
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
  assert.match(viewer, /<CopyIdButton\s+id=\{item\.codexSessionId\}/);
  assert.match(viewer, /onCopy=\{\(id, copiedKey\) => handleCopyId\(id, \{ copiedKey \}\)\}/);
  assert.match(viewer, /navigator\.clipboard\.writeText\(requestId\)\.then\([\s\S]*setCopiedId\(options\?\.copiedKey \?\? requestId\)/);
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
  const preview = await readSource("src/dev/mockApi.ts");

  assert.match(preview, /command === "clear_route_request_logs"/);
  assert.match(preview, /previewRouteRequestLogs\.length = 0/);
  assert.match(preview, /removedFileCount: hadLogs \? 1 : 0/);
  assert.match(preview, /recordingRestarted: true/);
});

// 搜索防抖的首次执行发生在挂载时，若此时重置分页，首页查询刚拿到的游标会被清空，
// 第 2 页及之后的页码都会保持禁用。
test("request log search debounce keeps the first page cursors when the query text is unchanged", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
  const source = viewer.match(
    /useEffect\(\(\) => \{\r?\n\s+const nextSearch = searchInput\.trim\(\);[\s\S]*?\}, \[searchInput, search\]\);/,
  )?.[0];

  assert.ok(source);
  assert.match(source, /if \(nextSearch === search\) return;/);
  assert.match(source, /window\.setTimeout\([\s\S]*?300/);
  assert.match(source, /setSearch\(nextSearch\)/);
  assert.match(source, /resetPagination\(\)/);
  assert.match(source, /window\.clearTimeout\(timer\)/);
});
