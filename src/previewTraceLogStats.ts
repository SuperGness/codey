import type { CrashpadPendingStats, TraceLogStats } from "./traceLogTypes";

const capturedAt = Math.floor(Date.now() / 1000);

export const previewTraceLogStats: TraceLogStats = {
  capturedAt,
  databaseBytes: 903634944,
  errors: [],
};

export const previewCrashpadPendingStats: CrashpadPendingStats = {
  pending: false,
  capturedAt,
  directoriesFound: 2,
  reportsFound: 13,
  completeReports: 13,
  filesFound: 26,
  managedFiles: 26,
  orphanFiles: 0,
  unmanagedFiles: 0,
  pendingBytes: 3448832,
  managedBytes: 3448832,
  oldestTimestamp: capturedAt - 55 * 86400,
  newestTimestamp: capturedAt - 51 * 86400,
  hardLimitBytes: 512 * 1024 * 1024,
  targetBytes: 384 * 1024 * 1024,
  overLimit: false,
  protectionEnabled: true,
  errors: [],
};
