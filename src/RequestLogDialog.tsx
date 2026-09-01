import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Loader,
  Modal,
  Pagination,
} from "@mantine/core";
import {
  IconAlertCircle,
  IconCheck,
  IconCopy,
  IconDatabaseOff,
  IconQuestionMark,
  IconRefresh,
  IconSearch,
} from "@tabler/icons-react";

import type { Config } from "./App.types";
import { invoke } from "./api";
import {
  Badge,
  Button,
  ActionIcon,
  Input,
  Select,
  Table,
  Tooltip,
} from "./components/mantine";
import { SETTINGS_OVERLAY_Z_INDEX } from "./overlay.constants";

type RouteRequestLogItem = {
  requestId: string;
  traceId: string;
  timestampUnixMs: number;
  provider?: string | null;
  providerName?: string | null;
  requestedModel: string;
  model?: string | null;
  reasoningEffort?: string | null;
  thinkingBudgetTokens?: number | null;
  ttftMs?: number | null;
  upstreamHeaderMs?: number | null;
  totalDurationMs: number;
  queueDelayMs: number;
  inputTokens?: number | null;
  outputTokens?: number | null;
  cachedInputTokens?: number | null;
  cacheCreationInputTokens?: number | null;
  reasoningOutputTokens?: number | null;
  totalTokens?: number | null;
  usageReported: boolean;
  usageUnavailableReason?: string | null;
  requestProtocol: string;
  upstreamTransport?: string | null;
  requestKind: string;
  status: string;
  statusCode?: number | null;
  upstreamStatusCode?: number | null;
  errorCode?: string | null;
  upstreamErrorSummary?: string | null;
  completionReason?: string | null;
  retryCount: number;
  fallbackCount: number;
  fallbackReason?: string | null;
  upstreamAuthority?: string | null;
  upstreamRequestId?: string | null;
  upstreamProtocol?: string | null;
  protocolBridge?: string | null;
  firstByteSource?: string | null;
  codexSessionId?: string | null;
  codexSessionIsParent?: boolean | null;
  subagent: boolean;
};

type RouteRequestLogQueryPage = {
  status: "ok" | "unavailable";
  backend: "sqlite" | "ndjson";
  queryable: boolean;
  reason?: string;
  page: number;
  pageSize: number;
  total: number;
  totalPages: number;
  items: RouteRequestLogItem[];
};

type RequestLogDialogProps = {
  config: Config;
  container: HTMLElement | null;
  opened: boolean;
  onClose: () => void;
};

const statusOptions = [
  { label: "全部状态", value: "all" },
  { label: "成功", value: "succeeded" },
  { label: "失败", value: "failed" },
  { label: "未完成", value: "incomplete" },
  { label: "已取消", value: "cancelled" },
];

const protocolOptions = [
  { label: "全部协议", value: "all" },
  { label: "HTTP", value: "http" },
  { label: "SSE", value: "sse" },
  { label: "WebSocket", value: "ws" },
];

const pageSizeOptions = [20, 50, 100].map((value) => ({
  label: `${value} 条 / 页`,
  value,
}));

const statusPresentation: Record<
  string,
  { label: string; variant: "success" | "destructive" | "warning" | "secondary" }
> = {
  succeeded: { label: "成功", variant: "success" },
  failed: { label: "失败", variant: "destructive" },
  incomplete: { label: "未完成", variant: "warning" },
  cancelled: { label: "已取消", variant: "secondary" },
};

function optionalFilter(value: string) {
  return value === "all" ? undefined : value;
}

function formatTimestamp(value: number) {
  if (!Number.isFinite(value)) return "—";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(value));
}

function formatDuration(value?: number | null) {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value < 1_000) return `${value.toLocaleString()} ms`;
  return `${(value / 1_000).toFixed(value < 10_000 ? 2 : 1)} s`;
}

function formatTokens(value?: number | null) {
  return value == null ? "—" : value.toLocaleString();
}

