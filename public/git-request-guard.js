(() => {
  "use strict";

  const guardKey = "__codeyGitRequestGuard";
  const scriptId = "git-request-guard";
  const version = 3;
  const mainProcessStatusRequestType = "codey-git-request-guard-status";
  const mainProcessStatusResponseType =
    "codey-git-request-guard-status-response";
  const mainProcessProbeTimeoutMs = 1_000;
  const targetMethods = new Set([
    "git-origins",
    "status-summary",
    "review-summary",
    "branch-diff-stats",
  ]);
  const tokenCapacity = 3;
  const tokenRefillMs = 1_000;
  const perKeyIntervalMs = 2_000;
  const maximumQueueSize = 48;
  const maximumPerKeyQueueSize = 6;
  const maximumQueueWaitMs = 15_000;
  const maximumFailureBackoffMs = 15_000;

  const existing = window[guardKey];
  if (existing && typeof existing.ensureInstalled === "function") {
    if (typeof existing.requestInstall === "function") {
      existing.requestInstall();
    } else {
      existing.ensureInstalled();
    }
    return;
  }

  const platformText = [
    window.navigator?.userAgentData?.platform,
    window.navigator?.platform,
    window.navigator?.userAgent,
  ]
    .filter((value) => typeof value === "string")
    .join(" ");
  const enabled = /\bwin(?:32|64|dows)?\b/i.test(platformText);
  const queue = [];
  const queuedByRequestId = new Map();
  const sentKeyByRequestId = new Map();
  const lastSentAtByKey = new Map();
  const failureCountByKey = new Map();
  const cooldownUntilByKey = new Map();
  const counters = {
    matched: 0,
    sent: 0,
    queued: 0,
    cancelledBeforeSend: 0,
    rejected: 0,
    transportFailures: 0,
    observedFailures: 0,
  };

  let availableTokens = tokenCapacity;
  let tokenUpdatedAt = Date.now();
  let drainTimer = 0;
  let drainTimerAt = Number.POSITIVE_INFINITY;
  let bridgePatched = false;
  let responseObserverPatched = false;
  let observedGitSubscriptions = 0;
  let lastMethod = "";
  let bridgeRetryTimer = 0;
  let bridgeRetryDelay = 50;
  let bridgeRetryDeadline = Date.now() + 30_000;
  // 慢速重试（30s 周期）封顶：桥长期缺席时不再无限期探测。
  // requestInstall 会重置计数并重新打开 30s 快速窗口。
  const MAX_SLOW_BRIDGE_RETRIES = 20;
  let bridgeSlowRetries = 0;
  let mainProcessProtected = false;
  let mainProcessProbePending = null;
  let mainProcessProbeAttempts = 0;
  let mainProcessProbeError = "";
  let mainProcessSnapshot = null;
  let mainProcessProbeTransport = "";

  const now = () => {
    const value = Date.now();
    return Number.isFinite(value) ? value : 0;
  };

  const hashText = (value) => {
    let hash = 0x811c9dc5;
    for (let index = 0; index < value.length; index += 1) {
      hash ^= value.charCodeAt(index);
      hash = Math.imul(hash, 0x01000193);
    }
    return (hash >>> 0).toString(16).padStart(8, "0");
  };

  const stringPart = (value) =>
    typeof value === "string" ? value.slice(0, 2_048) : "";

  const requestInfo = (workerId, message) => {
    if (
      workerId !== "git" ||
      !message ||
      typeof message !== "object" ||
      message.type !== "worker-request" ||
      (message.workerId != null && message.workerId !== "git")
    ) {
      return null;
    }
    const request = message.request;
    if (!request || typeof request !== "object" || typeof request.method !== "string") {
      return null;
    }

    const workerMethod = request.method;
    const outerParams =
      request.params && typeof request.params === "object" ? request.params : {};
    const query =
      workerMethod === "subscribe-live-query" &&
      outerParams.query &&
      typeof outerParams.query === "object"
        ? outerParams.query
        : null;
    const method = query && typeof query.method === "string" ? query.method : workerMethod;
    // Unknown subscriptions may not be Git reads; never delay them by shape alone.
    if (!targetMethods.has(method)) return null;

    const params =
      query?.params && typeof query.params === "object" ? query.params : outerParams;
    const operationSource = stringPart(
      params.operationSource ?? outerParams.operationSource,
    );
    const repositoryScope =
      method === "git-origins"
        ? "all-origins"
        : stringPart(
            params.cwd ??
              params.root ??
              params.commonDir ??
              outerParams.cwd ??
              outerParams.root ??
              outerParams.commonDir,
          );
    const keyMaterial = [
      workerMethod,
      method,
      operationSource,
      repositoryScope,
      stringPart(params.baseBranch),
      params.includeUntrackedFiles === true ? "untracked" : "",
      params.hideWhitespace === true ? "hide-whitespace" : "",
    ].join("\0");

    return {
      id: request.id,
      key: `${method}:${hashText(keyMaterial)}`,
      method,
    };
  };

  const refillTokens = (at) => {
    if (at < tokenUpdatedAt) {
      availableTokens = tokenCapacity;
      tokenUpdatedAt = at;
      return;
    }
    const elapsed = at - tokenUpdatedAt;
    if (elapsed <= 0) return;
    availableTokens = Math.min(
      tokenCapacity,
      availableTokens + elapsed / tokenRefillMs,
    );
    tokenUpdatedAt = at;
  };

  const nextEligibleAt = (info, at) => {
    refillTokens(at);
    const tokenReadyAt =
      availableTokens >= 1
        ? at
        : at + Math.ceil((1 - availableTokens) * tokenRefillMs);
    const keyReadyAt = Math.max(
      at,
      (lastSentAtByKey.get(info.key) ?? Number.NEGATIVE_INFINITY) +
        perKeyIntervalMs,
    );
    const cooldownReadyAt = Math.max(at, cooldownUntilByKey.get(info.key) ?? 0);
    return Math.max(tokenReadyAt, keyReadyAt, cooldownReadyAt);
  };

  const markEffective = () => {
    const entry = window.__codeyInjectionStatus?.[scriptId];
    if (!entry) return;
    const detail = enabled
      ? mainProcessProtected
        ? "Windows Git 请求限流已由主进程接管"
        : "Windows Git 请求限流已由 Renderer 接管"
      : "Git 请求保护已就绪，当前平台无需启用";
    const changed =
      entry.status !== "effective" ||
      entry.detail !== detail;
    entry.status = "effective";
    entry.detail = detail;
    entry.error = null;
    if (changed && typeof window.dispatchEvent === "function") {
      window.dispatchEvent(
        new CustomEvent("codey-injection-status-changed", {
          detail: { id: scriptId, status: "effective" },
        }),
      );
    }
  };

  const makeGuardError = (reason, info) => {
    const error = new Error(`Codey Git request guard: ${reason}`);
    error.name = "CodeyGitRequestGuardError";
    error.code = "CODEY_GIT_REQUEST_THROTTLED";
    error.method = info?.method ?? "";
    return error;
  };

  const recordFailure = (info, at, observed) => {
    const failureCount = Math.min((failureCountByKey.get(info.key) ?? 0) + 1, 5);
    failureCountByKey.set(info.key, failureCount);
    const backoff = Math.min(
      1_000 * 2 ** (failureCount - 1),
      maximumFailureBackoffMs,
    );
    cooldownUntilByKey.set(info.key, at + backoff);
    if (observed) counters.observedFailures += 1;
    else counters.transportFailures += 1;
  };

  const responseFailed = (message) => {
    const result = message?.response?.result;
    const value = result?.value;
    return (
      result?.type === "error" ||
      value?.type === "error" ||
      value?.status === "error" ||
      value?.status === "command-error" ||
      value?.success === false
    );
  };

  const observeWorkerMessage = (workerId, message) => {
    if (workerId !== "git" || message?.type !== "worker-response") return;
    const requestId = message?.response?.id;
    const info = sentKeyByRequestId.get(requestId);
    if (!info) return;
    sentKeyByRequestId.delete(requestId);
    if (responseFailed(message)) {
      recordFailure(info, now(), true);
    } else {
      failureCountByKey.delete(info.key);
      cooldownUntilByKey.delete(info.key);
    }
    scheduleDrain();
  };

  const replaceBridgeMethod = (bridge, method, wrapped) => {
    try {
      bridge[method] = wrapped;
    } catch {}
    if (bridge?.[method] === wrapped) return true;
    try {
      Object.defineProperty(bridge, method, {
        configurable: true,
        value: wrapped,
        writable: true,
      });
    } catch {}
    return bridge?.[method] === wrapped;
  };

  const removeQueuedEntry = (entry) => {
    const index = queue.indexOf(entry);
    if (index >= 0) queue.splice(index, 1);
    if (entry.info.id !== undefined) queuedByRequestId.delete(entry.info.id);
  };

  const rejectEntry = (entry, reason) => {
    removeQueuedEntry(entry);
    counters.rejected += 1;
    entry.reject(makeGuardError(reason, entry.info));
  };

  const dispatch = (entry, at) => {
    refillTokens(at);
    availableTokens = Math.max(0, availableTokens - 1);
    lastSentAtByKey.set(entry.info.key, at);
    if (observedGitSubscriptions > 0 && entry.info.id !== undefined) {
      sentKeyByRequestId.set(entry.info.id, entry.info);
    }
    lastMethod = entry.info.method;
    counters.sent += 1;

    let result;
    try {
      result = Reflect.apply(entry.original, entry.thisValue, entry.args);
    } catch (error) {
      recordFailure(entry.info, at, false);
      entry.reject(error);
      scheduleDrain();
      return;
    }
    Promise.resolve(result).then(
      (value) => {
        entry.resolve(value);
        scheduleDrain();
      },
      (error) => {
        recordFailure(entry.info, now(), false);
        entry.reject(error);
        scheduleDrain();
      },
    );
  };

  const scheduleDrain = () => {
    if (!enabled || queue.length === 0 || typeof window.setTimeout !== "function") {
      return;
    }
    const at = now();
    let earliest = Number.POSITIVE_INFINITY;
    for (const entry of queue) {
      earliest = Math.min(
        earliest,
        nextEligibleAt(entry.info, at),
        entry.enqueuedAt + maximumQueueWaitMs,
      );
    }
    if (!Number.isFinite(earliest)) return;
    if (drainTimer && drainTimerAt <= earliest) return;
    if (drainTimer) window.clearTimeout(drainTimer);
    drainTimerAt = earliest;
    drainTimer = window.setTimeout(drain, Math.max(0, earliest - at));
  };

  const drain = () => {
    drainTimer = 0;
    drainTimerAt = Number.POSITIVE_INFINITY;
    let at = now();

    for (const entry of [...queue]) {
      if (at - entry.enqueuedAt >= maximumQueueWaitMs) {
        rejectEntry(entry, "queue timeout");
      }
    }

    while (queue.length > 0) {
      at = now();
      let selected = null;
      for (const entry of queue) {
        if (nextEligibleAt(entry.info, at) <= at) {
          selected = entry;
          break;
        }
      }
      if (!selected) break;
      removeQueuedEntry(selected);
      dispatch(selected, at);
    }
    scheduleDrain();
  };

  const enqueue = (original, thisValue, args, info) => {
    const sameKeyQueued = queue.reduce(
      (count, entry) => count + (entry.info.key === info.key ? 1 : 0),
      0,
    );
    if (
      queue.length >= maximumQueueSize ||
      sameKeyQueued >= maximumPerKeyQueueSize
    ) {
      counters.rejected += 1;
      return Promise.reject(makeGuardError("queue capacity exceeded", info));
    }
    counters.queued += 1;
    return new Promise((resolve, reject) => {
      const entry = {
        original,
        thisValue,
        args,
        info,
        enqueuedAt: now(),
        resolve,
        reject,
      };
      queue.push(entry);
      if (info.id !== undefined) queuedByRequestId.set(info.id, entry);
      scheduleDrain();
    });
  };

  const sendGuarded = (original, thisValue, args, info) => {
    counters.matched += 1;
    const at = now();
    if (queue.length === 0 && nextEligibleAt(info, at) <= at) {
      return new Promise((resolve, reject) => {
        dispatch({ original, thisValue, args, info, resolve, reject }, at);
      });
    }
    return enqueue(original, thisValue, args, info);
  };

  const patchSendWorkerMessage = (bridge) => {
    const current = bridge?.sendWorkerMessageFromView;
    if (typeof current !== "function") return false;
    if (current.__codeyGitRequestGuardOwner === api) return true;

    const original = current;
    const wrapped = function (...args) {
      const [workerId, message] = args;
      if (
        enabled &&
        workerId === "git" &&
        message?.type === "worker-request-cancel"
      ) {
        const queued = queuedByRequestId.get(message.id);
        if (queued) {
          removeQueuedEntry(queued);
          counters.cancelledBeforeSend += 1;
          queued.resolve(undefined);
          scheduleDrain();
          return Promise.resolve(undefined);
        }
        sentKeyByRequestId.delete(message.id);
      }

      const info =
        enabled && !mainProcessProtected ? requestInfo(workerId, message) : null;
      if (!info) return Reflect.apply(original, this, args);
      return sendGuarded(original, this, args, info);
    };
    Object.defineProperties(wrapped, {
      __codeyGitRequestGuardOwner: { value: api },
      __codeyGitRequestGuardOriginal: { value: original },
    });
    return replaceBridgeMethod(bridge, "sendWorkerMessageFromView", wrapped);
  };

  const patchWorkerMessageSubscription = (bridge) => {
    const current = bridge?.subscribeToWorkerMessages;
    if (typeof current !== "function") return false;
    if (current.__codeyGitRequestGuardOwner === api) return true;

    const original = current;
    const wrapped = function (workerId, listener, ...rest) {
      if (workerId !== "git" || typeof listener !== "function") {
        return Reflect.apply(original, this, [workerId, listener, ...rest]);
      }
      observedGitSubscriptions += 1;
      const observed = function (...listenerArgs) {
        try {
          observeWorkerMessage(workerId, listenerArgs[0]);
        } catch {}
        return Reflect.apply(listener, this, listenerArgs);
      };
      return Reflect.apply(original, this, [workerId, observed, ...rest]);
    };
    Object.defineProperties(wrapped, {
      __codeyGitRequestGuardOwner: { value: api },
      __codeyGitRequestGuardOriginal: { value: original },
    });
    return replaceBridgeMethod(bridge, "subscribeToWorkerMessages", wrapped);
  };

  const probeMainProcessProtection = () => {
    if (!enabled || mainProcessProtected || mainProcessProbePending) {
      return mainProcessProbePending;
    }
    const bridge = window.electronBridge;
    const sendStatusRequest = bridge?.sendMessageFromView;
    if (typeof sendStatusRequest !== "function") {
      mainProcessProbeError = "主进程状态通道不可用";
      return null;
    }
    mainProcessProbeAttempts += 1;
    const requestId =
      window.crypto?.randomUUID?.() ??
      `codey-git-guard-${Date.now()}-${mainProcessProbeAttempts}`;
    let responseListener = null;
    let responseTimer = 0;
    let finishResponseWait;
    const responseWait = new Promise((resolve) => {
      let settled = false;
      finishResponseWait = (value) => {
        if (settled) return;
        settled = true;
        if (responseListener) {
          window.removeEventListener?.("message", responseListener);
        }
        if (responseTimer) window.clearTimeout?.(responseTimer);
        resolve(value);
      };
      responseListener = (event) => {
        const message = event?.data;
        if (
          message?.type === mainProcessStatusResponseType &&
          message?.requestId === requestId
        ) {
          finishResponseWait(message);
        }
      };
      if (typeof window.addEventListener !== "function") {
        finishResponseWait(null);
        return;
      }
      window.addEventListener("message", responseListener);
      responseTimer = window.setTimeout?.(
        () => finishResponseWait(null),
        mainProcessProbeTimeoutMs,
      ) ?? 0;
    });
    const request = Promise.resolve()
      .then(() =>
        Reflect.apply(sendStatusRequest, bridge, [
          { type: mainProcessStatusRequestType, version, requestId },
        ]),
      )
      .then(async (directResult) => {
        const result = directResult === undefined
          ? await responseWait
          : directResult;
        const guard = result?.guard;
        if (
          result?.status === "ok" &&
          guard?.enabled === true &&
          guard?.gitHandlerPatched === true &&
          guard.matched > 0
        ) {
          mainProcessProtected = true;
          mainProcessProbeTransport =
            directResult === undefined ? "renderer-event" : "invoke-return";
          mainProcessSnapshot = guard;
          mainProcessProbeError = "";
          if (bridgeRetryTimer) window.clearTimeout(bridgeRetryTimer);
          bridgeRetryTimer = 0;
          markEffective();
          return;
        }
        mainProcessProbeError =
          result?.status === "ok"
            ? "主进程尚未确认受保护的 Git 请求"
            : "主进程未回传保护状态";
      })
      .catch((error) => {
        mainProcessProbeError = String(
          error instanceof Error ? error.message : error || "主进程状态查询失败",
        ).slice(0, 160);
      })
      .finally(() => {
        finishResponseWait(null);
        if (mainProcessProbePending === request) {
          mainProcessProbePending = null;
        }
      });
    mainProcessProbePending = request;
    return request;
  };

  const ensureInstalled = () => {
    if (!enabled) {
      bridgePatched = false;
      responseObserverPatched = false;
      markEffective();
      return true;
    }
    probeMainProcessProtection();
    const bridge = window.electronBridge;
    bridgePatched = patchSendWorkerMessage(bridge);
    responseObserverPatched = patchWorkerMessageSubscription(bridge);
    if (mainProcessProtected || bridgePatched) {
      if (bridgeRetryTimer) window.clearTimeout(bridgeRetryTimer);
      bridgeRetryTimer = 0;
      bridgeSlowRetries = 0;
      markEffective();
      return true;
    }
    scheduleBridgeRetry();
    return false;
  };

  const snapshot = () => ({
    version,
    enabled,
    installed: enabled ? mainProcessProtected || bridgePatched : true,
    strategy: mainProcessProtected
      ? "main-process-ipc"
      : bridgePatched
        ? "renderer-bridge"
        : "pending",
    mainProcessProtected,
    mainProcessProbeAttempts,
    mainProcessProbeError,
    mainProcessSnapshot,
    mainProcessProbeTransport,
    bridgePatched,
    responseObserverPatched,
    observedGitSubscriptions,
    queued: queue.length,
    matched: counters.matched,
    sent: counters.sent,
    queuedTotal: counters.queued,
    cancelledBeforeSend: counters.cancelledBeforeSend,
    rejected: counters.rejected,
    transportFailures: counters.transportFailures,
    observedFailures: counters.observedFailures,
    lastMethod,
    targetMethods: [...targetMethods],
    tokenCapacity,
    tokenRefillMs,
    perKeyIntervalMs,
  });

  const scheduleBridgeRetry = () => {
    if (bridgeRetryTimer || typeof window.setTimeout !== "function") return;
    const fastRetry = now() < bridgeRetryDeadline;
    if (!fastRetry) {
      if (bridgeSlowRetries >= MAX_SLOW_BRIDGE_RETRIES) return;
      bridgeSlowRetries += 1;
    }
    const delay = fastRetry ? bridgeRetryDelay : 30_000;
    bridgeRetryTimer = window.setTimeout(retryInstall, delay);
  };

  const retryInstall = () => {
    bridgeRetryTimer = 0;
    const fastRetry = now() < bridgeRetryDeadline;
    bridgeRetryDelay = fastRetry ? Math.min(bridgeRetryDelay * 2, 2_000) : 30_000;
    ensureInstalled();
  };

  const requestInstall = () => {
    bridgeRetryDeadline = now() + 30_000;
    bridgeRetryDelay = 50;
    bridgeSlowRetries = 0;
    if (bridgeRetryTimer) window.clearTimeout(bridgeRetryTimer);
    bridgeRetryTimer = 0;
    return ensureInstalled();
  };

  const api = Object.freeze({
    version,
    enabled,
    ensureInstalled,
    requestInstall,
    snapshot,
  });
  Object.defineProperty(window, guardKey, {
    configurable: false,
    value: api,
    writable: false,
  });

  requestInstall();
})();
