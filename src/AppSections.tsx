import { type CSSProperties, useRef, useState } from "react";
import {
  IconActivity as Activity,
  IconAdjustmentsHorizontal,
  IconBell as BellRing,
  IconBrandWindows,
  IconClock,
  IconCode as Code,
  IconCheck as Check,
  IconCloudCheck,
  IconCpu,
  IconDatabase,
  IconDownload,
  IconFileCheck,
  IconFolderOpen,
  IconGitBranch,
  IconHistory,
  IconLoader2 as LoaderCircle,
  IconPlugConnected as PlugZap,
  IconRefresh as RefreshCw,
  IconSend,
  IconServer,
  IconShieldCheck,
  IconShoppingBag,
  IconX as X,
  IconBolt as Zap,
} from "@tabler/icons-react";

import type {
  CcSwitchStatus,
  Config,
  InlineResult,
  ModelState,
  PluginMarketplaceStatus,
  RuntimeStatus,
  UpdateCheck,
  UpdateDownload,
} from "./App.types";
import { Badge, Button, Card, Input, Switch, Tooltip } from "./components/semi";

const Cpu = IconCpu;
const Download = IconDownload;
const FolderOpen = IconFolderOpen;
const GitBranch = IconGitBranch;
const History = IconHistory;
const Send = IconSend;
const Server = IconServer;

const GPU_LAUNCH_MODES = [
  { value: "off", label: "关闭" },
  { value: "disableGpu", label: "禁用 GPU" },
  { value: "disableGpuRasterization", label: "禁用 GPU 栅格化" },
] as const satisfies ReadonlyArray<{
  value: Config["gpuLaunchMode"];
  label: string;
}>;

type OperationsPanelProps = {
  config: Config;
  status: RuntimeStatus;
  busy: string | null;
  isBusy: boolean;
  pluginMarketplaceStatus: PluginMarketplaceStatus | null;
  onRepairPluginMarketplace: () => void;
  onRestart: () => void;
};

