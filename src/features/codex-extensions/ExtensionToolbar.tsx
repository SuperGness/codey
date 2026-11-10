import type { ReactNode } from "react";
import {
  IconChevronDown,
  IconFolder,
  IconFolderCode,
  IconSearch,
  IconUser,
  IconX,
} from "@tabler/icons-react";
import { Button, Input, Select } from "../../components/ui";
import { matchesResource } from "./state";
import type { Inventory, McpEntry, Scope, SkillEntry } from "./types";

export function ExtensionScope({
  kind,
  scope,
  inventory,
  count,
  projectMode,
  projectPath,
  busy,
  onMode,
  onPath,
  onPick,
  onRead,
}: {
  kind: "mcp" | "skill";
  scope: Scope;
  inventory: Inventory | null;
  count: number;
  projectMode: boolean;
  projectPath: string;
  busy: boolean;
  onMode: (project: boolean) => void;
  onPath: (path: string) => void;
  onPick: () => void;
  onRead: () => void;
}) {
  const trimmedPath = projectPath.trim();
  const validPath = /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(trimmedPath);
  const isProjectLoaded =
    scope.kind === "project" && scope.projectPath === trimmedPath;
  const activeEntries = inventory
    ? kind === "mcp"
      ? inventory.mcps
      : inventory.skills.filter(
          (entry) => entry.ownership !== "plugin" && entry.scope !== "plugin",
        )
    : [];
  const enabledCount = activeEntries.filter(
    (entry) =>
      entry.enabled &&
      (!("enabledKnown" in entry) || entry.enabledKnown !== false),
  ).length;
  const invalidCount = activeEntries.filter(
    (entry) => entry.configurationStatus === "invalid",
  ).length;
  const configPath = inventory
    ? kind === "skill"
      ? inventory.skillConfigPath || inventory.configPath
      : inventory.configPath
    : "";

  return (
    <div className="extension-workspace-bar">
      <div className="extension-workspace-top">
        <div className="extension-scope-left">
          <span className="text-xs font-semibold text-muted">配置范围</span>
          <div
            className="extension-scope-pills"
            role="group"
            aria-label="配置范围"
          >
            <button
              type="button"
              aria-pressed={!projectMode}
              className={`extension-scope-pill ${!projectMode ? "active" : ""}`}
              disabled={busy}
              onClick={() => onMode(false)}
            >
              <IconUser size={13} aria-hidden="true" />
              <span>当前用户</span>
            </button>
            <button
              type="button"
              aria-pressed={projectMode}
              className={`extension-scope-pill ${projectMode ? "active" : ""}`}
              disabled={busy}
              onClick={() => onMode(true)}
            >
              <IconFolderCode size={13} aria-hidden="true" />
              <span>指定项目</span>
            </button>
          </div>
          {scope.kind === "project" && !projectMode && (
            <span className="extension-tag is-blue truncate max-w-[240px]" title={scope.projectPath}>
              项目：{scope.projectPath}
            </span>
          )}
        </div>

        {inventory && (
          <div className="extension-scope-meta" aria-label="资源概览统计">
            <span className="extension-stat-pill">
              <span>共</span>
              <strong>{count}</strong>
              <span>个{kind === "mcp" ? "服务" : "技能"}</span>
            </span>
            <span className="extension-stat-pill">
              <span
                className={`extension-stat-dot ${enabledCount > 0 ? "is-enabled" : ""}`}
                aria-hidden="true"
              />
              <strong>{enabledCount}</strong>
              <span>已启用</span>
            </span>
            {invalidCount > 0 && (
              <span className="extension-stat-pill">
                <span className="extension-stat-dot is-invalid" aria-hidden="true" />
                <strong className="text-danger">{invalidCount}</strong>
                <span className="text-danger">配置无效</span>
              </span>
            )}
          </div>
        )}
      </div>

      {projectMode && (
        <div className="extension-project-bar">
          <div className="extension-project-row">
            <div className="min-w-0 basis-full sm:flex-1 sm:basis-auto">
              <Input
                aria-label="项目绝对路径"
                className="w-full font-mono text-xs"
                placeholder="输入或选择项目目录的绝对路径（如 /Users/name/workspace）"
                disabled={busy}
                value={projectPath}
                leftSection={<IconFolderCode size={14} className="text-muted" />}
                onChange={(event) => onPath(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && validPath && !busy) {
                    event.preventDefault();
                    onRead();
                  }
                }}
              />
            </div>
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={onPick}
            >
              <IconFolder size={14} className="mr-1 inline-block" />
              选择项目
            </Button>
            <Button
              size="sm"
              variant={isProjectLoaded ? "outline" : "default"}
              disabled={busy || !validPath}
              onClick={onRead}
            >
              读取项目
            </Button>
          </div>
          <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted">
            {!validPath ? (
              <p className="m-0 text-xs text-muted">
                请输入或选择项目绝对路径，再读取该项目的配置。
              </p>
            ) : isProjectLoaded ? (
              <p className="m-0 flex items-center gap-1.5 text-xs text-muted">
                <span className="extension-stat-dot is-enabled" aria-hidden="true" />
                <span>当前正在管理此项目工作区的配置。</span>
              </p>
            ) : (
              <p className="m-0 text-xs text-amber-600 dark:text-amber-400">
                路径已就绪，请点击「读取项目」或按回车加载该项目配置。
              </p>
            )}
          </div>
        </div>
      )}

      {inventory && (
        <details className="extension-config-disclosure group">
          <summary className="extension-config-summary">
            <IconFolder size={13} className="shrink-0 text-muted" aria-hidden="true" />
            <span className="shrink-0 text-[11.5px] font-medium text-foreground/80">
              {scope.kind === "user" ? "用户配置" : "项目配置"}
            </span>
            <span className="text-muted/40" aria-hidden="true">·</span>
            <span
              className="min-w-0 flex-1 truncate font-mono text-[11.5px] text-muted"
              title={configPath}
            >
              {configPath}
            </span>
            <span className="inline-flex shrink-0 items-center gap-1 text-[11.5px] font-medium text-accent">
              <span className="group-open:hidden">范围说明</span>
              <span className="hidden group-open:inline">收起说明</span>
              <IconChevronDown
                size={13}
                className="transition-transform duration-150 group-open:rotate-180"
                aria-hidden="true"
              />
            </span>
          </summary>
          <div className="extension-config-details-grid">
            <div className="extension-config-detail-item">
              <span className="extension-config-detail-label">当前范围</span>
              <span className="extension-config-detail-value">
                {scope.kind === "user" ? "当前用户" : scope.projectPath}
              </span>
            </div>
            <div className="extension-config-detail-item">
              <span className="extension-config-detail-label">配置位置</span>
              <span className="extension-config-detail-value font-mono text-[11.5px] select-text">
                {configPath}
              </span>
            </div>
            <div className="extension-config-detail-item sm:col-span-2">
              <span className="extension-config-detail-label">生效规则</span>
              <span className="extension-config-detail-value">
                {inventory.applyNotice ||
                  "MCP 保存后自动刷新 Codex 配置；Skill 变更请在新会话中确认。"}
              </span>
            </div>
          </div>
        </details>
      )}
    </div>
  );
}

