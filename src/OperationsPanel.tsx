import { memo, useMemo, useState } from "react";
import {
  IconActivity as Activity,
  IconChevronDown,
  IconChevronUp,
  IconCode as Code,
  IconCloudCheck,
  IconCpu,
  IconDatabase,
  IconFileCheck,
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
  Notice,
  PluginMarketplaceStatus,
  RuntimeStatus,
} from "./App.types";
import { Disclosure } from "@heroui/react";
import { Badge, Button } from "./components/ui";
import {
  buildEnabledOptimizationFeatures,
  summarizeInjectionScripts,
  type EnabledOptimizationFeature as PresentationOptimizationFeature,
} from "./runtimeStatusPresentation";

const Cpu = IconCpu;
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
  onPrepareComputerUse: () => void;
  computerUseNotice?: Notice | null;
  configRepairNotice?: { tone: "info" | "success" | "error"; text: string } | null;
};

function OperationsPanelComponent({
  fastContextToolsStatus,
  status,
  busy,
  isBusy,
  pluginMarketplaceStatus,
  onRepairPluginMarketplace,
  onPrepareComputerUse,
  computerUseNotice,
  configRepairNotice,
}: OperationsPanelProps) {
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
  const computerUse = pluginMarketplaceStatus?.computerUse;
  const computerUsePreparing = busy === "prepare-computer-use";
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
  const startupNeedsAttention = maintenance?.performanceStatus === "degraded";
  const startupInjectionMode = maintenance?.startupInjectionMode ?? "";
  const injectionModeCard =
    !status.running
      ? {
          label: "待启动",
          tone: "info" as const,
          description: "Codex 启动后将在这里显示启动兼容方案的注入路径。",
        }
      : startupInjectionMode === "cli"
        ? {
            label: "CLI 包装器",
            tone: "success" as const,
            description:
              "CLI 包装器已确认接管 app-server 请求；页面补丁经渲染进程调试通道投递。",
          }
        : {
            label: "未确认",
            tone: "warning" as const,
            description:
              "本次启动未确认 CLI 兼容入口，页面功能以检测结果为准。",
          };
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
  const restartPending = Boolean(status.restartRequired);


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
                ? maintenance?.performanceDetail || "启动失败，请查看错误详情。"
                : startupNeedsAttention
                  ? maintenance?.performanceDetail ||
                    "部分启动设置未能应用，请查看错误详情。"
                  : "已启用功能运行正常。",
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
                : startupNeedsAttention
                  ? "需检查"
                  : "正常",
      tone:
        injectionError || performanceError
          ? "destructive"
          : injectionStatusPending ||
              internalInjectionPending ||
              unverifiedInjectionScriptCount > 0 ||
              startupNeedsAttention
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

  return (
    <section
      className={`operations-hub${restartPending ? " pending" : status.running ? " running" : ""}`}
      aria-label="服务状态与诊断"
    >
      <div className="operations-panel">
        {/* 3列核心服务卡片 */}
        <div
          className="operations-status-cards"
          role="list"
          aria-label="核心服务状态"
        >
          {statusCards.map((item) => {
            const StatusIcon = item.icon;
            const isExpanded = activeCardTitle === item.title;

            return (
              <button
                key={item.title}
                type="button"
                role="listitem"
                className={`status-strip-item tone-${item.tone}${isExpanded ? " is-expanded" : ""}`}
                onClick={() => toggleCard(item.title)}
                aria-expanded={isExpanded}
                aria-label={`${item.title}（${item.label}），点击${isExpanded ? "收起" : "展开"}`}
              >
                <span className="status-strip-head">
                  <StatusIcon size={15} className="status-strip-icon" aria-hidden="true" />
                  <strong>{item.title}</strong>
                  <span className="status-strip-state">
                    <span className="status-strip-dot" aria-hidden="true" />
                    {item.label}
                  </span>
                </span>
                <span className="status-strip-description">{item.description}</span>
                <span className="status-strip-toggle">
                  {isExpanded ? "收起诊断" : "查看诊断"}
                  {isExpanded ? (
                    <IconChevronUp size={12} aria-hidden="true" />
                  ) : (
                    <IconChevronDown size={12} aria-hidden="true" />
                  )}
                </span>
              </button>
            );
          })}
        </div>

        {/* 详情面板只做展开收起动画，没有独立触发器：状态按钮本身就是开关。 */}
        <Disclosure isExpanded={Boolean(activeCardTitle)} className="border-0 p-0">
          <Disclosure.Content>
          <Disclosure.Body className="p-0">
          {expandedStatusCard && (
            <div
              className="operations-expanded-tray codey-card"
              role="region"
              aria-label="展开的系统详情"
            >
              <article
                key={expandedStatusCard.title}
                className={`operations-expanded-card tone-${expandedStatusCard.tone}`}
              >
                <div className="expanded-card-body">
                  {expandedStatusCard.showInjectionScripts && configRepairNotice && (
                    <p role="status" className={`mb-3 whitespace-pre-line break-words text-sm ${configRepairNotice.tone === "error" ? "text-danger" : "text-[var(--codey-text-secondary)]"}`}>
                      {configRepairNotice.text}
                    </p>
                  )}
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

                  {expandedStatusCard.title === "插件市场" && computerUse?.supported && (
                    <section
                      className="injection-status-section injection-mode-panel"
                      aria-labelledby="computer-use-title"
                    >
                      <div className="injection-mode-copy">
                        <div className="injection-mode-heading">
                          <h4 id="computer-use-title">桌面操作插件</h4>
                          <Badge variant={computerUse.ready ? "success" : "secondary"}>
                            {computerUsePreparing ? "准备中" : computerUse.ready ? "已准备" : "待准备"}
                          </Badge>
                        </div>
                        <p className="injection-mode-description">
                          点击后准备本地资源，再到 Codex 插件页面安装或更新 Codey Computer Use；启停也在该页面管理。
                        </p>
                        {computerUseNotice && (
                          <p
                            role={computerUseNotice.tone === "error" ? "alert" : "status"}
                            className={`mt-2 break-words text-xs ${computerUseNotice.tone === "error" ? "text-danger" : "text-success"}`}
                          >
                            {computerUseNotice.text}
                          </p>
                        )}
                      </div>
                      <div className="injection-mode-action">
                        <Button
                          variant="outline"
                          size="xs"
                          disabled={isBusy}
                          onClick={onPrepareComputerUse}
                        >
                          {computerUsePreparing && <LoaderCircle className="animate-spin" aria-hidden="true" />}
                          {computerUsePreparing ? "准备中" : computerUse.ready ? "重新准备" : "准备桌面插件"}
                        </Button>
                      </div>
                    </section>
                  )}

                  {expandedStatusCard.showInjectionScripts && (
                    <section
                      className="injection-status-section injection-mode-panel"
                      aria-labelledby="injection-mode-title"
                    >
                      <div className="injection-mode-copy">
                        <div className="injection-mode-heading">
                          <h4 id="injection-mode-title">启动兼容</h4>
                          <Badge variant={injectionModeCard.tone}>
                            {injectionModeCard.label}
                          </Badge>
                        </div>
                        <p className="injection-mode-description">
                          {injectionModeCard.description}
                        </p>
                      </div>
                    </section>
                  )}

                  {expandedStatusCard.showInjectionScripts && (
                    <section
                      className="injection-status-section"
                      aria-labelledby="injection-status-title"
                    >
                      <div className="injection-status-header">
                        <h4 id="injection-status-title">
                          {enabledOptimizationFeatures.length > 0
                            ? `已生效 ${enabledOptimizationFeatures.length} 项功能`
                            : "已生效功能"}
                        </h4>
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
          </Disclosure.Body>
          </Disclosure.Content>
        </Disclosure>

      </div>
    </section>
  );
}

export const OperationsPanel = memo(OperationsPanelComponent);
