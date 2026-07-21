import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  BellRing,
  Check,
  CircleAlert,
  CircleCheck,
  Cpu,
  GitBranch,
  History,
  LoaderCircle,
  PlugZap,
  Power,
  RefreshCw,
  Save,
  Search,
  Send,
  Server,
  Settings2,
  Sparkles,
  Trash2,
  X,
  Zap,
} from "lucide-react";
import { invoke } from "./api";
import { formatBytes, TraceLogModule, type TraceLogStats } from "./TraceLogModule";
import {
  BorderBeam,
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
  restartRequired?: boolean;
  closeInProgress?: boolean;
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
  action: "clear" | "close";
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

type AppProps = {
  embedded?: boolean;
  onClose?: () => void;
};

const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

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
    if (!status.closeInProgress) return;
    let cancelled = false;
    let timer = 0;
    const poll = () => {
      timer = window.setTimeout(async () => {
        try {
          const next = await invoke<RuntimeStatus>("runtime_status");
          if (cancelled) return;
          setStatus(next);
          if (next.closeInProgress) poll();
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
  }, [status.closeInProgress]);

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

  function askClearTraceLogs() {
    setConfirmation({
      action: "clear",
      title: "清理 Codex 日志库？",
      description: "将清空并压缩 logs_*.sqlite，只删除本地诊断/Trace 日志，不影响聊天历史、账号、配置或插件。清理后的诊断记录无法恢复。",
      confirmLabel: "确认清理",
      run: () => void clearTraceLogs(),
    });
  }

  function askCloseCodex() {
    setConfirmation({
      action: "close",
      title: "关闭 Codex？",
      description: "当前 Codex 将被关闭，正在执行的本地任务会被中断。关闭完成后会显示系统提示，请按提示手动运行 Codey 重新启动。",
      confirmLabel: "关闭 Codex",
      run: () => void closeCodex(),
    });
  }

  async function closeCodex() {
    if (!config) return;
    await runOperation("close", async () => {
      if (dirty) await persist(config);
      setNotice({
        tone: "info",
        text: "正在关闭 Codex，关闭后请按系统提示手动运行 Codey…",
      });
      await invoke("close_codex");
      setStatus((current) => ({
        ...current,
        closeInProgress: true,
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
        text: `已清理 ${cleanup.databasesCleaned} 个日志库、${cleanup.rowsDeleted} 条记录，释放 ${formatBytes(cleanup.bytesReclaimed)}；${protectionDetail}，统计将在下次 Codey 启动时更新`,
      });
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
  const performanceOk = maintenance?.performanceStatus === "ready";
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
    fullDetail?: boolean;
  }> = [
    {
      title: "会话恢复",
      description: sessionOk ? "会话索引与恢复链路工作正常。" : "正在确认会话索引与恢复链路。",
      detail: maintenance?.sessionDetail || "等待 Codey 返回会话状态",
      label: sessionOk ? "正常" : maintenance ? "需检查" : "检查中",
      tone: sessionOk ? "success" : maintenance ? "destructive" : "warning",
      icon: History,
      fullDetail: true,
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
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark"><Sparkles size={17} /></span>
          <div>
            <strong>Codey</strong>
            <span>AI 控制中心</span>
          </div>
        </div>
        <div className="topbar-actions">
          <Badge
            className="header-route-tag"
            variant={provider.official ? "info" : "violet"}
          >
            <Zap size={13} aria-hidden="true" />
            当前线路 · {provider.name}
          </Badge>
          <div className={`runtime-summary ${status.running ? "online" : ""}`}>
            <span className="runtime-dot" />
            <div>
              <strong>{status.running ? "服务运行中" : "服务未运行"}</strong>
              <small>{status.running ? "Codex 已连接" : "等待启动"}</small>
            </div>
          </div>
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
      </header>

      <div className="page-scroll">
        <div className="page" id="codey-settings-content">
          <section className="status-overview" aria-labelledby="status-overview-title">
            <div className="section-title overview-title">
              <div>
                <span className="section-kicker">Live overview</span>
                <h1 id="status-overview-title">当前运行状态</h1>
                <p>状态来自 Codey 运行时与 Codex 客户端，本页打开时自动同步。</p>
              </div>
              <Badge variant="outline">
                <Activity size={12} aria-hidden="true" />
                实时快照
              </Badge>
            </div>
            <div className="status-grid">
              {statusCards.map((item, index) => {
                const Icon = item.icon;
                return (
                  <Card
                    className={`status-card status-card-${item.tone}`}
                    key={item.title}
                    gradientFrom={item.tone === "success" ? "#10b981" : "#8b5cf6"}
                    gradientTo={item.tone === "destructive" ? "#ef4444" : "#38bdf8"}
                  >
                    {index === 0 && sessionOk && (
                      <BorderBeam colorFrom="#10b981" colorTo="#38bdf8" duration={10} />
                    )}
                    <div className="status-card-head">
                      <span className="status-card-icon"><Icon size={18} /></span>
                      <Badge variant={item.tone}>{item.label}</Badge>
                    </div>
                    <div className="status-card-copy">
                      <h3>{item.title}</h3>
                      <p>{item.description}</p>
                    </div>
                    <span className={`status-card-detail${item.fullDetail ? " status-card-detail-full" : ""}`}>
                      {item.detail}
                    </span>
                  </Card>
                );
              })}
            </div>
          </section>

          <div className="configuration-heading">
            <span className="section-kicker">Configuration</span>
            <h2>其他配置</h2>
            <p>管理线路与模型、应用精简策略，以及通知和日志。</p>
          </div>

          <div className={`runtime-action-bar${restartPending ? " pending" : ""}`}>
            <span className="runtime-action-icon">
              <RefreshCw
                className={busy === "close" || status.closeInProgress ? "spinner" : ""}
                size={17}
                aria-hidden="true"
              />
            </span>
            <div className="runtime-action-copy" aria-live="polite">
              <strong>
                {restartPending
                  ? "配置等待重启"
                  : status.running
                    ? "当前配置已应用"
                    : "Codex 尚未启动"}
              </strong>
              <small>
                {restartPending
                  ? "模型目录或启动参数将在重启后生效"
                  : status.running
                    ? "当前线路与运行参数已载入"
                    : "请手动运行 Codey 应用当前配置"}
              </small>
            </div>
            <Button
              variant={restartPending ? "default" : "outline"}
              size="sm"
              disabled={isBusy || status.closeInProgress || !status.running}
              onClick={askCloseCodex}
            >
              {busy === "close" || status.closeInProgress
                ? <LoaderCircle className="spinner" aria-hidden="true" />
                : <Power aria-hidden="true" />}
              {status.running ? "关闭 Codex" : "请手动启动 Codey"}
            </Button>
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
                  <h2 id="runtime-title">系统与应用</h2>
                  <p>精简策略和基础运行参数。</p>
                </div>
              </div>
              <Card className="secondary-card runtime-card">
                <div className="health-list">
                  <div className="health-row">
                    <span className={`health-icon ${config.slimCodexPet ? "ready" : ""}`}>
                      <Zap size={16} />
                    </span>
                  <div>
                      <strong>精简 Codex 宠物模块</strong>
                      <small>
                        {config.slimCodexPet
                          ? "已停止宠物窗口和相关运行时模块"
                          : "保留 Codex 宠物的完整功能"}
                      </small>
                    </div>
                    <Switch
                      checked={config.slimCodexPet}
                      onCheckedChange={(checked) => editConfig({ ...config, slimCodexPet: checked })}
                      aria-label="精简 Codex 宠物模块"
                    />
                  </div>
                  <div className="health-row">
                    <span className={`health-icon ${config.slimCodexVoice ? "ready" : ""}`}>
                      <Zap size={16} />
                    </span>
                  <div>
                      <strong>精简 Codex 语音模块</strong>
                      <small>
                        {config.slimCodexVoice
                          ? "已停止听写、快捷键与音频初始化"
                          : "保留 Codex 语音的完整功能"}
                      </small>
                    </div>
                    <Switch
                      checked={config.slimCodexVoice}
                      onCheckedChange={(checked) => editConfig({ ...config, slimCodexVoice: checked })}
                      aria-label="精简 Codex 语音模块"
                    />
                  </div>
                  <div className="health-row">
                    <span className={`health-icon ${config.fastContextTools ? "ready" : ""}`}>
                      <Sparkles size={16} />
                    </span>
                    <div>
                      <strong>FastCtx 上下文工具</strong>
                      <small>
                        {config.fastContextTools
                          ? "可显著提高模型完成任务速度和准确性"
                          : "保持 Codex 默认文件工具，不加载额外 MCP"}
                      </small>
                    </div>
                    <Switch
                      checked={config.fastContextTools}
                      onCheckedChange={(checked) => editConfig({ ...config, fastContextTools: checked })}
                      aria-label="启用 FastCtx 上下文工具"
                    />
                  </div>
                  <div className="health-row">
                    <span className={`health-icon ${config.subagentOptimization ? "ready" : ""}`}>
                      <GitBranch size={16} />
                    </span>
                    <div>
                      <strong>子代理协作优化</strong>
                      <small>
                        {config.subagentOptimization
                          ? "下次启动启用 V2 并行配置，退出时自动恢复原文件"
                          : "保持 Codex 默认子代理配置，不注入协作提示词"}
                      </small>
                    </div>
                    <Switch
                      checked={config.subagentOptimization}
                      onCheckedChange={(checked) => editConfig({ ...config, subagentOptimization: checked })}
                      aria-label="启用子代理协作优化"
                    />
                  </div>
                  <div className="health-row">
                    <span className={`health-icon ${performanceOk ? "ready" : performanceError ? "error" : ""}`}>
                      <Zap size={16} />
                    </span>
                    <div>
                      <strong>Windows 新版卡顿补丁</strong>
                      <small>
                        {maintenance?.performanceDetail
                          || "启动时隔离 Codex Micro，并停止每 30 秒触发的 WMI 进程采样"}
                      </small>
                    </div>
                    <Badge
                      variant={performanceError ? "destructive" : "success"}
                    >
                      {performanceError ? "异常" : "自动"}
                    </Badge>
                  </div>
                </div>

                <div className="runtime-fields">
                  <label className="field">
                    <span>Codex 应用路径</span>
                    <Input
                      value={config.codexAppPath}
                      onChange={(event) => editConfig({ ...config, codexAppPath: event.target.value })}
                      placeholder={resolvedCodexPath}
                      spellCheck={false}
                    />
                  </label>
                </div>
              </Card>
            </section>

            <section className="secondary-section" aria-labelledby="notification-title">
              <div className="section-title compact">
                <div>
                  <h2 id="notification-title">飞书通知</h2>
                  <p>仅使用 Webhook 地址发送会话状态。</p>
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
                    <strong>飞书机器人</strong>
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
              busy={busy === "clear-trace-logs"}
              disabled={isBusy}
              onProtectionChange={(checked) => editConfig({
                ...config,
                disableTraceLogWrites: checked,
              })}
              onClear={askClearTraceLogs}
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
              variant={confirmation?.action === "close" ? "default" : "destructive"}
              onClick={() => {
                const pending = confirmation;
                setConfirmation(null);
                pending?.run();
              }}
            >
              {confirmation?.action === "close"
                ? <Power aria-hidden="true" />
                : <Trash2 aria-hidden="true" />}
              {confirmation?.confirmLabel}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </main>
  );
}
