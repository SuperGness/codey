export type TraceLogStats = {
  capturedAt: number;
  databaseBytes: number;
  errors: string[];
};

export type CrashpadPendingStats = {
  pending: boolean;
  capturedAt: number;
  directoriesFound: number;
  reportsFound: number;
  completeReports: number;
  filesFound: number;
  managedFiles: number;
  orphanFiles: number;
  unmanagedFiles: number;
  pendingBytes: number;
  managedBytes: number;
  oldestTimestamp?: number;
  newestTimestamp?: number;
  hardLimitBytes: number;
  targetBytes: number;
  overLimit: boolean;
  protectionEnabled: boolean;
  errors: string[];
};
