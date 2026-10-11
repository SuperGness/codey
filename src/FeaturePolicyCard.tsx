import { memo, type CSSProperties } from "react";
import {
  IconAdjustments,
  IconCode,
  IconDeviceDesktopCode,
  IconFocus2,
  IconInfoCircle,
  IconPhotoSearch,
  IconUsersGroup,
  IconWorldSearch,
} from "@tabler/icons-react";

import type {
  Config,
  FastContextToolsStatus,
  SubagentRoleId,
} from "./App.types";
import {
  Button,
  Badge,
  Select,
  Switch,
  Tooltip,
} from "./components/ui";
import { ModelCombobox } from "./components/ModelCombobox";
import { routeProviderId } from "./modelRoutes";
import {
  resolveSubagentModelOption,
  type SubagentModelOption,
} from "./subagentModels";
import { SettingsPageHeader } from "./SettingsPageHeader";
import type { DiagnosticStorageTarget } from "./diagnosticStorage";
import { NotificationChannelsCard } from "./notifications/NotificationChannelsCard";
import type { NotificationChannel } from "./notifications/types";

const GPU_LAUNCH_MODES = [
  { value: "off", label: "关闭" },
  { value: "disableGpu", label: "禁用 GPU" },
  { value: "disableGpuRasterization", label: "禁用 GPU 栅格化" },
] as const satisfies ReadonlyArray<{
  value: Config["gpuLaunchMode"];
  label: string;
}>;
const REASONING_EFFORT_LABELS: Record<string, string> = {
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "极高",
  max: "最大",
  ultra: "超高",
};
const DEFAULT_SUBAGENT_TASK = {
  id: "default",
  name: "默认子代理",
  icon: IconUsersGroup,
  description: "未指定专用角色时使用的模型与思考深度。",
} as const;
const SUBAGENT_TASK_TYPES = [
  {
    id: "codey_quick_scan",
    name: "快速定位",
    kind: "analysis",
    icon: IconFocus2,
    description: "用于精确位置、重复性检查、低风险事实查找和小范围快速检索。",
  },
  {
    id: "codey_deep_research",
    name: "深度检索",
    kind: "analysis",
    icon: IconWorldSearch,
    description: "用于跨文件、日志、代码和文档的宽范围检索、归纳与架构探索。",
  },
  {
    id: "codey_visual_analysis",
    name: "视觉分析",
    kind: "analysis",
    icon: IconPhotoSearch,
    description: "用于截图、页面、GUI、PDF 或渲染结果的视觉证据分析。",
  },
  {
    id: "codey_worker",
    name: "代码实施",
    kind: "implementation",
    icon: IconCode,
    description: "用于边界清晰、可回滚、可测试的低到中等复杂度非视觉实现。",
  },
  {
    id: "codey_visual_worker",
    name: "视觉实施",
    kind: "implementation",
    icon: IconDeviceDesktopCode,
    description: "用于页面、GUI、PDF 或其他依赖视觉证据和渲染验证的实现。",
  },
] as const satisfies ReadonlyArray<{
  id: SubagentRoleId;
  name: string;
  kind: "analysis" | "implementation";
  icon: typeof IconFocus2;
  description: string;
}>;

export type SubagentPolicyCardProps = {
  config: Config;
  isBusy: boolean;
  subagentModelOptions: SubagentModelOption[];
  onConfigChange: (config: Config) => void;
  onSubagentOptimizationChange: (checked: boolean) => void;
};

