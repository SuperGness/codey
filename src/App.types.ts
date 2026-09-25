import type { NotificationChannel } from "./notifications/types";

export type UpstreamProtocol =
  | "official"
  | "openaiResponses"
  | "openaiChatCompletions"
  | "anthropicMessages";

export type Profile = {
  enabled?: boolean;
  id: string;
  name: string;
  shortName: string;
  baseUrl: string;
  apiKey: string;
  upstreamProtocol: UpstreamProtocol;
  authMode: "officialAccount" | "apiKey";
  apiKeyConfigured: boolean;
  clearApiKey?: boolean;
  modelRequestHeaders?: Record<string, string>;
  upstreamProxy?: string;
  sourceProviderId?: string;
  officialAccount: boolean;
  /** 该线路来自哪个已保存的官方账号；多条官方线路靠它区分。 */
  officialAccountId?: string;
  supportsRemoteCompaction?: boolean;
  supportsWebsockets?: boolean;
  supportsNativeWebSearch?: boolean;
  supportsAutoReview?: boolean;
};

export type PromptOptimizationConfig = {
  enabled: boolean;
  mode: "codeyRoute" | "manual";
  baseUrl: string;
  apiKey: string;
  apiKeyConfigured: boolean;
  clearApiKey?: boolean;
  model: string;
  upstreamProtocol:
    | "openaiResponses"
    | "openaiChatCompletions"
    | "anthropicMessages";
  instruction: string;
};

export type SubagentRoleId =
  | "codey_quick_scan"
  | "codey_deep_research"
  | "codey_visual_analysis"
  | "codey_worker"
  | "codey_visual_worker"
  | "default";

export type SubagentRoleConfig = {
  enabled: boolean;
  model: string;
  reasoningEffort: string;
};

export type RouteRequestLogConfig = {
  enabled: boolean;
  backend: "ndjson" | "sqlite";
  queueCapacity: number;
  batchSize: number;
  flushIntervalMs: number;
  shutdownFlushTimeoutMs: number;
  sampleRatePerMillion: number;
  maxFileBytes: number;
  retainedFiles: number;
  retentionDays: number;
};

export type ModelContextConfig = {
  contextWindowTokens: number;
  autoCompactTokenLimit?: number | null;
  reserveOutputTokens?: number | null;
};

export type ModelReasoningEffort = {
  level: string;
  value: string;
};

export type Config = {
  settingsRevision: number;
  autoCheckCodeyUpdates: boolean;
  localRouterEnabled: boolean;
  routeRequestLog: RouteRequestLogConfig;
  streamMaxRetries: number;
  activeProfileId: string;
  profiles: Profile[];
  initialRouteImportCompleted: boolean;
  webhook: { channels: NotificationChannel[] };
  promptOptimization: PromptOptimizationConfig;
  codexAppPath: string;
  userScripts: string[];
  selectedModelsByProvider: Record<string, string[]>;
  modelContextByProvider?: Record<string, Record<string, ModelContextConfig>>;
  modelReasoningEffortsByProvider?: Record<
    string,
    Record<string, ModelReasoningEffort[]>
  >;
  manualThirdPartyModelsByProvider: Record<string, string[]>;
  declaredOfficialModelsByProvider: Record<string, string[]>;
  upstreamModelsByProvider: Record<string, string[]>;
  defaultModel: string;
  disableTraceLogWrites: boolean;
  protectCrashpadPending: boolean;
  slimCodexPet: boolean;
  gpuLaunchMode: "off" | "disableGpu" | "disableGpuRasterization";
  fastContextTools: boolean;
  subagentOptimization: boolean;
  subagentModel: string;
  subagentReasoningEffort: string;
  subagentRoles: Record<SubagentRoleId, SubagentRoleConfig>;
  miscModel: string;
  hideFullAccessWarning: boolean;
  showAccountUsageInHeader: boolean;
};

export type OfficialModelState = {
  slug: string;
  displayName: string;
  supported: boolean;
  supportedReasoningEfforts: string[];
  defaultReasoningEffort: string;
};

export type ThirdPartyModelState = {
  slug: string;
  supportedReasoningEfforts: string[];
  autoSupportedReasoningEfforts: string[];
  reasoningEfforts: ModelReasoningEffort[];
  defaultReasoningEffort: string;
};

export type ModelState = {
  officialModels: OfficialModelState[];
  officialModelIds: string[];
  thirdPartyModels: string[];
  thirdPartyModelMetadata?: ThirdPartyModelState[];
  manualThirdPartyModels: string[];
  upstreamModels: string[];
  defaultModel: string;
};

export type FastContextToolsStatus = {
  userConfigured: boolean;
  detectionFailed: boolean;
  serverId?: string;
};

export type Maintenance = {
  sessionStatus?: string;
  sessionFilesFixed?: number;
  sqliteRowsUpdated?: number;
  ghostTasksPruned?: number;
  performanceStatus?: string;
  performanceDetail?: string;
  startupInjectionMode?: string;
};

