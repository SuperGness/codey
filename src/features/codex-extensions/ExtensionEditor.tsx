import { Button, Input } from "../../components/ui";
import { useId, useState, type ReactNode } from "react";
import {
  IconAlertCircle,
  IconCheck,
  IconDownload,
  IconFileCode,
  IconFolderOpen,
  IconInfoCircle,
  IconRefresh,
  IconShieldCheck,
} from "@tabler/icons-react";
import type { EditorDraft, Inventory } from "./types";
import { editorError, draftChanged } from "./state";
import { ExtensionError } from "./ExtensionError";
import { ResourceInfo } from "./ResourceInfo";
import { McpJsonImport } from "./McpJsonImport";
import { mcpJsonError } from "./mcpJson";

export function ExtensionEditor({
  draft,
  busy,
  submissionError,
  onChange,
  onSave,
  onCancel,
  onPick,
  inventory,
  onReload,
  saving,
  previewing = false,
  onPreview,
  children,
}: {
  draft: EditorDraft;
  busy: boolean;
  submissionError?: string;
  onChange: (draft: EditorDraft) => void;
  onSave: () => void;
  onCancel: () => void;
  onPick: () => void;
  inventory?: Inventory | null;
  onReload?: () => void;
  saving?: boolean;
  previewing?: boolean;
  onPreview?: () => void;
  children?: ReactNode;
}) {
  const idError = draft.kind === "mcp" && (
    !/^[A-Za-z0-9_-]{1,128}$/.test(draft.id) ||
    (draft.isNew && inventory?.mcps.some((entry) => entry.id === draft.id))
  );
  const error =
    draft.kind === "mcp" &&
    draft.isNew &&
    inventory?.mcps.some((entry) => entry.id === draft.id)
      ? "此服务标识已存在，请使用其他标识；如需修改，请从列表打开原有服务。"
      : (idError ? editorError(draft) : mcpJsonError(draft) || editorError(draft));
  const [jsonReading, setJsonReading] = useState(false);
  const fieldId = useId();
  const errorId = `${fieldId}-error`;
  const hintId = `${fieldId}-hint`;
  const entry = draft.entry;
  const lineCount = (draft.content.match(/\n/g)?.length ?? 0) + 1;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden text-foreground">
      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto overscroll-contain pt-1">
      {entry && <ResourceInfo entry={entry} inventory={inventory} />}

      {draft.kind === "install" && (
        <div className="flex items-start gap-3 rounded-xl border border-accent/15 bg-accent/5 px-3.5 py-3 text-xs leading-relaxed">
          <IconShieldCheck size={18} className="mt-0.5 shrink-0 text-accent" aria-hidden="true" />
          <div className="space-y-1">
            <p className="m-0 font-medium">仅安装可信来源</p>
            <p className="m-0 text-muted">从本地目录或 ZIP 复制，保留来源且不执行外部脚本。安装后默认禁用，确认可信后再启用。</p>
          </div>
        </div>
      )}

      {draft.kind === "mcp" && (
        <div className="flex items-start gap-2.5 rounded-xl bg-black/[0.025] px-3.5 py-3 text-xs leading-relaxed text-muted dark:bg-white/[0.04]">
          <IconInfoCircle size={16} className="mt-0.5 shrink-0 text-accent" aria-hidden="true" />
          <div className="space-y-1">
            <div>
              {draft.isNew
                ? "支持 mcpServers、mcp_servers 和单个服务配置。新服务保存后自动启用并刷新 Codex；请仅保存可信服务。"
                : "服务标识保持不变，保留脱敏占位符即可沿用原值。保存后自动刷新 Codex，无需重启。"}
            </div>
            {!draft.isNew && (
              <p className="mb-0 mt-1 text-xs text-muted">
                导出会隐藏凭证和未知字段。迁移到其他环境时，请补齐这些配置；导入只填充草稿。
              </p>
            )}
          </div>
        </div>
      )}

      {draft.kind === "mcp" && (
        <label className="block space-y-2 text-xs font-medium text-foreground">
          <span className="flex flex-wrap items-center justify-between gap-2">
            <span>服务标识</span>
            <span className="font-normal text-muted">{draft.isNew ? "字母、数字、下划线或短横线" : "创建后不可修改"}</span>
          </span>
          <Input
            aria-label="服务标识"
            aria-invalid={!!idError}
            aria-describedby={idError ? errorId : undefined}
            value={draft.id}
            disabled={busy || jsonReading || !draft.isNew}
            onChange={(event) => onChange({ ...draft, id: event.target.value })}
          />
        </label>
      )}

      {draft.kind === "mcp" ? (
        <McpJsonImport
          draft={draft}
          busy={busy}
          onChange={onChange}
          onReadingChange={setJsonReading}
          validationErrorId={error && !idError ? errorId : undefined}
        />
      ) : draft.kind === "install" ? (
        <div className="space-y-3">
          <label htmlFor={fieldId} className="block text-xs font-medium text-foreground">
            Skill 来源路径
          </label>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input
              aria-label="Skill 来源绝对路径"
              id={fieldId}
              aria-invalid={!!error && !!draft.content.trim()}
              aria-describedby={error && draft.content.trim() ? errorId : hintId}
              placeholder="输入或粘贴目录或 .zip 文件的绝对路径"
              value={draft.content}
              disabled={busy && !previewing}
              leftSection={<IconFolderOpen size={16} className="text-muted" />}
              onChange={(event) =>
                onChange({ ...draft, content: event.target.value })
              }
              className="min-w-0 flex-1 text-xs"
            />
            <Button
              variant="outline"
              disabled={busy}
              onClick={onPick}
              className="shrink-0 gap-1.5"
            >
              <IconFolderOpen size={15} />
              <span>选择目录</span>
            </Button>
          </div>
          <p id={hintId} className="m-0 text-xs leading-relaxed text-muted">
            支持包含 SKILL.md 的本地目录，或打包好的 .zip 归档文件。
          </p>
          {previewing && <p role="status" className="m-0 text-xs text-muted">正在读取来源并校验文件；修改路径后需重新预览。</p>}
          {draft.installPreview && (
            <section aria-label="Skill 安装预览" className="overflow-hidden rounded-xl border border-black/[0.1] text-xs dark:border-white/[0.12]">
              <div className="space-y-3 bg-black/[0.02] p-4 dark:bg-white/[0.03]">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 space-y-1.5">
                    <h4 className="m-0 break-words text-sm font-semibold">{draft.installPreview.name}</h4>
                    <p className="m-0 break-words leading-relaxed text-muted">{draft.installPreview.description}</p>
                  </div>
                  <Button size="xs" variant="outline" className="shrink-0 gap-1.5" disabled={busy} onClick={onPreview}>
                    <IconRefresh size={13} aria-hidden="true" />重新预览
                  </Button>
                </div>
                <div className="flex flex-wrap items-center gap-2 text-muted">
                  <span className="rounded-md border border-black/[0.08] bg-[var(--codey-surface,#fff)] px-2 py-1 dark:border-white/[0.1] dark:bg-white/[0.04]">{draft.installPreview.sourceKind === "zip" ? "ZIP 归档" : "本地目录"}</span>
                  <span className="tabular-nums">共 {draft.installPreview.fileCount} 个文件 · {formatBytes(draft.installPreview.totalBytes)}</span>
                </div>
                <p className="m-0 break-all font-mono text-[11px] leading-relaxed text-muted">{draft.installPreview.sourcePath}</p>
              </div>
              <div className="flex justify-between border-y border-black/[0.06] px-4 py-2 text-[11px] font-medium text-muted dark:border-white/[0.08]"><span>待安装文件</span><span>大小</span></div>
              <ul aria-label="待安装文件" className="m-0 max-h-44 list-none overflow-y-auto px-4 py-1.5">
                {draft.installPreview.files.slice(0, 100).map((file) => (
                  <li key={file.path} className="flex items-start justify-between gap-3 py-1.5">
                    <span className="min-w-0 break-all font-mono leading-relaxed">{file.path}{file.executable && <span className="ml-2 font-sans text-warning">可执行文件</span>}</span>
                    <span className="shrink-0 font-mono tabular-nums leading-relaxed text-muted">{formatBytes(file.size)}</span>
                  </li>
                ))}
              </ul>
              {draft.installPreview.fileCount > Math.min(100, draft.installPreview.files.length) && (
                <p className="m-0 px-4 pb-3 text-muted">仅显示前 {Math.min(100, draft.installPreview.files.length)} 项，安装时会校验全部文件。</p>
              )}
              <p role="status" className="m-0 border-t border-black/[0.06] px-4 py-3 leading-relaxed text-muted dark:border-white/[0.08]">预览未写入文件。确认信任来源后安装，安装完成后默认禁用。</p>
            </section>
          )}
        </div>
      ) : (
        <div className="space-y-2">
          <div className="overflow-hidden rounded-xl border border-black/[0.12] bg-black/[0.015] transition-colors focus-within:border-accent focus-within:ring-2 focus-within:ring-accent/20 dark:border-white/[0.12] dark:bg-black/15">
            <div className="flex flex-wrap items-center justify-between gap-2 border-b border-black/[0.08] bg-black/[0.025] px-3.5 py-2.5 dark:border-white/[0.08] dark:bg-white/[0.03]">
              <div className="flex items-center gap-2">
                <IconFileCode size={16} className="text-blue-600 dark:text-blue-400" />
                <span className="font-mono text-xs font-semibold text-foreground">
                  SKILL.md
                </span>
                <span className="rounded bg-black/[0.05] px-1.5 py-0.5 font-mono text-[10px] text-muted dark:bg-white/[0.08]">
                  Markdown / YAML
                </span>
                {draft.readOnly && (
                  <span className="rounded bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400">
                    只读
                  </span>
                )}
              </div>
            </div>
            <textarea
              aria-label="SKILL.md"
              aria-invalid={!draft.readOnly && !!error}
              aria-describedby={error && !draft.readOnly ? `${errorId} ${hintId}` : !draft.readOnly ? hintId : undefined}
              className="block min-h-[280px] w-full resize-y bg-transparent px-4 py-3.5 font-mono text-[13px] leading-6 text-foreground outline-none disabled:opacity-60"
              spellCheck={false}
              value={draft.content}
              disabled={busy}
              readOnly={draft.readOnly}
              onChange={(event) =>
                onChange({ ...draft, content: event.target.value })
              }
              placeholder="编写 SKILL.md 内容..."
            />
            <div className="flex flex-wrap items-center justify-between gap-2 border-t border-black/[0.06] px-3.5 py-2 text-[11px] text-muted dark:border-white/[0.08]">
              <span>{draft.readOnly ? "仅查看内容" : "Markdown 正文与 YAML 头部"}</span>
              <span className="font-mono tabular-nums">{lineCount} 行 · {draft.content.length} 字符</span>
            </div>
          </div>
          {!draft.readOnly ? (
            <p id={hintId} className="m-0 text-xs leading-relaxed text-muted">
              头部必须包含 <code>name</code> 与 <code>description</code> 声明，文件上限 4 MiB。
              {draft.isNew ? "保存时按 name 创建安装目录，默认禁用。" : "仅 Codey 托管的 Skill 可编辑；修改 name 不会重命名安装目录。"}
            </p>
          ) : null}
        </div>
      )}

      {error && !draft.readOnly && (draft.kind !== "install" || !!draft.content.trim()) && (
        <div
          role="status"
          id={errorId}
          className="flex items-start gap-2 rounded-xl border border-warning/25 bg-warning/8 px-3.5 py-3 text-xs leading-relaxed text-warning"
        >
          <IconAlertCircle size={16} className="mt-0.5 shrink-0" />
          <span className="min-w-0 break-words">{error}</span>
        </div>
      )}

      {submissionError && (
        <ExtensionError message={submissionError} draftPreserved />
      )}

      {!draft.readOnly &&
        ((!draft.isNew && draft.kind !== "install") || !!submissionError) && (
          <Button
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={onReload}
          >
            {draft.isNew || draft.kind === "install"
              ? "重新读取范围，保留新建草稿"
              : "加载最新内容，与保留的草稿比对"}
          </Button>
        )}

        {children}
      </div>
      <div className="mt-4 flex shrink-0 flex-wrap items-center justify-end gap-2.5 border-t border-black/[0.08] pt-4 dark:border-white/[0.1]">
        <Button variant="outline" disabled={busy} onClick={onCancel}>
          {draft.readOnly ? "返回列表" : "取消"}
        </Button>
        {!draft.readOnly && (
          <Button
            loading={saving}
            disabled={
              busy ||
              jsonReading ||
              !!error ||
              (draft.kind !== "mcp" && !draft.isNew && !draftChanged(draft))
            }
            onClick={onSave}
            className="gap-1.5"
          >
            {draft.kind === "install" ? (
              <>
                <IconDownload size={15} />
                <span>{previewing ? "正在预览…" : draft.installPreview ? "确认安装（默认禁用）" : "预览来源"}</span>
              </>
            ) : (
              <>
                <IconCheck size={15} />
                <span>校验并保存</span>
              </>
            )}
          </Button>
        )}
      </div>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}
