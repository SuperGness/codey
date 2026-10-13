import type { EditorDraft, Inventory, McpEntry, SkillEntry } from "./types";

const CONTENT_BYTE_LIMIT = 1024 * 1024;
const encoder = new TextEncoder();

// 每次按键都会走到这里：纯 ASCII 内容的字节数等于长度，不必整串编码；
// 只有出现非 ASCII 时才需要精确计算，接近上限的输入也不会每键都做全量编码。
function exceedsByteLimit(content: string) {
  if (content.length > CONTENT_BYTE_LIMIT) return true;
  return (
    /[^\x00-\x7F]/.test(content) &&
    encoder.encode(content).length > CONTENT_BYTE_LIMIT
  );
}

export function scopeLabel(scope?: string): string {
  return (
    (
      {
        user: "用户",
        project: "项目",
        system: "系统",
        plugin: "插件",
      } as Record<string, string>
    )[scope ?? ""] ?? "待确认"
  );
}
export function configurationLabel(status?: string): string {
  return status === "valid" ? "有效" : status === "invalid" ? "无效" : "待检查";
}
export function dependencyStatus(
  dependency: { type: string; name: string },
  scope: string | undefined,
  inventory?: Inventory | null,
): string {
  const kind = dependency.type.toLowerCase();
  if (kind !== "mcp" && kind !== "skill") return "请在对应平台确认此依赖";
  const candidates = kind === "mcp" ? inventory?.mcps : inventory?.skills;
  const matches =
    candidates?.filter(
      (candidate) =>
        (candidate.name === dependency.name ||
          candidate.id === dependency.name) &&
        (candidate.scope ?? inventory?.scope.kind) === scope,
    ) ?? [];
  if (!matches.length) return "同范围未找到，请检查来源和范围";
  if (matches.length > 1) return "同范围有多个同名资源，请确认实际使用项";
  const match = matches[0];
  if (match.configurationStatus === "invalid") return "已找到，但配置无效";
  if (match.enabledKnown === false) return "已找到，启用状态待确认";
  return match.enabled
    ? "同范围已启用，实际运行需确认"
    : "同范围已找到，尚未启用";
}
export function batchTargets(
  entries: (McpEntry | SkillEntry)[],
  selected: string[],
  enabled: boolean,
): string[] {
  const ids = new Set(selected);
  return entries
    .filter(
      (entry) =>
        ids.has(entry.id) &&
        canToggle(entry) &&
        entry.enabled !== enabled &&
        (!enabled || entry.configurationStatus !== "invalid"),
    )
    .map((entry) => entry.id);
}

export function draftChanged(draft: EditorDraft | null): boolean {
  return (
    !!draft &&
    (draft.content !== draft.original || draft.id !== draft.originalId)
  );
}
export function matchesResource(
  entry: McpEntry | SkillEntry,
  query: string,
  filter: string,
): boolean {
  const text = [
    entry.name,
    entry.id,
    entry.sourcePath,
    "description" in entry ? entry.description : entry.summary,
  ]
    .join(" ")
    .toLocaleLowerCase();
  const enabledKnown =
    !("enabledKnown" in entry) || entry.enabledKnown !== false;
  return (
    text.includes(query.trim().toLocaleLowerCase()) &&
    (filter === "all" ||
      (filter === "enabled"
        ? enabledKnown && entry.enabled
        : filter === "disabled"
          ? enabledKnown && !entry.enabled
          : filter === "unknown"
            ? !enabledKnown
            : filter === "invalid"
              ? entry.configurationStatus === "invalid"
              : entry.readOnly))
  );
}
export function editorError(draft: EditorDraft): string {
  if (draft.kind === "mcp" && !/^[A-Za-z0-9_-]{1,128}$/.test(draft.id))
    return "服务标识需为 1 至 128 个英文字母、数字、连字符或下划线。";
  if (exceedsByteLimit(draft.content))
    return "内容超过 1 MB，请精简后重试。";
  if (!draft.content.trim())
    return draft.kind === "install"
      ? "请输入 Skill 目录或 ZIP 的绝对路径。"
      : "内容不能为空。";
  if (
    draft.kind === "install" &&
    !/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(draft.content.trim())
  )
    return "请使用绝对路径。";
  if (
    draft.kind === "skill" &&
    (!/^---\r?\n/.test(draft.content) ||
      !/^name:\s*\S+/m.test(draft.content) ||
      !/^description:\s*\S+/m.test(draft.content))
  )
    return "SKILL.md 需要 YAML 元数据，其中包含 name 和 description。";
  return "";
}
function resourceSource(entry: McpEntry | SkillEntry): string {
  return "ownership" in entry ? entry.ownership : entry.transport;
}
export function canToggle(entry: McpEntry | SkillEntry): boolean {
  return (
    !("ownership" in entry && (entry.ownership === "plugin" || entry.scope === "plugin")) &&
    !entry.readOnly &&
    entry.canToggle !== false &&
    !("enabledKnown" in entry && entry.enabledKnown === false) &&
    (entry.enabled || entry.configurationStatus !== "invalid")
  );
}
export function selectResources(
  entries: (McpEntry | SkillEntry)[],
  query: string,
  filter: string,
  source: string,
  sort: string,
) {
  return entries
    .filter(
      (entry) =>
        !("ownership" in entry && (entry.ownership === "plugin" || entry.scope === "plugin")) &&
        matchesResource(entry, query, filter) &&
        (source === "all" || resourceSource(entry) === source),
    )
    .sort((a, b) =>
      sort === "updated"
        ? (b.updatedAt ?? "").localeCompare(a.updatedAt ?? "") ||
          a.name.localeCompare(b.name)
        : sort === "enabled"
          ? Number(b.enabled) - Number(a.enabled) ||
            a.name.localeCompare(b.name)
          : a.name.localeCompare(b.name),
    );
}
export function pageResources<T>(entries: T[], page: number, size = 20) {
  const pages = Math.max(1, Math.ceil(entries.length / size));
  const current = Math.max(1, Math.min(page, pages));
  return {
    entries: entries.slice((current - 1) * size, current * size),
    page: current,
    pages,
  };
}
export function causeText(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
