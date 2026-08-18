import type { CrashpadPendingStats, TraceLogStats } from "./traceLogTypes";
import type { NotificationChannel } from "./notifications/types";

export type Profile = {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  protocol: "responses" | "chatCompletions";
  ccSwitchProviderId?: string;
  ccSwitchReadOnly: boolean;
};

export type PromptOptimizationConfig = {
  enabled: boolean;
  baseUrl: string;
  apiKey: string;
  apiKeyConfigured: boolean;
  clearApiKey?: boolean;
  model: string;
  protocol: "responses" | "chatCompletions";
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
  model: string;
  reasoningEffort: string;
};

export type Config = {
  settingsRevision: number;
  activeProfileId: string;
  profiles: Profile[];
  webhook: { channels: NotificationChannel[] };
  promptOptimization: PromptOptimizationConfig;
  codexAppPath: string;
  userScripts: string[];
  selectedModelsByProvider: Record<string, string[]>;
  manualThirdPartyModelsByProvider: Record<string, string[]>;
  declaredOfficialModelsByProvider: Record<string, string[]>;
  upstreamModelsByProvider: Record<string, string[]>;
  defaultModelByProvider: Record<string, string>;
  disableTraceLogWrites: boolean;
  protectCrashpadPending: boolean;
  slimCodexPet: boolean;
  gpuLaunchMode: "off" | "disableGpu" | "disableGpuRasterization";
  fastContextTools: boolean;
  fastCodexStartup: boolean;
  subagentOptimization: boolean;
  subagentModel: string;
  subagentReasoningEffort: string;
  subagentRoles: Record<SubagentRoleId, SubagentRoleConfig>;
  hideFullAccessWarning: boolean;
  showAccountUsageInHeader: boolean;
  showTokenStatsCard: boolean;
};

export type OfficialModelState = {
  slug: string;
  displayName: string;
  supported: boolean;
  supportedReasoningEfforts: string[];
  defaultReasoningEffort: string;
};

export type ModelState = {
  officialModels: OfficialModelState[];
  officialModelIds: string[];
  thirdPartyModels: string[];
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
  status: "effective" | "executed" | "failed" | "unknown";
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
  startupError?: string;
  codexAppPath?: string;
  maintenance?: Maintenance;
  injectionScripts?: InjectionScriptStatus[];
  traceLogStats?: TraceLogStats;
  crashpadPendingStats?: CrashpadPendingStats;
};

export type PluginMarketplaceStatus = {
  status: "ready" | "needs_repair" | "error";
  needsRepair?: boolean;
  officialMarketplace?: boolean;
  officialRegistered?: boolean;
  officialPath?: string | null;
  remoteMarketplace?: boolean;
  remoteRegistered?: boolean;
  remotePath?: string | null;
  localMarketplacePath?: string;
  initializedRemote?: boolean;
  configuredRemote?: boolean;
  configChanged?: boolean;
  message?: string;
};

export type CcSwitchStatus = {
  changed: boolean;
  provider: {
    id: string;
    name: string;
    official: boolean;
    baseUrl: string;
    protocol: "responses" | "chatCompletions";
  };
};

export type Notice = { tone: "info" | "success" | "error"; text: string };
export type InlineResult = {
  tone: "idle" | "pending" | "success" | "error";
  text: string;
};

export type Confirmation = {
  action: "clear" | "restart" | "install-update" | "delete-notification-channel";
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
