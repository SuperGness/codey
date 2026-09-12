import type React from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Alert, CloseButton, Drawer, Pagination, Spinner, Table } from "@heroui/react";
import { UNSAFE_PortalProvider } from "react-aria";
import {
  IconAlertTriangle,
  IconChartBar,
  IconCheck,
  IconChevronDown,
  IconChevronUp,
  IconCopy,
  IconDatabaseOff,
  IconLoader2,
  IconQuestionMark,
  IconRefresh,
  IconSearch,
  IconTrash,
  IconX,
} from "@tabler/icons-react";

import type { Config, Profile } from "./App.types";
import requestLogStyles from "./styles.request-log.css?inline";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import { formatTimestamp } from "./formatters";
import { QuotaEstimateDialog } from "./QuotaEstimateDialog";
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Select,
  Tooltip,
} from "./components/ui";

type RouteRequestLogItem = {
  requestId: string;
  traceId: string;
  timestampUnixMs: number;
  provider?: string | null;
  providerName?: string | null;
  requestedModel: string;
  requestedServiceTier?: string | null;
  serviceTier?: string | null;
  model?: string | null;
  reasoningEffort?: string | null;
  thinkingBudgetTokens?: number | null;
  ttftMs?: number | null;
  routerPreUpstreamMs?: number | null;
  upstreamFirstByteMs?: number | null;
  downstreamFirstContentMs?: number | null;
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
  fallbackCount: number;
  fallbackReason?: string | null;
  upstreamAuthority?: string | null;
  upstreamRequestHeaders?: string | null;
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
  nextCursor: LogCursor | null;
  hasMore: boolean;
};

type LogCursor = { timestampUnixMs: number; requestId: string };
type LogSummary = {
  total: number;
  succeededCount: number;
  failedCount: number;
  incompleteCount: number;
  cancelledCount: number;
  successRate: number | null;
  avgDuration: number | null;
  avgTtft: number | null;
  avgRouterPreUpstream: number | null;
  avgUpstreamHeader: number | null;
  avgUpstreamFirstByte: number | null;
  avgDownstreamFirstContent: number | null;
  avgQueueDelay: number | null;
  inputTokensSum: number | null;
  outputTokensSum: number | null;
  totalTokensSum: number | null;
  cachedTokensSum: number | null;
  usageReportedCount: number;
  totalTokensKnownCount: number;
};
type LogAnalytics = LogSummary & {
  queryable: boolean;
  fromUnixMs: number;
  toUnixMs: number;
  groups: Array<LogSummary & { key: string }>;
  groupsTruncated: boolean;
  trend: Array<{ timestampUnixMs: number; total: number; totalTokensSum: number | null; avgDuration: number | null; avgTtft: number | null; avgDownstreamFirstContent: number | null }>;
  bucketMs: number;
  databaseBytes?: number;
  walBytes?: number;
  recordingHealth?: {
    enabled: boolean; active: boolean; sampleRatePerMillion: number; pendingEntries: number;
    accepted: number; entriesWritten: number; sampledOut: number;
    droppedFull: number; droppedClosed: number; writeDropped: number; writeFailures: number;
    observerPanics: number; writerPanics: number; shutdownTimeouts: number;
  } | null;
};

type ClearRouteRequestLogsResult = {
  status: "ok" | "failed";
  message?: string;
  removedFileCount: number;
  removedFiles: string[];
  recordingEnabled: boolean;
  recordingActive: boolean;
  recordingRestarted: boolean;
  error?: string;
  restartError?: string;
};

type ActionNotice = {
  tone: "success" | "error";
  text: string;
};

export type RequestLogCatalog = {
  officialAccountAvailable: boolean;
  profiles: Array<Pick<Profile, "id" | "name" | "sourceProviderId">>;
  selectedModelsByProvider: Config["selectedModelsByProvider"];
  declaredOfficialModelsByProvider: Config["declaredOfficialModelsByProvider"];
  upstreamModelsByProvider: Config["upstreamModelsByProvider"];
};

type RequestLogDialogProps = {
  catalog: RequestLogCatalog;
  container: HTMLElement | null;
  opened: boolean;
  onClose: () => void;
  standalone?: boolean;
};

const statusOptions = [
  { label: "全部状态", value: "all" },
  { label: "成功", value: "succeeded" },
  { label: "失败", value: "failed" },
  { label: "未完成", value: "incomplete" },
  { label: "已中断", value: "cancelled" },
];

const protocolOptions = [
  { label: "全部上游协议", value: "all" },
  { label: "HTTP", value: "http" },
  { label: "SSE", value: "http_sse" },
  { label: "WebSocket", value: "ws" },
];

const pageSizeOptions = [20, 50, 100];

function protocolTagClass(transport?: string | null): string {
  const norm = (transport || "").toLowerCase();
  if (norm === "http_sse" || norm === "sse") return "request-log-protocol-sse";
  if (norm === "ws" || norm === "websocket") return "request-log-protocol-ws";
  if (norm === "http" || norm === "https") return "request-log-protocol-http";
  if (norm === "grpc") return "request-log-protocol-grpc";
  return "request-log-protocol-default";
}

type PaginationEntry = number | "ellipsis";
// 首页、末页与当前页前后各一页，其余以省略号折叠。
function paginationItems(current: number, total: number): PaginationEntry[] {
  const pages = new Set<number>([1, total, current - 1, current, current + 1].filter((page) => page >= 1 && page <= total));
  const sorted = Array.from(pages).sort((a, b) => a - b);
  const entries: PaginationEntry[] = [];
  sorted.forEach((page, index) => {
    if (index > 0 && page - sorted[index - 1] > 1) entries.push("ellipsis");
    entries.push(page);
  });
  return entries;
}

// 内嵌页面把弹层挂到 overlay 容器；独立页面沿用 UiProvider（document.body）。
function PortalScope({ container, children }: { container: HTMLElement | null | undefined; children: React.ReactNode }) {
  const getContainer = useCallback(() => container ?? null, [container]);
  if (!container) return <>{children}</>;
  return <UNSAFE_PortalProvider getContainer={getContainer}>{children}</UNSAFE_PortalProvider>;
}

type RequestLogTableColumn<Row> = { title: string; width: number; render: (record: Row) => React.ReactNode };
type RequestLogTableRow = { key: string; item: RouteRequestLogItem; cells: React.ReactNode[] };
function RequestLogTable({ columns, rows = [], onRowAction }: {
  columns: RequestLogTableColumn<RequestLogTableRow>[];
  rows?: RequestLogTableRow[];
  onRowAction: (key: string) => void;
}) {
  return (
    <Table className="request-log-table" variant="secondary">
      <Table.ScrollContainer className="overflow-auto">
        <Table.Content
          aria-label="请求日志"
          className="min-w-[1600px]"
          onRowAction={(key) => onRowAction(String(key))}
        >
          <Table.Header>
            {columns.map((column, index) => (
              <Table.Column key={column.title} isRowHeader={index === 0} style={{ width: column.width, minWidth: column.width }}>
                {column.title}
              </Table.Column>
            ))}
          </Table.Header>
          <Table.Body items={rows}>
            {(record) => (
              <Table.Row id={record.key} textValue={record.item.requestId} aria-label={`查看请求详情：${record.item.requestId}`}>
                {columns.map((column) => (
                  <Table.Cell key={column.title}>{column.render(record)}</Table.Cell>
                ))}
              </Table.Row>
            )}
          </Table.Body>
        </Table.Content>
      </Table.ScrollContainer>
    </Table>
  );
}

const groupByLabels: Record<string, string> = {
  model: "实际模型",
  provider: "供应商",
  status: "状态",
  protocol: "上游协议",
  request_kind: "请求类型",
  session: "会话",
};

const statusPresentation: Record<
  string,
  { label: string; variant: "success" | "destructive" | "warning" | "secondary" }
> = {
  succeeded: { label: "成功", variant: "success" },
  failed: { label: "失败", variant: "destructive" },
  incomplete: { label: "未完成", variant: "warning" },
  cancelled: { label: "已中断", variant: "secondary" },
};

const cancellationPresentations: Record<string, { label: string; message: string }> = {
  downstream_stream_header_write_failed: {
    label: "响应头未送达",
    message: "发送流式响应头时连接已断开，HTTP 响应未完整建立。",
  },
  downstream_event_write_failed: {
    label: "连接中断",
    message: "HTTP 响应已建立，但流式内容传输期间连接断开。通常是客户端停止请求、关闭页面或网络中断。",
  },
  downstream_stream_finish_failed: {
    label: "结束时断开",
    message: "流式内容已经发送，但连接在结束响应时断开。",
  },
  downstream_json_write_failed: {
    label: "响应未送达",
    message: "响应已经生成，但写回客户端时连接断开。",
  },
  downstream_proxy_write_failed: {
    label: "连接中断",
    message: "转发上游响应时客户端连接断开。",
  },
  downstream_error_write_failed: {
    label: "错误响应未送达",
    message: "错误响应已经生成，但写回客户端时连接断开。",
  },
};