export type InjectionScriptStatus = {
  id: string;
  name: string;
  source: "builtin" | "user";
  visibility: "feature" | "internal";
  status: "effective" | "executed" | "inactive" | "failed" | "unknown";
  detail?: string;
  error?: string;
};

export type OfficialAccount = {
  id: string;
  email?: string;
  planType?: string;
  accountId?: string;
  addedAt: number;
  lastRefresh?: string;
  routeName?: string;
  routeShortName?: string;
  upstreamProxy?: string;
  /** 自定义 OpenAI 网关。留空时使用官方默认地址。 */
  baseUrl?: string;
  invalid?: boolean;
  invalidReason?: string;
  isDefault: boolean;
};

export type OfficialAccountsResult = {
  accounts?: OfficialAccount[];
  accountId?: string;
  defaultAccountId?: string | null;
  officialAccountAvailable?: boolean;
  officialAccountStatus?: string;
  config?: Config;
  modelState?: ModelState;
  restartRequired?: boolean;
  modelHotReloaded?: boolean;
  warning?: string;
};

export type RuntimeStatus = {
  running: boolean;
  appVersion?: string;
  availableUpdate?: UpdateCheck;
  codexAppVersion?: string;
  clientPlatform?: string;
  restartRequired?: boolean;
  restartInProgress?: boolean;
  activeProfileId?: string;
  activeProfileName?: string;
  officialAccountAvailable?: boolean;
  startupError?: string;
  codexAppPath?: string;
  maintenance?: Maintenance;
  injectionScripts?: InjectionScriptStatus[];
  fastContextToolsActive?: boolean;
  subagentOptimizationActive?: boolean;
  notificationChannelsActive?: boolean;
  activeNotificationChannelCount?: number;
  traceLogWriteProtectionActive?: boolean;
  crashpadDiskProtectionActive?: boolean;
};

export type PluginMarketplaceStatus = {
  status: "ready" | "needs_repair" | "error";
  needsRepair?: boolean;
  officialMarketplace?: boolean;
  officialPath?: string | null;
  remoteMarketplace?: boolean;
  remoteRegistered?: boolean;
  remotePath?: string | null;
  managedConfigCompatible?: boolean;
  localMarketplacePath?: string;
  initializedRemote?: boolean;
  configuredRemote?: boolean;
  configChanged?: boolean;
  message?: string;
};

export type ProviderStatus = {
  changed: boolean;
  provider: {
    id: string;
    name: string;
    official: boolean;
    baseUrl: string;
  };
};

export type Notice = { tone: "info" | "success" | "error"; text: string };
export type InlineResult = {
  tone: "idle" | "pending" | "success" | "error";
  text: string;
};

export type Confirmation = {
  action:
    | "restart"
    | "repair-codex-config"
    | "install-update"
    | "download-update"
    | "disable-auto-update-check"
    | "delete-notification-channel"
    | "delete-route"
    | "delete-official-account";
  title: string;
  description: string;
  confirmLabel: string;
  run: () => void;
  /// 用户点"稍后"/关闭对话框时触发，用于记录"本次不再提示"。
  onDismiss?: () => void;
};

export type TraceLogCleanup = {
  databasesFound: number;
  databasesCleaned: number;
  rowsDeleted: number;
  bytesBefore: number;
  bytesAfter: number;
  bytesReclaimed: number;
};

export type CrashpadCleanup = {
  directoriesFound: number;
  reportsFound: number;
  reportsDeleted: number;
  filesFound: number;
  filesDeleted: number;
  orphanFilesDeleted: number;
  unmanagedFiles: number;
  skippedRecentReports: number;
  bytesBefore: number;
  bytesAfter: number;
  bytesReclaimed: number;
  limitApplied: boolean;
  stillOverLimit: boolean;
  errors: string[];
};

export type UpdateCheck = {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  selectedAsset?: UpdateAsset;
  releaseNotes?: string | null;
  publishId?: string | null;
};

export type UpdateAsset = {
  platform: string;
  arch: string;
  packageType: string;
  fileName: string;
  url: string;
  sha256: string;
  size: number;
};

export type UpdateDownload = {
  latestVersion: string;
  filePath: string;
  fileName: string;
  size: number;
  sha256: string;
  asset: UpdateAsset;
  publishId?: string | null;
};

/// 上一次更新安装留给本次启动的结果。助手在退出前写下，控制台读取后删除，
/// 保证同一次结果只提示一次。
export type UpdateInstallReport = {
  version: string;
  status: "started" | "installed" | "unverified" | "failed";
  message: string;
  writtenAt: number;
};

export type AppProps = {
  embedded?: boolean;
  modalContainer?: HTMLElement | null;
  modalVisible?: boolean;
  onAfterClose?: () => void;
  onClose?: () => void;
};
