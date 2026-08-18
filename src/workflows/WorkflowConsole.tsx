import {
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  IconAlertTriangle,
  IconArchive,
  IconChevronDown,
  IconChevronRight,
  IconClock,
  IconFileDescription,
  IconGitBranch,
  IconLoader2,
  IconPlayerPause,
  IconPlayerPlay,
  IconRefresh,
  IconRotateClockwise,
  IconShieldCheck,
  IconSquareX,
  IconUserQuestion,
} from "@tabler/icons-react";

import { Badge, Button } from "../components/semi";
import { workflowApi } from "./api";
import { useWorkflowRuns } from "./useWorkflowRuns";
import type {
  WorkflowApi,
  WorkflowArtifact,
  WorkflowArtifactPage,
  WorkflowInteractionChoice,
  WorkflowInteractionKind,
  WorkflowNodeState,
  WorkflowRisk,
  WorkflowRunState,
  WorkflowTimestamp,
} from "./types";

const ARTIFACT_PAGE_BYTES = 16_384;
const MAX_RENDERED_ARTIFACT_CHARS = 16_000;

const RUN_STATE_LABELS: Record<WorkflowRunState, string> = {
  created: "已创建",
  queued: "排队中",
  running: "运行中",
  recovering: "恢复核对中",
  succeeded: "已成功",
  failed: "失败",
  needsAttention: "需要介入",
  pausing: "暂停中",
  paused: "已暂停",
  canceling: "取消中",
  canceled: "已取消",
};

const NODE_STATE_LABELS: Record<WorkflowNodeState, string> = {
  pending: "等待依赖",
  ready: "就绪",
  leased: "已租赁",
  running: "运行中",
  waitingApproval: "等待审批",
  pausing: "暂停中",
  paused: "已暂停",
  succeeded: "已成功",
  failed: "失败",
  canceled: "已取消",
  skipped: "已跳过",
  unknownOutcome: "结果未知",
  compensating: "补偿中",
  compensated: "已补偿",
};

const MODE_LABELS = {
  direct: "Direct",
  guarded: "Guarded",
  parallel: "Parallel",
  expert: "Expert",
} as const;

const RISK_LABELS: Record<WorkflowRisk, string> = {
  low: "低风险",
  medium: "中风险",
  high: "高风险",
  critical: "关键风险",
};

type BadgeTone =
  | "secondary"
  | "success"
  | "warning"
  | "destructive"
  | "info"
  | "brand";

interface ArtifactViewState {
  artifactId: string;
  page: WorkflowArtifactPage | null;
  loading: boolean;
  error: string | null;
  offsets: number[];
  frontendTruncated: boolean;
}

interface PendingPrompt {
  id: string;
  kind: WorkflowInteractionKind;
  title: string;
  prompt: string;
  risk?: WorkflowRisk;
  requestRevision: number;
  choices?: WorkflowInteractionChoice[];
}

export interface WorkflowConsoleProps {
  active?: boolean;
  api?: WorkflowApi;
  className?: string;
  initialRunId?: string;
  threadId?: string;
  onRunSelectionChange?: (runId: string) => void;
}

export function redactWorkflowText(value: string): string {
  return value
    .replace(/\bBearer\s+[A-Za-z0-9._~+\/-]{8,}/gi, "Bearer [已隐藏]")
    .replace(/\b(?:sk|xox[baprs])-[A-Za-z0-9_-]{8,}/gi, "[已隐藏凭据]")
    .replace(
      /((?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|authorization)\s*[=:]\s*)[^\s,;]+/gi,
      "$1[已隐藏]",
    );
}

function safeText(value?: string): string {
  return redactWorkflowText(value?.trim() || "");
}

function runStateLabel(state: WorkflowRunState): string {
  return RUN_STATE_LABELS[state] ?? state;
}

function nodeStateLabel(state: WorkflowNodeState): string {
  return NODE_STATE_LABELS[state] ?? state;
}

function stateTone(state: WorkflowRunState | WorkflowNodeState): BadgeTone {
  if (state === "succeeded" || state === "compensated") return "success";
  if (
    state === "failed" ||
    state === "canceled" ||
    state === "unknownOutcome"
  ) return "destructive";
  if (
    state === "needsAttention" ||
    state === "waitingApproval" ||
    state === "pausing" ||
    state === "paused" ||
    state === "canceling"
  ) return "warning";
  if (
    state === "running" ||
    state === "recovering" ||
    state === "leased" ||
    state === "ready"
  ) return "info";
  return "secondary";
}

