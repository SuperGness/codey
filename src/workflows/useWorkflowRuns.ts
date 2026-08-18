import { useCallback, useEffect, useRef, useState } from "react";

import { workflowApi } from "./api";
import {
  checkWorkflowEventBatch,
  compareWorkflowRunVersion,
  engineEpochChanged,
  mergeWorkflowEvents,
  reconcileWorkflowRuns,
  reconcileWorkflowSnapshot,
  replaceWorkflowEvents,
  workflowValuesEqual,
} from "./snapshot";
import type {
  EngineEpoch,
  Revision,
  WorkflowApi,
  WorkflowCapabilities,
  WorkflowEvent,
  WorkflowInteractionReply,
  WorkflowRunSnapshot,
  WorkflowRunState,
  WorkflowRunSummary,
} from "./types";

const DEFAULT_ACTIVE_POLL_MS = 1_000;
const DEFAULT_IDLE_POLL_MS = 7_000;
const EVENT_PAGE_SIZE = 200;
const RUN_PAGE_SIZE = 50;

export type WorkflowBusyAction =
  | "pause"
  | "resume"
  | "cancel"
  | `retry:${string}`
  | `reply:${string}`;

export interface WorkflowRunsState {
  capabilities: WorkflowCapabilities | null;
  engineEpoch: EngineEpoch | null;
  runs: WorkflowRunSummary[];
  nextCursor?: string;
  selectedRunId: string | null;
  snapshot: WorkflowRunSnapshot | null;
  events: WorkflowEvent[];
  lastSequence: number;
  eventsTruncated: boolean;
  eventBacklog: boolean;
  initialLoading: boolean;
  refreshing: boolean;
  error: string | null;
  mutationError: string | null;
  busyAction: WorkflowBusyAction | null;
  lastUpdatedAt: number | null;
  pollCompletedAt: number;
}

export interface UseWorkflowRunsOptions {
  active?: boolean;
  api?: WorkflowApi;
  initialRunId?: string;
  threadId?: string;
  activePollMs?: number;
  idlePollMs?: number;
}

export interface WorkflowRunsController extends WorkflowRunsState {
  documentVisible: boolean;
  selectRun: (runId: string) => void;
  refresh: () => Promise<void>;
  pause: () => Promise<void>;
  resume: () => Promise<void>;
  cancel: () => Promise<void>;
  retryNode: (nodeId: string) => Promise<void>;
  replyInteraction: (
    interactionId: string,
    requestRevision: Revision,
    reply: WorkflowInteractionReply,
  ) => Promise<void>;
  clearMutationError: () => void;
}

interface FullLoadResult {
  capabilities: WorkflowCapabilities;
  list: Awaited<ReturnType<WorkflowApi["list"]>>;
  selectedRunId: string | null;
  snapshot: WorkflowRunSnapshot | null;
  events: WorkflowEvent[];
  lastSequence: number;
  eventsTruncated: boolean;
  eventBacklog: boolean;
  engineEpoch: EngineEpoch;
}

interface IncrementalLoadResult {
  kind: "incremental";
  capabilities: WorkflowCapabilities;
  list: Awaited<ReturnType<WorkflowApi["list"]>>;
  snapshot: WorkflowRunSnapshot;
  eventsResponse: Awaited<ReturnType<WorkflowApi["events"]>>;
  freshEvents: WorkflowEvent[];
  lastSequence: number;
  eventBacklog: boolean;
  engineEpoch: EngineEpoch;
}

const EMPTY_INITIAL_STATE: Omit<WorkflowRunsState, "selectedRunId"> = {
  capabilities: null,
  engineEpoch: null,
  runs: [],
  snapshot: null,
  events: [],
  lastSequence: 0,
  eventsTruncated: false,
  eventBacklog: false,
  initialLoading: true,
  refreshing: false,
  error: null,
  mutationError: null,
  busyAction: null,
  lastUpdatedAt: null,
  pollCompletedAt: 0,
};

