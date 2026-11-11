import { useEffect, useState } from "react";
import { IconAlertTriangle, IconFilePlus, IconFolderOpen, IconPuzzle, IconRefresh } from "@tabler/icons-react";
import { cn } from "@heroui/react";
import { Button, Input } from "./components/ui";
import type { CodeyPlugin, CodeyPluginPreview } from "./codeyPlugins";

function packageFileName(path: string) {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] || path;
}

/** 插件页顶部的常驻导入面板：选择或拖入插件包后原地展开预览，确认后安装。 */
export function PluginImportPanel({
  platform,
  busy,
  disabled = false,
  preview,
  upgrading,
  error,
  step = "",
  dragActive = false,
  fileName = "",
  onRetry,
  onCancel,
  onSelectFile,
  onInspectPath,
  onClearPreview,
  onConfirm,
}: {
  platform?: string;
  busy: boolean;
  disabled?: boolean;
  preview: CodeyPluginPreview | null;
  upgrading?: CodeyPlugin;
  error: string;
  step?: "checking" | "installing" | "";
  dragActive?: boolean;
  fileName?: string;
  onRetry?: () => void;
  onCancel: () => void;
  onSelectFile: () => void;
  onInspectPath: (path: string) => void;
  onClearPreview: () => void;
  onConfirm: () => void;
}) {
  const nativePicker = platform !== "linux";
  const [path, setPath] = useState("");
  useEffect(() => {
    if (preview?.path) setPath(preview.path);
  }, [preview?.path]);

  const duplicate = Boolean(preview && upgrading?.version === preview.manifest.version);
  const capabilities = [
    ...(preview?.manifest.capabilities ?? []),
    ...(preview?.manifest.permissions ?? []),
  ];
  const engaged = Boolean(preview || error || fileName);
  const locked = busy || disabled;

  const title = step === "installing"
    ? "正在导入插件…"
    : step === "checking"
      ? "正在读取并检查插件包…"
      : dragActive
        ? "松开文件，检查并导入插件包"
        : fileName || (preview ? packageFileName(preview.path) : "拖入 .codey-plugin 插件包，或选择文件");
  const hint = fileName
    ? "已拖入本地插件包"
    : preview
      ? preview.path
      : "一次一个文件 · 最大 64 MiB · 导入前先检查，新安装的插件默认停用";

  return (
    <div
      role="region"
      aria-label="插件文件拖放区域"
      aria-busy={Boolean(step)}
      data-codey-plugin-drop-zone=""
      className={cn(
        "plugin-import-panel",
        engaged && "is-engaged",
        dragActive && "is-drag-active ring-2 ring-[var(--codey-blue-soft)]",
      )}
    >
      <div className="plugin-import-head">
        <span className="plugin-import-icon" aria-hidden="true">
          {step ? <IconRefresh size={19} className="animate-spin" /> : <IconFilePlus size={19} />}
        </span>
        <div className="min-w-0 flex-1" role="status" aria-live="polite">
          <div className="plugin-import-title">{title}</div>
          <p className={cn("plugin-import-hint", preview && !fileName && "font-mono")}>{hint}</p>
        </div>
        {nativePicker && (
          <Button
            size="sm"
            variant={engaged ? "outline" : "default"}
            disabled={locked}
            onClick={onSelectFile}
          >
            <IconFolderOpen size={14} aria-hidden="true" />
            <span>{engaged ? "重新选择" : "选择文件"}</span>
          </Button>
        )}
      </div>

      {!nativePicker && (
        <div className="plugin-import-path">
          <Input
            aria-label="插件包路径"
            placeholder=".codey-plugin 文件的完整路径"
            value={path}
            disabled={locked}
            onChange={(event) => {
              setPath(event.target.value);
              if (engaged) onClearPreview();
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && path.trim() && !locked) {
                event.preventDefault();
                onInspectPath(path.trim());
              }
            }}
            className="flex-1"
          />
          <Button
            size="sm"
            variant="outline"
            disabled={locked || !path.trim()}
            onClick={() => onInspectPath(path.trim())}
          >
            检查安装包
          </Button>
        </div>
      )}

      {preview ? (
        <div className="plugin-import-preview">
          <div className="flex items-center gap-3">
            <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-[var(--codey-text)] text-[var(--codey-surface)]">
              <IconPuzzle size={18} stroke={1.85} aria-hidden="true" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="truncate text-[15px] font-bold text-foreground">{preview.manifest.name}</span>
                <span className="rounded-full bg-[var(--codey-surface-sunken)] px-2 py-0.5 font-mono text-[11px] text-muted">
                  {upgrading ? `${upgrading.version} → ${preview.manifest.version}` : `v${preview.manifest.version}`}
                </span>
                <span className="rounded-full bg-[var(--codey-blue-soft)] px-2 py-0.5 text-[11.5px] font-semibold text-[var(--codey-blue)]">
                  {duplicate ? "已安装" : upgrading ? "升级" : "新安装"}
                </span>
              </div>
              <p className="mb-0 mt-0.5 truncate font-mono text-[12.5px] text-muted">{preview.manifest.id}</p>
            </div>
          </div>

          {preview.manifest.description ? (
            <p className="m-0 text-[14px] leading-relaxed text-foreground sm:pl-[52px]">{preview.manifest.description}</p>
          ) : null}

          <div className="space-y-1.5 text-[13px] sm:pl-[52px]">
            <div className="font-semibold text-foreground">声明能力</div>
            {capabilities.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {capabilities.map((capability) => (
                  <span
                    key={capability}
                    className="inline-flex items-center rounded-full border border-[var(--codey-border-strong)] px-2.5 py-0.5 font-mono text-[11.5px] text-muted"
                  >
                    {capability}
                  </span>
                ))}
              </div>
            ) : (
              <p className="m-0 text-muted">无特定权限声明</p>
            )}
            {preview.manifest.headerNames && preview.manifest.headerNames.length > 0 ? (
              <div className="text-muted">
                <strong className="font-semibold text-foreground">可修改请求头：</strong>
                {preview.manifest.headerNames.join("、")}
              </div>
            ) : null}
            {capabilities.includes("provider.account.v1") ? (
              <p className="m-0 leading-relaxed text-muted">
                启用后，插件将接收其线路的请求正文，以及配置邮箱对应账号的访问令牌；不会接收刷新令牌。
              </p>
            ) : null}
          </div>

          <div className="plugin-import-note is-warning sm:ml-[52px]">
            <IconAlertTriangle className="mt-0.5 shrink-0" size={15} aria-hidden="true" />
            <p className="m-0">原生插件与 Codey 同进程运行，具有相同系统权限。请确认来源安全。</p>
          </div>
        </div>
      ) : null}

      {duplicate && (
        <div role="alert" className="plugin-import-note is-warning">
          此插件的 v{preview?.manifest.version} 已安装，无需重复导入。请选择更高版本的插件包升级；现有插件不会被覆盖。
        </div>
      )}

      {error ? (
        <div role="alert" className="plugin-import-note is-error">
          <IconAlertTriangle size={15} className="mt-0.5 shrink-0" aria-hidden="true" />
          <div className="min-w-0 flex-1 space-y-2 break-words">
            <div>{error}</div>
            {onRetry && !duplicate && !preview && (
              <Button size="xs" variant="outline" disabled={locked} onClick={onRetry}>
                <IconRefresh size={13} aria-hidden="true" />
                重试检查
              </Button>
            )}
          </div>
        </div>
      ) : null}

      {engaged && (
        <div className="plugin-import-actions">
          <Button variant="outline" size="sm" disabled={busy} onClick={onCancel}>
            取消
          </Button>
          <Button
            size="sm"
            disabled={locked || !preview || duplicate}
            loading={busy && step === "installing"}
            onClick={onConfirm}
          >
            {step === "installing" ? "正在导入…" : error && preview && !duplicate ? "重试导入" : upgrading ? "确认升级" : "确认导入"}
          </Button>
        </div>
      )}
    </div>
  );
}
