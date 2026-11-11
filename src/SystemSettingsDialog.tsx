import { memo, useEffect, useRef, useState } from "react";
import { Tabs } from "@heroui/react";
import {
  IconCheck,
  IconCopy,
  IconFolder,
  IconHelpCircle,
  IconLoader2 as LoaderCircle,
  IconSettings,
  IconTool,
} from "@tabler/icons-react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Tooltip,
} from "./components/ui";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import type { Config } from "./App.types";

export type SystemSettingsDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  container: HTMLElement | null;
  appVersion?: string;
  codexAppVersion?: string;
  codexAppPath?: string;
  isBusy: boolean;
  busy: string | null;
  onRepairCodexConfig: () => void;
  configRepairNotice?: { tone: "info" | "success" | "error"; text: string } | null;
  codeyUpdatePolicy: Config["codeyUpdatePolicy"];
  onCodeyUpdatePolicyChange: (policy: Config["codeyUpdatePolicy"]) => void;
};

function SystemSettingsDialogComponent({
  open,
  onOpenChange,
  container,
  appVersion,
  codexAppVersion,
  codexAppPath,
  isBusy,
  busy,
  onRepairCodexConfig,
  configRepairNotice,
  codeyUpdatePolicy,
  onCodeyUpdatePolicyChange,
}: SystemSettingsDialogProps) {
  const [copied, setCopied] = useState(false);
  const [machineNo, setMachineNo] = useState<string | null>(null);
  const [machineNoLoading, setMachineNoLoading] = useState(true);
  const [machineNoError, setMachineNoError] = useState<string | null>(null);
  const [machineNoAttempt, setMachineNoAttempt] = useState(0);
  const [machineNoCopyState, setMachineNoCopyState] = useState<
    "idle" | "copied" | "error"
  >("idle");
  const machineNoCopyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pathCopyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (machineNoCopyTimer.current !== null) clearTimeout(machineNoCopyTimer.current);
    if (pathCopyTimer.current !== null) clearTimeout(pathCopyTimer.current);
  }, []);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setMachineNo(null);
    setMachineNoLoading(true);
    setMachineNoError(null);
    setMachineNoCopyState("idle");
    const timer = setTimeout(() => {
      cancelled = true;
      setMachineNoError("读取超时，请检查网络连接后重试。");
      setMachineNoLoading(false);
    }, 20000);
    const finish = () => {
      if (cancelled) return false;
      cancelled = true;
      clearTimeout(timer);
      setMachineNoLoading(false);
      return true;
    };
    void invoke<string | null>("get_device_machine_no")
      .then(
        (value) => {
          if (finish()) setMachineNo(value);
        },
        (error: unknown) => {
          if (!finish()) return;
          const reason = error instanceof Error || typeof error === "string"
            ? errorText(error).trim()
            : "";
          setMachineNoError(reason || "暂时无法读取设备号，请稍后重试。");
        },
      );
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [open, machineNoAttempt]);

  const handleCopyMachineNo = async () => {
    if (!machineNo || machineNoLoading) return;
    try {
      await navigator.clipboard.writeText(machineNo);
      setMachineNoCopyState("copied");
    } catch {
      setMachineNoCopyState("error");
    }
    if (machineNoCopyTimer.current !== null) clearTimeout(machineNoCopyTimer.current);
    machineNoCopyTimer.current = setTimeout(() => {
      machineNoCopyTimer.current = null;
      setMachineNoCopyState("idle");
    }, 2000);
  };

  const resolvedCodexPath = codexAppPath || "/Applications/ChatGPT.app";
  const formattedCodexVersion = codexAppVersion?.trim()
    ? `v${codexAppVersion.trim().replace(/^v/, "")}`
    : "未运行 / 未检测到";

  const handleCopyPath = async () => {
    try {
      await navigator.clipboard.writeText(resolvedCodexPath);
      setCopied(true);
      if (pathCopyTimer.current !== null) clearTimeout(pathCopyTimer.current);
      pathCopyTimer.current = setTimeout(() => {
        pathCopyTimer.current = null;
        setCopied(false);
      }, 2000);
    } catch {
      // 忽略写入剪贴板异常
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="system-settings-dialog sm:w-[500px]"
        container={container}
      >
        <DialogHeader>
          <div className="flex items-center gap-3">
            <div className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-[var(--codey-blue-soft)] text-[var(--codey-blue)]">
              <IconSettings size={18} aria-hidden="true" />
            </div>
            <div className="min-w-0 flex-1">
              <DialogTitle>系统设置与偏好</DialogTitle>
              <DialogDescription>
                查看版本、设备号与本地路径，管理 Codex 维护与更新策略。
              </DialogDescription>
            </div>
          </div>
        </DialogHeader>

        <div className="flex flex-col gap-2.5 py-1 text-sm">
          {/* 版本、设备号与目录信息 */}
          <div className="flex flex-col rounded-xl border border-[var(--codey-border-subtle)] bg-[var(--codey-surface-muted)] p-3">
            {/* 上半部分：Codey 与 Codex 版本并排 */}
            <div className="grid grid-cols-2 divide-x divide-[var(--codey-border-subtle)] pb-2.5 border-b border-[var(--codey-border-subtle)]">
              <div className="flex items-center justify-between pr-3 min-w-0">
                <span className="text-xs font-medium text-[var(--codey-muted)] shrink-0">Codey 版本</span>
                <span className="font-mono text-xs font-semibold tracking-tight text-[var(--codey-text)] truncate">
                  v{appVersion || "0.0.1"}
                </span>
              </div>
              <div className="flex items-center justify-between pl-3 min-w-0">
                <span className="text-xs font-medium text-[var(--codey-muted)] shrink-0">Codex 版本</span>
                <span className="font-mono text-xs font-semibold tracking-tight text-[var(--codey-text)] truncate">
                  {formattedCodexVersion}
                </span>
              </div>
            </div>

            <div className="flex items-center gap-3 border-b border-[var(--codey-border-subtle)] py-2.5">
              <span className="shrink-0 text-xs font-medium text-[var(--codey-muted)]">当前设备号</span>
              <div className="min-w-0 flex-1" aria-live="polite">
                <code
                  className="break-all select-all text-xs text-[var(--codey-text)]"
                  title={machineNo || undefined}
                >
                  {machineNoLoading ? "读取中…" : machineNoError ? "读取失败" : machineNo || "尚未登记"}
                </code>
                {machineNoError && (
                  <p className="mt-1 break-words text-xs leading-relaxed text-danger">
                    {machineNoError}
                  </p>
                )}
              </div>
              {machineNoError ? (
                <Button
                  variant="ghost"
                  size="sm"
                  aria-label="重新读取设备号"
                  onClick={() => setMachineNoAttempt((attempt) => attempt + 1)}
                >
                  重试
                </Button>
              ) : (
                <Tooltip content={
                  machineNoCopyState === "copied"
                    ? "已复制到剪贴板"
                    : machineNoCopyState === "error"
                      ? "复制失败，请选中设备号手动复制"
                      : "复制设备号"
                }>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="size-6 min-w-6 shrink-0 text-[var(--codey-muted)] hover:text-[var(--codey-text)]"
                    aria-label="复制设备号"
                    disabled={!machineNo || machineNoLoading}
                    onClick={handleCopyMachineNo}
                  >
                    {machineNoCopyState === "copied" ? (
                      <IconCheck size={13} className="text-success" aria-hidden="true" />
                    ) : (
                      <IconCopy size={13} aria-hidden="true" />
                    )}
                  </Button>
                </Tooltip>
              )}
            </div>

            {/* Codex 目录支持长路径换行与一键复制 */}
            <div className="pt-2.5 flex flex-col gap-1.5">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-1.5 text-xs font-medium text-[var(--codey-muted)]">
                  <IconFolder size={14} className="shrink-0 text-[var(--codey-muted)]" aria-hidden="true" />
                  <span>Codex 目录</span>
                </div>
                <Tooltip content={copied ? "已复制到剪贴板" : "复制路径"}>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="size-6 min-w-6 shrink-0 text-[var(--codey-muted)] hover:text-[var(--codey-text)]"
                    aria-label="复制 Codex 路径"
                    onClick={handleCopyPath}
                  >
                    {copied ? (
                      <IconCheck size={13} className="text-success" aria-hidden="true" />
                    ) : (
                      <IconCopy size={13} aria-hidden="true" />
                    )}
                  </Button>
                </Tooltip>
              </div>
              <div className="rounded-lg border border-[var(--codey-border-subtle)] bg-[var(--codey-surface,#fff)] px-2.5 py-1.5">
                <code
                  className="block font-mono text-[11px] text-[var(--codey-text)] break-all select-all leading-relaxed max-h-20 overflow-y-auto"
                  title={resolvedCodexPath}
                >
                  {resolvedCodexPath}
                </code>
              </div>
            </div>
          </div>

          {/* Codex 配置维护 */}
          <div className="flex flex-col gap-2 rounded-xl border border-[var(--codey-border-subtle)] bg-[var(--codey-surface-muted)] p-3">
            <div className="flex items-center justify-between gap-3">
              <span className="text-xs font-semibold text-[var(--codey-text)]">
                修复 Codex 配置
              </span>
              <Button
                variant="outline"
                size="sm"
                disabled={isBusy}
                onClick={onRepairCodexConfig}
                className="shrink-0 whitespace-nowrap"
              >
                {busy === "repair-codex-config" ? (
                  <LoaderCircle className="animate-spin" size={14} aria-hidden="true" />
                ) : (
                  <IconTool size={14} aria-hidden="true" />
                )}
                <span>{busy === "repair-codex-config" ? "修复中…" : "修复"}</span>
              </Button>
            </div>
            {configRepairNotice && (
              <div
                role="status"
                className={`mt-1 rounded-lg px-2.5 py-1.5 text-xs whitespace-pre-line break-words ${
                  configRepairNotice.tone === "error"
                    ? "bg-danger/10 text-danger"
                    : "bg-success/10 text-success"
                }`}
              >
                {configRepairNotice.text}
              </div>
            )}
          </div>

          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-[var(--codey-border-subtle)] bg-[var(--codey-surface-muted)] p-3">
            <div className="flex items-center gap-1.5 min-w-0">
              <span className="text-xs font-semibold text-[var(--codey-text)]">Codey 更新策略</span>
              <Tooltip content="保存后生效。不检查仅关闭自动检查，仍可手动检查稳定版。" position="top">
                <span
                  className="inline-flex shrink-0 cursor-help items-center text-[var(--codey-muted)] transition-colors hover:text-[var(--codey-text)]"
                  aria-label="Codey 更新策略说明"
                >
                  <IconHelpCircle size={14} aria-hidden="true" />
                </span>
              </Tooltip>
            </div>
            <Tabs
              selectedKey={codeyUpdatePolicy}
              onSelectionChange={(key) => onCodeyUpdatePolicyChange(String(key) as Config["codeyUpdatePolicy"])}
              className="shrink-0"
            >
              <Tabs.ListContainer>
                <Tabs.List aria-label="Codey 更新策略">
                  <Tabs.Tab id="off" isDisabled={isBusy} className="text-xs">不检查<Tabs.Indicator /></Tabs.Tab>
                  <Tabs.Tab id="stable" isDisabled={isBusy} className="text-xs">稳定版<Tabs.Indicator /></Tabs.Tab>
                  <Tabs.Tab id="experimental" isDisabled={isBusy} className="text-xs">实验版<Tabs.Indicator /></Tabs.Tab>
                </Tabs.List>
              </Tabs.ListContainer>
            </Tabs>
          </div>
        </div>

        <DialogFooter>
          <Button variant="default" onClick={() => onOpenChange(false)}>
            完成
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export const SystemSettingsDialog = memo(SystemSettingsDialogComponent);
