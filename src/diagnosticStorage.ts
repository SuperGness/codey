import type { CrashpadCleanup, TraceLogCleanup } from "./App.types";
import type { CrashpadPendingStats, TraceLogStats } from "./traceLogTypes";

export type DiagnosticStorageTarget = "trace" | "crashpad";
export type DiagnosticStorageCleanup = {
  status: "ok" | "partial";
  traceCleanup?: TraceLogCleanup | null;
  traceLogStatsBefore?: TraceLogStats | null;
  crashpadCleanup: CrashpadCleanup;
  traceLogWriteProtectionActive: boolean;
  errors: string[];
  traceLogStats: TraceLogStats;
  crashpadPendingStats: CrashpadPendingStats;
};
