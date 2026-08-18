import type {
  Revision,
  WorkflowApi,
  WorkflowApi as WorkflowApiContract,
  WorkflowArtifact,
  WorkflowArtifactPage,
  WorkflowArtifactRequest,
  WorkflowCapabilities,
  WorkflowEventsRequest,
  WorkflowEventsResponse,
  WorkflowInteractionReply,
  WorkflowListRequest,
  WorkflowListResponse,
  WorkflowMutationResponse,
  WorkflowNode,
  WorkflowRequestOptions,
  WorkflowRun,
  WorkflowRunSnapshot,
  WorkflowRunSummary,
} from "./types";

export const WORKFLOW_API_COMMANDS = [
  "workflow_capabilities",
  "workflow_list",
  "workflow_get",
  "workflow_events",
  "workflow_artifact",
  "workflow_pause",
  "workflow_resume",
  "workflow_cancel",
  "workflow_retry_node",
  "workflow_reply_interaction",
] as const;

export type WorkflowApiCommand = (typeof WORKFLOW_API_COMMANDS)[number];

type WorkflowBridge = (
  command: WorkflowApiCommand,
  args: Record<string, unknown>,
) => Promise<unknown>;

type UnknownRecord = Record<string, unknown>;

const ENUM_VALUE_KEYS = new Set([
  "decision",
  "kind",
  "mode",
  "risk",
  "severity",
  "sideEffectState",
  "state",
  "status",
  "type",
]);

const DEFAULT_ACTIONS = {
  artifact: false,
  pause: false,
  resume: false,
  cancel: false,
  retryNode: false,
  replyInteraction: false,
};

export class WorkflowApiError extends Error {
  readonly command: WorkflowApiCommand;

  constructor(command: WorkflowApiCommand, message: string) {
    super(message);
    this.name = "WorkflowApiError";
    this.command = command;
  }
}

function isRecord(value: unknown): value is UnknownRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function asRecord(value: unknown): UnknownRecord {
  return isRecord(value) ? value : {};
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function asNumber(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function asBoolean(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function camelizeKey(value: string): string {
  return value.replace(/[_-]([a-zA-Z0-9])/g, (_, character: string) =>
    character.toUpperCase(),
  );
}

function lowerCamelEnum(value: string): string {
  if (!value) return value;
  if (value.includes("_") || value.includes("-") || value.includes(" ")) {
    const parts = value.split(/[_\-\s]+/).filter(Boolean);
    return parts
      .map((part, index) => {
        const lower = part.toLowerCase();
        return index === 0
          ? lower
          : `${lower.charAt(0).toUpperCase()}${lower.slice(1)}`;
      })
      .join("");
  }
  return `${value.charAt(0).toLowerCase()}${value.slice(1)}`;
}

function copyAlias(record: UnknownRecord, source: string, target: string): void {
  if (record[target] === undefined && record[source] !== undefined) {
    record[target] = record[source];
  }
}

function applyWireAliases(record: UnknownRecord): UnknownRecord {
  copyAlias(record, "workflowId", "runId");
  copyAlias(record, "workflow", "run");
  copyAlias(record, "runGeneration", "generation");
  copyAlias(record, "workflowSeq", "sequence");
  copyAlias(record, "latestSeq", "latestSequence");
  copyAlias(record, "latestWorkflowSeq", "latestSequence");
  copyAlias(record, "afterSeq", "afterSequence");
  copyAlias(record, "requestVersion", "requestRevision");
  copyAlias(record, "expectedVersion", "expectedRevision");
  copyAlias(record, "pendingInteractions", "interactions");
  copyAlias(record, "artifactManifests", "artifacts");
  copyAlias(record, "dependsOn", "dependencies");
  if (
    record.revision === undefined &&
    record.version !== undefined &&
    (record.runId !== undefined ||
      record.nodeId !== undefined ||
      record.attemptId !== undefined ||
      record.interactionId !== undefined ||
      record.approvalId !== undefined)
  ) {
    record.revision = record.version;
  }
  return record;
}

/** Normalizes Rust-style snake_case fields and enum spellings at the bridge edge. */
export function normalizeWorkflowWireValue(
  value: unknown,
  parentKey?: string,
): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => normalizeWorkflowWireValue(item, parentKey));
  }
  if (isRecord(value)) {
    const normalized: UnknownRecord = {};
    for (const [rawKey, rawValue] of Object.entries(value)) {
      const key = camelizeKey(rawKey);
      normalized[key] = normalizeWorkflowWireValue(rawValue, key);
    }
    return applyWireAliases(normalized);
  }
  if (typeof value === "string" && parentKey && ENUM_VALUE_KEYS.has(parentKey)) {
    return lowerCamelEnum(value);
  }
  return value;
}