export function isActiveWorkflowRunState(state?: WorkflowRunState): boolean {
  return (
    state === "created" ||
    state === "queued" ||
    state === "running" ||
    state === "recovering" ||
    state === "pausing" ||
    state === "canceling"
  );
}

export function isWorkflowAbortError(error: unknown): boolean {
  return (
    error instanceof DOMException
      ? error.name === "AbortError"
      : error instanceof Error && error.name === "AbortError"
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "工作流请求失败，请稍后重试";
}

function firstDefinedEpoch(...epochs: Array<EngineEpoch | undefined>): EngineEpoch {
  return epochs.find((epoch) => Boolean(epoch)) ?? "";
}

function assertConsistentEpochs(...epochs: Array<EngineEpoch | undefined>): void {
  const present = new Set(epochs.filter((epoch): epoch is EngineEpoch => Boolean(epoch)));
  if (present.size > 1) {
    throw new Error("工作流引擎正在切换，已丢弃不一致的旧快照");
  }
}

function resolveSelectedRunId(
  state: WorkflowRunsState,
  runs: WorkflowRunSummary[],
): string | null {
  if (state.selectedRunId) {
    const stillKnown = runs.some((run) => run.runId === state.selectedRunId);
    if (stillKnown || state.snapshot?.run.runId === state.selectedRunId) {
      return state.selectedRunId;
    }
  }
  return runs[0]?.runId ?? null;
}

function lastEventSequence(events: WorkflowEvent[], fallback = 0): number {
  return events.reduce(
    (latest, event) => Math.max(latest, event.sequence),
    fallback,
  );
}

async function loadFull(
  api: WorkflowApi,
  state: WorkflowRunsState,
  threadId: string | undefined,
  signal: AbortSignal,
): Promise<FullLoadResult> {
  const [capabilities, list] = await Promise.all([
    api.capabilities({ signal }),
    api.list({ limit: RUN_PAGE_SIZE, threadId }, { signal }),
  ]);
  assertConsistentEpochs(capabilities.engineEpoch, list.engineEpoch);
  const selectedRunId = resolveSelectedRunId(state, list.runs);
  if (!selectedRunId) {
    return {
      capabilities,
      list,
      selectedRunId: null,
      snapshot: null,
      events: [],
      lastSequence: 0,
      eventsTruncated: false,
      eventBacklog: false,
      engineEpoch: firstDefinedEpoch(
        capabilities.engineEpoch,
        list.engineEpoch,
        state.engineEpoch ?? undefined,
      ),
    };
  }

  let snapshot: WorkflowRunSnapshot | null = null;
  let eventsResponse: Awaited<ReturnType<WorkflowApi["events"]>> | null = null;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    [snapshot, eventsResponse] = await Promise.all([
      api.get(selectedRunId, { signal }),
      api.events(
        { runId: selectedRunId, afterSequence: 0, limit: EVENT_PAGE_SIZE },
        { signal },
      ),
    ]);
    const responseRunId: string = eventsResponse.runId || selectedRunId;
    const generationMatches =
      eventsResponse.generation === 0 ||
      eventsResponse.generation === snapshot.generation;
    if (
      snapshot.run.runId === selectedRunId &&
      responseRunId === selectedRunId &&
      generationMatches
    ) {
      break;
    }
    snapshot = null;
    eventsResponse = null;
  }
  if (!snapshot || !eventsResponse) {
    throw new Error("运行快照在刷新期间已换代，请稍后重试");
  }

  assertConsistentEpochs(
    capabilities.engineEpoch,
    list.engineEpoch,
    snapshot.engineEpoch,
    eventsResponse.engineEpoch,
  );
  const sortedEvents = [...eventsResponse.events].sort(
    (left, right) => left.sequence - right.sequence,
  );
  const firstSequence = sortedEvents[0]?.sequence;
  const retained = replaceWorkflowEvents([], sortedEvents, false);
  const responseLatest = eventsResponse.latestSequence;
  const lastSequence = lastEventSequence(retained.events);
  const historyStartsLate = firstSequence !== undefined && firstSequence > 1;
  const omittedHistory = sortedEvents.length === 0 && responseLatest > 0;
  const eventBacklog = Boolean(
    eventsResponse.hasMore && responseLatest > lastSequence,
  );
  return {
    capabilities,
    list,
    selectedRunId,
    snapshot,
    events: retained.events,
    lastSequence: omittedHistory ? responseLatest : lastSequence,
    eventsTruncated:
      retained.truncated || historyStartsLate || omittedHistory,
    eventBacklog,
    engineEpoch: firstDefinedEpoch(
      capabilities.engineEpoch,
      list.engineEpoch,
      snapshot.engineEpoch,
      eventsResponse.engineEpoch,
      state.engineEpoch ?? undefined,
    ),
  };
}

