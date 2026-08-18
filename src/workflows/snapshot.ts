import type {
  EngineEpoch,
  WorkflowEvent,
  WorkflowRunSnapshot,
  WorkflowRunSummary,
} from "./types";

const MAX_RETAINED_EVENTS = 500;

export function workflowValuesEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => workflowValuesEqual(value, right[index]))
    );
  }
  if (
    left === null ||
    right === null ||
    typeof left !== "object" ||
    typeof right !== "object"
  ) {
    return false;
  }
  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  const leftKeys = Object.keys(leftRecord);
  const rightKeys = Object.keys(rightRecord);
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every(
      (key) =>
        Object.prototype.hasOwnProperty.call(rightRecord, key) &&
        workflowValuesEqual(leftRecord[key], rightRecord[key]),
    )
  );
}

function sameReferences<Value>(left: Value[], right: Value[]): boolean {
  return (
    left.length === right.length &&
    left.every((value, index) => value === right[index])
  );
}

function reconcileArrayByKey<Value>(
  current: Value[],
  next: Value[],
  keyOf: (value: Value) => string,
): Value[] {
  const currentByKey = new Map(current.map((value) => [keyOf(value), value]));
  const reconciled = next.map((value) => {
    const previous = currentByKey.get(keyOf(value));
    return previous && workflowValuesEqual(previous, value) ? previous : value;
  });
  return sameReferences(current, reconciled) ? current : reconciled;
}

export function compareWorkflowRunVersion(
  current: Pick<WorkflowRunSummary, "runId" | "generation" | "revision">,
  next: Pick<WorkflowRunSummary, "runId" | "generation" | "revision">,
): number {
  if (current.runId !== next.runId) return 0;
  if (current.generation !== next.generation) {
    return next.generation > current.generation ? 1 : -1;
  }
  if (current.revision !== next.revision) {
    return next.revision > current.revision ? 1 : -1;
  }
  return 0;
}

export function reconcileWorkflowRuns(
  current: WorkflowRunSummary[],
  next: WorkflowRunSummary[],
  epochChanged = false,
): WorkflowRunSummary[] {
  if (epochChanged) return next;
  const currentById = new Map(current.map((run) => [run.runId, run]));
  const reconciled = next.map((run) => {
    const previous = currentById.get(run.runId);
    if (!previous) return run;
    const versionComparison = compareWorkflowRunVersion(previous, run);
    if (versionComparison < 0) return previous;
    return workflowValuesEqual(previous, run) ? previous : run;
  });
  return sameReferences(current, reconciled) ? current : reconciled;
}

export function reconcileWorkflowSnapshot(
  current: WorkflowRunSnapshot | null,
  next: WorkflowRunSnapshot,
  epochChanged = false,
): WorkflowRunSnapshot {
  if (!current || epochChanged || current.run.runId !== next.run.runId) return next;
  const versionComparison = compareWorkflowRunVersion(current.run, next.run);
  if (versionComparison < 0) return current;
  if (workflowValuesEqual(current, next)) return current;

  const run = workflowValuesEqual(current.run, next.run) ? current.run : next.run;
  const nodes = reconcileArrayByKey(
    current.nodes,
    next.nodes,
    (node) => node.nodeId,
  );
  const attempts = reconcileArrayByKey(
    current.attempts,
    next.attempts,
    (attempt) => attempt.attemptId,
  );
  const artifacts = reconcileArrayByKey(
    current.artifacts,
    next.artifacts,
    (artifact) => artifact.artifactId,
  );
  const approvals = reconcileArrayByKey(
    current.approvals,
    next.approvals,
    (approval) => approval.approvalId,
  );
  const interactions = reconcileArrayByKey(
    current.interactions,
    next.interactions,
    (interaction) => interaction.interactionId,
  );
  const events = next.events
    ? reconcileArrayByKey(
        current.events ?? [],
        next.events,
        (event) => event.eventId || String(event.sequence),
      )
    : current.events;
  const acceptanceGate = workflowValuesEqual(
    current.acceptanceGate,
    next.acceptanceGate,
  )
    ? current.acceptanceGate
    : next.acceptanceGate;

  return {
    ...next,
    run,
    nodes,
    attempts,
    artifacts,
    approvals,
    interactions,
    events,
    acceptanceGate,
  };
}

export interface WorkflowEventBatchCheck {
  freshEvents: WorkflowEvent[];
  hasGap: boolean;
  latestSequence: number;
}

export function checkWorkflowEventBatch(
  events: WorkflowEvent[],
  afterSequence: number,
): WorkflowEventBatchCheck {
  const bySequence = new Map<number, WorkflowEvent>();
  for (const event of events) {
    if (event.sequence > afterSequence && !bySequence.has(event.sequence)) {
      bySequence.set(event.sequence, event);
    }
  }
  const freshEvents = [...bySequence.values()].sort(
    (left, right) => left.sequence - right.sequence,
  );
  let expected = afterSequence + 1;
  let hasGap = false;
  for (const event of freshEvents) {
    if (event.sequence !== expected) {
      hasGap = true;
      break;
    }
    expected += 1;
  }
  return {
    freshEvents,
    hasGap,
    latestSequence:
      freshEvents.length > 0
        ? freshEvents[freshEvents.length - 1].sequence
        : afterSequence,
  };
}

export function mergeWorkflowEvents(
  current: WorkflowEvent[],
  incoming: WorkflowEvent[],
): { events: WorkflowEvent[]; truncated: boolean } {
  if (incoming.length === 0) return { events: current, truncated: false };
  const currentBySequence = new Map(current.map((event) => [event.sequence, event]));
  for (const event of incoming) {
    const previous = currentBySequence.get(event.sequence);
    if (!previous || !workflowValuesEqual(previous, event)) {
      currentBySequence.set(event.sequence, event);
    }
  }
  const all = [...currentBySequence.values()].sort(
    (left, right) => left.sequence - right.sequence,
  );
  const truncated = all.length > MAX_RETAINED_EVENTS;
  const limited = truncated ? all.slice(-MAX_RETAINED_EVENTS) : all;
  const reconciled = reconcileArrayByKey(
    current,
    limited,
    (event) => event.eventId || String(event.sequence),
  );
  return { events: reconciled, truncated };
}

export function replaceWorkflowEvents(
  current: WorkflowEvent[],
  next: WorkflowEvent[],
  canReuse: boolean,
): { events: WorkflowEvent[]; truncated: boolean } {
  const sorted = [...next].sort((left, right) => left.sequence - right.sequence);
  const truncated = sorted.length > MAX_RETAINED_EVENTS;
  const limited = truncated ? sorted.slice(-MAX_RETAINED_EVENTS) : sorted;
  return {
    events: canReuse
      ? reconcileArrayByKey(
          current,
          limited,
          (event) => event.eventId || String(event.sequence),
        )
      : limited,
    truncated,
  };
}

export function engineEpochChanged(
  current: EngineEpoch | null,
  ...nextEpochs: Array<EngineEpoch | undefined>
): boolean {
  if (!current) return false;
  return nextEpochs.some((epoch) => Boolean(epoch) && epoch !== current);
}