function createAbortError(): DOMException {
  return new DOMException("工作流请求已取消", "AbortError");
}

function withAbort<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return promise;
  if (signal.aborted) return Promise.reject(createAbortError());
  return new Promise<T>((resolve, reject) => {
    const handleAbort = () => {
      cleanup();
      reject(createAbortError());
    };
    const cleanup = () => signal.removeEventListener("abort", handleAbort);
    signal.addEventListener("abort", handleAbort, { once: true });
    promise.then(
      (value) => {
        cleanup();
        resolve(value);
      },
      (error: unknown) => {
        cleanup();
        reject(error);
      },
    );
  });
}

function bridge(): WorkflowBridge {
  const invoke = window.__codeyInvokeApi as unknown as WorkflowBridge | undefined;
  if (typeof invoke !== "function") {
    throw new Error("Codey bridge 尚未连接，请退出 Codex 后重新启动 Codey");
  }
  return invoke;
}

async function invokeWorkflow(
  command: WorkflowApiCommand,
  args: Record<string, unknown>,
  options?: WorkflowRequestOptions,
): Promise<unknown> {
  const raw = await withAbort(bridge()(command, args), options?.signal);
  const rawRecord = asRecord(raw);
  const rawStatus = rawRecord.status;
  if (rawStatus === "failed" || rawStatus === "error") {
    throw new WorkflowApiError(
      command,
      asString(rawRecord.message, "Codey 工作流请求失败"),
    );
  }
  return normalizeWorkflowWireValue(raw);
}

function payloadRecord(value: unknown): UnknownRecord {
  const outer = asRecord(value);
  const nested = isRecord(outer.data)
    ? outer.data
    : isRecord(outer.result) && Object.keys(outer).every((key) =>
        ["message", "result", "status"].includes(key),
      )
      ? outer.result
      : undefined;
  return nested ? { ...outer, ...nested } : outer;
}

function normalizeRun(value: unknown): WorkflowRun {
  const run = asRecord(value);
  const runId = asString(run.runId);
  return {
    ...run,
    runId,
    originThreadId: asString(run.originThreadId) || undefined,
    generation: asNumber(run.generation),
    revision: asNumber(run.revision),
    state: asString(run.state ?? run.status, "created") as WorkflowRun["state"],
    title: asString(run.title ?? run.name, runId || "未命名工作流"),
    mode: asString(run.mode, "guarded") as WorkflowRun["mode"],
    risk: asString(run.risk, "medium") as WorkflowRun["risk"],
    createdAt: (run.createdAt ?? run.updatedAt ?? Date.now()) as WorkflowRun["createdAt"],
    updatedAt: (run.updatedAt ?? run.createdAt ?? Date.now()) as WorkflowRun["updatedAt"],
  } as WorkflowRun;
}

function normalizeNode(value: unknown, run: WorkflowRun): WorkflowNode {
  const node = asRecord(value);
  const attempts = asArray(node.attempts) as WorkflowNode["attempts"];
  return {
    ...node,
    nodeId: asString(node.nodeId ?? node.id),
    runId: asString(node.runId, run.runId),
    generation: asNumber(node.generation, run.generation),
    title: asString(node.title ?? node.name, asString(node.nodeId ?? node.id)),
    state: asString(node.state ?? node.status, "pending") as WorkflowNode["state"],
    dependencies: asArray(node.dependencies).map((dependency) => asString(dependency)),
    attemptCount: asNumber(node.attemptCount, attempts?.length ?? 0),
    attempts,
  } as WorkflowNode;
}

function normalizeArtifact(value: unknown, run?: WorkflowRun): WorkflowArtifact {
  const artifact = asRecord(value);
  const artifactId = asString(artifact.artifactId ?? artifact.id);
  return {
    ...artifact,
    artifactId,
    runId: asString(artifact.runId, run?.runId ?? ""),
    generation: asNumber(artifact.generation, run?.generation ?? 0),
    kind: asString(artifact.kind, "evidence"),
    title: asString(artifact.title ?? artifact.name, artifactId || "工作流证据"),
    mimeType: asString(artifact.mimeType, "application/octet-stream"),
    byteLength: asNumber(artifact.byteLength ?? artifact.totalBytes),
    createdAt: (artifact.createdAt ?? Date.now()) as WorkflowArtifact["createdAt"],
  } as WorkflowArtifact;
}

