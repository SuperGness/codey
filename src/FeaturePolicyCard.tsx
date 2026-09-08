import { Table } from "antd";
import { memo, type CSSProperties } from "react";
import { IconAdjustmentsHorizontal, IconInfoCircle, IconUsersGroup } from "@tabler/icons-react";

import type {
  Config,
  FastContextToolsStatus,
  SubagentRoleId,
} from "./App.types";
import {
  Button,
  Badge,
  Card,
  Select,
  Switch,
  Tooltip,
} from "./components/antd";
import { ModelCombobox } from "./components/ModelCombobox";
import { routeProviderId } from "./modelRoutes";
import {
  resolveSubagentModelOption,
  type SubagentModelOption,
} from "./subagentModels";
import { surfaceCardPaddingClass } from "./uiClasses";

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
const SUBAGENT_TASK_TYPES = [
  {
    id: "codey_quick_scan",
    name: "快速定位",
    access: "readOnly",
    description: "默认只读；用于精确位置、重复性检查、低风险事实查找和小范围快速检索。",
  },
  {
    id: "codey_deep_research",
    name: "深度检索",
    access: "readOnly",
    description: "默认只读；用于跨文件、日志、代码和文档的宽范围检索、归纳与架构探索。",
  },
  {
    id: "codey_visual_analysis",
    name: "视觉分析",
    access: "readOnly",
    description: "默认只读；仅用于必须读取截图、页面、GUI、PDF 或渲染结果的视觉证据分析。",
  },
  {
    id: "codey_worker",
    name: "代码实施",
    access: "write",
    description: "默认可写；用于边界清晰、可回滚、可测试的低到中等复杂度非视觉实现。",
  },
  {
    id: "codey_visual_worker",
    name: "视觉实施",
    access: "write",
    description: "默认可写；用于页面、GUI、PDF 或其他依赖视觉证据和渲染验证的实现。",
  },
] as const satisfies ReadonlyArray<{
  id: SubagentRoleId;
  name: string;
  access: "readOnly" | "write";
  description: string;
}>;

const WRITABLE_SUBAGENT_ROLE_IDS = [
  "codey_worker",
  "codey_visual_worker",
] as const satisfies ReadonlyArray<SubagentRoleId>;

export type SubagentPolicyCardProps = {
  config: Config;
  popupContainer: HTMLElement | null;
  tooltipContainer: HTMLElement | null;
  isBusy: boolean;
  subagentModelOptions: SubagentModelOption[];
  onConfigChange: (config: Config) => void;
  onSubagentOptimizationChange: (checked: boolean) => void;
};