function riskTone(risk?: WorkflowRisk): BadgeTone {
  if (risk === "critical" || risk === "high") return "destructive";
  if (risk === "medium") return "warning";
  return "secondary";
}

function formatTimestamp(value?: WorkflowTimestamp): string {
  if (value === undefined || value === "") return "时间未知";
  const numeric = typeof value === "number" && value < 1_000_000_000_000
    ? value * 1_000
    : value;
  const date = new Date(numeric);
  if (Number.isNaN(date.getTime())) return String(value);
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(date);
}

function formatDuration(milliseconds?: number): string {
  if (milliseconds === undefined || milliseconds < 0) return "—";
  const seconds = Math.floor(milliseconds / 1_000);
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分 ${seconds % 60} 秒`;
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`;
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function isTextMime(mimeType: string): boolean {
  return (
    mimeType.startsWith("text/") ||
    mimeType.includes("json") ||
    mimeType.includes("xml") ||
    mimeType.includes("yaml") ||
    mimeType.includes("javascript")
  );
}

function nodeDomId(nodeId: string): string {
  return `workflow-node-${nodeId.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
}

export function WorkflowConsole({
  active = true,
  api = workflowApi,
  className,
  initialRunId,
  threadId,
  onRunSelectionChange,
}: WorkflowConsoleProps) {
  const controller = useWorkflowRuns({ active, api, initialRunId, threadId });
  const [expandedNodeId, setExpandedNodeId] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [artifactView, setArtifactView] = useState<ArtifactViewState | null>(null);
  const artifactAbortRef = useRef<AbortController | null>(null);
  const snapshot = controller.snapshot;
  const run = snapshot?.run;

  useEffect(() => {
    artifactAbortRef.current?.abort();
    setArtifactView(null);
    setExpandedNodeId(null);
    setDrafts({});
  }, [controller.selectedRunId]);

  useEffect(() => () => artifactAbortRef.current?.abort(), []);

  const nodesById = useMemo(
    () => new Map((snapshot?.nodes ?? []).map((node) => [node.nodeId, node])),
    [snapshot?.nodes],
  );

  const pendingPrompts = useMemo<PendingPrompt[]>(() => {
    if (!snapshot) return [];
    const interactions = snapshot.interactions
      .filter((interaction) => interaction.state === "pending")
      .map((interaction) => ({
        id: interaction.interactionId,
        kind: interaction.kind,
        title: interaction.title,
        prompt: interaction.prompt,
        risk: interaction.risk,
        requestRevision: interaction.requestRevision,
        choices: interaction.choices,
      }));
    const known = new Set(interactions.map((interaction) => interaction.id));
    const approvals = snapshot.approvals
      .filter((approval) => approval.state === "pending")
      .map((approval) => ({
        id: approval.interactionId ?? approval.approvalId,
        kind: "approval" as const,
        title: approval.title,
        prompt: approval.prompt,
        risk: approval.risk,
        requestRevision: approval.requestRevision,
      }))
      .filter((approval) => !known.has(approval.id));
    return [...interactions, ...approvals];
  }, [snapshot]);

  const selectRun = (runId: string) => {
    controller.selectRun(runId);
    onRunSelectionChange?.(runId);
  };

  const focusNode = (nodeId: string) => {
    setExpandedNodeId(nodeId);
    window.requestAnimationFrame(() => {
      document.getElementById(nodeDomId(nodeId))?.focus();
    });
  };

  const loadArtifact = async (
    artifact: WorkflowArtifact,
    offset: number,
    offsets: number[],
  ) => {
    if (!run) return;
    artifactAbortRef.current?.abort();
    const abortController = new AbortController();
    artifactAbortRef.current = abortController;
    setArtifactView({
      artifactId: artifact.artifactId,
      page: null,
      loading: true,
      error: null,
      offsets,
      frontendTruncated: false,
    });
    try {
      const page = await api.artifact(
        {
          runId: run.runId,
          artifactId: artifact.artifactId,
          offset,
          limit: ARTIFACT_PAGE_BYTES,
        },
        { signal: abortController.signal },
      );
      if (abortController.signal.aborted) return;
      const rawText = page.text ?? page.content ?? "";
      setArtifactView({
        artifactId: artifact.artifactId,
        page: {
          ...page,
          text: rawText.slice(0, MAX_RENDERED_ARTIFACT_CHARS),
          content: undefined,
        },
        loading: false,
        error: null,
        offsets,
        frontendTruncated: rawText.length > MAX_RENDERED_ARTIFACT_CHARS,
      });
    } catch (error) {
      if (abortController.signal.aborted) return;
      setArtifactView({
        artifactId: artifact.artifactId,
        page: null,
        loading: false,
        error: error instanceof Error ? safeText(error.message) : "证据加载失败",
        offsets,
        frontendTruncated: false,
      });
    }
  };

  const openArtifact = (artifact: WorkflowArtifact) => {
    if (
      artifact.sensitive &&
      !window.confirm("此 Artifact 标记为敏感。继续后只显示当前分页，疑似凭据仍会自动隐藏。")
    ) return;
    void loadArtifact(artifact, 0, []);
  };

  const actionUnavailable =
    controller.capabilities?.readOnly
      ? "引擎处于只读安全模式，状态变更已禁用。"
      : !controller.capabilities?.available
        ? controller.capabilities?.unavailableReason || "工作流引擎当前不可用。"
        : null;
  const anyBusy = controller.busyAction !== null;
  const canPause = Boolean(
    run?.state === "running" && controller.capabilities?.actions.pause,
  );
  const canResume = Boolean(
    (run?.state === "paused" ||
      (run?.state === "needsAttention" && run.resumable !== false)) &&
      controller.capabilities?.actions.resume,
  );
  const canCancel = Boolean(
    run &&
      ["created", "queued", "running", "needsAttention", "pausing", "paused"].includes(
        run.state,
      ) &&
      controller.capabilities?.actions.cancel,
  );

  return (
    <section
      className={`workflow-console${className ? ` ${className}` : ""}`}
      aria-labelledby="workflow-console-title"
    >
      <div className="workflow-console-header">
        <div>
          <span className="section-kicker">Workflow Engine</span>
          <h1 id="workflow-console-title">
            {threadId ? "本会话工作流" : "工作流控制台"}
          </h1>
          <p>
            {threadId
              ? "仅显示当前 Codex 会话关联的运行、人工门禁与证据。"
              : "查看真实执行状态、人工门禁与可追溯证据。"}
          </p>
        </div>
        <div className="workflow-console-header-actions">
          <Badge
            variant={
              controller.capabilities?.available
                ? controller.capabilities.readOnly
                  ? "warning"
                  : "success"
                : "secondary"
            }
          >
            {!controller.documentVisible
              ? "轮询已暂停"
              : controller.capabilities?.readOnly
                ? "只读模式"
                : controller.capabilities?.available
                  ? "引擎在线"
                  : "引擎不可用"}
          </Badge>
          <Button
            variant="outline"
            size="sm"
            disabled={controller.refreshing || !controller.documentVisible}
            onClick={() => void controller.refresh()}
          >
            <IconRefresh className={controller.refreshing ? "spinner" : ""} aria-hidden="true" />
            刷新
          </Button>
        </div>
      </div>

      <div className="workflow-live-region" aria-live="polite" aria-atomic="true">
        {controller.refreshing
          ? "正在刷新工作流状态"
          : controller.error
            ? `刷新失败：${safeText(controller.error)}`
            : controller.lastUpdatedAt
              ? `状态已更新：${formatTimestamp(controller.lastUpdatedAt)}`
              : "等待工作流状态"}
      </div>

      {(controller.error || controller.mutationError || actionUnavailable) && (
        <div className="workflow-alert" role="alert">
          <IconAlertTriangle aria-hidden="true" />
          <span>
            {safeText(controller.mutationError || controller.error || actionUnavailable || "")}
          </span>
          {controller.mutationError && (
            <Button size="xs" variant="ghost" onClick={controller.clearMutationError}>
              关闭
            </Button>
          )}
        </div>
      )}

      <div className="workflow-console-layout">
        <aside className="workflow-run-sidebar" aria-label="工作流运行列表">
          <div className="workflow-pane-heading">
            <strong>{threadId ? "本会话运行" : "运行记录"}</strong>
            <span>{controller.runs.length}</span>
          </div>
          {controller.initialLoading ? (
            <div className="workflow-empty" role="status">
              <IconLoader2 className="spinner" aria-hidden="true" />
              正在加载运行记录…
            </div>
          ) : controller.runs.length === 0 ? (
            <div className="workflow-empty">暂无工作流运行</div>
          ) : (
            <ul className="workflow-run-list">
              {controller.runs.map((item) => (
                <li key={`${item.runId}:${item.generation}`}>
                  <button
                    type="button"
                    className="workflow-run-option"
                    aria-current={controller.selectedRunId === item.runId ? "true" : undefined}
                    onClick={() => selectRun(item.runId)}
                  >
                    <span className="workflow-run-option-topline">
                      <strong>{safeText(item.title)}</strong>
                      <Badge variant={stateTone(item.state)}>{runStateLabel(item.state)}</Badge>
                    </span>
                    <span className="workflow-run-option-meta">
                      <span>{MODE_LABELS[item.mode] ?? item.mode}</span>
                      <span>{RISK_LABELS[item.risk] ?? item.risk}</span>
                      <time>{formatTimestamp(item.updatedAt)}</time>
                    </span>
                    {item.blockedReason && (
                      <span className="workflow-run-blocked">{safeText(item.blockedReason)}</span>
                    )}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </aside>

        <main className="workflow-run-detail" aria-label="选中的工作流详情">
          {!controller.selectedRunId ? (
            <div className="workflow-detail-empty">
              <IconGitBranch aria-hidden="true" />
              <h2>尚未选择运行</h2>
              <p>从左侧选择一条运行记录查看节点、事件与证据。</p>
            </div>
          ) : !snapshot || !run ? (
            <div className="workflow-detail-empty" role="status">
              <IconLoader2 className="spinner" aria-hidden="true" />
              <h2>正在同步运行快照</h2>
            </div>
          ) : (
            <>
              <header className="workflow-run-header">
                <div className="workflow-run-title">
                  <div>
                    <span className="section-kicker">Run · generation {run.generation}</span>
                    <h2>{safeText(run.title)}</h2>
                  </div>
                  <div className="workflow-run-badges">
                    <Badge variant={stateTone(run.state)}>{runStateLabel(run.state)}</Badge>
                    <Badge variant="brand">{MODE_LABELS[run.mode] ?? run.mode}</Badge>
                    <Badge variant={riskTone(run.risk)}>{RISK_LABELS[run.risk] ?? run.risk}</Badge>
                  </div>
                </div>
                {(run.blockedReason || run.recoveryHint) && (
                  <div className="workflow-block-reason">
                    <IconAlertTriangle aria-hidden="true" />
                    <div>
                      <strong>{run.blockedReason ? "阻塞原因" : "恢复提示"}</strong>
                      <p>{safeText(run.blockedReason || run.recoveryHint)}</p>
                    </div>
                  </div>
                )}
                <div className="workflow-run-actions" aria-label="运行控制">
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={!canPause || anyBusy}
                    onClick={() => void controller.pause()}
                  >
                    {controller.busyAction === "pause"
                      ? <IconLoader2 className="spinner" aria-hidden="true" />
                      : <IconPlayerPause aria-hidden="true" />}
                    暂停
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={!canResume || anyBusy}
                    onClick={() => void controller.resume()}
                  >
                    {controller.busyAction === "resume"
                      ? <IconLoader2 className="spinner" aria-hidden="true" />
                      : <IconPlayerPlay aria-hidden="true" />}
                    恢复
                  </Button>
                  <Button
                    size="sm"
                    variant="destructive"
                    disabled={!canCancel || anyBusy}
                    onClick={() => {
                      if (window.confirm("取消只会提交取消意图；执行侧确认停止前仍会显示“取消中”。确定继续吗？")) {
                        void controller.cancel();
                      }
                    }}
                  >
                    {controller.busyAction === "cancel"
                      ? <IconLoader2 className="spinner" aria-hidden="true" />
                      : <IconSquareX aria-hidden="true" />}
                    取消
                  </Button>
                </div>
                <p className="workflow-action-note">
                  {actionUnavailable || "按钮只提交持久化意图，最终状态以引擎确认后的快照为准。"}
                </p>
              </header>

              <div className="workflow-metrics" aria-label="运行摘要">
                <div><span>阶段</span><strong>{safeText(run.phase) || "未标记"}</strong></div>
                <div><span>进度</span><strong>{run.progress ? `${run.progress.completedNodes}/${run.progress.totalNodes}` : `${snapshot.nodes.filter((node) => node.state === "succeeded").length}/${snapshot.nodes.length}`}</strong></div>
                <div><span>Token</span><strong>{(run.usage?.totalTokens ?? 0).toLocaleString("zh-CN")}</strong></div>
                <div><span>耗时</span><strong>{formatDuration(run.elapsedMs)}</strong></div>
                <div><span>并行度</span><strong>{run.currentParallelism ?? 0}/{run.maxParallelism ?? 1}</strong></div>
                <div><span>重试</span><strong>{run.retryCount ?? snapshot.nodes.reduce((sum, node) => sum + Math.max(0, node.attemptCount - 1), 0)}</strong></div>
              </div>

              {pendingPrompts.length > 0 && (
                <section className="workflow-section workflow-interactions" aria-labelledby="workflow-interactions-title">
                  <div className="workflow-section-heading">
                    <div><IconUserQuestion aria-hidden="true" /><h3 id="workflow-interactions-title">待人工处理</h3></div>
                    <Badge variant="warning">{pendingPrompts.length} 项</Badge>
                  </div>
                  <div className="workflow-interaction-list">
                    {pendingPrompts.map((prompt) => {
                      const busy = controller.busyAction === `reply:${prompt.id}`;
                      const submitChoice = (choiceId: string) =>
                        controller.replyInteraction(prompt.id, prompt.requestRevision, { choiceId });
                      return (
                        <article key={prompt.id} className="workflow-interaction-card">
                          <div className="workflow-interaction-title">
                            <strong>{safeText(prompt.title)}</strong>
                            <Badge variant={riskTone(prompt.risk)}>
                              {prompt.kind === "approval" ? "审批" : prompt.kind === "adjudication" ? "人工裁决" : "需要输入"}
                            </Badge>
                          </div>
                          <p>{safeText(prompt.prompt)}</p>
                          {prompt.choices && prompt.choices.length > 0 ? (
                            <div className="workflow-interaction-actions">
                              {prompt.choices.map((choice) => (
                                <Button
                                  key={choice.id}
                                  size="sm"
                                  variant={choice.destructive ? "destructive" : "outline"}
                                  disabled={anyBusy}
                                  title={safeText(choice.description)}
                                  onClick={() => void submitChoice(choice.id)}
                                >
                                  {busy && <IconLoader2 className="spinner" aria-hidden="true" />}
                                  {safeText(choice.label)}
                                </Button>
                              ))}
                            </div>
                          ) : prompt.kind === "approval" ? (
                            <div className="workflow-interaction-actions">
                              <Button disabled={anyBusy} size="sm" onClick={() => void controller.replyInteraction(prompt.id, prompt.requestRevision, { decision: "approved" })}>批准</Button>
                              <Button disabled={anyBusy} size="sm" variant="destructive" onClick={() => void controller.replyInteraction(prompt.id, prompt.requestRevision, { decision: "rejected" })}>拒绝</Button>
                            </div>
                          ) : (
                            <div className="workflow-interaction-input">
                              <label htmlFor={`workflow-reply-${prompt.id}`}>回复内容</label>
                              <textarea
                                id={`workflow-reply-${prompt.id}`}
                                value={drafts[prompt.id] ?? ""}
                                onChange={(event) => setDrafts((current) => ({ ...current, [prompt.id]: event.target.value }))}
                                rows={3}
                              />
                              <Button
                                size="sm"
                                disabled={anyBusy || !(drafts[prompt.id] ?? "").trim()}
                                onClick={() => void controller.replyInteraction(prompt.id, prompt.requestRevision, { value: (drafts[prompt.id] ?? "").trim() })}
                              >
                                {busy && <IconLoader2 className="spinner" aria-hidden="true" />}
                                提交回复
                              </Button>
                            </div>
                          )}
                        </article>
                      );
                    })}
                  </div>
                </section>
              )}

              <section className="workflow-section" aria-labelledby="workflow-nodes-title">
                <div className="workflow-section-heading">
                  <div><IconGitBranch aria-hidden="true" /><h3 id="workflow-nodes-title">节点与依赖</h3></div>
                  <span>结构化列表，可用键盘展开</span>
                </div>
                <ol className="workflow-node-list">
                  {snapshot.nodes.map((node) => {
                    const expanded = expandedNodeId === node.nodeId;
                    const retryable = node.state === "failed" && controller.capabilities?.actions.retryNode;
                    const retryBusy = controller.busyAction === `retry:${node.nodeId}`;
                    return (
                      <li key={node.nodeId} className={`workflow-node tone-${stateTone(node.state)}`}>
                        <button
                          id={nodeDomId(node.nodeId)}
                          type="button"
                          className="workflow-node-summary"
                          aria-expanded={expanded}
                          onClick={() => setExpandedNodeId(expanded ? null : node.nodeId)}
                        >
                          {expanded ? <IconChevronDown aria-hidden="true" /> : <IconChevronRight aria-hidden="true" />}
                          <span className="workflow-node-copy">
                            <strong>{safeText(node.title)}</strong>
                            <small>{safeText(node.role || node.executor || node.kind) || "执行者未分配"}</small>
                          </span>
                          <span className="workflow-node-dependency-count">依赖 {node.dependencies.length}</span>
                          <Badge variant={stateTone(node.state)}>{nodeStateLabel(node.state)}</Badge>
                        </button>
                        {expanded && (
                          <div className="workflow-node-details">
                            <dl>
                              <div><dt>节点 ID</dt><dd>{safeText(node.nodeId)}</dd></div>
                              <div><dt>尝试次数</dt><dd>{node.attemptCount}</dd></div>
                              <div><dt>执行者</dt><dd>{safeText(node.executor) || "尚未分配"}</dd></div>
                            </dl>
                            <div className="workflow-dependencies">
                              <strong>前置依赖</strong>
                              {node.dependencies.length === 0 ? <span>无</span> : node.dependencies.map((dependency) => (
                                <button key={dependency} type="button" onClick={() => focusNode(dependency)}>
                                  {safeText(nodesById.get(dependency)?.title || dependency)}
                                </button>
                              ))}
                            </div>
                            {(node.blockedReason || node.recoveryHint || node.lastAttempt?.errorMessage) && (
                              <p className="workflow-node-message">{safeText(node.blockedReason || node.recoveryHint || node.lastAttempt?.errorMessage)}</p>
                            )}
                            <div className="workflow-node-footer">
                              {node.state === "unknownOutcome" && <span>结果未知时禁止直接重试，请先人工核对副作用。</span>}
                              <Button
                                size="xs"
                                variant="outline"
                                disabled={!retryable || anyBusy || node.state === "unknownOutcome"}
                                onClick={() => {
                                  if (window.confirm("重试会创建新的 attempt，不会覆盖历史记录。确定继续吗？")) void controller.retryNode(node.nodeId);
                                }}
                              >
                                {retryBusy ? <IconLoader2 className="spinner" aria-hidden="true" /> : <IconRotateClockwise aria-hidden="true" />}
                                重试节点
                              </Button>
                            </div>
                          </div>
                        )}
                      </li>
                    );
                  })}
                </ol>
              </section>

              <section className="workflow-section" aria-labelledby="workflow-artifacts-title">
                <div className="workflow-section-heading">
                  <div><IconArchive aria-hidden="true" /><h3 id="workflow-artifacts-title">证据与 Artifact</h3></div>
                  <span>按需加载，每页最多 {formatBytes(ARTIFACT_PAGE_BYTES)}</span>
                </div>
                {snapshot.artifacts.length === 0 ? (
                  <div className="workflow-section-empty">尚无可用证据</div>
                ) : (
                  <ul className="workflow-artifact-list">
                    {snapshot.artifacts.map((artifact) => (
                      <li key={artifact.artifactId}>
                        <div className="workflow-artifact-meta">
                          <IconFileDescription aria-hidden="true" />
                          <div><strong>{safeText(artifact.title)}</strong><span>{artifact.mimeType} · {formatBytes(artifact.byteLength)}</span></div>
                          {artifact.sensitive && <Badge variant="warning">敏感</Badge>}
                        </div>
                        <Button size="xs" variant="outline" disabled={!controller.capabilities?.actions.artifact || artifactView?.loading} onClick={() => openArtifact(artifact)}>
                          加载当前页
                        </Button>
                      </li>
                    ))}
                  </ul>
                )}
                {artifactView && (
                  <div className="workflow-artifact-viewer" aria-live="polite">
                    {artifactView.loading ? (
                      <div className="workflow-empty"><IconLoader2 className="spinner" aria-hidden="true" />正在读取证据分页…</div>
                    ) : artifactView.error ? (
                      <div className="workflow-inline-error" role="alert">{artifactView.error}</div>
                    ) : artifactView.page ? (
                      <>
                        <div className="workflow-artifact-viewer-heading">
                          <div><strong>{safeText(artifactView.page.artifact.title)}</strong><span>{artifactView.page.mimeType} · 偏移 {artifactView.page.offset.toLocaleString("zh-CN")}</span></div>
                          <Button size="xs" variant="ghost" onClick={() => setArtifactView(null)}>关闭</Button>
                        </div>
                        {isTextMime(artifactView.page.mimeType) ? (
                          <pre>{safeText(artifactView.page.text)}</pre>
                        ) : (
                          <p className="workflow-binary-note">二进制内容不在控制台内直接渲染，仅显示清单与分页元数据。</p>
                        )}
                        {(artifactView.frontendTruncated || artifactView.page.truncated) && <p className="workflow-truncation-note">当前只显示一个受限分页；大内容不会一次载入。</p>}
                        <div className="workflow-artifact-pagination">
                          <Button
                            size="xs"
                            variant="outline"
                            disabled={artifactView.offsets.length === 0}
                            onClick={() => {
                              const previousOffsets = artifactView.offsets.slice(0, -1);
                              const previous = artifactView.offsets[artifactView.offsets.length - 1] ?? 0;
                              void loadArtifact(artifactView.page!.artifact, previous, previousOffsets);
                            }}
                          >上一页</Button>
                          <Button
                            size="xs"
                            variant="outline"
                            disabled={artifactView.page.nextOffset === undefined}
                            onClick={() => void loadArtifact(artifactView.page!.artifact, artifactView.page!.nextOffset!, [...artifactView.offsets, artifactView.page!.offset])}
                          >下一页</Button>
                        </div>
                      </>
                    ) : null}
                  </div>
                )}
              </section>

              <section className="workflow-section" aria-labelledby="workflow-events-title">
                <div className="workflow-section-heading">
                  <div><IconClock aria-hidden="true" /><h3 id="workflow-events-title">事件时间线</h3></div>
                  <span>sequence {controller.lastSequence}</span>
                </div>
                {controller.eventsTruncated && <div className="workflow-history-note">仅保留最近事件或服务端可用历史；检测到增量缺口时会自动全量刷新。</div>}
                {controller.events.length === 0 ? (
                  <div className="workflow-section-empty">尚无事件</div>
                ) : (
                  <ol className="workflow-event-list">
                    {[...controller.events].reverse().map((event) => (
                      <li key={event.eventId || event.sequence}>
                        <span className={`workflow-event-dot severity-${event.severity ?? "info"}`} aria-hidden="true" />
                        <div>
                          <div className="workflow-event-title"><strong>{safeText(event.type).replace(/[_-]/g, " ")}</strong><span>#{event.sequence}</span></div>
                          <p>{safeText(event.summary || event.message || event.errorCategory) || "状态事件"}</p>
                          <small>{formatTimestamp(event.createdAt)}{event.actor ? ` · ${safeText(event.actor)}` : ""}</small>
                        </div>
                      </li>
                    ))}
                  </ol>
                )}
              </section>

              {(snapshot.acceptanceGate || run.acceptanceGate) && (
                <div className="workflow-acceptance-gate">
                  <IconShieldCheck aria-hidden="true" />
                  <div><strong>最终验收门禁：{safeText((snapshot.acceptanceGate ?? run.acceptanceGate)?.state)}</strong><p>{safeText((snapshot.acceptanceGate ?? run.acceptanceGate)?.summary) || "仅门禁提交成功后，运行才会显示为已成功。"}</p></div>
                </div>
              )}
            </>
          )}
        </main>
      </div>
    </section>
  );
}