function normalizeSnapshot(value: unknown): WorkflowRunSnapshot {
  const outer = payloadRecord(value);
  const nested = isRecord(outer.snapshot) ? outer.snapshot : outer;
  const source = { ...outer, ...nested };
  const run = normalizeRun(source.run ?? source.workflow ?? source);
  const nodes = asArray(source.nodes).map((node) => normalizeNode(node, run));
  const artifacts = asArray(source.artifacts).map((artifact) =>
    normalizeArtifact(artifact, run),
  );
  return {
    ...source,
    engineEpoch: asString(source.engineEpoch ?? run.engineEpoch),
    generation: asNumber(source.generation, run.generation),
    revision: asNumber(source.revision, run.revision),
    sequence: asNumber(source.sequence ?? source.latestSequence ?? run.latestSequence),
    run,
    nodes,
    attempts: asArray(source.attempts) as WorkflowRunSnapshot["attempts"],
    events: Array.isArray(source.events)
      ? (source.events as WorkflowRunSnapshot["events"])
      : undefined,
    artifacts,
    approvals: asArray(source.approvals) as WorkflowRunSnapshot["approvals"],
    interactions: asArray(source.interactions) as WorkflowRunSnapshot["interactions"],
  } as WorkflowRunSnapshot;
}

function capabilityFlag(
  source: UnknownRecord,
  action: keyof WorkflowCapabilities["actions"],
  fallback: boolean,
): boolean {
  const commands = asRecord(source.commands);
  const actions = asRecord(source.actions ?? source.capabilities);
  const workflowName = `workflow${action.charAt(0).toUpperCase()}${action.slice(1)}`;
  for (const value of [actions[action], actions[workflowName], commands[action], commands[workflowName]]) {
    if (typeof value === "boolean") return value;
  }
  return fallback;
}

function normalizeCapabilities(value: unknown): WorkflowCapabilities {
  const source = payloadRecord(value);
  const enabled = asBoolean(source.enabled ?? source.workflowEnabled, true);
  const unavailableReason = asString(
    source.unavailableReason ?? source.reason,
  ) || undefined;
  const available = asBoolean(
    source.available,
    enabled && unavailableReason === undefined,
  );
  const readOnly = asBoolean(source.readOnly ?? source.safeMode, false);
  const mutable = enabled && available && !readOnly;
  return {
    ...source,
    enabled,
    available,
    readOnly,
    schemaVersion: asNumber(source.schemaVersion, 1),
    engineEpoch: asString(source.engineEpoch ?? source.epoch),
    unavailableReason,
    actions: {
      ...DEFAULT_ACTIONS,
      artifact: capabilityFlag(source, "artifact", enabled && available),
      pause: capabilityFlag(source, "pause", mutable),
      resume: capabilityFlag(source, "resume", mutable),
      cancel: capabilityFlag(source, "cancel", mutable),
      retryNode: capabilityFlag(source, "retryNode", mutable),
      replyInteraction: capabilityFlag(source, "replyInteraction", mutable),
    },
  } as WorkflowCapabilities;
}

function normalizeList(value: unknown): WorkflowListResponse {
  const source = payloadRecord(value);
  const rawRuns = source.runs ?? source.workflows ?? source.items;
  return {
    engineEpoch: asString(source.engineEpoch ?? source.epoch),
    runs: asArray(rawRuns).map((run) => normalizeRun(run) as WorkflowRunSummary),
    threadId: asString(source.threadId) || undefined,
    nextCursor: asString(source.nextCursor) || undefined,
  };
}

function normalizeEvents(
  value: unknown,
  request: WorkflowEventsRequest,
): WorkflowEventsResponse {
  const source = payloadRecord(value);
  const events = asArray(source.events ?? source.items) as WorkflowEventsResponse["events"];
  const latestEventSequence = events.reduce(
    (latest, event) => Math.max(latest, asNumber(event.sequence)),
    request.afterSequence,
  );
  return {
    ...source,
    runId: asString(source.runId, request.runId),
    engineEpoch: asString(source.engineEpoch ?? source.epoch),
    generation: asNumber(source.generation),
    revision: asNumber(source.revision),
    afterSequence: asNumber(source.afterSequence, request.afterSequence),
    latestSequence: asNumber(source.latestSequence, latestEventSequence),
    events,
    hasMore: asBoolean(source.hasMore, false),
    resetRequired: asBoolean(source.resetRequired, false),
  } as WorkflowEventsResponse;
}

