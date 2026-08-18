export type EngineEpoch = string;
export type RunGeneration = number;
export type Revision = number;
export type Sequence = number;
export type WorkflowTimestamp = string | number;

export type WorkflowRunState =
  | "created"
  | "queued"
  | "running"
  | "recovering"
  | "succeeded"
  | "failed"
  | "needsAttention"
  | "pausing"
  | "paused"
  | "canceling"
  | "canceled";

export type WorkflowNodeState =
  | "pending"
  | "ready"
  | "leased"
  | "running"
  | "waitingApproval"
  | "pausing"
  | "paused"
  | "succeeded"
  | "failed"
  | "canceled"
  | "skipped"
  | "unknownOutcome"
  | "compensating"
  | "compensated";

export type WorkflowAttemptState =
  | "created"
  | "queued"
  | "leased"
  | "running"
  | "waitingApproval"
  | "pausing"
  | "paused"
  | "succeeded"
  | "failed"
  | "canceled"
  | "unknownOutcome"
  | "compensating"
  | "compensated";

export type WorkflowMode = "direct" | "guarded" | "parallel" | "expert";
export type WorkflowRisk = "low" | "medium" | "high" | "critical";
export type WorkflowEventSeverity = "debug" | "info" | "warning" | "error";
export type WorkflowInteractionKind = "approval" | "input" | "adjudication";

export type WorkflowCapabilityAction =
  | "artifact"
  | "pause"
  | "resume"
  | "cancel"
  | "retryNode"
  | "replyInteraction";

export interface WorkflowCapabilityActions {
  artifact: boolean;
  pause: boolean;
  resume: boolean;
  cancel: boolean;
  retryNode: boolean;
  replyInteraction: boolean;
}

export interface WorkflowProtocolCapabilities {
  appServerVersion?: string;
  canCancel?: boolean;
  canCreateContext?: boolean;
  canQueryRun?: boolean;
  canReplyToApproval?: boolean;
  canReplyToInput?: boolean;
  canResumeContext?: boolean;
  canStreamStableEvents?: boolean;
  canTrackUsage?: boolean;
  unavailable?: string[];
}

export interface WorkflowCapabilities {
  enabled: boolean;
  available: boolean;
  readOnly: boolean;
  schemaVersion: number;
  engineEpoch: EngineEpoch;
  unavailableReason?: string;
  actions: WorkflowCapabilityActions;
  protocol?: WorkflowProtocolCapabilities;
}

export interface WorkflowUsageSummary {
  inputTokens?: number;
  outputTokens?: number;
  cachedInputTokens?: number;
  totalTokens?: number;
  estimatedCost?: number;
  currency?: string;
}

export interface WorkflowProgressSummary {
  completedNodes: number;
  failedNodes: number;
  runningNodes: number;
  totalNodes: number;
  waitingNodes?: number;
}

export interface WorkflowRunSummary {
  runId: string;
  originThreadId?: string;
  generation: RunGeneration;
  revision: Revision;
  state: WorkflowRunState;
  title: string;
  mode: WorkflowMode;
  risk: WorkflowRisk;
  phase?: string;
  engineEpoch?: EngineEpoch;
  latestSequence?: Sequence;
  blockedReason?: string;
  createdAt: WorkflowTimestamp;
  updatedAt: WorkflowTimestamp;
  startedAt?: WorkflowTimestamp;
  completedAt?: WorkflowTimestamp;
  progress?: WorkflowProgressSummary;
  usage?: WorkflowUsageSummary;
}

export interface WorkflowAcceptanceGate {
  state: "pending" | "passed" | "failed" | "inconclusive";
  summary?: string;
  reviewerCount?: number;
  evidenceArtifactIds?: string[];
}

export interface WorkflowRun extends WorkflowRunSummary {
  objective?: string;
  profile?: string;
  baseRevision?: string;
  policyRevision?: string;
  currentParallelism?: number;
  maxParallelism?: number;
  elapsedMs?: number;
  retryCount?: number;
  resumable?: boolean;
  cancelRequested?: boolean;
  pauseRequested?: boolean;
  recoveryHint?: string;
  acceptanceGate?: WorkflowAcceptanceGate;
}

export interface WorkflowAttempt {
  attemptId: string;
  runId: string;
  generation: RunGeneration;
  nodeId: string;
  number: number;
  state: WorkflowAttemptState;
  revision?: Revision;
  leaseEpoch?: number;
  executor?: string;
  model?: string;
  provider?: string;
  startedAt?: WorkflowTimestamp;
  completedAt?: WorkflowTimestamp;
  heartbeatAt?: WorkflowTimestamp;
  durationMs?: number;
  resultSummary?: string;
  errorCategory?: string;
  errorMessage?: string;
  retryable?: boolean;
  sideEffectState?: "none" | "planned" | "started" | "committed" | "unknown";
  artifactIds?: string[];
}

export interface WorkflowNode {
  nodeId: string;
  runId: string;
  generation: RunGeneration;
  revision?: Revision;
  sequence?: Sequence;
  title: string;
  kind?: string;
  role?: string;
  state: WorkflowNodeState;
  risk?: WorkflowRisk;
  readOnly?: boolean;
  dependencies: string[];
  dependents?: string[];
  executor?: string;
  attemptCount: number;
  lastAttempt?: WorkflowAttempt;
  attempts?: WorkflowAttempt[];
  blockedReason?: string;
  recoveryHint?: string;
  artifactIds?: string[];
  approvalIds?: string[];
}