export function OperationsPanel({
  config,
  status,
  busy,
  isBusy,
  pluginMarketplaceStatus,
  onRepairPluginMarketplace,
  onRestart,
}: OperationsPanelProps) {
  const operationsHubRef = useRef<HTMLElement>(null);
  const [activeCardTitle, setActiveCardTitle] = useState<string | null>(null);

  const getTooltipContainer = () =>
    operationsHubRef.current?.closest<HTMLElement>(".app-shell") ?? document.body;

  const toggleCard = (title: string) => {
    setActiveCardTitle((prev) => (prev === title ? null : title));
  };

  const maintenance = status.maintenance;
  const sessionOk = maintenance?.sessionStatus === "ready";
  const pluginOk = pluginMarketplaceStatus?.status === "ready";
  const pluginStatusError = pluginMarketplaceStatus?.status === "error";
  const pluginRepairing = busy === "repair-plugin-marketplace";
  const performanceError = maintenance?.performanceStatus === "error";
  const injectionScripts = status.injectionScripts ?? [];
  const effectiveScriptCount = injectionScripts.filter(
    (script) => script.status === "effective",
  ).length;
  const unverifiedInjectionScripts = injectionScripts.filter(
    (script) => script.status === "executed",
  );
  const failedInjectionScripts = injectionScripts.filter(
    (script) => script.status === "failed" || script.status === "unknown",
  );
  const injectionStatusPending = injectionScripts.length === 0;
  const injectionError = failedInjectionScripts.length > 0;
  const isWindowsClient = status.clientPlatform === "windows";
  const windowsPatchReady = maintenance?.performanceStatus === "ready";
  const windowsPatchFailed = performanceError || Boolean(status.startupError);
  const windowsPatchTone = windowsPatchReady
    ? "success"
    : windowsPatchFailed
      ? "destructive"
      : "warning";
  const windowsPatchLabel = windowsPatchReady
    ? "已启用"
    : windowsPatchFailed
      ? "未生效"
      : "待检测";
  const windowsPatchDetail = windowsPatchReady
    ? maintenance?.performanceDetail
      || "WMI 周期采样、临时 WebView 残留与执行环境泄漏修复已生效。"
    : windowsPatchFailed
      ? maintenance?.performanceDetail
        || status.startupError
        || "Windows 优化补丁加载异常。"
      : status.running
        ? "正在确认 Windows 优化补丁状态。"
        : "将在 Codex 启动时自动安装并校验 Windows 优化补丁。";
  const resolvedCodexPath = status.codexAppPath || "/Applications/ChatGPT.app";
  const restartPending = Boolean(status.restartRequired);
  const pluginIssues = [
    pluginMarketplaceStatus?.officialMarketplace === false ? "官方市场快照缺失" : "",
    pluginMarketplaceStatus?.officialMarketplace && pluginMarketplaceStatus.officialRegistered === false
      ? "官方市场尚未注册"
      : "",
    pluginMarketplaceStatus?.remoteMarketplace === false ? "远程市场快照缺失" : "",
    pluginMarketplaceStatus?.remoteMarketplace && pluginMarketplaceStatus.remoteRegistered === false
      ? "远程市场尚未注册"
      : "",
  ].filter(Boolean);

  type MetricItem = {
    id: string;
    icon: typeof Activity;
    tooltip: string;
    tone?: "success" | "warning" | "destructive" | "info";
  };

  // Session Recovery Metrics
  const sessionDetailStr = maintenance?.sessionDetail || "";
  const filesMatch = sessionDetailStr.match(/修复 (\d+) 个会话文件/);
  const rowsMatch = sessionDetailStr.match(/更新 (\d+) 行数据库索引/);
  const ghostMatch = sessionDetailStr.match(/清理 (\d+) 条幽灵任务/);

  const filesCount = filesMatch ? filesMatch[1] : "0";
  const rowsCount = rowsMatch ? rowsMatch[1] : "0";
  const ghostCount = ghostMatch ? ghostMatch[1] : "0";

  const sessionMetrics: MetricItem[] = [
    {
      id: "session-files",
      icon: IconFileCheck,
      tooltip: `会话文件：已修复 ${filesCount} 个会话文件`,
      tone: sessionOk ? "success" : "warning",
    },
    {
      id: "session-db",
      icon: IconDatabase,
      tooltip: `数据库索引：已更新 ${rowsCount} 行数据库索引`,
      tone: sessionOk ? "success" : "warning",
    },
    {
      id: "session-ghost",
      icon: IconShieldCheck,
      tooltip: `幽灵任务：已清理 ${ghostCount} 条幽灵任务`,
      tone: sessionOk ? "success" : "warning",
    },
  ];

  // System Optimization Metrics
  const optimizationMetrics: MetricItem[] = [
    {
      id: "opt-fastctx",
      icon: Zap,
      tooltip: config.fastContextTools
        ? "FastCtx 上下文加速：已按当前配置启用"
        : "FastCtx 上下文加速：未启用",
      tone: config.fastContextTools ? "success" : "info",
    },
    {
      id: "opt-slim",
      icon: IconAdjustmentsHorizontal,
      tooltip: (config.slimCodexPet || config.slimCodexVoice)
        ? `客户端精简：已开启${config.slimCodexPet ? "宠物" : ""}${config.slimCodexVoice ? "/语音" : ""}精简`
        : "客户端精简：保留完整功能",
      tone: (config.slimCodexPet || config.slimCodexVoice) ? "success" : "info",
    },
    {
      id: "opt-patch",
      icon: isWindowsClient ? IconBrandWindows : IconCpu,
      tooltip: windowsPatchReady
        ? "性能策略已生效：采样与泄漏修复"
        : "性能策略：运行确认中",
      tone: windowsPatchReady ? "success" : "warning",
    },
    {
      id: "opt-injection",
      icon: Code,
      tooltip: injectionError
        ? `脚本注入：${failedInjectionScripts.length} 个异常`
        : unverifiedInjectionScripts.length > 0
          ? `脚本注入：${unverifiedInjectionScripts.length} 个未验证生效`
          : injectionScripts.length > 0
            ? `脚本注入：${effectiveScriptCount}/${injectionScripts.length} 已生效`
            : "脚本注入：等待 Codex 启动后检测",
      tone: injectionError
        ? "destructive"
        : unverifiedInjectionScripts.length > 0
          ? "warning"
          : injectionScripts.length > 0
            ? "success"
            : "warning",
    },
  ];

  // Plugin Marketplace Metrics
  const pluginMetrics: MetricItem[] = [
    {
      id: "plugin-official",
      icon: IconShoppingBag,
      tooltip: pluginMarketplaceStatus?.officialMarketplace !== false
        ? "官方市场：快照与注册完整"
        : "官方市场：快照缺失或尚未注册",
      tone: pluginMarketplaceStatus?.officialMarketplace !== false ? "success" : "warning",
    },
    {
      id: "plugin-remote",
      icon: IconCloudCheck,
      tooltip: pluginMarketplaceStatus?.remoteMarketplace !== false
        ? "远程市场：快照与注册完整"
        : "远程市场：快照缺失或尚未注册",
      tone: pluginMarketplaceStatus?.remoteMarketplace !== false ? "success" : "warning",
    },
    {
      id: "plugin-host",
      icon: PlugZap,
      tooltip: pluginOk
        ? "插件托管：插件服务正常且链路已就绪"
        : "插件托管：正在检查或等待修复",
      tone: pluginOk ? "success" : "warning",
    },
  ];

  const statusCards: Array<{
    title: string;
    description: string;
    metrics: MetricItem[];
    label: string;
    tone: "success" | "warning" | "destructive" | "info";
    icon: typeof Activity;
    action?: {
      label: string;
      disabled: boolean;
      loading: boolean;
      onClick: () => void;
    };
    showInjectionScripts?: boolean;
  }> = [
    {
      title: "会话恢复",
      description: sessionOk ? "索引与恢复链路运行正常，上下文恢复就绪。" : "正在确认会话索引与恢复链路。",
      metrics: sessionMetrics,
      label: sessionOk ? "正常" : maintenance ? "需检查" : "检查中",
      tone: sessionOk ? "success" : maintenance ? "destructive" : "warning",
      icon: History,
    },
    {
      title: "系统优化",
      description: injectionError
        ? `${failedInjectionScripts.length} 个脚本注入异常，可展开查看错误。`
        : unverifiedInjectionScripts.length > 0
          ? `${unverifiedInjectionScripts.length} 个脚本已执行，但未验证实际效果。`
          : injectionStatusPending
            ? status.running
              ? "正在读取最近一次脚本注入结果。"
              : "Codex 启动后将记录每个脚本的注入结果。"
            : !performanceError
              ? "精简策略、性能补丁与脚本生效自检均已通过。"
              : "部分精简策略尚未启用，保留完整功能。",
      metrics: optimizationMetrics,
      label: injectionError
        ? `${failedInjectionScripts.length} 个异常`
        : unverifiedInjectionScripts.length > 0
          ? `${unverifiedInjectionScripts.length} 个未验证`
          : injectionStatusPending
            ? status.running
              ? "检测中"
              : "待启动"
            : performanceError
              ? "异常"
              : "已优化",
      tone: injectionError || performanceError
        ? "destructive"
        : injectionStatusPending || unverifiedInjectionScripts.length > 0
          ? "warning"
          : "success",
      icon: Cpu,
      showInjectionScripts: true,
    },
    {
      title: "插件市场",
      description: pluginOk
        ? "配置状态完整，可正常发现与管理插件。"
        : "仅检查当前状态，不会在打开配置页时自动修复。",
      metrics: pluginMetrics,
      label: pluginRepairing
        ? "修复中"
        : pluginOk
          ? "正常"
          : pluginStatusError
            ? "读取失败"
            : pluginMarketplaceStatus
              ? "需修复"
              : "检查中",
      tone: pluginOk ? "success" : pluginStatusError ? "destructive" : "warning",
      icon: PlugZap,
      action: {
        label: pluginOk ? "重新检查并修复" : "手动修复",
        disabled: isBusy,
        loading: pluginRepairing,
        onClick: onRepairPluginMarketplace,
      },
    },
  ];

  return (
    <section
      ref={operationsHubRef}
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
              <h1 id="operations-title">Codex 运行状态</h1>
              <div className="path-display header-path-display" aria-label="Codex 应用路径">
                <FolderOpen size={14} aria-hidden="true" />
                <code>{config.codexAppPath || resolvedCodexPath}</code>
              </div>
            </div>
          </div>

          <div className="operations-actions">
            <div className="operations-status-icons" role="list" aria-label="核心服务状态">
              {statusCards.map((item) => {
                const StatusIcon = item.icon;
                const isExpanded = activeCardTitle === item.title;
                return (
                  <Tooltip
                    key={item.title}
                    content={isExpanded ? `收起“${item.title}”` : `点击展开“${item.title}”详情`}
                    getPopupContainer={getTooltipContainer}
                    position="top"
                  >
                    <button
                      type="button"
                      className={`operations-icon-badge tone-${item.tone}${isExpanded ? " active" : ""}`}
                      onClick={() => toggleCard(item.title)}
                      aria-expanded={isExpanded}
                      aria-label={`${item.title}（${item.label}），点击${isExpanded ? "收起" : "展开"}`}
                    >
                      <StatusIcon size={16} aria-hidden="true" />
                      <span className="operations-icon-dot" aria-hidden="true" />
                    </button>
                  </Tooltip>
                );
              })}
            </div>

            <Badge variant={restartPending ? "warning" : status.running ? "success" : "secondary"}>
              <span className="operations-status-dot" aria-hidden="true" />
              {restartPending ? "等待重启" : status.running ? "运行中" : "未启动"}
            </Badge>
            <Button
              variant="warning"
              size="sm"
              disabled={isBusy || status.restartInProgress || !status.running}
              onClick={onRestart}
            >
              {busy === "restart" || status.restartInProgress
                ? <LoaderCircle className="spinner" aria-hidden="true" />
                : <RefreshCw aria-hidden="true" />}
              {status.running ? "重启 Codex" : "未运行"}
            </Button>
          </div>
        </div>

        {activeCardTitle && (
          <div className="operations-expanded-grid" role="region" aria-label="展开的系统详情">
            {statusCards
              .filter((item) => item.title === activeCardTitle)
              .map((item) => {
                const StatusIcon = item.icon;
                return (
                  <article key={item.title} className={`operations-expanded-card tone-${item.tone}`}>
                    <div className="expanded-card-header">
                      <div className="expanded-card-title">
                        <span className={`expanded-card-icon tone-${item.tone}`}>
                          <StatusIcon size={18} aria-hidden="true" />
                        </span>
                        <div>
                          <h3>{item.title}</h3>
                          <p>{item.description}</p>
                        </div>
                      </div>
                      <div className="expanded-card-actions">
                        <Badge variant={item.tone}>{item.label}</Badge>
                      </div>
                    </div>

                    <div className="expanded-card-body">
                      <div className="expanded-card-metrics">
                        {item.metrics.map((metric) => {
                          const MetricIcon = metric.icon;
                          return (
                            <div key={metric.id} className="expanded-metric-item">
                              <span className={`expanded-metric-icon tone-${metric.tone || "info"}`}>
                                <MetricIcon size={14} aria-hidden="true" />
                              </span>
                              <span className="expanded-metric-text">{metric.tooltip}</span>
                            </div>
                          );
                        })}
                      </div>

                      {item.showInjectionScripts && (
                        <section className="injection-status-section" aria-labelledby="injection-status-title">
                          <div className="injection-status-header">
                            <h4 id="injection-status-title">脚本生效状态</h4>
                            <span className="injection-status-summary">
                              {injectionScripts.length > 0
                                ? `${effectiveScriptCount}/${injectionScripts.length} 已生效`
                                : "暂无结果"}
                            </span>
                          </div>

                          {injectionScripts.length > 0 ? (
                            <div className="injection-status-list" role="list">
                              {injectionScripts.map((script) => {
                                const scriptEffective = script.status === "effective";
                                const scriptUnverified = script.status === "executed";
                                const scriptFailed = !scriptEffective && !scriptUnverified;
                                const ScriptStatusIcon = scriptEffective
                                  ? Check
                                  : scriptUnverified
                                    ? IconClock
                                    : X;
                                const stateClass = scriptFailed
                                  ? " failed"
                                  : scriptUnverified
                                    ? " unverified"
                                    : "";
                                return (
                                  <div
                                    key={script.id}
                                    className={`injection-status-row${stateClass}`}
                                    role="listitem"
                                  >
                                    <span className="injection-script-icon" aria-hidden="true">
                                      <Code size={15} />
                                    </span>
                                    <div className="injection-script-copy">
                                      <div className="injection-script-title">
                                        <span>{script.name}</span>
                                        <span className="injection-script-source">
                                          {script.source === "user" ? "用户脚本" : "内置"}
                                        </span>
                                      </div>
                                      {script.error && (
                                        <code className="injection-script-error">{script.error}</code>
                                      )}
                                      {!script.error && script.detail && (
                                        <span className="injection-script-detail">{script.detail}</span>
                                      )}
                                    </div>
                                    <span
                                      className={`injection-script-state${stateClass}`}
                                      title={scriptEffective
                                        ? "生效探针通过"
                                        : scriptUnverified
                                          ? "脚本已执行，但没有生效证据"
                                          : "脚本异常"}
                                    >
                                      <ScriptStatusIcon size={14} aria-hidden="true" />
                                      {scriptEffective
                                        ? "已生效"
                                        : scriptUnverified
                                          ? "未验证"
                                          : "异常"}
                                    </span>
                                  </div>
                                );
                              })}
                            </div>
                          ) : (
                            <div className="injection-status-empty">
                              {status.running
                                ? "正在等待最近一次脚本注入结果"
                                : "Codex 启动后将在这里显示每个脚本的注入状态"}
                            </div>
                          )}
                        </section>
                      )}

                      {item.action && (
                        <div className="expanded-card-footer">
                          <Button
                            variant="outline"
                            size="xs"
                            disabled={item.action.disabled}
                            onClick={item.action.onClick}
                          >
                            {item.action.loading
                              ? <LoaderCircle className="spinner" aria-hidden="true" />
                              : <RefreshCw aria-hidden="true" />}
                            {item.action.label}
                          </Button>
                        </div>
                      )}
                    </div>
                  </article>
                );
              })}
          </div>
        )}

        {isWindowsClient && (
          <div
            className={`windows-patch-status windows-patch-status-${windowsPatchTone}`}
            role="status"
            aria-live="polite"
          >
            <span className="windows-patch-icon">
              <IconBrandWindows size={18} aria-hidden="true" />
            </span>
            <div className="windows-patch-copy">
              <div className="windows-patch-heading">
                <strong>Windows 优化补丁</strong>
                <Badge variant={windowsPatchTone}>{windowsPatchLabel}</Badge>
              </div>
              <p>{windowsPatchDetail}</p>
            </div>
          </div>
        )}
      </Card>
    </section>
  );
}

