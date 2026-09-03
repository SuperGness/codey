import { memo, useMemo, useRef, useState } from "react";
import {
  IconActivity as Activity,
  IconCode as Code,
  IconCloudCheck,
  IconCpu,
  IconDatabase,
  IconFileCheck,
  IconFolderOpen,
  IconHistory,
  IconLoader2 as LoaderCircle,
  IconPlugConnected as PlugZap,
  IconRefresh as RefreshCw,
  IconShieldCheck,
  IconShoppingBag,
  IconBolt as Zap,
} from "@tabler/icons-react";

import type {
  FastContextToolsStatus,
  PluginMarketplaceStatus,
  RuntimeStatus,
} from "./App.types";
import { Badge, Button, Card, Collapse } from "./components/mantine";
import { flushCardClass } from "./uiClasses";
import {
  buildEnabledOptimizationFeatures,
  summarizeInjectionScripts,
  type EnabledOptimizationFeature as PresentationOptimizationFeature,
} from "./runtimeStatusPresentation";

const Cpu = IconCpu;
const FolderOpen = IconFolderOpen;
const History = IconHistory;
const EMPTY_INJECTION_SCRIPTS: NonNullable<
  RuntimeStatus["injectionScripts"]
> = [];
type EnabledOptimizationFeature = Omit<
  PresentationOptimizationFeature,
  "icon"
> & {
  icon: typeof Activity;
};
const OPTIMIZATION_FEATURE_ICONS: Record<
  PresentationOptimizationFeature["icon"],
  typeof Activity
> = {
  code: Code,
  database: IconDatabase,
  fastctx: Zap,
  notifications: PlugZap,
  subagent: Cpu,
};

type OperationsRuntimeStatus = Pick<
  RuntimeStatus,
  | "running"
  | "codexAppVersion"
  | "clientPlatform"
  | "restartRequired"
  | "restartInProgress"
  | "codexAppPath"
  | "maintenance"
  | "injectionScripts"
  | "fastContextToolsActive"
  | "subagentOptimizationActive"
  | "notificationChannelsActive"
  | "activeNotificationChannelCount"
  | "traceLogWriteProtectionActive"
  | "crashpadDiskProtectionActive"
>;

type OperationsPanelProps = {
  codexAppPath: string;
  fastContextToolsStatus: FastContextToolsStatus;
  status: OperationsRuntimeStatus;
  busy: string | null;
  isBusy: boolean;
  pluginMarketplaceStatus: PluginMarketplaceStatus | null;
  onRepairPluginMarketplace: () => void;
  onRestart: () => void;
  showRestartAction?: boolean;
};

