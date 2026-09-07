import { useCallback, useEffect, useRef, useState } from "react";

import { invoke } from "./api";
import { withTimeout } from "./appUtils";
import type { RuntimeStatus } from "./App.types";
import {
  createStatusPollScheduler,
  createStatusPollTask,
  DIAGNOSTIC_PROBE_DELAYS_MS,
  INJECTION_PROBE_DELAYS_MS,
  INJECTION_PROBE_MAX_DURATION_MS,
  STATUS_POLL_MAX_DURATION_MS,
  type StatusPollScheduler,
} from "./runtimeStatusPollScheduler";
import { reconcileRuntimeStatus } from "./runtimeStatusSnapshot";

const INJECTION_STATUS_CHANGED_EVENT = "codey-injection-status-changed";
export const SETTINGS_OPENED_EVENT = "codey-settings-opened";

type UseRuntimeStatusOptions = {
  active: boolean;
  embedded: boolean;
};

type RuntimeStatusFlight = {
  refreshesInjectionStatus: boolean;
  promise: Promise<RuntimeStatus>;
};

export function useRuntimeStatus({
  active,
  embedded,
}: UseRuntimeStatusOptions) {
  const [status, setStatus] = useState<RuntimeStatus>({ running: false });
  const [restartStatusError, setRestartStatusError] = useState<string | null>(null);
  const runtimeStatusFlightRef = useRef<RuntimeStatusFlight | null>(null);
  const statusPollSchedulerRef = useRef<StatusPollScheduler | null>(null);
  const settingsOpenRefreshRequestedRef = useRef(false);
  const mountedRef = useRef(true);
  const requestGenerationRef = useRef(0);
  const activeRef = useRef(active);
  activeRef.current = active;

  const requestRuntimeStatus = useCallback(
    (shouldRefreshInjectionStatus: boolean): Promise<RuntimeStatus> => {
      const requestCanCommit = (requestGeneration: number) =>
        mountedRef.current &&
        activeRef.current &&
        requestGenerationRef.current === requestGeneration;
      const startRequest = (
        refreshesInjectionStatus: boolean,
        requestGeneration = requestGenerationRef.current,
      ) => {
        return withTimeout(invoke<RuntimeStatus>("runtime_status", {
          refreshInjectionStatus: refreshesInjectionStatus,
        }), 10_000, "运行状态查询超时，请重新查询").then((next) => {
          if (requestCanCommit(requestGeneration)) {
            setStatus((current) => reconcileRuntimeStatus(current, next));
            setRestartStatusError(null);
          }
          return next;
        });
      };

      const currentFlight = runtimeStatusFlightRef.current;
      if (currentFlight) {
        if (
          !shouldRefreshInjectionStatus ||
          currentFlight.refreshesInjectionStatus
        ) {
          return currentFlight.promise;
        }

        const queuedGeneration = requestGenerationRef.current;
        const startQueuedRefresh = () =>
          requestCanCommit(queuedGeneration)
            ? startRequest(true, queuedGeneration)
            : null;
        const queuedFlight: RuntimeStatusFlight = {
          refreshesInjectionStatus: true,
          promise: currentFlight.promise.then(
            (next) => startQueuedRefresh() ?? next,
            (error) => startQueuedRefresh() ?? Promise.reject(error),
          ),
        };
        runtimeStatusFlightRef.current = queuedFlight;
        const clearQueuedFlight = () => {
          if (runtimeStatusFlightRef.current === queuedFlight) {
            runtimeStatusFlightRef.current = null;
          }
        };
        void queuedFlight.promise.then(clearQueuedFlight, clearQueuedFlight);
        return queuedFlight.promise;
      }

      const flight: RuntimeStatusFlight = {
        refreshesInjectionStatus: shouldRefreshInjectionStatus,
        promise: startRequest(shouldRefreshInjectionStatus),
      };
      runtimeStatusFlightRef.current = flight;
      const clearFlight = () => {
        if (runtimeStatusFlightRef.current === flight) {
          runtimeStatusFlightRef.current = null;
        }
      };
      void flight.promise.then(clearFlight, clearFlight);
      return flight.promise;
    },
    [],
  );

  const refreshStatus = useCallback(
    () => requestRuntimeStatus(false),
    [requestRuntimeStatus],
  );

  const refreshInjectionStatus = useCallback(
    () => requestRuntimeStatus(true),
    [requestRuntimeStatus],
  );

  if (statusPollSchedulerRef.current === null) {
    statusPollSchedulerRef.current = createStatusPollScheduler(
      requestRuntimeStatus,
    );
  }
  const statusPollScheduler = statusPollSchedulerRef.current;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestGenerationRef.current += 1;
      runtimeStatusFlightRef.current = null;
      statusPollScheduler.clear();
    };
  }, [statusPollScheduler]);

  useEffect(() => {
    if (active) return;
    // Ignore requests started for a panel that is no longer visible and make
    // the next activation start a fresh single-flight request.
    requestGenerationRef.current += 1;
    runtimeStatusFlightRef.current = null;
    statusPollScheduler.clear();
  }, [active, statusPollScheduler]);

  const refreshStatusForLoad = useCallback(() => {
    const shouldRefreshInjectionStatus =
      !embedded || !settingsOpenRefreshRequestedRef.current;
    return shouldRefreshInjectionStatus
      ? refreshInjectionStatus()
      : refreshStatus();
  }, [embedded, refreshInjectionStatus, refreshStatus]);

  useEffect(() => {
    const handleInjectionStatusChanged = () => {
      if (!activeRef.current) return;
      void refreshInjectionStatus().catch(() => {});
    };
    window.addEventListener(
      INJECTION_STATUS_CHANGED_EVENT,
      handleInjectionStatusChanged,
    );
    return () => {
      window.removeEventListener(
        INJECTION_STATUS_CHANGED_EVENT,
        handleInjectionStatusChanged,
      );
    };
  }, [refreshInjectionStatus]);

  useEffect(() => {
    const handleSettingsOpened = () => {
      settingsOpenRefreshRequestedRef.current = true;
      void refreshInjectionStatus().catch(() => {});
    };
    window.addEventListener(SETTINGS_OPENED_EVENT, handleSettingsOpened);
    return () => {
      window.removeEventListener(SETTINGS_OPENED_EVENT, handleSettingsOpened);
    };
  }, [refreshInjectionStatus]);

  const builtinInjectionProbePending =
    status.injectionScripts?.some(
      (script) =>
        script.source === "builtin" && script.status === "executed",
    ) ?? false;

  useEffect(() => {
    if (!active || !builtinInjectionProbePending) return;
    const task = createStatusPollTask(
      {
        kind: "injection",
        delays: INJECTION_PROBE_DELAYS_MS,
        pending: (next) =>
          next.injectionScripts?.some(
            (script) =>
              script.source === "builtin" && script.status === "executed",
          ) ?? false,
        refreshesInjectionStatus: true,
      },
      INJECTION_PROBE_MAX_DURATION_MS,
    );
    statusPollScheduler.add(task);
    return () => statusPollScheduler.remove(task);
  }, [active, builtinInjectionProbePending, statusPollScheduler]);

  useEffect(() => {
    if (
      !active ||
      (!status.traceLogStats?.pending &&
        !status.crashpadPendingStats?.pending)
    )
      return;
    const task = createStatusPollTask(
      {
        kind: "diagnostics",
        delays: DIAGNOSTIC_PROBE_DELAYS_MS,
        pending: (next) =>
          Boolean(
            next.traceLogStats?.pending ||
              next.crashpadPendingStats?.pending,
          ),
        refreshesInjectionStatus: false,
      },
      STATUS_POLL_MAX_DURATION_MS,
    );
    statusPollScheduler.add(task);
    return () => statusPollScheduler.remove(task);
  }, [
    active,
    status.crashpadPendingStats?.pending,
    status.traceLogStats?.pending,
    statusPollScheduler,
  ]);

  useEffect(() => {
    if (!active || !status.restartInProgress || restartStatusError) return;
    const task = createStatusPollTask(
      {
        kind: "restart",
        delays: [500],
        pending: (next) => Boolean(next.restartInProgress),
        refreshesInjectionStatus: false,
        onExhausted: () => setRestartStatusError(
          "暂时无法确认 Codex 是否重启完成，请点击重新查询状态",
        ),
      },
      STATUS_POLL_MAX_DURATION_MS,
    );
    statusPollScheduler.add(task);
    return () => statusPollScheduler.remove(task);
  }, [active, status.restartInProgress, restartStatusError, statusPollScheduler]);

  return {
    status,
    setStatus,
    refreshStatus,
    refreshStatusForLoad,
    restartStatusError,
    setRestartStatusError,
  };
}
