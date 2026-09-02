(() => {
  const disablePet = __DISABLE_PET__;
  const requireAppServerRuntimeOverrideValidation =
    __REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__;
  const codeyErrorLoggerExecutable = "__CODEY_ERROR_LOGGER_EXECUTABLE__";
  const maxOptionalPatchFailureBatchSize = 64;
  const optionalPatchFailureQueue = [];
  let optionalPatchFailureFlushScheduled = false;
  const reportPatchLogError = (error) => {
    try {
      console.error("[Codey] failed to write patch error log", error);
    } catch {}
  };
  const writeCodeyPatchFailureSync = (record) => {
    const result = process.getBuiltinModule("child_process").spawnSync(
      codeyErrorLoggerExecutable,
      ["--codey-record-error"],
      {
        input: JSON.stringify(record),
        encoding: "utf8",
        maxBuffer: 64 * 1024,
        timeout: 2000,
        windowsHide: true,
      },
    );
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new Error(
        `Codey error log helper exited with ${result.status}: ${String(result.stderr || "").trim()}`,
      );
    }
  };
  const writeCodeyPatchFailuresAsync = (records) => {
    try {
      const child = process.getBuiltinModule("child_process").spawn(
        codeyErrorLoggerExecutable,
        ["--codey-record-error"],
        {
          stdio: ["pipe", "ignore", "ignore"],
          windowsHide: true,
        },
      );
      const timeout = setTimeout(() => {
        try {
          child.kill();
        } catch {}
      }, 2000);
      timeout.unref?.();
      const clearKillTimeout = () => clearTimeout(timeout);
      child.once("exit", clearKillTimeout);
      child.once("error", (error) => {
        clearKillTimeout();
        reportPatchLogError(error);
      });
      child.stdin?.once("error", reportPatchLogError);
      child.stdin?.end(JSON.stringify(records), "utf8");
      child.unref();
    } catch (error) {
      reportPatchLogError(error);
    }
  };
  const scheduleOptionalPatchFailureFlush = () => {
    if (optionalPatchFailureFlushScheduled) return;
    optionalPatchFailureFlushScheduled = true;
    setImmediate(() => {
      optionalPatchFailureFlushScheduled = false;
      const records = optionalPatchFailureQueue.splice(
        0,
        maxOptionalPatchFailureBatchSize,
      );
      if (records.length) writeCodeyPatchFailuresAsync(records);
      if (optionalPatchFailureQueue.length) scheduleOptionalPatchFailureFlush();
    });
  };
  const queueOptionalPatchFailure = (record) => {
    if (optionalPatchFailureQueue.length >= maxOptionalPatchFailureBatchSize) return;
    optionalPatchFailureQueue.push(record);
    scheduleOptionalPatchFailureFlush();
  };
  const recordCodeyPatchFailure = (operation, error, context = {}) => {
    const unresolvedExecutable =
      ["__CODEY", "ERROR_LOGGER_EXECUTABLE__"].join("_");
    if (
      !codeyErrorLoggerExecutable ||
      codeyErrorLoggerExecutable === unresolvedExecutable
    ) return;
    const message = error instanceof Error
      ? `${error.name}: ${error.message}${error.stack ? `\n${error.stack}` : ""}`
      : String(error || "unknown patch failure");
    try {
      const now = new Date();
      const platform =
        process.platform === "win32"
          ? "windows"
          : process.platform === "darwin"
            ? "macos"
            : process.platform;
      const optionalPatch =
        operation.startsWith("renderer_patch:") ||
        operation.startsWith("optional_main_bundle_patch:") ||
        operation === "patch_codex_renderer_asset";
      const stage = operation.startsWith("renderer_patch:") ||
        operation === "patch_codex_renderer_asset"
        ? "startup.renderer_asset_patch"
        : operation.startsWith("optional_main_bundle_patch:")
          ? "startup.optional_main_bundle_patch"
          : "startup.main_process_patch";
      const record = {
        timestamp: now.toISOString(),
        platform,
        versions: {
          electron: process.versions?.electron || undefined,
          chrome: process.versions?.chrome || undefined,
          node: process.versions?.node || undefined,
        },
        event: "patch_failed",
        operation,
        error: message,
        stage,
        recoverable: optionalPatch,
        context,
      };
      if (optionalPatch) queueOptionalPatchFailure(record);
      else writeCodeyPatchFailureSync(record);
    } catch (logError) {
      reportPatchLogError(logError);
    }
  };
  const threadOwnerDiscoveryTimeoutMs = 150;
  const disableWindowsOptimizations = process.platform === "win32";
  const disableMicro = disableWindowsOptimizations;
  const disableWindowsWmiSampler = disableWindowsOptimizations;
  const Module = process.getBuiltinModule("module");
  const originalLoad = Module._load;
  const mainGitGuardStatusRequestType = "codey-git-request-guard-status";
  const mainGitGuardStatusResponseType =
    "codey-git-request-guard-status-response";
  const windowsWmiSamplerStatusRequestType =
    "codey-windows-wmi-sampler-status";
  const windowsWmiSamplerStatusResponseType =
    "codey-windows-wmi-sampler-status-response";
  const rendererMessageChannel = "codex_desktop:message-for-view";
  const windowsWmiSamplerInstalledAtMs = Date.now();
  const windowsWmiSamplerEvidence = {
    version: 4,
    enabled: disableWindowsWmiSampler,
    workerWrapperPatched: false,
    esmExportsSynchronized: false,
    selfTestPassed: false,
    selfTestError: "",
    workersObserved: 0,
    sourceInspections: 0,
    sourceSignatureMatches: 0,
    sourceSignatureMisses: 0,
    sourceReadFailures: 0,
    blocked: 0,
    lastMatchReason: "",
    lastWorkerName: "",
    lastObservedWorkerName: "",
    lastObservedThreadName: "",
    lastObservedSourceSignals: [],
  };
  const windowsWmiSamplerSnapshot = () => ({
    ...windowsWmiSamplerEvidence,
    installed:
      !windowsWmiSamplerEvidence.enabled ||
      (windowsWmiSamplerEvidence.workerWrapperPatched &&
        windowsWmiSamplerEvidence.esmExportsSynchronized),
    observationMs: Math.max(0, Date.now() - windowsWmiSamplerInstalledAtMs),
  });
  const createMainGitRequestGuard = ({
    enabled = false,
    clock = () => Date.now(),
    scheduleTimeout = (callback, delay) => setTimeout(callback, delay),
    cancelTimeout = (timer) => clearTimeout(timer),
    limits = {},
  } = {}) => {
    const targetMethods = new Set([
      "git-origins",
      "status-summary",
      "review-summary",
      "branch-diff-stats",
    ]);
    const tokenCapacity = limits.tokenCapacity ?? 3;
    const tokenRefillMs = limits.tokenRefillMs ?? 1000;
    const perKeyIntervalMs = limits.perKeyIntervalMs ?? 2000;
    const maximumQueueSize = limits.maximumQueueSize ?? 48;
    const maximumPerKeyQueueSize = limits.maximumPerKeyQueueSize ?? 6;
    const maximumQueueWaitMs = limits.maximumQueueWaitMs ?? 15000;
    const queue = [];
    const queuedByRequestId = new Map();
    const lastSentAtByKey = new Map();
    const counters = {
      matched: 0,
      sent: 0,
      queued: 0,
      cancelledBeforeSend: 0,
      rejected: 0,
    };
    let availableTokens = tokenCapacity;
    let tokenUpdatedAt = Number(clock()) || 0;
    let drainTimer = null;
    let drainTimerAt = Number.POSITIVE_INFINITY;
    let gitHandlerPatched = false;
    let statusHandlerPatched = false;
    let ipcHandlersWrapped = 0;
    let lastWrappedChannel = "";
    let lastMethod = "";

    const now = () => {
      const value = Number(clock());
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
      typeof value === "string" ? value.slice(0, 2048) : "";
    const requestInfo = (message, channel = "") => {
      if (
        !message ||
        typeof message !== "object" ||
        message.type !== "worker-request" ||
        (message.workerId != null && message.workerId !== "git") ||
        (message.workerId == null &&
          !/(?:^|[:/_-])git(?:$|[:/_-])/i.test(channel))
      ) {
        return null;
      }
      const request = message.request;
      if (
        !request ||
        typeof request !== "object" ||
        typeof request.method !== "string"
      ) {
        return null;
      }
      const workerMethod = request.method;
      const outerParams =
        request.params && typeof request.params === "object"
          ? request.params
          : {};
      const query =
        workerMethod === "subscribe-live-query" &&
        outerParams.query &&
        typeof outerParams.query === "object"
          ? outerParams.query
          : null;
      const method =
        query && typeof query.method === "string"
          ? query.method
          : workerMethod;
      if (!targetMethods.has(method) && query == null) return null;
      const params =
        query?.params && typeof query.params === "object"
          ? query.params
          : outerParams;
      const keyMaterial = [
        workerMethod,
        method,
        stringPart(params.operationSource ?? outerParams.operationSource),
        method === "git-origins"
          ? "all-origins"
          : stringPart(
              params.cwd ??
                params.root ??
                params.commonDir ??
                outerParams.cwd ??
                outerParams.root ??
                outerParams.commonDir,
            ),
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
      return Math.max(tokenReadyAt, keyReadyAt);
    };
    const makeGuardError = (reason, info) => {
      const error = new Error(`Codey Git request guard: ${reason}`);
      error.name = "CodeyGitRequestGuardError";
      error.code = "CODEY_GIT_REQUEST_THROTTLED";
      error.method = info?.method ?? "";
      return error;
    };
    const removeQueuedEntry = (entry) => {
      const index = queue.indexOf(entry);
      if (index >= 0) queue.splice(index, 1);
      if (entry.info.id !== undefined) {
        queuedByRequestId.delete(entry.info.id);
      }
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
      lastMethod = entry.info.method;
      counters.sent += 1;
      let result;
      try {
        result = Reflect.apply(entry.handler, entry.thisValue, entry.args);
      } catch (error) {
        entry.reject(error);
        scheduleDrain();
        return;
      }
      Promise.resolve(result).then(entry.resolve, entry.reject).finally(scheduleDrain);
    };
    const scheduleDrain = () => {
      if (!enabled || queue.length === 0) return;
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
      if (drainTimer !== null && drainTimerAt <= earliest) return;
      if (drainTimer !== null) cancelTimeout(drainTimer);
      drainTimerAt = earliest;
      drainTimer = scheduleTimeout(drain, Math.max(0, earliest - at));
      drainTimer?.unref?.();
    };
    const drain = () => {
      drainTimer = null;
      drainTimerAt = Number.POSITIVE_INFINITY;
      let at = now();
      for (const entry of [...queue]) {
        if (at - entry.enqueuedAt >= maximumQueueWaitMs) {
          rejectEntry(entry, "queue timeout");
        }
      }
      while (queue.length > 0) {
        at = now();
        const selected = queue.find(
          (entry) => nextEligibleAt(entry.info, at) <= at,
        );
        if (!selected) break;
        removeQueuedEntry(selected);
        dispatch(selected, at);
      }
      scheduleDrain();
    };
    const enqueue = (handler, thisValue, args, info) => {
      const sameKeyQueued = queue.reduce(
        (count, entry) => count + (entry.info.key === info.key ? 1 : 0),
        0,
      );
      if (
        queue.length >= maximumQueueSize ||
        sameKeyQueued >= maximumPerKeyQueueSize
      ) {
        counters.rejected += 1;
        return Promise.reject(
          makeGuardError("queue capacity exceeded", info),
        );
      }
      counters.queued += 1;
      return new Promise((resolve, reject) => {
        const entry = {
          handler,
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
    const sendGuarded = (handler, thisValue, args, info) => {
      counters.matched += 1;
      const at = now();
      if (queue.length === 0 && nextEligibleAt(info, at) <= at) {
        return new Promise((resolve, reject) => {
          dispatch({ handler, thisValue, args, info, resolve, reject }, at);
        });
      }
      return enqueue(handler, thisValue, args, info);
    };
    const snapshot = () => ({
      version: 1,
      enabled,
      installed: enabled ? gitHandlerPatched : true,
      strategy: enabled ? "main-process-ipc" : "not-required",
      gitHandlerPatched,
      statusHandlerPatched,
      ipcHandlersWrapped,
      lastWrappedChannel,
      queued: queue.length,
      matched: counters.matched,
      sent: counters.sent,
      queuedTotal: counters.queued,
      cancelledBeforeSend: counters.cancelledBeforeSend,
      rejected: counters.rejected,
      lastMethod,
      targetMethods: [...targetMethods],
      tokenCapacity,
      tokenRefillMs,
      perKeyIntervalMs,
    });
    const wrapGitHandler = (handler, channel = "") => {
      if (typeof handler !== "function") return handler;
      if (handler.__codeyMainGitRequestGuardOwner === api) {
        gitHandlerPatched = true;
        return handler;
      }
      gitHandlerPatched = true;
      const wrapped = function (...args) {
        if (!enabled) return Reflect.apply(handler, this, args);
        const message = args[1];
        if (
          message?.type === "worker-request-cancel" &&
          (message.workerId === "git" ||
            (message.workerId == null &&
              /(?:^|[:/_-])git(?:$|[:/_-])/i.test(channel)))
        ) {
          const queued = queuedByRequestId.get(message.id);
          if (queued) {
            removeQueuedEntry(queued);
            counters.cancelledBeforeSend += 1;
            queued.resolve(undefined);
            scheduleDrain();
            return Promise.resolve(undefined);
          }
        }
        const info = requestInfo(message, channel);
        if (!info) return Reflect.apply(handler, this, args);
        return sendGuarded(handler, this, args, info);
      };
      Object.defineProperty(wrapped, "__codeyMainGitRequestGuardOwner", {
        value: api,
      });
      return wrapped;
    };
    const wrapStatusHandler = (handler) => {
      if (typeof handler !== "function") return handler;
      if (handler.__codeyMainGitRequestGuardStatusOwner === api) {
        statusHandlerPatched = true;
        return handler;
      }
      statusHandlerPatched = true;
      const wrapped = function (...args) {
        const event = args[0];
        const message = args[1];
        const sendStatusResponse = (type, payload) => {
          const requestId =
            typeof message?.requestId === "string" ? message.requestId : "";
          if (!requestId || typeof event?.sender?.send !== "function") return;
          try {
            event.sender.send(rendererMessageChannel, {
              type,
              requestId,
              status: "ok",
              ...payload,
            });
          } catch {}
        };
        if (message?.type === mainGitGuardStatusRequestType) {
          const guard = snapshot();
          sendStatusResponse(mainGitGuardStatusResponseType, { guard });
          return { status: "ok", guard };
        }
        if (message?.type === windowsWmiSamplerStatusRequestType) {
          const sampler = windowsWmiSamplerSnapshot();
          sendStatusResponse(windowsWmiSamplerStatusResponseType, { sampler });
          return { status: "ok", sampler };
        }
        return Reflect.apply(handler, this, args);
      };
      Object.defineProperty(wrapped, "__codeyMainGitRequestGuardStatusOwner", {
        value: api,
      });
      return wrapped;
    };
    const wrapIpcHandler = (handler, channel = "") => {
      if (typeof handler !== "function") return handler;
      if (handler.__codeyMainIpcGuardOwner === api) return handler;
      const wrapped = wrapStatusHandler(wrapGitHandler(handler, channel));
      Object.defineProperty(wrapped, "__codeyMainIpcGuardOwner", {
        value: api,
      });
      ipcHandlersWrapped += 1;
      lastWrappedChannel = String(channel || "").slice(0, 160);
      return wrapped;
    };
    const api = Object.freeze({
      enabled,
      snapshot,
      wrapGitHandler,
      wrapIpcHandler,
      wrapStatusHandler,
    });
    return api;
  };
  const mainGitRequestGuard = createMainGitRequestGuard({
    enabled: disableWindowsOptimizations,
  });
  Object.defineProperty(globalThis, "__CODEY_CREATE_MAIN_GIT_REQUEST_GUARD__", {
    configurable: false,
    value: createMainGitRequestGuard,
    writable: false,
  });
  Object.defineProperty(globalThis, "__CODEY_MAIN_GIT_REQUEST_GUARD__", {
    configurable: false,
    value: mainGitRequestGuard,
    writable: false,
  });
  const isInspectorArgument = (argument) =>
    typeof argument === "string" && /^--inspect(?:-brk)?(?:=|$)/.test(argument);
  const maxRendererPatchFingerprints = 64;
  const rendererPatchFailuresByFingerprint = new Map();
  let activeRendererPatchFailures = null;
  const rendererPatchFingerprint = (source) => {
    try {
      return process
        .getBuiltinModule("crypto")
        .createHash("sha256")
        .update(source)
        .digest("base64url");
    } catch {
      // Fingerprinting is only an optimization. If crypto is unavailable, keep
      // the existing compatibility behavior instead of risking a false cache hit.
      return null;
    }
  };
  const rendererPatchFailuresForSource = (source) => {
    const fingerprint = rendererPatchFingerprint(source);
    if (fingerprint == null) return null;
    const existing = rendererPatchFailuresByFingerprint.get(fingerprint);
    if (existing) {
      // Refresh insertion order so the bounded map behaves as an LRU.
      rendererPatchFailuresByFingerprint.delete(fingerprint);
      rendererPatchFailuresByFingerprint.set(fingerprint, existing);
      return existing;
    }
    const failures = new Set();
    rendererPatchFailuresByFingerprint.set(fingerprint, failures);
    while (rendererPatchFailuresByFingerprint.size > maxRendererPatchFingerprints) {
      const oldest = rendererPatchFailuresByFingerprint.keys().next().value;
      rendererPatchFailuresByFingerprint.delete(oldest);
    }
    return failures;
  };
  // Each renderer gate is optional and independent. Codex bundles are minified
  // and reshape between releases, so a single drifted anchor must skip only its
  // own gate — never discard the sibling gates that are still compatible. That
  // is what previously hid the whole Fast/service-tier control on the builds
  // where one unrelated anchor moved: an exception here aborted every gate on
  // the asset. Log and return the source unchanged so the rest still apply.
  // Field builds ship minified bundles whose shapes drift between platforms
  // and releases. When a gate matches nothing, these are the neighborhood
  // markers every gate sits near; capturing printable windows around them lets
  // a field diagnostic log be turned into the next compatible variant without
  // reproducing that exact bundle locally.
  const rendererGateDiagnosticAnchors = [
    "`composer.toggleFastMode`",
    "composer.speedSlashCommand.disableDescription",
    "isServiceTierAllowed",
    "selectedServiceTier",
    "featureRequirements?.fast_mode",
    "useHiddenModels",
    "availableOptions.length",
    "includeUltraReasoningEffort",
    "isCustomModelProvider",
  ];
  const rendererGateFailureExcerpts = (source) => {
    const excerpts = [];
    for (const anchor of rendererGateDiagnosticAnchors) {
      if (excerpts.length >= 2) break;
      const index = source.indexOf(anchor);
      if (index < 0) continue;
      excerpts.push(
        source
          .slice(Math.max(0, index - 150), index + anchor.length + 190)
          .replace(/[^\x20-\x7E]/g, "?"),
      );
    }
    return excerpts;
  };
  const recordIncompatibleRendererGate = (source, name, matchCount) => {
    activeRendererPatchFailures?.add(name);
    const message =
      `Codey skipped an incompatible Codex renderer patch: ${name} gate matched ${matchCount} times`;
    const context = { matchCount };
    const excerpts = rendererGateFailureExcerpts(source);
    if (excerpts.length) context.excerpts = excerpts;
    recordCodeyPatchFailure(`renderer_patch:${name}`, message, context);
    try {
      console.error(message);
    } catch {}
    return source;
  };
  const replaceUniqueRendererGate = (source, pattern, replacement, name) => {
    // app:// assets can be requested repeatedly during reloads or renderer
    // recovery. A gate already known to be incompatible with the exact same
    // source must remain skipped without rerunning its full-bundle regexes or
    // spawning another error-log helper.
    if (activeRendererPatchFailures?.has(name)) return source;
    const gates = Array.isArray(pattern) ? pattern : [{ pattern, replacement }];
    let matchCount = 0;
    let patched = source;
    for (const gate of gates) {
      let gateCount = 0;
      const candidate = source.replace(gate.pattern, (...args) => {
        gateCount += 1;
        return typeof gate.replacement === "function"
          ? gate.replacement(...args)
          : gate.replacement;
      });
      if (gateCount > 0 && matchCount === 0) patched = candidate;
      matchCount += gateCount;
    }
    if (matchCount !== 1) {
      return recordIncompatibleRendererGate(source, name, matchCount);
    }
    return patched;
  };
  const replaceNearestRendererGateBeforeAnchor = (
    source,
    pattern,
    replacement,
    name,
    anchor,
    maximumDistance,
  ) => {
    if (activeRendererPatchFailures?.has(name)) return source;
    const anchorIndexes = [];
    for (
      let index = source.indexOf(anchor);
      index >= 0;
      index = source.indexOf(anchor, index + anchor.length)
    ) anchorIndexes.push(index);
    if (anchorIndexes.length !== 1) {
      return recordIncompatibleRendererGate(source, name, anchorIndexes.length);
    }

    const anchorIndex = anchorIndexes[0];
    const scopeStart = Math.max(0, anchorIndex - maximumDistance);
    const scope = source.slice(scopeStart, anchorIndex + anchor.length);
    const gates = Array.isArray(pattern) ? pattern : [{ pattern, replacement }];
    const candidates = [];
    for (const gate of gates) {
      scope.replace(gate.pattern, (...args) => {
        candidates.push({ args, gate, offset: args.at(-2) });
        return args[0];
      });
    }
    if (candidates.length === 0) {
      return recordIncompatibleRendererGate(source, name, 0);
    }

    const nearestOffset = Math.max(
      ...candidates.map((candidate) => candidate.offset),
    );
    const nearestCandidates = candidates.filter(
      (candidate) => candidate.offset === nearestOffset,
    );
    if (nearestCandidates.length !== 1) {
      return recordIncompatibleRendererGate(
        source,
        name,
        nearestCandidates.length,
      );
    }

    const [{ args, gate, offset }] = nearestCandidates;
    const effectiveReplacement = gate.replacement ?? replacement;
    const replaced = typeof effectiveReplacement === "function"
      ? effectiveReplacement(...args)
      : effectiveReplacement;
    const absoluteOffset = scopeStart + offset;
    return source.slice(0, absoluteOffset) +
      replaced +
      source.slice(absoluteOffset + args[0].length);
  };
  const rendererHasNativeCustomProviderModelAccess = (source) =>
    /function\s+[$A-Z_a-z][$\w]*\(\{[^}]*isCustomModelProvider\s*:\s*([$A-Z_a-z][$\w]*)[^}]*model\s*:\s*([$A-Z_a-z][$\w]*)[^}]*useHiddenModels\s*:\s*([$A-Z_a-z][$\w]*)[^}]*\}\)\s*\{\s*return[\s\S]{0,512}?\3\s*&&\s*!\s*\1\s*&&[\s\S]{0,256}?\?\s*[$A-Z_a-z][$\w]*\.has\(\s*\2\.model\s*\)\s*:\s*!\s*\2\.hidden\s*\)*\s*\}/.test(
      source,
    );
  const replacePetRendererImportWithStubs = (match, importClause) => {
    if (typeof importClause !== "string" || importClause.trim() === "") {
      return "";
    }
    const localBindings = [];
    const rememberBinding = (binding) => {
      if (
        /^[$A-Z_a-z][$\w]*$/.test(binding)
        && !localBindings.includes(binding)
      ) {
        localBindings.push(binding);
      }
    };
    const defaultBinding = importClause.match(/^\s*([$A-Z_a-z][$\w]*)/);
    if (defaultBinding) rememberBinding(defaultBinding[1]);
    for (const specifier of importClause.matchAll(
      /(?:^|[,{])\s*([$A-Z_a-z][$\w]*)(?:\s+as\s+([$A-Z_a-z][$\w]*))?\s*(?=[,}])/g,
    )) {
      rememberBinding(specifier[2] ?? specifier[1]);
    }
    for (const namespace of importClause.matchAll(
      /\*\s+as\s+([$A-Z_a-z][$\w]*)/g,
    )) {
      rememberBinding(namespace[1]);
    }
    if (!localBindings.length) {
      const message =
        "Codey could not identify Codex pet settings renderer import bindings";
      recordCodeyPatchFailure("renderer_patch:pet settings avatar resources", message);
      try {
        console.error(message);
      } catch {}
      return match;
    }
    const [firstBinding, ...aliases] = localBindings;
    const aliasDeclarations = aliases
      .map((binding) => `,${binding}=${firstBinding}`)
      .join("");
    return `const ${firstBinding}=(()=>{const target=function(){return null};return new Proxy(target,{get(target,property,receiver){if(property===Symbol.iterator)return function*(){};if(property===\`map\`||property===\`filter\`||property===\`flatMap\`||property===\`slice\`)return()=>[];if(property===\`then\`)return void 0;return Reflect.get(target,property,receiver)},construct(){return{}}})})()${aliasDeclarations};`;
  };
  const threadOwnerDiscoveryExpression = (
    coordinationName,
    hostIdName,
    conversationIdName,
  ) =>
    [
      "await (globalThis.__CODEY_THREAD_OWNER_DISCOVERY_V2__??=(()=>{",
      "const requestsByClient=new WeakMap;",
      "return{find(client,hostId,conversationId){",
      "let requests=requestsByClient.get(client);",
      "if(requests==null){requests=new Map;requestsByClient.set(client,requests)}",
      "const key=String(hostId)+String.fromCharCode(0)+String(conversationId);",
      "const existing=requests.get(key);",
      "if(existing!=null)return existing;",
      "let settled=false,timer;",
      "const lookup=Promise.resolve().then(()=>client.findThreadOwner({hostId,conversationId}));",
      "const request=new Promise((resolve,reject)=>{",
      `timer=globalThis.setTimeout(()=>{if(settled)return;settled=true;resolve(null)},${threadOwnerDiscoveryTimeoutMs});`,
      "lookup.then(owner=>{",
      "if(settled)return;",
      "settled=true;globalThis.clearTimeout(timer);",
      "resolve(owner)",
      "},error=>{",
      "if(settled)return;",
      "settled=true;globalThis.clearTimeout(timer);reject(error)",
      "})",
      "}).finally(()=>{if(requests.get(key)===request)requests.delete(key)});",
      "requests.set(key,request);",
      "return request",
      "}}",
      "})()).find(",
      `${coordinationName}.clientCoordination,${hostIdName},${conversationIdName})`,
    ].join("");
  const patchCodexRendererAsset = (source) => {
    let patched = source;
    let nativeCustomProviderModelAccess = false;
    if (
      source.includes("codex-message-from-view")
      && source.includes("sendMessageFromView")
      && source.includes("Failed to send message from view")
    ) {
      // The native renderer forwards the request through Electron before it
      // emits codex-message-from-view. Event-only injections therefore see an
      // already-sent payload. Invoke Codey's synchronous route rewrite at the
      // actual bridge boundary so thread/start is born with modelProvider.
      patched = replaceUniqueRendererGate(
        patched,
        /if\(([$A-Z_a-z][$\w]*)\?\.sendMessageFromView\)\{let ([$A-Z_a-z][$\w]*)=([$A-Z_a-z][$\w]*);\1\.sendMessageFromView\(\2\)\.catch\(([$A-Z_a-z][$\w]*)=>\{/g,
        (_match, bridgeName, messageName, sourceName, errorName) =>
          `if(${bridgeName}?.sendMessageFromView){let ${messageName}=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage?.(${sourceName})??${sourceName};if(globalThis.__codeyModelWhitelistPatch?.isBlockedOutgoingMessage?.(${messageName})){globalThis.__codeyModelWhitelistPatch?.notifyBlockedOutgoingMessage?.(${messageName});return}${bridgeName}.sendMessageFromView(${messageName}).catch(${errorName}=>{`,
        "model route bridge preflight",
      );
    }
    if (
      source.includes("AppServerRequestClient is missing a message dispatcher")
      && source.includes("mcp_request_enqueued")
      && source.includes("this.dispatchMessage?.(`mcp-request`")
    ) {
      // Current Codex can create threads through AppServerRequestClient without
      // touching the renderer bridge helper above. Rewrite at enqueue time so
      // thread/start and prewarm requests bind the selected Codey route before
      // they reach the app server.
      patched = replaceUniqueRendererGate(
        patched,
        /(enqueueRequest\(([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*=>\{this\.dispatchMessage\?\.\(`mcp-request`,\{request:[$A-Z_a-z][$\w]*,hostId:this\.hostId,[\s\S]{0,700}?widget:\4\?\.widget\}\)\},[$A-Z_a-z][$\w]*=null\)\{)let /g,
        (_match, prefix, methodName, paramsName) =>
          `${prefix}let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage?.({type:\`mcp-request\`,request:{method:${methodName},params:${paramsName}}});if(__codeyRoute?.request){if(globalThis.__codeyModelWhitelistPatch?.isBlockedOutgoingMessage?.(__codeyRoute)){globalThis.__codeyModelWhitelistPatch?.notifyBlockedOutgoingMessage?.(__codeyRoute);return Promise.reject(Error(\`Codey blocked cross-provider model request\`))}${methodName}=__codeyRoute.request.method??${methodName},${paramsName}=__codeyRoute.request.params??${paramsName}}let `,
        "app server request route preflight",
      );
      // AppServerRequestClient runs the preflight before createRequest assigns
      // an id. Register the concrete request afterwards so a successful legacy
      // OpenAI resume is remembered as a codey_router migration when its reply
      // still exposes the rollout's persisted `openai` provider.
      patched = replaceUniqueRendererGate(
        patched,
        /(let\{request:([$A-Z_a-z][$\w]*),promise:[$A-Z_a-z][$\w]*\}=this\.createRequest\([^;]{1,256}\);)/g,
        (_match, createRequest, requestName) =>
          `${createRequest}globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage?.({type:\`mcp-request\`,request:${requestName}});`,
        "app server request identity tracking",
      );
    }
    if (
      disablePet
      && /settings\.(?:(?:appearance|personalization)\.)?pets(?:[."`]|$)/.test(source)
      && /import(?:\s*[^;"']+?\s*from)?\s*["']\.\/codex-avatar(?:[~-][^/"']*)?\.js["']/.test(source)
    ) {
      // Recent Codex builds keep the Pets settings preview in a regular
      // settings chunk and statically import codex-avatar from it. Hiding the
      // controls after React mounts is too late: that import has already pulled
      // the avatar renderer and every bundled spritesheet into the main window.
      // Replace only that settings-side dependency with inert callable/iterable
      // bindings. The shared avatar overlay host stays intact because current
      // Codex builds also use it for voice controls.
      patched = replaceUniqueRendererGate(
        patched,
        /import(?:\s*([^;"']+?)\s*from)?\s*["']\.\/codex-avatar(?:[~-][^/"']*)?\.js["'];?/g,
        replacePetRendererImportWithStubs,
        "pet settings avatar resources",
      );
    }
    if (
      source.includes("72216192") &&
      source.includes("enable_i18n") &&
      source.includes("locale_source") &&
      source.includes(".localeOverride")
    ) {
      // Resolve the locale before React's first i18n render. The later CDP
      // injection still persists localeOverride, but it can arrive after the
      // first route has already selected and cached English messages.
      patched = replaceUniqueRendererGate(
        patched,
        /let\s+([$A-Z_a-z][$\w]*)\s*=\s*([$A-Z_a-z][$\w]*)\s*,\s*([$A-Z_a-z][$\w]*)\s*=\s*([$A-Z_a-z][$\w]*)\?\.\s*get\(\s*`locale_source`\s*,\s*`IDE`\s*\)\s*,\s*([$A-Z_a-z][$\w]*)\s*=\s*([$A-Z_a-z][$\w]*)\(\s*([$A-Z_a-z][$\w]*)\.localeOverride\s*\)/g,
        (
          _match,
          i18nEnabledName,
          _i18nGateValueName,
          localeSourceName,
          _dynamicConfigName,
          localeOverrideName,
        ) =>
          `let ${i18nEnabledName}=(globalThis.__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__=!0),${localeSourceName}=\`SYSTEM\`,${localeOverrideName}=\`zh-CN\``,
        "default Chinese locale",
      );
    }
    if (
      source.includes("maybe_resume_owner_discovery_failed")
      && source.includes("followExistingOwner")
      && source.includes(".clientCoordination.findThreadOwner")
    ) {
      // Owner discovery is an optimization for reusing a stream already owned
      // by another window. Merge only duplicate in-flight lookups: a settled
      // positive answer can become stale as soon as its owner disconnects, and
      // reusing it would mark this renderer as a follower without receiving a
      // snapshot. Every later hydration attempt revalidates the live owner.
      // Lookups retain a short safety window before local hydration.
      patched = replaceUniqueRendererGate(
        patched,
        /await\s+([$A-Z_a-z][$\w]*)\.clientCoordination\.findThreadOwner\(\{\s*hostId\s*:\s*([$A-Z_a-z][$\w]*)\s*,\s*conversationId\s*:\s*([$A-Z_a-z][$\w]*)\s*\}\)/g,
        (_match, coordinationName, hostIdName, conversationIdName) =>
          threadOwnerDiscoveryExpression(
            coordinationName,
            hostIdName,
            conversationIdName,
          ),
        "thread owner discovery coalescing",
      );
    }
    if (
      !source.includes("codeyReconcileCompletedConversation")
      && source.includes("isLocalConversationInProgress")
      && source.includes("inactiveThreadUnsubscriber.clearConversationStreamOwnership")
      && source.includes("getConversationStreamRevision")
      && source.includes("async resumeConversation(")
    ) {
      // A renderer can miss the terminal notification while retaining its old
      // in-progress turn and stream role. Confirm the native app-server state
      // twice, reject a concurrent stream revision or follower window, then use
      // the same needs_resume + maybeResumeConversation path as reconnect. This
      // hydrates paginated history without discarding, interrupting, or starting
      // another turn.
      patched = replaceUniqueRendererGate(
        patched,
        /async resumeConversation\(([$A-Z_a-z][$\w]*)\)\{await this\.maybeResumeConversation\(\1\);/g,
        (_match, paramsName) =>
          `async codeyReconcileCompletedConversation(${paramsName}){let __codeyConversationId=${paramsName}?.conversationId,__codeyConversation=__codeyConversationId==null?null:this.getConversation(__codeyConversationId);if(__codeyConversationId==null||__codeyConversation==null||!this.productPolicy.runtimePolicy.isLocalConversationInProgress(__codeyConversation)||this.getStreamRole(__codeyConversationId)?.role===\`follower\`)return!1;let __codeyRevision=this.getConversationStreamRevision(__codeyConversationId),__codeyReadStatus=async()=>{let __codeyResponse=await this.sendRequest(\`thread/read\`,{threadId:__codeyConversationId,includeTurns:!1}),__codeyStatus=__codeyResponse?.thread?.status;return typeof __codeyStatus===\`string\`?__codeyStatus:__codeyStatus?.type},__codeyStatus=await __codeyReadStatus();if(__codeyStatus!==\`idle\`&&__codeyStatus!==\`error\`)return!1;await new Promise(__codeyResolve=>setTimeout(__codeyResolve,250));if(await __codeyReadStatus()!==__codeyStatus||this.getConversationStreamRevision(__codeyConversationId)!==__codeyRevision)return!1;__codeyConversation=this.getConversation(__codeyConversationId);if(__codeyConversation==null||!this.productPolicy.runtimePolicy.isLocalConversationInProgress(__codeyConversation)||this.getStreamRole(__codeyConversationId)?.role===\`follower\`)return!1;this.inactiveThreadUnsubscriber.clearConversationStreamOwnership(__codeyConversationId);this.updateConversationState(__codeyConversationId,__codeyState=>{__codeyState.resumeState=\`needs_resume\`},!1);await this.maybeResumeConversation(${paramsName});__codeyConversation=this.getConversation(__codeyConversationId);return __codeyConversation!=null&&!this.productPolicy.runtimePolicy.isLocalConversationInProgress(__codeyConversation)}async resumeConversation(${paramsName}){await this.maybeResumeConversation(${paramsName});`,
        "completed thread reconciliation",
      );
    }
    if (
      source.includes("assistantMessage.hookStats.label")
      && source.includes("assistantMessage.hookStats.title")
      && source.includes("tooltipMaxWidth:")
    ) {
      // Hook details can exceed the collision-limited tooltip height. Opt this
      // one rich tooltip into Codex's native hover handoff so the pointer can
      // enter its scrollable content without closing it on trigger leave.
      patched = replaceUniqueRendererGate(
        patched,
        /(\{\s*)(tooltipContent\s*:\s*[$A-Z_a-z][$\w]*\s*,\s*tooltipClassName\s*:\s*`px-3 py-2`\s*,\s*tooltipMaxWidth\s*:\s*`min\(32rem,\s*var\(--radix-tooltip-content-available-width\),\s*calc\(100vw - 16px\)\)`)/g,
        (_match, objectStart, tooltipProps) =>
          `${objectStart}interactive:!0,${tooltipProps}`,
        "hook details interactivity",
      );
    }
    if (
      source.includes("useHiddenModels:") &&
      source.includes("availableModels:") &&
      source.includes("includeUltraReasoningEffort") &&
      source.includes("amazonBedrock")
    ) {
      // Newer Codex builds already bypass the native allowlist for custom
      // providers and fall back to the model's own visibility bit. Recognize
      // that semantic shape as compatible instead of logging a false failure.
      nativeCustomProviderModelAccess =
        rendererHasNativeCustomProviderModelAccess(source);
      if (!nativeCustomProviderModelAccess) {
        patched = replaceUniqueRendererGate(
          patched,
          /if\s*\(\s*\(*\s*(?:[$A-Z_a-z][$\w]*\s*(?:\?\.|\.)\s*has\(\s*[$A-Z_a-z][$\w]*\.model\s*\)\s*(?:===\s*!0)?\s*\|\|\s*)?\(?\s*([$A-Z_a-z][$\w]*)\s*\?\s*([$A-Z_a-z][$\w]*)\.has\(\s*([$A-Z_a-z][$\w]*)\.model\s*\)\s*:\s*(?:!\s*\3\.hidden|\3\.hidden\s*!==\s*!0|\3\.hidden\s*===\s*!1)\s*\)?\s*\)*\s*\)/g,
          (_match, useAllowlistName, allowlistName, modelName) =>
            `if(${useAllowlistName}?(${allowlistName}.has(${modelName}.model)||!${modelName}.hidden):!${modelName}.hidden)`,
          "model allowlist",
        );
      }
    }
    if (
      source.includes("useHiddenModels:") &&
      source.includes("includeUltraReasoningEffort") &&
      source.includes("amazonBedrock") &&
      !nativeCustomProviderModelAccess
    ) {
      patched = replaceUniqueRendererGate(
        patched,
        /(\b[$A-Z_a-z][$\w]*\s*=\s*\(?\s*[$A-Z_a-z][$\w]*(?:\s*(?:\?\.|\.)\s*[$A-Z_a-z][$\w]*)?\s*\)?\s*&&\s*)\(?\s*([$A-Z_a-z][$\w]*(?:\s*(?:\?\.|\.)\s*[$A-Z_a-z][$\w]*)?)\s*(?:!==|!=)\s*(["'`])amazonBedrock\3\s*\)?/g,
        (_match, visibilityPrefix, authMethodExpression) =>
          `${visibilityPrefix}${authMethodExpression}=== \`chatgpt\``,
        "model visibility",
      );
    }
    if (
      source.includes("isServiceTierAllowed") &&
      source.includes("featureRequirements?.fast_mode") &&
      source.includes("authMethod:")
    ) {
      // Model serviceTiers are the authority for whether the control exists.
      // Account requirements and their loading state must never hide it.
      patched = replaceUniqueRendererGate(
        patched,
        /(\b([$A-Z_a-z][$\w]*)\s*=\s*)([$A-Z_a-z][$\w]*)\s*&&\s*!([$A-Z_a-z][$\w]*)\s*&&\s*([$A-Z_a-z][$\w]*)\s*!=\s*null\s*&&\s*\5\?\.requirements\?\.featureRequirements\?\.fast_mode\s*!==\s*!1/g,
        (_match, assignment) => `${assignment}!0`,
        "service tier UI",
      );
    }
    if (
      source.includes("isServiceTierAllowed") &&
      source.includes("serviceTierForRequest:") &&
      source.includes("availableOptions:")
    ) {
      // Preserve the model-aware resolver but remove its entitlement argument.
      // This also covers builds where the permission provider above reshaped.
      patched = replaceUniqueRendererGate(
        patched,
        /(\?\s*)([$A-Z_a-z][$\w]*)\s*\?\s*([$A-Z_a-z][$\w]*)\s*:\s*null\s*:\s*([$A-Z_a-z][$\w]*)\(\s*([$A-Z_a-z][$\w]*)\s*,\s*\3\s*,\s*\2\s*\)/g,
        (_match, _questionMark, _isAllowedName, tierName, resolverName, modelName) =>
          `?${tierName}:${resolverName}(${modelName},${tierName})`,
        "service tier selection permission",
      );
      // Reuse Codex's normalized selected tier for the request too. A Fast tier
      // left over from another model must become null after switching to a
      // model whose serviceTiers do not contain it.
      patched = replaceUniqueRendererGate(
        patched,
        /(\b([$A-Z_a-z][$\w]*)\s*=\s*([$A-Z_a-z][$\w]*)\s*==\s*null\s*\?\s*null\s*:\s*([$A-Z_a-z][$\w]*)\(\s*([$A-Z_a-z][$\w]*)\s*,\s*\3\s*\))(?=\s*;\s*let\s+[$A-Z_a-z][$\w]*\s*=\s*[$A-Z_a-z][$\w]*\(\s*\3\s*\?\?\s*null\s*\))/g,
        (_match, selectedExpression, selectedName, requestTierName) =>
          `${selectedExpression},${requestTierName}=${selectedName}`,
        "service tier model validation",
      );
      // Requirements can remain pending independently of the model catalog.
      // Do not report that entitlement fetch as service-tier option loading.
      patched = replaceUniqueRendererGate(
        patched,
        /(\b([$A-Z_a-z][$\w]*)\s*=\s*([$A-Z_a-z][$\w]*)\.isLoading\s*\|\|\s*([$A-Z_a-z][$\w]*)\s*\|\|\s*([$A-Z_a-z][$\w]*)\.isLoading)\s*\|\|\s*[$A-Z_a-z][$\w]*\s*==\s*null\s*&&\s*[$A-Z_a-z][$\w]*(?=\s*,)/g,
        (_match, modelLoadingExpression) => modelLoadingExpression,
        "service tier entitlement loading",
      );
    }
    if (
      source.includes("composer.toggleFastMode") &&
      source.includes("isServiceTierAllowed") &&
      source.includes("availableOptions.length")
    ) {
      // The current model's options decide whether the speed control exists.
      patched = replaceNearestRendererGateBeforeAnchor(
        patched,
        [
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)\(?\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1\s*\)?\s*&&\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*[$A-Z_a-z][$\w]*(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (_match, assignment, _resultName, settingsName, draftName) =>
              `${assignment}${settingsName}.availableOptions.length>1&&!${draftName}`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)\(?\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1\s*\)?\s*&&\s*[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (_match, assignment, _resultName, settingsName, draftName) =>
              `${assignment}${settingsName}.availableOptions.length>1&&!${draftName}`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)\(?\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1\s*\)?\s*&&\s*[$A-Z_a-z][$\w]*(?!\s*&&\s*!)(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (_match, assignment, _resultName, settingsName) =>
              `${assignment}${settingsName}.availableOptions.length>1`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*)\(?\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1\s*\)?\s*&&\s*[$A-Z_a-z][$\w]*(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (
              _match,
              preservedPrefix,
              _resultName,
              _draftName,
              settingsName,
            ) => `${preservedPrefix}${settingsName}.availableOptions.length>1`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*\(?\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1\s*\)?(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (
              _match,
              assignment,
              _resultName,
              draftName,
              settingsName,
            ) => `${assignment}!${draftName}&&${settingsName}.availableOptions.length>1`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*\(?\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1\s*\)?\s*&&\s*!\s*([$A-Z_a-z][$\w]*)(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (
              _match,
              assignment,
              _resultName,
              settingsName,
              draftName,
            ) => `${assignment}${settingsName}.availableOptions.length>1&&!${draftName}`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (_match, assignment, _resultName, settingsName) =>
              `${assignment}${settingsName}.availableOptions.length>1`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*)[$A-Z_a-z][$\w]*\s*&&\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1(?=\s*[,;][\s\S]{0,8192}?`composer\.toggleFastMode`)/g,
            replacement: (
              _match,
              preservedPrefix,
              _resultName,
              _draftName,
              settingsName,
            ) => `${preservedPrefix}${settingsName}.availableOptions.length>1`,
          },
        ],
        undefined,
        "model-aware service tier control",
        "`composer.toggleFastMode`",
        8192,
      );
      if (source.includes("!=null")) {
        patched = replaceUniqueRendererGate(
          patched,
          [
            {
              pattern: /(`composer\.toggleFastMode`[\s\S]{0,4096}?\{\s*enabled\s*:\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*([$A-Z_a-z][$\w]*)\s*!=\s*null/g,
              replacement: (_match, prefix, loadingName, fastOptionName) =>
                `${prefix}!${loadingName}&&${fastOptionName}!=null`,
            },
            {
              pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*([$A-Z_a-z][$\w]*)\s*!=\s*null(?=\s*[,;][\s\S]{0,4096}?\{\s*enabled\s*:\s*\2\s*\}[\s\S]{0,4096}?`composer\.toggleFastMode`)/g,
              replacement: (
                _match,
                preservedPrefix,
                _resultName,
                _draftName,
                loadingName,
                fastOptionName,
              ) => `${preservedPrefix}!${loadingName}&&${fastOptionName}!=null`,
            },
          ],
          undefined,
          "model-aware Fast toggle",
        );
      }
    }
    if (
      source.includes("composer.speedSlashCommand.disableDescription") &&
      source.includes("isServiceTierAllowed") &&
      source.includes("availableOptions.map")
    ) {
      // These commands are created only for service tiers exposed by the model.
      patched = replaceUniqueRendererGate(
        patched,
        /(enabled\s*:\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)\.isLoading(?=\s*,\s*isSelected\s*:)/g,
        (_match, assignment, settingsName) =>
          `${assignment}!${settingsName}.isLoading`,
        "model-aware service tier commands",
      );
    }
    if (
      source.includes("isServiceTierAllowed") &&
      /availableOptions\.length\s*<=\s*1/.test(source) &&
      source.includes("selectedServiceTier")
    ) {
      patched = replaceUniqueRendererGate(
        patched,
        /if\s*\(\s*!\s*([$A-Z_a-z][$\w]*)\s*\|\|\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*<=\s*1\s*\)\s*return\s+null/g,
        (_match, _isAllowedName, settingsName) =>
          `if(${settingsName}.availableOptions.length<=1)return null`,
        "service tier settings UI",
      );
    }
    if (
      source.includes("Failed to load config requirements for service tier") &&
      source.includes("featureRequirements?.fast_mode")
    ) {
      // A tier selected from the current model must not be stripped from thread
      // requests by an account entitlement lookup.
      patched = replaceUniqueRendererGate(
        patched,
        /if\s*\(\s*\(\s*await\s+([$A-Z_a-z][$\w]*)\(\s*\)\s*\)\.requirements\?\.featureRequirements\?\.fast_mode\s*===\s*!1\s*\)\s*return\s+null/g,
        "",
        "service tier request sanitizer",
      );
    }
    if (
      source.includes("Failed to read service tier for request") &&
      source.includes("featureRequirements?.fast_mode")
    ) {
      patched = replaceUniqueRendererGate(
        patched,
        /async\s+function\s+([$A-Z_a-z][$\w]*)\(\s*([$A-Z_a-z][$\w]*)\s*,\s*([$A-Z_a-z][$\w]*)\s*\)\s*\{\s*let\s+([$A-Z_a-z][$\w]*)\s*=\s*await\s+[$A-Z_a-z][$\w]*\(\s*\2\s*,\s*\3\s*\)\s*;\s*if\s*\(\s*\4\s*!==\s*`chatgpt`\s*\)\s*return\s*!1\s*;[\s\S]{0,768}?\.requirements\?\.featureRequirements\?\.fast_mode\s*!==\s*!1\s*\}/g,
        (_match, functionName, firstArgumentName, secondArgumentName) =>
          `async function ${functionName}(${firstArgumentName},${secondArgumentName}){return!0}`,
        "service tier request entitlement",
      );
    }
    if (
      source.includes("composer.intelligenceDropdown.model.title") &&
      source.includes("composer.intelligenceDropdown.model.rowLabel") &&
      source.includes("modelPickerTriggerConfig:") &&
      source.includes("selectedServiceTierIconKind:") &&
      source.includes("showFastServiceTierIndicator:")
    ) {
      // Third-party catalogs can expose fewer power selections than Codex's
      // native threshold even though model, effort, and Fast are all available.
      // Keep the modern native trigger in that case: it owns the filled Fast
      // indicator and avoids falling back to the legacy outlined model icon.
      patched = replaceUniqueRendererGate(
        patched,
        [
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)(?=\s*,[\s\S]{0,8192}?modelPickerTriggerConfig\s*:\s*\2\s*\?)/g,
            replacement: (
              _match,
              assignment,
              _triggerConfigName,
              hideLabelName,
            ) => `${assignment}!${hideLabelName}`,
          },
          {
            pattern: /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)(?=\s*,[\s\S]{0,4096}?\b([$A-Z_a-z][$\w]*)\s*=\s*\2\s*\?\s*\{[\s\S]{0,1024}?selectedServiceTierIconKind\s*:[\s\S]{0,1024}?showFastServiceTierIndicator\s*:[\s\S]{0,8192}?modelPickerTriggerConfig\s*:\s*\4\b)/g,
            replacement: (
              _match,
              assignment,
              _triggerConfigName,
              hideLabelName,
            ) => `${assignment}!${hideLabelName}`,
          },
        ],
        undefined,
        "fast model trigger availability",
      );
      // Preserve Codex's native Fast indicators. Its own model/tier support
      // checks already prevent them from appearing on unsupported models.
      patched = replaceUniqueRendererGate(
        patched,
        /(modelPickerTriggerConfig\s*:\s*([$A-Z_a-z][$\w]*)\s*[,}][\s\S]{0,2048}?selectedServiceTierIconKind\s*:[\s\S]{0,12288}?)if\s*\(\s*[$A-Z_a-z][$\w]*\s*&&\s*\2\s*!=\s*null\s*\)|if\s*\(\s*[$A-Z_a-z][$\w]*\s*&&\s*modelPickerTriggerConfig\s*!=\s*null\s*\)/g,
        (_match, aliasedPrefix, triggerConfigName) =>
          aliasedPrefix == null
            ? "if(modelPickerTriggerConfig!=null)"
            : `${aliasedPrefix}if(${triggerConfigName}!=null)`,
        "fast model trigger fallback",
      );
    }
    if (
      source.includes("activeInteractions=new Map") &&
      source.includes("beginCpuSampling") &&
      source.includes(
        "ensureHeartbeat(){this.heartbeatTimer??=setInterval",
      ) &&
      source.includes("rendererProcessCpuPercentAvg")
    ) {
      // Codey launches app-server with analytics.enabled=false, so renderer
      // interaction telemetry is discarded after paying for main/renderer CPU
      // snapshots and a 1 Hz heartbeat. Preserve span lifecycle semantics while
      // removing only those two recurring/IPC costs.
      patched = replaceUniqueRendererGate(
        patched,
        /cpuSampling:([$A-Z_a-z][$\w]*)===`dropped`\|\|([$A-Z_a-z][$\w]*)\.backfilled===!0\?null:this\.beginCpuSampling\(\)/g,
        "cpuSampling:null",
        "interaction CPU sampling",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /ensureHeartbeat\(\)\{this\.heartbeatTimer\?\?=setInterval\(\(\)=>\{let ([$A-Z_a-z][$\w]*)=this\.now\(\),([$A-Z_a-z][$\w]*)=this\.wallNow\(\);for\(let ([$A-Z_a-z][$\w]*) of this\.activeInteractions\.values\(\)\)this\.recordHeartbeat\(\3,\1,\2\)\},([$A-Z_a-z][$\w]*)\)\}/g,
        "ensureHeartbeat(){}",
        "interaction heartbeat",
      );
    }
    return patched;
  };
  const discoveredCodexRendererAssets = new Set();
  const maximumDiscoveredCodexRendererAssets = 128;
  const rememberCodexRendererAsset = (baseUrl, specifier) => {
    try {
      const url = new URL(specifier, baseUrl);
      if (
        url.protocol !== "app:" ||
        !url.pathname.includes("/assets/") ||
        !/\.(?:c|m)?js$/i.test(url.pathname)
      ) return;
      discoveredCodexRendererAssets.delete(url.pathname);
      discoveredCodexRendererAssets.add(url.pathname);
      while (
        discoveredCodexRendererAssets.size >
        maximumDiscoveredCodexRendererAssets
      ) {
        const oldest = discoveredCodexRendererAssets.keys().next().value;
        if (oldest === undefined) break;
        discoveredCodexRendererAssets.delete(oldest);
      }
    } catch {}
  };
  const discoverCodexRendererAssets = (baseUrl, source) => {
    for (const match of source.matchAll(
      /\bsrc\s*=\s*(["'])([^"']+\.(?:c|m)?js(?:[?#][^"']*)?)\1/gi,
    )) rememberCodexRendererAsset(baseUrl, match[2]);
  };
  const isCodexRendererBootstrapRequest = (request) => {
    try {
      const url = new URL(request?.url);
      return url.protocol === "app:" && /\/index\.html$/i.test(url.pathname);
    } catch {
      return false;
    }
  };
  const isCodexRendererAssetRequest = (request) => {
    try {
      const url = new URL(request?.url);
      return (
        url.protocol === "app:" &&
        url.pathname.includes("/assets/") &&
        (
          /\/(?:(?:app-initial|codex-composer-adapter|general-settings|model-list-filter|windows-model-controls|use-service-tier-settings|read-service-tier-for-request|subagent-activity-chip-group)(?:[~-][^/]*)?)\.(?:c|m)?js$/i.test(
            url.pathname,
          ) ||
          (
            disablePet &&
            /\/(?:(?:appearance-settings|pet-settings|pets-settings)(?:[~-][^/]*)?)\.(?:c|m)?js$/i.test(
              url.pathname,
            )
          ) ||
          discoveredCodexRendererAssets.has(url.pathname)
        )
      );
    } catch {
      return false;
    }
  };
  const patchCodexRendererResponse = async (request, response) => {
    if (response?.ok !== true) return response;
    if (isCodexRendererBootstrapRequest(request)) {
      try {
        discoverCodexRendererAssets(request.url, await response.clone().text());
      } catch (error) {
        recordCodeyPatchFailure("renderer_patch:asset discovery", error, {
          requestUrl: request?.url,
        });
      }
      return response;
    }
    if (!isCodexRendererAssetRequest(request)) return response;
    let source;
    try {
      source = await response.clone().text();
    } catch (error) {
      recordCodeyPatchFailure("renderer_patch:asset read", error, {
        requestUrl: request?.url,
      });
      return response;
    }
    let patched;
    const previousRendererPatchFailures = activeRendererPatchFailures;
    activeRendererPatchFailures = rendererPatchFailuresForSource(source);
    try {
      patched = patchCodexRendererAsset(source);
    } catch (error) {
      // Codex renderer bundles are minified implementation details and their
      // shapes change between releases. These UI restorations are optional:
      // never turn a stale patch anchor into a failed app:// module request,
      // otherwise Codex remains on its static startup loader forever.
      recordCodeyPatchFailure("patch_codex_renderer_asset", error, {
        requestUrl: request?.url,
      });
      try {
        console.error("Codey skipped an incompatible Codex renderer patch", error);
      } catch {}
      return response;
    } finally {
      activeRendererPatchFailures = previousRendererPatchFailures;
    }
    if (patched === source) return response;
    const headers = new Headers(response.headers);
    for (const header of [
      "content-encoding",
      "content-length",
      "content-md5",
      "digest",
      "etag",
      "last-modified",
    ]) headers.delete(header);
    return new Response(patched, {
      headers,
      status: response.status,
      statusText: response.statusText,
    });
  };

  // The inspector is only a startup injection mechanism. Do not pass its
  // pause state or command-line flags to Codex workers.
  process.execArgv.splice(
    0,
    process.execArgv.length,
    ...process.execArgv.filter((argument) => !isInspectorArgument(argument)),
  );
  process.argv.splice(
    0,
    process.argv.length,
    ...process.argv.filter((argument) => !isInspectorArgument(argument)),
  );

  // The desktop client explicitly opts the bundled app-server into analytics.
  // Remove that opt-in and add a command-local config override without touching
  // the user's persistent Codex configuration.
  const appServerAnalyticsConfig = "analytics.enabled=false";
  const codeyRuntimeConfigOverrides = "__CODEY_RUNTIME_CONFIG_OVERRIDES__";
  const wslOnlyRuntimeOverridePrefix = "__CODEY_WSL_ONLY__:";
  const validRuntimeConfigOverrides = Array.isArray(codeyRuntimeConfigOverrides)
    ? codeyRuntimeConfigOverrides.filter(
        (entry) => typeof entry === "string" && entry.length > 0,
      )
    : [];
  const nativeRuntimeConfigOverrides = validRuntimeConfigOverrides.filter(
    (entry) => !entry.startsWith(wslOnlyRuntimeOverridePrefix),
  );
  const wslOnlyRuntimeConfigOverrides = validRuntimeConfigOverrides
    .filter((entry) => entry.startsWith(wslOnlyRuntimeOverridePrefix))
    .map((entry) => entry.slice(wslOnlyRuntimeOverridePrefix.length));
  const runtimeOverrideKey = (config) => {
    if (typeof config !== "string") return "";
    const separatorIndex = config.indexOf("=");
    return (separatorIndex < 0 ? config : config.slice(0, separatorIndex)).trim();
  };
  const uniqueRuntimeConfigsByKey = (configs) => {
    const uniqueConfigs = [];
    const indexesByKey = new Map();
    for (const config of configs) {
      const key = runtimeOverrideKey(config);
      if (key.length === 0) continue;
      const existingIndex = indexesByKey.get(key);
      if (existingIndex == null) {
        indexesByKey.set(key, uniqueConfigs.length);
        uniqueConfigs.push(config);
      } else {
        uniqueConfigs[existingIndex] = config;
      }
    }
    return uniqueConfigs;
  };
  const appServerRuntimeConfigs = uniqueRuntimeConfigsByKey([
    appServerAnalyticsConfig,
    ...nativeRuntimeConfigOverrides.filter(
      (config) => runtimeOverrideKey(config) !== runtimeOverrideKey(appServerAnalyticsConfig),
    ),
  ]);
  const appServerRuntimeOverrideVerifiedResult =
    "codey-app-server-runtime-overrides-verified";
  const appServerRuntimeOverrideTimeoutMs = 20_000;
  const appServerRuntimeOverrideEvidence = {
    version: 1,
    observed: false,
    complete: appServerRuntimeConfigs.length === 0,
    attempts: 0,
    mode: "",
    command: "",
    argumentCount: 0,
    missingRuntimeConfigs: [...appServerRuntimeConfigs],
    requiredRuntimeConfigs: [...appServerRuntimeConfigs],
  };
  let resolveAppServerRuntimeOverrideValidation = null;
  const appServerRuntimeOverrideValidationPromise = new Promise((resolve) => {
    resolveAppServerRuntimeOverrideValidation = resolve;
  });
  const formatAppServerRuntimeOverrideError = (status) => {
    const missing = status.missingRuntimeConfigs?.length
      ? `；缺失：${status.missingRuntimeConfigs
          .map(runtimeOverrideKey)
          .join(", ")}`
      : "";
    const observed = status.observed
      ? `；已观察到 ${status.mode || "unknown"} 启动：${status.command || ""}（参数 ${status.argumentCount ?? 0} 个）`
      : "；未观察到 app-server 启动调用";
    return (
      "当前 Codex 版本的 app-server 启动参数结构与 Codey 不兼容，" +
      `未能确认注入 model_provider=codey_router 与 model_providers.codey_router.*${missing}${observed}`
    );
  };
  const finishAppServerRuntimeOverrideValidation = (status) => {
    if (appServerRuntimeOverrideEvidence.complete) return;
    Object.assign(appServerRuntimeOverrideEvidence, status);
    if (status.complete) {
      appServerRuntimeOverrideEvidence.complete = true;
      resolveAppServerRuntimeOverrideValidation?.(
        appServerRuntimeOverrideVerifiedResult,
      );
      return;
    }
    resolveAppServerRuntimeOverrideValidation?.(status);
  };
  const collectRuntimeConfigArgsAfterAppServer = (args) => {
    const appServerIndex = args.indexOf("app-server");
    if (appServerIndex < 0) return [];
    const configs = [];
    for (let index = appServerIndex + 1; index < args.length; index += 1) {
      const argument = args[index];
      if (
        (argument === "-c" || argument === "--config") &&
        typeof args[index + 1] === "string"
      ) {
        configs.push(args[index + 1]);
        index += 1;
        continue;
      }
      if (typeof argument === "string" && argument.startsWith("--config=")) {
        configs.push(argument.slice("--config=".length));
      }
    }
    return configs;
  };
  const validateRuntimeConfigSet = (configs, requiredConfigs) => {
    const observed = new Set(configs);
    return requiredConfigs.filter((config) => !observed.has(config));
  };
  const recordCodexAppServerRuntimeOverrideAttempt = (status) => {
    const normalized = {
      version: 1,
      observed: true,
      complete: status.missingRuntimeConfigs.length === 0,
      attempts: appServerRuntimeOverrideEvidence.attempts + 1,
      mode: status.mode,
      command: String(status.command ?? "").slice(0, 512),
      argumentCount: Array.isArray(status.args) ? status.args.length : 0,
      missingRuntimeConfigs: status.missingRuntimeConfigs,
      requiredRuntimeConfigs: [...status.requiredRuntimeConfigs],
    };
    finishAppServerRuntimeOverrideValidation(normalized);
  };
  const inspectCodexAppServerRuntimeOverrides = (command, args) => {
    if (!Array.isArray(args)) return null;
    const commandName = String(command ?? "");
    const appServerArgCount = args
      .filter((argument) => argument === "app-server")
      .length;
    const directCodexCommand = /(?:^|[/\\])codex(?:\.exe)?$/i.test(commandName);
    const runtimeManagedAppServer =
      nativeRuntimeConfigOverrides.length > 0 && appServerArgCount === 1;
    if (
      appServerArgCount === 1 &&
      (directCodexCommand || runtimeManagedAppServer)
    ) {
      const configs = collectRuntimeConfigArgsAfterAppServer(args);
      return {
        mode: "argv",
        command,
        args,
        requiredRuntimeConfigs: appServerRuntimeConfigs,
        missingRuntimeConfigs: validateRuntimeConfigSet(
          configs,
          appServerRuntimeConfigs,
        ),
      };
    }
    if (!/(?:^|[/\\])wsl(?:\.exe)?$/i.test(commandName)) return null;
    const shellFlagIndexes = args
      .map((argument, index) => argument === "-lc" ? index : -1)
      .filter((index) => index >= 0);
    if (shellFlagIndexes.length !== 1) return null;
    const shellFlagIndex = shellFlagIndexes[0];
    const shellCommand = args[shellFlagIndex + 1];
    if (
      !/(?:^|[/\\])bash$/i.test(String(args[shellFlagIndex - 1] ?? "")) ||
      typeof shellCommand !== "string"
    ) {
      return null;
    }
    const execMatches = [...shellCommand.matchAll(/(?:^|;)\s*exec\s+/g)];
    if (execMatches.length !== 1) return null;
    const execCommandOffset = execMatches[0].index + execMatches[0][0].length;
    const execCommand = shellCommand.slice(execCommandOffset);
    const executableToken = /^(?:"[^"]+"|'[^']+'|(?:\\.|[^\s;&|])+)/.exec(
      execCommand,
    )?.[0];
    if (executableToken == null) return null;
    const normalizedExecutable = executableToken
      .replace(/^(["'])|(["'])$/g, "")
      .replace(/\\ /g, " ");
    if (!/(?:^|[/\\])codex(?:\.exe)?$/i.test(normalizedExecutable)) {
      return null;
    }
    const appServerOffset = execCommand.search(/\bapp-server\b/);
    if (appServerOffset < 0) return null;
    const afterAppServer = execCommand.slice(
      appServerOffset + "app-server".length,
    );
    const wslReplacementKeys = new Set(
      wslOnlyRuntimeConfigOverrides.map(runtimeOverrideKey),
    );
    const requiredRuntimeConfigs = uniqueRuntimeConfigsByKey([
      appServerAnalyticsConfig,
      ...nativeRuntimeConfigOverrides.filter(
        (config) =>
          runtimeOverrideKey(config) !== runtimeOverrideKey(appServerAnalyticsConfig) &&
          !wslReplacementKeys.has(runtimeOverrideKey(config)),
      ),
      ...wslOnlyRuntimeConfigOverrides,
    ]).map(rewriteTomlWindowsPathsForWsl);
    return {
      mode: "wsl-shell",
      command,
      args,
      requiredRuntimeConfigs,
      missingRuntimeConfigs: requiredRuntimeConfigs.filter(
        (config) => !hasShellConfigArg(afterAppServer, config),
      ),
    };
  };
  const awaitCodexAppServerRuntimeOverrides = async () => {
    if (appServerRuntimeOverrideEvidence.complete) {
      return appServerRuntimeOverrideVerifiedResult;
    }
    if (appServerRuntimeOverrideEvidence.observed) {
      throw new Error(
        formatAppServerRuntimeOverrideError(appServerRuntimeOverrideEvidence),
      );
    }
    let timeout = null;
    try {
      const result = await Promise.race([
        appServerRuntimeOverrideValidationPromise,
        new Promise((_resolve, reject) => {
          timeout = setTimeout(() => {
            reject(
              new Error(
                formatAppServerRuntimeOverrideError(
                  appServerRuntimeOverrideEvidence,
                ),
              ),
            );
          }, appServerRuntimeOverrideTimeoutMs);
          timeout.unref?.();
        }),
      ]);
      if (result === appServerRuntimeOverrideVerifiedResult) return result;
      throw new Error(formatAppServerRuntimeOverrideError(result));
    } finally {
      if (timeout != null) clearTimeout(timeout);
      setImmediate(() => {
        try { process.getBuiltinModule("inspector").close(); } catch {}
      });
    }
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__",
    {
      configurable: false,
      value: awaitCodexAppServerRuntimeOverrides,
      writable: false,
    },
  );
  const subagentGateRuntimeEnv = "CODEY_SUBAGENT_GATE_ACTIVE";
  const subagentGateRuntimeIdEnv = "CODEY_SUBAGENT_GATE_RUNTIME_ID";
  const subagentGateRuntimeActive =
    typeof __SUBAGENT_GATE_ACTIVE__ === "boolean" &&
    __SUBAGENT_GATE_ACTIVE__;
  const randomUuid = process.getBuiltinModule("crypto")?.randomUUID;
  const subagentGateRuntimeId = typeof randomUuid === "function"
    ? randomUuid()
    : `${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  const rewriteTomlWindowsPathsForWsl = (config) => {
    if (typeof config !== "string") return config;
    return config.replace(/"(?:\\.|[^"\\])*"/g, (literal) => {
      try {
        const value = JSON.parse(literal);
        const match = /^(['"]?)([A-Za-z]):[\\/](.*)$/s.exec(value);
        if (match == null) return literal;
        const [, quote, drive, rest] = match;
        return JSON.stringify(
          `${quote}/mnt/${drive.toLowerCase()}/${rest.replace(/\\/g, "/")}`,
        );
      } catch {
        return literal;
      }
    });
  };
  const rewriteCodexAppServerArgs = (args) => {
    if (!Array.isArray(args)) return args;
    const appServerIndexes = args
      .map((argument, index) => argument === "app-server" ? index : -1)
      .filter((index) => index >= 0);
    if (appServerIndexes.length !== 1) return args;

    const managedConfigKeys = new Set(
      appServerRuntimeConfigs.map(runtimeOverrideKey),
    );
    const rewritten = [];
    for (let index = 0; index < args.length; index += 1) {
      const argument = args[index];
      if (argument === "--analytics-default-enabled") continue;
      if (
        (argument === "-c" || argument === "--config") &&
        typeof args[index + 1] === "string"
      ) {
        const config = args[index + 1];
        if (managedConfigKeys.has(runtimeOverrideKey(config))) {
          index += 1;
          continue;
        }
        rewritten.push(argument, config);
        index += 1;
        continue;
      }
      if (typeof argument === "string" && argument.startsWith("--config=")) {
        const config = argument.slice("--config=".length);
        if (managedConfigKeys.has(runtimeOverrideKey(config))) continue;
      }
      rewritten.push(argument);
    }
    const appServerIndex = rewritten.indexOf("app-server");
    // Keep Codey's overrides in the app-server command's own config layer.
    // The desktop app appends mcp_servers.codex_app after the subcommand; placing
    // Codey's mcp_servers entries in the global layer lets that later table mask
    // FastCtx and the subagent-control server.
    rewritten.splice(
      appServerIndex + 1,
      0,
      ...appServerRuntimeConfigs.flatMap((config) => ["-c", config]),
    );
    if (
      rewritten.length === args.length &&
      rewritten.every((argument, index) => argument === args[index])
    ) {
      return args;
    }
    return rewritten;
  };
  const shellQuote = (value) => `'${String(value).replace(/'/g, "'\\''")}'`;
  const escapeRegExp = (value) =>
    String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const hasShellConfigArg = (command, config) => {
    const forms = [config, shellQuote(config)];
    return forms.some((form) => {
      const escaped = escapeRegExp(form);
      return new RegExp(
        `(?:^|[\\s;])(?:-c|--config)\\s+${escaped}(?=$|[\\s;&|])`,
      ).test(command) || new RegExp(
        `(?:^|[\\s;])--config=${escaped}(?=$|[\\s;&|])`,
      ).test(command);
    });
  };
  const rewriteCodexAppServerShellCommand = (
    command,
    runtimeConfigs = appServerRuntimeConfigs,
  ) => {
    if (typeof command !== "string") return command;
    const execMatches = [...command.matchAll(/(?:^|;)\s*exec\s+/g)];
    if (execMatches.length !== 1) return command;
    const execMatch = execMatches[0];
    const execCommandOffset = execMatch.index + execMatch[0].length;
    const execCommand = command.slice(execCommandOffset);
    const executableToken = /^(?:"[^"]+"|'[^']+'|(?:\\.|[^\s;&|])+)/.exec(
      execCommand,
    )?.[0];
    if (executableToken == null) return command;
    const normalizedExecutable = executableToken
      .replace(/^(["'])|(["'])$/g, "")
      .replace(/\\ /g, " ");
    if (!/(?:^|[/\\])codex(?:\.exe)?$/i.test(normalizedExecutable)) {
      return command;
    }

    const appServerMatches = execCommand.match(/\bapp-server\b/g);
    if (appServerMatches?.length !== 1) {
      return command;
    }

    let rewritten = execCommand.replace(
      /(^|[\s;])(-c|--config)\s+analytics\.enabled=[^\s;&|]+(?=$|[\s;&|])/g,
      (_match, prefix) => prefix,
    );
    rewritten = rewritten.replace(
      /(^|[\s;])--config=analytics\.enabled=[^\s;&|]+(?=$|[\s;&|])/g,
      (_match, prefix) => prefix,
    );
    rewritten = rewritten.replace(
      /(^|[\s;])--analytics-default-enabled(?=$|[\s;&|])/g,
      (_match, prefix) => prefix,
    );
    const rewrittenAppServerOffset = rewritten.search(/\bapp-server\b/);
    const afterAppServer = rewritten.slice(
      rewrittenAppServerOffset + "app-server".length,
    );
    const injectedConfigs = runtimeConfigs.filter(
      (config) => !hasShellConfigArg(afterAppServer, config),
    );
    if (injectedConfigs.length > 0) {
      rewritten = rewritten.replace(
        /\bapp-server\b/,
        `app-server ${injectedConfigs.map((config) => `-c ${shellQuote(config)}`).join(" ")}`,
      );
    }
    let commandPrefix = command.slice(0, execCommandOffset);
    if (
      subagentGateRuntimeActive &&
      !commandPrefix.includes(`${subagentGateRuntimeEnv}=1 `)
    ) {
      const execKeywordIndex = commandPrefix.lastIndexOf("exec");
      commandPrefix =
        commandPrefix.slice(0, execKeywordIndex) +
        `${subagentGateRuntimeIdEnv}=${shellQuote(subagentGateRuntimeId)} ` +
        `${subagentGateRuntimeEnv}=1 ` +
        commandPrefix.slice(execKeywordIndex);
    }
    return commandPrefix + rewritten;
  };
  const rewriteCodexAppServerSpawnArgs = (command, args) => {
    if (!Array.isArray(args)) return args;
    const commandName = String(command ?? "");
    const appServerArgCount = args
      .filter((argument) => argument === "app-server")
      .length;
    const directCodexCommand = /(?:^|[/\\])codex(?:\.exe)?$/i.test(commandName);
    const runtimeManagedAppServer =
      nativeRuntimeConfigOverrides.length > 0 && appServerArgCount === 1;
    if (
      appServerArgCount === 1 &&
      (directCodexCommand || runtimeManagedAppServer)
    ) {
      return rewriteCodexAppServerArgs(args);
    }
    if (!/(?:^|[/\\])wsl(?:\.exe)?$/i.test(commandName)) return args;

    const shellFlagIndexes = args
      .map((argument, index) => argument === "-lc" ? index : -1)
      .filter((index) => index >= 0);
    if (shellFlagIndexes.length !== 1) return args;
    const shellFlagIndex = shellFlagIndexes[0];
    if (
      !/(?:^|[/\\])bash$/i.test(String(args[shellFlagIndex - 1] ?? "")) ||
      typeof args[shellFlagIndex + 1] !== "string"
    ) {
      return args;
    }
    const wslReplacementKeys = new Set(
      wslOnlyRuntimeConfigOverrides.map(runtimeOverrideKey),
    );
    const wslRuntimeConfigs = uniqueRuntimeConfigsByKey([
      appServerAnalyticsConfig,
      ...nativeRuntimeConfigOverrides.filter(
        (config) =>
          runtimeOverrideKey(config) !== runtimeOverrideKey(appServerAnalyticsConfig) &&
          !wslReplacementKeys.has(runtimeOverrideKey(config)),
      ),
      ...wslOnlyRuntimeConfigOverrides,
    ]).map(rewriteTomlWindowsPathsForWsl);
    const rewrittenCommand = rewriteCodexAppServerShellCommand(
      args[shellFlagIndex + 1],
      wslRuntimeConfigs,
    );
    if (rewrittenCommand === args[shellFlagIndex + 1]) return args;
    const rewritten = [...args];
    rewritten[shellFlagIndex + 1] = rewrittenCommand;
    return rewritten;
  };
  Object.defineProperty(globalThis, "__CODEY_REWRITE_CODEX_APP_SERVER_ARGS__", {
    configurable: false,
    value: rewriteCodexAppServerSpawnArgs,
    writable: false,
  });

  let appServerAnalyticsPatchCount = 0;
  const childProcess = process.getBuiltinModule("child_process");
  const NativeSpawn = childProcess.spawn;
  if (!NativeSpawn.__codeyAppServerAnalyticsDisabled) {
    const isManagedCodexAppServerSpawn = (command, args) =>
      subagentGateRuntimeActive &&
      Array.isArray(args) &&
      args.filter((argument) => argument === "app-server").length === 1 &&
      (
        /(?:^|[/\\])codex(?:\.exe)?$/i.test(String(command ?? "")) ||
        nativeRuntimeConfigOverrides.length > 0
      );
    const withSubagentGateEnvironment = (rest) => {
      const options = rest[0];
      if (options == null) {
        return [{
          env: {
            ...process.env,
            [subagentGateRuntimeEnv]: "1",
            [subagentGateRuntimeIdEnv]: subagentGateRuntimeId,
          },
        }];
      }
      if (typeof options !== "object" || Array.isArray(options)) return rest;
      const inheritedEnvironment = options.env == null ? process.env : options.env;
      return [{
        ...options,
        env: {
          ...inheritedEnvironment,
          [subagentGateRuntimeEnv]: "1",
          [subagentGateRuntimeIdEnv]: subagentGateRuntimeId,
        },
      }, ...rest.slice(1)];
    };
    const codeyAnalyticsDisabledSpawn = function (command, args, ...rest) {
      const rewritten = rewriteCodexAppServerSpawnArgs(command, args);
      const rewrittenRest = isManagedCodexAppServerSpawn(command, rewritten)
        ? withSubagentGateEnvironment(rest)
        : rest;
      const runtimeOverrideStatus = inspectCodexAppServerRuntimeOverrides(
        command,
        rewritten,
      );
      if (runtimeOverrideStatus != null) {
        recordCodexAppServerRuntimeOverrideAttempt(runtimeOverrideStatus);
      }
      if (rewritten === args && rewrittenRest === rest) {
        return Reflect.apply(NativeSpawn, this, arguments);
      }
      if (rewritten !== args) appServerAnalyticsPatchCount += 1;
      return Reflect.apply(NativeSpawn, this, [
        command,
        rewritten,
        ...rewrittenRest,
      ]);
    };
    Object.defineProperty(
      codeyAnalyticsDisabledSpawn,
      "__codeyAppServerAnalyticsDisabled",
      { value: true },
    );
    childProcess.spawn = codeyAnalyticsDisabledSpawn;
  }

  const externalPluginFocusReconcileMinIntervalMs = 30_000;
  let externalPluginFocusReconcileSuppressedCount = 0;
  const throttleExternalPluginFocusReconcile = (
    listener,
    minimumIntervalMs = externalPluginFocusReconcileMinIntervalMs,
  ) => {
    const monotonicNow = () => globalThis.performance?.now?.() ?? Date.now();
    let lastRunAt = Number.NEGATIVE_INFINITY;
    let trailingTimer = null;
    let trailingThis = null;
    let trailingArgs = null;
    const invoke = (receiver, args) => {
      lastRunAt = monotonicNow();
      trailingThis = null;
      trailingArgs = null;
      return Reflect.apply(listener, receiver, args);
    };
    const wrapped = function (...args) {
      const elapsed = monotonicNow() - lastRunAt;
      if (trailingTimer == null && elapsed >= minimumIntervalMs) {
        return invoke(this, args);
      }
      externalPluginFocusReconcileSuppressedCount += 1;
      trailingThis = this;
      trailingArgs = args;
      if (trailingTimer == null) {
        trailingTimer = setTimeout(() => {
          trailingTimer = null;
          invoke(trailingThis, trailingArgs ?? []);
        }, Math.max(1, minimumIntervalMs - elapsed));
        trailingTimer.unref?.();
      }
      return undefined;
    };
    Object.defineProperty(wrapped, "cancel", {
      configurable: false,
      value: () => {
        if (trailingTimer != null) clearTimeout(trailingTimer);
        trailingTimer = null;
        trailingThis = null;
        trailingArgs = null;
      },
      writable: false,
    });
    return wrapped;
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__",
    {
      configurable: false,
      value: throttleExternalPluginFocusReconcile,
      writable: false,
    },
  );
  const patchCodexMainFocusReconcile = (source) => {
    if (
      !source.includes("browser-window-focus") ||
      !source.includes("reconcileExternalPluginState")
    ) {
      throw new Error("Codey external plugin focus reconcile anchors not found");
    }
    let listenerName = null;
    let count = 0;
    let patched = source.replace(
      /(\b[$A-Z_a-z][$\w]*)=\(\)=>\{([$A-Z_a-z][$\w]*)\.reconcileExternalPluginState\((`focus`|"focus"|'focus')\)\}/g,
      (_match, matchedListenerName, coordinatorName, focusLiteral) => {
        count += 1;
        listenerName = matchedListenerName;
        return (
          `${matchedListenerName}=globalThis.` +
          `__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__(` +
          `()=>{${coordinatorName}.reconcileExternalPluginState(${focusLiteral})})`
        );
      },
    );
    if (count !== 1) {
      throw new Error(
        `Codey external plugin focus reconcile matched ${count} times`,
      );
    }
    let cleanupCount = 0;
    patched = patched.replace(
      /(\b[$A-Z_a-z][$\w]*)\.add\(\(\)=>\{([$A-Z_a-z][$\w]*)\.app\.off\((`browser-window-focus`|"browser-window-focus"|'browser-window-focus'),([$A-Z_a-z][$\w]*)\)\}\)/g,
      (match, disposerName, appName, eventLiteral, cleanupListenerName) => {
        if (cleanupListenerName !== listenerName) return match;
        cleanupCount += 1;
        return (
          `${disposerName}.add(()=>{${appName}.app.off(` +
          `${eventLiteral},${cleanupListenerName}),${cleanupListenerName}.cancel?.()})`
        );
      },
    );
    if (cleanupCount !== 1) {
      throw new Error(
        `Codey external plugin focus reconcile cleanup matched ${cleanupCount} times`,
      );
    }
    return patched;
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_PATCH_CODEX_MAIN_FOCUS_RECONCILE__",
    {
      configurable: false,
      value: patchCodexMainFocusReconcile,
      writable: false,
    },
  );

  // Desktop CES telemetry has its own main-process transport and worker
  // transport. Disable the transport promise, worker bootstrap value, and the
  // later startup-config update explicitly so no events queue while app-server
  // configuration is still resolving.
  const patchCodexMainDesktopAnalytics = (source) => {
    let workerBootstrapCount = 0;
    let workerUpdateCount = 0;
    let mainTransportCount = 0;
    let patched = source.replace(
      /analyticsEnabled:([$A-Z_a-z][$\w]*)!=null&&\1\.analytics\?\.enabled!==!1/g,
      () => {
        workerBootstrapCount += 1;
        return "analyticsEnabled:!1";
      },
    );
    patched = patched.replace(
      /postMessage\(\{type:(`worker-analytics-enabled-update`|"worker-analytics-enabled-update"|'worker-analytics-enabled-update'),enabled:([$A-Z_a-z][$\w]*)\.analytics\?\.enabled!==!1\}\)/g,
      (_match, messageLiteral) => {
        workerUpdateCount += 1;
        return `postMessage({type:${messageLiteral},enabled:!1})`;
      },
    );
    patched = patched.replace(
      /analyticsEnabled:([$A-Z_a-z][$\w]*)\.get\(\)\.then\(([$A-Z_a-z][$\w]*)=>\2\.analytics\?\.enabled!==!1\)/g,
      () => {
        mainTransportCount += 1;
        return "analyticsEnabled:!1";
      },
    );
    if (
      workerBootstrapCount !== 1 ||
      workerUpdateCount !== 1 ||
      mainTransportCount !== 1
    ) {
      throw new Error(
        "Codey desktop analytics matches " +
        `${workerBootstrapCount}/${workerUpdateCount}/${mainTransportCount}`,
      );
    }
    return patched;
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__",
    {
      configurable: false,
      value: patchCodexMainDesktopAnalytics,
      writable: false,
    },
  );

  // Codex's sampler manager asks the focused renderer for a full diagnostic
  // app-state snapshot every 30 seconds, then only records it as a debug log and
  // Sentry breadcrumb. Keep renderer-ready and explicit trigger snapshots, but
  // remove the periodic diagnostic heartbeat.
  const patchCodexMainAppStateHeartbeat = (source) => {
    if (
      !source.includes("appStateHeartbeat") ||
      !source.includes("electron-app-state-snapshot-request")
    ) {
      throw new Error("Codey app-state heartbeat anchors not found");
    }
    let count = 0;
    const patched = source.replace(
      /this\.appStateHeartbeat=setInterval\(\(\)=>\{this\.requestAppStateSnapshot\((`heartbeat`|"heartbeat"|'heartbeat')\)\},[$A-Z_a-z][$\w]*\),this\.appStateHeartbeat\.unref\(\)/g,
      () => {
        count += 1;
        return "this.appStateHeartbeat=null";
      },
    );
    if (count !== 1) {
      throw new Error(`Codey app-state heartbeat matched ${count} times`);
    }
    return patched;
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_PATCH_CODEX_MAIN_APP_STATE_HEARTBEAT__",
    {
      configurable: false,
      value: patchCodexMainAppStateHeartbeat,
      writable: false,
    },
  );

  // Codex prewarms the shared avatar/voice overlay at startup by creating a
  // hidden BrowserWindow. In slim-pet mode the pet entry points are already
  // unavailable, so keep the manager and voice path intact but make prewarm a
  // no-op. Voice can still create the overlay on demand through the manager's
  // regular presentation path.
  const patchCodexAvatarOverlayPrewarm = (source) => {
    if (!disablePet) return source;
    let count = 0;
    let patched = "";
    let lastIndex = 0;
    const prewarmMethodPattern = /async\s+prewarm\s*\([^)]*\)\s*\{/g;
    for (const match of source.matchAll(prewarmMethodPattern)) {
      const bodyStart = match.index + match[0].length;
      // The native prewarm body is a flat minified method. Stop at its first
      // closing brace so an unrelated prewarm method cannot borrow semantic
      // anchors from a later class in the monolithic bundle.
      const bodyEnd = source.indexOf("}", bodyStart);
      if (bodyEnd < 0) continue;
      const bodyPreview = source.slice(
        bodyStart,
        Math.min(bodyEnd, bodyStart + 1600),
      );
      if (
        !bodyPreview.includes("this.windowVisibilitySequence") ||
        !bodyPreview.includes("this.openingWindowPromise") ||
        !bodyPreview.includes("this.isAppQuitting") ||
        !bodyPreview.includes("this.ensureWindow(") ||
        !bodyPreview.includes("this.positionWindow(")
      ) {
        continue;
      }
      count += 1;
      patched += source.slice(lastIndex, bodyStart) + "return;";
      lastIndex = bodyStart;
    }
    patched += source.slice(lastIndex);
    if (count !== 1) {
      throw new Error(`Codey avatar overlay prewarm matches ${count}`);
    }
    return patched;
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_PATCH_CODEX_AVATAR_OVERLAY_PREWARM__",
    {
      configurable: false,
      value: patchCodexAvatarOverlayPrewarm,
      writable: false,
    },
  );

  const workerThreads = process.getBuiltinModule("worker_threads");
  const NativeWorker = workerThreads.Worker;
  const windowsWmiSamplerSelfTest = Symbol("codey-wmi-sampler-self-test");
  if (!NativeWorker.__codeyNoInspectWrapper) {
    const EventEmitter = process.getBuiltinModule("events").EventEmitter;
    const maximumWmiWorkerSourceBytes = 2 * 1024 * 1024;
    const maximumWmiWorkerSourceCacheEntries = 256;
    const workerSourceMatchCache = new Map();
    const rememberWorkerSourceMatch = (key, value) => {
      if (!key) return;
      workerSourceMatchCache.delete(key);
      workerSourceMatchCache.set(key, value);
      while (
        workerSourceMatchCache.size > maximumWmiWorkerSourceCacheEntries
      ) {
        const oldestKey = workerSourceMatchCache.keys().next().value;
        if (oldestKey === undefined) break;
        workerSourceMatchCache.delete(oldestKey);
      }
    };
    const workerSpecifierText = (filename) => {
      if (typeof filename === "string") return filename;
      if (typeof filename?.href === "string") return filename.href;
      return String(filename ?? "");
    };
    const workerDisplayName = (filename, options) => {
      const rawSpecifier = workerSpecifierText(filename);
      if (options?.eval === true) return "eval-worker";
      if (/^data:/i.test(rawSpecifier)) return "data-worker";
      const specifier = rawSpecifier
        .replace(/[?#].*$/, "")
        .replace(/[/\\]+$/, "");
      const encodedName = specifier.split(/[/\\]/).at(-1) || "unknown-worker";
      try {
        return decodeURIComponent(encodedName).slice(0, 160);
      } catch {
        return encodedName.slice(0, 160);
      }
    };
    const isKnownWmiSnapshotWorkerName = (filename) =>
      /(?:^|[/\\])child[-_]process[-_]snapshot[-_]worker(?:[-.][^/\\?#]+)?\.(?:c?js|mjs)(?:[?#].*)?$/i
        .test(workerSpecifierText(filename));
    const isKnownWmiSnapshotWorkerThreadName = (options) =>
      typeof options?.name === "string" &&
      /^child[-_]process[-_]snapshot$/i.test(options.name.trim());
    const workerThreadName = (options) =>
      typeof options?.name === "string"
        ? options.name
            .replace(/[\u0000-\u001f\u007f]/g, " ")
            .trim()
            .slice(0, 80)
        : "";
    const wmiSnapshotSourceSignals = (source) => ({
      cim: /Get-(?:CimInstance|WmiObject)/i.test(source),
      win32Process: /\bWin32_Process\b/i.test(source),
      perfProcess:
        /\bWin32_Perf(?:Formatted|Raw)Data_PerfProc_Process\b/i.test(source),
      powershell: /\b(?:powershell|pwsh)(?:\.exe)?\b/i.test(source),
      workerMessaging:
        /(?:worker_threads|parentPort|postMessage|workerData)/.test(source),
    });
    const hasWmiSnapshotSourceSignature = (signals) =>
      Object.values(signals).every(Boolean);
    const decodeDataWorkerSource = (specifier) => {
      const commaIndex = specifier.indexOf(",");
      if (commaIndex < 0) return "";
      const metadata = specifier.slice(0, commaIndex);
      const payload = specifier.slice(commaIndex + 1);
      const source = /;base64(?:;|$)/i.test(metadata)
        ? Buffer.from(payload, "base64").toString("utf8")
        : decodeURIComponent(payload);
      return source.slice(0, maximumWmiWorkerSourceBytes);
    };
    const workerFilePath = (filename) => {
      const specifier = workerSpecifierText(filename);
      if (/^file:/i.test(specifier)) {
        const urlModule = process.getBuiltinModule("url");
        const url = new urlModule.URL(specifier);
        url.search = "";
        url.hash = "";
        return urlModule.fileURLToPath(url);
      }
      if (
        /^[A-Za-z][A-Za-z+.-]*:/.test(specifier) &&
        !/^[A-Za-z]:[/\\]/.test(specifier)
      ) {
        return null;
      }
      return specifier.replace(/[?#].*$/, "");
    };
    const describeWorkerSource = (filename, options) => {
      if (options?.eval === true) {
        return {
          cacheKey: null,
          load: () => String(filename ?? "").slice(
            0,
            maximumWmiWorkerSourceBytes,
          ),
        };
      }
      const specifier = workerSpecifierText(filename);
      if (/^data:/i.test(specifier)) {
        return {
          cacheKey: null,
          load: () => decodeDataWorkerSource(specifier),
        };
      }
      const path = workerFilePath(filename);
      if (!path) return null;
      const fs = process.getBuiltinModule("fs");
      const stats = fs.statSync(path, { bigint: true });
      return {
        cacheKey: [
          path,
          stats.dev,
          stats.ino,
          stats.size,
          stats.mtimeNs,
          stats.ctimeNs,
        ].join("\0"),
        load: () => fs
          .readFileSync(path, "utf8")
          .slice(0, maximumWmiWorkerSourceBytes),
      };
    };
    const classifyWmiSnapshotWorker = (filename, options) => {
      if (!disableWindowsWmiSampler) return null;
      const workerName = workerDisplayName(filename, options);
      if (options?.[windowsWmiSamplerSelfTest] === true) {
        return { reason: "self-test", workerName };
      }
      windowsWmiSamplerEvidence.workersObserved += 1;
      windowsWmiSamplerEvidence.lastObservedWorkerName = workerName;
      windowsWmiSamplerEvidence.lastObservedThreadName =
        workerThreadName(options);
      windowsWmiSamplerEvidence.lastObservedSourceSignals = [];
      if (isKnownWmiSnapshotWorkerName(filename)) {
        return { reason: "known-worker-name", workerName };
      }
      if (isKnownWmiSnapshotWorkerThreadName(options)) {
        return { reason: "worker-option-name", workerName };
      }

      try {
        const descriptor = describeWorkerSource(filename, options);
        if (!descriptor) return null;
        if (
          descriptor.cacheKey &&
          workerSourceMatchCache.has(descriptor.cacheKey)
        ) {
          const cached = workerSourceMatchCache.get(descriptor.cacheKey);
          windowsWmiSamplerEvidence.lastObservedSourceSignals =
            cached?.sourceSignals ?? [];
          return cached ? { ...cached, workerName } : null;
        }
        windowsWmiSamplerEvidence.sourceInspections += 1;
        const sourceSignals = wmiSnapshotSourceSignals(descriptor.load());
        const matchedSourceSignals = Object.entries(
          sourceSignals,
        )
          .filter(([, matched]) => matched)
          .map(([signal]) => signal);
        windowsWmiSamplerEvidence.lastObservedSourceSignals =
          matchedSourceSignals;
        const matched = hasWmiSnapshotSourceSignature(sourceSignals);
        if (matched) {
          windowsWmiSamplerEvidence.sourceSignatureMatches += 1;
          const match = {
            reason: "source-signature",
            sourceSignals: matchedSourceSignals,
          };
          rememberWorkerSourceMatch(descriptor.cacheKey, match);
          return { ...match, workerName };
        }
        windowsWmiSamplerEvidence.sourceSignatureMisses += 1;
        rememberWorkerSourceMatch(descriptor.cacheKey, null);
      } catch {
        windowsWmiSamplerEvidence.sourceReadFailures += 1;
      }
      return null;
    };

    // Codex starts this telemetry worker every 30 seconds. On Windows the
    // worker shells out to PowerShell for two full CIM/WMI process scans.
    // Return the protocol's valid empty snapshot without creating a thread,
    // process, timer, or PowerShell child.
    class CodeyDisabledWmiSnapshotWorker extends EventEmitter {
      constructor(selfTest = false) {
        super();
        this.threadId = -1;
        this.stdin = null;
        this.stdout = null;
        this.stderr = null;
        this.codeyTerminated = false;
        Object.defineProperty(this, "__codeyWmiSamplerSelfTest", {
          value: selfTest,
        });
        process.nextTick(() => {
          if (this.codeyTerminated) return;
          this.emit("message", { type: "ok", value: [] });
          this.emit("exit", 0);
        });
      }
      postMessage() {}
      ref() { return this; }
      unref() { return this; }
      terminate() {
        if (!this.codeyTerminated) {
          this.codeyTerminated = true;
          process.nextTick(() => this.emit("exit", 0));
        }
        return Promise.resolve(0);
      }
    }

    class CodeyNoInspectWorker extends NativeWorker {
      constructor(filename, options = {}) {
        const match = classifyWmiSnapshotWorker(filename, options);
        if (match) {
          const selfTest = match.reason === "self-test";
          if (!selfTest) {
            windowsWmiSamplerEvidence.blocked += 1;
            windowsWmiSamplerEvidence.lastMatchReason = match.reason;
            windowsWmiSamplerEvidence.lastWorkerName = match.workerName;
          }
          return new CodeyDisabledWmiSnapshotWorker(selfTest);
        }
        super(filename, {
          ...options,
          execArgv: options.execArgv ?? [],
        });
      }
    }
    Object.defineProperty(CodeyNoInspectWorker, "__codeyNoInspectWrapper", {
      value: true,
    });
    Object.defineProperty(
      CodeyNoInspectWorker,
      "__codeyRunWmiSamplerSelfTest",
      {
        value() {
          const sourceProbe = [
            'const { parentPort } = require("node:worker_threads");',
            'const executable = "powershell.exe";',
            'const command = "Get-CimInstance Win32_Process Win32_PerfFormattedData_PerfProc_Process";',
            "parentPort.postMessage({ executable, command });",
          ].join("\n");
          const recognizersPassed =
            isKnownWmiSnapshotWorkerName(
              "child-process-snapshot-worker-codey-self-test.js",
            ) &&
            isKnownWmiSnapshotWorkerThreadName({
              name: "child-process-snapshot",
            }) &&
            hasWmiSnapshotSourceSignature(
              wmiSnapshotSourceSignals(sourceProbe),
            );
          if (!recognizersPassed) return false;
          const probe = new CodeyNoInspectWorker(
            "codey-wmi-sampler-self-test.js",
            { [windowsWmiSamplerSelfTest]: true },
          );
          const passed =
            probe?.__codeyWmiSamplerSelfTest === true &&
            probe?.threadId === -1;
          probe?.terminate?.();
          return passed;
        },
      },
    );
    workerThreads.Worker = CodeyNoInspectWorker;
  }
  windowsWmiSamplerEvidence.workerWrapperPatched =
    workerThreads.Worker?.__codeyNoInspectWrapper === true;
  try {
    Module.syncBuiltinESMExports?.();
    windowsWmiSamplerEvidence.esmExportsSynchronized = true;
  } catch (error) {
    windowsWmiSamplerEvidence.esmExportsSynchronized = false;
    recordCodeyPatchFailure("sync_worker_threads_esm_exports", error);
  }
  if (
    disableWindowsWmiSampler &&
    windowsWmiSamplerEvidence.workerWrapperPatched &&
    windowsWmiSamplerEvidence.esmExportsSynchronized
  ) {
    try {
      const runSelfTest =
        workerThreads.Worker?.__codeyRunWmiSamplerSelfTest;
      windowsWmiSamplerEvidence.selfTestPassed =
        typeof runSelfTest === "function" && runSelfTest();
      if (!windowsWmiSamplerEvidence.selfTestPassed) {
        throw new Error("WMI sampler Worker wrapper did not intercept its self-test");
      }
    } catch (error) {
      windowsWmiSamplerEvidence.selfTestPassed = false;
      windowsWmiSamplerEvidence.selfTestError =
        error instanceof Error ? error.message.slice(0, 240) : String(error);
      recordCodeyPatchFailure("wmi_sampler_self_test", error);
    }
  }

  const executionProcessEvidence = {
    version: 1,
    snapshotWorkerConfigured: false,
    snapshots: 0,
    snapshotFailures: 0,
    terminationAttempts: 0,
    terminated: 0,
    lastError: "",
  };
  let executionProcessSnapshotWorkerPath = "";
  const normalizeExecutionProcessSnapshot = (processes) => {
    if (!Array.isArray(processes)) return [];
    const observedAtMs = Date.now();
    const normalizedInput = processes.flatMap((processInfo) => {
      const pid = Number(processInfo?.pid);
      const parentPid = Number(processInfo?.parentPid);
      if (
        !Number.isSafeInteger(pid) || pid <= 1 ||
        !Number.isSafeInteger(parentPid) || parentPid < 0
      ) return [];
      const ageSeconds = Number.isFinite(processInfo?.ageSeconds)
        ? Math.max(0, Number(processInfo.ageSeconds))
        : null;
      const providedStartedAtMs = Number(processInfo?.startedAtMs);
      const startedAtMs = Number.isFinite(providedStartedAtMs)
        ? providedStartedAtMs
        : ageSeconds == null
          ? null
          : observedAtMs - ageSeconds * 1000;
      return [{
        ...processInfo,
        ageSeconds,
        command: String(processInfo?.command ?? "").trim(),
        parentPid,
        pid,
        startedAtMs,
      }];
    });
    const byPid = new Map(
      normalizedInput.map((processInfo) => [processInfo.pid, processInfo]),
    );
    const normalized = [];
    for (const processInfo of byPid.values()) {
      let cursor = processInfo;
      let rootChild = processInfo;
      let relativeDepth = 1;
      const visited = new Set([processInfo.pid]);
      while (true) {
        const parent = byPid.get(cursor.parentPid);
        if (parent == null || visited.has(parent.pid)) break;
        if (parent.kind === "app_server") {
          normalized.push({
            ...processInfo,
            appServerPid: parent.pid,
            depth: relativeDepth,
            rootChildPid: rootChild.pid,
          });
          break;
        }
        visited.add(parent.pid);
        cursor = parent;
        rootChild = parent;
        relativeDepth += 1;
      }
    }
    return normalized;
  };
  const configureExecutionProcessSnapshotWorker = (mainBundleFilename) => {
    const path = process.getBuiltinModule("path");
    executionProcessSnapshotWorkerPath = path.join(
      path.dirname(mainBundleFilename),
      "child-process-snapshot-worker.js",
    );
    executionProcessEvidence.snapshotWorkerConfigured = true;
  };
  const snapshotExecutionProcesses = () => {
    executionProcessEvidence.snapshots += 1;
    return new Promise((resolve, reject) => {
      if (!executionProcessSnapshotWorkerPath) {
        const error = new Error("Codey execution snapshot worker is not configured");
        executionProcessEvidence.snapshotFailures += 1;
        executionProcessEvidence.lastError = error.message;
        reject(error);
        return;
      }
      let settled = false;
      let worker = null;
      let timer = null;
      const finish = (error, processes) => {
        if (settled) return;
        settled = true;
        if (timer != null) clearTimeout(timer);
        if (worker != null) {
          try { Promise.resolve(worker.terminate()).catch(() => {}); } catch {}
        }
        if (error != null) {
          executionProcessEvidence.snapshotFailures += 1;
          executionProcessEvidence.lastError =
            error instanceof Error ? error.message.slice(0, 240) : String(error);
          reject(error);
          return;
        }
        executionProcessEvidence.lastError = "";
        resolve(normalizeExecutionProcessSnapshot(processes));
      };
      try {
        worker = new NativeWorker(executionProcessSnapshotWorkerPath, {
          name: "codey-execution-process-reaper",
          workerData: process.pid,
        });
        worker.once("message", (message) => {
          if (message?.type === "ok" && Array.isArray(message.value)) {
            finish(null, message.value);
          } else {
            finish(new Error(
              message?.error?.message || "Codey execution snapshot worker failed",
            ));
          }
        });
        worker.once("error", (error) => finish(error));
        worker.once("exit", (code) => {
          if (!settled) {
            finish(new Error(`Codey execution snapshot worker exited with ${code}`));
          }
        });
        worker.unref?.();
        timer = setTimeout(() => {
          finish(new Error("Codey execution snapshot worker timed out"));
        }, 10 * 1000);
        timer.unref?.();
      } catch (error) {
        finish(error);
      }
    });
  };
  const isStandaloneNodeReplProcess = (processInfo) => {
    const command = String(processInfo?.command ?? "");
    const pid = Number(processInfo?.pid);
    return (
      processInfo?.kind === "other" &&
      Number.isSafeInteger(pid) &&
      pid === Number(processInfo?.rootChildPid) &&
      Number(processInfo?.depth) === 1 &&
      Number(processInfo?.parentPid) === Number(processInfo?.appServerPid) &&
      /(?:^|[/\\])cua_node[/\\](?:bin[/\\])?node_repl(?:\.exe)?(?:\s|$)/i.test(command)
    );
  };
  const terminateExecutionProcess = async (pid, expectedProcess) => {
    const normalizedPid = Number(pid);
    const appServerPid = Number(expectedProcess?.appServerPid);
    if (
      !Number.isSafeInteger(normalizedPid) || normalizedPid <= 1 ||
      normalizedPid === process.pid ||
      expectedProcess?.pid !== normalizedPid ||
      !isStandaloneNodeReplProcess(expectedProcess) ||
      !Number.isSafeInteger(appServerPid) || appServerPid <= 1 ||
      normalizedPid === appServerPid
    ) return false;
    executionProcessEvidence.terminationAttempts += 1;
    try {
      process.kill(normalizedPid, "SIGTERM");
      executionProcessEvidence.terminated += 1;
      return true;
    } catch (error) {
      if (error?.code === "ESRCH") {
        executionProcessEvidence.terminated += 1;
        return true;
      }
      executionProcessEvidence.lastError =
        error instanceof Error ? error.message.slice(0, 240) : String(error);
      return false;
    }
  };
  const executionProcessLifecycle = Object.freeze({
    configure: configureExecutionProcessSnapshotWorker,
    normalizeSnapshot: normalizeExecutionProcessSnapshot,
    snapshot: snapshotExecutionProcesses,
    terminate: terminateExecutionProcess,
    get status() {
      return { ...executionProcessEvidence };
    },
  });
  Object.defineProperty(globalThis, "__CODEY_EXECUTION_PROCESS_LIFECYCLE__", {
    configurable: false,
    value: executionProcessLifecycle,
    writable: false,
  });

  const temporaryWebViews = new WeakMap();
  const temporaryWebViewLifecycle = Object.freeze({
    close(owner, partition) {
      const guests = temporaryWebViews.get(owner);
      const guest = guests?.get(partition);
      guests?.delete(partition);
      if (guests?.size === 0) temporaryWebViews.delete(owner);
      if (guest != null && !guest.isDestroyed()) guest.close();
    },
    track(owner, partition, guest) {
      let guests = temporaryWebViews.get(owner);
      if (guests == null) {
        guests = new Map();
        temporaryWebViews.set(owner, guests);
      }
      const previous = guests.get(partition);
      if (previous != null && previous !== guest && !previous.isDestroyed()) previous.close();
      guests.set(partition, guest);
      guest.once("destroyed", () => {
        if (guests.get(partition) === guest) guests.delete(partition);
        if (guests.size === 0) temporaryWebViews.delete(owner);
      });
    },
  });
  Object.defineProperty(globalThis, "__CODEY_TEMP_WEBVIEW_LIFECYCLE__", {
    configurable: false,
    value: temporaryWebViewLifecycle,
    writable: false,
  });
  const findCheckoutWebViewAttachFunction = (source) => {
    const attachFunctionPattern =
      /function [$A-Z_a-z][$\w]*\(\{getAuthToken:[$A-Z_a-z][$\w]*[^{}]{0,500},owner:([$A-Z_a-z][$\w]*)\}\)\{/g;
    const matches = [];
    for (const candidate of source.matchAll(attachFunctionPattern)) {
      const signature = candidate[0];
      if (
        !signature.includes("onBusinessCheckoutComplete:") ||
        !signature.includes("onPersonalCheckoutComplete:") ||
        !signature.includes("onPurchaseCheckoutComplete:")
      ) continue;
      const end = source.indexOf("}function ", candidate.index + signature.length);
      if (end < 0 || end > candidate.index + 3000) continue;
      const body = source.slice(candidate.index, end + 1);
      if (!body.includes("will-attach-webview") || !body.includes("did-attach-webview")) continue;
      matches.push({ body, match: candidate });
    }
    return matches.length === 1 ? matches[0] : null;
  };
  const patchCheckoutWebViewAttachment = (source) => {
    const attachment = findCheckoutWebViewAttachFunction(source);
    if (!attachment) {
      throw new Error("Codey temporary WebView attach handler not found");
    }
    const shiftedEntries = [...attachment.body.matchAll(
      /let ([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*\.shift\(\);if\(\1==null\)return;/g,
    )];
    if (shiftedEntries.length !== 1) {
      throw new Error("Codey temporary WebView attachment queue not found");
    }
    const shiftedEntry = shiftedEntries[0];
    const guestReferences = [...attachment.body
      .slice(shiftedEntry.index + shiftedEntry[0].length)
      .matchAll(/webContents:([$A-Z_a-z][$\w]*)\}\)\},/g)];
    if (guestReferences.length !== 1) {
      throw new Error("Codey temporary WebView guest reference not found");
    }
    const trackOffset =
      attachment.match.index + shiftedEntry.index + shiftedEntry[0].length;
    return (
      source.slice(0, trackOffset) +
      `globalThis.__CODEY_TEMP_WEBVIEW_LIFECYCLE__.track(${attachment.match[1]},${shiftedEntry[1]}.partition,${guestReferences[0][1]});` +
      source.slice(trackOffset)
    );
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_PATCH_CHECKOUT_WEBVIEW_ATTACHMENT__",
    {
      configurable: false,
      value: patchCheckoutWebViewAttachment,
      writable: false,
    },
  );

  const installExecutionReaper = ({
    connection,
    kill,
    snapshot,
    completionGraceMs: configuredCompletionGraceMs,
  }) => {
    const activeTurns = new Map();
    const completionGraceMs = Math.max(0, configuredCompletionGraceMs ?? 1000);
    const reclaimRetryMs = 60 * 1000;
    const subagentUnsubscribeRetryMs = 60 * 1000;
    const maxSubagentUnsubscribeAttempts = 3;
    const terminalTurnStates = new Set([
      "completed",
      "aborted",
      "cancelled",
      "canceled",
      "failed",
      "error",
      "errored",
      "closed",
      "stopped",
      "interrupted",
    ]);
    const terminalThreadMethods = new Set([
      "thread/archived",
      "thread/closed",
      "thread/deleted",
    ]);
    const successfulThreadUnsubscribeStates = new Set([
      "unsubscribed",
      "notSubscribed",
      "notLoaded",
    ]);
    const subagentThreadIds = new Set();
    const subagentUnsubscribeAttempts = new Map();
    const subagentUnsubscribeTimers = new Map();
    let cleanupPromise = null;
    let reclaimTimer = null;
    let reclaimBarrier = null;
    let reclaimAuthorizedVersion = null;
    let disposed = false;
    let lastTurnActivityAt = Date.now();
    let turnStateVersion = 0;

    const processCommandIdentity = (processInfo) => {
      let command = String(processInfo?.command ?? "")
        .replace(/\s+/g, " ")
        .trim();
      if (process.platform === "win32") command = command.toLowerCase();
      return command;
    };
    const sameProcessIdentity = (left, right) => {
      if (
        left?.pid !== right?.pid ||
        left?.parentPid !== right?.parentPid ||
        left?.appServerPid !== right?.appServerPid ||
        left?.rootChildPid !== right?.rootChildPid ||
        left?.kind !== right?.kind ||
        processCommandIdentity(left) !== processCommandIdentity(right) ||
        !Number.isFinite(left?.startedAtMs) ||
        !Number.isFinite(right?.startedAtMs)
      ) return false;
      return Math.abs(left.startedAtMs - right.startedAtMs) <= 2500;
    };
    const selectReclaimCandidates = (processes) => {
      const candidates = new Map();
      for (const processInfo of processes) {
        if (!isStandaloneNodeReplProcess(processInfo)) continue;
        const pid = Number(processInfo?.pid);
        if (!Number.isSafeInteger(pid) || pid <= 1 || pid === process.pid) continue;
        candidates.set(pid, { processInfo, reclaimClass: "node-repl" });
      }
      return Array.from(candidates.values()).sort(
        (left, right) =>
          (right.processInfo?.depth ?? 0) - (left.processInfo?.depth ?? 0),
      );
    };

    const clearReclaimTimer = () => {
      if (reclaimTimer == null) return;
      clearTimeout(reclaimTimer);
      reclaimTimer = null;
    };

    const cancelReclaimBarrier = () => {
      if (reclaimBarrier == null) return;
      const barrier = reclaimBarrier;
      reclaimBarrier = null;
      clearTimeout(barrier.timer);
      barrier.resolve(false);
    };

    const isReclaimAuthorized = (expectedVersion) =>
      !disposed &&
      activeTurns.size === 0 &&
      reclaimAuthorizedVersion === expectedVersion &&
      turnStateVersion === expectedVersion;

    const isReclaimSafe = (expectedVersion, now = Date.now()) =>
      isReclaimAuthorized(expectedVersion) &&
      now - lastTurnActivityAt >= completionGraceMs;

    const waitForReclaimBarrier = (expectedVersion, delayMs) => {
      if (!isReclaimSafe(expectedVersion)) return Promise.resolve(false);
      cancelReclaimBarrier();
      return new Promise((resolve) => {
        const timer = setTimeout(() => {
          if (reclaimBarrier?.timer === timer) reclaimBarrier = null;
          resolve(isReclaimSafe(expectedVersion));
        }, Math.max(0, delayMs));
        timer.unref?.();
        reclaimBarrier = { resolve, timer };
      });
    };

    const armReclaim = (reason, minimumDelayMs = 0) => {
      clearReclaimTimer();
      const expectedVersion = reclaimAuthorizedVersion;
      if (expectedVersion == null || !isReclaimAuthorized(expectedVersion)) return;
      const graceRemaining = completionGraceMs - (Date.now() - lastTurnActivityAt);
      reclaimTimer = setTimeout(() => {
        reclaimTimer = null;
        void reclaim(reason);
      }, Math.max(1, graceRemaining, minimumDelayMs));
      reclaimTimer.unref?.();
    };

    const recordTurnStateChange = (now) => {
      lastTurnActivityAt = now;
      turnStateVersion += 1;
      reclaimAuthorizedVersion = null;
      clearReclaimTimer();
      cancelReclaimBarrier();
    };

    const reclaim = (reason) => {
      const expectedVersion = reclaimAuthorizedVersion;
      if (expectedVersion == null) return cleanupPromise;
      if (cleanupPromise != null) return cleanupPromise;
      if (!isReclaimSafe(expectedVersion)) {
        if (isReclaimAuthorized(expectedVersion)) armReclaim(reason);
        return cleanupPromise;
      }
      clearReclaimTimer();
      let cleanupSucceeded = false;
      cleanupPromise = Promise.resolve()
        .then(snapshot)
        .then(async (processes) => {
          // A fresh quiet window after the process snapshot lets queued turn
          // notifications invalidate this cleanup before the first kill.
          if (!await waitForReclaimBarrier(expectedVersion, completionGraceMs)) {
            return { reason, reclaimed: 0 };
          }
          let candidates = selectReclaimCandidates(processes);
          if (candidates.length > 0) {
            const originalCandidates = new Map(
              candidates.map((candidate) => [candidate.processInfo.pid, candidate]),
            );
            const freshProcesses = await snapshot();
            if (!isReclaimSafe(expectedVersion)) {
              return { reason, reclaimed: 0 };
            }
            candidates = selectReclaimCandidates(freshProcesses).filter((candidate) => {
              const original = originalCandidates.get(candidate.processInfo.pid);
              return original?.reclaimClass === candidate.reclaimClass &&
                sameProcessIdentity(original.processInfo, candidate.processInfo);
            });
          }
          let reclaimed = 0;
          let allKillsSucceeded = true;
          for (const { processInfo } of candidates) {
            // Yield once more immediately before each irreversible operation.
            if (!await waitForReclaimBarrier(expectedVersion, 0)) {
              break;
            }
            try {
              if (await kill(processInfo.pid, processInfo) !== false) reclaimed += 1;
              else allKillsSucceeded = false;
            } catch {
              allKillsSucceeded = false;
            }
            if (!isReclaimSafe(expectedVersion)) break;
          }
          cleanupSucceeded =
            allKillsSucceeded &&
            reclaimed === candidates.length &&
            isReclaimSafe(expectedVersion);
          return { reason, reclaimed };
        })
        .catch(() => ({ reason, reclaimed: 0 }))
        .finally(() => {
          cleanupPromise = null;
          cancelReclaimBarrier();
          if (disposed) return;
          if (cleanupSucceeded && isReclaimSafe(expectedVersion)) {
            reclaimAuthorizedVersion = null;
            return;
          }
          if (reclaimAuthorizedVersion != null) {
            armReclaim(
              "turn-state-changed",
              reclaimAuthorizedVersion === expectedVersion ? reclaimRetryMs : 0,
            );
          }
        });
      return cleanupPromise;
    };

    const normalizedId = (value) =>
      typeof value === "string" && value.length > 0 ? value : null;
    const turnKey = (threadId, turnId) => `${threadId}\u0000${turnId}`;
    const markTurnActivity = (threadId, turnId, now) => {
      const key = turnKey(threadId, turnId);
      const turn = activeTurns.get(key);
      if (turn == null) return false;
      activeTurns.set(key, { ...turn, lastSeen: now });
      return true;
    };
    const removeThreadTurns = (threadId) => {
      let changed = false;
      for (const [key, turn] of activeTurns) {
        if (turn.threadId !== threadId) continue;
        activeTurns.delete(key);
        changed = true;
      }
      return changed;
    };
    const hasActiveThreadTurn = (threadId) => {
      for (const turn of activeTurns.values()) {
        if (turn.threadId === threadId) return true;
      }
      return false;
    };
    const clearSubagentUnsubscribeTimer = (threadId) => {
      const timer = subagentUnsubscribeTimers.get(threadId);
      if (timer == null) return;
      subagentUnsubscribeTimers.delete(threadId);
      clearTimeout(timer);
    };
    const forgetSubagentThread = (threadId) => {
      clearSubagentUnsubscribeTimer(threadId);
      subagentThreadIds.delete(threadId);
      subagentUnsubscribeAttempts.delete(threadId);
    };
    const markSubagentThread = (value) => {
      const threadId = normalizedId(value);
      if (threadId != null) subagentThreadIds.add(threadId);
      return threadId;
    };
    const scheduleSubagentUnsubscribe = (threadId, delayMs = completionGraceMs) => {
      if (
        disposed ||
        !subagentThreadIds.has(threadId) ||
        hasActiveThreadTurn(threadId)
      ) return;
      if (typeof connection?.unsubscribeThread !== "function") {
        forgetSubagentThread(threadId);
        return;
      }
      clearSubagentUnsubscribeTimer(threadId);
      const timer = setTimeout(async () => {
        if (subagentUnsubscribeTimers.get(threadId) !== timer) return;
        subagentUnsubscribeTimers.delete(threadId);
        if (
          disposed ||
          !subagentThreadIds.has(threadId) ||
          hasActiveThreadTurn(threadId)
        ) return;
        try {
          const result = await Reflect.apply(
            connection.unsubscribeThread,
            connection,
            [threadId],
          );
          const status = result?.status ?? result?.result?.status;
          if (
            typeof status === "string" &&
            !successfulThreadUnsubscribeStates.has(status)
          ) throw new Error(`Unexpected thread unsubscribe status: ${status}`);
          if (!disposed) forgetSubagentThread(threadId);
        } catch {
          if (disposed) return;
          const attempts = (subagentUnsubscribeAttempts.get(threadId) ?? 0) + 1;
          subagentUnsubscribeAttempts.set(threadId, attempts);
          if (attempts < maxSubagentUnsubscribeAttempts) {
            scheduleSubagentUnsubscribe(threadId, subagentUnsubscribeRetryMs);
          } else {
            forgetSubagentThread(threadId);
          }
        }
      }, Math.max(1, delayMs));
      timer.unref?.();
      subagentUnsubscribeTimers.set(threadId, timer);
    };

    let unsubscribe = connection.registerInternalNotificationHandler((notification) => {
      if (disposed) return;
      const method =
        typeof notification?.method === "string"
          ? notification.method.toLowerCase()
          : "";
      const params = notification?.params;
      const threadId = normalizedId(
        params?.threadId ?? params?.thread_id ?? params?.thread?.id,
      );
      const turnId = normalizedId(
        params?.turn?.id ?? params?.turnId ?? params?.turn_id,
      );
      const now = Date.now();
      const terminalTurnState =
        method.startsWith("turn/") && terminalTurnStates.has(method.slice(5));
      const terminalThread = terminalThreadMethods.has(method);
      const item = params?.item;
      if (method === "thread/started") {
        const source = params?.thread?.source;
        if (
          (typeof source === "string" && source.toLowerCase().startsWith("subagent")) ||
          (source != null && typeof source === "object" && "subAgent" in source)
        ) markSubagentThread(threadId);
      }
      if (
        (method === "item/started" || method === "item/completed") &&
        item != null && typeof item === "object"
      ) {
        if (item.type === "subAgentActivity") {
          markSubagentThread(item.agentThreadId);
        }
        if (item.type === "collabAgentToolCall") {
          const receiverThreadIds = Array.isArray(item.receiverThreadIds)
            ? item.receiverThreadIds
            : [];
          for (const receiverThreadId of receiverThreadIds) {
            if (receiverThreadId === threadId) continue;
            const subagentThreadId = markSubagentThread(receiverThreadId);
            if (
              subagentThreadId != null &&
              method === "item/completed" &&
              item.tool === "closeAgent" &&
              item.status === "completed"
            ) scheduleSubagentUnsubscribe(subagentThreadId);
          }
        }
      }

      if (method === "turn/started" && threadId != null && turnId != null) {
        clearSubagentUnsubscribeTimer(threadId);
        subagentUnsubscribeAttempts.delete(threadId);
        recordTurnStateChange(now);
        activeTurns.set(turnKey(threadId, turnId), { threadId, turnId, lastSeen: now });
        return;
      }

      if (terminalTurnState || terminalThread) {
        let changed = false;
        if (terminalThread && threadId != null) {
          changed = removeThreadTurns(threadId);
        } else if (threadId != null && turnId != null) {
          changed = activeTurns.delete(turnKey(threadId, turnId));
        } else if (threadId != null) {
          changed = removeThreadTurns(threadId);
        }
        if (terminalThread && threadId != null) {
          forgetSubagentThread(threadId);
        } else if (
          threadId != null &&
          subagentThreadIds.has(threadId) &&
          !hasActiveThreadTurn(threadId)
        ) {
          scheduleSubagentUnsubscribe(threadId);
        }
        // A terminal event that does not match a turn observed by this
        // subscription cannot prove that the connection is globally idle.
        if (!changed) return;
        recordTurnStateChange(now);
        if (activeTurns.size > 0) return;
        reclaimAuthorizedVersion = turnStateVersion;
        armReclaim(`task-${method.slice(method.lastIndexOf("/") + 1)}`);
        return;
      }

      if (threadId == null || turnId == null) return;
      if (!markTurnActivity(threadId, turnId, now)) return;
      recordTurnStateChange(now);
    });
    return () => {
      if (disposed) return;
      disposed = true;
      turnStateVersion += 1;
      reclaimAuthorizedVersion = null;
      clearReclaimTimer();
      cancelReclaimBarrier();
      activeTurns.clear();
      for (const timer of subagentUnsubscribeTimers.values()) clearTimeout(timer);
      subagentUnsubscribeTimers.clear();
      subagentUnsubscribeAttempts.clear();
      subagentThreadIds.clear();
      const disposeNotifications = unsubscribe;
      unsubscribe = null;
      try { disposeNotifications?.(); } catch {}
    };
  };
  Object.defineProperty(globalThis, "__CODEY_INSTALL_EXECUTION_REAPER__", {
    configurable: false,
    value: installExecutionReaper,
    writable: false,
  });

  const optionalMainBundlePatchFailures = [];
  let mainBundleSourcePatchAttempted = false;
  let mainBundleSourcePatched = false;
  let mainBundleFilename = "";
  const hasOptionalMainBundlePatchFailure = (name) =>
    optionalMainBundlePatchFailures.some((failure) => failure.name === name);
  const applyOptionalMainBundlePatch = (name, patch, source) => {
    try {
      const patched = patch(source);
      const failureIndex = optionalMainBundlePatchFailures.findIndex(
        (failure) => failure.name === name,
      );
      if (failureIndex >= 0) {
        optionalMainBundlePatchFailures.splice(failureIndex, 1);
      }
      return patched;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const failure = { name, message };
      const failureIndex = optionalMainBundlePatchFailures.findIndex(
        (entry) => entry.name === name,
      );
      if (failureIndex >= 0) {
        optionalMainBundlePatchFailures[failureIndex] = failure;
      } else {
        optionalMainBundlePatchFailures.push(failure);
      }
      recordCodeyPatchFailure(`optional_main_bundle_patch:${name}`, error, {
        patchName: name,
      });
      console.warn(`[Codey] skipped incompatible ${name} patch: ${message}`);
      return source;
    }
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_APPLY_OPTIONAL_MAIN_BUNDLE_PATCH__",
    {
      configurable: false,
      value: applyOptionalMainBundlePatch,
      writable: false,
    },
  );

  // Install the main-bundle lifecycle and telemetry patches before V8 compiles
  // the monolithic bundle. Slim-pet mode skips only the eager hidden overlay
  // prewarm; the manager and native composition bridge stay available for
  // explicit voice use.
  {
    const originalJsExtension = Module._extensions[".js"];
    Module._extensions[".js"] = function codeyMainBundleCompileHook(module, filename) {
      const isCodexBuildScript =
        /[\\/]\.vite[\\/]build[\\/][^\\/]+\.(?:cjs|js)$/i.test(filename);
      if (!isCodexBuildScript) {
        return Reflect.apply(originalJsExtension, this, arguments);
      }

      const fs = process.getBuiltinModule("fs");
      let source = fs.readFileSync(filename, "utf8");
      const hasMainBundleName =
        /[\\/]\.vite[\\/]build[\\/]main(?:[-.][^\\/]*)?\.(?:cjs|js)$/i.test(filename);
      const hasMainBundleSignature =
        source.includes("checkout-webview-presentation-changed") &&
        source.includes("will-attach-webview") &&
        source.includes("did-attach-webview");
      if (!hasMainBundleName && !hasMainBundleSignature) {
        return Reflect.apply(originalJsExtension, this, arguments);
      }

      mainBundleSourcePatchAttempted = true;
      mainBundleFilename = filename.split(/[\\/]/).at(-1)?.slice(0, 160) ?? "";
      try {
      executionProcessLifecycle.configure(filename);
      source = applyOptionalMainBundlePatch(
        "desktopCesAnalytics",
        patchCodexMainDesktopAnalytics,
        source,
      );
      source = applyOptionalMainBundlePatch(
        "externalPluginFocusReconcile",
        patchCodexMainFocusReconcile,
        source,
      );
      source = applyOptionalMainBundlePatch(
        "appStateHeartbeat",
        patchCodexMainAppStateHeartbeat,
        source,
      );
      if (disablePet) {
        source = applyOptionalMainBundlePatch(
          "avatarOverlayPrewarm",
          patchCodexAvatarOverlayPrewarm,
          source,
        );
      }
      const presentationCall = source.match(
        /case`checkout-webview-presentation-changed`:([$A-Z_a-z][$\w]*)\(([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*)\);break/,
      );
      if (!presentationCall) {
        throw new Error("Codey temporary WebView close anchor not found");
      }
      const presentationFunctionName = presentationCall[1].replace(/[$]/g, "\\$&");
      const presentationFunction = new RegExp(
        "function " + presentationFunctionName +
          "\\(([$A-Z_a-z][$\\w]*),\\{partition:([$A-Z_a-z][$\\w]*),url:([$A-Z_a-z][$\\w]*)\\}\\)\\{",
      ).exec(source);
      if (!presentationFunction) {
        throw new Error("Codey temporary WebView presentation handler not found");
      }
      const ownerName = presentationFunction[1];
      const partitionName = presentationFunction[2];
      const urlName = presentationFunction[3];
      const closeBranch = `if(${urlName}==null){`;
      const closeBranchOffset = source.indexOf(closeBranch, presentationFunction.index);
      if (closeBranchOffset < 0 || closeBranchOffset > presentationFunction.index + 1000) {
        throw new Error("Codey temporary WebView close branch not found");
      }
      source =
        source.slice(0, closeBranchOffset + closeBranch.length) +
        `globalThis.__CODEY_TEMP_WEBVIEW_LIFECYCLE__.close(${ownerName},${partitionName});` +
        source.slice(closeBranchOffset + closeBranch.length);

      source = patchCheckoutWebViewAttachment(source);

      const reaperAnchorPattern =
        /([$A-Z_a-z][$\w]*)\.add\(([$A-Z_a-z][$\w]*)\(\{appServerConnection:([$A-Z_a-z][$\w]*)\(\),closeActiveTurn:([$A-Z_a-z][$\w]*)\.closeActiveTurn\}\)\);/;
      const reaperAnchor = reaperAnchorPattern.exec(source);
      if (!reaperAnchor) {
        throw new Error("Codey execution reaper completion anchor not found");
      }
      const disposerName = reaperAnchor[1];
      const connectionFactoryName = reaperAnchor[3];
      const reaperInstall =
        `${disposerName}.add(globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({` +
        `connection:${connectionFactoryName}(),` +
        `snapshot:()=>globalThis.__CODEY_EXECUTION_PROCESS_LIFECYCLE__.snapshot(),` +
        `kill:(pid,processInfo)=>globalThis.__CODEY_EXECUTION_PROCESS_LIFECYCLE__.terminate(pid,processInfo)` +
        `}));`;
      const reaperOffset = reaperAnchor.index + reaperAnchor[0].length;
      source = source.slice(0, reaperOffset) + reaperInstall + source.slice(reaperOffset);

      globalThis.__CODEY_TEMP_WEBVIEW_SOURCE_PATCHED__ = true;
      globalThis.__CODEY_EXECUTION_REAPER_SOURCE_PATCHED__ = true;
      globalThis.__CODEY_EXTERNAL_PLUGIN_FOCUS_RECONCILE_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("externalPluginFocusReconcile");
      globalThis.__CODEY_DESKTOP_ANALYTICS_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("desktopCesAnalytics");
      globalThis.__CODEY_APP_STATE_HEARTBEAT_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("appStateHeartbeat");
      mainBundleSourcePatched = true;
      module._compile(source, filename);
      } catch (error) {
        recordCodeyPatchFailure("patch_codex_main_bundle", error, { filename });
        throw error;
      }
    };
  }

  const microStub = {
    __codexMicroDisabledLocal: true,
    ConnectionEventType: {
      CONNECTED: "CONNECTED",
      DISCONNECTED: "DISCONNECTED",
      ERROR: "ERROR",
    },
    DeviceType: { Project2077: "Project2077" },
    OAILightingEffect: { off: 0, breath: 1, solid: 2, snake: 3 },
    WLDeviceDiscovery: class NoCodexMicroDeviceDiscovery {
      findWLDevices() { return []; }
    },
    WLDeviceCommImpl: class NoCodexMicroDeviceComm {
      onConnectionEvent() { return () => {}; }
      async connect() {}
      async disconnect() {}
    },
    RPCApiOAI: class NoCodexMicroApi {
      onHidReceived() { return () => {}; }
      onJoystickMove() { return () => {}; }
      async sendLightingConfig() { return true; }
      async sendThreadsLighting() { return true; }
      async getDeviceStatus() { return {}; }
    },
  };

  let electronProxy = null;
  let electronProtocolProxy = null;
  let electronIpcMainProxy = null;
  let electronBrowserWindowProxy = null;
  const electronMainRequests = new Set(["electron", "electron/main"]);
  const installNativeIpcMainGuards = (ipcMain) => {
    if (!ipcMain) return false;
    let installed = false;
    const installRegistrationGuard = (property, guarded) => {
      Object.defineProperty(guarded, "__codeyMainIpcRegistrationGuard", {
        value: true,
      });
      try {
        ipcMain[property] = guarded;
      } catch {}
      if (ipcMain[property] !== guarded) {
        try {
          Object.defineProperty(ipcMain, property, {
            configurable: true,
            value: guarded,
            writable: true,
          });
        } catch {}
      }
      installed ||= ipcMain[property] === guarded;
    };
    for (const property of ["handle", "handleOnce"]) {
      const original = ipcMain[property];
      if (typeof original !== "function") continue;
      if (original.__codeyMainIpcRegistrationGuard === true) {
        installed = true;
        continue;
      }
      const guarded = function (channel, handler, ...rest) {
        return Reflect.apply(original, ipcMain, [
          channel,
          mainGitRequestGuard.wrapIpcHandler(handler, channel),
          ...rest,
        ]);
      };
      installRegistrationGuard(property, guarded);
    }

    const eventRegistrations = new Map(
      ["on", "addListener", "once"].map((property) => [
        property,
        ipcMain[property],
      ]),
    );
    const originalOn =
      eventRegistrations.get("on") ?? eventRegistrations.get("addListener");
    for (const property of ["on", "addListener"]) {
      const original = eventRegistrations.get(property);
      if (typeof original !== "function") continue;
      if (original.__codeyMainIpcRegistrationGuard === true) {
        installed = true;
        continue;
      }
      const guarded = function (channel, handler, ...rest) {
        const effectiveHandler =
          mainGitRequestGuard.wrapIpcHandler(handler, channel);
        if (effectiveHandler !== handler) {
          Object.defineProperty(effectiveHandler, "listener", {
            configurable: true,
            value: handler,
          });
        }
        return Reflect.apply(original, ipcMain, [
          channel,
          effectiveHandler,
          ...rest,
        ]);
      };
      installRegistrationGuard(property, guarded);
    }
    const originalOnce = eventRegistrations.get("once");
    if (
      typeof originalOnce === "function" &&
      typeof originalOn === "function" &&
      originalOnce.__codeyMainIpcRegistrationGuard !== true
    ) {
      const guardedOnce = function (channel, handler, ...rest) {
        const effectiveHandler =
          mainGitRequestGuard.wrapIpcHandler(handler, channel);
        const onceHandler = function (...args) {
          ipcMain.removeListener?.(channel, onceHandler);
          return Reflect.apply(effectiveHandler, this, args);
        };
        Object.defineProperty(onceHandler, "listener", {
          configurable: true,
          value: handler,
        });
        return Reflect.apply(originalOn, ipcMain, [
          channel,
          onceHandler,
          ...rest,
        ]);
      };
      installRegistrationGuard("once", guardedOnce);
    }
    return installed;
  };
  Module._load = function codeyStartupPatchLoader(request, parent, isMain) {
    if (disableMicro && request === "@worklouder/device-kit-oai") return microStub;

    const loaded = Reflect.apply(originalLoad, this, arguments);
    if (
      !electronMainRequests.has(request) ||
      (!loaded?.BrowserWindow && !loaded?.ipcMain && !loaded?.protocol)
    ) return loaded;
    if (electronProxy) return electronProxy;

    if (loaded.protocol) {
      electronProtocolProxy = new Proxy(loaded.protocol, {
        get(target, property, receiver) {
          if (property === "handle") {
            return (scheme, handler) => {
              const effectiveHandler =
                scheme === "app" && typeof handler === "function"
                  ? async (request) =>
                      patchCodexRendererResponse(request, await handler(request))
                  : handler;
              return target.handle(scheme, effectiveHandler);
            };
          }
          const value = Reflect.get(target, property, receiver);
          return typeof value === "function" ? value.bind(target) : value;
        },
      });
    }
    if (loaded.ipcMain) {
      const nativeGuardInstalled = installNativeIpcMainGuards(loaded.ipcMain);
      electronIpcMainProxy = nativeGuardInstalled
        ? loaded.ipcMain
        : new Proxy(loaded.ipcMain, {
            get(target, property, receiver) {
              if (
                (property === "handle" || property === "handleOnce") &&
                typeof target[property] === "function"
              ) {
                return (channel, handler, ...rest) =>
                  Reflect.apply(target[property], target, [
                    channel,
                    mainGitRequestGuard.wrapIpcHandler(handler, channel),
                    ...rest,
                  ]);
              }
              const value = Reflect.get(target, property, receiver);
              return typeof value === "function" ? value.bind(target) : value;
            },
          });
    }
    if (disablePet && typeof loaded.BrowserWindow === "function") {
      electronBrowserWindowProxy = new Proxy(loaded.BrowserWindow, {
        construct(target, args, newTarget) {
          const [options, ...rest] = args;
          const isHiddenAvatarOverlay =
            options?.alwaysOnTop === true &&
            options?.transparent === true &&
            options?.focusable === false &&
            options?.frame === false &&
            options?.skipTaskbar === true &&
            options?.show === false;
          const restoreVisibleFrameRate =
            options?.webPreferences?.backgroundThrottling === false;
          const effectiveOptions = isHiddenAvatarOverlay
            ? {
                ...options,
                webPreferences: {
                  ...options.webPreferences,
                  backgroundThrottling: true,
                },
              }
            : options;
          const window = Reflect.construct(
            target,
            [effectiveOptions, ...rest],
            newTarget,
          );
          if (isHiddenAvatarOverlay && restoreVisibleFrameRate) {
            window.on?.("show", () => {
              window.webContents?.setBackgroundThrottling?.(false);
            });
            window.on?.("hide", () => {
              window.webContents?.setBackgroundThrottling?.(true);
            });
          }
          return window;
        },
      });
    }
    electronProxy = new Proxy(loaded, {
      get(target, property, receiver) {
        if (property === "protocol" && electronProtocolProxy) return electronProtocolProxy;
        if (property === "ipcMain" && electronIpcMainProxy) return electronIpcMainProxy;
        if (property === "BrowserWindow" && electronBrowserWindowProxy) {
          return electronBrowserWindowProxy;
        }
        return Reflect.get(target, property, receiver);
      },
    });
    return electronProxy;
  };
  for (const request of electronMainRequests) {
    try {
      const parent = typeof module === "object" ? module : undefined;
      Module._load(request, parent, false);
      if (electronProxy) break;
    } catch {}
  }
  globalThis.__CODEY_CODEX_STARTUP_PATCH__ = Object.freeze({
    disableWindowsOptimizations,
    disableMicro,
    disablePet,
    disableAppServerAnalytics: true,
    get disableDesktopCesAnalytics() {
      return !hasOptionalMainBundlePatchFailure("desktopCesAnalytics");
    },
    get appServerAnalyticsPatchCount() {
      return appServerAnalyticsPatchCount;
    },
    get appServerRuntimeOverrides() {
      return { ...appServerRuntimeOverrideEvidence };
    },
    get throttleExternalPluginFocusReconcile() {
      return !hasOptionalMainBundlePatchFailure(
        "externalPluginFocusReconcile",
      );
    },
    get externalPluginFocusReconcileSuppressedCount() {
      return externalPluginFocusReconcileSuppressedCount;
    },
    get disableAppStateHeartbeat() {
      return !hasOptionalMainBundlePatchFailure("appStateHeartbeat");
    },
    get optionalMainBundlePatchFailures() {
      return optionalMainBundlePatchFailures.map((failure) => ({ ...failure }));
    },
    get mainBundleSourcePatch() {
      return {
        attempted: mainBundleSourcePatchAttempted,
        filename: mainBundleFilename,
        patched: mainBundleSourcePatched,
      };
    },
    reclaimExecutionEnvironments: true,
    get executionResourceCleanup() {
      return executionProcessLifecycle.status;
    },
    restoreNativeModelAndSpeedControls: true,
    destroyTemporaryWebViews: true,
    throttleHiddenAvatarOverlay: disablePet,
    disableWindowsWmiSampler,
    get windowsWmiSampler() {
      return windowsWmiSamplerSnapshot();
    },
    get mainGitRequestGuard() {
      return mainGitRequestGuard.snapshot();
    },
  });
  setImmediate(() => {
    if (requireAppServerRuntimeOverrideValidation) return;
    try { process.getBuiltinModule("inspector").close(); } catch {}
  });
  return "codey-startup-patch-installed-v37";
})()
