import type { CrashpadPendingStats, TraceLogStats } from "./traceLogTypes";
import type { NotificationChannel } from "./notifications/types";

export type UpstreamProtocol =
  | "official"
  | "openaiResponses"
  | "openaiChatCompletions"
  | "anthropicMessages";

export type Profile = {
  id: string;
  name: string;
  shortName: string;
  baseUrl: string;
  apiKey: string;
  upstreamProtocol: UpstreamProtocol;
  authMode: "officialAccount" | "apiKey";
  apiKeyConfigured: boolean;
  clearApiKey?: boolean;
  sourceProviderId?: string;
  officialAccount: boolean;
  supportsRemoteCompaction?: boolean;
  supportsWebsockets?: boolean;
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

export type Config = {
  settingsRevision: number;
  activeProfileId: string;
  profiles: Profile[];
  initialRouteImportCompleted: boolean;
  webhook: { channels: NotificationChannel[] };
  promptOptimization: PromptOptimizationConfig;
  codexAppPath: string;
  userScripts: string[];
  selectedModelsByProvider: Record<string, string[]>;
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
  traceLogStats?: TraceLogStats;
  crashpadPendingStats?: CrashpadPendingStats;
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
    | "clear"
    | "restart"
    | "install-update"
    | "delete-notification-channel"
    | "delete-route";
  title: string;
  description: string;
  confirmLabel: string;
  run: () => void;
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
};

export type AppProps = {
  embedded?: boolean;
  modalContainer?: HTMLElement | null;
  modalVisible?: boolean;
  onAfterClose?: () => void;
  onClose?: () => void;
};
