export type ExtensionTransport = <T>(
  request: Record<string, unknown>,
) => Promise<T>;
export type Scope = { kind: "user" } | { kind: "project"; projectPath: string };
export interface ResourceCapabilities {
  updatedAt?: string | null;
  configurationStatus?: "valid" | "invalid" | "unknown";
  error?: string;
  canEdit?: boolean;
  canToggle?: boolean;
  canRemove?: boolean;
  canCheck?: boolean;
}
export interface McpEntry extends ResourceCapabilities {
  id: string;
  name: string;
  enabled: boolean;
  enabledKnown?: boolean;
  sourcePath: string;
  transport: "stdio" | "http" | "unknown";
  readOnly: boolean;
  reason?: string;
  summary: string;
  scope?: "user" | "project";
}
export interface SkillEntry extends ResourceCapabilities {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  enabledKnown?: boolean;
  sourcePath: string;
  manifestPath: string;
  scope: "user" | "project" | "plugin" | "system";
  ownership: "managed" | "external" | "plugin" | "builtin";
  readOnly: boolean;
  reason?: string;
  version?: string | null;
  origin?: string;
  dependencies?: { type: string; name: string }[];
  dependencyWarnings?: string[];
  error?: string;
}
export interface Inventory {
  scope: Scope;
  configPath: string;
  skillConfigPath?: string;
  revision: string;
  mcps: McpEntry[];
  skills: SkillEntry[];
  warnings: string[];
  applyNotice: string;
}
export interface SkillCacheInventory {
  revision: string;
  skills: SkillEntry[];
  warnings: string[];
}
export interface MutationResult {
  inventory: Inventory;
  applyStatus:
    | "restart-required"
    | "reload-required"
    | "applied"
    | "pending-runtime"
    | "reload-failed"
    | "unchanged";
  message: string;
}
export interface CheckResult {
  checkedAt?: string;
  serverInfo?: { name?: string; version?: string };
  protocolVersion?: string;
  ok: boolean;
  summary: string;
  checks: { name: string; ok: boolean; message: string }[];
}
export interface SkillInstallPreview {
  name: string;
  description: string;
  sourcePath: string;
  sourceKind: "directory" | "zip";
  files: { path: string; size: number; executable: boolean }[];
  fileCount: number;
  totalBytes: number;
  sourceRevision: string;
}
export interface EditorDraft {
  kind: "mcp" | "skill" | "install";
  id: string;
  content: string;
  original: string;
  originalId: string;
  revision: string;
  readOnly: boolean;
  isNew?: boolean;
  jsonService?: string;
  entry?: McpEntry | SkillEntry;
  installPreview?: SkillInstallPreview;
}
export interface Confirmation {
  title: string;
  description: string;
  action: Record<string, unknown>;
  destructive?: boolean;
}
