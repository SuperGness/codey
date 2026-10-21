import type { ReactNode } from "react";
import {
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
  const validPath = /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(projectPath.trim());
  return (
    <div className="codey-card space-y-3 px-4 py-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-3">
          <span className="text-xs font-medium text-muted">配置范围</span>
          <div
            className="codey-segmented-control"
            role="group"
            aria-label="配置范围"
          >
            <button
              type="button"
              aria-pressed={!projectMode}
              className={`codey-segmented-item ${!projectMode ? "active" : ""}`}
              disabled={busy}
              onClick={() => onMode(false)}
            >
              <IconUser size={13} className="inline-block mr-1" />
              当前用户
            </button>
            <button
              type="button"
              aria-pressed={projectMode}
              className={`codey-segmented-item ${projectMode ? "active" : ""}`}
              disabled={busy}
              onClick={() => onMode(true)}
            >
              <IconFolderCode size={13} className="inline-block mr-1" />
              指定项目
            </button>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {inventory && (
            <span className="text-xs tabular-nums text-muted">
              {count} 个{kind === "mcp" ? "服务" : "技能"}
            </span>
          )}
        </div>
      </div>
      {projectMode && (
        <>
          <div className="flex flex-wrap items-center gap-2 pt-3 border-t border-default">
            <div className="min-w-0 basis-full sm:flex-1 sm:basis-auto">
              <Input
                aria-label="项目绝对路径"
                className="w-full text-xs font-mono"
                placeholder="项目绝对路径"
                disabled={busy}
                value={projectPath}
                onChange={(event) => onPath(event.target.value)}
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
              variant={scope.kind === "project" ? "default" : "outline"}
              disabled={busy || !validPath}
              onClick={onRead}
            >
              读取项目
            </Button>
          </div>
          {!validPath && (
            <p className="m-0 text-xs text-muted">
              请输入或选择项目绝对路径，再读取该项目的配置。
            </p>
          )}
        </>
      )}
      {inventory && (
        <details className="group text-xs text-muted">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-md focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-accent">
            <IconFolder size={13} className="shrink-0" aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate" title={kind === "skill" ? inventory.skillConfigPath || inventory.configPath : inventory.configPath}>
              {kind === "skill" ? inventory.skillConfigPath || inventory.configPath : inventory.configPath}
            </span>
            <span className="shrink-0 text-accent group-open:hidden">范围说明</span>
            <span className="hidden shrink-0 text-accent group-open:inline">收起说明</span>
          </summary>
          <div className="mt-3 space-y-1.5 border-t border-default pt-3 leading-relaxed">
            <p className="m-0 break-all">当前范围：{scope.kind === "user" ? "当前用户" : scope.projectPath}</p>
            <p className="m-0 break-all">配置位置：{kind === "skill" ? inventory.skillConfigPath || inventory.configPath : inventory.configPath}</p>
            <p className="m-0">{inventory.applyNotice || "MCP 保存后自动刷新 Codex 配置；Skill 变更请在新会话中确认。"}</p>
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
    count: entries.filter((entry) => matchesResource(entry, "", option.value)).length,
  })).filter(
    (option) =>
      option.value === "all" ||
      option.value === "enabled" ||
      option.value === "disabled" ||
      option.count > 0 ||
      option.value === filter,
  );
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Input
          type="search"
          aria-label={`搜索 ${kind === "mcp" ? "MCP" : "Skill"}`}
          className="min-w-48 flex-1 basis-full text-xs md:basis-48"
          placeholder="搜索名称、描述或路径"
          value={query}
          onChange={(event) => onQuery(event.target.value)}
          leftSection={<IconSearch size={14} className="text-muted" />}
        />
        <Select
          aria-label="来源筛选"
          className="min-w-32 flex-1 text-xs md:w-36 md:flex-none"
          value={source}
          onChange={(value) => onSource(String(value))}
          optionList={
            kind === "mcp"
              ? [
                  { value: "all", label: "全部连接类型" },
                  { value: "stdio", label: "本地进程" },
                  { value: "http", label: "HTTP" },
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
          className="min-w-28 flex-1 text-xs md:w-32 md:flex-none"
          value={sort}
          onChange={(value) => onSort(String(value))}
          optionList={[
            { value: "name", label: "名称排序" },
            { value: "updated", label: "最近更新" },
            { value: "enabled", label: "启用优先" },
          ]}
        />
      </div>
      <div className="flex min-h-8 flex-wrap items-center gap-2">
        <div className="extension-status-chips" role="group" aria-label="状态筛选">
          {statusOptions.map((option) => (
            <button
              key={option.value}
              type="button"
              aria-pressed={filter === option.value}
              className={`extension-status-chip is-${option.value}`}
              onClick={() => onFilter(option.value)}
            >
              <span>{option.label}</span>
              <span className="extension-status-chip-count">{option.count}</span>
            </button>
          ))}
        </div>
        {(query.trim() || filter !== "all" || source !== "all") && (
          <Button size="xs" variant="ghost" onClick={onClear}>
            <IconX size={13} aria-hidden="true" />
            清除筛选
          </Button>
        )}
        {trailing}
      </div>
    </div>
  );
}