function normalizeArtifactPage(
  value: unknown,
  request: WorkflowArtifactRequest,
): WorkflowArtifactPage {
  const source = payloadRecord(value);
  const artifact = normalizeArtifact(source.artifact ?? source.manifest ?? source);
  const text = typeof source.text === "string"
    ? source.text
    : typeof source.content === "string"
      ? source.content
      : undefined;
  const offset = asNumber(source.offset, request.offset ?? 0);
  const limit = asNumber(source.limit, request.limit ?? 16_384);
  const totalBytes = asNumber(source.totalBytes, artifact.byteLength);
  const nextOffset = typeof source.nextOffset === "number"
    ? source.nextOffset
    : undefined;
  return {
    ...source,
    artifact,
    offset,
    limit,
    totalBytes,
    mimeType: asString(source.mimeType, artifact.mimeType),
    text,
    content: text,
    truncated: asBoolean(
      source.truncated,
      nextOffset !== undefined || offset + limit < totalBytes,
    ),
    nextOffset,
  } as WorkflowArtifactPage;
}

function normalizeMutation(
  value: unknown,
  commandId: string,
  runId: string,
): WorkflowMutationResponse {
  const source = payloadRecord(value);
  const snapshot = isRecord(source.snapshot)
    ? normalizeSnapshot(source.snapshot)
    : undefined;
  return {
    ...source,
    accepted: asBoolean(source.accepted, source.status !== "failed"),
    commandId: asString(source.commandId, commandId),
    runId: asString(source.runId, runId),
    generation: asNumber(source.generation, snapshot?.generation ?? 0),
    revision: asNumber(source.revision, snapshot?.revision ?? 0),
    state: asString(
      source.state ?? snapshot?.run.state,
      "needsAttention",
    ) as WorkflowMutationResponse["state"],
    message: asString(source.message) || undefined,
    snapshot,
  } as WorkflowMutationResponse;
}

export function createWorkflowCommandId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `workflow-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

async function mutate(
  command: Extract<
    WorkflowApiCommand,
    | "workflow_pause"
    | "workflow_resume"
    | "workflow_cancel"
    | "workflow_retry_node"
    | "workflow_reply_interaction"
  >,
  runId: string,
  expectedRevision: Revision,
  args: Record<string, unknown>,
  options?: WorkflowRequestOptions,
): Promise<WorkflowMutationResponse> {
  const commandId = createWorkflowCommandId();
  const response = await invokeWorkflow(
    command,
    { ...args, runId, expectedRevision, commandId },
    options,
  );
  const normalized = normalizeMutation(response, commandId, runId);
  window.dispatchEvent(
    new CustomEvent("codey:workflow-changed", {
      detail: { runId: normalized.runId, state: normalized.state },
    }),
  );
  return normalized;
}

export const workflowApi: WorkflowApiContract = {
  async capabilities(options) {
    return normalizeCapabilities(
      await invokeWorkflow("workflow_capabilities", {}, options),
    );
  },

  async list(request: WorkflowListRequest = {}, options) {
    return normalizeList(
      await invokeWorkflow("workflow_list", { ...request }, options),
    );
  },

  async get(runId, options) {
    return normalizeSnapshot(
      await invokeWorkflow("workflow_get", { runId }, options),
    );
  },

  async events(request: WorkflowEventsRequest, options) {
    return normalizeEvents(
      await invokeWorkflow("workflow_events", { ...request }, options),
      request,
    );
  },

  async artifact(request: WorkflowArtifactRequest, options) {
    return normalizeArtifactPage(
      await invokeWorkflow("workflow_artifact", { ...request }, options),
      request,
    );
  },

  pause(runId, expectedRevision, options) {
    return mutate("workflow_pause", runId, expectedRevision, {}, options);
  },

  resume(runId, expectedRevision, options) {
    return mutate("workflow_resume", runId, expectedRevision, {}, options);
  },

  cancel(runId, expectedRevision, options) {
    return mutate("workflow_cancel", runId, expectedRevision, {}, options);
  },

  retryNode(runId, nodeId, expectedRevision, options) {
    return mutate(
      "workflow_retry_node",
      runId,
      expectedRevision,
      { nodeId },
      options,
    );
  },

  replyInteraction(
    runId,
    interactionId,
    requestRevision,
    expectedRevision,
    reply: WorkflowInteractionReply,
    options,
  ) {
    return mutate(
      "workflow_reply_interaction",
      runId,
      expectedRevision,
      { interactionId, requestRevision, reply },
      options,
    );
  },
};

export type { WorkflowApi };