const STATUS_FILTERS = [
  { value: "all", label: "全部" },
  { value: "enabled", label: "已启用" },
  { value: "disabled", label: "已禁用" },
  { value: "readonly", label: "只读" },
  { value: "unknown", label: "状态待确认" },
  { value: "invalid", label: "配置无效" },
] as const;

export function ExtensionFilters({
  kind,
  entries,
  query,
  filter,
  source,
  sort,
  trailing,
  onQuery,
  onFilter,
  onSource,
  onSort,
  onClear,
}: {
  kind: "mcp" | "skill";
  entries: (McpEntry | SkillEntry)[];
  query: string;
  filter: string;
  source: string;
  sort: string;
  trailing?: ReactNode;
  onQuery: (value: string) => void;
  onFilter: (value: string) => void;
  onSource: (value: string) => void;
  onSort: (value: string) => void;
  onClear: () => void;
}) {
  const statusOptions = STATUS_FILTERS.map((option) => ({
    ...option,
    count: entries.filter((entry) => matchesResource(entry, "", option.value))
      .length,
  })).filter(
    (option) =>
      option.value === "all" ||
      option.value === "enabled" ||
      option.value === "disabled" ||
      option.count > 0 ||
      option.value === filter,
  );
  const hasActiveFilters =
    Boolean(query.trim()) || filter !== "all" || source !== "all";

  return (
    <div className="extension-filter-panel">
      <div className="extension-filter-top">
        <div className="extension-filter-controls">
          <Input
            type="search"
            aria-label={`搜索 ${kind === "mcp" ? "MCP" : "Skill"}`}
            className="min-w-48 flex-1 basis-full text-xs sm:basis-60"
            placeholder={
              kind === "mcp"
                ? "搜索服务名称、连接命令、URL 或路径…"
                : "搜索技能名称、描述或路径…"
            }
            value={query}
            onChange={(event) => onQuery(event.target.value)}
            leftSection={<IconSearch size={14} className="text-muted" />}
            rightSection={
              query ? (
                <button
                  type="button"
                  aria-label="清空搜索关键词"
                  className="inline-flex size-5 items-center justify-center rounded-full text-muted hover:bg-[var(--codey-press)] hover:text-foreground"
                  onClick={() => onQuery("")}
                >
                  <IconX size={12} aria-hidden="true" />
                </button>
              ) : undefined
            }
          />
          <Select
            aria-label="来源筛选"
            className="min-w-32 flex-1 text-xs sm:w-36 sm:flex-none"
            value={source}
            onChange={(value) => onSource(String(value))}
            optionList={
              kind === "mcp"
                ? [
                    { value: "all", label: "全部连接类型" },
                    { value: "stdio", label: "本地进程 (STDIO)" },
                    { value: "http", label: "远程服务 (HTTP)" },
                    { value: "unknown", label: "未知类型" },
                  ]
                : [
                    { value: "all", label: "全部来源" },
                    { value: "managed", label: "Codey 托管" },
                    { value: "external", label: "外部安装" },
                    { value: "builtin", label: "系统内置" },
                  ]
            }
          />
          <Select
            aria-label="排序"
            className="min-w-28 flex-1 text-xs sm:w-32 sm:flex-none"
            value={sort}
            onChange={(value) => onSort(String(value))}
            optionList={[
              { value: "name", label: "名称排序" },
              { value: "updated", label: "最近更新" },
              { value: "enabled", label: "启用优先" },
            ]}
          />
        </div>
      </div>

      <div className="extension-filter-bottom">
        <div className="flex flex-wrap items-center gap-2">
          <div
            className="extension-status-chips"
            role="group"
            aria-label="状态筛选"
          >
            {statusOptions.map((option) => (
              <button
                key={option.value}
                type="button"
                aria-pressed={filter === option.value}
                className={`extension-status-chip is-${option.value}`}
                onClick={() => onFilter(option.value)}
              >
                <span>{option.label}</span>
                <span className="extension-status-chip-count">
                  {option.count}
                </span>
              </button>
            ))}
          </div>
          {hasActiveFilters && (
            <Button size="xs" variant="ghost" onClick={onClear}>
              <IconX size={13} aria-hidden="true" />
              清除筛选
            </Button>
          )}
        </div>
        {trailing}
      </div>
    </div>
  );
}