type AppUpdateCardProps = {
  status: RuntimeStatus;
  updateResult: InlineResult;
  updateCheck: UpdateCheck | null;
  downloadedUpdate: UpdateDownload | null;
  busy: string | null;
  isBusy: boolean;
  onCheckUpdates: () => void;
  onDownloadUpdate: () => void;
  onInstallUpdate: () => void;
};

export function AppUpdateCard({
  status,
  updateResult,
  updateCheck,
  downloadedUpdate,
  busy,
  isBusy,
  onCheckUpdates,
  onDownloadUpdate,
  onInstallUpdate,
}: AppUpdateCardProps) {
  return (
    <section className="secondary-section" aria-labelledby="update-title">
      <div className="section-title compact">
        <div>
          <h2 id="update-title">应用更新</h2>
          <p>检查软件版本与在线更新</p>
        </div>
      </div>
      <Card className="secondary-card update-card">
        <div className="update-card-header">
          <div className="update-card-title">
            <span className="column-icon"><RefreshCw size={16} /></span>
            <div>
              <strong>应用更新</strong>
              <small>当前版本 {status.appVersion ? `v${status.appVersion}` : "读取中"}</small>
            </div>
          </div>
          <Badge variant={updateCheck?.updateAvailable ? "warning" : "secondary"}>
            {updateCheck?.updateAvailable ? "发现新版本" : "已是最新"}
          </Badge>
        </div>

        <div className="update-card-content">
          <div className="update-status-msg">
            <span className={`inline-result ${updateResult.tone}`} aria-live="polite">
              {updateResult.text || "从公开更新源检查最新稳定版本。"}
            </span>
          </div>
          <div className="update-actions-row">
            {downloadedUpdate ? (
              <Button
                variant="default"
                size="sm"
                disabled={isBusy}
                onClick={onInstallUpdate}
              >
                {busy === "install-update"
                  ? <LoaderCircle className="spinner" aria-hidden="true" />
                  : <Check aria-hidden="true" />}
                安装并重启
              </Button>
            ) : updateCheck?.updateAvailable && updateCheck.selectedAsset ? (
              <Button
                variant="default"
                size="xs"
                disabled={isBusy}
                onClick={onDownloadUpdate}
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
              onClick={onCheckUpdates}
            >
              {busy === "check-update"
                ? <LoaderCircle className="spinner" aria-hidden="true" />
                : <RefreshCw aria-hidden="true" />}
              检查更新
            </Button>
          </div>
        </div>
      </Card>
    </section>
  );
}

type ModelSectionProps = {
  ccSwitchStatus: CcSwitchStatus;
  provider: CcSwitchStatus["provider"];
  modelState: ModelState;
  dirty: boolean;
  isBusy: boolean;
  busy: string | null;
  onSyncCurrentProvider: () => void;
  onFetchCurrentModels: () => void;
  onSetDefaultModel: (model: string) => void;
};

export function ModelSection({
  ccSwitchStatus,
  provider,
  modelState,
  dirty,
  isBusy,
  busy,
  onSyncCurrentProvider,
  onFetchCurrentModels,
  onSetDefaultModel,
}: ModelSectionProps) {
  const defaultModel = modelState.defaultModel;
  return (
    <section className="route-section" aria-labelledby="route-title">
      <div className="section-title">
        <div>
          <h2 id="route-title">线路与模型</h2>
          <p>{ccSwitchStatus.available ? "cc-switch 当前配置" : "本地 Codex 直登配置"}</p>
        </div>
        <div className="route-heading-actions">
          <Button
            variant="outline"
            size="sm"
            disabled={dirty || isBusy}
            onClick={onSyncCurrentProvider}
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
            <div className="provider-name-box">
              <strong>{provider.name}</strong>
            </div>
          </div>
          <div className="provider-meta">
            <div>
              <span>类型</span>
              <strong>{provider.official ? "OpenAI 官方" : "第三方 API"}</strong>
            </div>
            <div className="provider-endpoint">
              <span>地址</span>
              <strong>{provider.official ? "ChatGPT 登录" : provider.baseUrl}</strong>
            </div>
            <div>
              <span>默认模型</span>
              <strong>{defaultModel || "未配置"}</strong>
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
                onClick={onFetchCurrentModels}
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
                {modelState.officialModels.map((model) => {
                  const isDefault = defaultModel === model.slug;
                  return (
                    <div
                      className={`catalog-model-row${model.supported ? "" : " unsupported"}${isDefault ? " default-model" : ""}`}
                      key={model.slug}
                      aria-disabled={!model.supported}
                    >
                      <span className="model-availability">
                        {model.supported ? <Check size={12} /> : <X size={12} />}
                      </span>
                      <div>
                        <strong>{model.displayName}</strong>
                        <small>{model.slug}</small>
                      </div>
                      <div className="catalog-model-actions">
                        {isDefault && <Badge variant="brand">默认</Badge>}
                        {model.supported && !isDefault && (
                          <Button
                            variant="ghost"
                            size="xs"
                            disabled={isBusy}
                            onClick={() => onSetDefaultModel(model.slug)}
                          >
                            设为默认
                          </Button>
                        )}
                      </div>
                    </div>
                  );
                })}
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
                  <Badge variant="brand">API</Badge>
                </div>
                <div
                  className={modelState.thirdPartyModels.length === 0
                    ? "catalog-model-list catalog-model-list-empty"
                    : "catalog-model-list"}
                >
                  {modelState.thirdPartyModels.map((model) => {
                    const isDefault = defaultModel === model;
                    return (
                      <div className={`catalog-model-row third-party${isDefault ? " default-model" : ""}`} key={model}>
                        <span className="model-availability"><Check size={12} /></span>
                        <div>
                          <strong>{model}</strong>
                          <small>第三方模型</small>
                        </div>
                        <div className="catalog-model-actions">
                          {isDefault && <Badge variant="brand">默认</Badge>}
                          {!isDefault && (
                            <Button
                              variant="ghost"
                              size="xs"
                              disabled={isBusy}
                              onClick={() => onSetDefaultModel(model)}
                            >
                              设为默认
                            </Button>
                          )}
                        </div>
                      </div>
                    );
                  })}
                  {modelState.thirdPartyModels.length === 0 && (
                    <div
                      className="catalog-empty-state"
                      role="region"
                      aria-labelledby="third-party-empty-title"
                      aria-busy={busy === "fetch-models"}
                    >
                      <span className="catalog-empty-icon" aria-hidden="true">
                        <PlugZap size={22} />
                      </span>
                      <div className="catalog-empty-copy">
                        <strong id="third-party-empty-title">尚未添加三方模型</strong>
                        <p>当前线路还没有已选模型。</p>
                      </div>
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={isBusy}
                        onClick={onFetchCurrentModels}
                      >
                        {busy === "fetch-models"
                          ? <LoaderCircle className="spinner" aria-hidden="true" />
                          : <RefreshCw aria-hidden="true" />}
                        {busy === "fetch-models" ? "同步中" : "同步并选择模型"}
                      </Button>
                    </div>
                  )}
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
  );
}

type FeaturePolicyCardProps = {
  config: Config;
  status: RuntimeStatus;
  busy: string | null;
  isBusy: boolean;
  subagentModel: string;
  onConfigChange: (config: Config) => void;
  onSubagentOptimizationChange: (checked: boolean) => void;
};

export function FeaturePolicyCard({
  config,
  status,
  busy,
  isBusy,
  subagentModel,
  onConfigChange,
  onSubagentOptimizationChange,
}: FeaturePolicyCardProps) {
  const isMacClient = status.clientPlatform === "macos";
  const configuredGpuLaunchModeIndex = GPU_LAUNCH_MODES.findIndex(
    ({ value }) => value === config.gpuLaunchMode,
  );
  const gpuLaunchModeIndex = isMacClient
    ? 0
    : Math.max(configuredGpuLaunchModeIndex, 0);
  const gpuLaunchMode = GPU_LAUNCH_MODES[gpuLaunchModeIndex];
  const gpuLaunchModeStyle = {
    "--gpu-mode-offset": `${gpuLaunchModeIndex * 100}%`,
  } as CSSProperties;

  return (
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
                onCheckedChange={(checked) => onConfigChange({ ...config, slimCodexPet: checked })}
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
                onCheckedChange={(checked) => onConfigChange({ ...config, slimCodexVoice: checked })}
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

          <div className={`feature-card gpu-mode-card ${!isMacClient && gpuLaunchMode.value !== "off" ? "active" : ""}`}>
            <div className="feature-card-header">
              <div className="feature-card-title">
                <strong>GPU 渲染模式</strong>
                <Badge variant={isMacClient ? "secondary" : "warning"}>
                  {isMacClient ? "macOS 不可用" : "实验性"}
                </Badge>
              </div>
            </div>
            <div className="feature-card-body gpu-mode-card-body">
              <fieldset
                className="gpu-mode-fieldset"
                disabled={isMacClient}
                aria-describedby="gpu-launch-mode-description"
              >
                <legend className="sr-only">Codex GPU 启动模式</legend>
                <div className="gpu-mode-slider" style={gpuLaunchModeStyle}>
                  <span className="gpu-mode-slider-thumb" aria-hidden="true" />
                  {GPU_LAUNCH_MODES.map((mode) => (
                    <label
                      key={mode.value}
                      className={`gpu-mode-option ${gpuLaunchMode.value === mode.value ? "selected" : ""}`}
                    >
                      <input
                        type="radio"
                        name="codey-gpu-launch-mode"
                        value={mode.value}
                        checked={gpuLaunchMode.value === mode.value}
                        onChange={() => onConfigChange({
                          ...config,
                          gpuLaunchMode: mode.value,
                        })}
                      />
                      <span>{mode.label}</span>
                    </label>
                  ))}
                </div>
              </fieldset>
              <small id="gpu-launch-mode-description" aria-live="polite">
                {isMacClient
                  ? "macOS 下已禁用，不会向 Codex 传递 GPU 诊断参数"
                  : gpuLaunchMode.value === "disableGpu"
                    ? "启动 Codex 时附加 --disable-gpu；可能增加 CPU 占用"
                    : gpuLaunchMode.value === "disableGpuRasterization"
                      ? "启动 Codex 时附加 --disable-gpu-rasterization；仅将栅格化移到 CPU"
                      : "保持 Codex 默认 GPU 渲染，不附加诊断参数"}
              </small>
            </div>
          </div>

          <div className={`feature-card ${config.fastContextTools ? "active" : ""}`}>
            <div className="feature-card-header">
              <strong>FastCtx 上下文工具</strong>
              <Switch
                checked={config.fastContextTools}
                onCheckedChange={(checked) => onConfigChange({ ...config, fastContextTools: checked })}
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
                onCheckedChange={(checked) => onConfigChange({
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
                onCheckedChange={(checked) => onSubagentOptimizationChange(checked)}
                aria-label="启用子代理协作优化"
              />
            </div>
            <div className="feature-card-body">
              <small>
                {busy === "check-subagent-model"
                  ? `正在校验当前线路是否支持 ${subagentModel}`
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
                onCheckedChange={(checked) => onConfigChange({ ...config, hideFullAccessWarning: checked })}
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
  );
}

type WebhookCardProps = {
  config: Config;
  busy: string | null;
  isBusy: boolean;
  webhookResult: InlineResult;
  onWebhookChange: (patch: Partial<Config["webhook"]>) => void;
  onTestWebhook: () => void;
};

export function WebhookCard({
  config,
  busy,
  isBusy,
  webhookResult,
  onWebhookChange,
  onTestWebhook,
}: WebhookCardProps) {
  return (
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
            onCheckedChange={(checked) => onWebhookChange({ enabled: checked })}
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
                onChange={(event) => onWebhookChange({ url: event.target.value })}
                placeholder="https://open.feishu.cn/..."
                spellCheck={false}
              />
            </div>
          </label>
        </div>
        <div className="notification-action">
          <span className={`inline-result ${webhookResult.tone}`}>
            {webhookResult.text || ""}
          </span>
          <Button
            variant="secondary"
            size="sm"
            disabled={isBusy || !config.webhook.url.trim()}
            onClick={onTestWebhook}
          >
            {busy === "test-webhook"
              ? <LoaderCircle className="spinner" aria-hidden="true" />
              : <Send aria-hidden="true" />}
            测试通知
          </Button>
        </div>
      </Card>
    </section>
  );
}