function OperationsPanelComponent({
  codexAppPath,
  fastContextToolsStatus,
  status,
  busy,
  isBusy,
  pluginMarketplaceStatus,
  onRepairPluginMarketplace,
  onRestart,
  showRestartAction = true,
}: OperationsPanelProps) {
  const operationsHubRef = useRef<HTMLElement>(null);
  const [activeCardTitle, setActiveCardTitle] = useState<string | null>(null);
  const [expandedCardTitle, setExpandedCardTitle] = useState<string | null>(
    null,
  );

  const toggleCard = (title: string) => {
    if (activeCardTitle === title) {
      setActiveCardTitle(null);
      return;
    }

    setActiveCardTitle(title);
    setExpandedCardTitle(title);
  };

  const maintenance = status.maintenance;
  const sessionOk = maintenance?.sessionStatus === "ready";
  const pluginOk = pluginMarketplaceStatus?.status === "ready";
  const pluginStatusError = pluginMarketplaceStatus?.status === "error";
  const pluginRepairing = busy === "repair-plugin-marketplace";
  const pluginStatusKnown = Boolean(
    pluginMarketplaceStatus && !pluginStatusError,
  );
  const remoteMarketplaceCached =
    pluginMarketplaceStatus?.remoteMarketplace === true;
  const remoteMarketplaceReady =
    remoteMarketplaceCached &&
    pluginMarketplaceStatus?.remoteRegistered === true;
  const managedConfigCompatible =
    pluginMarketplaceStatus?.managedConfigCompatible === true;
  const embeddedMarketplaceReady =
    remoteMarketplaceReady && managedConfigCompatible;
  const performanceError = maintenance?.performanceStatus === "error";
  const performanceDegraded = maintenance?.performanceStatus === "degraded";
  const injectionScripts = status.injectionScripts ?? EMPTY_INJECTION_SCRIPTS;
  const enabledOptimizationFeatures = useMemo<EnabledOptimizationFeature[]>(
    () =>
      buildEnabledOptimizationFeatures(
        { ...status, injectionScripts },
        fastContextToolsStatus,
      ).map((feature) => ({
        ...feature,
        icon: OPTIMIZATION_FEATURE_ICONS[feature.icon],
      })),
    [
      fastContextToolsStatus.serverId,
      fastContextToolsStatus.userConfigured,
      status.activeNotificationChannelCount,
      status.crashpadDiskProtectionActive,
      status.fastContextToolsActive,
      status.notificationChannelsActive,
      status.running,
      status.subagentOptimizationActive,
      status.traceLogWriteProtectionActive,
      injectionScripts,
    ],
  );
  const {
    failedInjectionScriptCount,
    internalInjectionError,
    internalInjectionPending,
    unverifiedInjectionScriptCount,
  } = useMemo(
    () => summarizeInjectionScripts(injectionScripts),
    [injectionScripts],
  );
  const injectionStatusPending = injectionScripts.length === 0;
  const injectionError =
    internalInjectionError || failedInjectionScriptCount > 0;
  const isWindowsClient = status.clientPlatform === "windows";
  const resolvedCodexPath = status.codexAppPath || "/Applications/ChatGPT.app";
  const restartPending = Boolean(status.restartRequired);
  const codexVersion = status.codexAppVersion?.trim();
  const codexVersionLabel = codexVersion
    ? `Codex v${codexVersion}`
    : "Codex 版本未知";

  const handleCollapseTransitionEnd = () => {
    if (!activeCardTitle) {
      setExpandedCardTitle(null);
    }
  };

  type MetricItem = {
    id: string;
    icon: typeof Activity;
    tooltip: string;
    tone?: "success" | "warning" | "destructive" | "info";
  };

  const sessionMetrics = useMemo<MetricItem[]>(
    () => [
      {
        id: "session-files",
        icon: IconFileCheck,
        tooltip: `会话文件：已修复 ${maintenance?.sessionFilesFixed ?? 0} 个会话文件`,
        tone: sessionOk ? "success" : "warning",
      },
      {
        id: "session-db",
        icon: IconDatabase,
        tooltip: `数据库索引：已更新 ${maintenance?.sqliteRowsUpdated ?? 0} 行数据库索引`,
        tone: sessionOk ? "success" : "warning",
      },
      {
        id: "session-ghost",
        icon: IconShieldCheck,
        tooltip: `幽灵任务：已清理 ${maintenance?.ghostTasksPruned ?? 0} 条幽灵任务`,
        tone: sessionOk ? "success" : "warning",
      },
    ],
    [
      maintenance?.ghostTasksPruned,
      maintenance?.sessionFilesFixed,
      maintenance?.sqliteRowsUpdated,
      sessionOk,
    ],
  );

  // Plugin Marketplace Metrics
  const pluginMetrics = useMemo<MetricItem[]>(
    () => [
      {
        id: "plugin-official",
        icon: IconShoppingBag,
        tooltip: !pluginStatusKnown
          ? "官方市场：正在检查"
          : embeddedMarketplaceReady
            ? pluginMarketplaceStatus?.officialMarketplace === true
              ? "官方市场：Codey 内置快照已接管，并检测到兼容缓存"
              : "官方市场：Codey 内置快照已接管，无需联网下载"
            : "官方市场：等待 Codey 内置快照修复",
        tone: !pluginStatusKnown
          ? "info"
          : embeddedMarketplaceReady
            ? "success"
            : "warning",
      },
      {
        id: "plugin-remote",
        icon: IconCloudCheck,
        tooltip: !pluginStatusKnown
          ? "Codey 内置市场：正在检查"
          : !remoteMarketplaceCached
            ? "Codey 内置市场：快照缺失"
            : !remoteMarketplaceReady
              ? "Codey 内置市场：快照存在但尚未注册"
              : !managedConfigCompatible
                ? "Codey 内置市场：旧保留名配置待迁移"
                : "Codey 内置市场：快照与注册完整",
        tone:
          !pluginStatusKnown
            ? "info"
            : embeddedMarketplaceReady
              ? "success"
              : "warning",
      },
      {
        id: "plugin-host",
        icon: PlugZap,
        tooltip: pluginOk
          ? "插件托管：插件服务正常且链路已就绪"
          : "插件托管：正在检查或等待修复",
        tone: pluginOk ? "success" : "warning",
      },
    ],
    [
      embeddedMarketplaceReady,
      managedConfigCompatible,
      pluginMarketplaceStatus?.officialMarketplace,
      pluginOk,
      pluginStatusKnown,
      remoteMarketplaceCached,
      remoteMarketplaceReady,
    ],
  );

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
    enabledFeatureCount?: number;
  }> = [
    {
      title: "会话恢复",
      description: sessionOk
        ? "索引与恢复链路运行正常，上下文恢复就绪。"
        : "正在确认会话索引与恢复链路。",
      metrics: sessionMetrics,
      label: sessionOk ? "正常" : maintenance ? "需检查" : "检查中",
      tone: sessionOk ? "success" : maintenance ? "destructive" : "warning",
      icon: History,
    },
    {
      title: "系统优化",
      description: internalInjectionError
        ? "基础组件运行异常，已确认生效的功能仍列于下方。"
        : failedInjectionScriptCount > 0
        ? `${failedInjectionScriptCount} 个脚本注入异常，下方仅列出已确认生效的功能。`
        : internalInjectionPending
          ? "基础组件状态确认中，已生效功能会自动更新。"
          : unverifiedInjectionScriptCount > 0
            ? `${unverifiedInjectionScriptCount} 个功能尚待确认，下方仅列出已确认生效的功能。`
            : injectionStatusPending
              ? status.running
                ? "正在读取最近一次功能生效结果。"
                : "Codex 启动后将在这里汇总已生效功能。"
              : performanceError
                ? maintenance?.performanceDetail || "系统优化组件启动失败。"
                : performanceDegraded
                  ? maintenance?.performanceDetail ||
                    "兼容模式运行正常，部分非必要精简策略未启用。"
                  : isWindowsClient
                    ? "精简策略、Windows 性能补丁与功能自检均已通过。"
                    : "精简策略与功能自检均已通过。",
      metrics: [],
      label: internalInjectionError
        ? "基础异常"
        : failedInjectionScriptCount > 0
        ? `${failedInjectionScriptCount} 个异常`
        : internalInjectionPending
          ? "确认中"
          : unverifiedInjectionScriptCount > 0
            ? `${unverifiedInjectionScriptCount} 个待确认`
            : injectionStatusPending
              ? status.running
                ? "检测中"
                : "待启动"
              : performanceError
                ? "异常"
                : performanceDegraded
                  ? "兼容模式"
                  : "已优化",
      tone:
        injectionError || performanceError
          ? "destructive"
          : injectionStatusPending ||
              internalInjectionPending ||
              unverifiedInjectionScriptCount > 0 ||
              performanceDegraded
            ? "warning"
            : "success",
      icon: Cpu,
      showInjectionScripts: true,
      enabledFeatureCount: enabledOptimizationFeatures.length,
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
      tone: pluginOk
        ? "success"
        : pluginStatusError
          ? "destructive"
          : "warning",
      icon: PlugZap,
      action: {
        label: pluginOk ? "重新检查并修复" : "手动修复",
        disabled: isBusy,
        loading: pluginRepairing,
        onClick: onRepairPluginMarketplace,
      },
    },
  ];
  const expandedStatusCard = expandedCardTitle
    ? statusCards.find((item) => item.title === expandedCardTitle) ?? null
    : null;
  const ExpandedStatusIcon = expandedStatusCard?.icon;

  return (
    <section
      ref={operationsHubRef}
      className={`operations-hub${restartPending ? " pending" : status.running ? " running" : ""}`}
      aria-labelledby="operations-title"
    >
      <Card className={`operations-panel ${flushCardClass}`}>
        <div className="operations-header">
          <div className="operations-heading">
            <span className="operations-heading-icon">
              <Activity size={18} aria-hidden="true" />
            </span>
            <div className="operations-heading-copy">
              <div className="operations-title-row">
                <h2 id="operations-title">Codex 运行状态</h2>
                <span className="codex-version-tag">{codexVersionLabel}</span>
              </div>
              <div
                className="path-display header-path-display"
                aria-label="Codex 应用路径"
              >
                <FolderOpen size={14} aria-hidden="true" />
                <code>{codexAppPath || resolvedCodexPath}</code>
              </div>
            </div>
          </div>

          <div className="operations-actions">
            <div
              className="operations-status-chips"
              role="list"
              aria-label="核心服务状态"
            >
              {statusCards.map((item) => {
                const StatusIcon = item.icon;
                const isExpanded = activeCardTitle === item.title;
                return (
                  <span
                    key={item.title}
                    className="operations-status-chip-wrap"
                    role="listitem"
                  >
                    <button
                      type="button"
                      className={`operations-status-chip tone-${item.tone}${isExpanded ? " active" : ""}`}
                      onClick={() => toggleCard(item.title)}
                      aria-expanded={isExpanded}
                      aria-label={`${item.title}（${item.label}），点击${isExpanded ? "收起" : "展开"}`}
                    >
                      <span
                        className="operations-status-chip-icon"
                        aria-hidden="true"
                      >
                        <StatusIcon size={14} />
                      </span>
                      <span className="operations-status-chip-copy">
                        <strong>{item.title}</strong>
                        <small>{item.label}</small>
                      </span>
                    </button>
                  </span>
                );
              })}
            </div>

            <Badge
              variant={
                restartPending
                  ? "warning"
                  : status.running
                    ? "success"
                    : "secondary"
              }
            >
              <span className="operations-status-dot" aria-hidden="true" />
              {restartPending
                ? "等待重启"
                : status.running
                  ? "运行中"
                  : "未启动"}
            </Badge>
            {showRestartAction && (
              <Button
                variant="warning"
                size="sm"
                disabled={isBusy || status.restartInProgress || !status.running}
                onClick={onRestart}
              >
                {busy === "restart" || status.restartInProgress ? (
                  <LoaderCircle className="animate-spin" aria-hidden="true" />
                ) : (
                  <RefreshCw aria-hidden="true" />
                )}
                {status.running ? "重启 Codex" : "未运行"}
              </Button>
            )}
          </div>
        </div>

        <Collapse
          animateOpacity
          className="operations-expanded-collapse"
          expanded={Boolean(activeCardTitle)}
          keepMounted
          onTransitionEnd={handleCollapseTransitionEnd}
          transitionDuration={180}
        >
          {expandedStatusCard && ExpandedStatusIcon && (
            <div
              className="operations-expanded-grid"
              role="region"
              aria-label="展开的系统详情"
            >
              <article
                key={expandedStatusCard.title}
                className={`operations-expanded-card tone-${expandedStatusCard.tone}`}
              >
                <div className="expanded-card-header">
                  <div className="expanded-card-title">
                    <span
                      className={`expanded-card-icon tone-${expandedStatusCard.tone}`}
                    >
                      <ExpandedStatusIcon size={18} aria-hidden="true" />
                    </span>
                    <div className="expanded-card-copy">
                      <div className="expanded-card-heading">
                        <h3>{expandedStatusCard.title}</h3>
                        {expandedStatusCard.enabledFeatureCount !==
                          undefined && (
                          <span className="expanded-card-feature-count">
                            已启用 {expandedStatusCard.enabledFeatureCount} 项
                          </span>
                        )}
                      </div>
                      <p>{expandedStatusCard.description}</p>
                    </div>
                  </div>
                  <div className="expanded-card-actions">
                    <Badge variant={expandedStatusCard.tone}>
                      {expandedStatusCard.label}
                    </Badge>
                  </div>
                </div>

                <div className="expanded-card-body">
                  {expandedStatusCard.metrics.length > 0 && (
                    <div className="expanded-card-metrics">
                      {expandedStatusCard.metrics.map((metric) => {
                        const MetricIcon = metric.icon;
                        return (
                          <div
                            key={metric.id}
                            className="expanded-metric-item"
                          >
                            <span
                              className={`expanded-metric-icon tone-${metric.tone || "info"}`}
                            >
                              <MetricIcon size={14} aria-hidden="true" />
                            </span>
                            <span className="expanded-metric-text">
                              {metric.tooltip}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                  )}

                  {expandedStatusCard.showInjectionScripts && (
                    <section
                      className="injection-status-section"
                      aria-labelledby="injection-status-title"
                    >
                      <div className="injection-status-header">
                        <h4 id="injection-status-title">已生效功能</h4>
                      </div>

                      {enabledOptimizationFeatures.length > 0 ? (
                        <div className="injection-status-list" role="list">
                          {enabledOptimizationFeatures.map((feature) => {
                            const FeatureIcon = feature.icon;
                            return (
                              <div
                                key={feature.id}
                                className="injection-status-row"
                                role="listitem"
                              >
                                <span
                                  className="injection-script-icon"
                                  aria-hidden="true"
                                >
                                  <FeatureIcon size={15} />
                                </span>
                                <div className="injection-script-copy">
                                  <div className="injection-script-title">
                                    <span>{feature.name}</span>
                                    <span className="injection-script-source">
                                      {feature.sourceLabel}
                                    </span>
                                  </div>
                                  {feature.detail && (
                                    <span className="injection-script-detail">
                                      {feature.detail}
                                    </span>
                                  )}
                                </div>
                              </div>
                            );
                          })}
                        </div>
                      ) : (
                        <div className="injection-status-empty">
                          {status.running
                            ? injectionScripts.length > 0
                              ? "暂未检测到已生效功能"
                              : "正在读取已生效功能"
                            : "Codex 启动后将在这里显示已生效功能"}
                        </div>
                      )}
                    </section>
                  )}

                  {expandedStatusCard.action && (
                    <div className="expanded-card-footer">
                      <Button
                        variant="outline"
                        size="xs"
                        disabled={expandedStatusCard.action.disabled}
                        onClick={expandedStatusCard.action.onClick}
                      >
                        {expandedStatusCard.action.loading ? (
                          <LoaderCircle
                            className="animate-spin"
                            aria-hidden="true"
                          />
                        ) : (
                          <RefreshCw aria-hidden="true" />
                        )}
                        {expandedStatusCard.action.label}
                      </Button>
                    </div>
                  )}
                </div>
              </article>
            </div>
          )}
        </Collapse>

      </Card>
    </section>
  );
}

export const OperationsPanel = memo(OperationsPanelComponent);