async function loadIncremental(
  api: WorkflowApi,
  state: WorkflowRunsState,
  threadId: string | undefined,
  signal: AbortSignal,
): Promise<IncrementalLoadResult | null> {
  const runId = state.selectedRunId;
  const currentSnapshot = state.snapshot;
  if (!runId || !currentSnapshot) return null;

  const [capabilities, list, snapshot, eventsResponse] = await Promise.all([
    api.capabilities({ signal }),
    api.list({ limit: RUN_PAGE_SIZE, threadId }, { signal }),
    api.get(runId, { signal }),
    api.events(
      {
        runId,
        afterSequence: state.lastSequence,
        limit: EVENT_PAGE_SIZE,
      },
      { signal },
    ),
  ]);

  const summary = list.runs.find((run) => run.runId === runId);
  const epochChanged = engineEpochChanged(
    state.engineEpoch,
    capabilities.engineEpoch,
    list.engineEpoch,
    snapshot.engineEpoch,
    eventsResponse.engineEpoch,
  );
  const snapshotChangedGeneration =
    snapshot.run.runId !== runId ||
    snapshot.generation !== currentSnapshot.generation;
  const eventChangedGeneration =
    Boolean(eventsResponse.runId && eventsResponse.runId !== runId) ||
    (eventsResponse.generation !== 0 &&
      eventsResponse.generation !== currentSnapshot.generation);
  const summaryChangedGeneration = Boolean(
    summary && summary.generation > currentSnapshot.generation,
  );
  const batch = checkWorkflowEventBatch(
    eventsResponse.events,
    state.lastSequence,
  );
  const missingPayload =
    eventsResponse.latestSequence > batch.latestSequence &&
    eventsResponse.hasMore !== true;
  const wrongCursor =
    eventsResponse.afterSequence !== 0 &&
    eventsResponse.afterSequence !== state.lastSequence;

  if (
    epochChanged ||
    snapshotChangedGeneration ||
    eventChangedGeneration ||
    summaryChangedGeneration ||
    eventsResponse.resetRequired ||
    batch.hasGap ||
    missingPayload ||
    wrongCursor
  ) {
    return null;
  }

  assertConsistentEpochs(
    capabilities.engineEpoch,
    list.engineEpoch,
    snapshot.engineEpoch,
    eventsResponse.engineEpoch,
  );
  return {
    kind: "incremental",
    capabilities,
    list,
    snapshot,
    eventsResponse,
    freshEvents: batch.freshEvents,
    lastSequence: batch.latestSequence,
    eventBacklog: Boolean(
      eventsResponse.hasMore &&
      eventsResponse.latestSequence > batch.latestSequence,
    ),
    engineEpoch: firstDefinedEpoch(
      capabilities.engineEpoch,
      list.engineEpoch,
      snapshot.engineEpoch,
      eventsResponse.engineEpoch,
      state.engineEpoch ?? undefined,
    ),
  };
}

