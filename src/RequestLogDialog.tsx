import type React from "react";
import { memo, useCallback, useEffect, useId, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { Alert, Chip, CloseButton, Drawer, Pagination, Spinner, Table, Tag, TagGroup } from "@heroui/react";
import { UNSAFE_PortalProvider } from "react-aria";
import {
  IconAlertTriangle,
  IconArchive,
  IconArrowDown,
  IconArrowUp,
  IconBulb,
  IconChartBar,
  IconCheck,
  IconChevronDown,
  IconChevronRight,
  IconChevronUp,
  IconCopy,
  IconCornerDownRight,
  IconDatabaseOff,
  IconLoader2,
  IconRefresh,
  IconSearch,
  IconTrash,
  IconX,
} from "@tabler/icons-react";

import type { Config, OfficialAccount, Profile } from "./App.types";
import requestLogStyles from "./styles.request-log.css?inline";
import { invoke } from "./api";
import { listOfficialAccounts } from "./officialAccountsRequests";
import { loadRequestLogModels, type ModelPage } from "./requestLogModels";
import { errorText } from "./appUtils";
import { formatBytes, formatTimestamp } from "./formatters";
import { modelIdsEqual } from "./modelIds";
import { displayRequestLogModelName, requestLogModelLabels, requestLogRowModelNames, type RequestLogModelLabels } from "./requestLogModelLabels";
import { formatRouteName } from "./routeShortNames";
import type { LogAnalytics } from "./usageAnalysis";
import { maskEmail } from "./sensitiveText";
import { RequestLogRouteDiagnostics, type RouteDiagnostics } from "./RequestLogRouteDiagnostics";
import {
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

const fallbackReasonLabels: Record<string, string> = {
  websocket_not_supported: "当前线路不支持上游 WebSocket，使用 HTTP/SSE",
  websocket_history_requires_http: "当前续接历史需通过 HTTP/SSE 恢复",
  websocket_history_unavailable: "WebSocket 续接历史不可用，尝试通过 HTTP/SSE 恢复",
  websocket_backoff: "WebSocket 正在探测或等待重试，暂用 HTTP/SSE",
  websocket_endpoint_unsupported: "上游端点不支持 WebSocket，改用 HTTP/SSE",
  websocket_connect_failed: "WebSocket 握手失败，改用 HTTP/SSE",
  websocket_to_http_sse: "上游由 WebSocket 转为 HTTP/SSE，旧记录未区分具体原因",
};

type RouteRequestLogItem = RouteDiagnostics & {
  requestId: string;
  traceId: string;
  timestampUnixMs: number;
  provider?: string | null;
  providerName?: string | null;
  officialAccountId?: string | null;
  requestedModel: string;
  requestedServiceTier?: string | null;
  serviceTier?: string | null;
  model?: string | null;
  /// 上游响应里回报的实际使用模型，上游没有回报时为空。
  upstreamResponseModel?: string | null;
  reasoningEffort?: string | null;
  thinkingBudgetTokens?: number | null;
  ttftMs?: number | null;
  routerPreUpstreamMs?: number | null;
  upstreamFirstByteMs?: number | null;
  downstreamFirstContentMs?: number | null;
  upstreamHeaderMs?: number | null;
  totalDurationMs: number;
  outputTokensPerSecond?: number | null;
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
  upstreamResponseHeaders?: string | null;
  upstreamRequestId?: string | null;
  upstreamProtocol?: string | null;
  protocolBridge?: string | null;
  requestInputState?: string | null;
  requestInputItems?: number | null;
  requestHasPreviousResponseId?: boolean | null;
  requestBytes?: number | null;
  upstreamInputState?: string | null;
  upstreamInputItems?: number | null;
  upstreamHasPreviousResponseId?: boolean | null;
  upstreamBytes?: number | null;
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
  profiles: Array<Pick<Profile, "id" | "name" | "shortName" | "sourceProviderId" | "officialAccount" | "officialAccountId">>;
  selectedModelsByProvider: Config["selectedModelsByProvider"];
  declaredOfficialModelsByProvider: Config["declaredOfficialModelsByProvider"];
  upstreamModelsByProvider: Config["upstreamModelsByProvider"];
  modelAliasHistory?: Config["modelAliasHistory"];
};

type RequestLogDialogProps = {
  catalog: RequestLogCatalog;
  container: HTMLElement | null;
  opened: boolean;
  standalone?: boolean;
};

// 分段筛选的状态选项：色点与列表里判定 pill 的语义一致，「全部」不带色点——它表示没有判定。
const statusSegmentOptions = [
  { label: "全部", value: "all", tone: "" },
  { label: "成功", value: "succeeded", tone: "is-success" },
  { label: "失败", value: "failed", tone: "is-danger" },
  { label: "未完成", value: "incomplete", tone: "is-warning" },
  { label: "中断", value: "cancelled", tone: "is-muted" },
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

// 列宽模板写在 .request-log-th-{id} 上（table-layout:fixed 取首行宽度）；
// chevron 是无标题的展开指示列。
const REQUEST_LOG_STREAM_COLUMNS = [
  { id: "time", title: "时间" },
  { id: "status", title: "状态" },
  { id: "provider", title: "供应商 / 上游" },
  { id: "protocol", title: "协议" },
  { id: "model", title: "模型" },
  { id: "effort", title: "思考强度" },
  { id: "latency", title: "耗时" },
  { id: "tokens", title: "Token 用量" },
  { id: "cache", title: "缓存" },
  { id: "chevron", title: "" },
] as const;

const groupByLabels: Record<string, string> = {
  model: "请求模型",
  provider: "供应商",
  official_account: "官方账号",
  status: "状态",
  protocol: "上游协议",
  request_kind: "请求类型",
  session: "会话",
};

const statusPresentation: Record<
  string,
  { label: string; color: "success" | "danger" | "warning" | "default" }
> = {
  succeeded: { label: "成功", color: "success" },
  failed: { label: "失败", color: "danger" },
  incomplete: { label: "未完成", color: "warning" },
  cancelled: { label: "已中断", color: "default" },
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
  if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 2 : 1)} s`;
  // 超过一分钟换成 Xm Ys 免脑内换算；超过一小时进位为 Xh Ym。
  const totalSec = Math.round(value / 1_000);
  if (totalSec < 3_600) return `${Math.floor(totalSec / 60)}m ${totalSec % 60}s`;
  return `${Math.floor(totalSec / 3_600)}h ${Math.floor((totalSec % 3_600) / 60)}m`;
}

function formatTokens(value?: number | null) {
  return value == null ? "—" : value.toLocaleString();
}

// 缓存命中率色阶取 0–100 的数值，供 pill 的 OKLCH 混色用；无法计算时为空。
function cacheRateValue(input?: number | null, cached?: number | null) {
  if (input == null || cached == null || !Number.isFinite(input) || !Number.isFinite(cached) || input <= 0 || cached < 0 || cached > input) return null;
  return Math.min(100, Math.max(0, (cached / input) * 100));
}

// 上游回报的输入总量包含缓存读写：把缓存部分拆出去后才是实际付费的新鲜输入。
function freshInputTokens(input?: number | null, cached?: number | null, cacheWrite?: number | null) {
  if (input == null || !Number.isFinite(input)) return null;
  return Math.max(0, input - (cached ?? 0) - (cacheWrite ?? 0));
}

// 单元格悬浮明细：标签-读数两列，总量行与明细之间留一条分隔线。
function LogCellDetail({
  title,
  rows,
  total,
}: {
  title: string;
  rows: Array<[label: string, value: string]>;
  total?: [label: string, value: string];
}) {
  return (
    <span className="request-log-cell-detail">
      <span className="request-log-cell-detail-title">{title}</span>
      {rows.map(([label, value]) => (
        <span key={label} className="request-log-cell-detail-row">
          <span>{label}</span>
          <span className="request-log-cell-detail-value">{value}</span>
        </span>
      ))}
      {total ? (
        <span className="request-log-cell-detail-row request-log-cell-detail-total">
          <span>{total[0]}</span>
          <span className="request-log-cell-detail-value">{total[1]}</span>
        </span>
      ) : null}
    </span>
  );
}

// 延迟按固定阈值分四档着色：首字 10s/30s/60s，总耗时 60s/180s/300s。
function latencySeverity(ms: number | null | undefined, thresholds: readonly [number, number, number]) {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "none";
  if (ms >= thresholds[2]) return "critical";
  if (ms >= thresholds[1]) return "slow";
  if (ms >= thresholds[0]) return "warn";
  return "good";
}

// 与读数分档同一份语义色，供耗时格的渐变健康竖条取色。
const latencySeverityColor: Record<string, string> = {
  none: "var(--codey-subtle, #8e8e93)",
  good: "var(--codey-green, #047857)",
  warn: "var(--codey-amber, #b45309)",
  slow: "color-mix(in oklch, var(--codey-amber, #b45309) 55%, var(--codey-red, #b91c1c))",
  critical: "var(--codey-red, #b91c1c)",
};

// 上游常回报带日期或快照后缀的同型变体（如 -20250929），与真正换了型号区分开。
function normalizeModelVariant(model: string) {
  return model.trim().toLowerCase()
    .replace(/-latest$/, "")
    .replace(/-\d{4}-\d{2}-\d{2}$/, "")
    .replace(/-\d{8}$/, "");
}

const requestLogDayFormatter = new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit" });
const requestLogTimeFormatter = new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });

// 列表里的时间：本年的不显示年份，显示月日及时间；跨年的补齐年份，完整时间留在悬浮提示。
function formatRequestLogTime(timestampUnixMs: number) {
  const date = new Date(timestampUnixMs);
  if (!Number.isFinite(date.getTime())) return "—";
  const now = new Date();
  const sameYear = date.getFullYear() === now.getFullYear();
  return sameYear
    ? `${requestLogDayFormatter.format(date)} ${requestLogTimeFormatter.format(date)}`
    : formatTimestamp(timestampUnixMs);
}

/** `<time dateTime>` 只接受合法 ISO 时间；时间戳缺失或超出范围时省略该属性，
 * 而不是抛出 RangeError 打断整行渲染。 */
function isoTimestampOrUndefined(timestampUnixMs: number) {
  const date = new Date(timestampUnixMs);
  return Number.isFinite(date.getTime()) ? date.toISOString() : undefined;
}

// 记录侧对短回复（<100 Token）和过短生成窗口不落速度值，避免失真；
// 行内仍按「首包后耗时」给出估算读数（≈ 前缀与正式值区分），真正缺数据才显示 —。
function outputSpeedEstimate(item: RouteRequestLogItem) {
  const output = item.outputTokens;
  const pre = item.routerPreUpstreamMs;
  const firstByte = item.upstreamFirstByteMs;
  if (output == null || !Number.isFinite(output) || output <= 0) return null;
  if (pre == null || firstByte == null || !Number.isFinite(pre) || !Number.isFinite(firstByte)) return null;
  const generationMs = item.totalDurationMs - pre - firstByte;
  if (!Number.isFinite(generationMs) || generationMs <= 0) return null;
  return output * 1_000 / generationMs;
}

// 估算也算不出时，「—」读数用悬浮说明缺数缘由。
function outputSpeedUnavailableReason(item: RouteRequestLogItem): string {
  const output = item.outputTokens;
  if (output == null || !Number.isFinite(output)) return "上游未回报输出 Token，无法计算速度";
  const pre = item.routerPreUpstreamMs;
  const firstByte = item.upstreamFirstByteMs;
  if (pre == null || firstByte == null || !Number.isFinite(pre) || !Number.isFinite(firstByte)) {
    return "缺少上游计时，无法圈定生成窗口";
  }
  return "首包后的生成窗口异常，无法计算速度";
}

/** 输出速度的数值部分；未知、非有限或非正样本一律视为没有可比读数。 */
function formatTokensPerSecondValue(value?: number | null) {
  if (value == null || !Number.isFinite(value) || value <= 0) return null;
  return value.toFixed(1);
}

function formatTokensPerSecond(value?: number | null) {
  const text = formatTokensPerSecondValue(value);
  return text ? `${text} tok/s` : "—";
}

// 日志只保留请求体的形态摘要，这里把形态和项数还原成可读文字。
function requestShapeText(state?: string | null, items?: number | null, hasPreviousResponseId?: boolean | null) {
  if (!state) return null;
  if (state === "array" && (items ?? 0) === 0) return "空数组（0 项）";
  const parts: string[] = [];
  switch (state) {
    case "absent":
      parts.push("无 input 字段");
      break;
    case "null":
      parts.push("input 为 null");
      break;
    case "array":
      parts.push(`数组 ${(items ?? 0).toLocaleString()} 项`);
      break;
    case "string":
      parts.push("字符串");
      break;
    case "object":
      parts.push("对象");
      break;
    case "other":
      parts.push("其他类型");
      break;
    default:
      parts.push(state);
      break;
  }
  if (hasPreviousResponseId) parts.push("带 previous_response_id");
  return parts.join(" · ");
}

// input 为空数组时会触发上游的 input items 校验错误，单独标红提示。
function isEmptyInputArray(state?: string | null, items?: number | null) {
  return state === "array" && (items ?? 0) === 0;
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

// 思考强度按档位着色：低档位克制、高档位醒目，与协议徽章的用色体系一致。
function reasoningTagClass(label: string) {
  const normalized = label.trim().toLowerCase();
  if (normalized === "low" || normalized === "minimal" || normalized === "none") {
    return "request-log-effort-low";
  }
  if (normalized === "high") {
    return "request-log-effort-high";
  }
  if (normalized === "max" || normalized === "xhigh") {
    return "request-log-effort-max";
  }
  return "request-log-effort-medium";
}

function unavailableMessage(reason?: string) {
  if (reason === "ndjson_not_queryable") {
    return "当前请求日志使用 NDJSON 存储，无法在线分页查询。开启页面上的日志记录开关后会切换为 SQLite。";
  }
  return "当前请求日志存储暂不可查询，请稍后重试。";
}

let copiedIdSnapshot: string | null = null;
let copiedIdResetTimer: number | null = null;
const copiedIdListeners = new Set<() => void>();

function subscribeCopiedId(listener: () => void) {
  copiedIdListeners.add(listener);
  return () => {
    copiedIdListeners.delete(listener);
  };
}

function getCopiedIdSnapshot() {
  return copiedIdSnapshot;
}

function setCopiedId(id: string | null) {
  if (Object.is(copiedIdSnapshot, id)) return;
  if (copiedIdResetTimer != null) {
    window.clearTimeout(copiedIdResetTimer);
    copiedIdResetTimer = null;
  }
  copiedIdSnapshot = id;
  copiedIdListeners.forEach((listener) => listener());
  // 标记按按钮实例区分，2.2s 后自动复位，与复制提示的停留时间一致。
  if (id != null) {
    copiedIdResetTimer = window.setTimeout(() => setCopiedId(null), 2200);
  }
}

type CopyIdKind = "request" | "session";
type CopyIdOptions = ({ kind: CopyIdKind; label?: string } | { kind?: never; label: string }) & {
  subtext?: string;
  copiedKey?: string;
};

function CopyIdButton({
  id,
  kind,
  label,
  "aria-label": ariaLabel,
  className,
  iconClassName,
  title,
  onCopy,
}: {
  id: string;
  kind: CopyIdKind;
  label: string;
  "aria-label"?: string;
  className: string;
  iconClassName: string;
  title: string;
  onCopy: (id: string, options: CopyIdOptions) => void;
}) {
  // 已复制标记按按钮实例区分：相同 ID 出现在多行时，只有被点击的那个显示「已复制」。
  const copiedKey = useId();
  const copiedId = useSyncExternalStore(subscribeCopiedId, getCopiedIdSnapshot, getCopiedIdSnapshot);
  const copied = copiedId === copiedKey;
  return (
    <Button
      variant="ghost"
      size="xs"
      aria-label={ariaLabel ?? label}
      className={className}
      title={title}
      onClick={(event) => {
        // 复制是留在列表里的操作：不能让它冒泡成行激活、把详情抽屉盖在上面。
        event.stopPropagation();
        onCopy(id, { kind, copiedKey });
      }}
    >
      <span className="truncate select-all">{copied ? "已复制" : id}</span>
      {copied ? (
        <IconCheck size={11} className="shrink-0 text-emerald-600 dark:text-emerald-400" aria-hidden="true" />
      ) : (
        <IconCopy size={11} className={iconClassName} aria-hidden="true" />
      )}
    </Button>
  );
}

function requestLogProviderLabel(item: RouteRequestLogItem, profiles: RequestLogCatalog["profiles"]) {
  const profile = profiles.find((profile) =>
    Boolean(item.provider) && (profile.sourceProviderId === item.provider || profile.id === item.provider)
    && (!item.officialAccountId || profile.officialAccountId === item.officialAccountId),
  ) ?? (item.officialAccountId
    ? profiles.find((profile) => profile.officialAccountId === item.officialAccountId)
    : undefined);
  if (profile) {
    const label = formatRouteName(profile.name, profile.shortName || "");
    if (label) return label;
  }
  return item.providerName || item.provider || "—";
}

// 非模型调用的旁路请求（压缩、图像、被拒请求等）在模型名旁给一个种类标记。
const requestKindLabels: Record<string, string> = {
  responses_compact: "压缩",
  responses_compact_v2: "压缩 v2",
  images_generations: "图像",
  models: "模型列表",
  http_rejected: "拒绝",
};

const statusPillTone: Record<string, string> = {
  succeeded: "is-success",
  failed: "is-danger",
  incomplete: "is-warning",
  cancelled: "is-muted",
};

const RequestLogRow = memo(function RequestLogRow({
  item,
  providerLabel,
  officialAccountLabel,
  modelLabels,
  selected,
  onOpen,
  onCopyId,
}: {
  item: RouteRequestLogItem;
  providerLabel: string;
  officialAccountLabel: (accountId?: string | null) => string;
  modelLabels: RequestLogModelLabels;
  selected: boolean;
  onOpen: (item: RouteRequestLogItem) => void;
  onCopyId: (requestId: string, options: CopyIdOptions) => void;
}) {
  const presentation = statusPresentation[item.status] ?? { label: item.status || "未知", color: "default" as const };
  const hasUpstreamError = [item.statusCode, item.upstreamStatusCode].some((statusCode) => statusCode != null && (statusCode < 200 || statusCode >= 300));
  const upstreamErrorSummary = item.upstreamErrorSummary || item.errorCode || "上游未提供具体错误信息";
  const upstreamErrorPreview = upstreamErrorSummary.length > 512
    ? `${upstreamErrorSummary.slice(0, 512)}…（完整内容见详情）`
    : upstreamErrorSummary;
  const usageUnavailable = usageUnavailablePresentation(item.usageUnavailableReason);
  const cacheRate = cacheRateValue(item.inputTokens, item.cachedInputTokens);
  const cancellation = cancellationPresentation(item);
  const reasoning = reasoningLabel(item);
  const displayedTtft = item.downstreamFirstContentMs ?? item.ttftMs;
  // 模型链分三级：客户端的请求模型是主读数，网关映射后的发送模型与上游
  // 响应回报的模型在与上一级不同时各挂一行路由标记；仅请求选择器恢复上游名称。
  const requestedModelText = item.requestedModel.trim();
  const sentModelRaw = (item.model ?? "").trim();
  const returnedModelRaw = (item.upstreamResponseModel ?? "").trim();
  const { primaryModel, sentDisplay, returnedDisplay } = requestLogRowModelNames(item, modelLabels);
  const mappedModel = sentDisplay && primaryModel && !modelIdsEqual(sentDisplay, primaryModel)
    ? sentDisplay
    : "";
  const returnedShown = returnedDisplay
    && !modelIdsEqual(returnedDisplay, mappedModel || sentDisplay || primaryModel)
    ? returnedDisplay
    : "";
  // 回报模型与发送模型只差快照/日期后缀时按变体弱化显示，警示色留给真正换型的不一致。
  const returnedIsVariant = Boolean(returnedShown)
    && normalizeModelVariant(returnedDisplay) === normalizeModelVariant(mappedModel || sentDisplay || primaryModel);
  const statusDetail = hasUpstreamError ? upstreamErrorPreview : cancellation?.message ?? null;
  const kindLabel = item.requestKind === "responses" ? null : requestKindLabels[item.requestKind] ?? item.requestKind;
  // 与统计区共用同一份数值格式化，避免两处对未知值给出不同结论。
  const tpsText = formatTokensPerSecondValue(item.outputTokensPerSecond);
  const tpsEstimateText = tpsText ? null : formatTokensPerSecondValue(outputSpeedEstimate(item));
  const speedTitle = tpsText
    ? undefined
    : tpsEstimateText
      ? "按首包后耗时估算：输出 <100 Token 或窗口过短的请求不落统计值"
      : outputSpeedUnavailableReason(item);

  // 只有挂提示的 pill 需要可聚焦：键盘与触屏用户才拿得到错误/中断详情。
  const renderStatusPill = (focusable: boolean) => (
    <Chip
      size="sm"
      className={`request-log-result ${statusPillTone[item.status] ?? "is-muted"}`}
      tabIndex={focusable ? 0 : undefined}
    >
      <i className="request-log-result-dot" aria-hidden="true" />
      {presentation.label}
    </Chip>
  );

  // 行激活走 Table.Row 的 onAction：复制按钮等行内控件的交互不会触发行打开。
  return (
    <Table.Row
      id={`${item.timestampUnixMs}:${item.requestId}`}
      className={`request-log-row${selected ? " is-selected" : ""}`}
      onAction={() => onOpen(item)}
    >
      {/* 时间：主行是扫读用的短时间（子代理请求紧跟子标签），请求与会话 ID 收进等宽子行 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-time">
        <span className="request-log-time-row">
          <time
            className="request-log-time"
            dateTime={isoTimestampOrUndefined(item.timestampUnixMs)}
            title={formatTimestamp(item.timestampUnixMs)}
          >
            {formatRequestLogTime(item.timestampUnixMs)}
          </time>
          {item.subagent ? (
            <Chip size="sm" className="request-log-subagent-tag" title="子代理请求">
              子
            </Chip>
          ) : null}
        </span>
        <span className="request-log-id-line">
          <span className="request-log-id-label">请求</span>
          <CopyIdButton
            id={item.requestId}
            kind="request"
            label={`复制请求 ID：${item.requestId}`}
            aria-label={`复制请求 ID：${item.requestId}`}
            className="request-log-id"
            iconClassName="request-log-id-icon"
            title={`请求 ID: ${item.requestId}（点击复制）`}
            onCopy={onCopyId}
          />
        </span>
        <span className="request-log-id-line">
          <span className="request-log-id-label">会话</span>
          {item.codexSessionId ? (
            <CopyIdButton
              id={item.codexSessionId}
              kind="session"
              label={`复制${item.codexSessionIsParent ? "父会话" : "会话"} ID：${item.codexSessionId}`}
              className="request-log-id"
              iconClassName="request-log-id-icon"
              title={`${item.codexSessionIsParent ? "父会话" : "会话"} ID: ${item.codexSessionId}（点击复制）`}
              onCopy={onCopyId}
            />
          ) : (
            <span className="request-log-none">—</span>
          )}
        </span>
      </div>
      </Table.Cell>

      {/* 状态：带色点的判定 pill，出错或中断时 pill 自身就是详情提示的触发器 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-status">
        {statusDetail ? (
          <Tooltip
            content={(
              <span className="block max-w-[420px] break-words whitespace-normal">
                {statusDetail}
              </span>
            )}
            position="top"
          >
            {renderStatusPill(true)}
          </Tooltip>
        ) : renderStatusPill(false)}
        {item.statusCode != null || item.errorCode || cancellation ? (
          <span className="request-log-status-sub">
            {item.statusCode != null
              ? `HTTP ${item.statusCode}`
              : cancellation?.label || item.errorCode}
            {cancellation && item.statusCode != null
              ? ` · ${cancellation.label}`
              : null}
          </span>
        ) : null}
      </div>
      </Table.Cell>

      {/* 供应商：官方线路带「官」标，副行是账号或上游域名 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-provider">
        <span className="request-log-provider-main">
          {item.officialAccountId ? (
            <Chip size="sm" className="request-log-official-tag" title="官方账号">官</Chip>
          ) : null}
          <strong className="request-log-provider-name" title={providerLabel}>
            {providerLabel}
          </strong>
        </span>
        {item.officialAccountId ? (
          <span
            className="request-log-provider-sub"
            title={`官方账号：${officialAccountLabel(item.officialAccountId)}`}
          >
            {officialAccountLabel(item.officialAccountId)}
          </span>
        ) : item.upstreamAuthority ? (
          <span
            className="request-log-provider-sub font-mono"
            title={`上游: ${item.upstreamAuthority}`}
          >
            {item.upstreamAuthority}
          </span>
        ) : null}
      </div>
      </Table.Cell>

      {/* 上游协议 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-protocol">
        <Chip
          size="sm"
          className={`request-log-protocol ${protocolTagClass(item.upstreamTransport)}`}
        >
          {item.upstreamTransport === "http_sse" ? "SSE" : (item.upstreamTransport || "—").toUpperCase()}
        </Chip>
      </div>
      </Table.Cell>

      {/* 模型：主行是请求模型与标记，映射/回报不一致时各挂一条路由副行 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-model">
        <span className="request-log-model-main">
          <strong
            className="request-log-model-name"
            title={requestedModelText || sentModelRaw ? `请求模型：${requestedModelText || sentModelRaw}` : undefined}
          >
            {primaryModel || "—"}
          </strong>
        </span>
        {kindLabel ? (
          <span className="request-log-model-tags">
            <Chip size="sm" className="request-log-kind-tag">{kindLabel}</Chip>
          </span>
        ) : null}
        {mappedModel ? (
          <span
            className="request-log-model-route is-mapped"
            title={`网关映射后发送给上游的模型：${sentModelRaw}`}
          >
            <IconCornerDownRight size={11} className="shrink-0" aria-hidden="true" />
            <span className="truncate">{mappedModel}</span>
          </span>
        ) : null}
        {returnedShown ? (
          <span
            className={`request-log-model-route ${returnedIsVariant ? "is-variant" : "is-returned"}`}
            title={returnedIsVariant
              ? `上游回报的同型快照变体：${returnedModelRaw}`
              : `上游响应回报的模型与发送不一致：${returnedModelRaw}`}
          >
            <IconCornerDownRight size={11} className="shrink-0" aria-hidden="true" />
            <span className="truncate">{returnedShown}</span>
          </span>
        ) : null}
      </div>
      </Table.Cell>

      {/* 思考强度：独立成列，档位胶囊按强度分色；无记录时留安静的占位 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-effort">
        {reasoning !== "—" ? (
          <Chip
            size="sm"
            className={`request-log-meta-tag ${reasoningTagClass(reasoning)}`}
            title={`思考强度：${reasoning}`}
          >
            <IconBulb size={9} aria-hidden="true" />
            {reasoning}
          </Chip>
        ) : (
          <span className="request-log-none">—</span>
        )}
      </div>
      </Table.Cell>

      {/* 耗时：左侧竖条上半随首字档、下半随总耗时档，纵向扫读即见健康度；
          读数三行收首字/总耗时/输出速度；悬浮给出各阶段明细 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-latency">
        <Tooltip
          content={(
            <LogCellDetail
              title="延迟明细"
              rows={([
                ["排队等待", item.queueDelayMs],
                ["路由前置", item.routerPreUpstreamMs],
                ["上游首包", item.upstreamFirstByteMs],
                ["上游响应头", item.upstreamHeaderMs],
                [item.downstreamFirstContentMs != null ? "端到端首字" : "首字", displayedTtft],
              ] as Array<[string, number | null | undefined]>)
                .filter(([, ms]) => ms != null && Number.isFinite(ms))
                .map(([label, ms]) => [label, formatDuration(ms)] as [string, string])}
              total={["总耗时", formatDuration(item.totalDurationMs)]}
            />
          )}
        >
          <span className="request-log-latency-wrap" tabIndex={0}>
            <i
              className="request-log-latency-bar"
              style={{
                "--bar-top": latencySeverityColor[latencySeverity(
                  displayedTtft ?? item.totalDurationMs, [10_000, 30_000, 60_000],
                )],
                "--bar-bottom": latencySeverityColor[latencySeverity(item.totalDurationMs, [60_000, 180_000, 300_000])],
              } as React.CSSProperties}
              aria-hidden="true"
            />
            <span className="request-log-latency-grid">
              <span className="request-log-latency-label">首字</span>
              <span className={`request-log-latency-value request-log-sev-${latencySeverity(displayedTtft, [10_000, 30_000, 60_000])}`}>
                {formatDuration(displayedTtft)}
              </span>
              <span className="request-log-latency-label">总耗时</span>
              <span className={`request-log-latency-value request-log-sev-${latencySeverity(item.totalDurationMs, [60_000, 180_000, 300_000])}`}>
                {formatDuration(item.totalDurationMs)}
              </span>
              <span
                className="request-log-latency-label"
                title="全部输出 Token（含思考）除以上游首包后的响应时间；计时不完整或输出样本不足时未知"
              >
                速度
              </span>
              <span
                className="request-log-latency-value"
                title={speedTitle}
              >
                {tpsText ? `${tpsText} tok/s` : tpsEstimateText ? `≈${tpsEstimateText} tok/s` : "—"}
              </span>
            </span>
          </span>
        </Tooltip>
      </div>
      </Table.Cell>

      {/* Token：新鲜输入与输出是第一读数，缓存读写与思考量收进副行 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-tokens">
        {item.totalTokens == null ? (
          <Tooltip
            content={(
              <span className="block max-w-[360px] whitespace-normal">
                {usageUnavailable.message}
              </span>
            )}
            position="top"
          >
            <span className="request-log-usage-none" tabIndex={0}>
              {usageUnavailable.label}
            </span>
          </Tooltip>
        ) : (
          <Tooltip
            className="request-log-token-tip"
            content={(
              <LogCellDetail
                title="Token 明细"
                rows={([
                  ["新鲜输入", freshInputTokens(item.inputTokens, item.cachedInputTokens, item.cacheCreationInputTokens)],
                  ["缓存读取", item.cachedInputTokens],
                  ["缓存写入", item.cacheCreationInputTokens],
                  ["输出", item.outputTokens],
                  ["其中思考输出", item.reasoningOutputTokens],
                ] as Array<[string, number | null | undefined]>)
                  .filter(([, tokens]) => tokens != null && Number.isFinite(tokens))
                  .map(([label, tokens]) => [label, formatTokens(tokens)] as [string, string])}
                total={["总 Token", formatTokens(item.totalTokens)]}
              />
            )}
          >
            <span className="request-log-token-grid" tabIndex={0}>
            <span
              className="request-log-token-in"
              title={`新鲜输入 Token（上报输入减去缓存读写）：${formatTokens(freshInputTokens(item.inputTokens, item.cachedInputTokens, item.cacheCreationInputTokens))}`}
            >
              <IconArrowDown size={10} aria-hidden="true" />
              {formatTokens(freshInputTokens(item.inputTokens, item.cachedInputTokens, item.cacheCreationInputTokens))}
            </span>
            <span className="request-log-token-out" title={`输出 Token：${formatTokens(item.outputTokens)}`}>
              <IconArrowUp size={10} aria-hidden="true" />
              {formatTokens(item.outputTokens)}
            </span>
            {(item.cacheCreationInputTokens != null && item.cacheCreationInputTokens > 0) || (item.reasoningOutputTokens != null && item.reasoningOutputTokens > 0) ? (
              <span className="request-log-token-extra">
                {item.cacheCreationInputTokens != null && item.cacheCreationInputTokens > 0 ? (
                  <span title={`缓存写入 Token：${formatTokens(item.cacheCreationInputTokens)}`}>
                    +{formatTokens(item.cacheCreationInputTokens)}
                  </span>
                ) : null}
                {item.reasoningOutputTokens != null && item.reasoningOutputTokens > 0 ? (
                  <span
                    className="request-log-tokens-reasoning"
                    title={`思考输出 Token：${formatTokens(item.reasoningOutputTokens)}`}
                  >
                    <IconBulb size={10} aria-hidden="true" />
                    {formatTokens(item.reasoningOutputTokens)}
                  </span>
                ) : null}
              </span>
            ) : null}
            </span>
          </Tooltip>
        )}
      </div>
      </Table.Cell>

      {/* 缓存：读取缓存与缓存率一列上下展示 */}
      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-cache">
        {item.totalTokens == null && item.cachedInputTokens == null ? (
          <span className="request-log-usage-none">—</span>
        ) : (
          <Tooltip
            content={`缓存命中率 ${cacheRate != null ? `${cacheRate.toFixed(1)}%` : "—"} · 缓存 ${formatTokens(item.cachedInputTokens)} / 输入 ${formatTokens(item.inputTokens)} Token`}
            position="top"
          >
            <span className="request-log-cache-grid" tabIndex={0}>
              <span
                className="request-log-cache-read"
                title={`缓存读取 Token：${formatTokens(item.cachedInputTokens)}`}
              >
                <IconArchive size={9} aria-hidden="true" />
                {formatTokens(item.cachedInputTokens)}
              </span>
              {cacheRate != null ? (
                <Chip
                  size="sm"
                  className="request-log-cache-pill"
                  style={{ "--cache-rate": `${cacheRate.toFixed(2)}%` } as React.CSSProperties}
                >
                  <i className="request-log-cache-dot" aria-hidden="true" />
                  {cacheRate.toFixed(1)}%
                </Chip>
              ) : (
                <Chip size="sm" className="request-log-cache-pill is-empty" title="未记录缓存 Token">—</Chip>
              )}
            </span>
          </Tooltip>
        )}
      </div>
      </Table.Cell>

      <Table.Cell className="request-log-td">
      <div className="request-log-cell request-log-cell-chevron">
        <IconChevronRight size={14} className="request-log-chevron" aria-hidden="true" />
      </div>
      </Table.Cell>
    </Table.Row>
  );
});

export function RequestLogDialog({
  catalog,
  container,
  opened,
  standalone = false,
}: RequestLogDialogProps) {
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [provider, setProvider] = useState("all");
  const [officialAccount, setOfficialAccount] = useState("all");
  const [officialAccounts, setOfficialAccounts] = useState<OfficialAccount[]>([]);
  const [model, setModel] = useState("all");
  const [status, setStatus] = useState("all");
  const [protocol, setProtocol] = useState("all");
  const [page, setPage] = useState(1);
  const [pageJumpInput, setPageJumpInput] = useState("1");
  const [pageSize, setPageSize] = useState(20);
  const [cursors, setCursors] = useState<Record<number, LogCursor>>({});
  // 筛选变化时同步重置页码和游标，避免新查询使用旧筛选下的游标。
  const resetPagination = useCallback(() => {
    setPage(1);
    setPageJumpInput("1");
    setCursors({});
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
  const [modelsCatalogNeeded, setModelsCatalogNeeded] = useState(false);
  const copyToastTimer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (copyToastTimer.current) window.clearTimeout(copyToastTimer.current);
      setCopiedId(null);
    },
    [],
  );
  const requestRevision = useRef(0);
  const listTask = useRef(Promise.resolve());
  const statsTask = useRef(Promise.resolve());
  const modelsTask = useRef(Promise.resolve());
  const clearInFlight = useRef(false);

  const handleCopyId = useCallback((requestId: string, options: CopyIdOptions) => {
    if (!navigator.clipboard) return;
    void navigator.clipboard.writeText(requestId).then(
      () => {
        setCopiedId(options?.copiedKey ?? requestId);
        if (copyToastTimer.current) {
          window.clearTimeout(copyToastTimer.current);
        }
        const defaultLabel = options.kind === "session" ? "会话 ID" : "请求 ID";
        const label = options?.label || defaultLabel;
        setCopyToast({
          text: `已复制${label}`,
          subtext: options?.subtext ?? requestId,
        });
        copyToastTimer.current = window.setTimeout(() => {
          setCopyToast(null);
        }, 2200);
      },
      () => undefined,
    );
  }, []);

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
    ...(optionalFilter(officialAccount) ? { officialAccountId: officialAccount } : {}),
    ...(optionalFilter(model) ? { model } : {}),
    ...(optionalFilter(status) ? { status } : {}),
    ...(optionalFilter(protocol) ? { protocol } : {}),
    ...(optionalFilter(requestKind) ? { requestKind } : {}),
  }), [fromUnixMs, toUnixMs, search, searchMode, provider, officialAccount, model, status, protocol, requestKind]);
  const cursor = page === 1 ? null : cursors[page] ?? null;
  const health = stats?.recordingHealth;
  const dropped = health ? health.droppedFull + health.droppedClosed + health.writeDropped : 0;
  const healthWarning = health && (dropped > 0 || !health.active || health.sampleRatePerMillion < 1_000_000
    || health.writeFailures > 0 || health.observerPanics > 0 || health.writerPanics > 0 || health.shutdownTimeouts > 0);

  const providerOptions = useMemo(() => {
    const providers = new Map<string, string>();
    for (const profile of catalog.profiles) {
      const value = profile.sourceProviderId || profile.id;
      if (value) providers.set(value, formatRouteName(profile.name, profile.shortName || "") || value);
    }
    return [
      { label: "全部供应商", value: "all" },
      ...[...providers].map(([value, label]) => ({ label, value })),
    ];
  }, [catalog.profiles]);

  const modelLabels = useMemo(() => requestLogModelLabels(catalog), [catalog]);

  // 独立托管的日志页只拿到配置本身，启动期能力标志可能缺失；存储账号的官方
  // 线路自带凭据，只要配置里存在官方线路就读取账号列表，与线路列表口径一致。
  const officialRoutesPresent = useMemo(
    () => catalog.officialAccountAvailable === true
      || catalog.profiles.some((profile) => profile.officialAccount || Boolean(profile.officialAccountId)),
    [catalog.officialAccountAvailable, catalog.profiles],
  );

  // 官方账号只用于区分同一供应商下的多条官方线路，标签优先取邮箱。
  useEffect(() => {
    if (!opened || !officialRoutesPresent) return;
    let active = true;
    void listOfficialAccounts().then(
      (result) => { if (active) setOfficialAccounts(result.accounts ?? []); },
      () => { if (active) setOfficialAccounts([]); },
    );
    return () => { active = false; };
  }, [opened, officialRoutesPresent]);

  // 请求日志页可能被截图或分享，账号标签只显示脱敏后的邮箱。
  const officialAccountLabels = useMemo(() => new Map(officialAccounts.map((account) => {
    const email = account.email?.trim();
    return [account.id, email ? maskEmail(email) : account.routeName?.trim() || account.id];
  })), [officialAccounts]);

  const officialAccountOptions = useMemo(() => [
    { label: "全部官方账号", value: "all" },
    ...officialAccounts.map((account) => ({
      label: `${officialAccountLabels.get(account.id) ?? account.id}${account.isDefault ? "（默认）" : ""}`,
      value: account.id,
    })),
  ], [officialAccounts, officialAccountLabels]);

  const officialAccountLabel = useCallback((accountId?: string | null) => {
    if (!accountId) return "—";
    return officialAccountLabels.get(accountId) ?? accountId;
  }, [officialAccountLabels]);

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
        .map((value) => ({ label: displayRequestLogModelName(value, modelLabels), value })),
    ];
  }, [usedModels, result?.items, model, modelLabels]);

  useEffect(() => {
    if (!opened || !validRange) return;
    if (!modelsCatalogNeeded && model === "all") return;
    let active = true;
    setUsedModels([]);
    modelsTask.current = modelsTask.current.then(async () => {
      if (!active) return;
      try {
        const models = await loadRequestLogModels((afterModel) => invoke<ModelPage>("query_route_request_log_models", {
          fromUnixMs,
          toUnixMs,
          afterModel,
          ...(optionalFilter(provider) ? { provider } : {}),
          ...(optionalFilter(officialAccount) ? { officialAccountId: officialAccount } : {}),
        }), () => active);
        if (active && models) setUsedModels(models);
      } catch (nextError) {
        if (active) setError(errorText(nextError));
      }
    });
    return () => {
      active = false;
    };
  }, [opened, fromUnixMs, toUnixMs, provider, officialAccount, validRange, refreshRevision, modelsCatalogNeeded, model]);

  // 搜索内容未变时保留分页，避免挂载时清空首页查询刚写入的下一页游标。
  useEffect(() => {
    const nextSearch = searchInput.trim();
    if (nextSearch === search) return;
    const timer = window.setTimeout(() => {
      setSearch(nextSearch);
      resetPagination();
    }, 300);
    return () => window.clearTimeout(timer);
  }, [searchInput, search]);

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
          ...filters, page, pageSize, cursor,
          // 已知游标时继续使用游标分页，其余页直接按页码查询。
          cursorMode: page === 1 || cursor !== null,
        });
        if (active && revision === requestRevision.current) {
          setResult(nextResult);
          setCursors((prev) => {
            const next = { ...prev };
            if (nextResult.nextCursor) next[page + 1] = nextResult.nextCursor;
            else delete next[page + 1];
            return next;
          });
        }
      } catch (nextError) {
        if (active) setError(errorText(nextError));
      } finally {
        if (active) setLoading(false);
      }
    });
    return () => { active = false; };
  }, [opened, filters, page, pageSize, cursor, validRange, refreshRevision]);

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
    setOfficialAccount("all");
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
      officialAccount !== "all" ||
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
    if (officialAccount !== "all") count += 1;
    if (model !== "all") count += 1;
    if (status !== "all") count += 1;
    if (protocol !== "all") count += 1;
    if (requestKind !== "all") count += 1;
    if (timeRange !== "24h") count += 1;
    return count;
  }, [search, provider, officialAccount, model, status, protocol, requestKind, timeRange]);
  // React Aria 集合以 item.id 作为行 React key；RequestLogRow 是包装组件拿不到 id，
  // 这里显式包装一份带稳定 id 的 items，避免筛选/翻页时 index key 复用导致
  // "Cannot change the id of an item" 崩溃。
  const streamItems = useMemo(
    () => (result?.items ?? []).map((item) => ({ id: `${item.timestampUnixMs}:${item.requestId}`, item })),
    [result?.items],
  );
  const firstVisible = result?.items.length ? (page - 1) * pageSize + 1 : 0;
  const lastVisible = result?.items.length ? firstVisible + result.items.length - 1 : 0;
  const totalCount = stats?.total ?? result?.total ?? 0;
  const totalPages = Math.max(1, Math.ceil(totalCount / pageSize));
  const jumpPage = Number(pageJumpInput);
  const validJumpPage = /^\d+$/.test(pageJumpInput) && Number.isSafeInteger(jumpPage)
    && jumpPage >= 1 && jumpPage <= totalPages;
  const goToPage = (nextPage: number) => {
    if (loading || !Number.isSafeInteger(nextPage) || nextPage < 1 || nextPage > totalPages) return;
    setPageJumpInput(String(nextPage));
    setPage(nextPage);
  };

  const searchModeLabel = searchMode === "requestId" ? "请求 ID" : searchMode === "sessionId" ? "会话 ID" : "关键词";
  const filterChips: Array<{ key: string; label: string; onRemove: () => void }> = [];
  if (search) {
    filterChips.push({
      key: "search",
      label: `${searchModeLabel}：${search}`,
      onRemove: () => {
        setSearchInput("");
        setSearch("");
        resetPagination();
      },
    });
  }
  if (timeRange !== "24h") {
    const timeLabel = timeRange === "custom"
      ? "自定义"
      : timeRange === "7d" ? "最近 7 天" : "最近 30 天";
    filterChips.push({
      key: "time",
      label: `时间：${timeLabel}`,
      onRemove: () => {
        setTimeRange("24h");
        resetPagination();
      },
    });
  }
  if (provider !== "all") {
    filterChips.push({
      key: "provider",
      label: `供应商：${providerOptions.find((option) => option.value === provider)?.label ?? provider}`,
      onRemove: () => {
        setProvider("all");
        resetPagination();
      },
    });
  }
  if (officialAccount !== "all") {
    filterChips.push({
      key: "officialAccount",
      label: `官方账号：${officialAccountLabel(officialAccount)}`,
      onRemove: () => {
        setOfficialAccount("all");
        resetPagination();
      },
    });
  }
  if (model !== "all") {
    filterChips.push({
      key: "model",
      label: `模型：${displayRequestLogModelName(model, modelLabels)}`,
      onRemove: () => {
        setModel("all");
        resetPagination();
      },
    });
  }
  if (status !== "all") {
    filterChips.push({
      key: "status",
      label: `状态：${statusPresentation[status]?.label ?? status}`,
      onRemove: () => {
        setStatus("all");
        resetPagination();
      },
    });
  }
  if (protocol !== "all") {
    filterChips.push({
      key: "protocol",
      label: `协议：${protocolOptions.find((option) => option.value === protocol)?.label ?? protocol}`,
      onRemove: () => {
        setProtocol("all");
        resetPagination();
      },
    });
  }
  if (requestKind !== "all") {
    filterChips.push({
      key: "requestKind",
      label: `类型：${requestKindLabels[requestKind] ?? requestKind}`,
      onRemove: () => {
        setRequestKind("all");
        resetPagination();
      },
    });
  }

  if (!opened) return null;

  return (
    <div className="request-log-workspace relative flex h-full min-h-0 flex-1 flex-col">
      <style>{requestLogStyles}</style>
      <h1 className="sr-only">请求日志</h1>

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
                leftSection={<IconSearch size={15} className="text-[var(--codey-subtle,#8e8e93)]" aria-hidden="true" />}
                rightSection={
                  searchInput ? (
                    <button
                      type="button"
                      className="flex h-5 w-5 cursor-pointer items-center justify-center rounded-full text-[var(--codey-subtle,#8e8e93)] hover:bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 hover:text-[var(--codey-text,#1d1d1f)]"
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

            <div
              className="request-log-segmented"
              role="radiogroup"
              aria-label="按状态筛选请求日志"
              onKeyDown={(event) => {
                // 单选组的键盘契约：方向键在选项间移动并同时选中，Tab 只进入一次。
                const step = event.key === "ArrowRight" || event.key === "ArrowDown"
                  ? 1
                  : event.key === "ArrowLeft" || event.key === "ArrowUp"
                    ? -1
                    : 0;
                if (step === 0) return;
                const currentIndex = statusSegmentOptions.findIndex((option) => option.value === status);
                const nextIndex = Math.min(
                  statusSegmentOptions.length - 1,
                  Math.max(0, (currentIndex < 0 ? 0 : currentIndex) + step),
                );
                const nextOption = statusSegmentOptions[nextIndex];
                if (!nextOption || nextOption.value === status) return;
                event.preventDefault();
                setStatus(nextOption.value);
                resetPagination();
                event.currentTarget
                  .querySelectorAll<HTMLButtonElement>("[role='radio']")
                  .item(nextIndex)
                  ?.focus();
              }}
            >
              {statusSegmentOptions.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  role="radio"
                  aria-checked={status === option.value}
                  tabIndex={status === option.value ? 0 : -1}
                  className={`request-log-segment${status === option.value ? " is-active" : ""}`}
                  onClick={() => {
                    setStatus(option.value);
                    resetPagination();
                  }}
                >
                  {option.tone ? <i className={`request-log-result-dot ${option.tone}`} aria-hidden="true" /> : null}
                  {option.label}
                </button>
              ))}
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

            <div className="request-log-meta">
              {stats || result ? (
                <span className="request-log-total" title="当前筛选范围内的请求记录数">
                  请求记录 <strong>{totalCount.toLocaleString()}</strong> 条
                </span>
              ) : null}
              {health ? (
                <div
                  role={healthWarning ? "alert" : "status"}
                  className={`flex shrink-0 items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[10px] font-medium ${
                    healthWarning
                      ? "border border-amber-500/30 dark:border-amber-700/30 bg-amber-50 dark:bg-amber-950 text-amber-900 dark:text-amber-300"
                      : "border border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface,#fff)] text-[var(--codey-muted,#6e6e73)]"
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
                  <span className="hidden text-[10px] text-[var(--codey-subtle,#8e8e93)] md:inline">
                    · 已处理 {health.entriesWritten.toLocaleString()} 条
                  </span>
                </div>
              ) : null}
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

          <div className="request-log-filter-facets">
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

            {officialAccounts.length > 0 ? <Select
              aria-label="按官方账号筛选请求日志"
              className="w-40 shrink-0"
              filter
              optionList={officialAccountOptions}
              value={officialAccount}
              onChange={(value) => {
                setOfficialAccount(String(value ?? "all"));
                resetPagination();
              }}
            /> : null}

            <Select
              aria-label="按请求模型筛选请求日志"
              className="w-36 shrink-0"
              filter
              optionList={modelOptions}
              value={model}
              onOpenChange={(open) => {
                if (open) setModelsCatalogNeeded(true);
              }}
              onChange={(value) => {
                setModel(String(value ?? "all"));
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
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <label className="flex items-center gap-1.5 text-xs text-[var(--codey-muted,#6e6e73)]">
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
              <label className="flex items-center gap-1.5 text-xs text-[var(--codey-muted,#6e6e73)]">
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

          {filterChips.length > 0 ? (
            <TagGroup
              className="request-log-chips"
              aria-label="当前生效的筛选条件"
              size="sm"
              variant="surface"
              onRemove={(keys) => {
                for (const key of keys) {
                  filterChips.find((chip) => chip.key === key)?.onRemove();
                }
              }}
            >
              <TagGroup.List items={filterChips}>
                {(chip) => (
                  <Tag id={chip.key} textValue={chip.label} className="request-log-chip">
                    <span className="request-log-chip-text">{chip.label}</span>
                    <Tag.RemoveButton aria-label={`移除筛选：${chip.label}`} />
                  </Tag>
                )}
              </TagGroup.List>
              <button
                type="button"
                className="request-log-chips-clear"
                onClick={resetFilters}
              >
                全部清除
              </button>
            </TagGroup>
          ) : null}
        </div>


        {statsLoading ? <p className="m-0 text-xs text-[var(--codey-muted,#6e6e73)]" role="status">正在统计所选范围…</p> : null}
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
                <IconChartBar size={14} className="text-[var(--codey-text,#1d1d1f)] shrink-0" aria-hidden="true" />
                <span className="text-xs font-semibold text-[var(--codey-text,#1d1d1f)] shrink-0">范围概览</span>
                {overviewCollapsed ? (
                  <div className="flex items-center gap-2.5 text-xs text-[var(--codey-muted,#6e6e73)] truncate ml-1">
                    <span>总请求: <strong className="font-semibold text-[var(--codey-text,#1d1d1f)]">{stats.total.toLocaleString()}</strong> 条</span>
                    <span>成功率: <strong className={`font-semibold ${stats.successRate != null && stats.successRate >= 95 ? "text-emerald-600 dark:text-emerald-400" : stats.successRate != null && stats.successRate >= 80 ? "text-amber-600 dark:text-amber-400" : "text-rose-600 dark:text-rose-400"}`}>{stats.successRate != null ? `${stats.successRate.toFixed(1)}%` : "—"}</strong></span>
                    <span className="hidden md:inline">TTFT: <strong className="font-semibold text-[var(--codey-text,#1d1d1f)]">{formatDuration(stats.avgTtft)}</strong></span>
                    <span className="hidden md:inline" title="合格请求的全部输出 Token（含思考）除以上游首包后的总响应时间">速度: <strong className="font-semibold text-[var(--codey-text,#1d1d1f)]">{formatTokensPerSecond(stats.avgOutputTokensPerSecond)}</strong></span>
                    <span className="hidden lg:inline">Token: <strong className="font-semibold text-[var(--codey-text,#1d1d1f)]">{formatTokens(stats.totalTokensSum)}</strong></span>
                  </div>
                ) : (
                  <span className="text-[11px] text-[var(--codey-muted,#6e6e73)] shrink-0 hidden sm:inline">按筛选范围统计</span>
                )}
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {!overviewCollapsed ? (
                  <Select
                    aria-label="统计分组"
                    className="w-44"
                    optionList={[
                      { label: "按请求模型统计", value: "model" },
                      { label: "按供应商统计", value: "provider" },
                      ...(officialAccounts.length > 0
                        ? [{ label: "按官方账号统计", value: "official_account" }]
                        : []),
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
                  className="text-xs text-[var(--codey-muted,#6e6e73)] hover:text-[var(--codey-text,#1d1d1f)]"
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
                      <span className="text-[11px] font-medium text-[var(--codey-muted,#73767d)]">总请求数</span>
                      <div className="flex items-baseline gap-1">
                        <span className="text-[15px] font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">
                          {stats.total.toLocaleString()}
                        </span>
                        <span className="text-[10px] text-[var(--codey-muted,#6e6e73)]">条</span>
                      </div>
                    </div>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-muted,#73767d)]">上游首字节</span>
                      <span className="text-[15px] font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">{formatDuration(stats.avgUpstreamFirstByte)}</span>
                    </div>
                    <span className="text-[10px] text-[var(--codey-muted,#6e6e73)] truncate">路由准备 {formatDuration(stats.avgRouterPreUpstream)} · 响应头 {formatDuration(stats.avgUpstreamHeader)}</span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-muted,#73767d)]">下游首段内容</span>
                      <span className="text-[15px] font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">{formatDuration(stats.avgDownstreamFirstContent)}</span>
                    </div>
                    <span className="text-[10px] text-[var(--codey-muted,#6e6e73)] truncate">日志排队 {formatDuration(stats.avgQueueDelay)}</span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-muted,#73767d)]">请求成功率</span>
                      <span
                        className={`text-[15px] font-semibold tabular-nums ${
                          stats.successRate != null && stats.successRate >= 95
                            ? "text-emerald-600 dark:text-emerald-400"
                            : stats.successRate != null && stats.successRate >= 80
                              ? "text-amber-600 dark:text-amber-400"
                              : "text-rose-600 dark:text-rose-400"
                        }`}
                      >
                        {stats.successRate != null ? `${stats.successRate.toFixed(1)}%` : "—"}
                      </span>
                    </div>
                    <span className="text-[10px] text-[var(--codey-muted,#6e6e73)] truncate">
                      成功 {stats.succeededCount} · 失败 {stats.failedCount} · 其他 {stats.incompleteCount + stats.cancelledCount}
                    </span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-muted,#73767d)] whitespace-nowrap">平均首字耗时 (TTFT)</span>
                      <span className="text-[15px] font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">
                        {formatDuration(stats.avgTtft)}
                      </span>
                    </div>
                    <span className="text-[10px] text-[var(--codey-muted,#6e6e73)] truncate">
                      平均总耗时 {formatDuration(stats.avgDuration)} · <span title="合格请求的全部输出 Token（含思考）除以上游首包后的总响应时间">平均输出 {formatTokensPerSecond(stats.avgOutputTokensPerSecond)}</span>
                    </span>
                  </div>
                  <div className="request-log-metric">
                    <div className="flex items-baseline justify-between gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-muted,#73767d)] whitespace-nowrap">所选范围 Token 消耗</span>
                      <div className="flex items-baseline gap-1">
                        <span className="text-[15px] font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">
                          {formatTokens(stats.totalTokensSum)}
                        </span>
                        <span className="text-[10px] text-[var(--codey-muted,#6e6e73)]">tokens</span>
                      </div>
                    </div>
                    <span className="text-[10px] text-[var(--codey-muted,#6e6e73)] truncate">
                      输入 {formatTokens(stats.inputTokensSum)} · 输出 {formatTokens(stats.outputTokensSum)} · 总量已知 {stats.totalTokensKnownCount.toLocaleString()} / {stats.total.toLocaleString()} 条
                    </span>
                  </div>
                </div>

                <div className="request-log-analytics">
                  <div className="flex flex-wrap items-center justify-between gap-1 text-[var(--codey-muted,#6e6e73)]">
                    <div className="flex items-center gap-1.5 font-medium text-[var(--codey-text,#1d1d1f)]">
                      <span>趋势与分组统计</span>
                      <span className="text-[10px] font-normal text-[var(--codey-muted,#6e6e73)]">
                        · 成功率包含失败、未完成和中断请求
                      </span>
                    </div>
                    <span className="text-[10px] text-[var(--codey-muted,#6e6e73)]">
                      {new Date(stats.fromUnixMs).toLocaleString()} 至 {new Date(stats.toUnixMs).toLocaleString()}（不含结束时间）
                      {stats.databaseBytes != null ? ` · 存储约 ${((stats.databaseBytes + (stats.walBytes ?? 0)) / 1_048_576).toFixed(1)} MiB` : ""}
                    </span>
                  </div>

                  <div className="request-log-analytics-grid">
                    {/* 时间趋势 */}
                    <div className="request-log-analytics-section">
                      <div className="request-log-analytics-title">
                        <span className="text-[11px] font-semibold text-[var(--codey-text,#1d1d1f)]">
                          {stats.bucketMs === 3_600_000 ? "每小时" : "每天"}趋势
                        </span>
                        <span className="text-[10px] text-[var(--codey-muted,#6e6e73)]">
                          UTC 划分，本地时间显示
                        </span>
                      </div>
                      <Table variant="secondary">
                        <Table.ScrollContainer className="request-log-analytics-scroll">
                          <Table.Content className="request-log-analytics-table" aria-label={stats.bucketMs === 3_600_000 ? "每小时趋势" : "每天趋势"}>
                            <Table.Header className="request-log-analytics-thead">
                              <Table.Column id="time" className="request-log-analytics-th" isRowHeader>时间</Table.Column>
                              <Table.Column id="requests" className="request-log-analytics-th text-right">请求数</Table.Column>
                              <Table.Column id="tokens" className="request-log-analytics-th text-right">Token</Table.Column>
                              <Table.Column id="duration" className="request-log-analytics-th text-right">平均耗时</Table.Column>
                              <Table.Column id="firstContent" className="request-log-analytics-th text-right">首段内容</Table.Column>
                            </Table.Header>
                            <Table.Body
                              className="font-mono"
                              items={stats.trend}
                              renderEmptyState={() => (
                                <span className="block py-4 text-center font-sans text-xs text-[var(--codey-subtle,#8e8e93)]">
                                  所选时间范围暂无趋势数据
                                </span>
                              )}
                            >
                              {(bucket) => (
                                <Table.Row id={bucket.timestampUnixMs}>
                                  <Table.Cell className="request-log-analytics-td whitespace-nowrap text-[var(--codey-text,#1d1d1f)]">
                                    {new Date(bucket.timestampUnixMs).toLocaleString(undefined, {
                                      month: "2-digit",
                                      day: "2-digit",
                                      hour: "2-digit",
                                      minute: "2-digit",
                                    })}
                                  </Table.Cell>
                                  <Table.Cell className="request-log-analytics-td text-right font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">
                                    {bucket.total.toLocaleString()}
                                  </Table.Cell>
                                  <Table.Cell className="request-log-analytics-td text-right text-[var(--codey-text-soft,#48484a)] tabular-nums">
                                    {formatTokens(bucket.totalTokensSum)}
                                  </Table.Cell>
                                  <Table.Cell className="request-log-analytics-td text-right text-[var(--codey-text-soft,#48484a)] tabular-nums">
                                    {formatDuration(bucket.avgDuration)}
                                  </Table.Cell>
                                  <Table.Cell className="request-log-analytics-td text-right text-[var(--codey-text-soft,#48484a)] tabular-nums">
                                    {formatDuration(bucket.avgDownstreamFirstContent)}
                                  </Table.Cell>
                                </Table.Row>
                              )}
                            </Table.Body>
                          </Table.Content>
                        </Table.ScrollContainer>
                      </Table>
                    </div>

                    {/* 分组统计 */}
                    <div className="request-log-analytics-section">
                      <div className="request-log-analytics-title">
                        <span className="text-[11px] font-semibold text-[var(--codey-text,#1d1d1f)]">
                          {groupByLabels[groupBy] || "所选维度"}统计
                        </span>
                        <span className="text-[10px] text-[var(--codey-muted,#6e6e73)]">
                          {stats.groupsTruncated ? "最多展示 50 组" : `共 ${stats.groups.length} 组`}
                        </span>
                      </div>
                      <Table variant="secondary">
                        <Table.ScrollContainer className="request-log-analytics-scroll">
                          <Table.Content className="request-log-analytics-table" aria-label={`${groupByLabels[groupBy] || "所选维度"}统计`}>
                            <Table.Header className="request-log-analytics-thead">
                              <Table.Column id="group" className="request-log-analytics-th" isRowHeader>分组</Table.Column>
                              <Table.Column id="requests" className="request-log-analytics-th text-right">请求数</Table.Column>
                              <Table.Column id="tokens" className="request-log-analytics-th text-right">Token</Table.Column>
                              <Table.Column id="successRate" className="request-log-analytics-th text-right">成功率</Table.Column>
                            </Table.Header>
                            <Table.Body
                              className="font-mono"
                              items={stats.groups}
                              renderEmptyState={() => (
                                <span className="block py-4 text-center font-sans text-xs text-[var(--codey-subtle,#8e8e93)]">
                                  所选分组暂无数据
                                </span>
                              )}
                            >
                              {(group) => {
                                const rate = group.successRate;
                                const label = (groupBy === "provider"
                                  ? providerOptions.find((option) => option.value === group.key)?.label
                                  : groupBy === "official_account"
                                    ? officialAccountLabels.get(group.key)
                                    : undefined) || group.key || "未知";
                                return (
                                  <Table.Row id={group.key}>
                                    <Table.Cell className="request-log-analytics-td text-[var(--codey-text,#1d1d1f)]">
                                      <div className="flex flex-wrap items-center gap-x-2">
                                      <span className="max-w-[240px] truncate" title={label}>{label}</span>
                                      </div>
                                    </Table.Cell>
                                    <Table.Cell className="request-log-analytics-td text-right font-semibold text-[var(--codey-text,#1d1d1f)] tabular-nums">
                                      {group.total.toLocaleString()}
                                    </Table.Cell>
                                    <Table.Cell className="request-log-analytics-td text-right text-[var(--codey-text-soft,#48484a)] tabular-nums">
                                      {formatTokens(group.totalTokensSum)}
                                    </Table.Cell>
                                    <Table.Cell className="request-log-analytics-td text-right tabular-nums">
                                      <span
                                        className={`font-semibold ${
                                          rate != null && rate >= 95
                                            ? "text-emerald-600 dark:text-emerald-400"
                                            : rate != null && rate >= 80
                                              ? "text-amber-600 dark:text-amber-400"
                                              : "text-rose-600 dark:text-rose-400"
                                        }`}
                                      >
                                        {rate != null ? `${rate.toFixed(1)}%` : "—"}
                                      </span>
                                    </Table.Cell>
                                  </Table.Row>
                                );
                              }}
                            </Table.Body>
                          </Table.Content>
                        </Table.ScrollContainer>
                      </Table>
                    </div>
                  </div>
                </div>
              </>
            ) : null}
          </div>
        ) : null}

        <div className="request-log-results relative flex min-h-0 flex-1 flex-col overflow-hidden">
          {loading && result ? (
            <div className="absolute top-0 left-0 right-0 z-10 h-0.5 overflow-hidden bg-blue-100 dark:bg-blue-950">
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
              <div className="grid max-w-lg justify-items-center gap-2 text-[var(--codey-muted,#6e6e73)]">
                <IconDatabaseOff size={28} aria-hidden="true" />
                <strong className="text-sm text-[var(--codey-text,#1d1d1f)]">日志暂不可在线查看</strong>
                <p className="m-0 text-xs leading-5">{unavailableMessage(result?.reason)}</p>
              </div>
            </div>
          ) : !result && loading ? (
            <div className="grid min-h-48 flex-1 place-items-center" role="status">
              <div className="flex items-center gap-2 text-xs text-[var(--codey-muted,#6e6e73)]">
                <Spinner size="sm" />
                正在加载请求日志…
              </div>
            </div>
          ) : result?.items.length === 0 ? (
            <div className="grid min-h-48 flex-1 place-items-center p-6 text-center">
              <div className="grid justify-items-center gap-2 text-[var(--codey-muted,#6e6e73)]">
                <IconSearch size={26} aria-hidden="true" />
                <strong className="text-sm text-[var(--codey-text,#1d1d1f)]">
                  {hasFilters ? "没有匹配的请求日志" : "暂无请求日志"}
                </strong>
                <p className="m-0 text-xs">
                  {hasFilters ? "调整搜索或筛选条件后重试。" : "新请求完成后会在这里显示。"}
                </p>
              </div>
            </div>
          ) : (
            <Table
              variant="secondary"
              className={`request-log-stream ${loading && result ? "is-refreshing" : ""}`}
              aria-busy={loading}
              aria-label="请求日志记录，点击行查看详情"
            >
              <Table.ScrollContainer className="request-log-stream-scroll">
                <Table.Content
                  className="request-log-table"
                  keyboardNavigationBehavior="tab"
                >
                  <Table.Header
                    className="request-log-stream-header"
                    columns={REQUEST_LOG_STREAM_COLUMNS}
                  >
                    {(column) => (
                      <Table.Column
                        id={column.id}
                        className={`request-log-th request-log-th-${column.id}`}
                        textValue={column.title}
                        isRowHeader={column.id === "time"}
                      >
                        {column.title}
                      </Table.Column>
                    )}
                  </Table.Header>
                  <Table.Body className="request-log-stream-body" items={streamItems}>
                    {({ item }) => (
                      <RequestLogRow
                        item={item}
                        providerLabel={requestLogProviderLabel(item, catalog.profiles)}
                        officialAccountLabel={officialAccountLabel}
                        modelLabels={modelLabels}
                        selected={
                          selectedItem != null
                          && selectedItem.requestId === item.requestId
                          && selectedItem.timestampUnixMs === item.timestampUnixMs
                        }
                        onOpen={setSelectedItem}
                        onCopyId={handleCopyId}
                      />
                    )}
                  </Table.Body>
                </Table.Content>
              </Table.ScrollContainer>
            </Table>
          )}

          {result?.queryable && result.status === "ok" ? (
            <div className="request-log-pagination flex flex-none flex-wrap items-center justify-between gap-3 border-t border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 px-3.5 py-2 text-xs max-[760px]:flex-col max-[760px]:items-stretch">
              <div className="flex flex-wrap items-center gap-2 text-[var(--codey-muted,#6e6e73)]">
                <span className="font-semibold text-[var(--codey-text,#1d1d1f)]">
                  共 {totalCount.toLocaleString()} 条
                </span>
                <span className="text-[rgb(var(--codey-ink-rgb,0,0,0))]/20">·</span>
                <span>
                  当前显示 {firstVisible.toLocaleString()}–{lastVisible.toLocaleString()} 条
                </span>
                <span className="text-[rgb(var(--codey-ink-rgb,0,0,0))]/20">·</span>
                <span className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/6 px-1.5 py-0.5 font-mono text-[11px] font-medium text-[var(--codey-text,#1d1d1f)]">
                  第 {page} / {totalPages} 页
                </span>
              </div>
              <div className="flex flex-wrap items-center gap-2.5 max-[760px]:justify-end">
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
                        aria-label="上一页"
                        onPress={() => goToPage(page - 1)}
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
                            isDisabled={loading}
                            aria-label={`第 ${entry} 页`}
                            onPress={() => goToPage(entry)}
                          >
                            {entry}
                          </Pagination.Link>
                        </Pagination.Item>
                      ),
                    )}
                    <Pagination.Item>
                      <Pagination.Next
                        isDisabled={loading || page >= totalPages || !result?.hasMore}
                        aria-label="下一页"
                        onPress={() => goToPage(page + 1)}
                      >
                        <Pagination.NextIcon />
                      </Pagination.Next>
                    </Pagination.Item>
                  </Pagination.Content>
                </Pagination>
                <form
                  className="flex items-center gap-1.5 whitespace-nowrap text-[var(--codey-muted,#6e6e73)]"
                  aria-label="跳转页码"
                  onSubmit={(event) => {
                    event.preventDefault();
                    if (validJumpPage) goToPage(jumpPage);
                  }}
                >
                  <span>前往</span>
                  <Input
                    aria-label="跳转到指定页"
                    className="h-7 w-16 min-w-0 px-1.5 text-center text-xs tabular-nums"
                    type="text"
                    inputMode="numeric"
                    pattern="[0-9]+"
                    title={`请输入 1 到 ${totalPages} 之间的整数页码`}
                    value={pageJumpInput}
                    disabled={loading}
                    aria-invalid={pageJumpInput !== "" && !validJumpPage}
                    onChange={(event) => setPageJumpInput(event.target.value)}
                  />
                  <span>页</span>
                  <Button type="submit" variant="outline" size="xs" disabled={loading || !validJumpPage}>
                    跳转
                  </Button>
                </form>
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
              className="mt-4 flex items-start gap-2 rounded-[9px] border border-red-700/20 dark:border-red-700/20 bg-red-50 dark:bg-red-950 px-3 py-2.5 text-xs leading-5 text-red-800 dark:text-red-300"
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
            className="request-log-detail relative z-10 flex h-full w-full max-w-[580px] flex-col bg-[var(--codey-surface,#fff)] shadow-2xl transition-transform"
          >
            {/* 抽屉头部 */}
            <div className="flex flex-none items-center justify-between border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface-muted,#fbfbfd)] px-5 py-3.5">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <h3 className="m-0 text-sm font-bold text-[var(--codey-text,#1d1d1f)]">请求详情</h3>
                  {(() => {
                    const pres = statusPresentation[selectedItem.status] ?? {
                      label: selectedItem.status || "未知",
                      color: "default" as const,
                    };
                    return (
                      <Chip size="sm" variant="soft" color={pres.color}>
                        {pres.label}
                      </Chip>
                    );
                  })()}
                  {selectedItem.statusCode != null ? (
                    <span className="font-mono text-[11px] text-[var(--codey-muted,#6e6e73)]">
                      HTTP {selectedItem.statusCode}
                    </span>
                  ) : null}
                </div>
                <p className="m-0 mt-0.5 truncate font-mono text-[11px] text-[var(--codey-subtle,#8e8e93)]">
                  {formatTimestamp(selectedItem.timestampUnixMs)} · {selectedItem.requestId}
                </p>
              </div>
              <div className="flex items-center gap-1.5">
                <button
                  type="button"
                  className="flex h-7 w-7 cursor-pointer items-center justify-center rounded-lg text-[var(--codey-subtle,#8e8e93)] hover:bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 hover:text-[var(--codey-text,#1d1d1f)]"
                  onClick={() => setSelectedItem(null)}
                  aria-label="关闭详情"
                >
                  <IconX size={16} aria-hidden="true" />
                </button>
              </div>
            </div>

            {/* 抽屉内容区 */}
            <div className="flex-1 overflow-y-auto p-5 space-y-4 text-xs text-[var(--codey-text,#1d1d1f)]">
              {/* 耗时与 Token 使用量 */}
              <div className="rounded-xl border border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface-muted,#fafafa)] p-3.5">
                {/* 耗时分解 */}
                <div>
                  <div className="flex items-center justify-between pb-2 border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/6">
                    <span className="font-semibold text-[var(--codey-text,#1d1d1f)]">端到端耗时分解</span>
                    <span className="font-mono font-bold text-sm text-[var(--codey-text,#1d1d1f)]">
                      {formatDuration(selectedItem.totalDurationMs)}
                    </span>
                  </div>
                  <div className="mt-3 space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-[var(--codey-muted,#6e6e73)]">端到端首内容 (TTFT)</span>
                      <span className="font-mono font-medium text-blue-600 dark:text-blue-400">
                        {formatDuration(selectedItem.downstreamFirstContentMs ?? selectedItem.ttftMs)}
                      </span>
                    </div>
                    {selectedItem.routerPreUpstreamMs != null ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[var(--codey-muted,#6e6e73)]">路由前置耗时</span>
                        <span className="font-mono">{formatDuration(selectedItem.routerPreUpstreamMs)}</span>
                      </div>
                    ) : null}
                    {selectedItem.upstreamFirstByteMs != null ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[var(--codey-muted,#6e6e73)]">上游首包耗时</span>
                        <span className="font-mono">{formatDuration(selectedItem.upstreamFirstByteMs)}</span>
                      </div>
                    ) : null}
                    {selectedItem.upstreamHeaderMs != null ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[var(--codey-muted,#6e6e73)]">上游响应头耗时</span>
                        <span className="font-mono">{formatDuration(selectedItem.upstreamHeaderMs)}</span>
                      </div>
                    ) : null}
                    {selectedItem.queueDelayMs > 0 ? (
                      <div className="flex items-center justify-between">
                        <span className="text-[var(--codey-muted,#6e6e73)]">排队延迟</span>
                        <span className="font-mono text-amber-600 dark:text-amber-400">{formatDuration(selectedItem.queueDelayMs)}</span>
                      </div>
                    ) : null}
                    <div className="flex items-center justify-between" title="全部输出 Token（含思考）除以上游首包后的响应时间；计时不完整或输出样本不足时未知">
                      <span className="text-[var(--codey-muted,#6e6e73)]">输出速度</span>
                      <span className="font-mono">{formatTokensPerSecond(selectedItem.outputTokensPerSecond)}</span>
                    </div>
                  </div>
                </div>

                {/* Token 使用量 */}
                <div className="mt-3.5 pt-3.5 border-t border-[rgb(var(--codey-ink-rgb,0,0,0))]/6">
                  <span className="block font-semibold text-[var(--codey-text,#1d1d1f)] pb-2 border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/6">
                    Token 使用量
                  </span>
                  {selectedItem.totalTokens == null ? (
                    <div className="mt-3 rounded-lg border border-[rgb(var(--codey-ink-rgb,0,0,0))]/6 bg-[var(--codey-surface,#fff)] p-3 text-center">
                      <span className="text-xs text-[var(--codey-subtle,#8e8e93)]">
                        {usageUnavailablePresentation(selectedItem.usageUnavailableReason).label}：
                        {usageUnavailablePresentation(selectedItem.usageUnavailableReason).message}
                      </span>
                    </div>
                  ) : (
                    <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5">
                      <div>
                        <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">总 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono text-base font-bold text-[var(--codey-text,#1d1d1f)]">
                          {formatTokens(selectedItem.totalTokens)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">缓存输入 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono text-base font-bold text-purple-600 dark:text-purple-400">
                          {formatTokens(selectedItem.cachedInputTokens)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">输入 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono font-medium text-[var(--codey-text,#1d1d1f)]">
                          {formatTokens(selectedItem.inputTokens)}
                        </dd>
                      </div>
                      <div>
                        <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">输出 Token</dt>
                        <dd className="m-0 mt-0.5 font-mono font-medium text-[var(--codey-text,#1d1d1f)]">
                          {formatTokens(selectedItem.outputTokens)}
                        </dd>
                      </div>
                      {selectedItem.reasoningOutputTokens != null ? (
                        <div>
                          <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">思考输出 Token</dt>
                          <dd className="m-0 mt-0.5 font-mono font-medium text-[var(--codey-text-soft,#48484a)]">
                            {formatTokens(selectedItem.reasoningOutputTokens)}
                          </dd>
                        </div>
                      ) : null}
                      {selectedItem.cacheCreationInputTokens != null ? (
                        <div>
                          <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">缓存创建 Token</dt>
                          <dd className="m-0 mt-0.5 font-mono font-medium text-[var(--codey-text-soft,#48484a)]">
                            {formatTokens(selectedItem.cacheCreationInputTokens)}
                          </dd>
                        </div>
                      ) : null}
                    </dl>
                  )}
                </div>
              </div>

              {/* 模型与上游路由 */}
              <div className="rounded-xl border border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface-muted,#fafafa)] p-3.5">
                <span className="block font-semibold text-[var(--codey-text,#1d1d1f)] pb-2 border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/6">
                  模型与路由
                </span>
                <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5">
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">供应商</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[var(--codey-text,#1d1d1f)]">
                      {requestLogProviderLabel(selectedItem, catalog.profiles)}
                    </dd>
                  </div>
                  {selectedItem.officialAccountId ? (
                    <div>
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">官方账号</dt>
                      <dd
                        className="m-0 mt-0.5 truncate font-medium text-[var(--codey-text,#1d1d1f)]"
                        title={officialAccountLabel(selectedItem.officialAccountId)}
                      >
                        {officialAccountLabel(selectedItem.officialAccountId)}
                      </dd>
                    </div>
                  ) : null}
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">请求模型</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[var(--codey-text,#1d1d1f)]">
                      {selectedItem.model || selectedItem.requestedModel || "—"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">实际使用模型</dt>
                    <dd className="m-0 mt-0.5 break-all font-medium text-[var(--codey-text,#1d1d1f)]">
                      {selectedItem.upstreamResponseModel?.trim() || "上游未回报"}
                    </dd>
                  </div>
                  {selectedItem.requestedModel.trim()
                  && !modelIdsEqual(selectedItem.requestedModel, selectedItem.model || selectedItem.requestedModel) ? (
                    <div>
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">Codex 选择器</dt>
                      <dd
                        className="m-0 mt-0.5 break-all font-mono text-[11px] text-[var(--codey-text-soft,#48484a)]"
                        title="Codex 请求 Codey 时选择的模型 ID，带线路前缀"
                      >
                        {selectedItem.requestedModel}
                      </dd>
                    </div>
                  ) : null}
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">计费档位（请求 / 实际）</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[var(--codey-text,#1d1d1f)]">
                      {selectedItem.requestedServiceTier || "未记录"} / {selectedItem.serviceTier || "未确认"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">思考强度 / 预算</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[var(--codey-text,#1d1d1f)]">
                      {reasoningLabel(selectedItem)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">上游传输方式</dt>
                    <dd className="m-0 mt-0.5 font-medium text-[var(--codey-text,#1d1d1f)]">
                      <Chip
                        size="sm"
                        className={`request-log-protocol ${protocolTagClass(selectedItem.upstreamTransport)}`}
                      >
                        {selectedItem.upstreamTransport === "http_sse" ? "SSE" : (selectedItem.upstreamTransport || "—").toUpperCase()}
                      </Chip>
                    </dd>
                  </div>
                  <div>
                    <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">上游域名</dt>
                    <dd className="m-0 mt-0.5 font-mono text-[11px] text-[var(--codey-text-soft,#48484a)] truncate" title={selectedItem.upstreamAuthority || undefined}>
                      {selectedItem.upstreamAuthority || "—"}
                    </dd>
                  </div>
                  {selectedItem.upstreamRequestId ? (
                    <div className="col-span-2">
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">上游请求 ID</dt>
                      <dd className="m-0 mt-0.5 font-mono text-[11px] text-[var(--codey-text-soft,#48484a)] truncate">
                        {selectedItem.upstreamRequestId}
                      </dd>
                    </div>
                  ) : null}
                  {selectedItem.codexSessionId ? (
                    <div className="col-span-2">
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">Codex 会话 ID</dt>
                      <dd className="m-0 mt-0.5 flex items-center gap-1.5 font-mono text-[11px] text-[var(--codey-text-soft,#48484a)]">
                        {selectedItem.codexSessionIsParent ? (
                          <Chip size="sm" variant="soft">父会话</Chip>
                        ) : null}
                        <span className="truncate">{selectedItem.codexSessionId}</span>
                        <button
                          type="button"
                          className="text-blue-600 dark:text-blue-400 hover:text-blue-700 ml-1 cursor-pointer"
                          onClick={() => handleCopyId(selectedItem.codexSessionId!, { kind: "session", label: selectedItem.codexSessionIsParent ? "父会话 ID" : "会话 ID" })}
                        >
                          复制
                        </button>
                      </dd>
                    </div>
                  ) : null}
                  {selectedItem.subagent ? (
                    <div>
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">子代理请求</dt>
                      <dd className="m-0 mt-0.5 font-medium text-purple-600 dark:text-purple-400">是</dd>
                    </div>
                  ) : null}
                  {selectedItem.protocolBridge ? (
                    <div>
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">协议桥接</dt>
                      <dd className="m-0 mt-0.5 font-mono text-[11px] text-[var(--codey-text-soft,#48484a)]">
                        {selectedItem.protocolBridge}
                      </dd>
                    </div>
                  ) : null}
                  {selectedItem.requestInputState || selectedItem.upstreamInputState ? (
                    <div className="col-span-2">
                      <dt className="text-[11px] text-[var(--codey-subtle,#8e8e93)]">请求体 input 形态（客户端 / 发往上游）</dt>
                      <dd className="m-0 mt-0.5 grid gap-0.5">
                        <span
                          className={`font-medium ${isEmptyInputArray(selectedItem.requestInputState, selectedItem.requestInputItems) ? "text-red-600 dark:text-red-400" : "text-[var(--codey-text,#1d1d1f)]"}`}
                        >
                          客户端：
                          {requestShapeText(
                            selectedItem.requestInputState,
                            selectedItem.requestInputItems,
                            selectedItem.requestHasPreviousResponseId,
                          ) ?? "未记录"}
                          {selectedItem.requestBytes != null ? ` · ${formatBytes(selectedItem.requestBytes)}` : ""}
                        </span>
                        <span
                          className={`font-medium ${isEmptyInputArray(selectedItem.upstreamInputState, selectedItem.upstreamInputItems) ? "text-red-600 dark:text-red-400" : "text-[var(--codey-text,#1d1d1f)]"}`}
                        >
                          上游：
                          {requestShapeText(
                            selectedItem.upstreamInputState,
                            selectedItem.upstreamInputItems,
                            selectedItem.upstreamHasPreviousResponseId,
                          ) ?? "未记录"}
                          {selectedItem.upstreamBytes != null ? ` · ${formatBytes(selectedItem.upstreamBytes)}` : ""}
                        </span>
                      </dd>
                    </div>
                  ) : null}
                </dl>
              </div>

              <RequestLogRouteDiagnostics item={selectedItem} />

              {/* 请求头与响应头 */}
              <div className="rounded-xl border border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface-muted,#fafafa)] p-3.5 text-xs">
                <span className="block font-semibold text-[var(--codey-text,#1d1d1f)] pb-2 border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/6">
                  请求头与响应头
                </span>
                <div className="mt-3 grid gap-3">
                  <div>
                    <div className="flex items-center gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-text-soft,#48484a)]">
                        请求头（Codey 发往上游，敏感值已脱敏）
                      </span>
                      {selectedItem.upstreamRequestHeaders ? (
                        <button
                          type="button"
                          className="inline-flex items-center rounded p-0.5 text-[var(--codey-subtle,#8e8e93)] transition-colors hover:bg-[rgb(var(--codey-ink-rgb,0,0,0))]/8 hover:text-[var(--codey-text,#1d1d1f)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500"
                          onClick={() => handleCopyId(selectedItem.upstreamRequestHeaders!, { label: "上游请求头", subtext: "请求头内容" })}
                          aria-label="复制上游请求头"
                          title="复制上游请求头"
                        >
                          <IconCopy size={12} aria-hidden="true" />
                        </button>
                      ) : null}
                    </div>
                    {selectedItem.upstreamRequestHeaders ? (
                      <pre className="m-0 mt-1 max-h-60 overflow-auto whitespace-pre-wrap rounded-lg bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 p-2 font-mono text-[10px] leading-relaxed text-[var(--codey-text-soft,#48484a)] break-words">
                        {selectedItem.upstreamRequestHeaders}
                      </pre>
                    ) : (
                      <p className="m-0 mt-1 rounded-lg bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 p-2 text-[11px] text-[var(--codey-subtle,#8e8e93)]">
                        未记录
                      </p>
                    )}
                  </div>
                  <div>
                    <div className="flex items-center gap-1">
                      <span className="text-[11px] font-medium text-[var(--codey-text-soft,#48484a)]">
                        响应头（上游返回，敏感值已脱敏）
                      </span>
                      {selectedItem.upstreamResponseHeaders ? (
                        <button
                          type="button"
                          className="inline-flex items-center rounded p-0.5 text-[var(--codey-subtle,#8e8e93)] transition-colors hover:bg-[rgb(var(--codey-ink-rgb,0,0,0))]/8 hover:text-[var(--codey-text,#1d1d1f)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500"
                          onClick={() => handleCopyId(selectedItem.upstreamResponseHeaders!, { label: "上游响应头", subtext: "响应头内容" })}
                          aria-label="复制上游响应头"
                          title="复制上游响应头"
                        >
                          <IconCopy size={12} aria-hidden="true" />
                        </button>
                      ) : null}
                    </div>
                    {selectedItem.upstreamResponseHeaders ? (
                      <pre className="m-0 mt-1 max-h-60 overflow-auto whitespace-pre-wrap rounded-lg bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 p-2 font-mono text-[10px] leading-relaxed text-[var(--codey-text-soft,#48484a)] break-words">
                        {selectedItem.upstreamResponseHeaders}
                      </pre>
                    ) : (
                      <p className="m-0 mt-1 rounded-lg bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 p-2 text-[11px] text-[var(--codey-subtle,#8e8e93)]">
                        未记录
                      </p>
                    )}
                  </div>
                </div>
              </div>

              {/* 异常与降级诊断 */}
              {(selectedItem.status !== "succeeded" || selectedItem.fallbackCount > 0 || selectedItem.upstreamErrorSummary || selectedItem.errorCode) ? (
                <div className="rounded-xl border border-red-200 dark:border-red-700 bg-red-50/50 dark:bg-red-950/50 p-3.5">
                  <span className="block font-semibold text-red-900 dark:text-red-300 pb-2 border-b border-red-200 dark:border-red-700">
                    异常与诊断
                  </span>
                  <div className="mt-3 space-y-2">
                    {selectedItem.errorCode ? (
                      <div>
                        <span className="text-[11px] font-medium text-red-800 dark:text-red-300">错误码：</span>
                        <code className="ml-1 rounded bg-red-100 dark:bg-red-950 px-1 py-0.5 font-mono text-[11px] text-red-900 dark:text-red-300">
                          {selectedItem.errorCode}
                        </code>
                      </div>
                    ) : null}
                    {selectedItem.upstreamErrorSummary ? (
                      <div>
                        <span className="text-[11px] font-medium text-red-800 dark:text-red-300">上游错误内容（已脱敏）：</span>
                        <pre className="m-0 mt-1 max-h-80 overflow-auto whitespace-pre-wrap rounded-lg bg-[var(--codey-surface,#fff)] p-2 text-[11px] leading-relaxed text-red-900 dark:text-red-300 break-words">
                          {selectedItem.upstreamErrorSummary}
                        </pre>
                      </div>
                    ) : null}
                    {(() => {
                      const canc = cancellationPresentation(selectedItem);
                      return canc ? (
                        <div>
                          <span className="text-[11px] font-medium text-amber-800 dark:text-amber-300">中断原因：</span>
                          <p className="m-0 mt-1 rounded-lg bg-[var(--codey-surface,#fff)] p-2 text-[11px] leading-relaxed text-[var(--codey-text-soft,#48484a)]">
                            <strong className="font-semibold">{canc.label}：</strong>{canc.message}
                          </p>
                        </div>
                      ) : null;
                    })()}
                    {selectedItem.fallbackCount > 0 ? (
                      <div>
                        <span className="text-[11px] font-medium text-amber-800 dark:text-amber-300">
                          回退与重试：{selectedItem.fallbackCount} 次
                        </span>
                        {selectedItem.fallbackReason ? (
                          <p className="m-0 mt-1 text-[11px] text-[var(--codey-muted,#6e6e73)]">
                            原因：{fallbackReasonLabels[selectedItem.fallbackReason] ?? selectedItem.fallbackReason}
                          </p>
                        ) : null}
                      </div>
                    ) : null}
                  </div>
                </div>
              ) : null}
            </div>

            {/* 抽屉底部操作栏 */}
            <div className="flex flex-none items-center justify-end gap-2 border-t border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface-muted,#fafafa)] px-5 py-3">
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
                onClick={() => handleCopyId(JSON.stringify(selectedItem, null, 2), { label: "完整日志 JSON" })}
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
          className="pointer-events-auto fixed bottom-6 right-6 z-[1000] flex max-w-[min(420px,calc(100vw-32px))] items-center gap-2.5 rounded-xl border border-[rgb(var(--codey-ink-rgb,0,0,0))]/10 bg-[var(--codey-surface,#fff)]/95 px-4 py-3 text-xs text-[var(--codey-text,#1d1d1f)] shadow-[0_12px_32px_rgba(0,0,0,0.14)] backdrop-blur-2xl transition-all duration-200"
        >
          <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-emerald-50 dark:bg-emerald-950 text-emerald-600 dark:text-emerald-400">
            <IconCheck size={15} stroke={2.5} aria-hidden="true" />
          </div>
          <div className="min-w-0 flex-1">
            <p className="m-0 font-medium text-[var(--codey-text,#1d1d1f)]">{copyToast.text}</p>
            {copyToast.subtext ? (
              <p className="m-0 mt-0.5 truncate font-mono text-[11px] text-[var(--codey-subtle,#8e8e93)]">
                {copyToast.subtext}
              </p>
            ) : null}
          </div>
          <button
            type="button"
            className="ml-1 -mr-1 flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-[var(--codey-subtle,#8e8e93)] hover:bg-[rgb(var(--codey-ink-rgb,0,0,0))]/5 hover:text-[var(--codey-text,#1d1d1f)]"
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