function cancellationPresentation(item: RouteRequestLogItem) {
  if (item.status !== "cancelled") return null;
  if (item.errorCode && cancellationPresentations[item.errorCode]) {
    return cancellationPresentations[item.errorCode];
  }
  if (item.completionReason === "scope_dropped") {
    return {
      label: "任务提前结束",
      message: "请求处理任务在响应完成前结束，可能是客户端取消、路由重启或程序退出。",
    };
  }
  return {
    label: "请求未完成",
    message: "请求已经开始，但响应没有完整传输到客户端。",
  };
}

function optionalFilter(value: string) {
  return value === "all" ? undefined : value;
}

function formatDuration(value?: number | null) {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value < 1_000) return `${Math.round(value).toLocaleString()} ms`;
  return `${(value / 1_000).toFixed(value < 10_000 ? 2 : 1)} s`;
}

function formatTokens(value?: number | null) {
  return value == null ? "—" : value.toLocaleString();
}

function formatCacheHitRate(input?: number | null, cached?: number | null) {
  if (input == null || cached == null || !Number.isFinite(input) || !Number.isFinite(cached) || input <= 0 || cached < 0 || cached > input) return "—";
  return `${(cached / input * 100).toFixed(1)}%`;
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
  catalog,
  container,
  opened,
  onClose: _onClose,
  standalone = false,
}: RequestLogDialogProps) {
  const [searchInput, setSearchInput] = useState("");
  const [quotaOpen, setQuotaOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("all");
  const [model, setModel] = useState("all");
  const [status, setStatus] = useState("all");
  const [protocol, setProtocol] = useState("all");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [cursors, setCursors] = useState<Array<LogCursor | null>>([null]);
  // Reset page and cursors together in the same handler; a separate effect
  // would run one render late and let a stale cursor reach the query.
  const resetPagination = useCallback(() => {
    setPage(1);
    setCursors([null]);
  }, []);
  const [timeRange, setTimeRange] = useState("24h");
  const [customFrom, setCustomFrom] = useState("");
  const [customTo, setCustomTo] = useState("");
  const [searchMode, setSearchMode] = useState("contains");
  const [requestKind, setRequestKind] = useState("all");
  const [groupBy, setGroupBy] = useState("model");
  const [stats, setStats] = useState<LogAnalytics | null>(null);
  const [statsLoading, setStatsLoading] = useState(false);
  const [statsError, setStatsError] = useState("");
  const [refreshRevision, setRefreshRevision] = useState(0);
  const [result, setResult] = useState<RouteRequestLogQueryPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [clearConfirmationOpened, setClearConfirmationOpened] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [actionNotice, setActionNotice] = useState<ActionNotice | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [copyToast, setCopyToast] = useState<{
    text: string;
    subtext?: string;
  } | null>(null);
  const [selectedItem, setSelectedItem] = useState<RouteRequestLogItem | null>(null);
  const [overviewCollapsed, setOverviewCollapsed] = useState(() => {
    try {
      const stored = window.sessionStorage.getItem("codey_request_logs_overview_collapsed");
      return stored === null ? true : stored === "1";
    } catch {
      return true;
    }
  });

  const toggleOverviewCollapsed = () => {
    setOverviewCollapsed((prev) => {
      const next = !prev;
      try {
        window.sessionStorage.setItem("codey_request_logs_overview_collapsed", next ? "1" : "0");
      } catch {
        // ignore
      }
      return next;
    });
  };
  const [usedModels, setUsedModels] = useState<string[]>([]);
  const copyToastTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (copyToastTimer.current) window.clearTimeout(copyToastTimer.current);
    },
    [],
  );
  const requestRevision = useRef(0);
  const listTask = useRef(Promise.resolve());
  const statsTask = useRef(Promise.resolve());
  const clearInFlight = useRef(false);

  const handleCopyId = (requestId: string, customLabel?: string, toastSubtext = requestId) => {
    if (!navigator.clipboard) return;
    void navigator.clipboard.writeText(requestId).then(
      () => {
        setCopiedId(requestId);
        if (copyToastTimer.current) {
          window.clearTimeout(copyToastTimer.current);
        }
        const isSession = requestId.includes("-") || (requestId.length === 36 && !requestId.startsWith("req_"));
        const defaultLabel = isSession ? "会话 ID" : "请求 ID";
        const label = customLabel || defaultLabel;
        setCopyToast({
          text: `已复制${label}`,
          subtext: toastSubtext,
        });
        copyToastTimer.current = window.setTimeout(() => {
          setCopyToast(null);
          setCopiedId((current) => (current === requestId ? null : current));
        }, 2200);
      },
      () => undefined,
    );
  };

  const rangeEnd = useMemo(() => Date.now(), [opened, refreshRevision, timeRange, customTo]);
  const toUnixMs = timeRange === "custom" ? new Date(customTo).getTime() : rangeEnd;
  const fromUnixMs = timeRange === "custom" ? new Date(customFrom).getTime()
    : toUnixMs - ({ "24h": 1, "7d": 7, "30d": 30 }[timeRange] ?? 1) * 86_400_000;
  const validRange = Number.isFinite(fromUnixMs) && Number.isFinite(toUnixMs)
    && fromUnixMs >= 0 && fromUnixMs < toUnixMs && toUnixMs - fromUnixMs <= 366 * 86_400_000;
  const filters = useMemo(() => ({
    cursorMode: true, fromUnixMs, toUnixMs,
    ...(search ? { [searchMode === "requestId" ? "requestId" : searchMode === "sessionId" ? "sessionId" : "search"]: search } : {}),
    ...(optionalFilter(provider) ? { provider } : {}),
    ...(optionalFilter(model) ? { model } : {}),
    ...(optionalFilter(status) ? { status } : {}),
    ...(optionalFilter(protocol) ? { protocol } : {}),
    ...(optionalFilter(requestKind) ? { requestKind } : {}),
  }), [fromUnixMs, toUnixMs, search, searchMode, provider, model, status, protocol, requestKind]);
  const cursor = page === 1 ? null : cursors[page - 1] ?? null;
  const health = stats?.recordingHealth;
  const dropped = health ? health.droppedFull + health.droppedClosed + health.writeDropped : 0;
  const healthWarning = health && (dropped > 0 || !health.active || health.sampleRatePerMillion < 1_000_000
    || health.writeFailures > 0 || health.observerPanics > 0 || health.writerPanics > 0 || health.shutdownTimeouts > 0);

  const providerOptions = useMemo(() => {
    const providers = new Map<string, string>();
    for (const profile of catalog.profiles) {
      const value = profile.sourceProviderId || profile.id;
      if (value) providers.set(value, profile.name || value);
    }
    return [
      { label: "全部供应商", value: "all" },
      ...[...providers].map(([value, label]) => ({ label, value })),
    ];
  }, [catalog.profiles]);

  const modelOptions = useMemo(() => {
    const models = new Set<string>(usedModels);
    result?.items?.forEach((item) => {
      const name = item.model?.trim() || item.requestedModel?.trim();
      if (name) models.add(name);
    });
    const current = model === "all" ? "" : model.trim();
    if (current) models.add(current);
    return [
      { label: "全部模型", value: "all" },
      ...[...models]
        .sort((left, right) => left.localeCompare(right))
        .map((value) => ({ label: value, value })),
    ];
  }, [usedModels, result?.items, model]);

  useEffect(() => {
    if (!opened || !validRange) return;
    let active = true;
    void invoke<LogAnalytics>("query_route_request_log_stats", {
      fromUnixMs,
      toUnixMs,
      groupBy: "model",
      ...(optionalFilter(provider) ? { provider } : {}),
    })
      .then((res) => {
        if (!active) return;
        setUsedModels(res.groups.map((g) => g.key).filter((k) => k.trim()));
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [opened, fromUnixMs, toUnixMs, provider, validRange, refreshRevision]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setSearch(searchInput.trim());
      resetPagination();
    }, 300);
    return () => window.clearTimeout(timer);
  }, [searchInput]);

  useEffect(() => {
    if (!opened) return;
    const revision = ++requestRevision.current;
    let active = true;
    if (!validRange) {
      setError("请选择有效的开始与结束时间，范围不能超过 366 天。");
      setResult(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError("");
    // Keep one list query in flight; superseded queued queries never reach SQLite.
    listTask.current = listTask.current.then(async () => {
      if (!active || revision !== requestRevision.current) return;
      try {
        const nextResult = await invoke<RouteRequestLogQueryPage>("query_route_request_logs", {
          ...filters, pageSize, cursor,
        });
        if (active && revision === requestRevision.current) {
          setResult(nextResult);
          if (nextResult?.nextCursor) {
            setCursors((prev) => {
              const next = [...prev];
              next[page] = nextResult.nextCursor;
              return next;
            });
          }
        }
      } catch (nextError) {
        if (active) setError(errorText(nextError));
      } finally {
        if (active) setLoading(false);
      }
    });
    return () => { active = false; };
  }, [opened, filters, pageSize, cursor, validRange, refreshRevision]);

  useEffect(() => {
    if (!opened) return;
    let active = true;
    setStats(null);
    setStatsError("");
    setStatsLoading(validRange);
    if (!validRange) return;
    statsTask.current = statsTask.current.then(async () => {
      if (!active) return;
      try {
        const nextStats = await invoke<LogAnalytics>("query_route_request_log_stats", { ...filters, groupBy });
        if (active) setStats(nextStats.queryable ? nextStats : null);
      } catch (nextError) {
        if (active) setStatsError(errorText(nextError));
      } finally {
        if (active) setStatsLoading(false);
      }
    });
    return () => { active = false; };
  }, [opened, filters, groupBy, validRange, refreshRevision]);

  const resetFilters = () => {
    setSearchInput("");
    setSearch("");
    setProvider("all");
    setModel("all");
    setStatus("all");
    setProtocol("all");
    setRequestKind("all");
    setSearchMode("contains");
    setTimeRange("24h");
    setCustomFrom("");
    setCustomTo("");
    resetPagination();
  };

  const clearRequestLogs = async () => {
    if (clearInFlight.current) return;
    clearInFlight.current = true;
    setClearing(true);
    setActionNotice(null);
    try {
      const clearResult = await invoke<ClearRouteRequestLogsResult>(
        "clear_route_request_logs",
        {},
      );
      if (clearResult.status !== "ok") {
        throw new Error(clearResult.message || "删除请求日志失败");
      }
      requestRevision.current += 1;
      setLoading(false);
      resetPagination();
            setRefreshRevision((value) => value + 1);
      setResult((current) => current
        ? {
            ...current,
            page: 1,
            total: 0,
            totalPages: 0,
            items: [],
          }
        : current);
      setClearConfirmationOpened(false);
      setActionNotice({
        tone: "success",
        text: clearResult.removedFileCount > 0
          ? "请求日志已全部删除。"
          : "当前没有需要删除的历史请求日志。",
      });
    } catch (nextError) {
      setActionNotice({
        tone: "error",
        text: errorText(nextError),
      });
      setClearConfirmationOpened(false);
    } finally {
      clearInFlight.current = false;
      setClearing(false);
    }
  };

  const hasFilters = Boolean(
    search ||
      provider !== "all" ||
      model !== "all" ||
      status !== "all" ||
      protocol !== "all" ||
      requestKind !== "all" ||
      timeRange !== "24h",
  );

  const activeFilterCount = useMemo(() => {
    let count = 0;
    if (search) count += 1;
    if (provider !== "all") count += 1;
    if (model !== "all") count += 1;
    if (status !== "all") count += 1;
    if (protocol !== "all") count += 1;
    if (requestKind !== "all") count += 1;
    if (timeRange !== "24h") count += 1;
    return count;
  }, [search, provider, model, status, protocol, requestKind, timeRange]);
  const firstVisible = result?.items.length ? (page - 1) * pageSize + 1 : 0;
  const lastVisible = result?.items.length ? firstVisible + result.items.length - 1 : 0;
  const totalCount = stats?.total ?? result?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(totalCount / pageSize));

  if (!opened) return null;

  return (
    <div className="request-log-workspace relative flex h-full min-h-0 flex-1 flex-col">
      <style>{requestLogStyles}</style>
      {quotaOpen && catalog.officialAccountAvailable === true && <QuotaEstimateDialog container={standalone ? document.body : container} onClose={() => setQuotaOpen(false)} />}
      <div className="request-log-header">
        <div className="request-log-heading">
          <div><p className="request-log-eyebrow">CODEY / 内置路由</p><h1>请求日志</h1></div>
          {health ? (
            <div
              role={healthWarning ? "alert" : "status"}
              className={`flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[10px] font-medium ${
                healthWarning
                  ? "border border-amber-500/30 bg-amber-50 text-amber-900"
                  : "border border-black/8 bg-white text-[#6e6e73]"
              }`}
              title={`当前记录周期已处理 ${health.entriesWritten.toLocaleString()} 条 · 待写入 ${health.pendingEntries.toLocaleString()} 条 · 异步记录；异常退出可能丢失尚未落盘的日志。${
                healthWarning
                  ? ` · 丢弃 ${dropped} 条 · 写入失败 ${health.writeFailures} 次 · 采样省略 ${health.sampledOut} 条 · 记录器异常 ${health.observerPanics + health.writerPanics + health.shutdownTimeouts} 次`
                  : ""
              }${health.sampleRatePerMillion < 1_000_000 ? " · 已配置采样，统计不代表全部请求" : ""}`}
            >
              <span
                className={`h-1.5 w-1.5 rounded-full ${
                  healthWarning ? "bg-amber-500 animate-pulse" : "bg-emerald-500"
                }`}
              />
              <span>
                {health.active
                  ? "日志记录中"
                  : health.enabled
                    ? "日志记录已停止，请重新开启记录并检查存储"
                    : "日志记录未开启"}
              </span>
              <span className="hidden text-[10px] text-[#8e8e93] md:inline">
                · 已处理 {health.entriesWritten.toLocaleString()} 条
              </span>
            </div>
          ) : null}
        </div>

        <div className="request-log-actions">
          {catalog.officialAccountAvailable === true && (
            <Button variant="link" color="primary" size="sm" onClick={() => setQuotaOpen(true)}>周限额度估算</Button>
          )}
          <Button
            variant="outline"
            size="sm"
            loading={loading}
            disabled={clearing}
            onClick={() => {
              setActionNotice(null);
              resetPagination();
                            setRefreshRevision((value) => value + 1);
            }}
          >
            {loading ? null : <IconRefresh size={14} aria-hidden="true" />}
            刷新
          </Button>
          <Button
            variant="destructive-light"
            size="sm"
            disabled={loading || clearing}
            onClick={() => {
              setActionNotice(null);
              setClearConfirmationOpened(true);
            }}
          >
            <IconTrash size={14} aria-hidden="true" />
            删除请求日志
          </Button>
        </div>
      </div>

        {actionNotice ? (
          <Alert className="flex-none" status={actionNotice.tone === "success" ? "success" : "danger"}>
            <Alert.Indicator />
            <Alert.Content>
              <Alert.Title>{actionNotice.tone === "success" ? "删除成功" : "删除失败"}</Alert.Title>
              <Alert.Description>{actionNotice.text}</Alert.Description>
            </Alert.Content>
            <CloseButton aria-label="关闭提示" className="shrink-0" onPress={() => setActionNotice(null)} />
          </Alert>
        ) : null}

        <div className="request-log-filters">
          <div className="request-log-filter-controls">
            <div className="request-log-search">
              <Select
                aria-label="搜索方式"
                className="w-36 shrink-0"
                value={searchMode}
                optionList={[
                  { label: "关键词搜索", value: "contains" },
                  { label: "精确请求 ID", value: "requestId" },
                  { label: "精确会话 ID", value: "sessionId" },
                ]}
                onChange={(value) => {
                  setSearchMode(String(value));
                  resetPagination();
                }}
              />
              <Input
                className="min-w-0 flex-1"
                aria-label="搜索请求 ID、会话 ID、供应商、模型或上游"
                placeholder={
                  searchMode === "requestId"
                    ? "输入精确请求 ID 搜索…"
                    : searchMode === "sessionId"
                      ? "输入精确会话 ID 搜索…"
                      : "搜索请求 ID、会话 ID、供应商、模型或上游"
                }
                value={searchInput}
                leftSection={<IconSearch size={15} className="text-[#8e8e93]" aria-hidden="true" />}
                rightSection={
                  searchInput ? (
                    <button
                      type="button"
                      className="flex h-5 w-5 cursor-pointer items-center justify-center rounded-full text-[#8e8e93] hover:bg-black/5 hover:text-[#1d1d1f]"
                      onClick={() => setSearchInput("")}
                      aria-label="清空搜索"
                    >
                      <IconX size={12} aria-hidden="true" />
                    </button>
                  ) : undefined
                }
                onChange={(event) => setSearchInput(event.currentTarget.value)}
              />
            </div>

            <Select
              aria-label="请求日志时间范围"
              className="w-36 shrink-0"
              value={timeRange}
              optionList={[
                { label: "最近 24 小时", value: "24h" },
                { label: "最近 7 天", value: "7d" },
                { label: "最近 30 天", value: "30d" },
                { label: "自定义时间", value: "custom" },
              ]}
              onChange={(value) => {
                setTimeRange(String(value));
                resetPagination();
              }}
            />

            <Select
              aria-label="按供应商筛选请求日志"
              className="w-32 shrink-0"
              filter
              optionList={providerOptions}
              value={provider}
              onChange={(value) => {
                setProvider(String(value ?? "all"));
                resetPagination();
              }}
            />

            <Select
              aria-label="按实际模型筛选请求日志"
              className="w-36 shrink-0"
              filter
              optionList={modelOptions}
              value={model}
              onChange={(value) => {
                setModel(String(value ?? "all"));
                resetPagination();
              }}
            />

            <Select
              aria-label="按状态筛选请求日志"
              className="w-24 shrink-0"
              optionList={statusOptions}
              value={status}
              onChange={(value) => {
                setStatus(String(value ?? "all"));
                resetPagination();
              }}
            />

            <Select
              aria-label="按上游协议筛选请求日志"
              className="w-36 shrink-0"
              optionList={protocolOptions}
              value={protocol}
              onChange={(value) => {
                setProtocol(String(value ?? "all"));
                resetPagination();
              }}
            />

            <Select
              aria-label="按请求类型筛选请求日志"
              className="w-36 shrink-0"
              optionList={[
                { label: "全部请求类型", value: "all" },
                { label: "模型请求", value: "responses" },
                { label: "上下文压缩", value: "responses_compact" },
                { label: "新版上下文压缩", value: "responses_compact_v2" },
                { label: "图像生成", value: "images_generations" },
                { label: "模型列表", value: "models" },
                { label: "拒绝的请求", value: "http_rejected" },
              ]}
              value={requestKind}
              onChange={(value) => {
                setRequestKind(String(value));
                resetPagination();
              }}
            />

            <Button
              size="sm"
              variant="link"
              color="primary"
              disabled={!hasFilters}
              onClick={resetFilters}
              className={`shrink-0 ${hasFilters ? "font-medium" : ""}`}
            >
              清除筛选
              {activeFilterCount > 0 ? ` (${activeFilterCount})` : ""}
            </Button>
          </div>

          {timeRange === "custom" ? (
            <div className="flex flex-wrap items-center gap-2 border-t border-black/6 pt-2 text-xs">
              <label className="flex items-center gap-1.5 text-xs text-[#6e6e73]">
                <span>开始时间</span>
                <Input
                  type="datetime-local"
                  aria-label="开始时间"
                  className="w-44"
                  value={customFrom}
                  onChange={(event) => {
                    setCustomFrom(event.currentTarget.value);
                    resetPagination();
                  }}
                />
              </label>
              <label className="flex items-center gap-1.5 text-xs text-[#6e6e73]">
                <span>结束时间</span>
                <Input
                  type="datetime-local"
                  aria-label="结束时间"
                  className="w-44"
                  value={customTo}
                  onChange={(event) => {
                    setCustomTo(event.currentTarget.value);
                    resetPagination();
                  }}
                />
              </label>
            </div>
          ) : null}
        </div>


        {statsLoading ? <p className="m-0 text-xs text-[#6e6e73]" role="status">正在统计所选范围…</p> : null}
        {statsError ? (
          <Alert status="danger">
            <Alert.Indicator />
            <Alert.Content>
              <Alert.Title>统计加载失败</Alert.Title>
              <Alert.Description>{statsError}</Alert.Description>
            </Alert.Content>
          </Alert>
        ) : null}

        {stats ? (
          <div className="request-log-overview">
            <div className="request-log-overview-heading">
              <div className="flex items-center gap-2 min-w-0">
                <IconChartBar size={14} className="text-[#1d1d1f] shrink-0" aria-hidden="true" />
                <span className="text-xs font-semibold text-[#1d1d1f] shrink-0">范围概览</span>
                {overviewCollapsed ? (
                  <div className="flex items-center gap-2.5 text-xs text-[#6e6e73] truncate ml-1">
                    <span>总请求: <strong className="font-semibold text-[#1d1d1f]">{stats.total.toLocaleString()}</strong> 条</span>
                    <span>成功率: <strong className={`font-semibold ${stats.successRate != null && stats.successRate >= 95 ? "text-emerald-600" : stats.successRate != null && stats.successRate >= 80 ? "text-amber-600" : "text-rose-600"}`}>{stats.successRate != null ? `${stats.successRate.toFixed(1)}%` : "—"}</strong></span>
                    <span className="hidden md:inline">TTFT: <strong className="font-semibold text-[#1d1d1f]">{formatDuration(stats.avgTtft)}</strong></span>
                    <span className="hidden lg:inline">Token: <strong className="font-semibold text-[#1d1d1f]">{formatTokens(stats.totalTokensSum)}</strong></span>
                  </div>
                ) : (
                  <span className="text-[11px] text-[#73767d] shrink-0 hidden sm:inline">按筛选范围统计</span>
                )}
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {!overviewCollapsed ? (
                  <Select
                    aria-label="统计分组"
                    className="w-44"
                    optionList={[
                      { label: "按实际模型统计", value: "model" },
                      { label: "按供应商统计", value: "provider" },
                      { label: "按状态统计", value: "status" },
                      { label: "按协议统计", value: "protocol" },
                      { label: "按请求类型统计", value: "request_kind" },
                      { label: "按会话统计", value: "session" },
                    ]}
                    value={groupBy}
                    onChange={(value) => setGroupBy(String(value))}
                  />
                ) : null}
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={toggleOverviewCollapsed}
                  aria-expanded={!overviewCollapsed}
                  className="text-xs text-[#6e6e73] hover:text-[#1d1d1f]"
                  title={overviewCollapsed ? "展开概览" : "收起概览以扩大列表区域"}
                >
                  {overviewCollapsed ? <IconChevronDown size={13} aria-hidden="true" /> : <IconChevronUp size={13} aria-hidden="true" />}
                  {overviewCollapsed ? "展开概览" : "收起概览"}
                </Button>
              </div>
            </div>

            {!overviewCollapsed ? (
              <>
                <div className="request-log-metrics">
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[#73767d]">总请求数</span>
                      <div className="flex items-baseline gap-1">
                        <span className="text-[15px] font-bold text-[#1d1d1f] tabular-nums">
                          {stats.total.toLocaleString()}
                        </span>
                        <span className="text-[10px] text-[#8e8e93]">条</span>
                      </div>
                    </div>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[#73767d]">上游首字节</span>
                      <span className="text-[15px] font-bold text-[#1d1d1f] tabular-nums">{formatDuration(stats.avgUpstreamFirstByte)}</span>
                    </div>
                    <span className="text-[10px] text-[#8e8e93] truncate">路由准备 {formatDuration(stats.avgRouterPreUpstream)} · 响应头 {formatDuration(stats.avgUpstreamHeader)}</span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[#73767d]">下游首段内容</span>
                      <span className="text-[15px] font-bold text-[#1d1d1f] tabular-nums">{formatDuration(stats.avgDownstreamFirstContent)}</span>
                    </div>
                    <span className="text-[10px] text-[#8e8e93] truncate">日志排队 {formatDuration(stats.avgQueueDelay)}</span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[#73767d]">请求成功率</span>
                      <span
                        className={`text-[15px] font-bold tabular-nums ${
                          stats.successRate != null && stats.successRate >= 95
                            ? "text-emerald-600"
                            : stats.successRate != null && stats.successRate >= 80
                              ? "text-amber-600"
                              : "text-rose-600"
                        }`}
                      >
                        {stats.successRate != null ? `${stats.successRate.toFixed(1)}%` : "—"}
                      </span>
                    </div>
                    <span className="text-[10px] text-[#8e8e93] truncate">
                      成功 {stats.succeededCount} · 失败 {stats.failedCount} · 其他 {stats.incompleteCount + stats.cancelledCount}
                    </span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[#73767d]">平均首字耗时 (TTFT)</span>
                      <span className="text-[15px] font-bold text-[#1d1d1f] tabular-nums">
                        {formatDuration(stats.avgTtft)}
                      </span>
                    </div>
                    <span className="text-[10px] text-[#8e8e93] truncate">
                      平均总耗时 {formatDuration(stats.avgDuration)}
                    </span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[#73767d]">所选范围 Token 消耗</span>
                      <div className="flex items-baseline gap-1">
                        <span className="text-[15px] font-bold text-[#1d1d1f] tabular-nums">
                          {formatTokens(stats.totalTokensSum)}
                        </span>
                        <span className="text-[10px] text-[#8e8e93]">tokens</span>
                      </div>
                    </div>
                    <span className="text-[10px] text-purple-600 truncate">
                      输入 {formatTokens(stats.inputTokensSum)} · 输出 {formatTokens(stats.outputTokensSum)} · 总量已知 {stats.totalTokensKnownCount.toLocaleString()} / {stats.total.toLocaleString()} 条
                    </span>
                  </div>
                </div>

                <div className="flex flex-col gap-2 rounded-lg border border-black/8 bg-[#fafafa] p-2.5 text-xs">
                  <div className="flex flex-wrap items-center justify-between gap-1 text-[#6e6e73]">
                    <div className="flex items-center gap-1.5 font-medium text-[#1d1d1f]">
                      <span>趋势与分组统计</span>
                      <span className="text-[10px] font-normal text-[#8e8e93]">
                        · 成功率包含失败、未完成和中断请求
                      </span>
                    </div>
                    <span className="text-[10px] text-[#8e8e93]">
                      {new Date(stats.fromUnixMs).toLocaleString()} 至 {new Date(stats.toUnixMs).toLocaleString()}（不含结束时间）
                      {stats.databaseBytes != null ? ` · 存储约 ${((stats.databaseBytes + (stats.walBytes ?? 0)) / 1_048_576).toFixed(1)} MiB` : ""}
                    </span>
                  </div>

                  <div className="grid grid-cols-2 gap-3 max-[820px]:grid-cols-1">
                    {/* 时间趋势卡片 */}
                    <div className="flex flex-col overflow-hidden rounded-lg border border-black/6 bg-white shadow-2xs">
                      <div className="flex items-center justify-between border-b border-black/6 bg-[#f8f8fa] px-3 py-1.5">
                        <span className="text-[11px] font-semibold text-[#1d1d1f]">
                          {stats.bucketMs === 3_600_000 ? "每小时" : "每天"}趋势
                        </span>
                        <span className="text-[10px] text-[#8e8e93]">
                          UTC 划分，本地时间显示
                        </span>
                      </div>
                      <div className="max-h-36 overflow-auto">
                        <table className="w-full text-left text-[11px]">
                          <thead className="sticky top-0 z-[1] bg-[#f8f8fa] text-[10px] font-medium text-[#6e6e73] shadow-[0_1px_0_rgba(0,0,0,0.06)]">
                            <tr>
                              <th className="py-1.5 px-2.5 font-medium">时间</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">请求数</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">Token</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">平均耗时</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">首段内容</th>
                            </tr>
                          </thead>
                          <tbody className="divide-y divide-black/4 font-mono">
                            {stats.trend.length === 0 ? (
                              <tr>
                                <td colSpan={5} className="py-4 text-center text-xs text-[#8e8e93] font-sans">
                                  所选时间范围暂无趋势数据
                                </td>
                              </tr>
                            ) : (
                              stats.trend.map((bucket) => (
                                <tr key={bucket.timestampUnixMs} className="transition-colors hover:bg-blue-50/30">
                                  <td className="py-1 px-2.5 whitespace-nowrap text-[#1d1d1f]">
                                    {new Date(bucket.timestampUnixMs).toLocaleString(undefined, {
                                      month: "2-digit",
                                      day: "2-digit",
                                      hour: "2-digit",
                                      minute: "2-digit",
                                    })}
                                  </td>
                                  <td className="py-1 px-2.5 text-right font-semibold text-[#1d1d1f] tabular-nums">
                                    {bucket.total.toLocaleString()}
                                  </td>
                                  <td className="py-1 px-2.5 text-right text-[#48484a] tabular-nums">
                                    {formatTokens(bucket.totalTokensSum)}
                                  </td>
                                  <td className="py-1 px-2.5 text-right text-[#48484a] tabular-nums">
                                    {formatDuration(bucket.avgDuration)}
                                  </td>
                                  <td className="py-1 px-2.5 text-right text-[#48484a] tabular-nums">
                                    {formatDuration(bucket.avgDownstreamFirstContent)}
                                  </td>
                                </tr>
                              ))
                            )}
                          </tbody>
                        </table>
                      </div>
                    </div>

                    {/* 分组统计卡片 */}
                    <div className="flex flex-col overflow-hidden rounded-lg border border-black/6 bg-white shadow-2xs">
                      <div className="flex items-center justify-between border-b border-black/6 bg-[#f8f8fa] px-3 py-1.5">
                        <span className="text-[11px] font-semibold text-[#1d1d1f]">
                          {groupByLabels[groupBy] || "所选维度"}统计
                        </span>
                        <span className="text-[10px] text-[#8e8e93]">
                          {stats.groupsTruncated ? "最多展示 50 组" : `共 ${stats.groups.length} 组`}
                        </span>
                      </div>
                      <div className="max-h-36 overflow-auto">
                        <table className="w-full text-left text-[11px]">
                          <thead className="sticky top-0 z-[1] bg-[#f8f8fa] text-[10px] font-medium text-[#6e6e73] shadow-[0_1px_0_rgba(0,0,0,0.06)]">
                            <tr>
                              <th className="py-1.5 px-2.5 font-medium">分组</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">请求数</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">Token</th>
                              <th className="py-1.5 px-2.5 text-right font-medium">成功率</th>
                            </tr>
                          </thead>
                          <tbody className="divide-y divide-black/4 font-mono">
                            {stats.groups.length === 0 ? (
                              <tr>
                                <td colSpan={4} className="py-4 text-center text-xs text-[#8e8e93] font-sans">
                                  所选分组暂无数据
                                </td>
                              </tr>
                            ) : (
                              stats.groups.map((group) => {
                                const rate = group.successRate;
                                const label = (groupBy === "provider"
                                  ? providerOptions.find((option) => option.value === group.key)?.label
                                  : undefined) || group.key || "未知";
                                return (
                                  <tr key={group.key} className="transition-colors hover:bg-blue-50/30">
                                    <td className="py-1 px-2.5 text-[#1d1d1f]" title={label}>
                                      <div className="flex flex-wrap items-center gap-x-2">
                                      <span className="max-w-[240px] truncate">{label}</span>
                                      </div>
                                    </td>
                                    <td className="py-1 px-2.5 text-right font-semibold text-[#1d1d1f] tabular-nums">
                                      {group.total.toLocaleString()}
                                    </td>
                                    <td className="py-1 px-2.5 text-right text-[#48484a] tabular-nums">
                                      {formatTokens(group.totalTokensSum)}
                                    </td>
                                    <td className="py-1 px-2.5 text-right tabular-nums">
                                      <span
                                        className={`font-semibold ${
                                          rate != null && rate >= 95
                                            ? "text-emerald-600"
                                            : rate != null && rate >= 80
                                              ? "text-amber-600"
                                              : "text-rose-600"
                                        }`}
                                      >
                                        {rate != null ? `${rate.toFixed(1)}%` : "—"}
                                      </span>
                                    </td>
                                  </tr>
                                );
                              })
                            )}
                          </tbody>
                        </table>
                      </div>
                    </div>
                  </div>
                </div>
              </>
            ) : null}
          </div>
        ) : null}

        <div className="request-log-results relative flex min-h-0 flex-1 flex-col overflow-hidden">
          <div className="request-log-results-heading"><strong>请求记录 <span>{totalCount.toLocaleString()}</span></strong><span>最新在前 · 点击记录查看详情</span></div>
          {loading && result ? (
            <div className="absolute top-0 left-0 right-0 z-10 h-0.5 overflow-hidden bg-blue-100">
              <div className="h-full w-full bg-blue-600 animate-pulse" />
            </div>
          ) : null}
          {error ? (
            <div className="grid min-h-48 flex-1 place-items-center p-6">
              <Alert status="danger">
                <Alert.Indicator />
                <Alert.Content>
                  <Alert.Title>请求日志加载失败</Alert.Title>
                  <Alert.Description>
                    <p className="m-0 mb-3 text-sm">{error}</p>
                    <Button size="xs" variant="outline" onClick={() => setRefreshRevision((value) => value + 1)}>
                      重试
                    </Button>
                  </Alert.Description>
                </Alert.Content>
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
                <Spinner size="sm" />
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
            <div className={`min-h-0 flex-1 overflow-auto ${loading && result ? "opacity-75 transition-opacity" : ""}`} aria-busy={loading}>
              <RequestLogTable
                columns={[
                  { title: "时间 / 请求 ID", width: 180, render: (record) => record.cells[0] },
                  { title: "会话 ID", width: 190, render: (record) => record.cells[1] },
                  { title: "供应商 / 上游", width: 180, render: (record) => record.cells[2] },
                  { title: "模型", width: 180, render: (record) => record.cells[3] },
                  { title: "思考强度", width: 100, render: (record) => record.cells[4] },
                  { title: "上游协议", width: 100, render: (record) => record.cells[5] },
                  { title: "状态", width: 120, render: (record) => record.cells[6] },
                  { title: "耗时", width: 150, render: (record) => record.cells[7] },
                  { title: "Token 用量", width: 230, render: (record) => record.cells[8] },
                  { title: "缓存 Token", width: 130, render: (record) => record.cells[9] },
                ]}
                rows={result?.items.map((item) => {
   const presentation = statusPresentation[item.status] ?? { label: item.status || "未知", variant: "secondary" as const };
   const hasUpstreamError = [item.statusCode, item.upstreamStatusCode].some((statusCode) => statusCode != null && (statusCode < 200 || statusCode >= 300));
   const upstreamErrorSummary = item.upstreamErrorSummary || item.errorCode || "上游未提供具体错误信息";
   const upstreamErrorPreview = upstreamErrorSummary.length > 512
     ? `${upstreamErrorSummary.slice(0, 512)}…（完整内容见详情）`
     : upstreamErrorSummary;
   const usageUnavailable = usageUnavailablePresentation(item.usageUnavailableReason);
   const cacheHitRate = formatCacheHitRate(item.inputTokens, item.cachedInputTokens);
   const cancellation = cancellationPresentation(item);
   const displayedTtft = item.downstreamFirstContentMs ?? item.ttftMs;
   const timingTitle = item.downstreamFirstContentMs == null
     ? `首字耗时 (旧指标，上游首包): ${formatDuration(item.ttftMs)}`
     : `端到端首内容: ${formatDuration(item.downstreamFirstContentMs)} · 路由前置: ${formatDuration(item.routerPreUpstreamMs)} · 上游首包: ${formatDuration(item.upstreamFirstByteMs)}`;
return { key: `${item.timestampUnixMs}:${item.requestId}`, item, cells: [<div>
                          <div className="grid min-w-36 max-w-44 gap-0.5 font-mono">
                            <span className="whitespace-nowrap text-[11px] text-[#1d1d1f]">
                              {formatTimestamp(item.timestampUnixMs)}
                            </span>
                            <Button
                              variant="ghost"
                              size="xs"
                              aria-label={`复制请求 ID：${item.requestId}`}
                              className="group h-auto min-h-0 justify-start gap-1 rounded-md px-0.5 font-mono text-[10px] font-normal text-[#8e8e93] transition-colors hover:text-[#1d1d1f] [&_svg]:size-[11px]"
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
                            </Button>
                          </div>
                        </div>,
<div className="w-40 max-w-40 overflow-hidden">
                          {item.codexSessionId ? (
                            <div className="flex w-36 max-w-36 items-center gap-1.5 overflow-hidden">
                              {item.codexSessionIsParent ? (
                                <Badge
                                  variant="secondary"
                                  className="shrink-0 whitespace-nowrap"
                                >
                                  父
                                </Badge>
                              ) : null}
                              <Button
                                variant="ghost"
                                size="xs"
                                className="group h-auto min-h-0 min-w-0 justify-start gap-1 rounded-md px-0.5 font-mono text-[10px] font-normal text-[#6e6e73] transition-colors hover:text-[#1d1d1f] [&_svg]:size-[11px]"
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
                              </Button>
                            </div>
                          ) : (
                            <span className="text-[#8e8e93]">—</span>
                          )}
                        </div>,
<div>
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
                        </div>,
<div className="max-w-56 truncate" title={item.model || item.requestedModel}>
                          <span className="font-medium text-[#1d1d1f]">
                            {item.model || item.requestedModel || "—"}
                          </span>
                        </div>,
<div className="whitespace-nowrap text-[#48484a]">{reasoningLabel(item)}</div>,
<div>
                          <Badge
                            variant="secondary"
                            className={`request-log-protocol ${protocolTagClass(item.upstreamTransport)}`}
                          >
                            {item.upstreamTransport === "http_sse" ? "SSE" : (item.upstreamTransport || "—").toUpperCase()}
                          </Badge>
                        </div>,
<div>
                          <div className="grid min-w-20 gap-1">
                            <div className="flex items-center gap-1">
                              <Badge variant={presentation.variant} className="request-log-status">
                                {presentation.label}
                              </Badge>
                              {hasUpstreamError ? (
                                <Tooltip
                                  content={(
                                    <span className="block max-w-[420px] break-words whitespace-normal">
                                      {upstreamErrorPreview}
                                    </span>
                                  )}
                                  position="top"
                                >
                                  <Button
                                    size="xs"
                                    variant="ghost"
                                    aria-label="查看上游错误信息"
                                  >
                                    <IconQuestionMark size={12} aria-hidden="true" />
                                  </Button>
                                </Tooltip>
                              ) : null}
                              {cancellation ? (
                                <Tooltip
                                  content={(
                                    <span className="block max-w-[420px] break-words whitespace-normal">
                                      {cancellation.message}
                                    </span>
                                  )}
                                  position="top"
                                >
                                  <Button
                                    size="xs"
                                    variant="ghost"
                                    aria-label={`查看中断原因：${cancellation.message}`}
                                  >
                                    <IconQuestionMark size={12} aria-hidden="true" />
                                  </Button>
                                </Tooltip>
                              ) : null}
                            </div>
                            {item.statusCode != null || item.errorCode || cancellation ? (
                              <small className="whitespace-nowrap font-mono text-[10px] text-[#8e8e93]">
                                {item.statusCode != null
                                  ? `HTTP ${item.statusCode}`
                                  : cancellation?.label || item.errorCode}
                                {cancellation && item.statusCode != null
                                  ? ` · ${cancellation.label}`
                                  : null}
                              </small>
                            ) : null}
                          </div>
                        </div>,
<div className="whitespace-nowrap tabular-nums">
                          <div className="grid gap-0.5 text-[11px] leading-4">
                            <div
                              className="flex items-center gap-1"
                              title={timingTitle}
                            >
                              <span className="text-[#6e6e73]">首字</span>
                              <span className="text-[11px] font-medium text-[#1d1d1f] tabular-nums">
                                {formatDuration(displayedTtft)}
                              </span>
                            </div>
                            <div
                              className="flex items-center gap-1"
                              title={`总耗时: ${formatDuration(item.totalDurationMs)}`}
                            >
                              <span className="text-[#6e6e73]">总用时</span>
                              <span className="text-[11px] text-[#48484a] tabular-nums">
                                {formatDuration(item.totalDurationMs)}
                              </span>
                            </div>
                          </div>
                        </div>,
<div className="grid gap-0.5 whitespace-nowrap tabular-nums">
                          <div className="flex items-center gap-1">
                            <span className="text-[11px] text-[#6e6e73]">总计:</span>
                          {item.totalTokens == null ? (
                            <div className="flex min-w-20 items-center gap-1">
                              <span className="text-[10px] font-medium text-[#8e8e93]">
                                {usageUnavailable.label}
                              </span>
                              <Tooltip
                                content={(
                                  <span className="block max-w-[360px] whitespace-normal">
                                    {usageUnavailable.message}
                                  </span>
                                )}
                                position="top"
                              >
                                <Button
                                  size="xs"
                                  variant="ghost"
                                  aria-label={`Token 使用量不可用：${usageUnavailable.message}`}
                                >
                                  <IconQuestionMark size={12} aria-hidden="true" />
                                </Button>
                              </Tooltip>
                            </div>
                          ) : (
                            <strong className="text-sm font-bold text-[#1d1d1f]">{formatTokens(item.totalTokens)}</strong>
                          )}
                          </div>
                          <div className="text-[10px] leading-4 text-[#6e6e73]">
                            输入: {formatTokens(item.inputTokens)} <span aria-hidden="true" className="text-[#c7c7cc]">|</span> 输出: {formatTokens(item.outputTokens)}
                          </div>
                          <div className="text-[10px] leading-4 text-[#6e6e73]">推理: {formatTokens(item.reasoningOutputTokens)}</div>
                        </div>,
<div className="grid gap-0.5 whitespace-nowrap tabular-nums" title="缓存命中率 = 缓存输入 Token / 输入 Token；未上报或无法计算时显示 —">
                          <strong className="text-sm font-bold text-[#1d1d1f]">{formatTokens(item.cachedInputTokens)}</strong>
                          <span className={`text-[10px] font-semibold leading-4 ${cacheHitRate === "—" ? "text-[#6e6e73]" : "text-[#c74735]"}`}>
                            {cacheHitRate} 命中
                          </span>
                        </div>] };})}
                onRowAction={(key) => {
                  const record = result?.items.find((item) => `${item.timestampUnixMs}:${item.requestId}` === key);
                  if (record) setSelectedItem(record);
                }}
              />
            </div>
          )}

          {result?.queryable && result.status === "ok" ? (
            <div className="request-log-pagination flex flex-none items-center justify-between gap-3 border-t border-black/8 px-3.5 py-2 text-xs max-[760px]:flex-col max-[760px]:items-stretch">
              <div className="flex flex-wrap items-center gap-2 text-[#6e6e73]">
                <span className="font-semibold text-[#1d1d1f]">
                  共 {totalCount.toLocaleString()} 条
                </span>
                <span className="text-black/20">·</span>
                <span>
                  当前显示 {firstVisible.toLocaleString()}–{lastVisible.toLocaleString()} 条
                </span>
                <span className="text-black/20">·</span>
                <span className="rounded bg-black/6 px-1.5 py-0.5 font-mono text-[11px] font-medium text-[#1d1d1f]">
                  第 {page} / {totalPages} 页
                </span>
              </div>
              <div className="flex items-center gap-2.5 shrink-0 max-[760px]:justify-end">
                <Select
                  aria-label="每页条数"
                  className="w-28 shrink-0"
                  disabled={loading}
                  value={pageSize}
                  optionList={pageSizeOptions.map((size) => ({ label: `${size} 条 / 页`, value: size }))}
                  onChange={(value) => {
                    const newPageSize = Number(value);
                    if (!Number.isFinite(newPageSize) || newPageSize === pageSize) return;
                    setPageSize(newPageSize);
                    resetPagination();
                                      }}
                />
                <Pagination size="sm" aria-label="请求日志分页" className="w-auto">
                  <Pagination.Content>
                    <Pagination.Item>
                      <Pagination.Previous
                        isDisabled={loading || page <= 1}
                        onPress={() => setPage(page - 1)}
                      >
                        <Pagination.PreviousIcon />
                      </Pagination.Previous>
                    </Pagination.Item>
                    {paginationItems(page, totalPages).map((entry, index) =>
                      entry === "ellipsis" ? (
                        <Pagination.Item key={`ellipsis-${index}`}>
                          <Pagination.Ellipsis />
                        </Pagination.Item>
                      ) : (
                        <Pagination.Item key={entry}>
                          <Pagination.Link
                            isActive={entry === page}
                            isDisabled={loading || entry > cursors.length}
                            aria-disabled={entry > cursors.length || undefined}
                            aria-label={`第 ${entry} 页`}
                            className={entry > cursors.length ? "cursor-not-allowed opacity-40" : undefined}
                            onPress={() => {
                              if (entry === page || entry > cursors.length) return;
                              setPage(entry);
                            }}
                          >
                            {entry}
                          </Pagination.Link>
                        </Pagination.Item>
                      ),
                    )}
                    <Pagination.Item>
                      <Pagination.Next
                        isDisabled={loading || page >= totalPages || !result?.hasMore || !result.nextCursor}
                        onPress={() => {
                          if (page + 1 <= cursors.length) setPage(page + 1);
                        }}
                      >
                        <Pagination.NextIcon />
                      </Pagination.Next>
                    </Pagination.Item>
                  </Pagination.Content>
                </Pagination>
              </div>
            </div>
          ) : null}
        </div>

      <Dialog
        open={clearConfirmationOpened}
        onOpenChange={(nextOpened) => {
          if (!nextOpened && !clearing) setClearConfirmationOpened(false);
        }}
      >
        {clearConfirmationOpened ? (
          <DialogContent
            className="w-[min(460px,calc(100vw-32px))]"
            container={standalone ? document.body : container}
            onEscapeKeyDown={(event) => {
              if (clearing) event.preventDefault();
            }}
            onPointerDownOutside={(event) => {
              if (clearing) event.preventDefault();
            }}
          >
            <DialogHeader>
              <DialogTitle>删除全部请求日志？</DialogTitle>
              <DialogDescription>
                这会删除全部历史请求日志，且不可恢复。正在进行的请求完成后仍可能产生新的日志。
              </DialogDescription>
            </DialogHeader>
            <div
              className="mt-4 flex items-start gap-2 rounded-[9px] border border-red-700/20 bg-red-50 px-3 py-2.5 text-xs leading-5 text-red-800"
              role="alert"
            >
              <IconAlertTriangle className="mt-0.5 shrink-0" size={17} aria-hidden="true" />
              <span>此操作只删除请求日志，不会关闭日志记录；后续请求仍会继续记录。</span>
            </div>
            <DialogFooter>
              <Button
                variant="outline"
                disabled={clearing}
                onClick={() => setClearConfirmationOpened(false)}
              >
                取消
              </Button>
              <Button
                variant="destructive"
                disabled={clearing}
                aria-busy={clearing}
                onClick={() => void clearRequestLogs()}
              >
                {clearing ? (
                  <IconLoader2 className="animate-spin" aria-hidden="true" />
                ) : (
                  <IconTrash aria-hidden="true" />
                )}
                {clearing ? "正在删除…" : "确认删除全部日志"}
              </Button>
            </DialogFooter>
          </DialogContent>
        ) : null}
      </Dialog>

      {selectedItem ? (
        <PortalScope container={standalone ? null : container}>
        <Drawer.Backdrop isOpen onOpenChange={(open) => { if (!open) setSelectedItem(null); }}>
            <Drawer.Content placement="right">
              <Drawer.Dialog className="w-[580px] max-w-[100vw] rounded-none p-0" aria-label="请求详情">
          <div
            className="request-log-detail relative z-10 flex h-full w-full max-w-[580px] flex-col bg-white shadow-2xl transition-transform"
          >
            {/* 抽屉头部 */}
            <div className="flex flex-none items-center justify-between border-b border-black/8 bg-[#fbfbfd] px-5 py-3.5">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <h3 className="m-0 text-sm font-bold text-[#1d1d1f]">请求详情</h3>
                  {(() => {
                    const pres = statusPresentation[selectedItem.status] ?? {
                      label: selectedItem.status || "未知",
                      variant: "secondary" as const,
                    };
                    return (
                      <Badge variant={pres.variant}>
                        {pres.label}
                      </Badge>
                    );
                  })()}
                  {selectedItem.statusCode != null ? (
                    <span className="font-mono text-[11px] text-[#6e6e73]">
                      HTTP {selectedItem.statusCode}
                    </span>
                  ) : null}
                </div>
                <p className="m-0 mt-0.5 truncate font-mono text-[11px] text-[#8e8e93]">
                  {formatTimestamp(selectedItem.timestampUnixMs)} · {selectedItem.requestId}
                </p>
              </div>
              <div className="flex items-center gap-1.5">
                <button
                  type="button"
                  className="flex h-7 w-7 cursor-pointer items-center justify-center rounded-lg text-[#8e8e93] hover:bg-black/5 hover:text-[#1d1d1f]"
                  onClick={() => setSelectedItem(null)}
                  aria-label="关闭详情"
                >
                  <IconX size={16} aria-hidden="true" />
                </button>
              </div>
            </div>

            {/* 抽屉内容区 */}
            <div className="flex-1 overflow-y-auto p-5 space-y-4 text-xs text-[#1d1d1f]">
              {/* 耗时与 Token 使用量 */}
              <div className="rounded-xl border border-black/8 bg-[#fafafa] p-3.5">
                {/* 耗时分解 */}
                <div>
                  <div className="flex items-center justify-between pb-2 border-b border-black/6">
                    <span className="font-semibold text-[#1d1d1f]">端到端耗时分解</span>
                    <span className="font-mono font-bold text-sm text-[#1d1d1f]">
                      {formatDuration(selectedItem.totalDurationMs)}
                    </span>
                  </div>
                  <div className="mt-3 space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-[#6e6e73]">端到端首内容 (TTFT)</span>
                      <span className="font-mono font-medium text-blue-600">
                        {formatDuration(selectedItem.downstreamFirstContentMs ?? selectedItem.ttftMs)}
                      </span>
                    </div>
                    {selectedItem.routerPreUpstreamMs != null ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[#6e6e73]">路由前置耗时</span>
                        <span className="font-mono">{formatDuration(selectedItem.routerPreUpstreamMs)}</span>
                      </div>
                    ) : null}
                    {selectedItem.upstreamFirstByteMs != null ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[#6e6e73]">上游首包耗时</span>
                        <span className="font-mono">{formatDuration(selectedItem.upstreamFirstByteMs)}</span>
                      </div>
                    ) : null}
                    {selectedItem.upstreamHeaderMs != null ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[#6e6e73]">上游响应头耗时</span>
                        <span className="font-mono">{formatDuration(selectedItem.upstreamHeaderMs)}</span>
                      </div>
                    ) : null}
                    {selectedItem.queueDelayMs > 0 ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[#6e6e73]">排队延迟</span>
                        <span className="font-mono text-amber-600">{formatDuration(selectedItem.queueDelayMs)}</span>
                      </div>
                    ) : null}
                  </div>
                </div>

                {/* Token 使用量 */}
                <div className="mt-3.5 pt-3.5 border-t border-black/6">
                  <span className="block font-semibold text-[#1d1d1f] pb-2 border-b border-black/6">
                    Token 使用量
                  </span>
                  {selectedItem.totalTokens == null ? (
                    <div className="mt-3 rounded-lg border border-black/6 bg-white p-3 text-center">
                      <span className="text-xs text-[#8e8e93]">
                        {usageUnavailablePresentation(selectedItem.usageUnavailableReason).label}：
                        {usageUnavailablePresentation(selectedItem.usageUnavailableReason).message}
                      </span>
                    </div>
                  ) : (
                    <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5">
                      <div>
                        <dt className="text-[11px] text-[#8e8e93]">总 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono text-base font-bold text-[#1d1d1f]">
                          {formatTokens(selectedItem.totalTokens)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-[11px] text-[#8e8e93]">缓存输入 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono text-base font-bold text-purple-600">
                          {formatTokens(selectedItem.cachedInputTokens)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-[11px] text-[#8e8e93]">输入 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono font-medium text-[#1d1d1f]">
                          {formatTokens(selectedItem.inputTokens)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-[11px] text-[#8e8e93]">输出 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono font-medium text-[#1d1d1f]">
                          {formatTokens(selectedItem.outputTokens)}
                        </dd>
                      </div>
                      {selectedItem.reasoningOutputTokens != null ? (
                        <div>
                          <dt className="text-[11px] text-[#8e8e93]">思考输出 Token</dt>
                          <dd className="m-0 mt-0.5 font-mono font-medium text-[#48484a]">
                            {formatTokens(selectedItem.reasoningOutputTokens)}
                          </dd>
                        </div>
                      ) : null}
                      {selectedItem.cacheCreationInputTokens != null ? (
                        <div>
                          <dt className="text-[11px] text-[#8e8e93]">缓存创建 Token</dt>
                          <dd className="m-0 mt-0.5 font-mono font-medium text-[#48484a]">
                            {formatTokens(selectedItem.cacheCreationInputTokens)}
                          </dd>
                        </div>
                      ) : null}
                    </dl>
                  )}
                </div>
              </div>

              {/* 模型与上游路由 */}
              <div className="rounded-xl border border-black/8 bg-[#fafafa] p-3.5">
                <span className="block font-semibold text-[#1d1d1f] pb-2 border-b border-black/6">
                  模型与路由
                </span>
                <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5">
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">供应商</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      {selectedItem.providerName || selectedItem.provider || "—"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">请求模型</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      {selectedItem.requestedModel}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">实际使用模型</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      {selectedItem.model || selectedItem.requestedModel}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">计费档位（请求 / 实际）</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      {selectedItem.requestedServiceTier || "未记录"} / {selectedItem.serviceTier || "未确认"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">思考强度 / 预算</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      {reasoningLabel(selectedItem)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">上游传输方式</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      <Badge
                        variant="secondary"
                        className={`request-log-protocol ${protocolTagClass(selectedItem.upstreamTransport)}`}
                      >
                        {selectedItem.upstreamTransport === "http_sse" ? "SSE" : (selectedItem.upstreamTransport || "—").toUpperCase()}
                      </Badge>
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">上游域名</dt>
                    <dd className="m-0 mt-0.5 font-mono text-[11px] text-[#48484a] truncate" title={selectedItem.upstreamAuthority || undefined}>
                      {selectedItem.upstreamAuthority || "—"}
                    </dd>
                  </div>
                  {selectedItem.upstreamRequestId ? (
                    <div className="col-span-2">
                      <dt className="text-[11px] text-[#8e8e93]">上游请求 ID</dt>
                      <dd className="m-0 mt-0.5 font-mono text-[11px] text-[#48484a] truncate">
                        {selectedItem.upstreamRequestId}
                      </dd>
                    </div>
                  ) : null}
                  {selectedItem.upstreamRequestHeaders ? (
                    <div className="col-span-2">
                      <dt className="flex items-center gap-1 text-[11px] text-[#8e8e93]">
                        <span>上游请求头（敏感值已脱敏）</span>
                        <button
                          type="button"
                          className="inline-flex items-center rounded p-0.5 text-[#8e8e93] transition-colors hover:bg-black/8 hover:text-[#1d1d1f] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500"
                          onClick={() => handleCopyId(selectedItem.upstreamRequestHeaders!, "上游请求头", "请求头内容")}
                          aria-label="复制上游请求头"
                          title="复制上游请求头"
                        >
                          <IconCopy size={12} aria-hidden="true" />
                        </button>
                      </dt>
                      <dd className="m-0 mt-1">
                        <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-black/5 p-2 font-mono text-[10px] leading-relaxed text-[#48484a] break-words">
                          {selectedItem.upstreamRequestHeaders}
                        </pre>
                      </dd>
                    </div>
                  ) : null}
                  {selectedItem.codexSessionId ? (
                    <div className="col-span-2">
                      <dt className="text-[11px] text-[#8e8e93]">Codex 会话 ID</dt>
                      <dd className="m-0 mt-0.5 flex items-center gap-1.5 font-mono text-[11px] text-[#48484a]">
                        {selectedItem.codexSessionIsParent ? (
                          <Badge variant="secondary">父会话</Badge>
                        ) : null}
                        <span className="truncate">{selectedItem.codexSessionId}</span>
                        <button
                          type="button"
                          className="text-blue-600 hover:text-blue-700 ml-1 cursor-pointer"
                          onClick={() => handleCopyId(selectedItem.codexSessionId!, selectedItem.codexSessionIsParent ? "父会话 ID" : "会话 ID")}
                        >
                          复制
                        </button>
                      </dd>
                    </div>
                  ) : null}
                  {selectedItem.subagent ? (
                    <div>
                      <dt className="text-[11px] text-[#8e8e93]">子代理请求</dt>
                      <dd className="m-0 mt-0.5 font-medium text-purple-600">是</dd>
                    </div>
                  ) : null}
                  {selectedItem.protocolBridge ? (
                    <div>
                      <dt className="text-[11px] text-[#8e8e93]">协议桥接</dt>
                      <dd className="m-0 mt-0.5 font-mono text-[11px] text-[#48484a]">
                        {selectedItem.protocolBridge}
                      </dd>
                    </div>
                  ) : null}
                </dl>
              </div>

              {/* 原始记录 JSON */}
              <div className="rounded-xl border border-black/8 bg-[#fafafa] p-3.5 text-xs">
                <span className="block font-semibold text-[#1d1d1f] pb-2 border-b border-black/6">
                  原始记录 JSON
                </span>
                <pre className="mt-3 max-h-60 overflow-auto rounded-lg bg-black/5 p-2.5 font-mono text-[10px] text-[#1d1d1f]">
                  {JSON.stringify(selectedItem, null, 2)}
                </pre>
              </div>

              {/* 异常与降级诊断 */}
              {(selectedItem.status !== "succeeded" || selectedItem.fallbackCount > 0 || selectedItem.upstreamErrorSummary || selectedItem.errorCode) ? (
                <div className="rounded-xl border border-red-200 bg-red-50/50 p-3.5">
                  <span className="block font-semibold text-red-900 pb-2 border-b border-red-200">
                    异常与诊断
                  </span>
                  <div className="mt-3 space-y-2">
                    {selectedItem.errorCode ? (
                      <div>
                        <span className="text-[11px] font-medium text-red-800">错误码：</span>
                        <code className="ml-1 rounded bg-red-100 px-1 py-0.5 font-mono text-[11px] text-red-900">
                          {selectedItem.errorCode}
                        </code>
                      </div>
                    ) : null}
                    {selectedItem.upstreamErrorSummary ? (
                      <div>
                        <span className="text-[11px] font-medium text-red-800">上游错误内容（已脱敏）：</span>
                        <pre className="m-0 mt-1 max-h-80 overflow-auto whitespace-pre-wrap rounded-lg bg-white p-2 text-[11px] leading-relaxed text-red-900 break-words">
                          {selectedItem.upstreamErrorSummary}
                        </pre>
                      </div>
                    ) : null}
                    {(() => {
                      const canc = cancellationPresentation(selectedItem);
                      return canc ? (
                        <div>
                          <span className="text-[11px] font-medium text-amber-800">中断原因：</span>
                          <p className="m-0 mt-1 rounded-lg bg-white p-2 text-[11px] leading-relaxed text-[#48484a]">
                            <strong className="font-semibold">{canc.label}：</strong>{canc.message}
                          </p>
                        </div>
                      ) : null;
                    })()}
                    {selectedItem.fallbackCount > 0 ? (
                      <div>
                        <span className="text-[11px] font-medium text-amber-800">
                          降级重试：已尝试 {selectedItem.fallbackCount} 次
                        </span>
                        {selectedItem.fallbackReason ? (
                          <p className="m-0 mt-1 text-[11px] text-[#6e6e73]">
                            原因: {selectedItem.fallbackReason}
                          </p>
                        ) : null}
                      </div>
                    ) : null}
                  </div>
                </div>
              ) : null}
            </div>

            {/* 抽屉底部操作栏 */}
            <div className="flex flex-none items-center justify-end gap-2 border-t border-black/8 bg-[#fafafa] px-5 py-3">
              <Button
                size="sm"
                variant="outline"
                onClick={() => setSelectedItem(null)}
              >
                关闭
              </Button>
              <Button
                size="sm"
                variant="default"
                onClick={() => handleCopyId(JSON.stringify(selectedItem, null, 2), "完整日志 JSON")}
              >
                <IconCopy size={13} aria-hidden="true" />
                复制完整 JSON
              </Button>
            </div>
          </div>
              </Drawer.Dialog>
            </Drawer.Content>
        </Drawer.Backdrop>
        </PortalScope>
      ) : null}

      {copyToast ? (
        <div
          role="status"
          aria-live="polite"
          className="pointer-events-auto fixed bottom-6 right-6 z-[1000] flex max-w-[min(420px,calc(100vw-32px))] items-center gap-2.5 rounded-xl border border-black/10 border-l-4 border-l-[#34c759] bg-white/95 px-4 py-3 text-xs text-[#1d1d1f] shadow-[0_12px_32px_rgba(0,0,0,0.14)] backdrop-blur-2xl transition-all duration-200"
        >
          <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-emerald-50 text-emerald-600">
            <IconCheck size={15} stroke={2.5} aria-hidden="true" />
          </div>
          <div className="min-w-0 flex-1">
            <p className="m-0 font-medium text-[#1d1d1f]">{copyToast.text}</p>
            {copyToast.subtext ? (
              <p className="m-0 mt-0.5 truncate font-mono text-[11px] text-[#8e8e93]">
                {copyToast.subtext}
              </p>
            ) : null}
          </div>
          <button
            type="button"
            className="ml-1 -mr-1 flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-[#8e8e93] hover:bg-black/5 hover:text-[#1d1d1f]"
            onClick={() => setCopyToast(null)}
            aria-label="关闭提示"
          >
            <IconX size={14} aria-hidden="true" />
          </button>
        </div>
      ) : null}
    </div>
  );
}