export function SubagentPolicyCardComponent({
  config,
  popupContainer,
  tooltipContainer,
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
  const writableRolesDisabled = WRITABLE_SUBAGENT_ROLE_IDS.every(
    (role) => config.subagentRoles[role]?.enabled === false,
  );
  const enabledReadOnlyRoleNames = SUBAGENT_TASK_TYPES.filter(
    ({ id, access }) =>
      access === "readOnly" && config.subagentRoles[id]?.enabled !== false,
  ).map(({ name }) => name);
  const writableRolesDisabledMessage =
    enabledReadOnlyRoleNames.length > 0
      ? `可写子代理已全部关闭；${enabledReadOnlyRoleNames.join("、")}仍可使用。`
      : "可写子代理已全部关闭；请先启用至少一个只读角色。";

  return (
    <section className="secondary-section subagent-section" aria-labelledby="subagent-title">
      <div className="section-title compact">
        <div className="section-heading">
          <span className="section-icon" aria-hidden="true">
            <IconUsersGroup size={15} />
          </span>
          <div>
            <h2 id="subagent-title">Codey 子代理角色与调度增强</h2>
            <p>基于 Codex 原生子代理的多角色调度与模型配置。</p>
          </div>
        </div>
        <Switch
          checked={config.subagentOptimization}
          disabled={isBusy}
          onCheckedChange={(checked) =>
            onSubagentOptimizationChange(checked)
          }
          aria-label="启用 Codey 子代理角色与调度增强"
        />
      </div>
      <Card className={`secondary-card subagent-card ${surfaceCardPaddingClass}`}>
        <div className="subagent-policy-body">
          {config.subagentOptimization ? (
            <>
              <div className="subagent-table-container">
                <Table
 className="subagent-table"
 size="small" pagination={false} bordered={false}

 columns={[{ title: <>启用</>, width: 64, render: (_value, record) => record.cells[0] },
{ title: <>任务角色</>, width: 170, render: (_value, record) => record.cells[1] },
{ title: <>指定模型</>, width: undefined, render: (_value, record) => record.cells[2] },
{ title: <>思考深度</>, width: 130, render: (_value, record) => record.cells[3] }]}
 dataSource={SUBAGENT_TASK_TYPES.map((task) => {
   const selection = config.subagentRoles[task.id] ?? { enabled: true, model: config.subagentModel, reasoningEffort: config.subagentReasoningEffort };
   const selectedModel = resolveSubagentModelOption(subagentModelOptions, selection.model, preferredProviderId);
   const reasoningEfforts = selectedModel?.supportedReasoningEfforts ?? [];
   const reasoningOptions = reasoningEfforts.map((effort) => ({ label: REASONING_EFFORT_LABELS[effort] ?? effort, value: effort }));
   const updateRole = (next: Partial<typeof selection>) => onConfigChange({ ...config, subagentRoles: { ...config.subagentRoles, [task.id]: { ...selection, ...next } } });
   const roleDisabled = !selection.enabled;
return { key: task.id, roleDisabled, cells: [<div>
                            <Switch
                              checked={selection.enabled}
                              disabled={
                                subagentPolicyControlsDisabled ||
                                (selection.enabled && enabledRoleCount <= 1)
                              }
                              onCheckedChange={(enabled) => updateRole({ enabled })}
                              aria-label={`${selection.enabled ? "关闭" : "启用"}${task.name}角色`}
                            />
                          </div>,
<div>
                            <div className="subagent-role-name">
                              <span>{task.name}</span>
                              <Badge
                                variant={task.access === "write" ? "warning" : "secondary"}
                                size="xs"
                              >
                                {task.access === "write" ? "可写" : "只读"}
                              </Badge>
                              <Tooltip
                                content={task.description}
                                getPopupContainer={() =>
                                  popupContainer ?? tooltipContainer ?? document.body
                                }
                                position="top"
                              >
                                <Button
                                  variant="ghost"
                                  size="xs"
                                  className="subagent-role-info-btn"
                                  aria-label={`${task.name}：${task.description}`}
                                >
                                  <IconInfoCircle size={13} aria-hidden="true" />
                                </Button>
                              </Tooltip>
                            </div>
                          </div>,
<div>
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
                              getPopupContainer={() => popupContainer ?? document.body}
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
                          </div>,
<div>
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
                              dropdownClassName="rounded-[10px]"
                              showClear={false}
                              filter={false}
                              getPopupContainer={() => popupContainer ?? document.body}
                              onChange={(value) =>
                                updateRole({
                                  model: selectedModel?.value ?? selection.model,
                                  reasoningEffort: String(value ?? ""),
                                })
                              }
                            />
                          </div>] };})}
 rowClassName={(record) => record.roleDisabled ? "subagent-role-disabled" : ""}
 />
              </div>
              <div className="subagent-policy-callout">
                <IconInfoCircle size={14} className="subagent-callout-icon" aria-hidden="true" />
                <div className="subagent-callout-text">
                  {subagentModelOptions.length === 0
                    ? "请先在模型管理中为任一可用线路启用模型。"
                    : writableRolesDisabled
                      ? `${writableRolesDisabledMessage}角色启用状态变更需重启 Codex，模型和思考深度保存后对下次派生生效。`
                      : "可搜索并选择任意可用线路模型；角色启用状态变更需重启 Codex，模型和思考深度保存后对下次派生生效。角色权限仍受父任务权限模式约束。"}
                </div>
              </div>
            </>
          ) : (
            <div className="feature-disabled-placeholder">
              <div className="feature-disabled-icon">
                <IconUsersGroup size={22} aria-hidden="true" />
              </div>
              <div className="feature-disabled-text">
                <strong>子代理角色与调度增强已关闭</strong>
                <p>开启后仅在宽范围、可并行或需要专门证据时选择性委派，并提供五类专用角色与汇合门禁。</p>
              </div>
            </div>
          )}
        </div>
      </Card>
    </section>
  );
}

export const SubagentPolicyCard = memo(SubagentPolicyCardComponent);

type FeaturePolicyCardProps = {
  config: Config;
  fastContextToolsStatus: FastContextToolsStatus;
  isMacClient: boolean;
  isWindowsClient: boolean;
  popupContainer: HTMLElement | null;
  tooltipContainer: HTMLElement | null;
  isBusy: boolean;
  onConfigChange: (config: Config) => void;
};

function FeaturePolicyCardComponent({
  config,
  fastContextToolsStatus,
  isMacClient,
  isWindowsClient,
  popupContainer,
  tooltipContainer,
  isBusy,
  onConfigChange,
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
    <section className="secondary-section" aria-labelledby="runtime-title">
      <div className="section-title compact">
        <div className="section-heading">
          <span className="section-icon" aria-hidden="true">
            <IconAdjustmentsHorizontal size={15} />
          </span>
          <div>
            <h2 id="runtime-title">Codex 功能策略</h2>
            <p>按需精简客户端模块和界面行为。</p>
          </div>
        </div>
      </div>
      <Card className={`secondary-card runtime-card ${surfaceCardPaddingClass}`}>
        <div className="feature-grid">
          {/* GPU 渲染模式：占满整行全宽，仅 Windows 客户端展示 */}
          {isWindowsClient && (
            <div
              className={`feature-card gpu-mode-card full-width-card ${gpuLaunchMode.value !== "off" ? "active" : ""}`}
            >
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
                <Badge variant="secondary">v0.2.6</Badge>
              </div>
              {fastctxStatusBlocksEmbedded ? (
                <Tooltip
                  content={fastctxBlockedReason}
                  getPopupContainer={() =>
                    popupContainer ?? tooltipContainer ?? document.body
                  }
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
            <div className="feature-card-body">
              <small>阻止Trace日志持续写入数据库影响硬盘寿命</small>
            </div>
          </div>

          {isMacClient && (
            <div
              className={`feature-card ${config.protectCrashpadPending ? "active" : ""}`}
            >
              <div className="feature-card-header">
                <strong>Crashpad 磁盘保护</strong>
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

export const FeaturePolicyCard = memo(FeaturePolicyCardComponent);
