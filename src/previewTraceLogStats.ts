import type { TraceLogStats } from "./TraceLogModule";

const capturedAt = Math.floor(Date.now() / 1000);

function dateKey(daysAgo: number): string {
  const date = new Date(capturedAt * 1000);
  date.setHours(12, 0, 0, 0);
  date.setDate(date.getDate() - daysAgo);
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

export const previewTraceLogStats: TraceLogStats = {
  capturedAt,
  recentDaysWindow: 7,
  databasesFound: 2,
  databasesScanned: 2,
  databaseBytes: 903634944,
  rowCount: 318757,
  estimatedLogBytes: 219676672,
  recentRowCount: 84630,
  recentEstimatedBytes: 71303168,
  oldestTimestamp: capturedAt - 37 * 86400,
  newestTimestamp: capturedAt - 45,
  daily: [
    { date: dateKey(6), rows: 9210, estimatedBytes: 7340032 },
    { date: dateKey(5), rows: 13820, estimatedBytes: 11534336 },
    { date: dateKey(4), rows: 7540, estimatedBytes: 5767168 },
    { date: dateKey(3), rows: 18600, estimatedBytes: 17825792 },
    { date: dateKey(2), rows: 11420, estimatedBytes: 8912896 },
    { date: dateKey(1), rows: 15700, estimatedBytes: 14680064 },
    { date: dateKey(0), rows: 8340, estimatedBytes: 5242880 },
  ],
  levels: [
    { name: "TRACE", rows: 216430, estimatedBytes: 165675008 },
    { name: "INFO", rows: 83210, estimatedBytes: 41943040 },
    { name: "DEBUG", rows: 18422, estimatedBytes: 11534336 },
    { name: "WARN", rows: 695, estimatedBytes: 524288 },
  ],
  topTargets: [
    { name: "codex_api::endpoint::responses_websocket", rows: 83042, estimatedBytes: 106954752 },
    { name: "codex_otel.log_only", rows: 85120, estimatedBytes: 33554432 },
    { name: "codex_otel.trace_safe", rows: 84380, estimatedBytes: 30408704 },
    { name: "codex_client::transport", rows: 24110, estimatedBytes: 25165824 },
    { name: "hyper_util::client::legacy::pool", rows: 9170, estimatedBytes: 7340032 },
  ],
  errors: [],
};