const usageUnavailablePresentations: Record<string, { label: string; message: string }> = {
  not_reported_by_upstream: {
    label: "未上报",
    message: "上游响应未提供 Token 使用量。",
  },
  response_tap_limit_exceeded: {
    label: "旧记录缺失",
    message: "该历史记录的响应超过旧版观测上限，Token 使用量未能提取。",
  },
  observer_queue_full: {
    label: "观测丢弃",
    message: "日志观测队列繁忙。为避免影响请求转发，本次 Token 数据已放弃。",
  },
  response_observer_queue_full: {
    label: "观测丢弃",
    message: "日志观测队列繁忙。为避免影响请求转发，本次 Token 数据已放弃。",
  },
  usage_projection_failed: {
    label: "解析失败",
    message: "已收到上游响应，但无法从响应格式中提取 Token 使用量。",
  },
  usage_projection_limit_exceeded: {
    label: "观测超限",
    message: "上游返回的 Token 元数据异常过大，已按安全上限停止提取。",
  },
  request_not_completed: {
    label: "请求未完成",
    message: "请求未正常完成，因此没有可记录的最终 Token 使用量。",
  },
};

function usageUnavailablePresentation(reason?: string | null) {
  if (reason && usageUnavailablePresentations[reason]) {
    return usageUnavailablePresentations[reason];
  }
  return {
    label: "不可用",
    message: reason
      ? `Token 使用量不可用（${reason}）。`
      : "本次请求没有可用的 Token 使用量。",
  };
}

function reasoningLabel(item: RouteRequestLogItem) {
  if (item.reasoningEffort) return item.reasoningEffort;
  if (item.thinkingBudgetTokens != null) {
    return `${item.thinkingBudgetTokens.toLocaleString()} tokens`;
  }
  return "—";
}

function unavailableMessage(reason?: string) {
  if (reason === "ndjson_not_queryable") {
    return "当前请求日志使用 NDJSON 存储，无法在线分页查询。开启页面上的日志记录开关后会切换为 SQLite。";
  }
  return "当前请求日志存储暂不可查询，请稍后重试。";
}