export interface WorkflowEvent {
  eventId: string;
  runId: string;
  generation: RunGeneration;
  revision: Revision;
  sequence: Sequence;
  engineEpoch?: EngineEpoch;
  type: string;
  severity?: WorkflowEventSeverity;
  actor?: string;
  nodeId?: string;
  attemptId?: string;
  causationId?: string;
  correlationId?: string;
  createdAt: WorkflowTimestamp;
  summary?: string;
  message?: string;
  errorCategory?: string;
}

export interface WorkflowArtifact {
  artifactId: string;
  runId: string;
  generation: RunGeneration;
  nodeId?: string;
  attemptId?: string;
  kind: string;
  title: string;
  mimeType: string;
  byteLength: number;
  hash?: string;
  sensitive?: boolean;
  summary?: string;
  createdAt: WorkflowTimestamp;
}

export interface WorkflowArtifactPage {
  artifact: WorkflowArtifact;
  offset: number;
  limit: number;
  totalBytes: number;
  mimeType: string;
  text?: string;
  content?: string;
  encoding?: string;
  truncated: boolean;
  nextOffset?: number;
}

export type WorkflowApprovalState =
  | "pending"
  | "approved"
  | "rejected"
  | "expired"
  | "canceled";

export interface WorkflowApproval {
  approvalId: string;
  interactionId?: string;
  runId: string;
  generation: RunGeneration;
  nodeId?: string;
  attemptId?: string;
  requestRevision: Revision;
  state: WorkflowApprovalState;
  risk?: WorkflowRisk;
  title: string;
  prompt: string;
  permissions?: string[];
  createdAt: WorkflowTimestamp;
  expiresAt?: WorkflowTimestamp;
  respondedAt?: WorkflowTimestamp;
}

export interface WorkflowInteractionChoice {
  id: string;
  label: string;
  description?: string;
  destructive?: boolean;
}

export interface WorkflowInteraction {
  interactionId: string;
  runId: string;
  generation: RunGeneration;
  nodeId?: string;
  attemptId?: string;
  requestRevision: Revision;
  kind: WorkflowInteractionKind;
  state: "pending" | "answered" | "expired" | "canceled";
  title: string;
  prompt: string;
  risk?: WorkflowRisk;
  choices?: WorkflowInteractionChoice[];
  required?: boolean;
  createdAt: WorkflowTimestamp;
  expiresAt?: WorkflowTimestamp;
}

export interface WorkflowRunSnapshot {
  engineEpoch: EngineEpoch;
  generation: RunGeneration;
  revision: Revision;
  sequence: Sequence;
  run: WorkflowRun;
  nodes: WorkflowNode[];
  attempts: WorkflowAttempt[];
  events?: WorkflowEvent[];
  artifacts: WorkflowArtifact[];
  approvals: WorkflowApproval[];
  interactions: WorkflowInteraction[];
  acceptanceGate?: WorkflowAcceptanceGate;
}

export interface WorkflowListRequest {
  cursor?: string;
  limit?: number;
  states?: WorkflowRunState[];
  threadId?: string;
}

export interface WorkflowListResponse {
  engineEpoch: EngineEpoch;
  runs: WorkflowRunSummary[];
  threadId?: string;
  nextCursor?: string;
}

export interface WorkflowEventsRequest {
  runId: string;
  afterSequence: Sequence;
  limit?: number;
}

export interface WorkflowEventsResponse {
  runId: string;
  engineEpoch: EngineEpoch;
  generation: RunGeneration;
  revision: Revision;
  afterSequence: Sequence;
  latestSequence: Sequence;
  events: WorkflowEvent[];
  hasMore?: boolean;
  resetRequired?: boolean;
}

export interface WorkflowArtifactRequest {
  runId: string;
  artifactId: string;
  offset?: number;
  limit?: number;
}

export interface WorkflowInteractionReply {
  decision?: "approved" | "rejected";
  choiceId?: string;
  value?: string;
  comment?: string;
}

export interface WorkflowMutationResponse {
  accepted: boolean;
  commandId: string;
  runId: string;
  generation: RunGeneration;
  revision: Revision;
  state: WorkflowRunState;
  message?: string;
  snapshot?: WorkflowRunSnapshot;
}

export interface WorkflowRequestOptions {
  signal?: AbortSignal;
}

export interface WorkflowApi {
  capabilities: (options?: WorkflowRequestOptions) => Promise<WorkflowCapabilities>;
  list: (
    request?: WorkflowListRequest,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowListResponse>;
  get: (
    runId: string,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowRunSnapshot>;
  events: (
    request: WorkflowEventsRequest,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowEventsResponse>;
  artifact: (
    request: WorkflowArtifactRequest,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowArtifactPage>;
  pause: (
    runId: string,
    expectedRevision: Revision,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowMutationResponse>;
  resume: (
    runId: string,
    expectedRevision: Revision,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowMutationResponse>;
  cancel: (
    runId: string,
    expectedRevision: Revision,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowMutationResponse>;
  retryNode: (
    runId: string,
    nodeId: string,
    expectedRevision: Revision,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowMutationResponse>;
  replyInteraction: (
    runId: string,
    interactionId: string,
    requestRevision: Revision,
    expectedRevision: Revision,
    reply: WorkflowInteractionReply,
    options?: WorkflowRequestOptions,
  ) => Promise<WorkflowMutationResponse>;
}