export function useWorkflowRuns({
  active = true,
  api = workflowApi,
  initialRunId,
  threadId,
  activePollMs = DEFAULT_ACTIVE_POLL_MS,
  idlePollMs = DEFAULT_IDLE_POLL_MS,
}: UseWorkflowRunsOptions = {}): WorkflowRunsController {
  const [state, setState] = useState<WorkflowRunsState>(() => ({
    ...EMPTY_INITIAL_STATE,
    selectedRunId: initialRunId ?? null,
  }));
  const [documentVisible, setDocumentVisible] = useState(
    () => typeof document === "undefined" || !document.hidden,
  );
  const stateRef = useRef(state);
  const apiRef = useRef(api);
  const activeRef = useRef(active);
  const visibleRef = useRef(documentVisible);
  const mountedRef = useRef(true);
  const requestIdRef = useRef(0);
  const pollAbortRef = useRef<AbortController | null>(null);
  const mutationAbortRef = useRef<AbortController | null>(null);
  const threadIdRef = useRef(threadId);
  const inFlightRef = useRef(false);
  apiRef.current = api;
  activeRef.current = active;
  visibleRef.current = documentVisible;

  const updateState = useCallback(
    (updater: (current: WorkflowRunsState) => WorkflowRunsState) => {
      setState((current) => {
        const next = updater(current);
        stateRef.current = next;
        return next;
      });
    },
    [],
  );

  const canCommit = useCallback(
    (requestId: number, signal: AbortSignal) =>
      mountedRef.current &&
      activeRef.current &&
      visibleRef.current &&
      !signal.aborted &&
      requestIdRef.current === requestId,
    [],
  );

  const poll = useCallback(
    async (forceFull: boolean): Promise<void> => {
      if (!activeRef.current || !visibleRef.current) return;
      if (inFlightRef.current) {
        if (!forceFull) return;
        pollAbortRef.current?.abort();
      }

      const requestId = requestIdRef.current + 1;
      requestIdRef.current = requestId;
      const controller = new AbortController();
      pollAbortRef.current = controller;
      inFlightRef.current = true;
      updateState((current) => ({ ...current, refreshing: true }));

      try {
        const requestState = stateRef.current;
        let incremental: IncrementalLoadResult | null = null;
        if (!forceFull) {
          incremental = await loadIncremental(
            apiRef.current,
            requestState,
            threadIdRef.current,
            controller.signal,
          );
        }
        if (!canCommit(requestId, controller.signal)) return;

        if (incremental) {
          updateState((current) => {
            if (current.selectedRunId !== incremental.snapshot.run.runId) {
              return current;
            }
            const epochChanged = engineEpochChanged(
              current.engineEpoch,
              incremental.engineEpoch,
            );
            const runs = reconcileWorkflowRuns(
              current.runs,
              incremental.list.runs,
              epochChanged,
            );
            const snapshot = reconcileWorkflowSnapshot(
              current.snapshot,
              incremental.snapshot,
              epochChanged,
            );
            const incomingOlder = Boolean(
              current.snapshot &&
                compareWorkflowRunVersion(
                  current.snapshot.run,
                  incremental.snapshot.run,
                ) < 0,
            );
            const merged = incomingOlder
              ? { events: current.events, truncated: false }
              : mergeWorkflowEvents(current.events, incremental.freshEvents);
            return {
              ...current,
              capabilities: workflowValuesEqual(
                current.capabilities,
                incremental.capabilities,
              )
                ? current.capabilities
                : incremental.capabilities,
              engineEpoch: incremental.engineEpoch || current.engineEpoch,
              runs,
              nextCursor: incremental.list.nextCursor,
              snapshot,
              events: merged.events,
              lastSequence: incomingOlder
                ? current.lastSequence
                : incremental.lastSequence,
              eventsTruncated: current.eventsTruncated || merged.truncated,
              eventBacklog: incomingOlder ? false : incremental.eventBacklog,
              initialLoading: false,
              refreshing: false,
              error: null,
              lastUpdatedAt: Date.now(),
              pollCompletedAt: Date.now(),
            };
          });
          return;
        }

        const full = await loadFull(
          apiRef.current,
          stateRef.current,
          threadIdRef.current,
          controller.signal,
        );
        if (!canCommit(requestId, controller.signal)) return;
        updateState((current) => {
          const epochChanged = engineEpochChanged(
            current.engineEpoch,
            full.engineEpoch,
          );
          const runs = reconcileWorkflowRuns(
            current.runs,
            full.list.runs,
            epochChanged,
          );
          const sameRun = Boolean(
            current.snapshot &&
              full.snapshot &&
              current.snapshot.run.runId === full.snapshot.run.runId,
          );
          const incomingOlder = Boolean(
            sameRun &&
              current.snapshot &&
              full.snapshot &&
              compareWorkflowRunVersion(current.snapshot.run, full.snapshot.run) < 0,
          );
          const snapshot = full.snapshot
            ? reconcileWorkflowSnapshot(
                current.snapshot,
                full.snapshot,
                epochChanged,
              )
            : null;
          const replaced = incomingOlder
            ? { events: current.events, truncated: false }
            : replaceWorkflowEvents(
                current.events,
                full.events,
                sameRun && !epochChanged,
              );
          return {
            ...current,
            capabilities: workflowValuesEqual(
              current.capabilities,
              full.capabilities,
            )
              ? current.capabilities
              : full.capabilities,
            engineEpoch: full.engineEpoch || current.engineEpoch,
            runs,
            nextCursor: full.list.nextCursor,
            selectedRunId: full.selectedRunId,
            snapshot,
            events: replaced.events,
            lastSequence: incomingOlder
              ? current.lastSequence
              : full.lastSequence,
            eventsTruncated:
              incomingOlder
                ? current.eventsTruncated
                : full.eventsTruncated || replaced.truncated,
            eventBacklog: incomingOlder ? false : full.eventBacklog,
            initialLoading: false,
            refreshing: false,
            error: null,
            lastUpdatedAt: Date.now(),
            pollCompletedAt: Date.now(),
          };
        });
      } catch (error) {
        if (!isWorkflowAbortError(error) && canCommit(requestId, controller.signal)) {
          updateState((current) => ({
            ...current,
            initialLoading: false,
            refreshing: false,
            error: errorMessage(error),
            pollCompletedAt: Date.now(),
          }));
        }
      } finally {
        if (requestIdRef.current === requestId) {
          inFlightRef.current = false;
          if (pollAbortRef.current === controller) pollAbortRef.current = null;
        }
      }
    },
    [canCommit, updateState],
  );

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestIdRef.current += 1;
      pollAbortRef.current?.abort();
      mutationAbortRef.current?.abort();
    };
  }, []);

  useEffect(() => {
    if (typeof document === "undefined") return;
    const handleVisibility = () => {
      const visible = !document.hidden;
      visibleRef.current = visible;
      setDocumentVisible(visible);
      if (!visible) {
        requestIdRef.current += 1;
        pollAbortRef.current?.abort();
        inFlightRef.current = false;
      } else {
        updateState((current) => ({ ...current, pollCompletedAt: 0 }));
      }
    };
    document.addEventListener("visibilitychange", handleVisibility);
    return () => document.removeEventListener("visibilitychange", handleVisibility);
  }, [updateState]);

  useEffect(() => {
    activeRef.current = active;
    if (!active) {
      requestIdRef.current += 1;
      pollAbortRef.current?.abort();
      mutationAbortRef.current?.abort();
      inFlightRef.current = false;
      updateState((current) => ({ ...current, refreshing: false }));
      return;
    }
    updateState((current) => ({ ...current, pollCompletedAt: 0 }));
  }, [active, updateState]);

  useEffect(() => {
    if (threadIdRef.current === threadId) return;
    threadIdRef.current = threadId;
    requestIdRef.current += 1;
    pollAbortRef.current?.abort();
    mutationAbortRef.current?.abort();
    inFlightRef.current = false;
    updateState((current) => ({
      ...EMPTY_INITIAL_STATE,
      selectedRunId: initialRunId ?? null,
      pollCompletedAt: 0,
      mutationError: current.mutationError,
    }));
  }, [initialRunId, threadId, updateState]);

  const selectedState =
    state.snapshot?.run.state ??
    state.runs.find((run) => run.runId === state.selectedRunId)?.state;

  useEffect(() => {
    if (!active || !documentVisible) return;
    const delay =
      state.pollCompletedAt === 0 || state.eventBacklog
        ? 0
        : isActiveWorkflowRunState(selectedState)
          ? activePollMs
          : idlePollMs;
    const timer = window.setTimeout(() => {
      void poll(false);
    }, Math.max(0, delay));
    return () => window.clearTimeout(timer);
  }, [
    active,
    activePollMs,
    documentVisible,
    idlePollMs,
    poll,
    selectedState,
    state.eventBacklog,
    state.pollCompletedAt,
    state.selectedRunId,
  ]);

  const selectRun = useCallback(
    (runId: string) => {
      if (!runId || stateRef.current.selectedRunId === runId) return;
      requestIdRef.current += 1;
      pollAbortRef.current?.abort();
      inFlightRef.current = false;
      updateState((current) => ({
        ...current,
        selectedRunId: runId,
        snapshot: null,
        events: [],
        lastSequence: 0,
        eventsTruncated: false,
        eventBacklog: false,
        error: null,
        pollCompletedAt: 0,
      }));
    },
    [updateState],
  );

  const refresh = useCallback(() => poll(true), [poll]);

  const runMutation = useCallback(
    async (
      busyAction: WorkflowBusyAction,
      operation: (
        api: WorkflowApi,
        snapshot: WorkflowRunSnapshot,
        signal: AbortSignal,
      ) => Promise<unknown>,
    ): Promise<void> => {
      const current = stateRef.current;
      if (!current.snapshot || current.busyAction) return;
      mutationAbortRef.current?.abort();
      const controller = new AbortController();
      mutationAbortRef.current = controller;
      updateState((value) => ({
        ...value,
        busyAction,
        mutationError: null,
      }));
      try {
        await operation(apiRef.current, current.snapshot, controller.signal);
        if (!mountedRef.current || controller.signal.aborted) return;
        await poll(true);
      } catch (error) {
        if (!isWorkflowAbortError(error) && mountedRef.current) {
          updateState((value) => ({
            ...value,
            mutationError: errorMessage(error),
          }));
        }
      } finally {
        if (mutationAbortRef.current === controller) {
          mutationAbortRef.current = null;
          if (mountedRef.current) {
            updateState((value) => ({ ...value, busyAction: null }));
          }
        }
      }
    },
    [poll, updateState],
  );

  const pause = useCallback(
    () =>
      runMutation("pause", (currentApi, snapshot, signal) =>
        currentApi.pause(snapshot.run.runId, snapshot.run.revision, { signal }),
      ),
    [runMutation],
  );

  const resume = useCallback(
    () =>
      runMutation("resume", (currentApi, snapshot, signal) =>
        currentApi.resume(snapshot.run.runId, snapshot.run.revision, { signal }),
      ),
    [runMutation],
  );

  const cancel = useCallback(
    () =>
      runMutation("cancel", (currentApi, snapshot, signal) =>
        currentApi.cancel(snapshot.run.runId, snapshot.run.revision, { signal }),
      ),
    [runMutation],
  );

  const retryNode = useCallback(
    (nodeId: string) =>
      runMutation(`retry:${nodeId}`, (currentApi, snapshot, signal) =>
        currentApi.retryNode(
          snapshot.run.runId,
          nodeId,
          snapshot.run.revision,
          { signal },
        ),
      ),
    [runMutation],
  );

  const replyInteraction = useCallback(
    (
      interactionId: string,
      requestRevision: Revision,
      reply: WorkflowInteractionReply,
    ) =>
      runMutation(`reply:${interactionId}`, (currentApi, snapshot, signal) =>
        currentApi.replyInteraction(
          snapshot.run.runId,
          interactionId,
          requestRevision,
          snapshot.run.revision,
          reply,
          { signal },
        ),
      ),
    [runMutation],
  );

  const clearMutationError = useCallback(() => {
    updateState((current) => ({ ...current, mutationError: null }));
  }, [updateState]);

  return {
    ...state,
    documentVisible,
    selectRun,
    refresh,
    pause,
    resume,
    cancel,
    retryNode,
    replyInteraction,
    clearMutationError,
  };
}
