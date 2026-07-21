import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  BellRing,
  Check,
  CircleAlert,
  CircleCheck,
  Cpu,
  Download,
  FolderOpen,
  GitBranch,
  History,
  LoaderCircle,
  PlugZap,
  RefreshCw,
  Save,
  Search,
  Send,
  Server,
  Trash2,
  X,
} from "lucide-react";
import { invoke } from "./api";
import { formatBytes, TraceLogModule, type TraceLogStats } from "./TraceLogModule";
import {
  MagicBadge as Badge,
  MagicButton as Button,
  MagicCard as Card,
  MagicCheckbox as Checkbox,
  MagicDialog as Dialog,
  MagicDialogContent as DialogContent,
  MagicDialogDescription as DialogDescription,
  MagicDialogFooter as DialogFooter,
  MagicDialogHeader as DialogHeader,
  MagicDialogTitle as DialogTitle,
  MagicInput as Input,
  MagicSwitch as Switch,
  ShimmerButton,
} from "./components/magicui";

type Profile = {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  protocol: "responses" | "chatCompletions";
  ccSwitchProviderId?: string;
  ccSwitchReadOnly: boolean;
};

type Config = {
  activeProfileId: string;
  profiles: Profile[];
  webhook: { enabled: boolean; url: string };
  codexAppPath: string;
  userScripts: string[];
  selectedModelsByProvider: Record<string, string[]>;
  upstreamModelsByProvider: Record<string, string[]>;
  disableTraceLogWrites: boolean;
  slimCodexPet: boolean;
  slimCodexVoice: boolean;
  fastContextTools: boolean;
  subagentOptimization: boolean;
  hideFullAccessWarning: boolean;
};

type OfficialModelState = {
  slug: string;
  displayName: string;
  supported: boolean;
};

type ModelState = {
  officialModels: OfficialModelState[];
  officialModelIds: string[];
  thirdPartyModels: string[];
  upstreamModels: string[];
};

type Maintenance = {
  sessionStatus?: string;
  sessionDetail?: string;
  sessionThreads?: number;
  pluginStatus?: string;
  pluginDetail?: string;
  performanceStatus?: string;
  performanceDetail?: string;
};

type RuntimeStatus = {
  running: boolean;
  appVersion?: string;
  restartRequired?: boolean;
  restartInProgress?: boolean;
  activeProfileId?: string;
  activeProfileName?: string;
  startupError?: string;
  codexAppPath?: string;
  maintenance?: Maintenance;
  traceLogStats?: TraceLogStats;
};

type CcSwitchStatus = {
  available: boolean;
  path: string;
  changed: boolean;
  message?: string;
  provider: {
    id: string;
    name: string;
    official: boolean;
    baseUrl: string;
    protocol: "responses" | "chatCompletions";
    source: "cc-switch" | "local";
  };
};

type Notice = { tone: "info" | "success" | "error"; text: string };
type InlineResult = { tone: "idle" | "pending" | "success" | "error"; text: string };
type Confirmation = {
  action: "clear" | "restart" | "install-update";
  title: string;
  description: string;
  confirmLabel: string;
  run: () => void;
};
type TraceLogCleanup = {
  databasesFound: number;
  databasesCleaned: number;
  rowsDeleted: number;
  bytesBefore: number;
  bytesAfter: number;
  bytesReclaimed: number;
};
type UpdateCheck = {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  selectedAsset?: UpdateAsset;
};
type UpdateAsset = {
  platform: string;
  arch: string;
  packageType: string;
  fileName: string;
  url: string;
  sha256: string;
  size: number;
};
type UpdateDownload = {
  latestVersion: string;
  filePath: string;
  fileName: string;
  size: number;
  sha256: string;
  asset: UpdateAsset;
};

type AppProps = {
  embedded?: boolean;
  onClose?: () => void;
};

const SUBAGENT_MODEL = "gpt-5.6-luna";
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);
const supportsModel = (models: string[], expected: string) => models.some(
  (model) => model.trim().toLowerCase() === expected,
);

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error(message)), timeoutMs);
    promise.then((value) => {
      window.clearTimeout(timer);
      resolve(value);
    }, (error) => {
      window.clearTimeout(timer);
      reject(error);
    });
  });
}

