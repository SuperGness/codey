import type { RuntimeStatus } from "./App.types";

export const STATUS_POLL_MAX_DURATION_MS = 5 * 60 * 1_000;
export const STATUS_POLL_MAX_CONSECUTIVE_ERRORS = 5;
export const INJECTION_PROBE_DELAYS_MS = [500, 1_000, 2_000, 5_000] as const;
export const INJECTION_PROBE_MAX_DURATION_MS = 60_000;
export const DIAGNOSTIC_PROBE_DELAYS_MS = [
  250, 500, 1_000, 2_000, 5_000,
] as const;

export type StatusPollTask = {
  deadline: number;
  delayIndex: number;
  delays: readonly number[];
  errors: number;
  kind: "injection" | "diagnostics" | "restart";
  nextAt: number;
  pending: (next: RuntimeStatus) => boolean;
  refreshesInjectionStatus: boolean;
  onExhausted?: () => void;
};

export type StatusPollScheduler = {
  add: (task: StatusPollTask) => void;
  clear: () => void;
  remove: (task: StatusPollTask) => void;
};

export type StatusPollClock = {
  now: () => number;
  setTimeout: (callback: () => void, delay: number) => number;
  clearTimeout: (timer: number) => void;
};

const browserStatusPollClock: StatusPollClock = {
  now: () => Date.now(),
  setTimeout: (callback, delay) => window.setTimeout(callback, delay),
  clearTimeout: (timer) => window.clearTimeout(timer),
};

export function createStatusPollScheduler(
  requestRuntimeStatus: (
    refreshesInjectionStatus: boolean,
  ) => Promise<RuntimeStatus>,
  clock: StatusPollClock = browserStatusPollClock,
): StatusPollScheduler {
  const tasks = new Map<StatusPollTask["kind"], StatusPollTask>();
  let timer = 0;
  let requestInProgress = false;

  const schedule = () => {
    if (requestInProgress) return;
    clock.clearTimeout(timer);
    timer = 0;
    if (tasks.size === 0) return;
    const nextAt = Math.min(...[...tasks.values()].map((task) => task.nextAt));
    timer = clock.setTimeout(() => {
      timer = 0;
      void poll();
    }, Math.max(0, nextAt - clock.now()));
  };

  const poll = async () => {
    if (requestInProgress || tasks.size === 0) return;
    const requestStartedAt = clock.now();
    const dueTasks = [...tasks.values()].filter(
      (task) => task.nextAt <= requestStartedAt,
    );
    if (dueTasks.length === 0) {
      schedule();
      return;
    }

    requestInProgress = true;
    try {
      const next = await requestRuntimeStatus(
        dueTasks.some((task) => task.refreshesInjectionStatus),
      );
      const completedAt = clock.now();
      for (const task of dueTasks) {
        if (tasks.get(task.kind) !== task) continue;
        task.errors = 0;
        task.delayIndex = Math.min(
          task.delayIndex + 1,
          task.delays.length - 1,
        );
        const pending = task.pending(next);
        if (completedAt >= task.deadline || !pending) {
          tasks.delete(task.kind);
          if (pending) task.onExhausted?.();
          continue;
        }
        task.nextAt =
          completedAt +
          (task.kind === "restart" ? 500 : task.delays[task.delayIndex]);
      }
    } catch {
      const failedAt = clock.now();
      for (const task of dueTasks) {
        if (tasks.get(task.kind) !== task) continue;
        task.errors += 1;
        task.delayIndex = Math.min(
          task.delayIndex + 1,
          task.delays.length - 1,
        );
        if (
          failedAt >= task.deadline ||
          task.errors >= STATUS_POLL_MAX_CONSECUTIVE_ERRORS
        ) {
          tasks.delete(task.kind);
          task.onExhausted?.();
          continue;
        }
        task.nextAt =
          failedAt +
          (task.kind === "restart"
            ? Math.min(500 * 2 ** task.errors, 5_000)
            : task.delays[task.delayIndex]);
      }
    } finally {
      requestInProgress = false;
      schedule();
    }
  };

  return {
    add(task) {
      tasks.set(task.kind, task);
      schedule();
    },
    clear() {
      tasks.clear();
      clock.clearTimeout(timer);
      timer = 0;
    },
    remove(task) {
      if (tasks.get(task.kind) === task) {
        tasks.delete(task.kind);
        schedule();
      }
    },
  };
}

export function createStatusPollTask(
  task: Omit<StatusPollTask, "deadline" | "delayIndex" | "errors" | "nextAt">,
  duration: number,
  now = Date.now(),
): StatusPollTask {
  return {
    ...task,
    deadline: now + duration,
    delayIndex: 0,
    errors: 0,
    nextAt: now + task.delays[0],
  };
}