export function SubagentPolicyCardComponent({
  config,
  isBusy,
  subagentModelOptions,
  onConfigChange,
  onSubagentOptimizationChange,
}: SubagentPolicyCardProps) {
  const subagentPolicyControlsDisabled = isBusy;
  const preferredProfile =
    config.profiles.find((profile) => profile.id === config.activeProfileId) ??
    config.profiles[0];
  const preferredProviderId = preferredProfile
    ? routeProviderId(preferredProfile)
    : undefined;
  const enabledRoleCount = SUBAGENT_TASK_TYPES.filter(
    ({ id }) => config.subagentRoles[id]?.enabled !== false,
  ).length;
  const analysisTasks = SUBAGENT_TASK_TYPES.filter(
    (task) => task.kind === "analysis",
  );
  const implementationTasks = SUBAGENT_TASK_TYPES.filter(
    (task) => task.kind === "implementation",
  );
  const enabledAnalysisCount = analysisTasks.filter(
    ({ id }) => config.subagentRoles[id]?.enabled !== false,
  ).length;
  const enabledImplementationCount = implementationTasks.filter(
    ({ id }) => config.subagentRoles[id]?.enabled !== false,
  ).length;

  const renderRoleCard = (
    task: (typeof SUBAGENT_TASK_TYPES)[number] | typeof DEFAULT_SUBAGENT_TASK,
  ) => {
    const isDefaultRole = task.id === "default";
    const TaskIcon = task.icon;
    const selection = config.subagentRoles[task.id] ?? {
      enabled: true,
      model: config.subagentModel,
      reasoningEffort: config.subagentReasoningEffort,
    };
    const selectedModel = resolveSubagentModelOption(
      subagentModelOptions,
      selection.model,
      preferredProviderId,
    );
    const reasoningEfforts = selectedModel?.supportedReasoningEfforts ?? [];
    const reasoningOptions = reasoningEfforts.map((effort) => ({
      label: REASONING_EFFORT_LABELS[effort] ?? effort,
      value: effort,
    }));
    const updateRole = (next: Partial<typeof selection>) => {
      const nextSelection = { ...selection, ...next };
      onConfigChange({
        ...config,
        ...(isDefaultRole
          ? {
              subagentModel: nextSelection.model,
              subagentReasoningEffort: nextSelection.reasoningEffort,
            }
          : {}),
        subagentRoles: {
          ...config.subagentRoles,
          [task.id]: nextSelection,
        },
      });
    };
    const roleDisabled = !isDefaultRole && !selection.enabled;
    const isSingleEnabledRole = selection.enabled && enabledRoleCount <= 1;

    return (
      <div
        key={task.id}
        id={task.id}
        className={`subagent-role-item ${
          roleDisabled ? "subagent-role-disabled" : ""
        }`}
      >
        <div className="subagent-role-item-header">
          <div className="subagent-role-identity">
            <div
              className={`subagent-role-icon-box subagent-role-icon-box--${task.id}`}
            >
              <TaskIcon size={18} stroke={1.9} aria-hidden="true" />
            </div>
            <div className="subagent-role-meta">
              <div className="subagent-role-title-row">
                <h4 className="subagent-role-name">{task.name}</h4>
                {roleDisabled && (
                  <span className="subagent-role-status-chip">已停用</span>
                )}
              </div>
              <p className="subagent-role-description">{task.description}</p>
            </div>
          </div>
          {!isDefaultRole && (
            <div
              className="subagent-role-switch-wrap"
              title={
                isSingleEnabledRole ? "至少需要保留一个启用的调度角色" : undefined
              }
            >
              <Switch
                checked={selection.enabled}
                disabled={
                  subagentPolicyControlsDisabled || isSingleEnabledRole
                }
                onCheckedChange={(enabled) => updateRole({ enabled })}
                aria-label={`${selection.enabled ? "关闭" : "启用"}${task.name}角色`}
              />
            </div>
          )}
        </div>

        <div className="subagent-role-controls-row">
          <div className="subagent-control-group subagent-control-group--model">
            <label className="subagent-control-label">指定模型</label>
            <div className="subagent-control-field">
              <ModelCombobox
                aria-label={`${task.name}模型`}
                value={selection.model}
                placeholder={
                  subagentModelOptions.length === 0
                    ? "所有线路均暂无模型"
                    : "请选择模型"
                }
                disabled={
                  subagentPolicyControlsDisabled ||
                  roleDisabled ||
                  subagentModelOptions.length === 0
                }
                options={subagentModelOptions}
                preferredProviderId={preferredProviderId}
                onChange={(value) => {
                  const option = subagentModelOptions.find(
                    (candidate) => candidate.value === value,
                  );
                  if (!option) return;
                  const reasoningEffort =
                    option.supportedReasoningEfforts.includes(
                      selection.reasoningEffort,
                    )
                      ? selection.reasoningEffort
                      : option.defaultReasoningEffort;
                  updateRole({
                    model: option.value,
                    reasoningEffort,
                  });
                }}
              />
            </div>
          </div>

          <div className="subagent-control-group subagent-control-group--effort">
            <label className="subagent-control-label">思考深度</label>
            <div className="subagent-control-field">
              <Select
                className="w-full min-w-0"
                aria-label={`${task.name}思考深度`}
                value={
                  reasoningEfforts.includes(selection.reasoningEffort)
                    ? selection.reasoningEffort
                    : undefined
                }
                placeholder="暂无可选深度"
                disabled={
                  subagentPolicyControlsDisabled ||
                  roleDisabled ||
                  reasoningEfforts.length === 0
                }
                optionList={reasoningOptions}
                filter={false}
                onChange={(value) =>
                  updateRole({
                    model: selectedModel?.value ?? selection.model,
                    reasoningEffort: String(value ?? ""),
                  })
                }
              />
            </div>
          </div>
        </div>
      </div>
    );
  };

  return (
    <section className="secondary-section subagent-section" aria-labelledby="subagent-title">
      <div className="subagent-settings">
        <SettingsPageHeader
          id="subagent-title"
          title="子代理优化"
          icon={<IconUsersGroup size={15} />}
          description="基于 Codex 原生子代理的多角色调度与模型配置。"
          actions={
            <Switch
              checked={config.subagentOptimization}
              disabled={isBusy}
              onCheckedChange={(checked) => onSubagentOptimizationChange(checked)}
              aria-label="启用子代理优化"
            />
          }
        />
        <div className="module-card-body subagent-policy-body">
          {config.subagentOptimization ? (
            <>
              <div className="subagent-group-card codey-card">
                {renderRoleCard(DEFAULT_SUBAGENT_TASK)}
              </div>
              <div className="subagent-group-card codey-card">
                <div className="subagent-group-header">
                  <div className="subagent-group-title-wrap">
                    <span className="subagent-group-title">分析角色</span>
                    <div className="subagent-stat-badge subagent-stat-badge--readonly">
                      <span className="subagent-stat-indicator" aria-hidden="true" />
                      <span>已启用</span>
                      <strong>
                        {enabledAnalysisCount} / {analysisTasks.length}
                      </strong>
                    </div>
                  </div>
                  <span className="subagent-group-desc">
                    负责定位、跨文件检索及视觉证据分析，可按任务授权调用工具
                  </span>
                </div>
                <div className="subagent-group-items">
                  {analysisTasks.map(renderRoleCard)}
                </div>
              </div>

              <div className="subagent-group-card codey-card">
                <div className="subagent-group-header">
                  <div className="subagent-group-title-wrap">
                    <span className="subagent-group-title">实施角色</span>
                    <div className="subagent-stat-badge subagent-stat-badge--write">
                      <span className="subagent-stat-indicator" aria-hidden="true" />
                      <span>已启用</span>
                      <strong>
                        {enabledImplementationCount} / {implementationTasks.length}
                      </strong>
                    </div>
                  </div>
                  <span className="subagent-group-desc">
                    负责代码修改、文件写入与页面渲染验证，受父任务权限约束
                  </span>
                </div>
                <div className="subagent-group-items">
                  {implementationTasks.map(renderRoleCard)}
                </div>
              </div>

              <div className="subagent-policy-callout">
                <IconInfoCircle
                  size={16}
                  className="subagent-callout-icon"
                  aria-hidden="true"
                />
                <div className="subagent-callout-text">
                  {subagentModelOptions.length === 0
                    ? "请先在模型管理中为任一可用线路启用模型。"
                    : "各角色工具权限一致，按任务分工选择模型。角色启停、模型和思考深度保存后对下次派生生效，正在运行的子代理沿用原配置；旧实例首次启用未注册角色时需重启一次。实际操作受父任务权限约束。"}
                </div>
              </div>
            </>
          ) : (
            <div className="module-disabled-placeholder">
              <div className="module-disabled-icon">
                <IconUsersGroup size={20} aria-hidden="true" />
              </div>
              <div className="module-disabled-text">
                <strong>子代理优化已关闭</strong>
                <p>开启后仅在宽范围、可并行或需要专门证据时选择性委派，并提供五类专用角色与汇合门禁。</p>
              </div>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}

export const SubagentPolicyCard = memo(SubagentPolicyCardComponent);

type FeaturePolicyCardProps = {
  config: Config;
  fastContextToolsStatus: FastContextToolsStatus;
  isMacClient: boolean;
  isWindowsClient: boolean;
  cleanupBusy: boolean;
  onAnalyzeDiagnosticStorage: (target: DiagnosticStorageTarget) => void;
  popupContainer: HTMLElement | null;
  isBusy: boolean;
  onConfigChange: (config: Config) => void;
  onAddChannel?: (channel: NotificationChannel) => Promise<boolean>;
  onChannelChange?: (
    channelId: string,
    patch: Partial<NotificationChannel>,
  ) => Promise<boolean>;
  onRequestRemoveChannel?: (channel: NotificationChannel) => void;
};

function FeaturePolicyCardComponent({
  config,
  fastContextToolsStatus,
  isMacClient,
  isWindowsClient,
  cleanupBusy,
  onAnalyzeDiagnosticStorage,
  popupContainer,
  isBusy,
  onConfigChange,
  onAddChannel,
  onChannelChange,
  onRequestRemoveChannel,
}: FeaturePolicyCardProps) {
  const configuredGpuLaunchModeIndex = GPU_LAUNCH_MODES.findIndex(
    ({ value }) => value === config.gpuLaunchMode,
  );
  const gpuLaunchModeIndex = Math.max(configuredGpuLaunchModeIndex, 0);
  const gpuLaunchMode = GPU_LAUNCH_MODES[gpuLaunchModeIndex];
  const gpuLaunchModeStyle = {
    "--gpu-mode-offset": `${gpuLaunchModeIndex * 100}%`,
  } as CSSProperties;
  const fastctxStatusBlocksEmbedded =
    fastContextToolsStatus.userConfigured ||
    fastContextToolsStatus.detectionFailed;
  const fastContextToolsEnabled =
    config.fastContextTools && !fastctxStatusBlocksEmbedded;
  const fastctxBlockedReason = fastContextToolsStatus.detectionFailed
    ? "暂时无法确认 Codex 配置中的 FastCtx 状态，为避免重复加载，Codey 内置 FastCtx 不可开启"
    : fastContextToolsStatus.userConfigured
      ? `已检测到 Codex 配置中的 FastCtx${
          fastContextToolsStatus.serverId
            ? `（${fastContextToolsStatus.serverId}）`
            : ""
        }，为避免重复加载，Codey 内置 FastCtx 不可开启`
      : "";
  const fastContextToolsSwitch = (
    <Switch
      checked={fastContextToolsEnabled}
      disabled={isBusy || fastctxStatusBlocksEmbedded}
      onCheckedChange={(checked) =>
        onConfigChange({ ...config, fastContextTools: checked })
      }
      aria-label="启用 FastCtx 上下文工具"
    />
  );

  return (
    <>
      <section className="secondary-section" aria-labelledby="runtime-title">
        <div className="feature-policy-heading">
          <div className="flex items-center gap-2">
            <span className="section-title-icon" aria-hidden="true">
              <IconAdjustments size={14} />
            </span>
            <h3 id="runtime-title" className="settings-section-heading">Codex 功能策略</h3>
          </div>
          <p>按需精简客户端模块和界面行为。</p>
        </div>
        <div className="runtime-settings">
          <div className="feature-grid">
            {/* GPU 渲染模式：仅 Windows 客户端展示 */}
            {isWindowsClient && (
              <div className={`feature-card gpu-mode-card full-width-card ${gpuLaunchMode.value !== "off" ? "active" : ""}`}>
                <div className="feature-card-header">
                  <div className="feature-card-title">
                    <strong>GPU 渲染模式</strong>
                    <Badge variant="warning">实验性</Badge>
                  </div>
                </div>
                <div className="feature-card-body gpu-mode-card-body">
                  <fieldset
                    className="gpu-mode-fieldset"
                    disabled={isBusy}
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
                            onChange={() =>
                              onConfigChange({
                                ...config,
                                gpuLaunchMode: mode.value,
                              })
                            }
                          />
                          <span>{mode.label}</span>
                        </label>
                      ))}
                    </div>
                  </fieldset>
                  <small id="gpu-launch-mode-description" aria-live="polite">
                    {gpuLaunchMode.value === "disableGpu"
                      ? "启动 Codex 时附加 --disable-gpu；可能增加 CPU 占用"
                      : gpuLaunchMode.value === "disableGpuRasterization"
                        ? "启动 Codex 时附加 --disable-gpu-rasterization；仅将栅格化移到 CPU"
                        : "保持 Codex 默认 GPU 渲染，不附加诊断参数"}
                  </small>
                </div>
              </div>
            )}

            <div
              className={`feature-card ${config.slimCodexPet ? "active" : ""}`}
            >
              <div className="feature-card-header">
                <strong>精简 Codex 宠物模块</strong>
                <Switch
                  checked={config.slimCodexPet}
                  disabled={isBusy}
                  onCheckedChange={(checked) =>
                    onConfigChange({ ...config, slimCodexPet: checked })
                  }
                  aria-label="精简 Codex 宠物模块"
                />
              </div>
              <div className="feature-card-body">
                <small>
                  {config.slimCodexPet
                    ? "已收起宠物并取消隐藏窗口预热；语音功能仍按需启用"
                    : "保留 Codex 宠物的完整功能"}
                </small>
              </div>
            </div>

            <div
              className={`feature-card ${fastContextToolsEnabled ? "active" : ""}`}
            >
              <div className="feature-card-header">
                <div className="feature-card-title">
                  <strong>FastCtx 上下文工具</strong>
                  <Badge variant="brand">v0.2.6</Badge>
                </div>
                {fastctxStatusBlocksEmbedded ? (
                  <Tooltip
                    content={fastctxBlockedReason}
                    position="top"
                  >
                    <span
                      className="fastctx-disabled-switch-tooltip"
                      tabIndex={0}
                      aria-label={fastctxBlockedReason}
                    >
                      {fastContextToolsSwitch}
                    </span>
                  </Tooltip>
                ) : (
                  fastContextToolsSwitch
                )}
              </div>
              <div className="feature-card-body">
                <small>
                  {fastctxStatusBlocksEmbedded
                    ? fastContextToolsStatus.detectionFailed
                      ? "暂时无法确认 FastCtx 状态，内置工具保持关闭"
                      : "已检测到已配置的 FastCtx，Codey 不会重复加载内置工具"
                    : config.fastContextTools
                      ? "下次启动加载 Codey 内置 FastCtx 文件工具"
                      : "保持 Codex 默认文件工具，不加载额外 MCP"}
                </small>
              </div>
            </div>

            <div
              className={`feature-card ${config.disableTraceLogWrites ? "active" : ""}`}
            >
              <div className="feature-card-header">
                <strong>Trace 日志写盘保护</strong>
                <div className="feature-card-actions">
                  <Button
                    className="feature-action-btn"
                    size="xs"
                    disabled={isBusy}
                    loading={cleanupBusy}
                    onClick={() => onAnalyzeDiagnosticStorage("trace")}
                    aria-label="分析并清理 Trace 日志"
                  >
                    分析并清理
                  </Button>
                  <Switch
                    checked={config.disableTraceLogWrites}
                    disabled={isBusy}
                    onCheckedChange={(checked) =>
                      onConfigChange({
                        ...config,
                        disableTraceLogWrites: checked,
                      })
                    }
                    aria-label="启用 Codex Trace 日志写盘保护"
                  />
                </div>
              </div>
              <div className="feature-card-body">
                <small>阻止 Trace 日志持续写入数据库影响硬盘寿命</small>
              </div>
            </div>

            {isMacClient && (
              <div
                className={`feature-card ${config.protectCrashpadPending ? "active" : ""}`}
              >
                <div className="feature-card-header">
                  <strong>Crashpad 磁盘保护</strong>
                  <div className="feature-card-actions">
                    <Button
                      className="feature-action-btn"
                      size="xs"
                      disabled={isBusy}
                      loading={cleanupBusy}
                      onClick={() => onAnalyzeDiagnosticStorage("crashpad")}
                      aria-label="分析并清理 Crashpad 报告"
                    >
                      分析并清理
                    </Button>
                    <Switch
                      checked={config.protectCrashpadPending}
                      disabled={isBusy}
                      onCheckedChange={(checked) =>
                        onConfigChange({
                          ...config,
                          protectCrashpadPending: checked,
                        })}
                      aria-label="启用 Codex Crashpad 磁盘保护"
                    />
                  </div>
                </div>
                <div className="feature-card-body">
                  <small>
                    {config.protectCrashpadPending
                      ? "待处理崩溃报告超过安全上限时自动收敛，并保留最近写入"
                      : "仅显示占用和提供手动清理，不执行自动容量保护"}
                  </small>
                </div>
              </div>
            )}

            <div
              className={`feature-card ${config.hideFullAccessWarning ? "active" : ""}`}
            >
              <div className="feature-card-header">
                <strong>屏蔽完全访问安全提示</strong>
                <Switch
                  checked={config.hideFullAccessWarning}
                  disabled={isBusy}
                  onCheckedChange={(checked) =>
                    onConfigChange({ ...config, hideFullAccessWarning: checked })
                  }
                  aria-label="屏蔽完全访问安全提示"
                />
              </div>
              <div className="feature-card-body">
                <small>
                  {config.hideFullAccessWarning
                    ? "自动隐藏完全访问模式和 Ultra 的原生安全提示"
                    : "保留 Codex 原生安全提示"}
                </small>
              </div>
            </div>
          </div>
        </div>
      </section>

      {onAddChannel && onChannelChange && onRequestRemoveChannel && (
        <section className="secondary-section notification-section" aria-labelledby="notification-title">
          <NotificationChannelsCard
            config={config}
            container={popupContainer ?? null}
            popupContainer={popupContainer ?? null}
            isBusy={isBusy}
            onAddChannel={onAddChannel}
            onChannelChange={onChannelChange}
            onRequestRemoveChannel={onRequestRemoveChannel}
          />
        </section>
      )}
    </>
  );
}

export const FeaturePolicyCard = memo(FeaturePolicyCardComponent);
