export type LogSummary = {
  total: number;
  succeededCount: number;
  failedCount: number;
  incompleteCount: number;
  cancelledCount: number;
  successRate: number | null;
  avgDuration: number | null;
  avgTtft: number | null;
  avgOutputTokensPerSecond: number | null;
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

export type LogAnalytics = LogSummary & {
  queryable: boolean;
  reason?: string;
  fromUnixMs: number;
  toUnixMs: number;
  groups: Array<LogSummary & { key: string }>;
  groupsTruncated: boolean;
  trend: Array<{ timestampUnixMs: number; total: number; totalTokensSum: number | null; avgDuration: number | null; avgTtft: number | null; avgOutputTokensPerSecond: number | null; avgDownstreamFirstContent: number | null }>;
  bucketMs: number;
  dailyTrend?: UsageDay[];
  databaseBytes?: number;
  walBytes?: number;
  recordingHealth?: {
    enabled: boolean; active: boolean; sampleRatePerMillion: number; pendingEntries: number;
    accepted: number; entriesWritten: number; sampledOut: number;
    droppedFull: number; droppedClosed: number; writeDropped: number; writeFailures: number;
    observerPanics: number; writerPanics: number; shutdownTimeouts: number;
  } | null;
};

export type UsageRange = "7d" | "30d" | "all";

export function usageQuery(range: UsageRange, now = Date.now()) {
  return {
    groupBy: "model", groupSort: "tokens", includeDailyTrend: true,
    ...(range === "all" ? { allTime: true } : {
      fromUnixMs: now - (range === "7d" ? 7 : 30) * 86_400_000,
      toUnixMs: now,
    }),
  };
}

export function formatUsageNumber(value: number | null | undefined) {
  return value == null || !Number.isFinite(value) ? "—" : value.toLocaleString("zh-CN");
}

export function usageCoverage(total: number, known: number) {
  return total > 0 ? `${(Math.min(total, Math.max(0, known)) / total * 100).toFixed(1)}%` : "—";
}

export function usageSlices(stats: Pick<LogAnalytics, "groups" | "groupsTruncated" | "totalTokensSum" | "total">) {
  const sorted = [...stats.groups].sort((a, b) => (b.totalTokensSum ?? -1) - (a.totalTokensSum ?? -1) || a.key.localeCompare(b.key));
  const top = sorted.slice(0, 12).map((group) => ({
    key: group.key, tokens: group.totalTokensSum, requests: group.total, other: false,
  }));
  if (sorted.length > 12 || stats.groupsTruncated) {
    const topSum = top.reduce((sum, group) => sum + (group.tokens ?? 0), 0);
    const remainder = stats.totalTokensSum == null ? null : Math.max(0, stats.totalTokensSum - topSum);
    // 剩余组可能全部未上报；有请求但没有已知值时不能把它画成零。
    const knownRemainder = sorted.slice(12).some((group) => group.totalTokensSum != null);
    top.push({ key: "其他", tokens: remainder === 0 && !knownRemainder ? null : remainder,
      requests: Math.max(0, stats.total - top.reduce((sum, group) => sum + group.requests, 0)), other: true });
  }
  return top;
}

export function usageTrend(stats: Pick<LogAnalytics, "trend" | "fromUnixMs" | "toUnixMs" | "bucketMs">) {
  const span = stats.toUnixMs - stats.fromUnixMs;
  if (!Number.isFinite(span) || span <= 0 || !Number.isFinite(stats.bucketMs) || stats.bucketMs <= 0) {
    return { peak: null, bars: [] };
  }
  const buckets = stats.trend.filter((bucket) =>
    bucket.timestampUnixMs < stats.toUnixMs && bucket.timestampUnixMs + stats.bucketMs > stats.fromUnixMs,
  );
  const known = buckets.flatMap((bucket) => bucket.totalTokensSum == null ? [] : [bucket.totalTokensSum]);
  const peak = known.length ? Math.max(...known) : null;
  const bars = buckets.map((bucket) => {
    // UTC 分桶可能跨越查询边界，只绘制所选范围内的部分。
    const start = Math.max(stats.fromUnixMs, bucket.timestampUnixMs);
    const end = Math.min(stats.toUnixMs, bucket.timestampUnixMs + stats.bucketMs);
    const width = (end - start) / span * 800;
    return {
      ...bucket,
      x: (start - stats.fromUnixMs) / span * 800,
      width: Math.max(width - 1, width / 2),
      // 有请求但没有已知用量的桶高度为 0，但必须标记出来，不能与真正的零用量混同。
      tokensUnknown: bucket.totalTokensSum == null,
      height: (bucket.totalTokensSum ?? 0) / Math.max(1, peak ?? 0) * 128,
    };
  });
  return { peak, bars };
}

export type UsageLoadState = { loading: boolean; data: LogAnalytics | null; error: string };

export type UsageDay = { timestampUnixMs: number; total: number; totalTokensSum: number | null; totalTokensKnownCount: number };
const dayMs = 86_400_000;

/** Daily values come from the server's UTC day aggregation, never from multi-day trend buckets. */
export function usageHeatmap(stats: Pick<LogAnalytics, "dailyTrend" | "fromUnixMs" | "toUnixMs">, requestedYear?: number) {
  const { fromUnixMs: from, toUnixMs: to } = stats;
  if (!Number.isFinite(from) || !Number.isFinite(to) || to <= from || !Number.isFinite(new Date(from).getTime()) || !Number.isFinite(new Date(to - 1).getTime())) {
    return { years: [] as number[], year: 0, days: [], peak: 0, available: false };
  }
  const firstYear = new Date(from).getUTCFullYear();
  const lastYear = new Date(to - 1).getUTCFullYear();
  const splitYears = Math.ceil(to / dayMs) - Math.floor(from / dayMs) > 366;
  const years = splitYears ? Array.from({ length: lastYear - firstYear + 1 }, (_, index) => lastYear - index) : [lastYear];
  const year = requestedYear != null && years.includes(requestedYear) ? requestedYear : lastYear;
  const start = splitYears ? Math.max(Math.floor(from / dayMs) * dayMs, Date.UTC(year, 0, 1)) : Math.floor(from / dayMs) * dayMs;
  const end = splitYears ? Math.min(to, Date.UTC(year + 1, 0, 1)) : to;
  const source = new Map((stats.dailyTrend ?? []).map((day) => [day.timestampUnixMs, day]));
  const days = [];
  for (let timestamp = start; timestamp < end; timestamp += dayMs) {
    const value = source.get(timestamp);
    days.push({ timestampUnixMs: timestamp, total: value?.total ?? 0, totalTokensSum: value?.totalTokensSum ?? null,
      totalTokensKnownCount: value?.totalTokensKnownCount ?? 0, partial: timestamp < from || timestamp + dayMs > to });
  }
  const known = days.flatMap((day) => day.totalTokensSum == null ? [] : [day.totalTokensSum]);
  const peak = known.length ? Math.max(0, ...known) : null;
  return { years, year, days: days.map((day) => ({ ...day, level: day.total === 0 ? "empty" : day.totalTokensSum == null ? "unknown" : day.totalTokensSum === 0 ? "zero" : String(Math.max(1, Math.ceil(day.totalTokensSum / Math.max(1, peak ?? 0) * 4))) })), peak, available: Array.isArray(stats.dailyTrend) };
}

// 调用方为请求加超时；取消只阻止旧响应写入，不假装取消已发出的后端查询。
export function observeUsageRequest(request: () => Promise<LogAnalytics>, publish: (state: UsageLoadState) => void) {
  let active = true;
  publish({ loading: true, data: null, error: "" });
  void Promise.resolve().then(request).then(
    (data) => { if (active) publish({ loading: false, data, error: "" }); },
    (error: unknown) => { if (active) publish({ loading: false, data: null, error: error instanceof Error ? error.message : String(error) }); },
  );
  return () => { active = false; };
}