export function App({ embedded = false, onClose }: AppProps) {
  const [config, setConfig] = useState<Config | null>(null);
  const [status, setStatus] = useState<RuntimeStatus>({ running: false });
  const [ccSwitchStatus, setCcSwitchStatus] = useState<CcSwitchStatus | null>(null);
  const [modelState, setModelState] = useState<ModelState>({
    officialModels: [],
    officialModelIds: [],
    thirdPartyModels: [],
    upstreamModels: [],
  });
  const [modelPickerVisible, setModelPickerVisible] = useState(false);
  const [modelQuery, setModelQuery] = useState("");
  const [draftModels, setDraftModels] = useState<string[]>([]);
  const [notice, setNotice] = useState<Notice>({ tone: "info", text: "正在连接 Codey…" });
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [webhookResult, setWebhookResult] = useState<InlineResult>({ tone: "idle", text: "" });
  const [updateResult, setUpdateResult] = useState<InlineResult>({ tone: "idle", text: "" });
  const [updateCheck, setUpdateCheck] = useState<UpdateCheck | null>(null);
  const [downloadedUpdate, setDownloadedUpdate] = useState<UpdateDownload | null>(null);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const [portalContainer, setPortalContainer] = useState<HTMLElement | null>(null);
  const [traceSnapshotStale, setTraceSnapshotStale] = useState(false);

  const provider = ccSwitchStatus?.provider;
  const officialSlugs = useMemo(
    () => new Set(modelState.officialModelIds),
    [modelState.officialModelIds],
  );
  const filteredUpstreamModels = useMemo(() => {
    const query = modelQuery.trim().toLowerCase();
    return query
      ? modelState.upstreamModels.filter((model) => model.toLowerCase().includes(query))
      : modelState.upstreamModels;
  }, [modelQuery, modelState.upstreamModels]);
  const isBusy = busy !== null;

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => {
    if (!status.traceLogStats?.pending) return;
    const delays = [250, 500, 1_000, 2_000, 5_000];
    let cancelled = false;
    let timer = 0;
    let delayIndex = 0;
    const poll = () => {
      if (cancelled) return;
      const delay = delays[delayIndex];
      delayIndex = Math.min(delayIndex + 1, delays.length - 1);
      timer = window.setTimeout(async () => {
        try {
          const next = await invoke<RuntimeStatus>("runtime_status");
          if (cancelled) return;
          setStatus(next);
          if (next.traceLogStats?.pending) poll();
        } catch {
          poll();
        }
      }, delay);
    };
    poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [status.traceLogStats?.pending]);

  useEffect(() => {
    if (!status.restartInProgress) return;
    let cancelled = false;
    let timer = 0;
    const poll = () => {
      timer = window.setTimeout(async () => {
        try {
          const next = await invoke<RuntimeStatus>("runtime_status");
          if (cancelled) return;
          setStatus(next);
          if (next.restartInProgress) poll();
        } catch {
          if (!cancelled) poll();
        }
      }, 500);
    };
    poll();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [status.restartInProgress]);

  async function load() {
    try {
      const result = await invoke<{
        config: Config;
        modelState?: ModelState;
        startupError?: string;
        ccSwitch?: CcSwitchStatus;
      }>("load_codey_config");
      setConfig(result.config);
      setCcSwitchStatus(result.ccSwitch ?? null);
      if (result.modelState) setModelState(result.modelState);
      const next = await refreshStatus();
      const startupError = next.startupError || result.startupError;
      if (startupError) {
        setNotice({ tone: "error", text: `自动启动失败：${startupError}` });
      } else if (next.restartRequired) {
        setNotice({ tone: "info", text: "已保存的配置需重启 Codex 后生效" });
      } else {
        setNotice({
          tone: next.running ? "success" : "info",
          text: next.running
            ? "当前线路和模型目录已同步"
            : "Codey 运行时已就绪",
        });
      }
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    }
  }

  async function refreshStatus() {
    const next = await invoke<RuntimeStatus>("runtime_status");
    setStatus(next);
    return next;
  }

  function editConfig(next: Config) {
    setConfig(next);
    setDirty(true);
  }

  function setSubagentOptimization(enabled: boolean) {
    setConfig((current) => current
      ? { ...current, subagentOptimization: enabled }
      : current);
    setDirty(true);
  }

  function updateWebhook(patch: Partial<Config["webhook"]>) {
    if (!config) return;
    editConfig({ ...config, webhook: { ...config.webhook, ...patch } });
    setWebhookResult({ tone: "idle", text: "" });
  }

  async function persist(next: Config) {
    const result = await invoke<{
      config: Config;
      ccSwitch?: CcSwitchStatus;
      modelState?: ModelState;
      restartRequired?: boolean;
    }>("save_codey_config", { config: next });
    setConfig(result.config);
    window.dispatchEvent(new CustomEvent("codey:config-changed", {
      detail: { config: result.config },
    }));
    if (result.ccSwitch) setCcSwitchStatus(result.ccSwitch);
    if (result.modelState) setModelState(result.modelState);
    if (typeof result.restartRequired === "boolean") {
      setStatus((current) => ({ ...current, restartRequired: result.restartRequired }));
    }
    setDirty(false);
    return result;
  }

  async function syncCurrentProvider() {
    if (dirty || isBusy) return;
    await runOperation("sync-provider", async () => {
      const result = await invoke<{
        config: Config;
        ccSwitch: CcSwitchStatus;
        modelState: ModelState;
        restartRequired?: boolean;
      }>("sync_current_provider");
      setConfig(result.config);
      setCcSwitchStatus(result.ccSwitch);
      setModelState(result.modelState);
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired ?? current.restartRequired,
      }));
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? `已读取「${result.ccSwitch.provider.name}」，重启 Codex 后应用线路`
          : `已同步「${result.ccSwitch.provider.name}」`,
      });
    });
  }

  async function runOperation(name: string, action: () => Promise<void>) {
    if (isBusy) return;
    setBusy(name);
    try {
      await action();
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) });
    } finally {
      setBusy(null);
    }
  }

  async function saveCurrent() {
    if (!config) return;
    await runOperation("save", async () => {
      const result = await persist(config);
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? "Codey 设置已保存，启动参数将在重启 Codex 后生效"
          : "Codey 设置已保存",
      });
    });
  }

  async function fetchCurrentModels() {
    if (!provider || provider.official) return;
    await runOperation("fetch-models", async () => {
      const result = await withTimeout(
        invoke<{ modelState: ModelState; restartRequired?: boolean }>("fetch_current_provider_models"),
        15_000,
        "获取上游模型超时，请检查当前线路",
      );
      setModelState(result.modelState);
      if (typeof result.restartRequired === "boolean") {
        setStatus((current) => ({ ...current, restartRequired: result.restartRequired }));
      }
      setDraftModels(result.modelState.thirdPartyModels);
      setModelQuery("");
      setModelPickerVisible(true);
    });
  }

  async function updateSubagentOptimization(checked: boolean) {
    if (!checked) {
      setSubagentOptimization(false);
      return;
    }
    if (!provider) {
      setNotice({ tone: "error", text: "当前线路尚未就绪，无法校验子代理模型" });
      return;
    }
    await runOperation("check-subagent-model", async () => {
      let supported = false;
      if (provider.official) {
        supported = modelState.officialModels.some(
          (model) => model.slug === SUBAGENT_MODEL && model.supported,
        );
      } else {
        let result: {
          models: string[];
          modelState: ModelState;
          restartRequired?: boolean;
        };
        try {
          result = await withTimeout(
            invoke("fetch_current_provider_models"),
            15_000,
            "获取上游模型超时，请检查当前线路",
          );
        } catch (error) {
          throw new Error(`无法确认当前第三方 API 是否支持 ${SUBAGENT_MODEL}：${errorText(error)}`);
        }
        setModelState(result.modelState);
        if (typeof result.restartRequired === "boolean") {
          setStatus((current) => ({ ...current, restartRequired: result.restartRequired }));
        }
        supported = supportsModel(result.models, SUBAGENT_MODEL);
      }

      if (!supported) {
        setNotice({
          tone: "error",
          text: `当前${provider.official ? "官方账号" : "第三方 API"}不支持 ${SUBAGENT_MODEL}，无法开启子代理协作优化`,
        });
        return;
      }
      setSubagentOptimization(true);
      setNotice({
        tone: "success",
        text: `已确认当前线路支持 ${SUBAGENT_MODEL}，保存并重启 Codex 后生效`,
      });
    });
  }

  function toggleDraftModel(model: string, checked: boolean) {
    setDraftModels((current) => checked
      ? current.includes(model) ? current : [...current, model]
      : current.filter((item) => item !== model));
  }

  async function saveModelSelection() {
    await runOperation("save-models", async () => {
      const result = await invoke<{
        config: Config;
        modelState: ModelState;
        restartRequired?: boolean;
      }>("save_selected_models", { models: draftModels });
      setConfig(result.config);
      setModelState(result.modelState);
      setStatus((current) => ({
        ...current,
        restartRequired: result.restartRequired ?? current.restartRequired,
      }));
      setModelPickerVisible(false);
      setNotice({
        tone: result.restartRequired ? "info" : "success",
        text: result.restartRequired
          ? `已更新模型列表，共 ${result.modelState.thirdPartyModels.length} 个三方模型；重启 Codex 后生效`
          : `已更新模型列表，共 ${result.modelState.thirdPartyModels.length} 个三方模型`,
      });
    });
  }

  async function testWebhook() {
    if (!config || isBusy) return;
    setBusy("test-webhook");
    setWebhookResult({ tone: "pending", text: "正在发送测试通知…" });
    try {
      const next = { ...config, webhook: { ...config.webhook, enabled: true } };
      await persist(next);
      await withTimeout(
        invoke("test_webhook"),
        12_000,
        "飞书测试在 12 秒内没有完成，请检查 Webhook 地址和网络",
      );
      setWebhookResult({ tone: "success", text: "测试卡片已发送，三种会话状态通知已开启" });
      setNotice({ tone: "success", text: "飞书机器人连接成功" });
    } catch (error) {
      const text = errorText(error);
      setWebhookResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  async function checkForUpdates() {
    if (!config || isBusy) return;
    setBusy("check-update");
    setUpdateResult({ tone: "pending", text: "正在检查更新…" });
    setUpdateCheck(null);
    setDownloadedUpdate(null);
    try {
      const result = await withTimeout(
        invoke<UpdateCheck>("check_for_updates"),
        12_000,
        "检查更新超时，请检查网络",
      );
      setUpdateCheck(result);
      const text = result.updateAvailable
        ? result.selectedAsset
          ? `发现 v${result.latestVersion} 更新（当前 v${result.currentVersion}）`
          : `发现 v${result.latestVersion} 更新，但当前系统暂无可安装包`
        : `当前已是最新版本 v${result.currentVersion}`;
      setUpdateResult({ tone: result.updateAvailable && !result.selectedAsset ? "error" : "success", text });
      setNotice({
        tone: result.updateAvailable && result.selectedAsset ? "info" : result.updateAvailable ? "error" : "success",
        text,
      });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  async function downloadUpdate() {
    if (!config || isBusy || !updateCheck?.updateAvailable || !updateCheck.selectedAsset) return;
    setBusy("download-update");
    setDownloadedUpdate(null);
    setUpdateResult({ tone: "pending", text: "正在下载并校验更新…" });
    try {
      const result = await withTimeout(
        invoke<UpdateDownload>("download_update"),
        300_000,
        "下载更新超时，请稍后重试",
      );
      setDownloadedUpdate(result);
      const text = `已下载 ${result.fileName}（${formatBytes(result.size)}），校验通过`;
      setUpdateResult({ tone: "success", text });
      setNotice({ tone: "success", text });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  function askInstallDownloadedUpdate() {
    if (!downloadedUpdate || isBusy) return;
    setConfirmation({
      action: "install-update",
      title: "安装更新",
      description: `Codey 将退出当前实例，安装 ${downloadedUpdate.fileName}，然后尝试启动新版。`,
      confirmLabel: "安装并重启",
      run: () => void installDownloadedUpdate(),
    });
  }

  async function installDownloadedUpdate() {
    if (!downloadedUpdate || isBusy) return;
    setBusy("install-update");
    setUpdateResult({ tone: "pending", text: "正在启动安装器…" });
    try {
      await invoke("install_downloaded_update", { filePath: downloadedUpdate.filePath });
      const text = "正在退出 Codey 并启动安装器…";
      setUpdateResult({ tone: "pending", text });
      setNotice({ tone: "info", text });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
      setBusy(null);
    }
  }

  function askClearTraceLogs() {
    setConfirmation({
      action: "clear",
      title: "清理 Codex 日志库？",
      description: "将清空并压缩 logs_*.sqlite，只删除本地诊断/Trace 日志，不影响聊天历史、账号、配置或插件。清理后的诊断记录无法恢复。",
      confirmLabel: "确认清理",
      run: () => void clearTraceLogs(),
    });
  }

  function askRestartCodex() {
    setConfirmation({
      action: "restart",
      title: "重启 Codex？",
      description: "当前 Codex 客户端将被关闭并由 Codey 自动重新拉起，正在执行的本地任务会被中断。",
      confirmLabel: "重启 Codex",
      run: () => void restartCodex(),
    });
  }

  async function restartCodex() {
    if (!config) return;
    await runOperation("restart", async () => {
      if (dirty) await persist(config);
      setNotice({
        tone: "info",
        text: "正在重启 Codex，Codey 将自动重新拉起客户端…",
      });
      await invoke("restart_codey");
      setStatus((current) => ({
        ...current,
        restartInProgress: true,
      }));
    });
  }

  async function clearTraceLogs() {
    await runOperation("clear-trace-logs", async () => {
      const result = await invoke<{
        cleanup: TraceLogCleanup;
        protectionEnabled: boolean;
      }>("clear_codex_trace_logs");
      const cleanup = result.cleanup;
      if (cleanup.databasesFound === 0) {
        setNotice({ tone: "info", text: "未发现 Codex 日志库，无需清理" });
        return;
      }
      setTraceSnapshotStale(true);
      const protectionDetail = result.protectionEnabled
        ? "Trace 写盘保护保持开启"
        : "Trace 写盘保护保持关闭";
      setNotice({
        tone: "success",
        text: `已清理 ${cleanup.databasesCleaned} 个日志库、${cleanup.rowsDeleted} 条记录，释放 ${formatBytes(cleanup.bytesReclaimed)}；${protectionDetail}，可手动刷新统计`,
      });
    });
  }

  async function refreshTraceLogStats() {
    await runOperation("refresh-trace-stats", async () => {
      const result = await invoke<{
        status: "ok" | "pending";
        traceLogStats: TraceLogStats;
      }>("refresh_trace_log_stats");
      setStatus((current) => ({ ...current, traceLogStats: result.traceLogStats }));
      if (result.status === "pending") {
        setNotice({ tone: "info", text: "Trace 日志正在统计，请稍候" });
        return;
      }
      setTraceSnapshotStale(false);
      setNotice({ tone: "success", text: "Trace 日志统计已更新" });
    });
  }

  if (!config || !provider) {
    return (
      <main className="app-shell loading-shell">
        <div className="loading-mark"><GitBranch size={17} /></div>
        <div>
          <strong>正在载入 Codey</strong>
          <p>{notice.text}</p>
        </div>
        <LoaderCircle className="spinner loading-spinner" size={16} aria-hidden="true" />
      </main>
    );
  }

  const maintenance = status.maintenance;
  const sessionOk = maintenance?.sessionStatus === "ready";
  const pluginOk = maintenance?.pluginStatus === "ready";
  const performanceError = maintenance?.performanceStatus === "error";
  const resolvedCodexPath = status.codexAppPath || "/Applications/ChatGPT.app";
  const optimizationReady = config.slimCodexPet
    && config.slimCodexVoice
    && config.fastContextTools
    && !performanceError;
  const restartPending = Boolean(status.restartRequired);
  const statusCards: Array<{
    title: string;
    description: string;
    detail: string;
    label: string;
    tone: "success" | "warning" | "destructive" | "info";
    icon: typeof Activity;
  }> = [
    {
      title: "会话恢复",
      description: sessionOk ? "会话索引与恢复链路工作正常。" : "正在确认会话索引与恢复链路。",
      detail: maintenance?.sessionDetail || "等待 Codey 返回会话状态",
      label: sessionOk ? "正常" : maintenance ? "需检查" : "检查中",
      tone: sessionOk ? "success" : maintenance ? "destructive" : "warning",
      icon: History,
    },
    {
      title: "系统优化",
      description: optimizationReady
        ? "精简策略与性能补丁已按当前配置启用。"
        : "部分精简策略尚未启用，保留完整功能。",
      detail: performanceError
        ? maintenance?.performanceDetail || "性能补丁加载异常"
        : "FastCtx、宠物、语音与 Windows 性能策略",
      label: performanceError ? "异常" : optimizationReady ? "已优化" : "标准",
      tone: performanceError ? "destructive" : optimizationReady ? "success" : "info",
      icon: Cpu,
    },
    {
      title: "插件服务",
      description: pluginOk ? "Codex 插件已注入并接管会话生命周期。" : "正在检测客户端插件与注入状态。",
      detail: maintenance?.pluginDetail || "等待插件服务状态",
      label: pluginOk ? "已连接" : maintenance ? "需检查" : "检查中",
      tone: pluginOk ? "success" : maintenance ? "destructive" : "warning",
      icon: PlugZap,
    },
  ];

  return (
    <main className={`app-shell${embedded ? " embedded" : ""}`} ref={setPortalContainer}>
      <a className="skip-link" href="#codey-settings-content">跳至设置内容</a>

      <header className="config-header">
        <div className="config-header-inner">
          <div className="config-brand">
            <div className="config-brand-mark">{">_"}</div>
            <div className="config-brand-copy">
              <div className="config-brand-title-row">
                <h1>Codey 配置</h1>
                <Badge variant={provider.official ? "outline" : "secondary"}>
                  {provider.name}
                </Badge>
              </div>
              <p>管理线路、模型、运行策略和诊断通知</p>
            </div>
          </div>

          <div className="config-header-actions">
            <ShimmerButton
              className="save-button"
              disabled={!dirty || isBusy}
              onClick={() => void saveCurrent()}
            >
              {busy === "save"
                ? <LoaderCircle className="spinner" aria-hidden="true" />
                : dirty
                  ? <Save aria-hidden="true" />
                  : <Check aria-hidden="true" />}
              {dirty ? "保存更改" : "已保存"}
            </ShimmerButton>
            {embedded && (
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="关闭设置"
                onClick={onClose}
              >
                <X aria-hidden="true" />
              </Button>
            )}
          </div>
        </div>

      </header>

      <div className="page-scroll">
        <div className="page" id="codey-settings-content">
          <section
            className={`operations-hub${restartPending ? " pending" : status.running ? " running" : ""}`}
            aria-labelledby="operations-title"
          >
            <Card className="operations-panel">
              <div className="operations-header">
                <div className="operations-heading">
                  <span className="operations-heading-icon">
                    <Activity size={18} aria-hidden="true" />
                  </span>
                  <div>
                    <span className="section-kicker">Runtime & maintenance</span>
                    <h1 id="operations-title">Codex 运行与维护</h1>
                    <p aria-live="polite">
                      {restartPending
                        ? "配置已保存，等待重启后载入新的模型目录或启动参数。"
                        : status.running
                          ? "当前线路与运行参数已就绪，状态会在本页自动同步。"
                          : "Codex 尚未启动，运行状态将在客户端启动后自动同步。"}
                    </p>
                  </div>
                </div>

                <div className="operations-actions">
                  <Badge variant={restartPending ? "warning" : status.running ? "success" : "secondary"}>
                    <span className="operations-status-dot" aria-hidden="true" />
                    {restartPending ? "等待重启" : status.running ? "运行中" : "未启动"}
                  </Badge>
                  <Button
                    variant={restartPending ? "default" : "outline"}
                    size="sm"
                    disabled={isBusy || status.restartInProgress || !status.running}
                    onClick={askRestartCodex}
                  >
                    {busy === "restart" || status.restartInProgress
                      ? <LoaderCircle className="spinner" aria-hidden="true" />
                      : <RefreshCw aria-hidden="true" />}
                    {status.running ? "重启 Codex" : "未运行"}
                  </Button>
                </div>
              </div>

              <div className="operations-status-grid" role="list" aria-label="运行状态">
                {statusCards.map((item) => {
                  const StatusIcon = item.icon;
                  return (
                    <article
                      className={`operations-status-item operations-status-${item.tone}`}
                      key={item.title}
                      role="listitem"
                    >
                      <div className="operations-status-title">
                        <span className="operations-status-icon">
                          <StatusIcon size={16} aria-hidden="true" />
                        </span>
                        <div>
                          <h2>{item.title}</h2>
                          <p>{item.description}</p>
                        </div>
                        <Badge variant={item.tone}>{item.label}</Badge>
                      </div>
                      <div className="operations-status-detail">{item.detail}</div>
                    </article>
                  );
                })}
              </div>

              <div className="maintenance-grid">
                <div className="maintenance-item patch-status">
                  <div className="maintenance-item-heading">
                    <span className="maintenance-item-icon">
                      <Cpu size={16} aria-hidden="true" />
                    </span>
                    <strong>Windows 新版卡顿补丁</strong>
                    <Badge variant={performanceError ? "destructive" : "success"}>
                      {performanceError ? "异常" : "自动生效"}
                    </Badge>
                  </div>
                  <p>
                    {maintenance?.performanceDetail
                      || "启动时隔离 Codex Micro，并停止周期性 WMI 进程采样。"}
                  </p>
                </div>

                <div className="maintenance-item update-status">
                  <div className="maintenance-item-heading">
                    <span className="maintenance-item-icon">
                      <RefreshCw size={16} aria-hidden="true" />
                    </span>
                    <strong>应用更新</strong>
                    <span className="maintenance-heading-badges">
                      <Badge variant="secondary">
                        当前 {status.appVersion ? `v${status.appVersion}` : "读取中"}
                      </Badge>
                      <Badge variant="info">R2 内置</Badge>
                    </span>
                  </div>
                  <div className="maintenance-update-row">
                    <span className={`inline-result ${updateResult.tone}`} aria-live="polite">
                      {updateResult.text || "从公开更新源检查最新稳定版本。"}
                    </span>
                    <span className="maintenance-update-actions">
                      {downloadedUpdate ? (
                        <Button
                          variant="default"
                          size="sm"
                          disabled={isBusy}
                          onClick={askInstallDownloadedUpdate}
                        >
                          {busy === "install-update"
                            ? <LoaderCircle className="spinner" aria-hidden="true" />
                            : <Check aria-hidden="true" />}
                          安装并重启
                        </Button>
                      ) : updateCheck?.updateAvailable && updateCheck.selectedAsset ? (
                        <Button
                          variant="default"
                          size="sm"
                          disabled={isBusy}
                          onClick={() => void downloadUpdate()}
                        >
                          {busy === "download-update"
                            ? <LoaderCircle className="spinner" aria-hidden="true" />
                            : <Download aria-hidden="true" />}
                          下载更新
                        </Button>
                      ) : null}
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={isBusy}
                        onClick={() => void checkForUpdates()}
                      >
                        {busy === "check-update"
                          ? <LoaderCircle className="spinner" aria-hidden="true" />
                          : <RefreshCw aria-hidden="true" />}
                        检查更新
                      </Button>
                    </span>
                  </div>
                </div>

                <div className="maintenance-item path-status">
                  <div className="path-status-layout">
                    <span className="path-status-label">Codex 应用路径</span>
                    <div className="path-display" aria-label="Codex 应用路径">
                      <FolderOpen size={15} aria-hidden="true" />
                      <code>{config.codexAppPath || resolvedCodexPath}</code>
                    </div>
                  </div>
                </div>
              </div>
            </Card>
          </section>

          <div className="configuration-heading">
            <span className="section-kicker">Configuration</span>
            <h2>配置项目</h2>
            <p>线路、模型、客户端策略、通知和诊断日志。</p>
          </div>

          <div className="dashboard-grid">
            <div className="dashboard-main-column">
                <section className="route-section" aria-labelledby="route-title">
                  <div className="section-title">
                    <div>
                      <h2 id="route-title">线路与模型</h2>
                      <p>{ccSwitchStatus.available ? "cc-switch 当前配置" : "本地 Codex 直登配置"}</p>
                    </div>
                    <div className="route-heading-actions">
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={dirty || isBusy}
                        onClick={() => void syncCurrentProvider()}
                      >
                        <RefreshCw className={busy === "sync-provider" ? "spinner" : ""} aria-hidden="true" />
                        同步当前线路
                      </Button>
                    </div>
                  </div>

                  <Card className="route-card">
                    <div className="provider-summary">
                      <div className="provider-identity">
                        <span className="column-icon"><Server size={16} /></span>
                        <div>
                          <strong>{provider.name}</strong>
                          <small>{provider.id}</small>
                        </div>
                      </div>
                      <div className="provider-meta">
                        <div>
                          <span>类型</span>
                          <strong>{provider.official ? "OpenAI 官方" : "第三方 API"}</strong>
                        </div>
                        <div>
                          <span>协议</span>
                          <strong>{provider.protocol === "responses" ? "Responses" : "Chat Completions"}</strong>
                        </div>
                        <div className="provider-endpoint">
                          <span>地址</span>
                          <strong>{provider.official ? "ChatGPT 登录" : provider.baseUrl}</strong>
                        </div>
                        <div>
                          <span>推理上限</span>
                          <strong>极高</strong>
                        </div>
                      </div>
                    </div>

                    <div className="catalog-workspace">
                      <div className="catalog-heading">
                        <div className="column-heading">
                          <span className="column-icon"><GitBranch size={16} /></span>
                          <div>
                            <strong>模型列表</strong>
                            <small>{provider.official ? "官方目录" : `已发现 ${modelState.upstreamModels.length} 个上游模型`}</small>
                          </div>
                        </div>
                        {!provider.official && (
                          <Button
                            variant="secondary"
                            size="sm"
                            disabled={isBusy}
                            onClick={() => void fetchCurrentModels()}
                          >
                            <RefreshCw className={busy === "fetch-models" ? "spinner" : ""} aria-hidden="true" />
                            同步模型
                          </Button>
                        )}
                      </div>

                      <div className="model-groups">
                        <section className="model-group">
                          <div className="model-group-title">
                            <div>
                              <strong>官方模型</strong>
                              <small>{modelState.officialModels.filter((model) => model.supported).length} / {modelState.officialModels.length}</small>
                            </div>
                            <Badge variant="info">OpenAI</Badge>
                          </div>
                          <div className="catalog-model-list">
                            {modelState.officialModels.map((model) => (
                              <div
                                className={`catalog-model-row${model.supported ? "" : " unsupported"}`}
                                key={model.slug}
                                aria-disabled={!model.supported}
                              >
                                <span className="model-availability"><Check size={12} /></span>
                                <div>
                                  <strong>{model.displayName}</strong>
                                  <small>{model.slug}</small>
                                </div>
                                <Badge variant={model.supported ? "success" : "secondary"}>
                                  {model.supported ? "支持" : "不可用"}
                                </Badge>
                              </div>
                            ))}
                            {modelState.officialModels.length === 0 && <div className="empty-state">暂未读取到官方模型</div>}
                          </div>
                        </section>

                        {!provider.official && (
                          <section className="model-group">
                            <div className="model-group-title">
                              <div>
                                <strong>三方模型</strong>
                                <small>{modelState.thirdPartyModels.length}</small>
                              </div>
                              <Badge variant="violet">API</Badge>
                            </div>
                            <div className="catalog-model-list">
                              {modelState.thirdPartyModels.map((model) => (
                                <div className="catalog-model-row third-party" key={model}>
                                  <span className="model-availability"><Check size={12} /></span>
                                  <div>
                                    <strong>{model}</strong>
                                    <small>第三方模型</small>
                                  </div>
                                  <Badge variant="success">已添加</Badge>
                                </div>
                              ))}
                              {modelState.thirdPartyModels.length === 0 && <div className="empty-state">尚未添加三方模型</div>}
                            </div>
                          </section>
                        )}
                      </div>
                    </div>

                    <div className="readonly-note">
                      <Server size={14} />
                      <span>
                        {ccSwitchStatus.available
                          ? "线路配置由 cc-switch 管理，Codey 仅在启动时读取当前线路"
                          : "未安装 cc-switch，Codey 读取本地 Codex 登录与 API 配置"}
                      </span>
                      <Badge variant="secondary">只读</Badge>
                    </div>
                  </Card>
                </section>
            </div>

            <div className="dashboard-side-column">
                <section className="secondary-section" aria-labelledby="runtime-title">
                  <div className="section-title compact">
                    <div>
                      <h2 id="runtime-title">Codex 功能策略</h2>
                      <p>按需精简客户端模块和界面行为。</p>
                    </div>
                  </div>
                  <Card className="secondary-card runtime-card">
                    <div className="feature-grid">
                      <div className={`feature-card ${config.slimCodexPet ? "active" : ""}`}>
                        <div className="feature-card-header">
                          <strong>精简 Codex 宠物模块</strong>
                          <Switch
                            checked={config.slimCodexPet}
                            onCheckedChange={(checked) => editConfig({ ...config, slimCodexPet: checked })}
                            aria-label="精简 Codex 宠物模块"
                          />
                        </div>
                        <div className="feature-card-body">
                          <small>
                            {config.slimCodexPet
                              ? "已停止宠物窗口和相关运行时模块"
                              : "保留 Codex 宠物的完整功能"}
                          </small>
                        </div>
                      </div>

                      <div className={`feature-card ${config.slimCodexVoice ? "active" : ""}`}>
                        <div className="feature-card-header">
                          <strong>精简 Codex 语音模块</strong>
                          <Switch
                            checked={config.slimCodexVoice}
                            onCheckedChange={(checked) => editConfig({ ...config, slimCodexVoice: checked })}
                            aria-label="精简 Codex 语音模块"
                          />
                        </div>
                        <div className="feature-card-body">
                          <small>
                            {config.slimCodexVoice
                              ? "已停止听写、快捷键与音频初始化"
                              : "保留 Codex 语音的完整功能"}
                          </small>
                        </div>
                      </div>

                      <div className={`feature-card ${config.fastContextTools ? "active" : ""}`}>
                        <div className="feature-card-header">
                          <strong>FastCtx 上下文工具</strong>
                          <Switch
                            checked={config.fastContextTools}
                            onCheckedChange={(checked) => editConfig({ ...config, fastContextTools: checked })}
                            aria-label="启用 FastCtx 上下文工具"
                          />
                        </div>
                        <div className="feature-card-body">
                          <small>
                            {config.fastContextTools
                              ? "可显著提高模型完成任务速度和准确性"
                              : "保持 Codex 默认文件工具，不加载额外 MCP"}
                          </small>
                        </div>
                      </div>

                      <div className={`feature-card ${config.disableTraceLogWrites ? "active" : ""}`}>
                        <div className="feature-card-header">
                          <strong>Trace 日志写盘保护</strong>
                          <Switch
                            checked={config.disableTraceLogWrites}
                            onCheckedChange={(checked) => editConfig({
                              ...config,
                              disableTraceLogWrites: checked,
                            })}
                            aria-label="启用 Codex Trace 日志写盘保护"
                          />
                        </div>
                        <div className="feature-card-body">
                          <small>阻止Trace日志持续写入数据库影响硬盘寿命</small>
                        </div>
                      </div>

                      <div className={`feature-card ${config.subagentOptimization ? "active" : ""}`}>
                        <div className="feature-card-header">
                          <div className="feature-card-title">
                            <strong>子代理协作优化</strong>
                            <Badge variant="warning">需支持 GPT-5.6-Luna</Badge>
                          </div>
                          <Switch
                            checked={config.subagentOptimization}
                            disabled={isBusy}
                            aria-busy={busy === "check-subagent-model"}
                            onCheckedChange={(checked) => void updateSubagentOptimization(checked)}
                            aria-label="启用子代理协作优化"
                          />
                        </div>
                        <div className="feature-card-body">
                          <small>
                            {busy === "check-subagent-model"
                              ? `正在校验当前线路是否支持 ${SUBAGENT_MODEL}`
                              : config.subagentOptimization
                              ? "启用v2并行配置"
                              : "保持 Codex 默认子代理配置，不注入协作提示词"}
                          </small>
                        </div>
                      </div>

                      <div className={`feature-card ${config.hideFullAccessWarning ? "active" : ""}`}>
                        <div className="feature-card-header">
                          <strong>屏蔽完全访问安全提示</strong>
                          <Switch
                            checked={config.hideFullAccessWarning}
                            onCheckedChange={(checked) => editConfig({ ...config, hideFullAccessWarning: checked })}
                            aria-label="屏蔽完全访问安全提示"
                          />
                        </div>
                        <div className="feature-card-body">
                          <small>
                            {config.hideFullAccessWarning
                              ? "自动隐藏完全访问模式的原生安全提示"
                              : "保留 Codex 原生安全提示"}
                          </small>
                        </div>
                      </div>

                    </div>
                  </Card>
                </section>
            </div>

            <div className="dashboard-side-column notification-column">
                <section className="secondary-section" aria-labelledby="notification-title">
                  <div className="section-title compact">
                    <div>
                      <h2 id="notification-title">飞书通知</h2>
                      <p>使用 Webhook 发送运行与会话提醒。</p>
                    </div>
                    <div className="enable-control">
                      <span>{config.webhook.enabled ? "已开启" : "已关闭"}</span>
                      <Switch
                        checked={config.webhook.enabled}
                        onCheckedChange={(checked) => updateWebhook({ enabled: checked })}
                        aria-label="启用飞书通知"
                      />
                    </div>
                  </div>
                  <Card className="secondary-card notification-card">
                    <div className="notification-title">
                      <span><BellRing size={16} /></span>
                      <div>
                        <strong>飞书机器人 Webhook</strong>
                        <small>发送完成、失败和等待介入提醒</small>
                      </div>
                    </div>
                    <div className="notification-fields">
                      <label className="field">
                        <span>Webhook 地址</span>
                        <div className="input-shell">
                          <Send size={15} aria-hidden="true" />
                          <Input
                            value={config.webhook.url}
                            onChange={(event) => updateWebhook({ url: event.target.value })}
                            placeholder="https://open.feishu.cn/..."
                            spellCheck={false}
                          />
                        </div>
                      </label>
                    </div>
                    <div className="notification-action">
                      <span className={`inline-result ${webhookResult.tone}`}>
                        {webhookResult.text || "不再保存或发送机器人签名密钥"}
                      </span>
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={isBusy || !config.webhook.url.trim()}
                        onClick={() => void testWebhook()}
                      >
                        {busy === "test-webhook"
                          ? <LoaderCircle className="spinner" aria-hidden="true" />
                          : <Send aria-hidden="true" />}
                        测试通知
                      </Button>
                    </div>
                  </Card>
                </section>
            </div>
          </div>

          <div className="dashboard-trace-column">
            <TraceLogModule
              stats={status.traceLogStats}
              snapshotStale={traceSnapshotStale}
              protectionEnabled={config.disableTraceLogWrites}
              clearBusy={busy === "clear-trace-logs"}
              refreshing={busy === "refresh-trace-stats"}
              disabled={isBusy}
              onClear={askClearTraceLogs}
              onRefresh={() => void refreshTraceLogStats()}
            />
          </div>
        </div>
      </div>

      {notice.text && (
        <div className={`notice-toast ${notice.tone}`} role="status" aria-live="polite">
          {notice.tone === "success"
            ? <CircleCheck size={17} />
            : notice.tone === "error"
              ? <CircleAlert size={17} />
              : <Activity size={17} />}
          <span>{notice.text}</span>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="关闭提示"
            onClick={() => setNotice({ tone: "info", text: "" })}
          >
            <X aria-hidden="true" />
          </Button>
        </div>
      )}

      <Dialog
        open={modelPickerVisible}
        onOpenChange={(open) => {
          if (!isBusy || open) setModelPickerVisible(open);
        }}
      >
        <DialogContent
          className="model-picker-dialog"
          container={portalContainer}
          onEscapeKeyDown={(event) => {
            if (isBusy) event.preventDefault();
          }}
          onPointerDownOutside={(event) => {
            if (isBusy) event.preventDefault();
          }}
        >
          <DialogHeader>
            <DialogTitle>添加三方模型</DialogTitle>
            <DialogDescription>从当前线路发现的上游模型中选择要显示的三方模型。</DialogDescription>
          </DialogHeader>
          <div className="model-picker-toolbar">
            <div className="input-shell">
              <Search size={15} aria-hidden="true" />
              <Input
                value={modelQuery}
                onChange={(event) => setModelQuery(event.target.value)}
                placeholder="搜索模型"
                spellCheck={false}
                aria-label="搜索上游模型"
              />
            </div>
            <div>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setDraftModels(modelState.upstreamModels.filter((model) => !officialSlugs.has(model)))}
              >
                全选三方
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setDraftModels([])}>
                清空
              </Button>
            </div>
          </div>
          <div className="model-picker-list">
            {filteredUpstreamModels.map((model) => {
              const officialModel = officialSlugs.has(model);
              return (
                <label className={`model-picker-row${officialModel ? " official" : ""}`} key={model}>
                  <Checkbox
                    checked={officialModel || draftModels.includes(model)}
                    disabled={officialModel}
                    onCheckedChange={(checked) => toggleDraftModel(model, checked === true)}
                    aria-label={`添加 ${model}`}
                  />
                  <span>{model}</span>
                  {officialModel && <Badge variant="info">官方模型</Badge>}
                </label>
              );
            })}
            {filteredUpstreamModels.length === 0 && <div className="empty-state">没有匹配的模型</div>}
          </div>
          <DialogFooter>
            <Button variant="outline" disabled={isBusy} onClick={() => setModelPickerVisible(false)}>
              取消
            </Button>
            <Button
              disabled={isBusy && busy !== "save-models"}
              onClick={() => void saveModelSelection()}
            >
              {busy === "save-models"
                ? <LoaderCircle className="spinner" aria-hidden="true" />
                : <Check aria-hidden="true" />}
              添加到模型列表
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(confirmation)} onOpenChange={(open) => !open && setConfirmation(null)}>
        <DialogContent className="confirmation-dialog" container={portalContainer}>
          <DialogHeader>
            <DialogTitle>{confirmation?.title}</DialogTitle>
            <DialogDescription>{confirmation?.description}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmation(null)}>取消</Button>
            <Button
              variant={confirmation?.action === "clear" ? "destructive" : "default"}
              onClick={() => {
                const pending = confirmation;
                setConfirmation(null);
                pending?.run();
              }}
            >
              {confirmation?.action === "clear"
                ? <Trash2 aria-hidden="true" />
                : confirmation?.action === "restart"
                ? <RefreshCw aria-hidden="true" />
                : <Check aria-hidden="true" />}
              {confirmation?.confirmLabel}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </main>
  );
}
