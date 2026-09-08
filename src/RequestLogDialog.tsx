import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Loader,
  Modal,
} from "@mantine/core";
import {
  IconAlertCircle,
  IconAlertTriangle,
  IconChartBar,
  IconCheck,
  IconChevronRight,
  IconCopy,
  IconDatabaseOff,
  IconFilter,
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
  const [selectedItem, setSelectedItem] = useState<RouteRequestLogItem | null>(null);
  const [showStats, setShowStats] = useState(false);
  const [showAdvancedFilters, setShowAdvancedFilters] = useState(false);
  const [usedModels, setUsedModels] = useState<string[]>([]);
  const copyToastTimer = useRef<number | null>(null);
  const requestRevision = useRef(0);
  const listTask = useRef(Promise.resolve());
  const statsTask = useRef(Promise.resolve());
  const clearInFlight = useRef(false);

  useEffect(() => {
    if (!selectedItem) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setSelectedItem(null);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [selectedItem]);

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
    const models = new Set<string>(usedModels);
    result?.items?.forEach((item) => {
      if (item.model?.trim()) models.add(item.model.trim());
      else if (item.requestedModel?.trim()) models.add(item.requestedModel.trim());
    });
    if (model !== "all" && model.trim()) {
      models.add(model.trim());
    }
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
        if (!active || !res?.groups) return;
        const models = res.groups
          .map((g) => g.key)
          .filter((k): k is string => Boolean(k && k.trim()));
        setUsedModels(models);
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [opened, fromUnixMs, toUnixMs, provider, validRange, refreshRevision]);

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
    setTimeRange("24h");
    setCustomFrom("");
    setCustomTo("");
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
        body: "flex h-full min-h-0 flex-1 flex-col overflow-hidden! p-0!",
        content: "flex! h-[100dvh]! max-h-[100dvh]! min-h-0 flex-col overflow-hidden! bg-[#f5f5f7]",
        header: "m-0 flex-none border-b border-black/8 bg-white/90 px-5! py-3! backdrop-blur-xl",
        inner: "h-full! max-h-full! p-0!",
        title: "text-base font-bold text-[#1d1d1f]",
      }}
      lockScroll={false}
      padding={0}
      portalProps={container ? { target: container } : undefined}
      withinPortal={Boolean(container)}
      zIndex={SETTINGS_OVERLAY_Z_INDEX}
    >
      <div className="flex h-full min-h-0 flex-1 flex-col gap-2.5 overflow-hidden p-4 max-[760px]:p-2">
        <div className="flex flex-none items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-2.5">
            <span className="text-xs font-semibold text-[#1d1d1f]">内置路由请求日志</span>
            {result?.status === "ok" ? (
              <span className="shrink-0 rounded-full border border-emerald-600/15 bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-700">
                按筛选范围统计
              </span>
            ) : null}
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

          <div className="flex shrink-0 items-center gap-2">
            <Button
              size="sm"
              variant={showStats ? "secondary" : "outline"}
              onClick={() => setShowStats((prev) => !prev)}
            >
              <IconChartBar size={14} aria-hidden="true" />
              {showStats ? "收起看板" : "统计看板"}
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

        {stats && !showStats ? (
          <div className="flex flex-none flex-wrap items-center justify-between gap-2 rounded-xl border border-black/8 bg-white px-3.5 py-1.5 text-xs shadow-2xs">
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
              <div className="flex items-baseline gap-1.5">
                <span className="text-[11px] font-medium text-[#8e8e93]">总请求数</span>
                <span className="font-mono text-xs font-bold tabular-nums text-[#1d1d1f]">
                  {stats.total.toLocaleString()}
                </span>
                <span className="text-[10px] text-[#8e8e93]">条</span>
              </div>
              <div className="h-3 w-px bg-black/10" />
              <div className="flex items-baseline gap-1.5">
                <span className="text-[11px] font-medium text-[#8e8e93]">请求成功率</span>
                <span
                  className={`font-mono text-xs font-bold tabular-nums ${
                    stats.successRate != null && stats.successRate >= 95
                      ? "text-emerald-600"
                      : stats.successRate != null && stats.successRate >= 80
                        ? "text-amber-600"
                        : "text-rose-600"
                  }`}
                >
                  {stats.successRate != null ? `${stats.successRate.toFixed(1)}%` : "—"}
                </span>
                <span className="hidden text-[10px] text-[#8e8e93] sm:inline">
                  (成 {stats.succeededCount} · 败 {stats.failedCount})
                </span>
              </div>
              <div className="h-3 w-px bg-black/10" />
              <div className="flex items-baseline gap-1.5">
                <span className="text-[11px] font-medium text-[#8e8e93]">平均首字耗时 (TTFT)</span>
                <span className="font-mono text-xs font-bold tabular-nums text-[#1d1d1f]">
                  {formatDuration(stats.avgTtft)}
                </span>
                <span className="hidden text-[10px] text-[#8e8e93] sm:inline">
                  (总 {formatDuration(stats.avgDuration)})
                </span>
              </div>
              <div className="h-3 w-px bg-black/10" />
              <div className="flex items-baseline gap-1.5">
                <span className="text-[11px] font-medium text-[#8e8e93]">Token 消耗</span>
                <span className="font-mono text-xs font-bold tabular-nums text-[#1d1d1f]">
                  {formatTokens(stats.totalTokensSum)}
                </span>
                <span className="hidden text-[10px] font-medium text-purple-600 sm:inline">
                  (入 {formatTokens(stats.inputTokensSum)} · 出 {formatTokens(stats.outputTokensSum)})
                </span>
                <span className="hidden text-[10px] text-[#8e8e93] lg:inline">
                  · 总量已知 {stats.totalTokensKnownCount.toLocaleString()} / {stats.total.toLocaleString()} 条
                </span>
              </div>
            </div>
            <button
              type="button"
              className="flex shrink-0 cursor-pointer items-center gap-1 font-medium text-blue-600 transition-colors hover:text-blue-700"
              onClick={() => setShowStats(true)}
            >
              <span>展开统计与趋势</span>
              <IconChevronRight size={13} aria-hidden="true" />
            </button>
          </div>
        ) : null}

        {showStats && stats ? (
          <div className="flex flex-none flex-col gap-2 rounded-xl border border-black/8 bg-white p-3 shadow-xs">
            <div className="flex items-center justify-between pb-1.5 border-b border-black/6">
              <div className="flex items-center gap-2">
                <span className="text-xs font-bold text-[#1d1d1f]">统计概览与趋势</span>
                <span className="rounded-full border border-emerald-600/15 bg-emerald-50 px-2 py-0.5 text-[10px] font-medium text-emerald-700">
                  按筛选范围统计
                </span>
              </div>
              <div className="flex items-center gap-2">
                <Select
                  aria-label="统计分组"
                  className="w-36"
                  getPopupContainer={() => container ?? document.body}
                  zIndex={SETTINGS_OVERLAY_Z_INDEX}
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
                <Button
                  size="xs"
                  variant="ghost"
                  onClick={() => setShowStats(false)}
                >
                  收起看板
                </Button>
              </div>
            </div>

            <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
              <div className="flex flex-col justify-between rounded-lg border border-black/6 bg-[#fafafa] p-2.5 shadow-2xs">
                <span className="text-[11px] font-medium text-[#8e8e93]">总请求数</span>
                <div className="mt-1 flex items-baseline gap-1.5">
                  <span className="text-base font-bold text-[#1d1d1f] tabular-nums">
                    {stats.total.toLocaleString()}
                  </span>
                  <span className="text-[10px] text-[#8e8e93]">条</span>
                </div>
                <span className="mt-0.5 text-[10px] text-[#6e6e73]">
                  {loading ? "列表加载中" : `当前显示 ${firstVisible.toLocaleString()}–${lastVisible.toLocaleString()} 条`}
                </span>
              </div>
              <div className="flex flex-col justify-between rounded-lg border border-black/6 bg-[#fafafa] p-2.5 shadow-2xs">
                <span className="text-[11px] font-medium text-[#8e8e93]">请求成功率</span>
                <div className="mt-1 flex items-baseline gap-1.5">
                  <span
                    className={`text-base font-bold tabular-nums ${
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
              <div className="flex flex-col justify-between rounded-lg border border-black/6 bg-[#fafafa] p-2.5 shadow-2xs">
                <span className="text-[11px] font-medium text-[#8e8e93]">平均首字耗时 (TTFT)</span>
                <div className="mt-1 flex items-baseline gap-1.5">
                  <span className="text-base font-bold text-[#1d1d1f] tabular-nums">
                    {formatDuration(stats.avgTtft)}
                  </span>
                </div>
                <span className="mt-0.5 text-[10px] text-[#6e6e73]">
                  平均总耗时 {formatDuration(stats.avgDuration)}
                </span>
              </div>
              <div className="flex flex-col justify-between rounded-lg border border-black/6 bg-[#fafafa] p-2.5 shadow-2xs">
                <span className="text-[11px] font-medium text-[#8e8e93]">所选范围 Token 消耗</span>
                <div className="mt-1 flex items-baseline gap-1.5">
                  <span className="text-base font-bold text-[#1d1d1f] tabular-nums">
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
                        </tr>
                      </thead>
                      <tbody className="divide-y divide-black/4 font-mono">
                        {stats.trend.length === 0 ? (
                          <tr>
                            <td colSpan={4} className="py-4 text-center text-xs text-[#8e8e93] font-sans">
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
                            return (
                              <tr key={group.key} className="transition-colors hover:bg-blue-50/30">
                                <td className="py-1 px-2.5 text-[#1d1d1f] max-w-[150px] truncate" title={group.key || "未知"}>
                                  {group.key || "未知"}
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
          </div>
        ) : null}

        <div className="flex flex-none flex-col gap-2 rounded-xl border border-black/8 bg-white p-2.5 shadow-2xs">
          <div className="flex flex-wrap items-center gap-2">
            <div className="flex min-w-[290px] flex-1 items-center gap-1.5">
              <Select
                aria-label="搜索方式"
                className="w-36 shrink-0"
                value={searchMode}
                getPopupContainer={() => container ?? document.body}
                zIndex={SETTINGS_OVERLAY_Z_INDEX}
                optionList={[
                  { label: "关键词搜索", value: "contains" },
                  { label: "精确请求 ID", value: "requestId" },
                  { label: "精确会话 ID", value: "sessionId" },
                ]}
                onChange={(value) => {
                  setSearchMode(String(value));
                  setPage(1);
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
              className="w-32 shrink-0"
              value={timeRange}
              getPopupContainer={() => container ?? document.body}
              zIndex={SETTINGS_OVERLAY_Z_INDEX}
              optionList={[
                { label: "最近 24 小时", value: "24h" },
                { label: "最近 7 天", value: "7d" },
                { label: "最近 30 天", value: "30d" },
                { label: "自定义时间", value: "custom" },
              ]}
              onChange={(value) => {
                setTimeRange(String(value));
                setPage(1);
              }}
            />

            <Select
              aria-label="按供应商筛选请求日志"
              className="w-36 shrink-0"
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
              className="w-40 shrink-0"
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
              className="w-28 shrink-0"
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
              className="w-36 shrink-0"
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
              variant={showAdvancedFilters || requestKind !== "all" ? "secondary" : "ghost"}
              onClick={() => setShowAdvancedFilters((v) => !v)}
              className="shrink-0 text-xs"
            >
              <IconFilter size={13} aria-hidden="true" />
              高级
              {requestKind !== "all" ? (
                <span className="ml-1 rounded-full bg-blue-100 px-1.5 py-0.2 text-[10px] font-semibold text-blue-700">
                  1
                </span>
              ) : null}
            </Button>

            <Button
              size="sm"
              variant="ghost"
              disabled={!hasFilters}
              onClick={resetFilters}
              className={`shrink-0 ${hasFilters ? "text-blue-600 hover:text-blue-700 font-medium" : ""}`}
            >
              清除筛选
              {activeFilterCount > 0 ? ` (${activeFilterCount})` : ""}
            </Button>
          </div>

          {(showAdvancedFilters || timeRange === "custom" || requestKind !== "all") ? (
            <div className="flex flex-wrap items-center gap-3 border-t border-black/6 pt-2 text-xs">
              <div className="flex items-center gap-2">
                <span className="text-[#6e6e73]">请求类型：</span>
                <Select
                  aria-label="请求类型"
                  className="w-40"
                  getPopupContainer={() => container ?? document.body}
                  zIndex={SETTINGS_OVERLAY_Z_INDEX}
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
                    setPage(1);
                  }}
                />
              </div>

              {timeRange === "custom" ? (
                <div className="flex flex-wrap items-center gap-2">
                  <label className="flex items-center gap-1.5 text-xs text-[#6e6e73]">
                    <span>开始时间</span>
                    <Input
                      type="datetime-local"
                      aria-label="开始时间"
                      className="w-44"
                      value={customFrom}
                      onChange={(event) => {
                        setCustomFrom(event.currentTarget.value);
                        setPage(1);
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
                        setPage(1);
                      }}
                    />
                  </label>
                </div>
              ) : null}
            </div>
          ) : null}
        </div>

        <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-black/8 bg-white shadow-sm">
          {loading && result ? (
            <div className="absolute top-0 left-0 right-0 z-10 h-0.5 overflow-hidden bg-blue-100">
              <div className="h-full w-full bg-blue-600 animate-pulse" />
            </div>
          ) : null}
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
            <div className={`min-h-0 flex-1 overflow-auto ${loading && result ? "opacity-75 transition-opacity" : ""}`} aria-busy={loading}>
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
                      <Table.Tr
                        key={`${item.timestampUnixMs}:${item.requestId}`}
                        className="cursor-pointer transition-colors hover:bg-blue-50/40"
                        onClick={(event) => {
                          const target = event.target as HTMLElement | null;
                          if (target?.closest("button") || target?.closest("[data-prevent-row-click]")) return;
                          setSelectedItem(item);
                        }}
                      >
                        <Table.Td>
                          <div className="grid min-w-36 max-w-44 gap-0.5 font-mono">
                            <span className="whitespace-nowrap text-[11px] text-[#1d1d1f]">
                              {formatTimestamp(item.timestampUnixMs)}
                            </span>
                            <div
                              className="group flex cursor-pointer items-center gap-1 text-[10px] text-[#8e8e93] transition-colors hover:text-[#1d1d1f]"
                              data-prevent-row-click="true"
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
            <div className="flex flex-none items-center justify-between gap-3 border-t border-black/8 bg-[#fafafa] px-3.5 py-2 text-xs max-[760px]:flex-col max-[760px]:items-stretch">
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
              <div className="flex items-center justify-end gap-2 max-[520px]:flex-wrap">
                <Select
                  aria-label="请求日志每页条数"
                  className="w-28"
                  getPopupContainer={() => container ?? document.body}
                  optionList={pageSizeOptions}
                  value={pageSize}
                  zIndex={SETTINGS_OVERLAY_Z_INDEX}
                  onChange={(value) => {
                    setPageSize(Number(value) || 20);
                    setPage(1);
                  }}
                />
                <div className="flex items-center gap-1">
                  <Button
                    size="xs"
                    variant="outline"
                    disabled={loading || page <= 1}
                    onClick={() => setPage(1)}
                  >
                    首页
                  </Button>
                  <Button
                    size="xs"
                    variant="outline"
                    disabled={loading || page <= 1}
                    onClick={() => setPage((value) => value - 1)}
                  >
                    上一页
                  </Button>
                  {page > 2 ? (
                    <Button
                      size="xs"
                      variant="ghost"
                      disabled={loading}
                      onClick={() => setPage(1)}
                      className="h-7 min-w-7 px-1.5 font-mono text-xs"
                    >
                      1
                    </Button>
                  ) : null}
                  {page > 3 ? <span className="px-0.5 font-mono text-[#8e8e93]">…</span> : null}
                  {page > 1 ? (
                    <Button
                      size="xs"
                      variant="ghost"
                      disabled={loading}
                      onClick={() => setPage(page - 1)}
                      className="h-7 min-w-7 px-1.5 font-mono text-xs"
                    >
                      {page - 1}
                    </Button>
                  ) : null}
                  <span className="flex h-7 min-w-7 items-center justify-center rounded bg-[#1d1d1f] px-2 font-mono text-xs font-bold text-white shadow-2xs">
                    {page}
                  </span>
                  <Button
                    size="xs"
                    variant="outline"
                    disabled={loading || !result.hasMore || !result.nextCursor}
                    onClick={() => {
                      setCursors((current) => [...current.slice(0, page), result.nextCursor]);
                      setPage((value) => value + 1);
                    }}
                  >
                    下一页
                  </Button>
                </div>
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

      {selectedItem ? (
        <div className="fixed inset-0 z-[1050] flex justify-end">
          <div
            className="fixed inset-0 bg-black/25 backdrop-blur-xs transition-opacity"
            onClick={() => setSelectedItem(null)}
            aria-hidden="true"
          />
          <div
            className="relative z-10 flex h-full w-full max-w-[580px] flex-col bg-white shadow-2xl transition-transform"
            role="dialog"
            aria-modal="true"
            aria-label="请求详情"
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
                      <Badge variant={pres.variant} size="xs">
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
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() => handleCopyId(JSON.stringify(selectedItem, null, 2), "完整日志 JSON")}
                >
                  <IconCopy size={13} aria-hidden="true" />
                  复制 JSON
                </Button>
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
              {/* 耗时分解 */}
              <div className="rounded-xl border border-black/8 bg-[#fafafa] p-3.5">
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
                    <dt className="text-[11px] text-[#8e8e93]">思考强度 / 预算</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      {reasoningLabel(selectedItem)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[#8e8e93]">上游传输方式</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[#1d1d1f]">
                      <Badge variant="secondary" size="xs">
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
                  {selectedItem.codexSessionId ? (
                    <div className="col-span-2">
                      <dt className="text-[11px] text-[#8e8e93]">Codex 会话 ID</dt>
                      <dd className="m-0 mt-0.5 flex items-center gap-1.5 font-mono text-[11px] text-[#48484a]">
                        {selectedItem.codexSessionIsParent ? (
                          <Badge variant="secondary" size="xs">父会话</Badge>
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

              {/* Token 使用量 */}
              <div className="rounded-xl border border-black/8 bg-[#fafafa] p-3.5">
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
                        <span className="text-[11px] font-medium text-red-800">上游错误：</span>
                        <p className="m-0 mt-1 rounded-lg bg-white p-2 text-[11px] leading-relaxed text-red-900 break-words">
                          {selectedItem.upstreamErrorSummary}
                        </p>
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

              {/* 原始 JSON 折叠 */}
              <details className="rounded-xl border border-black/8 bg-[#fafafa] p-3 text-xs">
                <summary className="cursor-pointer font-medium text-[#6e6e73] hover:text-[#1d1d1f]">
                  查看原始记录 JSON
                </summary>
                <pre className="mt-2 max-h-60 overflow-auto rounded-lg bg-black/5 p-2.5 font-mono text-[10px] text-[#1d1d1f]">
                  {JSON.stringify(selectedItem, null, 2)}
                </pre>
              </details>
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
        </div>
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
    </Modal>
  );
}
