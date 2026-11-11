import { useState } from "react";
import {
  IconAlertCircle,
  IconBook2,
  IconChevronDown,
  IconDownload,
  IconEye,
  IconFolder,
  IconFolderCode,
  IconPlugConnected,
  IconServer,
  IconSettings,
  IconSparkles,
  IconTerminal2,
  IconTrash,
  IconWorld,
  IconX,
} from "@tabler/icons-react";
import { Badge, Button, Checkbox, Switch, Tooltip } from "../../components/ui";
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
  onClearFilters,
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
  onClearFilters?: () => void;
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
      <div className="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-[var(--codey-border-strong)] bg-[var(--codey-panel)] px-6 py-12 text-center">
        <div className="mb-3.5 flex size-12 items-center justify-center rounded-2xl bg-[var(--codey-surface-sunken)] text-muted">
          <KindIcon size={22} stroke={1.75} />
        </div>
        <h4 className="m-0 text-[16px] font-extrabold text-foreground">
          {filtered
            ? "没有符合筛选条件的结果"
            : `当前范围暂无${isMcp ? " MCP 服务" : " Skill"}`}
        </h4>
        <p className="mb-0 mt-1.5 max-w-sm text-[13px] text-muted leading-relaxed">
          {filtered
            ? "请尝试更换搜索关键词、状态标签或来源筛选条件。"
            : isMcp
              ? "点击右上角「新增 MCP」添加本地命令或远程 HTTP 服务，或切换到项目范围查看。"
              : "点击右上角「新增 Skill」编写技能或通过「导入目录 / ZIP」安装本地技能包。"}
        </p>
        {filtered && onClearFilters && (
          <div className="mt-4">
            <Button size="xs" variant="outline" onClick={onClearFilters}>
              <IconX size={13} aria-hidden="true" />
              清除筛选条件
            </Button>
          </div>
        )}
      </div>
    );
  }

  const hasSelectableRows = Boolean(onSelect) && entries.some(canToggle);
  const selectedIds = selected.length > 0 ? new Set(selected) : null;

  return (
    <div className="extension-feed extension-card-list">
      {entries.map((entry) => {
        const isBuiltin = "ownership" in entry && entry.ownership === "builtin";
        const isEnabled = entry.enabled;
        const isKnown =
          isBuiltin || !("enabledKnown" in entry) || entry.enabledKnown !== false;
        const isManaged = "ownership" in entry && entry.ownership === "managed";
        const isInvalid = entry.configurationStatus === "invalid";
        const isReadOnlyView =
          entry.readOnly ||
          entry.canEdit === false ||
          ("ownership" in entry && !isManaged);
        const checkResult = checks[entry.id];
        const working = busy && busyResourceId === entry.id;
        const isSelected = selectedIds?.has(entry.id) ?? false;
        const toggleable = canToggle(entry);

        const EntryIcon =
          "transport" in entry
            ? entry.transport === "http"
              ? IconWorld
              : entry.transport === "stdio"
                ? IconTerminal2
                : IconServer
            : isBuiltin
              ? IconSparkles
              : isManaged
                ? IconBook2
                : IconFolderCode;

        const avatarTone =
          "transport" in entry
            ? entry.transport === "http"
              ? "tone-blue"
              : entry.transport === "stdio"
                ? "tone-purple"
                : ""
            : isBuiltin
              ? "tone-purple"
              : isManaged
                ? "tone-blue"
                : "tone-amber";

        return (
          <article
            key={entry.id}
            aria-busy={working}
            className={`extension-feed-item extension-item-card${
              isSelected ? " is-selected" : ""
            }${isInvalid ? " is-invalid" : ""}${
              !isEnabled && isKnown && !isBuiltin ? " is-disabled" : ""
            }`}
          >
            <div className="flex items-start gap-3.5 min-w-0">
              {hasSelectableRows && (
                toggleable ? (
                  <div className="mt-2.5 shrink-0">
                    <Checkbox
                      aria-label={`选择 ${entry.name}`}
                      checked={isSelected}
                      disabled={busy || (selected.length >= 100 && !isSelected)}
                      onCheckedChange={() => onSelect?.(entry.id)}
                    />
                  </div>
                ) : (
                  <div className="mt-2.5 w-4 shrink-0" aria-hidden="true" />
                )
              )}

              <div
                className={`extension-item-avatar ${
                  isEnabled && isKnown ? `is-active ${avatarTone}` : ""
                }`}
              >
                <EntryIcon size={19} stroke={1.8} aria-hidden="true" />
                <span
                  className={`extension-item-avatar-dot ${
                    isInvalid
                      ? "is-invalid"
                      : !isKnown
                        ? "is-unknown"
                        : isEnabled
                          ? "is-on"
                          : ""
                  }`}
                  aria-hidden="true"
                />
              </div>

              <div className="min-w-0 flex-1">
                {/* 顶部主信息行：名称 + 语义标签 | 状态与启用开关 */}
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <h4
                        title={entry.name}
                        className="m-0 max-w-full truncate text-[15px] font-bold text-foreground"
                      >
                        <button
                          type="button"
                          disabled={busy}
                          onClick={() => onAction("edit", entry)}
                          className="extension-item-title-btn truncate"
                        >
                          {entry.name}
                        </button>
                      </h4>

                      {entry.id !== entry.name && (
                        <span
                          className="extension-tag is-mono"
                          title={`唯一标识: ${entry.id}`}
                        >
                          {entry.id}
                        </span>
                      )}

                      {"transport" in entry && (
                        <span
                          className={`extension-tag is-mono ${
                            entry.transport === "http"
                              ? "is-blue"
                              : entry.transport === "stdio"
                                ? "is-purple"
                                : ""
                          }`}
                        >
                          {entry.transport === "http"
                            ? "HTTP"
                            : entry.transport === "stdio"
                              ? "STDIO"
                              : "未知类型"}
                        </span>
                      )}

                      {"version" in entry && entry.version && (
                        <span className="extension-tag is-mono">
                          v{entry.version}
                        </span>
                      )}

                      {"ownership" in entry && entry.ownership !== "builtin" && (
                        <span
                          className={`extension-tag ${
                            entry.ownership === "managed"
                              ? "is-blue"
                              : entry.ownership === "external"
                                ? "is-amber"
                                : ""
                          }`}
                        >
                          {
                            {
                              managed: "Codey 托管",
                              external: "外部安装",
                              plugin: "插件缓存",
                            }[entry.ownership]
                          }
                        </span>
                      )}

                      {!isBuiltin && entry.readOnly && (
                        <Badge
                          variant="outline"
                          className="px-1.5 py-0 text-[10px]"
                        >
                          只读
                        </Badge>
                      )}

                      {isInvalid && (
                        <Badge
                          variant="destructive"
                          className="px-1.5 py-0 text-[10px]"
                        >
                          配置无效
                        </Badge>
                      )}
                    </div>

                    {"transport" in entry && (
                      <p
                        title={`${entry.transport} · ${entry.summary}`}
                        className="mb-0 mt-1 truncate text-[13px] text-muted"
                      >
                        {entry.summary || (entry.transport === "http" ? "远程 HTTP 连接" : "本地进程启动")}
                      </p>
                    )}

                    {"description" in entry && (
                      <p
                        title={entry.description}
                        className="mb-0 mt-1 line-clamp-2 text-[13px] text-muted leading-relaxed"
                      >
                        {entry.description || "暂无技能描述"}
                      </p>
                    )}
                  </div>

                  <div className="flex shrink-0 items-center justify-end gap-2">
                    {isBuiltin ? (
                      <Badge
                        variant="outline"
                        className="border-default font-medium text-muted"
                      >
                        系统内置
                      </Badge>
                    ) : (
                      <>
                        <Badge
                          variant={
                            !isKnown
                              ? "secondary"
                              : isEnabled
                                ? "success"
                                : "secondary"
                          }
                        >
                          {working &&
                          (busyAction === "set_mcp_enabled" ||
                            busyAction === "set_skill_enabled")
                            ? "正在保存…"
                            : !isKnown
                              ? "状态待确认"
                              : isEnabled
                                ? "已启用"
                                : "已禁用"}
                        </Badge>
                        {!toggleable ? (
                          <Tooltip
                            content={
                              entry.reason ||
                              (!isKnown
                                ? "无法确认启用状态，暂不可切换"
                                : isInvalid
                                  ? "请先修复配置"
                                  : "当前资源不允许修改状态")
                            }
                          >
                            <span className="inline-flex">
                              <Switch
                                size="sm"
                                checked={Boolean(isEnabled)}
                                disabled
                                aria-label={
                                  isEnabled ? `禁用 ${entry.name}` : `启用 ${entry.name}`
                                }
                                onCheckedChange={() => onAction("toggle", entry)}
                              />
                            </span>
                          </Tooltip>
                        ) : (
                          <Switch
                            size="sm"
                            checked={Boolean(isEnabled)}
                            disabled={busy}
                            aria-label={
                              isEnabled ? `禁用 ${entry.name}` : `启用 ${entry.name}`
                            }
                            onCheckedChange={() => onAction("toggle", entry)}
                          />
                        )}
                      </>
                    )}
                  </div>
                </div>

                {/* 状态受限原因与异常提示 */}
                {!isBuiltin && !canToggle(entry) && !entry.reason && (
                  <p className="mb-0 mt-2 flex items-center gap-1.5 text-xs text-muted">
                    <IconAlertCircle size={13} className="shrink-0 opacity-75" aria-hidden="true" />
                    <span>
                      {!isKnown
                        ? "无法确认启用状态，暂不可切换。"
                        : isInvalid
                          ? "请先修复配置。"
                          : "当前资源不允许修改状态。"}
                    </span>
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
                    className="mt-2.5 flex items-start gap-2 rounded-xl border border-[var(--codey-red-border)] bg-[var(--codey-red-soft)] px-3 py-2 text-[12px] text-[var(--codey-red)] break-words"
                  >
                    <IconAlertCircle size={14} className="mt-0.5 shrink-0" aria-hidden="true" />
                    <span className="min-w-0 flex-1">{entry.error}</span>
                  </div>
                )}

                {/* 展开的检查详情面板 */}
                {checkResult && isCheckExpanded(entry.id, checkResult.ok) && (
                  <div
                    id={`check-panel-${entry.id}`}
                    role={checkResult.ok ? "status" : "alert"}
                    className={`mt-2.5 rounded-xl border p-3 text-xs ${
                      checkResult.ok
                        ? "border-[var(--codey-green-border)] bg-[var(--codey-green-soft)] text-foreground"
                        : "border-[var(--codey-amber-border)] bg-[var(--codey-amber-soft)] text-foreground"
                    }`}
                  >
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <span className="flex items-center gap-1.5 font-semibold">
                        <span
                          className={`size-2 rounded-full shrink-0 ${
                            checkResult.ok ? "bg-[var(--codey-green)]" : "bg-[var(--codey-amber)]"
                          }`}
                        />
                        <span>
                          {checkResult.ok ? "检查通过" : "存在异常"}：
                          {checkResult.summary}
                        </span>
                      </span>
                      <button
                        type="button"
                        className="cursor-pointer rounded px-1.5 py-0.5 text-[11px] font-medium opacity-75 transition-opacity hover:bg-black/5 hover:opacity-100 dark:hover:bg-white/10"
                        onClick={() => toggleCheckExpanded(entry.id, checkResult.ok)}
                        aria-label="收起检查详情"
                      >
                        收起
                      </button>
                    </div>
                    {checkResult.revision !== revision && (
                      <p className="mt-1.5 mb-0 text-[11.5px] font-medium text-[var(--codey-amber)]">
                        配置已变化，此检查结果已失效。
                      </p>
                    )}
                    {(checkResult.serverInfo?.name ||
                      checkResult.serverInfo?.version ||
                      checkResult.protocolVersion) && (
                      <div className="mt-1.5 flex flex-wrap items-center gap-2 text-[11.5px] text-muted">
                        <span>
                          服务：
                          <strong className="font-mono font-medium text-foreground/85">
                            {checkResult.serverInfo?.name ?? "未返回名称"}{" "}
                            {checkResult.serverInfo?.version ?? ""}
                          </strong>
                        </span>
                        <span className="text-muted/40">·</span>
                        <span>
                          协议：
                          <strong className="font-mono font-medium text-foreground/85">
                            {checkResult.protocolVersion ?? "未返回"}
                          </strong>
                        </span>
                      </div>
                    )}
                    {checkResult.checks && checkResult.checks.length > 0 && (
                      <ul className="mb-0 mt-2 space-y-1 border-t border-current/10 pt-2 pl-0 list-none text-[11.5px] text-muted">
                        {checkResult.checks.map((item, index) => (
                          <li key={index} className="flex items-start gap-2">
                            <span
                              className={`mt-1.5 inline-block size-1.5 shrink-0 rounded-full ${
                                item.ok ? "bg-emerald-500" : "bg-amber-500"
                              }`}
                            />
                            <span className="break-all leading-relaxed">
                              <span className="font-semibold text-foreground/85">
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

                {/* 底部技术元数据与操作工具栏 */}
                <div className="extension-item-footer">
                  <div className="extension-meta-list">
                    <span
                      title={entry.sourcePath}
                      className="extension-path-pill select-text"
                    >
                      <IconFolder size={12} className="shrink-0 opacity-70" aria-hidden="true" />
                      <span className="truncate">{entry.sourcePath}</span>
                    </span>

                    <span>
                      范围：
                      <span className="font-medium text-foreground/85">
                        {entry.scope === "project"
                          ? "项目"
                          : entry.scope === "system"
                            ? "系统"
                            : entry.scope === "plugin"
                              ? "插件"
                              : "用户"}
                      </span>
                    </span>

                    <span className="text-muted/35" aria-hidden="true">·</span>

                    <span>
                      配置：
                      <span
                        className={
                          isInvalid
                            ? "font-semibold text-danger"
                            : "font-medium text-foreground/85"
                        }
                      >
                        {entry.configurationStatus === "valid"
                          ? "有效"
                          : isInvalid
                            ? "无效"
                            : "待检查"}
                      </span>
                    </span>

                    {"dependencies" in entry &&
                      entry.dependencies &&
                      entry.dependencies.length > 0 && (
                        <>
                          <span className="text-muted/35" aria-hidden="true">·</span>
                          <span
                            title={`声明依赖: ${entry.dependencies
                              .map((d) => `${d.type}:${d.name}`)
                              .join(", ")}`}
                          >
                            依赖{" "}
                            <span className="font-medium text-foreground/85">
                              {entry.dependencies.length}
                            </span>{" "}
                            项
                          </span>
                        </>
                      )}

                    {entry.updatedAt && (
                      <>
                        <span className="text-muted/35" aria-hidden="true">·</span>
                        <span className="tabular-nums">
                          更新：{new Date(entry.updatedAt).toLocaleDateString("zh-CN")}
                        </span>
                      </>
                    )}
                  </div>

                  <div className="extension-item-actions">
                    {checkResult && (
                      <button
                        type="button"
                        aria-expanded={isCheckExpanded(entry.id, checkResult.ok)}
                        aria-controls={`check-panel-${entry.id}`}
                        onClick={() => toggleCheckExpanded(entry.id, checkResult.ok)}
                        className={`inline-flex items-center gap-1 rounded-full px-2.5 py-1 text-[11.5px] font-semibold truncate shrink-0 cursor-pointer transition-opacity hover:opacity-85 ${
                          checkResult.revision !== revision
                            ? "bg-[var(--codey-amber-soft)] text-[var(--codey-amber)]"
                            : checkResult.ok
                              ? "bg-[var(--codey-green-soft)] text-[var(--codey-green)]"
                              : "bg-[var(--codey-red-soft)] text-[var(--codey-red)]"
                        }`}
                        title={
                          checkResult.revision !== revision
                            ? "上次检查已失效，点击查看详情"
                            : `${checkResult.ok ? "上次检查通过" : "上次检查失败"}${
                                checkResult.checkedAt
                                  ? ` · ${new Date(
                                      checkResult.checkedAt,
                                    ).toLocaleTimeString("zh-CN", {
                                      hour: "2-digit",
                                      minute: "2-digit",
                                    })}`
                                  : ""
                              }（点击${
                                isCheckExpanded(entry.id, checkResult.ok)
                                  ? "收起"
                                  : "展开"
                              }详情）`
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
                            ` · ${new Date(
                              checkResult.checkedAt,
                            ).toLocaleTimeString("zh-CN", {
                              hour: "2-digit",
                              minute: "2-digit",
                            })}`}
                        </span>
                        <IconChevronDown
                          size={12}
                          className={`shrink-0 transition-transform ${
                            isCheckExpanded(entry.id, checkResult.ok)
                              ? "rotate-180"
                              : ""
                          }`}
                          aria-hidden="true"
                        />
                      </button>
                    )}

                    <Button
                      size="xs"
                      variant="outline"
                      disabled={
                        busy ||
                        entry.canCheck === false ||
                        (kind === "mcp" && isInvalid)
                      }
                      onClick={() => onAction("check", entry)}
                    >
                      <IconPlugConnected size={13} aria-hidden="true" />
                      <span>
                        {working &&
                        (busyAction === "test_mcp" ||
                          busyAction === "validate_skill")
                          ? "检查中…"
                          : kind === "mcp"
                            ? "测试连接"
                            : "检查可用性"}
                      </span>
                    </Button>

                    <Button
                      size="xs"
                      variant="ghost"
                      disabled={busy}
                      title={isReadOnlyView ? "查看配置" : "编辑配置"}
                      aria-label={
                        isReadOnlyView
                          ? `查看 ${entry.name}`
                          : `编辑 ${entry.name}`
                      }
                      onClick={() => onAction("edit", entry)}
                    >
                      {isReadOnlyView ? (
                        <IconEye size={14} aria-hidden="true" />
                      ) : (
                        <IconSettings size={14} aria-hidden="true" />
                      )}
                      {isReadOnlyView ? "查看" : "编辑"}
                    </Button>

                    <Button
                      size="icon-sm"
                      variant="ghost"
                      disabled={busy}
                      title={
                        kind === "mcp" ? "导出 MCP 配置 (JSON)" : "导出 Skill"
                      }
                      aria-label={`导出 ${entry.name}`}
                      onClick={() => onAction("export", entry)}
                    >
                      <IconDownload size={14} aria-hidden="true" />
                    </Button>

                    {!entry.readOnly && entry.canRemove !== false && (
                      <Button
                        size="icon-sm"
                        variant="ghost"
                        className="text-muted hover:bg-[var(--codey-red-soft)] hover:text-danger"
                        disabled={busy}
                        title={kind === "mcp" ? "移除 MCP 服务" : "卸载 Skill"}
                        aria-label={
                          kind === "mcp"
                            ? `移除 ${entry.name}`
                            : `卸载 ${entry.name}`
                        }
                        onClick={() => onAction("remove", entry)}
                      >
                        <IconTrash size={14} aria-hidden="true" />
                      </Button>
                    )}
                  </div>
                </div>
              </div>
            </div>
          </article>
        );
      })}
    </div>
  );
}
