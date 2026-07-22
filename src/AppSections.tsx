import {
  IconActivity as Activity,
  IconBell as BellRing,
  IconBrandWindows,
  IconCheck as Check,
  IconCpu,
  IconDownload,
  IconFolderOpen,
  IconGitBranch,
  IconHistory,
  IconLoader2 as LoaderCircle,
  IconPlugConnected as PlugZap,
  IconRefresh as RefreshCw,
  IconSend,
  IconServer,
  IconX as X,
} from "@tabler/icons-react";

import type {
  CcSwitchStatus,
  Config,
  InlineResult,
  ModelState,
  RuntimeStatus,
  UpdateCheck,
  UpdateDownload,
} from "./App.types";
import { Badge, Button, Card, Input, Switch } from "./components/ui";

const Cpu = IconCpu;
const Download = IconDownload;
const FolderOpen = IconFolderOpen;
const GitBranch = IconGitBranch;
const History = IconHistory;
const Send = IconSend;
const Server = IconServer;

type OperationsPanelProps = {
  config: Config;
  status: RuntimeStatus;
  busy: string | null;
  isBusy: boolean;
  onRestart: () => void;
};

export function OperationsPanel({
  config,
  status,
  busy,
  isBusy,
  onRestart,
}: OperationsPanelProps) {
  const maintenance = status.maintenance;
  const sessionOk = maintenance?.sessionStatus === "ready";
  const pluginOk = maintenance?.pluginStatus === "ready";
  const performanceError = maintenance?.performanceStatus === "error";
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
      description: !performanceError
        ? "精简策略与性能补丁已按当前配置启用。"
        : "部分精简策略尚未启用，保留完整功能。",
      detail: performanceError
        ? maintenance?.performanceDetail || "性能补丁加载异常"
        : "FastCtx、宠物、语音与 Windows 性能策略",
      label: performanceError ? "异常" : "已优化",
      tone: performanceError ? "destructive" : "success",
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
              <h1 id="operations-title">Codex 运行状态</h1>
              <div className="path-display header-path-display" aria-label="Codex 应用路径">
                <FolderOpen size={14} aria-hidden="true" />
                <code>{config.codexAppPath || resolvedCodexPath}</code>
              </div>
            </div>
          </div>

          <div className="operations-actions">
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
            variant="ghost"
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
              <small>{provider.id}</small>
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
                <div className="catalog-model-list">
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
  );
}

type FeaturePolicyCardProps = {
  config: Config;
  busy: string | null;
  isBusy: boolean;
  subagentModel: string;
  onConfigChange: (config: Config) => void;
  onSubagentOptimizationChange: (checked: boolean) => void;
};

export function FeaturePolicyCard({
  config,
  busy,
  isBusy,
  subagentModel,
  onConfigChange,
  onSubagentOptimizationChange,
}: FeaturePolicyCardProps) {
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
