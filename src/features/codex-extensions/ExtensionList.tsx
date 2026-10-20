import { useState } from "react";
import {
  IconBook2,
  IconDownload,
  IconEye,
  IconPlugConnected,
  IconServer,
  IconSettings,
  IconTrash,
} from "@tabler/icons-react";
import { Badge, Button, Checkbox, Switch } from "../../components/ui";
import type { McpEntry, SkillEntry, CheckResult } from "./types";
import { canToggle } from "./state";

export function ExtensionList({
  kind,
  entries,
  busy,
  busyAction,
  busyResourceId,
  onAction,
  selected = [],
  onSelect,
  checks = {},
  revision,
  filtered = false,
}: {
  kind: "mcp" | "skill";
  entries: (McpEntry | SkillEntry)[];
  busy: boolean;
  busyAction?: string;
  busyResourceId?: string;
  onAction: (action: string, entry: McpEntry | SkillEntry) => void;
  selected?: string[];
  onSelect?: (id: string) => void;
  checks?: Record<string, CheckResult & { revision?: string }>;
  revision?: string;
  filtered?: boolean;
}) {
  const [expandedChecks, setExpandedChecks] = useState<Record<string, boolean>>({});
  const isMcp = kind === "mcp";
  const KindIcon = isMcp ? IconServer : IconBook2;

  const isCheckExpanded = (id: string, ok: boolean) => {
    return expandedChecks[id] !== undefined ? expandedChecks[id] : !ok;
  };
  const toggleCheckExpanded = (id: string, ok: boolean) => {
    setExpandedChecks((current) => ({
      ...current,
      [id]: !isCheckExpanded(id, ok),
    }));
  };

  if (!entries.length) {
    return (
      <div className="flex min-h-64 flex-col items-center justify-center rounded-xl border border-dashed border-default px-5 py-12 text-center">
        <div className="mb-3 flex size-12 items-center justify-center rounded-2xl bg-default/40 text-muted">
          <KindIcon size={24} stroke={1.5} />
        </div>
        <h4 className="m-0 text-sm font-semibold text-foreground">
          {filtered
            ? "没有符合条件的结果"
            : `此范围暂无${isMcp ? " MCP 服务" : " Skill"}`}
        </h4>
        <p className="mb-0 mt-1 max-w-sm text-xs text-muted leading-relaxed">
          {filtered
            ? "尝试调整关键词、状态或来源条件。"
            : `切换范围或新增${isMcp ? " MCP 服务" : " Skill"}，开始管理资源。`}
        </p>
      </div>
    );
  }

  return (
    <div className="overflow-hidden rounded-xl border border-default bg-background divide-y divide-default">
      {entries.map((entry) => {
        const isBuiltin = "ownership" in entry && entry.ownership === "builtin";
        const isEnabled = entry.enabled;
        const isKnown =
          isBuiltin || !("enabledKnown" in entry) || entry.enabledKnown !== false;
        const isManaged = "ownership" in entry && entry.ownership === "managed";
        const checkResult = checks[entry.id];
        const working = busy && busyResourceId === entry.id;

        return (
          <article
            key={entry.id}
            aria-busy={working}
            className={`flex min-w-0 flex-col transition-colors hover:bg-default/15 lg:flex-row lg:items-start ${selected.includes(entry.id) ? "bg-accent/5" : ""}`}
          >
            <div className="min-w-0 flex-1 px-4 py-3.5">
              <div className="flex items-start justify-between gap-3">
                <div className="flex items-start gap-3 min-w-0 flex-1">
                  {onSelect && canToggle(entry) && (
                    <div className="mt-2 shrink-0">
                      <Checkbox
                        aria-label={`选择 ${entry.name}`}
                        checked={selected.includes(entry.id)}
                        disabled={
                          busy ||
                          (selected.length >= 100 && !selected.includes(entry.id))
                        }
                        onCheckedChange={() => onSelect(entry.id)}
                      />
                    </div>
                  )}
                  <div
                    className={`flex size-9 shrink-0 items-center justify-center rounded-lg ${isEnabled && isKnown ? "bg-accent/10 text-accent" : "bg-default/40 text-muted"}`}
                  >
                    <KindIcon size={20} stroke={1.8} aria-hidden="true" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <h4
                        title={entry.name}
                        className="m-0 max-w-full truncate text-sm font-semibold text-foreground"
                      >
                        {entry.name}
                      </h4>
                      {!isBuiltin && entry.readOnly && (
                        <Badge
                          variant="outline"
                          className="text-[10px] px-1.5 py-0"
                        >
                          只读
                        </Badge>
                      )}
                      {"ownership" in entry && entry.ownership !== "builtin" && (
                        <Badge
                          variant="outline"
                          className="text-[10px] px-1.5 py-0"
                        >
                          {
                            {
                              managed: "Codey 托管",
                              external: "外部安装",
                              plugin: "插件缓存",
                            }[entry.ownership]
                          }
                        </Badge>
                      )}
                    </div>
                    {"transport" in entry && (
                      <p title={`${entry.transport} · ${entry.summary}`} className="mb-0 mt-1 truncate text-xs text-muted">
                        {entry.transport} · {entry.summary}
                      </p>
                    )}
                    {"description" in entry && entry.description && (
                      <p title={entry.description} className="mb-0 mt-1 truncate text-xs text-muted">
                        {entry.description}
                      </p>
                    )}
                  </div>
                </div>

                <div className="flex flex-wrap items-center justify-end gap-2 shrink-0">
                  {isBuiltin ? (
                    <Badge
                      variant="outline"
                      className="border-default text-muted font-medium"
                    >
                      系统内置
                    </Badge>
                  ) : (
                    <>
                      <Badge
                        variant={
                          !isKnown ? "secondary" : isEnabled ? "success" : "secondary"
                        }
                      >
                        {working && (busyAction === "set_mcp_enabled" || busyAction === "set_skill_enabled")
                          ? "正在保存…"
                          : !isKnown ? "状态待确认" : isEnabled ? "已启用" : "已禁用"}
                      </Badge>
                      <Switch
                        size="sm"
                        checked={Boolean(isEnabled)}
                        disabled={busy || !canToggle(entry)}
                        aria-label={isEnabled ? `禁用 ${entry.name}` : `启用 ${entry.name}`}
                        onCheckedChange={() => onAction("toggle", entry)}
                      />
                    </>
                  )}
                </div>
              </div>

              <div title={entry.sourcePath} className={`mt-2 truncate font-mono text-[11px] text-muted select-text ${onSelect && canToggle(entry) ? "sm:ml-[76px]" : "sm:ml-12"}`}>
                {entry.sourcePath}
              </div>

              <div className={`mt-1.5 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-[11px] text-muted ${onSelect && canToggle(entry) ? "sm:ml-[76px]" : "sm:ml-12"}`}>
                <span>
                  配置：
                  <span
                    className={
                      entry.configurationStatus === "invalid"
                        ? "font-medium text-danger"
                        : "font-medium text-foreground/90"
                    }
                  >
                    {entry.configurationStatus === "valid"
                      ? "有效"
                      : entry.configurationStatus === "invalid"
                        ? "无效"
                        : "待检查"}
                  </span>
                </span>
                <span className="text-muted/40">·</span>
                <span>
                  范围：
                  <span className="text-foreground/90">
                    {entry.scope === "project"
                      ? "项目"
                      : entry.scope === "system"
                        ? "系统"
                        : entry.scope === "plugin"
                          ? "插件"
                          : "用户"}
                  </span>
                </span>
                {"version" in entry && entry.version && (
                  <>
                    <span className="text-muted/40">·</span>
                    <span>
                      版本：<span className="font-mono text-foreground/90">{entry.version}</span>
                    </span>
                  </>
                )}
                {entry.updatedAt && (
                  <>
                    <span className="text-muted/40">·</span>
                    <span>更新：{new Date(entry.updatedAt).toLocaleDateString("zh-CN")}</span>
                  </>
                )}
              </div>

              {!isBuiltin && !canToggle(entry) && !entry.reason && (
                <p className="mb-0 mt-2 text-xs text-muted">
                  {!isKnown
                    ? "无法确认启用状态，暂不可切换。"
                    : entry.configurationStatus === "invalid"
                      ? "请先修复配置。"
                      : "当前资源不允许修改状态。"}
                </p>
              )}

              {!isBuiltin && entry.reason && (
                <p className="mb-0 mt-2 text-xs text-muted leading-relaxed">
                  {entry.reason}
                </p>
              )}

              {"error" in entry && entry.error && (
                <div
                  role="alert"
                  className="mt-2.5 rounded-lg border border-red-200/70 bg-red-50/70 p-2.5 text-[11.5px] text-red-700 dark:border-red-900/60 dark:bg-red-950/40 dark:text-red-300 break-words"
                >
                  {entry.error}
                </div>
              )}

              {checkResult && isCheckExpanded(entry.id, checkResult.ok) && (
                <div
                  role={checkResult.ok ? "status" : "alert"}
                  className={`mt-2.5 rounded-lg border p-3 text-xs ${
                    checkResult.ok
                      ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-950 dark:text-emerald-200"
                      : "border-amber-500/30 bg-amber-500/10 text-amber-950 dark:text-amber-200"
                  }`}
                >
                  <div className="flex items-center justify-between gap-1.5">
                    <span className="flex items-center gap-1.5 font-medium">
                      <span
                        className={`size-1.5 rounded-full shrink-0 ${
                          checkResult.ok ? "bg-emerald-500" : "bg-amber-500"
                        }`}
                      />
                      <span>
                        {checkResult.ok ? "检查通过" : "存在异常"}：
                        {checkResult.summary}
                      </span>
                    </span>
                    <button
                      type="button"
                      className="cursor-pointer text-[11px] opacity-70 hover:opacity-100 transition-opacity"
                      onClick={() => toggleCheckExpanded(entry.id, checkResult.ok)}
                      aria-label="收起检查详情"
                    >
                      收起
                    </button>
                  </div>
                  {checkResult.revision !== revision && (
                    <p className="mt-1 mb-0 text-[11px] text-amber-700 dark:text-amber-300">
                      配置已变化，此检查结果已失效。
                    </p>
                  )}
                  {(checkResult.serverInfo?.name ||
                    checkResult.serverInfo?.version ||
                    checkResult.protocolVersion) && (
                    <p className="mt-1 mb-0 text-[11px] text-muted">
                      服务：{checkResult.serverInfo?.name ?? "未返回名称"}{" "}
                      {checkResult.serverInfo?.version ?? ""} · 协议：
                      {checkResult.protocolVersion ?? "未返回"}
                    </p>
                  )}
                  {checkResult.checks && checkResult.checks.length > 0 && (
                    <ul className="mb-0 mt-1.5 space-y-1 pl-3 text-[11px] text-muted">
                      {checkResult.checks.map((item, index) => (
                        <li key={index} className="flex items-start gap-1.5">
                          <span
                            className={`mt-1 inline-block size-1.5 shrink-0 rounded-full ${
                              item.ok ? "bg-emerald-500" : "bg-amber-500"
                            }`}
                          />
                          <span className="break-all leading-relaxed">
                            <span className="font-medium text-foreground/80">
                              {item.name}：
                            </span>
                            <span>{item.message}</span>
                          </span>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </div>

            <div className="flex flex-wrap items-center justify-between gap-2 px-4 pb-3 lg:w-56 lg:shrink-0 lg:flex-col lg:items-end lg:gap-3 lg:py-3.5">
              <div className="flex flex-wrap items-center gap-2 min-w-0 lg:justify-end">
                <Button
                  size="xs"
                  variant="outline"
                  disabled={
                    busy ||
                    entry.canCheck === false ||
                    (kind === "mcp" &&
                      entry.configurationStatus === "invalid")
                  }
                  onClick={() => onAction("check", entry)}
                >
                  <IconPlugConnected size={13} aria-hidden="true" />
                  <span>
                    {working &&
                    (busyAction === "test_mcp" || busyAction === "validate_skill")
                      ? "检查中…"
                      : kind === "mcp"
                        ? "测试连接"
                        : "检查可用性"}
                  </span>
                </Button>

                {checkResult && (
                  <button
                    type="button"
                    aria-expanded={isCheckExpanded(entry.id, checkResult.ok)}
                    onClick={() => toggleCheckExpanded(entry.id, checkResult.ok)}
                    className={`inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-xs font-medium truncate shrink-0 cursor-pointer transition-opacity hover:opacity-80 ${
                      checkResult.revision !== revision
                        ? "bg-amber-500/10 text-amber-700 dark:text-amber-300"
                        : checkResult.ok
                          ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
                          : "bg-red-500/10 text-red-700 dark:text-red-300"
                    }`}
                    title={
                      checkResult.revision !== revision
                        ? "上次检查已失效，点击查看详情"
                        : `${checkResult.ok ? "上次检查通过" : "上次检查失败"}${
                            checkResult.checkedAt
                              ? ` · ${new Date(checkResult.checkedAt).toLocaleTimeString("zh-CN", {
                                  hour: "2-digit",
                                  minute: "2-digit",
                                })}`
                              : ""
                          }（点击${isCheckExpanded(entry.id, checkResult.ok) ? "收起" : "展开"}详情）`
                    }
                  >
                    <span className="size-1.5 rounded-full bg-current shrink-0" />
                    <span className="truncate">
                      {checkResult.revision !== revision
                        ? "检查已失效"
                        : checkResult.ok
                          ? "检查通过"
                          : "检查失败"}
                      {checkResult.checkedAt &&
                        ` · ${new Date(checkResult.checkedAt).toLocaleTimeString("zh-CN", {
                          hour: "2-digit",
                          minute: "2-digit",
                        })}`}
                    </span>
                  </button>
                )}
              </div>

              <div className="flex items-center gap-1.5 shrink-0 ml-auto">
                <Button
                  size="xs"
                  variant="outline"
                  disabled={busy}
                  title={
                    entry.readOnly ||
                    entry.canEdit === false ||
                    ("ownership" in entry && !isManaged)
                      ? "查看配置"
                      : "编辑配置"
                  }
                  aria-label={
                    entry.readOnly ||
                    entry.canEdit === false ||
                    ("ownership" in entry && !isManaged)
                      ? `查看 ${entry.name}`
                      : `编辑 ${entry.name}`
                  }
                  onClick={() => onAction("edit", entry)}
                >
                  {entry.readOnly ||
                  entry.canEdit === false ||
                  ("ownership" in entry && !isManaged) ? (
                    <IconEye size={14} aria-hidden="true" />
                  ) : (
                    <IconSettings size={14} aria-hidden="true" />
                  )}
                  {entry.readOnly || entry.canEdit === false || ("ownership" in entry && !isManaged) ? "查看" : "编辑"}
                </Button>

                <Button
                  size="icon-sm"
                  variant="outline"
                  disabled={busy}
                  title={kind === "mcp" ? "导出 MCP 配置 (JSON)" : "导出 Skill"}
                  aria-label={`导出 ${entry.name}`}
                  onClick={() => onAction("export", entry)}
                >
                  <IconDownload size={14} aria-hidden="true" />
                </Button>

                {!entry.readOnly && entry.canRemove !== false && (
                  <Button
                    size="icon-sm"
                    variant="destructive-light"
                    disabled={busy}
                    title={kind === "mcp" ? "移除 MCP 服务" : "卸载 Skill"}
                    aria-label={kind === "mcp" ? `移除 ${entry.name}` : `卸载 ${entry.name}`}
                    onClick={() => onAction("remove", entry)}
                  >
                    <IconTrash size={14} aria-hidden="true" />
                  </Button>
                )}
              </div>
            </div>
          </article>
        );
      })}
    </div>
  );
}
