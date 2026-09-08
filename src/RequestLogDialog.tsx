import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Loader,
  Modal,
} from "@mantine/core";
import {
  IconAlertCircle,
  IconAlertTriangle,
  IconCheck,
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
import { invoke } from "./api";
import { formatTimestamp } from "./formatters";
import {
  Badge,
  Button,
  ActionIcon,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
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
  trend: Array<{ timestampUnixMs: number; total: number; totalTokensSum: number | null; avgDuration: number | null }>;
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
  onClose,
  standalone = false,
}: RequestLogDialogProps) {
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("all");
  const [model, setModel] = useState("all");
  const [status, setStatus] = useState("all");
  const [protocol, setProtocol] = useState("all");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [cursors, setCursors] = useState<Array<LogCursor | null>>([null]);
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
  const copyToastTimer = useRef<number | null>(null);
  const requestRevision = useRef(0);
  const listTask = useRef(Promise.resolve());
  const statsTask = useRef(Promise.resolve());
  const clearInFlight = useRef(false);

  const handleCopyId = (requestId: string, customLabel?: string) => {
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
          subtext: requestId,
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
    const models = new Set<string>();
    for (const modelsByProvider of [
      catalog.selectedModelsByProvider,
      catalog.declaredOfficialModelsByProvider,
      catalog.upstreamModelsByProvider,
    ]) {
      for (const values of Object.values(modelsByProvider)) {
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
    catalog.declaredOfficialModelsByProvider,
    catalog.selectedModelsByProvider,
    catalog.upstreamModelsByProvider,
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
    setResult(null);
    // Keep one list query in flight; superseded queued queries never reach SQLite.
    listTask.current = listTask.current.then(async () => {
      if (!active || revision !== requestRevision.current) return;
      try {
        const nextResult = await invoke<RouteRequestLogQueryPage>("query_route_request_logs", {
          ...filters, pageSize, cursor,
        });
        if (active && revision === requestRevision.current) setResult(nextResult);
      } catch (nextError) {
        if (active) setError(nextError instanceof Error ? nextError.message : String(nextError));
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
        if (active) setStatsError(nextError instanceof Error ? nextError.message : String(nextError));
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
    setPage(1);
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
      setPage(1);
      setCursors([null]);
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
        text: nextError instanceof Error ? nextError.message : String(nextError),
      });
      setClearConfirmationOpened(false);
    } finally {
      clearInFlight.current = false;
      setClearing(false);
    }
  };

  const hasFilters = Boolean(
    search || provider !== "all" || model !== "all" || status !== "all" || protocol !== "all" || requestKind !== "all",
  );
  const firstVisible = result?.items.length ? (page - 1) * pageSize + 1 : 0;
  const lastVisible = result?.items.length ? firstVisible + result.items.length - 1 : 0;

  return (
    <Modal
      fullScreen
      opened={opened}
      onClose={onClose}
      title="请求日志"
      withCloseButton={!standalone}
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
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4 max-[760px]:p-2.5">
        <div className="flex flex-none items-center justify-between gap-3 max-[640px]:flex-col max-[640px]:items-start">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-xs font-semibold text-[#1d1d1f]">内置路由请求日志</span>
              {result?.status === "ok" ? (
                <span className="rounded-full border border-emerald-600/15 bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-700">
                  按筛选范围统计
                </span>
              ) : null}
            </div>
            <p className="m-0 mt-0.5 text-xs text-[#6e6e73]">
              查看内置路由的供应商、模型、耗时与 Token 使用情况。
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <Button
              size="sm"
              variant="destructive-light"
              disabled={loading || clearing}
              onClick={() => {
                setActionNotice(null);
                setClearConfirmationOpened(true);
              }}
            >
              <IconTrash aria-hidden="true" />
              删除请求日志
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={loading || clearing}
              onClick={() => {
                setActionNotice(null);
                setPage(1);
                setCursors([null]);
                setRefreshRevision((value) => value + 1);
              }}
            >
              <IconRefresh className={loading ? "animate-spin" : ""} aria-hidden="true" />
              刷新
            </Button>
          </div>
        </div>

        {actionNotice ? (
          <Alert
            className="flex-none"
            color={actionNotice.tone === "success" ? "green" : "red"}
            icon={actionNotice.tone === "success"
              ? <IconCheck size={18} aria-hidden="true" />
              : <IconAlertCircle size={18} aria-hidden="true" />}
            title={actionNotice.tone === "success" ? "删除成功" : "删除失败"}
            withCloseButton
            onClose={() => setActionNotice(null)}
          >
            {actionNotice.text}
          </Alert>
        ) : null}

        {statsLoading ? <p className="m-0 text-xs text-[#6e6e73]" role="status">正在统计所选范围…</p> : null}
        {statsError ? <Alert color="red" title="统计加载失败">{statsError}</Alert> : null}
        {health ? <div role={healthWarning ? "alert" : "status"}
          className={`flex-none rounded-lg px-3 py-2 text-xs ${healthWarning ? "bg-amber-50 text-amber-900" : "bg-white text-[#6e6e73]"}`}>
          {health.active ? "日志记录中" : health.enabled ? "日志记录已停止，请重新开启记录并检查存储" : "日志记录未开启"}
          {` · 当前记录周期已处理 ${health.entriesWritten.toLocaleString()} 条 · 待写入 ${health.pendingEntries.toLocaleString()} 条`}
          {healthWarning ? ` · 丢弃 ${dropped} 条 · 写入失败 ${health.writeFailures} 次 · 采样省略 ${health.sampledOut} 条 · 记录器异常 ${health.observerPanics + health.writerPanics + health.shutdownTimeouts} 次` : ""}
          {health.sampleRatePerMillion < 1_000_000 ? " · 已配置采样，统计不代表全部请求" : ""}
          <span className="ml-2">异步记录；异常退出可能丢失尚未落盘的日志。</span>
        </div> : null}
        {stats ? (
          <div className="grid flex-none grid-cols-2 gap-2.5 sm:grid-cols-4">
            <div className="flex flex-col justify-between rounded-xl border border-black/8 bg-white p-3 shadow-xs">
              <span className="text-[11px] font-medium text-[#8e8e93]">总请求数</span>
              <div className="mt-1 flex items-baseline gap-1.5">
                <span className="text-lg font-bold text-[#1d1d1f] tabular-nums">
                  {stats.total.toLocaleString()}
                </span>
                <span className="text-[10px] text-[#8e8e93]">条</span>
              </div>
              <span className="mt-0.5 text-[10px] text-[#6e6e73]">
                {loading ? "列表加载中" : `当前显示 ${firstVisible.toLocaleString()}–${lastVisible.toLocaleString()} 条`}
              </span>
            </div>
            <div className="flex flex-col justify-between rounded-xl border border-black/8 bg-white p-3 shadow-xs">
              <span className="text-[11px] font-medium text-[#8e8e93]">请求成功率</span>
              <div className="mt-1 flex items-baseline gap-1.5">
                <span
                  className={`text-lg font-bold tabular-nums ${
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
              <span className="mt-0.5 text-[10px] text-[#6e6e73]">
                成功 {stats.succeededCount} · 失败 {stats.failedCount} · 其他 {stats.incompleteCount + stats.cancelledCount}
              </span>
            </div>
            <div className="flex flex-col justify-between rounded-xl border border-black/8 bg-white p-3 shadow-xs">
              <span className="text-[11px] font-medium text-[#8e8e93]">平均首字耗时 (TTFT)</span>
              <div className="mt-1 flex items-baseline gap-1.5">
                <span className="text-lg font-bold text-[#1d1d1f] tabular-nums">
                  {formatDuration(stats.avgTtft)}
                </span>
              </div>
              <span className="mt-0.5 text-[10px] text-[#6e6e73]">
                平均总耗时 {formatDuration(stats.avgDuration)}
              </span>
            </div>
            <div className="flex flex-col justify-between rounded-xl border border-black/8 bg-white p-3 shadow-xs">
              <span className="text-[11px] font-medium text-[#8e8e93]">所选范围 Token 消耗</span>
              <div className="mt-1 flex items-baseline gap-1.5">
                <span className="text-lg font-bold text-[#1d1d1f] tabular-nums">
                  {formatTokens(stats.totalTokensSum)}
                </span>
                <span className="text-[10px] text-[#8e8e93]">tokens</span>
              </div>
              <span className="mt-0.5 text-[10px] font-medium text-purple-600">
                输入 {formatTokens(stats.inputTokensSum)} · 输出 {formatTokens(stats.outputTokensSum)}
                <br />总量已知 {stats.totalTokensKnownCount.toLocaleString()} / {stats.total.toLocaleString()} 条
              </span>
            </div>
          </div>
        ) : null}

        <div className="grid flex-none grid-cols-4 gap-2 max-[760px]:grid-cols-2">
          <Select aria-label="请求日志时间范围" value={timeRange}
            getPopupContainer={() => container ?? document.body} zIndex={SETTINGS_OVERLAY_Z_INDEX}
            optionList={[{ label: "最近 24 小时", value: "24h" }, { label: "最近 7 天", value: "7d" }, { label: "最近 30 天", value: "30d" }, { label: "自定义时间", value: "custom" }]}
            onChange={(value) => { setTimeRange(String(value)); setPage(1); }} />
          <Select aria-label="搜索方式" value={searchMode}
            getPopupContainer={() => container ?? document.body} zIndex={SETTINGS_OVERLAY_Z_INDEX}
            optionList={[{ label: "关键词搜索", value: "contains" }, { label: "精确请求 ID", value: "requestId" }, { label: "精确会话 ID", value: "sessionId" }]}
            onChange={(value) => { setSearchMode(String(value)); setPage(1); }} />
          <Select aria-label="请求类型" value={requestKind}
            getPopupContainer={() => container ?? document.body} zIndex={SETTINGS_OVERLAY_Z_INDEX}
            optionList={[{ label: "全部请求类型", value: "all" }, { label: "模型请求", value: "responses" }, { label: "上下文压缩", value: "responses_compact" }, { label: "新版上下文压缩", value: "responses_compact_v2" }, { label: "图像生成", value: "images_generations" }, { label: "模型列表", value: "models" }, { label: "拒绝的请求", value: "http_rejected" }]}
            onChange={(value) => { setRequestKind(String(value)); setPage(1); }} />
          <Select aria-label="统计分组" value={groupBy}
            getPopupContainer={() => container ?? document.body} zIndex={SETTINGS_OVERLAY_Z_INDEX}
            optionList={[{ label: "按实际模型统计", value: "model" }, { label: "按供应商统计", value: "provider" }, { label: "按状态统计", value: "status" }, { label: "按协议统计", value: "protocol" }, { label: "按请求类型统计", value: "request_kind" }, { label: "按会话统计", value: "session" }]}
            onChange={(value) => setGroupBy(String(value))} />
          {timeRange === "custom" ? <>
            <label className="text-xs text-[#6e6e73]">开始时间（本地）
              <Input type="datetime-local" aria-label="开始时间" value={customFrom}
                onChange={(event) => { setCustomFrom(event.currentTarget.value); setPage(1); }} />
            </label>
            <label className="text-xs text-[#6e6e73]">结束时间（不包含）
              <Input type="datetime-local" aria-label="结束时间" value={customTo}
                onChange={(event) => { setCustomTo(event.currentTarget.value); setPage(1); }} />
            </label>
          </> : null}
        </div>

        {stats ? <details className="flex-none rounded-xl border border-black/8 bg-white p-3 text-xs">
          <summary className="cursor-pointer font-medium">趋势与分组统计 · 成功率包含失败、未完成和中断请求</summary>
          <p className="mb-0 text-[#6e6e73]">请求时间范围：{new Date(stats.fromUnixMs).toLocaleString()} 至 {new Date(stats.toUnixMs).toLocaleString()}（不含结束时间）
            {stats.databaseBytes != null ? ` · 日志存储约 ${((stats.databaseBytes + (stats.walBytes ?? 0)) / 1_048_576).toFixed(1)} MiB` : ""}
          </p>
          <div className="mt-2 grid max-h-40 grid-cols-2 gap-5 overflow-auto max-[640px]:grid-cols-1">
            <div>
              <p className="m-0 mb-1 text-[#6e6e73]">{stats.bucketMs === 3_600_000 ? "每小时" : "每天"}趋势 · 时间桶按 UTC 划分，显示本地时间；未列出的时间桶无请求</p>
              <table className="w-full text-left"><thead><tr><th>时间</th><th>请求数</th><th>Token</th><th>平均耗时</th></tr></thead>
                <tbody>{stats.trend.map((bucket) => <tr key={bucket.timestampUnixMs}>
                  <td>{new Date(bucket.timestampUnixMs).toLocaleString()}</td><td>{bucket.total}</td>
                  <td>{formatTokens(bucket.totalTokensSum)}</td><td>{formatDuration(bucket.avgDuration)}</td>
                </tr>)}</tbody>
              </table>
            </div>
            <div>
              <p className="m-0 mb-1 text-[#6e6e73]">{stats.groupsTruncated ? "请求数最多的 50 组，其余分组已省略" : "所选维度统计"}</p>
              <table className="w-full text-left"><thead><tr><th>分组</th><th>请求数</th><th>Token</th><th>成功率</th></tr></thead>
                <tbody>{stats.groups.map((group) => <tr key={group.key}>
                  <td className="max-w-48 truncate" title={group.key}>{group.key || "未知"}</td><td>{group.total}</td>
                  <td>{formatTokens(group.totalTokensSum)}</td><td>{group.successRate?.toFixed(1) ?? "—"}%</td>
                </tr>)}</tbody>
              </table>
            </div>
          </div>
        </details> : null}

        <div className="grid flex-none grid-cols-[minmax(220px,1.6fr)_repeat(4,minmax(132px,1fr))_auto] gap-2 rounded-xl border border-black/8 bg-white p-3 shadow-sm max-[1100px]:grid-cols-3 max-[640px]:grid-cols-1">
          <Input
            aria-label="搜索请求 ID、会话 ID、供应商、模型或上游"
            placeholder="搜索请求 ID、会话 ID、供应商、模型或上游"
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
            aria-label="按实际模型筛选请求日志"
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
            aria-label="按上游协议筛选请求日志"
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
            className={hasFilters ? "text-blue-600 hover:text-blue-700" : ""}
          >
            清除筛选
          </Button>
        </div>

        <div className="relative flex min-h-64 flex-1 flex-col overflow-hidden rounded-xl border border-black/8 bg-white shadow-sm max-[760px]:min-h-96">
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
              <Table highlightOnHover withColumnBorders withRowBorders className="min-w-[1360px] text-xs">
                <Table.Thead className="sticky top-0 z-[1] bg-[#f8f8fa] shadow-[0_1px_0_rgba(0,0,0,0.08)]">
                  <Table.Tr>
                    <Table.Th className="whitespace-nowrap">时间 / 请求 ID</Table.Th>
                    <Table.Th className="w-40 max-w-40 whitespace-nowrap">会话 ID</Table.Th>
                    <Table.Th className="whitespace-nowrap">供应商 / 上游</Table.Th>
                    <Table.Th className="whitespace-nowrap">模型</Table.Th>
                    <Table.Th className="whitespace-nowrap">思考强度</Table.Th>
                    <Table.Th className="whitespace-nowrap">上游协议</Table.Th>
                    <Table.Th className="whitespace-nowrap">状态</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">TTFT / 总耗时</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">输入 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">输出 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">缓存 Token</Table.Th>
                    <Table.Th className="whitespace-nowrap text-right">总 Token</Table.Th>
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
                    const cancellation = cancellationPresentation(item);
                    const displayedTtft = item.downstreamFirstContentMs ?? item.ttftMs;
                    const timingTitle = item.downstreamFirstContentMs == null
                      ? `首字耗时 (旧指标，上游首包): ${formatDuration(item.ttftMs)}`
                      : `端到端首内容: ${formatDuration(item.downstreamFirstContentMs)} · 路由前置: ${formatDuration(item.routerPreUpstreamMs)} · 上游首包: ${formatDuration(item.upstreamFirstByteMs)}`;
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
                        <Table.Td className="w-40 max-w-40 overflow-hidden">
                          {item.codexSessionId ? (
                            <div className="flex w-36 max-w-36 items-center gap-1.5 overflow-hidden">
                              {item.codexSessionIsParent ? (
                                <Badge
                                  variant="secondary"
                                  size="xs"
                                  className="shrink-0 whitespace-nowrap"
                                >
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
                          <Badge
                            variant="secondary"
                            size="xs"
                            className={
                              item.upstreamTransport === "ws"
                                ? "border-cyan-600/25 bg-cyan-50 text-cyan-700"
                                : item.upstreamTransport === "http_sse"
                                  ? "border-purple-600/25 bg-purple-50 text-purple-700"
                                  : ""
                            }
                          >
                            {item.upstreamTransport === "http_sse" ? "SSE" : (item.upstreamTransport || "—").toUpperCase()}
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
                              {cancellation ? (
                                <Tooltip
                                  content={(
                                    <span className="block max-w-[420px] break-words whitespace-normal">
                                      {cancellation.message}
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
                                    aria-label={`查看中断原因：${cancellation.message}`}
                                  >
                                    <IconQuestionMark size={12} aria-hidden="true" />
                                  </ActionIcon>
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
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono">
                          <div className="grid justify-items-end gap-0.5 leading-tight">
                            <div
                              className="flex items-center justify-end gap-1.5"
                              title={timingTitle}
                            >
                              <span className="rounded bg-blue-50 px-1 py-0.2 text-[9px] font-semibold text-blue-600">TTFT</span>
                              <span className="text-[11px] font-medium text-[#1d1d1f] tabular-nums">
                                {formatDuration(displayedTtft)}
                              </span>
                            </div>
                            <div
                              className="flex items-center justify-end gap-1.5"
                              title={`总耗时: ${formatDuration(item.totalDurationMs)}`}
                            >
                              <span className="text-[10px] text-[#8e8e93]">总</span>
                              <span className="text-[11px] text-[#48484a] tabular-nums">
                                {formatDuration(item.totalDurationMs)}
                              </span>
                            </div>
                          </div>
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono text-[#48484a] tabular-nums">{formatTokens(item.inputTokens)}</Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono text-[#48484a] tabular-nums">{formatTokens(item.outputTokens)}</Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono text-[#48484a] tabular-nums">
                          {item.cachedInputTokens && item.cachedInputTokens > 0 ? (
                            <span className="font-medium text-purple-600">
                              {formatTokens(item.cachedInputTokens)}
                            </span>
                          ) : (
                            formatTokens(item.cachedInputTokens)
                          )}
                        </Table.Td>
                        <Table.Td className="whitespace-nowrap text-right font-mono font-medium text-[#1d1d1f] tabular-nums">
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
                第 {page} 页，当前显示 {firstVisible.toLocaleString()}–{lastVisible.toLocaleString()} 条
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
                <Button size="sm" variant="outline" disabled={loading || page <= 1}
                  onClick={() => setPage((value) => value - 1)}>上一页</Button>
                <Button size="sm" variant="outline" disabled={loading || !result.hasMore || !result.nextCursor}
                  onClick={() => {
                    setCursors((current) => [...current.slice(0, page), result.nextCursor]);
                    setPage((value) => value + 1);
                  }}>下一页</Button>
              </div>
            </div>
          ) : null}
        </div>
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
            zIndex={SETTINGS_OVERLAY_Z_INDEX}
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
    </Modal>
  );
}
