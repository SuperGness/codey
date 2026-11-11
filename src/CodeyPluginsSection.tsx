import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  IconAlertTriangle,
  IconEraser,
  IconFolderOpen,
  IconHelpCircle,
  IconPuzzle,
  IconRefresh,
  IconSearch,
  IconSettings,
  IconTerminal2,
  IconTrash,
  IconX,
} from "@tabler/icons-react";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import { cn, toast } from "@heroui/react";
import {
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Switch,
  Tooltip,
} from "./components/ui";
import { PluginConfigDialog } from "./PluginConfigDialog";
import { PluginImportPanel } from "./PluginImportPanel";
import {
  parseCodeyPluginsResult,
  type CodeyPlugin,
  type CodeyPluginPreview,
  type CodeyPluginsResult,
} from "./codeyPlugins";
import { SettingsPageHeader } from "./SettingsPageHeader";
import { formatBytes } from "./formatters";
import { installPluginFileDrop, uploadPluginPackage, validateDroppedPluginFiles } from "./pluginPackageImport";

export function CodeyPluginsSection({ container }: { container?: HTMLElement | null }) {
  const [result, setResult] = useState<CodeyPluginsResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const setNotice = useCallback((text: string) => {
    if (text) toast.success(text);
  }, []);
  const setWarning = useCallback((text: string) => {
    if (text) toast.warning(text);
  }, []);
  const [searchQuery, setSearchQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<"all" | "enabled" | "disabled">("all");
  const [importError, setImportError] = useState("");
  const [preview, setPreview] = useState<CodeyPluginPreview | null>(null);
  const [importStep, setImportStep] = useState<"checking" | "installing" | "">("");
  const [importFileName, setImportFileName] = useState("");
  const [importResult, setImportResult] = useState("");
  const [importPanelKey, setImportPanelKey] = useState(0);
  const [dragActive, setDragActive] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<{ kind: "enable" | "uninstall"; plugin: CodeyPlugin } | null>(null);
  const [removeData, setRemoveData] = useState(false);
  const [clearLogsPlugin, setClearLogsPlugin] = useState<CodeyPlugin | null>(null);
  const [clearLogsError, setClearLogsError] = useState("");
  const [known, setKnown] = useState(false);
  const [sectionEl, setSectionEl] = useState<HTMLElement | null>(null);

  const configContainer = useMemo(() => {
    return sectionEl?.closest<HTMLElement>("#codey-settings-content") ?? sectionEl ?? container;
  }, [sectionEl, container]);

  const pending = useRef(false);
  const epoch = useRef(0);
  const configurationSaved = useRef(false);
  const retrySource = useRef<{ file: File } | { path: string } | null>(null);
  const uploadedPackage = useRef<string | null>(null);
  const uploadAbort = useRef<AbortController | null>(null);
  const discardUploadedPackage = useCallback(() => {
    const uploadId = uploadedPackage.current;
    uploadedPackage.current = null;
    if (uploadId) void invoke("inspect_codey_plugin", { discardUpload: uploadId }).catch(() => undefined);
  }, []);

  const accept = useCallback((data: unknown) => {
    try {
      const next = parseCodeyPluginsResult(data);
      setResult(next);
      setKnown(true);
      setError("");
    } catch (cause) {
      setKnown(false);
      throw cause;
    }
  }, []);

  const refresh = useCallback(async () => {
    const generation = epoch.current;
    setKnown(false);
    setConfirm(null);
    try {
      const data = await invoke("list_codey_plugins");
      if (epoch.current === generation) accept(data);
    } catch (cause) {
      if (epoch.current === generation) {
        setKnown(false);
        setError(errorText(cause));
      }
      throw cause;
    }
  }, [accept]);

  const mutate = useCallback(async (
    command: "set_codey_plugin_enabled" | "install_codey_plugin" | "uninstall_codey_plugin",
    args: Record<string, unknown>,
  ) => {
    const generation = epoch.current;
    const touchesRoutes = command === "set_codey_plugin_enabled" || command === "uninstall_codey_plugin";
    try {
      const data = await invoke(command, args);
      if (epoch.current !== generation) throw new Error("操作已过期");
      accept(data);
      if (touchesRoutes) {
        window.dispatchEvent(new CustomEvent("codey:plugin-routes-changed"));
      }
    } catch (cause) {
      if (touchesRoutes) {
        window.dispatchEvent(new CustomEvent("codey:plugin-routes-changed"));
      }
      if (epoch.current === generation) {
        try {
          await refresh();
        } catch {
          // refresh 已标记未知状态
        }
      }
      throw cause;
    }
  }, [accept, refresh]);

  const run = useCallback(async (action: () => Promise<void>, retry = false) => {
    if (pending.current || (!known && !retry)) return;
    const generation = ++epoch.current;
    pending.current = true;
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (cause) {
      if (epoch.current === generation) setError(errorText(cause));
    } finally {
      if (epoch.current === generation) {
        pending.current = false;
        setBusy(false);
      }
    }
  }, [known]);

  useEffect(() => {
    let cancelled = false;
    const generation = ++epoch.current;
    setLoading(true);
    setError("");
    void invoke<CodeyPluginsResult>("list_codey_plugins")
      .then((data) => {
        if (cancelled || generation !== epoch.current) return;
        accept(data);
      })
      .catch((cause) => {
        if (cancelled || generation !== epoch.current) return;
        setResult(null);
        setError(errorText(cause));
      })
      .finally(() => {
        if (!cancelled && generation === epoch.current) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [accept]);

  useEffect(() => () => {
    uploadAbort.current?.abort();
    discardUploadedPackage();
    ++epoch.current;
  }, [discardUploadedPackage]);

  const togglePlugin = useCallback(async (plugin: CodeyPlugin, enabled: boolean) => {
    await mutate("set_codey_plugin_enabled", { pluginId: plugin.id, enabled });
    setConfirm(null);
    setNotice(enabled ? `插件「${plugin.name}」已启用` : `插件「${plugin.name}」已停用`);
  }, [mutate]);

  const cancelImport = useCallback(() => {
    if (pending.current) return;
    discardUploadedPackage();
    retrySource.current = null;
    setPreview(null);
    setImportError("");
    setImportFileName("");
    setImportPanelKey((key) => key + 1);
  }, [discardUploadedPackage]);

  const inspectImport = useCallback((source: { file: File } | { path: string }) => {
    void run(async () => {
      discardUploadedPackage();
      retrySource.current = source;
      setPreview(null);
      setImportError("");
      setImportResult("");
      setImportFileName("file" in source ? source.file.name : "");
      setImportStep("checking");
      const controller = new AbortController();
      uploadAbort.current = controller;
      try {
        const inspected = "file" in source
          ? await uploadPluginPackage(source.file, args => invoke("inspect_codey_plugin", args), controller.signal)
          : await invoke<CodeyPluginPreview>("inspect_codey_plugin", { path: source.path });
        if (controller.signal.aborted) return;
        if ("uploadId" in inspected) uploadedPackage.current = inspected.uploadId as string;
        setPreview(inspected);
      } catch (cause) {
        if (!controller.signal.aborted) setImportError(`检查插件包失败：${errorText(cause)} 请修正文件后重试，或重新选择插件包。`);
      } finally {
        if (!controller.signal.aborted) setImportStep("");
      }
    }, true);
  }, [run, discardUploadedPackage]);

  const handleSelectPackage = useCallback(() => {
    void run(async () => {
      setImportStep("checking");
      setImportError("");
      setImportResult("");
      try {
        const selected = await invoke<CodeyPluginPreview | null>("select_codey_plugin_package");
        if (selected) {
          discardUploadedPackage();
          retrySource.current = { path: selected.path };
          setImportFileName("");
          setPreview(selected);
        }
      } catch (cause) {
        setPreview(null);
        setImportError(`检查插件包失败：${errorText(cause)} 请重新选择插件包。`);
      } finally {
        setImportStep("");
      }
    }, true);
  }, [run, discardUploadedPackage]);

  const handleInspectPath = useCallback((path: string) => {
    if (path.trim()) inspectImport({ path: path.trim() });
  }, [inspectImport]);

  const handleDroppedFiles = useCallback((files: File[]) => {
    if (pending.current || loading || !known) return;
    setConfirm(null);
    setImportResult("");
    const invalid = validateDroppedPluginFiles(files);
    if (invalid) {
      discardUploadedPackage();
      retrySource.current = files.length === 1 ? { file: files[0] } : null;
      setPreview(null);
      setImportFileName(files.length === 1 ? files[0].name : "");
      setImportError(invalid);
      return;
    }
    inspectImport({ file: files[0] });
  }, [inspectImport, discardUploadedPackage, loading, known]);

  useEffect(() => installPluginFileDrop(
    window, handleDroppedFiles, setDragActive,
    busy || loading || !known || Boolean(editId || confirm || clearLogsPlugin),
    () => Boolean(sectionEl?.getClientRects().length),
  ), [handleDroppedFiles, busy, loading, known, editId, confirm, clearLogsPlugin, sectionEl]);

  const handleConfirmImport = useCallback(() => {
    if (!preview) return;
    const selected = preview;
    const installed = result?.plugins.find(plugin => plugin.id === selected.manifest.id);
    if (installed?.version === selected.manifest.version) return;
    void run(async () => {
      setImportStep("installing");
      setImportError("");
      try {
        if (!known) await refresh();
        await mutate("install_codey_plugin", { path: selected.path, sha256: selected.sha256 });
        discardUploadedPackage();
        retrySource.current = null;
        setPreview(null);
        setImportFileName("");
        setImportPanelKey((key) => key + 1);
        setSearchQuery("");
        const message = installed
          ? `插件「${selected.manifest.name}」升级成功。`
          : `插件「${selected.manifest.name}」安装成功，默认处于停用状态。`;
        setImportResult(message);
        setNotice(message);
      } catch (cause) {
        setImportError(`导入插件失败：${errorText(cause)} 可重试导入或重新选择插件包。`);
      } finally {
        setImportStep("");
      }
    }, true);
  }, [preview, result, run, known, refresh, mutate, discardUploadedPackage, setNotice]);

  const filteredPlugins = useMemo(() => {
    if (!result?.plugins) return [];
    const q = searchQuery.trim().toLowerCase();
    const byStatus = statusFilter === "all"
      ? result.plugins
      : result.plugins.filter((p) => p.enabled === (statusFilter === "enabled"));
    if (!q) return byStatus;
    return byStatus.filter((p) =>
      p.name.toLowerCase().includes(q) ||
      p.id.toLowerCase().includes(q) ||
      (p.description && p.description.toLowerCase().includes(q)) ||
      (p.capabilities && p.capabilities.some((c) => c.toLowerCase().includes(q)))
    );
  }, [result?.plugins, searchQuery, statusFilter]);

  const enabledPluginCount = useMemo(
    () => (result ? result.plugins.reduce((sum, p) => sum + (p.enabled ? 1 : 0), 0) : 0),
    [result],
  );
  const upgrading = preview
    ? result?.plugins.find((plugin) => plugin.id === preview.manifest.id)
    : undefined;
  const blocked = busy || !known || loading;
  const editing = result?.plugins.find((p) => p.id === editId);

  return (
    <section ref={setSectionEl} className="secondary-section codey-plugins-section" aria-labelledby="codey-plugins-title" data-codey-plugin-drop-zone="">
      <SettingsPageHeader
        id="codey-plugins-title"
        title="Codey 插件"
        icon={<IconPuzzle size={15} />}
        description="安装独立功能模块，扩展 Codex 能力与环境支持。"
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={loading || busy}
              onClick={() => void run(refresh, true)}
              aria-label="刷新插件列表"
            >
              <IconRefresh size={14} className={loading ? "animate-spin" : ""} />
              <span>刷新</span>
            </Button>
          </div>
        }
      />

      <PluginImportPanel
        key={importPanelKey}
        platform={result?.platform}
        busy={busy}
        disabled={!known || loading}
        preview={preview}
        upgrading={upgrading}
        error={importError}
        step={importStep}
        dragActive={dragActive}
        fileName={importFileName}
        onRetry={!preview && retrySource.current ? () => {
          if (retrySource.current) inspectImport(retrySource.current);
        } : undefined}
        onCancel={cancelImport}
        onSelectFile={handleSelectPackage}
        onInspectPath={handleInspectPath}
        onClearPreview={() => {
          discardUploadedPackage();
          setPreview(null);
          setImportError("");
          setImportFileName("");
          retrySource.current = null;
        }}
        onConfirm={handleConfirmImport}
      />
      {importResult && <div role="status" className="plugin-import-result">{importResult}</div>}

      {/* 顶部筛选与搜索 */}
      <div className="mb-4 flex flex-col-reverse gap-3 sm:flex-row sm:items-center">
        <div className="flex min-h-8 flex-1 flex-wrap items-center gap-2 text-xs text-muted">
          {loading && !result ? (
            <span>正在读取插件…</span>
          ) : result ? (
            <>
              <div className="extension-status-chips" role="group" aria-label="插件状态筛选">
                {([
                  ["all", "全部", result.plugins.length],
                  ["enabled", "已启用", enabledPluginCount],
                  ["disabled", "未启用", result.plugins.length - enabledPluginCount],
                ] as const).map(([value, label, count]) => (
                  <button
                    key={value}
                    type="button"
                    aria-pressed={statusFilter === value}
                    className={`extension-status-chip is-${value}`}
                    onClick={() => setStatusFilter(value)}
                  >
                    <span>{label}</span>
                    <span className="extension-status-chip-count">{count}</span>
                  </button>
                ))}
              </div>
              <span className="ml-auto font-mono text-[11px] text-muted/75" title="当前客户端平台">
                {result.platform} / {result.arch}
              </span>
            </>
          ) : (
            <span className="text-[var(--codey-red)]">插件状态未知，请点击刷新重试</span>
          )}
        </div>

        <div className="relative min-w-[200px] sm:w-64">
          <Input
            aria-label="搜索插件"
            placeholder="搜索插件名称、ID 或能力…"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            leftSection={<IconSearch size={14} className="text-muted" />}
            rightSection={
              searchQuery ? (
                <button
                  type="button"
                  aria-label="清空插件搜索"
                  className="inline-flex size-6 cursor-pointer items-center justify-center rounded-full text-muted hover:bg-[var(--codey-press)] hover:text-foreground"
                  onClick={() => setSearchQuery("")}
                >
                  <IconX size={12} aria-hidden="true" />
                </button>
              ) : undefined
            }
            className="h-8 text-xs"
          />
        </div>
      </div>

      {/* 提示与错误信息 */}
      {error && (
        <div role="alert" className="mb-4 flex items-start gap-2 rounded-xl border border-[var(--codey-red-border)] bg-[var(--codey-red-soft)] p-3 text-xs text-[var(--codey-red)]">
          <IconAlertTriangle size={15} className="mt-0.5 shrink-0" />
          <div className="flex-1 break-words">{error}</div>
        </div>
      )}

      {/* 插件卡片网格 */}
      {filteredPlugins.length === 0 ? (
        loading && !result ? (
          <div aria-hidden="true" className="space-y-3">
            {[0, 1].map((row) => (
              <div
                key={row}
                className="flex items-center gap-3.5 rounded-2xl border border-[var(--codey-border)] bg-[var(--codey-card)] p-4"
              >
                <div className="size-10 shrink-0 rounded-full bg-default/50" />
                <div className="flex-1 space-y-2">
                  <div className="h-3.5 w-1/4 rounded bg-default/50" />
                  <div className="h-2.5 w-3/5 rounded bg-default/30" />
                </div>
              </div>
            ))}
          </div>
        ) : !known && !loading ? (
          <div className="flex flex-col items-center justify-center rounded-[18px] bg-[var(--codey-panel)] px-6 py-14 text-center">
            <div className="mb-3 flex size-12 items-center justify-center rounded-full bg-[var(--codey-red-soft)] text-[var(--codey-red)]">
              <IconAlertTriangle size={22} stroke={1.7} aria-hidden="true" />
            </div>
            <h4 className="m-0 text-[16px] font-extrabold text-foreground">无法读取插件列表</h4>
            <p className="mb-3 mt-1 max-w-sm text-[13px] text-muted leading-relaxed">
              请检查本地服务状态后重试读取插件目录。
            </p>
            <Button
              size="xs"
              variant="outline"
              disabled={loading || busy}
              onClick={() => void run(refresh, true)}
            >
              重新读取
            </Button>
          </div>
        ) : known && !loading && <div className="flex flex-col items-center justify-center rounded-[18px] bg-[var(--codey-panel)] px-6 py-16 text-center">
          <div className="mb-3.5 flex size-12 items-center justify-center rounded-full bg-[var(--codey-surface-sunken)] text-foreground">
            <IconPuzzle size={24} stroke={1.5} />
          </div>
          {searchQuery || statusFilter !== "all" ? (
            <>
              <h4 className="m-0 text-[17px] font-extrabold text-foreground">未找到匹配的插件</h4>
              <p className="mb-3 mt-1 text-[13.5px] text-muted">
                {searchQuery
                  ? `没有符合「${searchQuery}」的插件，请尝试其他关键词。`
                  : `当前没有${statusFilter === "enabled" ? "已启用" : "未启用"}的插件。`}
              </p>
              <Button
                size="xs"
                variant="outline"
                onClick={() => {
                  setSearchQuery("");
                  setStatusFilter("all");
                }}
              >
                清除筛选
              </Button>
            </>
          ) : (
            <>
              <h4 className="m-0 text-[17px] font-extrabold text-foreground">尚未安装任何插件</h4>
              <p className="mb-0 mt-1 max-w-sm text-[13.5px] text-muted leading-relaxed">
                在上方选择或拖入 .codey-plugin 插件包，扩展环境适配、模型代理拦截与自定增强功能。
              </p>
            </>
          )}
        </div>
      ) : (
        <div className="extension-feed">
          {filteredPlugins.map((plugin) => {
            const hasDirs = Boolean(plugin.pluginDir || plugin.dataDir || plugin.logDir);

            return (
              <article
                key={plugin.id}
                className={`extension-feed-item codey-plugin-card flex flex-col${plugin.enabled ? " is-enabled" : ""}`}
              >
                <div className="pb-2">
                  <div className="flex items-start justify-between gap-3">
                    <div className="flex items-start gap-3 min-w-0 flex-1">
                      <div
                        className={`flex size-10 shrink-0 items-center justify-center rounded-full transition-colors ${
                          plugin.enabled
                            ? "bg-[var(--codey-text)] text-[var(--codey-surface)]"
                            : "bg-[var(--codey-surface-sunken)] text-muted"
                        }`}
                      >
                        <IconPuzzle size={18} stroke={1.85} aria-hidden="true" />
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <h4 className="m-0 truncate text-[15px] font-bold text-foreground">
                            {plugin.name}
                          </h4>
                          <span className="inline-flex items-center rounded-full bg-[var(--codey-surface-sunken)] px-2 py-0.5 font-mono text-[11px] font-medium text-muted">
                            v{plugin.version}
                          </span>
                        </div>
                        <div className="mb-0 mt-0.5 flex items-center gap-1.5 font-mono text-[12.5px] text-muted">
                          <span className="truncate">{plugin.id}</span>
                          {hasDirs ? (
                            <Tooltip
                              delay={150}
                              position="top"
                              className="shrink-0"
                              content={
                                <div className="min-w-[200px] space-y-1.5 text-left leading-relaxed">
                                  <div className="font-semibold text-xs">文件与存储目录</div>
                                  {plugin.pluginDir ? (
                                    <div>
                                      <div className="text-[11px] font-medium opacity-75">插件目录</div>
                                      <div className="mt-0.5 break-all font-mono text-[11px] select-text opacity-90">
                                        {plugin.pluginDir}
                                      </div>
                                    </div>
                                  ) : null}
                                  {plugin.dataDir ? (
                                    <div>
                                      <div className="text-[11px] font-medium opacity-75">数据目录</div>
                                      <div className="mt-0.5 break-all font-mono text-[11px] select-text opacity-90">
                                        {plugin.dataDir}
                                      </div>
                                    </div>
                                  ) : null}
                                  {plugin.logDir ? (
                                    <div>
                                      <div className="text-[11px] font-medium opacity-75">日志目录</div>
                                      <div className="mt-0.5 break-all font-mono text-[11px] select-text opacity-90">
                                        {plugin.logDir}
                                      </div>
                                    </div>
                                  ) : null}
                                </div>
                              }
                            >
                              <span
                                className="inline-flex shrink-0 cursor-help items-center text-muted/60 transition-colors hover:text-foreground"
                                aria-label="查看文件与存储目录"
                              >
                                <IconHelpCircle size={13} aria-hidden="true" />
                              </span>
                            </Tooltip>
                          ) : null}
                        </div>
                      </div>
                    </div>
                    <div className="flex items-center gap-2.5 shrink-0">
                      <span className={`codey-plugin-state${plugin.enabled ? " is-on" : ""}`}>
                        {plugin.enabled ? "已启用" : "未启用"}
                      </span>
                      <Switch
                        size="sm"
                        checked={plugin.enabled}
                        disabled={blocked}
                        aria-label={plugin.enabled ? `停用插件 ${plugin.name}` : `启用插件 ${plugin.name}`}
                        onCheckedChange={(checked) => {
                          if (checked) {
                            setConfirm({ kind: "enable", plugin });
                          } else {
                            void run(() => togglePlugin(plugin, false));
                          }
                        }}
                      />
                    </div>
                  </div>

                  {plugin.description && (
                    <p className="mb-0 mt-1.5 line-clamp-2 text-[14px] text-foreground leading-relaxed sm:ml-[52px]">
                      {plugin.description}
                    </p>
                  )}

                  {plugin.capabilities && plugin.capabilities.length > 0 && (
                    <div className="mt-2.5 flex flex-wrap gap-1.5 sm:ml-[52px]">
                      {plugin.capabilities.map((cap) => (
                        <span
                          key={cap}
                          className="inline-flex items-center rounded-full border border-[var(--codey-border-strong)] px-2.5 py-0.5 font-mono text-[11.5px] text-muted"
                        >
                          {cap}
                        </span>
                      ))}
                    </div>
                  )}

                  {plugin.restartRequired && (
                    <div className="mt-3 rounded-2xl bg-[var(--codey-amber-soft)] px-3.5 py-2.5 text-[13px] text-[var(--codey-amber)] sm:ml-[52px]">
                      需要重启 Codey 或停用后重新启用，以应用版本更新或配置变更。
                    </div>
                  )}

                  {plugin.lastError && (
                    <div role="alert" className="mt-3 rounded-2xl bg-[var(--codey-red-soft)] px-3.5 py-2.5 text-[13px] text-[var(--codey-red)] sm:ml-[52px]">
                      {plugin.lastError}
                    </div>
                  )}
                </div>

                <div className="flex flex-wrap items-center justify-between gap-2 sm:ml-[52px]">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="whitespace-nowrap text-[12.5px] text-muted" title="当前日志及轮转备份的大小，刷新列表时更新">
                      日志 {plugin.logSizeBytes == null ? "大小未知" : formatBytes(plugin.logSizeBytes)}
                    </span>
                    <Button
                      size="xs"
                      variant="ghost"
                      disabled={blocked}
                      aria-label={`清除 ${plugin.name} 的运行日志`}
                      onClick={() => {
                        setClearLogsError("");
                        setClearLogsPlugin(plugin);
                      }}
                    >
                      <IconEraser size={14} aria-hidden="true" />
                      <span>清除日志</span>
                    </Button>
                    {plugin.restartRequired && plugin.enabled ? (
                      <Button
                        size="xs"
                        variant="warning"
                        disabled={blocked}
                        onClick={() =>
                          void run(async () => {
                            await mutate("set_codey_plugin_enabled", { pluginId: plugin.id, enabled: false });
                            try {
                              await togglePlugin(plugin, true);
                              setNotice("插件已重新启用，变更已生效");
                            } catch (cause) {
                              const message = errorText(cause);
                              throw new Error(`重新启用失败：${message}。请检查状态后重试启用。`);
                            }
                          })
                        }
                      >
                        <IconRefresh size={13} aria-hidden="true" />
                        <span>重新启用</span>
                      </Button>
                    ) : null}
                  </div>

                  <div className="flex items-center gap-0.5 ml-auto">
                    <Button
                      size="icon-sm"
                      variant="ghost"
                      disabled={blocked}
                      title="在本机终端查看日志"
                      aria-label={`在本机终端查看 ${plugin.name} 的日志`}
                      onClick={() => void run(async () => {
                        const opened = await invoke<{ status: "ok" | "already_open" }>(
                          "open_codey_plugin_logs", { pluginId: plugin.id },
                        );
                        if (opened.status === "already_open") {
                          setWarning(`${plugin.name} 当前已有日志窗口`);
                        } else {
                          setNotice(`已在本机终端打开 ${plugin.name} 的实时日志`);
                        }
                      })}
                    >
                      <IconTerminal2 size={14} aria-hidden="true" />
                    </Button>

                    <Button
                      size="icon-sm"
                      variant="ghost"
                      disabled={blocked}
                      title="在文件管理器中打开插件目录"
                      aria-label={`打开 ${plugin.name} 的插件目录`}
                      onClick={() => {
                        void invoke("open_codey_plugin_directory", { pluginId: plugin.id }).catch((cause) => {
                          setError(errorText(cause));
                        });
                      }}
                    >
                      <IconFolderOpen size={14} aria-hidden="true" />
                    </Button>

                    <Button
                      size="icon-sm"
                      variant="ghost"
                      disabled={blocked}
                      title="插件配置"
                      aria-label={`配置 ${plugin.name}`}
                      onClick={() => setEditId(plugin.id)}
                    >
                      <IconSettings size={14} aria-hidden="true" />
                    </Button>

                    <Button
                      size="icon-sm"
                      variant="ghost"
                      className="text-muted hover:text-danger"
                      disabled={blocked || plugin.enabled}
                      title={plugin.enabled ? "请先停用插件再卸载" : "卸载插件"}
                      aria-label={`卸载 ${plugin.name}`}
                      onClick={() => {
                        setRemoveData(false);
                        setConfirm({ kind: "uninstall", plugin });
                      }}
                    >
                      <IconTrash size={14} aria-hidden="true" />
                    </Button>
                  </div>
                </div>
              </article>
            );
          })}
        </div>
      )}

      {clearLogsPlugin && (
        <Dialog open onOpenChange={(open) => { if (!open && !busy) setClearLogsPlugin(null); }}>
          <DialogContent container={container} className="max-w-[460px]" onEscapeKeyDown={(event) => { if (busy) event.preventDefault(); }}>
            <DialogHeader>
              <DialogTitle>确认清除运行日志</DialogTitle>
              <DialogDescription>
                将清除 {clearLogsPlugin.name} 的现有运行日志及轮转备份，此操作无法恢复。插件配置和数据会保留，运行中的插件仍会继续记录新日志。
              </DialogDescription>
            </DialogHeader>
            {clearLogsError && <p role="alert" className="text-sm text-danger">{clearLogsError}</p>}
            <DialogFooter>
              <Button variant="outline" disabled={busy} onClick={() => setClearLogsPlugin(null)}>取消</Button>
              <Button variant="destructive" disabled={blocked} loading={busy} onClick={() => void run(async () => {
                setClearLogsError("");
                try {
                  const data = await invoke("clear_codey_plugin_logs", { pluginId: clearLogsPlugin.id, confirmed: true });
                  accept(data);
                  setNotice(`${clearLogsPlugin.name} 的现有运行日志已清除`);
                  setClearLogsPlugin(null);
                } catch (cause) {
                  setClearLogsError(errorText(cause));
                }
              })}>确认清除</Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}

      {/* 启用信任 / 卸载确认对话框 */}
      {confirm && (
        <Dialog open={Boolean(confirm)} onOpenChange={(open) => { if (!open) setConfirm(null); }}>
          <DialogContent container={container} className="max-w-[460px]">
            <DialogHeader>
              <div className="flex items-start gap-3.5">
                <div
                  className={cn(
                    "flex size-10 shrink-0 items-center justify-center rounded-xl",
                    confirm.kind === "uninstall"
                      ? "bg-red-500/10 text-red-600 dark:bg-red-500/15 dark:text-red-400"
                      : "bg-amber-500/10 text-amber-600 dark:bg-amber-500/15 dark:text-amber-400"
                  )}
                >
                  {confirm.kind === "uninstall" ? (
                    <IconTrash size={20} stroke={1.75} aria-hidden="true" />
                  ) : (
                    <IconAlertTriangle size={20} stroke={1.75} aria-hidden="true" />
                  )}
                </div>
                <div className="min-w-0 flex-1 space-y-1">
                  <DialogTitle>
                    {confirm.kind === "enable" ? "信任并启用插件" : "确认卸载插件"}
                  </DialogTitle>
                  <DialogDescription>
                    {confirm.kind === "enable"
                      ? "启用前请确认插件来源安全与所声明的运行权限"
                      : "确定要从系统中卸载此插件吗？"}
                  </DialogDescription>
                </div>
              </div>
            </DialogHeader>

            <div className="space-y-3.5 pt-2 text-xs">
              {/* 插件信息摘要卡片 */}
              <div className="flex items-center gap-3 rounded-xl border border-black/[0.08] bg-black/[0.02] p-3 dark:border-white/[0.08] dark:bg-white/[0.03]">
                <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-black/[0.05] text-foreground/70 dark:bg-white/[0.08] dark:text-foreground/80">
                  <IconPuzzle size={18} stroke={1.75} aria-hidden="true" />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-xs font-semibold text-foreground">
                      {confirm.plugin.name}
                    </span>
                    {confirm.plugin.version && (
                      <span className="rounded bg-black/[0.05] px-1.5 py-0.5 text-[10px] font-mono text-muted dark:bg-white/[0.08]">
                        v{confirm.plugin.version}
                      </span>
                    )}
                  </div>
                  <p className="mt-0.5 line-clamp-1 text-[11.5px] text-muted">
                    {confirm.plugin.description || confirm.plugin.id}
                  </p>
                </div>
              </div>

              {confirm.kind === "enable" ? (
                <div className="space-y-3">
                  <div className="space-y-1.5 rounded-xl border border-black/[0.08] bg-black/[0.015] p-3 dark:border-white/[0.08] dark:bg-white/[0.02]">
                    <div className="font-medium text-foreground">声明能力</div>
                    {confirm.plugin.capabilities.length > 0 ? (
                      <div className="flex flex-wrap gap-1.5 pt-0.5">
                        {confirm.plugin.capabilities.map((cap) => (
                          <span
                            key={cap}
                            className="inline-flex items-center rounded-md bg-black/[0.05] px-2 py-0.5 font-mono text-[11px] text-muted dark:bg-white/[0.08]"
                          >
                            {cap}
                          </span>
                        ))}
                      </div>
                    ) : (
                      <p className="m-0 text-muted">无特定权限声明</p>
                    )}
                  </div>

                  <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-amber-800 dark:text-amber-300">
                    <IconAlertTriangle className="mt-0.5 shrink-0" size={16} aria-hidden="true" />
                    <p className="m-0 leading-relaxed">
                      启用后将执行插件本地代码，插件具有与 Codey 相同的系统运行权限。请确认来源安全可靠。
                    </p>
                  </div>
                </div>
              ) : (
                <div className="space-y-3">
                  <p className="m-0 text-muted leading-relaxed">
                    卸载后将移除插件安装包与加载入口，该插件提供的扩展功能将立即停止。
                  </p>

                  <div
                    role="button"
                    tabIndex={0}
                    onClick={() => {
                      if (!blocked) setRemoveData((prev) => !prev);
                    }}
                    onKeyDown={(e) => {
                      if ((e.key === " " || e.key === "Enter") && !blocked) {
                        e.preventDefault();
                        setRemoveData((prev) => !prev);
                      }
                    }}
                    className={cn(
                      "group flex items-start gap-3 rounded-xl border p-3.5 transition-all cursor-pointer select-none",
                      removeData
                        ? "border-red-500/40 bg-red-500/[0.06] dark:border-red-500/30 dark:bg-red-500/[0.08]"
                        : "border-black/[0.08] bg-black/[0.015] hover:border-black/15 hover:bg-black/[0.03] dark:border-white/[0.08] dark:bg-white/[0.02] dark:hover:border-white/15 dark:hover:bg-white/[0.04]",
                      blocked && "pointer-events-none opacity-50"
                    )}
                  >
                    <div className="mt-0.5 shrink-0" onClick={(e) => e.stopPropagation()}>
                      <Checkbox
                        disabled={blocked}
                        checked={removeData}
                        onCheckedChange={(next) => setRemoveData(next === true)}
                        aria-label="同时彻底删除该插件的配置、数据和历史日志"
                      />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div
                        className={cn(
                          "font-medium transition-colors",
                          removeData ? "text-red-700 dark:text-red-300" : "text-foreground"
                        )}
                      >
                        同时彻底删除该插件的配置与数据
                      </div>
                      <p className="mt-1 mb-0 text-[11.5px] leading-relaxed text-muted">
                        包含所有本地配置文件、运行缓存及历史日志。默认保留数据，以便重新安装时恢复。
                      </p>
                    </div>
                  </div>
                </div>
              )}
            </div>

            <DialogFooter className="mt-5 pt-1">
              <Button variant="outline" size="sm" disabled={blocked} onClick={() => setConfirm(null)}>
                取消
              </Button>
              <Button
                size="sm"
                variant={confirm.kind === "uninstall" ? "destructive" : "default"}
                disabled={blocked}
                onClick={() =>
                  void run(async () => {
                    if (confirm.kind === "enable") {
                      await togglePlugin(confirm.plugin, true);
                    } else {
                      await mutate("uninstall_codey_plugin", {
                        pluginId: confirm.plugin.id,
                        removeData,
                      });
                      setConfirm(null);
                      setEditId(null);
                      setNotice(`插件「${confirm.plugin.name}」已成功卸载`);
                    }
                  })
                }
              >
                {confirm.kind === "uninstall" ? (
                  <>
                    <IconTrash size={14} aria-hidden="true" />
                    <span>确认卸载</span>
                  </>
                ) : (
                  <span>信任并启用</span>
                )}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}

      {/* 插件独立配置弹层 */}
      {editing && (
        <PluginConfigDialog
          key={editing.id}
          plugin={editing}
          container={configContainer}
          onClose={() => {
            setEditId(null);
            if (!configurationSaved.current) void run(refresh, true);
            configurationSaved.current = false;
          }}
          onChanged={(data) => {
            accept(data);
            configurationSaved.current = true;
            setNotice(`插件「${editing.name}」配置已保存`);
          }}
        />
      )}
    </section>
  );
}
