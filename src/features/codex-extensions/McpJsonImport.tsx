import { useEffect, useRef, useState } from "react";
import { Select } from "../../components/ui";
import { IconCheck, IconFileCode, IconFolderOpen } from "@tabler/icons-react";
import type { EditorDraft } from "./types";
import { parseMcpJson, updateMcpJsonDraft } from "./mcpJson";
import { ExtensionError } from "./ExtensionError";

export function McpJsonImport({
  draft,
  busy,
  onChange,
  onReadingChange,
  validationErrorId,
}: {
  draft: EditorDraft;
  busy: boolean;
  onChange: (draft: EditorDraft) => void;
  onReadingChange: (reading: boolean) => void;
  validationErrorId?: string;
}) {
  const sequence = useRef(0);
  const current = useRef(draft);
  current.current = draft;
  const [error, setError] = useState("");
  const [importedFile, setImportedFile] = useState("");
  const [reading, setLocalReading] = useState(false);
  const lineCount = (draft.content.match(/\n/g)?.length ?? 0) + 1;
  const setReading = (value: boolean) => {
    setLocalReading(value);
    onReadingChange(value);
  };
  useEffect(
    () => () => {
      sequence.current++;
    },
    [],
  );
  let services: ReturnType<typeof parseMcpJson> = [];
  try {
    services = parseMcpJson(draft.content);
  } catch {
    /* 主编辑器展示校验提示。 */
  }
  return (
    <div className="space-y-2.5">
      <div className="overflow-hidden rounded-xl border border-black/[0.12] bg-black/[0.015] transition-colors focus-within:border-accent focus-within:ring-2 focus-within:ring-accent/20 dark:border-white/[0.12] dark:bg-black/15">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-black/[0.08] bg-black/[0.025] px-3.5 py-2.5 dark:border-white/[0.08] dark:bg-white/[0.03]">
          <div className="flex items-center gap-2">
            <IconFileCode size={16} className="text-accent" aria-hidden="true" />
            <span className="text-xs font-semibold text-foreground">服务配置 JSON</span>
          </div>
          {!draft.readOnly && (
            <div className="relative">
              <input
                id="mcp-json-file-input"
                aria-label="导入 JSON 文件"
                type="file"
                accept=".json,application/json"
                disabled={busy}
                className="peer sr-only"
                onChange={async (event) => {
                  const file = event.target.files?.[0];
                  event.target.value = "";
                  if (!file) return;
                  const request = ++sequence.current;
                  setError("");
                  setImportedFile("");
                  setReading(true);
                  try {
                    if (!file.name.toLowerCase().endsWith(".json"))
                      throw new Error("请选择 .json 文件。");
                    if (file.size > 1024 * 1024)
                      throw new Error("JSON 文件不能超过 1 MB。");
                    const content = await file.text();
                    if (request !== sequence.current) return;
                    parseMcpJson(content);
                    onChange(updateMcpJsonDraft(current.current, content));
                    setImportedFile(file.name);
                  } catch (cause) {
                    if (request === sequence.current)
                      setError(
                        cause instanceof Error
                          ? cause.message
                          : "文件读取失败，请重试。",
                      );
                  } finally {
                    if (request === sequence.current) setReading(false);
                  }
                }}
              />
              <label
                htmlFor="mcp-json-file-input"
                className="inline-flex cursor-pointer items-center gap-1.5 rounded-lg border border-black/[0.12] bg-[var(--codey-surface,#fff)] px-2.5 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-black/[0.05] peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-accent peer-disabled:cursor-not-allowed peer-disabled:opacity-50 dark:border-white/[0.15] dark:bg-white/[0.04] dark:hover:bg-white/[0.08]"
              >
                <IconFolderOpen size={14} className="text-muted" />
                <span>导入 .json</span>
              </label>
            </div>
          )}
        </div>
        <textarea
          aria-label="服务配置 JSON"
          aria-invalid={!!validationErrorId}
          aria-describedby={validationErrorId}
          spellCheck={false}
          readOnly={draft.readOnly}
          disabled={busy}
          className="block min-h-[200px] w-full resize-y bg-transparent px-4 py-3.5 font-mono text-[13px] leading-6 text-foreground outline-none disabled:opacity-60"
          value={draft.content}
          onChange={(event) => {
            sequence.current++;
            setReading(false);
            setError("");
            onChange(updateMcpJsonDraft(draft, event.target.value));
          }}
        />
        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-black/[0.06] px-3.5 py-2 text-[11px] text-muted dark:border-white/[0.08]">
          <span>粘贴配置或导入文件 · 上限 1 MiB</span>
          <span className="font-mono tabular-nums">{lineCount} 行 · {draft.content.length} 字符</span>
        </div>
      </div>
      {reading && <p role="status" className="m-0 text-xs text-muted">正在读取 JSON 文件…</p>}
      {importedFile && !reading && (
        <p role="status" className="m-0 flex items-start gap-1.5 text-xs text-muted">
          <IconCheck size={14} className="mt-0.5 shrink-0 text-success" aria-hidden="true" />
          <span className="min-w-0 break-all">已载入：{importedFile}</span>
        </p>
      )}
      {error && <ExtensionError message={error} draftPreserved />}
      {services.length > 1 && (
        <label className="block space-y-2 rounded-xl border border-black/[0.08] bg-black/[0.02] p-3.5 text-xs dark:border-white/[0.1] dark:bg-white/[0.03]">
          <span>本次导入的服务（共 {services.length} 个，一次保存一个）</span>
          <Select
            aria-label="选择导入服务"
            value={draft.jsonService ?? ""}
            disabled={busy || draft.readOnly}
            placeholder="请选择一个服务"
            className="w-full"
            optionList={services.map((service) => ({
              value: service.name,
              label: service.name,
            }))}
            onChange={(value) => {
              sequence.current++;
              setReading(false);
              const name = String(value ?? "");
              onChange({
                ...draft,
                jsonService: name,
                id:
                  !draft.isNew || name === draft.jsonService ? draft.id : name,
              });
            }}
          />
        </label>
      )}
    </div>
  );
}