export function RequestLogDialog({
  config,
  container,
  opened,
  onClose,
}: RequestLogDialogProps) {
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("all");
  const [model, setModel] = useState("all");
  const [status, setStatus] = useState("all");
  const [protocol, setProtocol] = useState("all");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [refreshRevision, setRefreshRevision] = useState(0);
  const [result, setResult] = useState<RouteRequestLogQueryPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const requestRevision = useRef(0);

  const handleCopyId = (requestId: string) => {
    if (!navigator.clipboard) return;
    void navigator.clipboard.writeText(requestId);
    setCopiedId(requestId);
    window.setTimeout(() => {
      setCopiedId((current) => (current === requestId ? null : current));
    }, 1500);
  };

  const providerOptions = useMemo(() => {
    const providers = new Map<string, string>();
    for (const profile of config.profiles) {
      const value = profile.sourceProviderId || profile.id;
      if (value) providers.set(value, profile.name || value);
    }
    return [
      { label: "全部供应商", value: "all" },
      ...[...providers].map(([value, label]) => ({ label, value })),
    ];
  }, [config.profiles]);

  const modelOptions = useMemo(() => {
    const models = new Set<string>();
    for (const catalog of [
      config.selectedModelsByProvider,
      config.declaredOfficialModelsByProvider,
      config.upstreamModelsByProvider,
    ]) {
      for (const values of Object.values(catalog)) {
        for (const value of values) {
          if (value.trim()) models.add(value);
        }
      }
    }
    return [
      { label: "全部模型", value: "all" },
      ...[...models]
        .sort((left, right) => left.localeCompare(right))
        .map((value) => ({ label: value, value })),
    ];
  }, [
    config.declaredOfficialModelsByProvider,
    config.selectedModelsByProvider,
    config.upstreamModelsByProvider,
  ]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setSearch(searchInput.trim());
      setPage(1);
    }, 300);
    return () => window.clearTimeout(timer);
  }, [searchInput]);

  useEffect(() => {
    if (!opened) return;
    const currentRequest = ++requestRevision.current;
    setLoading(true);
    setError("");
    void invoke<RouteRequestLogQueryPage>("query_route_request_logs", {
      page,
      pageSize,
      ...(search ? { search } : {}),
      ...(optionalFilter(provider) ? { provider } : {}),
      ...(optionalFilter(model) ? { model } : {}),
      ...(optionalFilter(status) ? { status } : {}),
      ...(optionalFilter(protocol) ? { protocol } : {}),
    }).then((nextResult) => {
      if (requestRevision.current !== currentRequest) return;
      if (
        nextResult.queryable &&
        nextResult.totalPages > 0 &&
        page > nextResult.totalPages
      ) {
        setPage(nextResult.totalPages);
        return;
      }
      setResult(nextResult);
    }).catch((nextError: unknown) => {
      if (requestRevision.current !== currentRequest) return;
      setResult(null);
      setError(nextError instanceof Error ? nextError.message : String(nextError));
    }).finally(() => {
      if (requestRevision.current === currentRequest) setLoading(false);
    });
    return () => {
      requestRevision.current += 1;
    };
  }, [opened, page, pageSize, protocol, provider, model, refreshRevision, search, status]);

  const resetFilters = () => {
    setSearchInput("");
    setSearch("");
    setProvider("all");
    setModel("all");
    setStatus("all");
    setProtocol("all");
    setPage(1);
  };

  const hasFilters = Boolean(
    search || provider !== "all" || model !== "all" || status !== "all" || protocol !== "all",
  );
  const firstVisible = result && result.total > 0 ? (result.page - 1) * result.pageSize + 1 : 0;
  const lastVisible = result ? Math.min(result.page * result.pageSize, result.total) : 0;

  return (
    <Modal
      fullScreen
      opened={opened}
      onClose={onClose}
      title="请求日志"
      closeButtonProps={{ "aria-label": "关闭请求日志" }}
      closeOnClickOutside={false}
      classNames={{
        body: "flex min-h-0 flex-1 flex-col overflow-hidden p-0!",
        content: "flex! min-h-0 flex-col overflow-hidden! bg-[#f5f5f7]",
        header: "m-0 flex-none border-b border-black/8 bg-white/90 px-5! py-3! backdrop-blur-xl",
        title: "text-base font-bold text-[#1d1d1f]",
      }}
      lockScroll={false}
      padding={0}
      portalProps={container ? { target: container } : undefined}
      withinPortal={Boolean(container)}
      zIndex={SETTINGS_OVERLAY_Z_INDEX}
    >
      <div className="flex min-h-0 flex-1 flex-col gap-3 p-4 max-[760px]:p-2.5">
        <div className="flex flex-none items-center justify-between gap-3">
          <div className="min-w-0">
            <p className="m-0 text-xs text-[#6e6e73]">
              查看内置路由的供应商、模型、耗时与 Token 使用情况。
            </p>
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={loading}
            onClick={() => setRefreshRevision((value) => value + 1)}
          >
            <IconRefresh className={loading ? "animate-spin" : ""} aria-hidden="true" />
            刷新
          </Button>
        </div>

        <div className="grid flex-none grid-cols-[minmax(220px,1.6fr)_repeat(4,minmax(132px,1fr))_auto] gap-2 rounded-xl border border-black/8 bg-white p-3 shadow-sm max-[1100px]:grid-cols-3 max-[640px]:grid-cols-1">
          <Input
            aria-label="搜索请求日志"
            placeholder="搜索请求 ID、会话 ID、供应商、模型或上游"
            value={searchInput}
            leftSection={<IconSearch size={15} className="text-[#8e8e93]" aria-hidden="true" />}
            onChange={(event) => setSearchInput(event.currentTarget.value)}
          />
          <Select
            aria-label="按供应商筛选请求日志"
            filter
            getPopupContainer={() => container ?? document.body}
            optionList={providerOptions}
            value={provider}
            zIndex={SETTINGS_OVERLAY_Z_INDEX}
            onChange={(value) => {
              setProvider(String(value ?? "all"));
              setPage(1);
            }}
          />
          <Select
            aria-label="按模型筛选请求日志"
            filter
            getPopupContainer={() => container ?? document.body}
            optionList={modelOptions}
            value={model}
            zIndex={SETTINGS_OVERLAY_Z_INDEX}
            onChange={(value) => {
              setModel(String(value ?? "all"));
              setPage(1);
            }}
          />
          <Select
            aria-label="按状态筛选请求日志"
            getPopupContainer={() => container ?? document.body}
            optionList={statusOptions}
            value={status}
            zIndex={SETTINGS_OVERLAY_Z_INDEX}
            onChange={(value) => {
              setStatus(String(value ?? "all"));
              setPage(1);
            }}
          />
          <Select
            aria-label="按协议筛选请求日志"
            getPopupContainer={() => container ?? document.body}
            optionList={protocolOptions}
            value={protocol}
            zIndex={SETTINGS_OVERLAY_Z_INDEX}
            onChange={(value) => {
              setProtocol(String(value ?? "all"));
              setPage(1);
            }}
          />
          <Button
            size="sm"
            variant="ghost"
            disabled={!hasFilters}
            onClick={resetFilters}
          >
            清除筛选
          </Button>
        </div>

        <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-black/8 bg-white shadow-sm">
          {error ? (
            <div className="grid min-h-48 flex-1 place-items-center p-6">
              <Alert
                color="red"
                icon={<IconAlertCircle size={18} aria-hidden="true" />}
                title="请求日志加载失败"
              >
                <p className="m-0 mb-3 text-sm">{error}</p>
                <Button size="xs" variant="outline" onClick={() => setRefreshRevision((value) => value + 1)}>
                  重试
                </Button>
              </Alert>
            </div>
          ) : result?.status === "unavailable" || result?.queryable === false ? (
            <div className="grid min-h-48 flex-1 place-items-center p-6 text-center">
              <div className="grid max-w-lg justify-items-center gap-2 text-[#6e6e73]">
                <IconDatabaseOff size={28} aria-hidden="true" />
                <strong className="text-sm text-[#1d1d1f]">日志暂不可在线查看</strong>
                <p className="m-0 text-xs leading-5">{unavailableMessage(result?.reason)}</p>
              </div>
            </div>
          ) : !result && loading ? (
            <div className="grid min-h-48 flex-1 place-items-center" role="status">
              <div className="flex items-center gap-2 text-xs text-[#6e6e73]">
                <Loader size="sm" />
                正在加载请求日志…
              </div>
            </div>
          ) : result?.items.length === 0 ? (
            <div className="grid min-h-48 flex-1 place-items-center p-6 text-center">
              <div className="grid justify-items-center gap-2 text-[#6e6e73]">
                <IconSearch size={26} aria-hidden="true" />
                <strong className="text-sm text-[#1d1d1f]">
                  {hasFilters ? "没有匹配的请求日志" : "暂无请求日志"}
                </strong>
                <p className="m-0 text-xs">
                  {hasFilters ? "调整搜索或筛选条件后重试。" : "新请求完成后会在这里显示。"}
                </p>
              </div>
            </div>
          ) : (
            <div className="min-h-0 flex-1 overflow-auto" aria-busy={loading}>
              <Table highlightOnHover withColumnBorders withRowBorders className="min-w-[1420px] text-xs">
                <Table.Thead className="sticky top-0 z-[1] bg-[#f8f8fa] shadow-[0_1px_0_rgba(0,0,0,0.08)]">
                  <Table.Tr>
                    <Table.Th className="whitespace-nowrap">时间 / 请求 ID</Table.Th>
                    <Table.Th className="whitespace-nowrap">会话 ID</Table.Th>
                    <Table.Th className="whitespace-nowrap">供应商 / 上游</Table.Th>
                    <Table.Th className="whitespace-nowrap">模型</Table.Th>
                    <Table.Th className="whitespace-nowrap">思考强度</Table.Th>
                    <Table.Th className="whitespace-nowrap">协议</Table.Th>
                    <Table.Th className="whitespace-nowrap">状态</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">TTFT / 总耗时</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">输入 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">输出 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">缓存 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">总 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">重试</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {result?.items.map((item) => {
                    const presentation = statusPresentation[item.status] ?? {
                      label: item.status || "未知",
                      variant: "secondary" as const,
                    };
                    const hasUpstreamError = [
                      item.statusCode,
                      item.upstreamStatusCode,
                    ].some(
                      (statusCode) =>
                        statusCode != null &&
                        (statusCode < 200 || statusCode >= 300),
                    );
                    const upstreamErrorSummary =
                      item.upstreamErrorSummary ||
                      item.errorCode ||
                      "上游未提供具体错误信息";
                    const usageUnavailable = usageUnavailablePresentation(
                      item.usageUnavailableReason,
                    );
                    return (
                      <Table.Tr key={`${item.timestampUnixMs}:${item.requestId}`}>
                        <Table.Td>
                          <div className="grid min-w-36 max-w-44 gap-0.5 font-mono">
                            <span className="whitespace-nowrap text-[11px] text-[#1d1d1f]">
                              {formatTimestamp(item.timestampUnixMs)}
                            </span>
                            <div
                              className="group flex cursor-pointer items-center gap-1 text-[10px] text-[#8e8e93] transition-colors hover:text-[#1d1d1f]"
                              title={`请求 ID: ${item.requestId}（点击复制）`}
                              onClick={() => handleCopyId(item.requestId)}
                            >
                              <span className="truncate select-all">
                                {copiedId === item.requestId ? "已复制" : item.requestId}
                              </span>
                              {copiedId === item.requestId ? (
                                <IconCheck size={11} className="shrink-0 text-emerald-600" aria-hidden="true" />
                              ) : (
                                <IconCopy size={11} className="shrink-0 opacity-0 transition-opacity group-hover:opacity-100" aria-hidden="true" />
                              )}
                            </div>
                          </div>
                        </Table.Td>
                        <Table.Td>
                          {item.codexSessionId ? (
                            <div className="flex min-w-32 max-w-52 items-center gap-1.5">
                              {item.codexSessionIsParent ? (
                                <Badge variant="secondary" size="xs">
                                  父
                                </Badge>
                              ) : null}
                              <button
                                type="button"
                                className="group flex min-w-0 cursor-pointer items-center gap-1 border-0 bg-transparent p-0 font-mono text-[10px] text-[#6e6e73] transition-colors hover:text-[#1d1d1f]"
                                title={`${item.codexSessionIsParent ? "父会话" : "会话"} ID: ${item.codexSessionId}（点击复制）`}
                                aria-label={`复制${item.codexSessionIsParent ? "父会话" : "会话"} ID：${item.codexSessionId}`}
                                onClick={() => handleCopyId(item.codexSessionId!)}
                              >
                                <span className="truncate select-all">
                                  {copiedId === item.codexSessionId ? "已复制" : item.codexSessionId}
                                </span>
                                {copiedId === item.codexSessionId ? (
                                  <IconCheck size={11} className="shrink-0 text-emerald-600" aria-hidden="true" />
                                ) : (
                                  <IconCopy size={11} className="shrink-0 opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100" aria-hidden="true" />
                                )}
                              </button>
                            </div>
                          ) : (
                            <span className="text-[#8e8e93]">—</span>
                          )}
                        </Table.Td>
                        <Table.Td>
                          <div className="grid min-w-32 max-w-56 gap-0.5">
                            <strong
                              className="truncate font-semibold text-[#1d1d1f]"
                              title={item.providerName || item.provider || undefined}
                            >
                              {item.providerName || item.provider || "—"}
                            </strong>
                            {item.upstreamAuthority ? (
                              <span
                                className="truncate font-mono text-[10px] text-[#8e8e93]"
                                title={`上游: ${item.upstreamAuthority}`}
                              >
                                {item.upstreamAuthority}
                              </span>
                            ) : null}
                          </div>
                        </Table.Td>
                        <Table.Td className="max-w-56 truncate" title={item.model || item.requestedModel}>
                          <span className="font-medium text-[#1d1d1f]">
                            {item.model || item.requestedModel || "—"}
                          </span>
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-[#48484a]">{reasoningLabel(item)}</Table.Td>
                        <Table.Td>
                          <Badge variant="secondary" size="xs">
                            {(item.requestProtocol || "—").toUpperCase()}
                          </Badge>
                        </Table.Td>
                        <Table.Td>
                          <div className="grid min-w-20 gap-1">
                            <div className="flex items-center gap-1">
                              <Badge variant={presentation.variant} size="xs">
                                {presentation.label}
                              </Badge>
                              {hasUpstreamError ? (
                                <Tooltip
                                  content={(
                                    <span className="block max-w-[420px] break-words whitespace-normal">
                                      {upstreamErrorSummary}
                                    </span>
                                  )}
                                  getPopupContainer={() => container ?? document.body}
                                  position="top"
                                  autoAdjustOverflow
                                  zIndex={SETTINGS_OVERLAY_Z_INDEX}
                                >
                                  <ActionIcon
                                    size="xs"
                                    variant="subtle"
                                    color="red"
                                    aria-label={`查看上游错误信息：${upstreamErrorSummary}`}
                                  >
                                    <IconQuestionMark size={12} aria-hidden="true" />
                                  </ActionIcon>
                                </Tooltip>
                              ) : null}
                            </div>
                            {item.statusCode || item.errorCode ? (
                              <small className="whitespace-nowrap font-mono text-[10px] text-[#8e8e93]">
                                {item.statusCode ?? item.errorCode}
                              </small>
                            ) : null}
                          </div>
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono">
                          <div className="grid justify-items-end gap-0.5 leading-tight">
                            <div
                              className="flex items-center justify-end gap-1.5"
                              title={`首字耗时 (TTFT): ${formatDuration(item.ttftMs)}`}
                            >
                              <span className="text-[10px] text-[#8e8e93]">TTFT</span>
                              <span className="text-[11px] font-medium text-[#1d1d1f]">
                                {formatDuration(item.ttftMs)}
                              </span>
                            </div>
                            <div
                              className="flex items-center justify-end gap-1.5"
                              title={`总耗时: ${formatDuration(item.totalDurationMs)}`}
                            >
                              <span className="text-[10px] text-[#8e8e93]">总</span>
                              <span className="text-[11px] text-[#48484a]">
                                {formatDuration(item.totalDurationMs)}
                              </span>
                            </div>
                          </div>
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono text-[#48484a]">{formatTokens(item.inputTokens)}</Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono text-[#48484a]">{formatTokens(item.outputTokens)}</Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono text-[#48484a]">{formatTokens(item.cachedInputTokens)}</Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono font-medium text-[#1d1d1f]">
                          {item.totalTokens == null ? (
                            <div className="flex min-w-20 items-center justify-end gap-1">
                              <span className="text-[10px] font-medium text-[#8e8e93]">
                                {usageUnavailable.label}
                              </span>
                              <Tooltip
                                content={(
                                  <span className="block max-w-[360px] whitespace-normal">
                                    {usageUnavailable.message}
                                  </span>
                                )}
                                getPopupContainer={() => container ?? document.body}
                                position="top"
                                autoAdjustOverflow
                                zIndex={SETTINGS_OVERLAY_Z_INDEX}
                              >
                                <ActionIcon
                                  size="xs"
                                  variant="subtle"
                                  color="gray"
                                  aria-label={`Token 使用量不可用：${usageUnavailable.message}`}
                                >
                                  <IconQuestionMark size={12} aria-hidden="true" />
                                </ActionIcon>
                              </Tooltip>
                            </div>
                          ) : (
                            formatTokens(item.totalTokens)
                          )}
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono">
                          {item.retryCount > 0 ? (
                            <span className="font-semibold text-amber-600">{item.retryCount}</span>
                          ) : (
                            <span className="text-[#8e8e93]">0</span>
                          )}
                        </Table.Td>
                      </Table.Tr>
                    );
                  })}
                </Table.Tbody>
              </Table>
            </div>
          )}

          {result?.queryable && result.status === "ok" ? (
            <div className="flex flex-none items-center justify-between gap-3 border-t border-black/8 bg-[#fafafa] px-3 py-2 max-[760px]:flex-col max-[760px]:items-stretch">
              <span className="text-[11px] text-[#6e6e73]">
                共 {result.total.toLocaleString()} 条，当前显示 {firstVisible.toLocaleString()}–{lastVisible.toLocaleString()}
              </span>
              <div className="flex items-center justify-end gap-3 max-[520px]:flex-col max-[520px]:items-stretch">
                <Select
                  aria-label="请求日志每页条数"
                  className="w-32"
                  getPopupContainer={() => container ?? document.body}
                  optionList={pageSizeOptions}
                  value={pageSize}
                  zIndex={SETTINGS_OVERLAY_Z_INDEX}
                  onChange={(value) => {
                    setPageSize(Number(value) || 20);
                    setPage(1);
                  }}
                />
                <Pagination
                  size="sm"
                  total={Math.max(result.totalPages, 1)}
                  value={page}
                  onChange={setPage}
                  disabled={loading || result.totalPages <= 1}
                  withEdges
                />
              </div>
            </div>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}
