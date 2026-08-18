(() => {
  const disablePet = __DISABLE_PET__;
  const fastCodexStartup = __FAST_CODEX_STARTUP__;
  const codeyErrorLoggerExecutable = "__CODEY_ERROR_LOGGER_EXECUTABLE__";
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
        operation.startsWith("optional_main_bundle_patch:");
      const stage = operation.startsWith("renderer_patch:")
        ? "startup.renderer_asset_patch"
        : operation.startsWith("optional_main_bundle_patch:")
          ? "startup.optional_main_bundle_patch"
          : "startup.main_process_patch";
      const result = process.getBuiltinModule("child_process").spawnSync(
        codeyErrorLoggerExecutable,
        ["--codey-record-error"],
        {
          input: JSON.stringify({
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
          }),
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
    } catch (logError) {
      try {
        console.error("[Codey] failed to write patch error log", logError);
      } catch {}
    }
  };
  const statsigBootstrapTimeoutMs = 1500;
  const threadOwnerDiscoveryTimeoutMs = 150;
  const statsigStartupRemainingMs =
    `Math.max(0,(globalThis.__CODEY_STATSIG_STARTUP_DEADLINE_MS__??=Date.now()+${statsigBootstrapTimeoutMs})-Date.now())`;
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
    version: 3,
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
    const requestInfo = (message) => {
      if (
        !message ||
        typeof message !== "object" ||
        message.type !== "worker-request" ||
        message.workerId !== "git"
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
      if (!targetMethods.has(method)) return null;
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
    const wrapGitHandler = (handler) => {
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
          message.workerId === "git"
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
        const info = requestInfo(message);
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
      const wrapped = wrapStatusHandler(wrapGitHandler(handler));
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
  // Each renderer gate is optional and independent. Codex bundles are minified
  // and reshape between releases, so a single drifted anchor must skip only its
  // own gate — never discard the sibling gates that are still compatible. That
  // is what previously hid the whole Fast/service-tier control on the builds
  // where one unrelated anchor moved: an exception here aborted every gate on
  // the asset. Log and return the source unchanged so the rest still apply.
  const replaceUniqueRendererGate = (source, pattern, replacement, name) => {
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
      const message =
        `Codey skipped an incompatible Codex renderer patch: ${name} gate matched ${matchCount} times`;
      recordCodeyPatchFailure(`renderer_patch:${name}`, message, { matchCount });
      try {
        console.error(message);
      } catch {}
      return source;
    }
    return patched;
  };
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
      source.includes("useHiddenModels:") &&
      source.includes("availableModels:") &&
      source.includes("includeUltraReasoningEffort") &&
      source.includes("amazonBedrock")
    ) {
      patched = replaceUniqueRendererGate(
        patched,
        /if\s*\(\s*\(*\s*(?:[$A-Z_a-z][$\w]*\s*(?:\?\.|\.)\s*has\(\s*[$A-Z_a-z][$\w]*\.model\s*\)\s*(?:===\s*!0)?\s*\|\|\s*)?\(?\s*([$A-Z_a-z][$\w]*)\s*\?\s*([$A-Z_a-z][$\w]*)\.has\(\s*([$A-Z_a-z][$\w]*)\.model\s*\)\s*:\s*(?:!\s*\3\.hidden|\3\.hidden\s*!==\s*!0|\3\.hidden\s*===\s*!1)\s*\)?\s*\)*\s*\)/g,
        (_match, useAllowlistName, allowlistName, modelName) =>
          `if(${useAllowlistName}?(${allowlistName}.has(${modelName}.model)||!${modelName}.hidden):!${modelName}.hidden)`,
        "model allowlist",
      );
    }
    if (
      source.includes("useHiddenModels:") &&
      source.includes("includeUltraReasoningEffort") &&
      source.includes("amazonBedrock")
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
      patched = replaceUniqueRendererGate(
        patched,
        /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*([$A-Z_a-z][$\w]*)\.availableOptions\.length\s*>\s*1(?=\s*[,;][\s\S]{0,2048}?`composer\.toggleFastMode`)/g,
        (_match, assignment, _resultName, settingsName) =>
          `${assignment}${settingsName}.availableOptions.length>1`,
        "model-aware service tier control",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /(`composer\.toggleFastMode`[\s\S]{0,512}?\{\s*enabled\s*:\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)\s*&&\s*([$A-Z_a-z][$\w]*)\s*!=\s*null/g,
        (_match, prefix, loadingName, fastOptionName) =>
          `${prefix}!${loadingName}&&${fastOptionName}!=null`,
        "model-aware Fast toggle",
      );
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
        /(\b([$A-Z_a-z][$\w]*)\s*=\s*)[$A-Z_a-z][$\w]*\s*&&\s*!\s*([$A-Z_a-z][$\w]*)(?=\s*,[\s\S]{0,8192}?modelPickerTriggerConfig\s*:\s*\2\s*\?)/g,
        (
          _match,
          assignment,
          _triggerConfigName,
          hideLabelName,
        ) => `${assignment}!${hideLabelName}`,
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
      fastCodexStartup &&
      source.includes(
        "CODEX_POST_LOGIN_STATSIG_BOOTSTRAP_FAILURE_TYPE_CLIENT_INITIALIZATION_FAILED",
      ) &&
      source.includes("Statsig: error while bootstrapping post-login client") &&
      source.includes("CodexStatsigProvider.sync")
    ) {
      // Keep the bootstrap call outside the inner StatsigClient block. Minified
      // bundles often reuse the bootstrap argument name for the client binding;
      // moving both into one block would put the argument in that binding's TDZ.
      // A timeout still rejects the mutation and enters the provider fallback.
      patched = replaceUniqueRendererGate(
        patched,
        /let\s+([$A-Z_a-z][$\w]*)\s*=\s*await\s+([$A-Z_a-z][$\w]*)\(\s*([$A-Z_a-z][$\w]*)\s*\)\s*;\s*try\s*\{\s*let\s+([$A-Z_a-z][$\w]*)\s*=\s*new\s+([$A-Z_a-z][$\w]*)\.StatsigClient\s*\(/g,
        (
          _match,
          payloadName,
          bootstrapName,
          bootstrapInputName,
          clientName,
          statsigModuleName,
        ) =>
          `let ${payloadName}=await Promise.race([${bootstrapName}(${bootstrapInputName}),new Promise((_,reject)=>globalThis.setTimeout(()=>reject(new Error("Codey Statsig bootstrap timeout")),${statsigStartupRemainingMs}))]);try{let ${clientName}=new ${statsigModuleName}.StatsigClient(`,
        "post-login Statsig bootstrap timeout",
      );
    }
    if (
      fastCodexStartup &&
      source.includes("useStatsigInternalClientFactoryAsync") &&
      source.includes("_getInstance") &&
      source.includes("loadingStatus")
    ) {
      // The SDK's anonymous-client hook otherwise keeps the entire route tree
      // behind its loading placeholder until initializeAsync settles.
      patched = replaceUniqueRendererGate(
        patched,
        /([$A-Z_a-z][$\w]*)\.loadingStatus\s*!==\s*`Ready`\s*&&\s*\1\.initializeAsync\(\)\.catch\s*\(/g,
        (_match, clientName) =>
          `${clientName}.loadingStatus!==\`Ready\`&&Promise.race([${clientName}.initializeAsync(),new Promise((_,reject)=>globalThis.setTimeout(()=>reject(new Error("Codey Statsig async initialization timeout")),${statsigStartupRemainingMs}))]).catch(`,
        "Statsig async client initialization timeout",
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
  const isCodexRendererAssetRequest = (request) => {
    try {
      const url = new URL(request?.url);
      return (
        url.protocol === "app:" &&
        url.pathname.includes("/assets/") &&
        (
          /\/(?:(?:app-initial|codex-composer-adapter|general-settings|model-list-filter|windows-model-controls|use-service-tier-settings|read-service-tier-for-request)(?:[~-][^/]*)?)\.js$/i.test(
            url.pathname,
          )
          || (
            disablePet
            && /\/(?:(?:appearance-settings|pet-settings|pets-settings)(?:[~-][^/]*)?)\.js$/i.test(
              url.pathname,
            )
          )
        )
      );
    } catch {
      return false;
    }
  };
  const patchCodexRendererResponse = async (request, response) => {
    if (!isCodexRendererAssetRequest(request) || response?.ok !== true) return response;
    const source = await response.clone().text();
    let patched;
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
    }
    if (patched === source) return response;
    const headers = new Headers(response.headers);
    headers.delete("content-length");
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
  const appServerRuntimeConfigs = [
    appServerAnalyticsConfig,
    ...nativeRuntimeConfigOverrides,
  ];
  const subagentGateRuntimeEnv = "CODEY_SUBAGENT_GATE_ACTIVE";
  const subagentGateRuntimeActive =
    typeof __SUBAGENT_GATE_ACTIVE__ === "boolean" &&
    __SUBAGENT_GATE_ACTIVE__;
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
  const hasAppServerConfigArg = (args, config) => {
    for (let index = 0; index < args.length; index += 1) {
      const argument = args[index];
      if (
        (argument === "-c" || argument === "--config") &&
        args[index + 1] === config
      ) {
        return true;
      }
      if (argument === `--config=${config}`) return true;
    }
    return false;
  };
  const rewriteCodexAppServerArgs = (args) => {
    if (!Array.isArray(args)) return args;
    const appServerIndexes = args
      .map((argument, index) => argument === "app-server" ? index : -1)
      .filter((index) => index >= 0);
    const analyticsFlagCount = args
      .filter((argument) => argument === "--analytics-default-enabled")
      .length;
    if (appServerIndexes.length !== 1 || analyticsFlagCount > 1) return args;

    const rewritten = args.filter(
      (argument) => argument !== "--analytics-default-enabled",
    );
    let hasAnalyticsConfig = false;
    for (let index = 0; index < rewritten.length; index += 1) {
      const argument = rewritten[index];
      if (
        (argument === "-c" || argument === "--config") &&
        /^analytics\.enabled=/.test(String(rewritten[index + 1] ?? ""))
      ) {
        rewritten[index + 1] = appServerAnalyticsConfig;
        hasAnalyticsConfig = true;
      } else if (/^--config=analytics\.enabled=/.test(String(argument))) {
        rewritten[index] = `--config=${appServerAnalyticsConfig}`;
        hasAnalyticsConfig = true;
      }
    }
    const appServerIndex = rewritten.indexOf("app-server");
    const missingRuntimeConfigs = appServerRuntimeConfigs.filter(
      (config) => !hasAppServerConfigArg(rewritten, config),
    );
    if (!hasAnalyticsConfig && !missingRuntimeConfigs.includes(appServerAnalyticsConfig)) {
      missingRuntimeConfigs.unshift(appServerAnalyticsConfig);
    }
    if (missingRuntimeConfigs.length > 0) {
      rewritten.splice(
        appServerIndex,
        0,
        ...missingRuntimeConfigs.flatMap((config) => ["-c", config]),
      );
    }
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
    const analyticsFlagMatches = execCommand.match(
      /(?:^|[\s;])--analytics-default-enabled(?=$|[\s;&|])/g,
    );
    if (appServerMatches?.length !== 1 || (analyticsFlagMatches?.length ?? 0) > 1) {
      return command;
    }

    let hasAnalyticsConfig = hasShellConfigArg(
      execCommand,
      appServerAnalyticsConfig,
    );
    let rewritten = execCommand.replace(
      /(^|[\s;])(-c|--config)\s+analytics\.enabled=[^\s;&|]+(?=$|[\s;&|])/g,
      (_match, prefix, configFlag) => {
        hasAnalyticsConfig = true;
        return `${prefix}${configFlag} ${appServerAnalyticsConfig}`;
      },
    );
    rewritten = rewritten.replace(
      /(^|[\s;])--config=analytics\.enabled=[^\s;&|]+(?=$|[\s;&|])/g,
      (_match, prefix) => {
        hasAnalyticsConfig = true;
        return `${prefix}--config=${appServerAnalyticsConfig}`;
      },
    );
    rewritten = rewritten.replace(
      /(^|[\s;])--analytics-default-enabled(?=$|[\s;&|])/g,
      (_match, prefix) => prefix,
    );
    const missingRuntimeConfigs = runtimeConfigs.filter(
      (config) =>
        config !== appServerAnalyticsConfig &&
        !hasShellConfigArg(rewritten, config),
    );
    const injectedConfigs = [
      ...(!hasAnalyticsConfig ? [appServerAnalyticsConfig] : []),
      ...missingRuntimeConfigs,
    ];
    if (injectedConfigs.length > 0) {
      rewritten = rewritten.replace(
        /\bapp-server\b/,
        `${injectedConfigs.map((config) => `-c ${shellQuote(config)}`).join(" ")} app-server`,
      );
    }
    let commandPrefix = command.slice(0, execCommandOffset);
    if (subagentGateRuntimeActive) {
      const execKeywordIndex = commandPrefix.lastIndexOf("exec");
      commandPrefix =
        commandPrefix.slice(0, execKeywordIndex) +
        `${subagentGateRuntimeEnv}=1 ` +
        commandPrefix.slice(execKeywordIndex);
    }
    return commandPrefix + rewritten;
  };
  const rewriteCodexAppServerSpawnArgs = (command, args) => {
    if (!Array.isArray(args)) return args;
    const commandName = String(command ?? "");
    if (/(?:^|[/\\])codex(?:\.exe)?$/i.test(commandName)) {
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
    const runtimeOverrideKey = (config) =>
      config.slice(0, Math.max(0, config.indexOf("=")));
    const wslReplacementKeys = new Set(
      wslOnlyRuntimeConfigOverrides.map(runtimeOverrideKey),
    );
    const wslRuntimeConfigs = [
      appServerAnalyticsConfig,
      ...nativeRuntimeConfigOverrides.filter(
        (config) => !wslReplacementKeys.has(runtimeOverrideKey(config)),
      ),
      ...wslOnlyRuntimeConfigOverrides,
    ].map(rewriteTomlWindowsPathsForWsl);
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

  // Workflow Engine V1 is deliberately opt-in. The launcher/WorkflowService
  // injects a per-launch loopback endpoint and random capability token. Keep the
  // token in the environment only; neither helper arguments nor status objects
  // contain it.
  const workflowProxyEnvironment = Object.freeze({
    enabled: "CODEY_WORKFLOW_PROXY_ENABLED",
    executable: "CODEY_WORKFLOW_PROXY_EXECUTABLE",
    controlAddress: "CODEY_WORKFLOW_PROXY_CONTROL_ADDR",
    capabilityToken: "CODEY_WORKFLOW_PROXY_TOKEN",
    bypass: "CODEY_WORKFLOW_PROXY_BYPASS",
  });
  const embeddedWorkflowProxyLaunchConfig = "__CODEY_WORKFLOW_PROXY_LAUNCH_CONFIG__";
  const isExactDirectCodexAppServerSpawn = (command, args) => {
    if (
      !/(?:^|[/\\])codex(?:\.exe)?$/i.test(String(command ?? "")) ||
      !Array.isArray(args)
    ) return false;
    const appServerIndexes = args
      .map((argument, index) => argument === "app-server" ? index : -1)
      .filter((index) => index >= 0);
    if (appServerIndexes.length !== 1) return false;

    // The desktop's direct invocation has only global config overrides before
    // the app-server subcommand. Reject lookalikes such as `codex exec
    // app-server`; trailing values belong to app-server itself and stay opaque.
    const appServerIndex = appServerIndexes[0];
    for (let index = 0; index < appServerIndex; index += 1) {
      const argument = args[index];
      if (argument === "-c" || argument === "--config") {
        if (
          index + 1 >= appServerIndex ||
          typeof args[index + 1] !== "string" ||
          args[index + 1].length === 0
        ) return false;
        index += 1;
        continue;
      }
      if (typeof argument === "string" && /^--config=.+/s.test(argument)) {
        continue;
      }
      return false;
    }
    return true;
  };
  const isLoopbackWorkflowControlAddress = (address) => {
    if (typeof address !== "string") return false;
    const match = /^(127(?:\.\d{1,3}){3}|\[::1\]):([1-9]\d{0,4})$/.exec(address);
    if (match == null) return false;
    const port = Number(match[2]);
    if (!Number.isSafeInteger(port) || port > 65535) return false;
    if (match[1] === "[::1]") return true;
    return match[1]
      .split(".")
      .every((part) => Number(part) >= 0 && Number(part) <= 255);
  };
  const spawnEnvironment = (rest) => {
    const options = rest[0];
    return options && typeof options === "object" && !Array.isArray(options)
      && options.env && typeof options.env === "object"
      ? options.env
      : process.env;
  };
  const workflowProxyConfigForSpawn = (command, args, rest) => {
    if (!isExactDirectCodexAppServerSpawn(command, args)) return null;
    const environment = spawnEnvironment(rest);
    const embedded = embeddedWorkflowProxyLaunchConfig
      && typeof embeddedWorkflowProxyLaunchConfig === "object"
      && !Array.isArray(embeddedWorkflowProxyLaunchConfig)
      ? embeddedWorkflowProxyLaunchConfig
      : null;
    if (
      (embedded == null && environment[workflowProxyEnvironment.enabled] !== "1") ||
      environment[workflowProxyEnvironment.bypass] === "1"
    ) return null;
    const executable = embedded?.executable
      ?? environment[workflowProxyEnvironment.executable];
    const controlAddress = embedded?.controlAddress
      ?? environment[workflowProxyEnvironment.controlAddress];
    const capabilityToken = embedded?.capabilityToken
      ?? environment[workflowProxyEnvironment.capabilityToken];
    if (
      typeof executable !== "string" ||
      executable.length === 0 ||
      !process.getBuiltinModule("path").isAbsolute(executable) ||
      !process.getBuiltinModule("fs").existsSync(executable) ||
      !isLoopbackWorkflowControlAddress(controlAddress) ||
      typeof capabilityToken !== "string" ||
      !/^[A-Za-z0-9_-]{32,512}$/.test(capabilityToken)
    ) return null;
    const normalizeExecutable = (value) => {
      const resolved = process.getBuiltinModule("path").resolve(value);
      return process.platform === "win32" ? resolved.toLowerCase() : resolved;
    };
    if (normalizeExecutable(executable) === normalizeExecutable(String(command))) {
      return null;
    }
    return { capabilityToken, controlAddress, environment, executable };
  };
  const withWorkflowProxyEnvironment = (rest, config) => {
    const options = rest[0];
    const inheritedEnvironment = options && typeof options === "object"
      && !Array.isArray(options) && options.env && typeof options.env === "object"
      ? options.env
      : process.env;
    const nextOptions = {
      ...(options && typeof options === "object" && !Array.isArray(options)
        ? options
        : {}),
      env: {
        ...inheritedEnvironment,
        [workflowProxyEnvironment.enabled]: "1",
        [workflowProxyEnvironment.executable]: config.executable,
        [workflowProxyEnvironment.controlAddress]: config.controlAddress,
        [workflowProxyEnvironment.capabilityToken]: config.capabilityToken,
      },
    };
    return options == null
      ? [nextOptions]
      : [nextOptions, ...rest.slice(1)];
  };
  const rewriteCodexAppServerProxySpawn = (command, args, rest = []) => {
    const config = workflowProxyConfigForSpawn(command, args, rest);
    if (config == null) return null;
    return {
      command: config.executable,
      args: [
        "--codey-app-server-proxy",
        "--codex-executable",
        String(command),
        "--",
        ...args,
      ],
      rest: withWorkflowProxyEnvironment(rest, config),
    };
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_REWRITE_CODEX_APP_SERVER_PROXY_SPAWN__",
    {
      configurable: false,
      value: rewriteCodexAppServerProxySpawn,
      writable: false,
    },
  );

  let appServerAnalyticsPatchCount = 0;
  let appServerWorkflowProxySpawnCount = 0;
  const childProcess = process.getBuiltinModule("child_process");
  const NativeSpawn = childProcess.spawn;
  if (!NativeSpawn.__codeyAppServerAnalyticsDisabled) {
    const isDirectCodexAppServerSpawn = (command, args) =>
      subagentGateRuntimeActive &&
      isExactDirectCodexAppServerSpawn(command, args);
    const withSubagentGateEnvironment = (rest) => {
      const options = rest[0];
      if (options == null) {
        return [{
          env: {
            ...process.env,
            [subagentGateRuntimeEnv]: "1",
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
        },
      }, ...rest.slice(1)];
    };
    const codeyAnalyticsDisabledSpawn = function (command, args, ...rest) {
      const rewritten = rewriteCodexAppServerSpawnArgs(command, args);
      const rewrittenRest = isDirectCodexAppServerSpawn(command, rewritten)
        ? withSubagentGateEnvironment(rest)
        : rest;
      const workflowProxySpawn = rewriteCodexAppServerProxySpawn(
        command,
        rewritten,
        rewrittenRest,
      );
      if (rewritten === args && rewrittenRest === rest) {
        if (workflowProxySpawn == null) {
          return Reflect.apply(NativeSpawn, this, arguments);
        }
      }
      if (rewritten !== args) appServerAnalyticsPatchCount += 1;
      if (workflowProxySpawn != null) appServerWorkflowProxySpawnCount += 1;
      return Reflect.apply(NativeSpawn, this, [
        workflowProxySpawn?.command ?? command,
        workflowProxySpawn?.args ?? rewritten,
        ...(workflowProxySpawn?.rest ?? rewrittenRest),
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
    const patched = source.replace(
      /async prewarm\(([$A-Z_a-z][$\w]*)\)\{if\(this\.window!=null\|\|this\.openingWindowPromise!=null\|\|this\.isAppQuitting\)return;let ([$A-Z_a-z][$\w]*)=this\.windowVisibilitySequence,([$A-Z_a-z][$\w]*)=await this\.ensureWindow\(\2\);/g,
      (match) => {
        count += 1;
        return match.replace("{", "{return;");
      },
    );
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
      powershell: /powershell(?:\.exe)?/i.test(source),
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
    const readWorkerSource = (filename, options) => {
      if (options?.eval === true) {
        return {
          cacheKey: null,
          source: String(filename ?? "").slice(
            0,
            maximumWmiWorkerSourceBytes,
          ),
        };
      }
      const specifier = workerSpecifierText(filename);
      if (/^data:/i.test(specifier)) {
        return {
          cacheKey: null,
          source: decodeDataWorkerSource(specifier),
        };
      }
      const path = workerFilePath(filename);
      if (!path) return null;
      return {
        cacheKey: path,
        source: process
          .getBuiltinModule("fs")
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

      const specifier = workerSpecifierText(filename);
      const cacheKey =
        options?.eval === true || /^data:/i.test(specifier)
          ? null
          : specifier;
      if (cacheKey && workerSourceMatchCache.has(cacheKey)) {
        const cached = workerSourceMatchCache.get(cacheKey);
        return cached ? { ...cached, workerName } : null;
      }

      try {
        const loaded = readWorkerSource(filename, options);
        if (!loaded) {
          rememberWorkerSourceMatch(cacheKey, null);
          return null;
        }
        windowsWmiSamplerEvidence.sourceInspections += 1;
        const sourceSignals = wmiSnapshotSourceSignals(loaded.source);
        windowsWmiSamplerEvidence.lastObservedSourceSignals = Object.entries(
          sourceSignals,
        )
          .filter(([, matched]) => matched)
          .map(([signal]) => signal);
        const matched = hasWmiSnapshotSourceSignature(sourceSignals);
        if (matched) {
          windowsWmiSamplerEvidence.sourceSignatureMatches += 1;
          const match = { reason: "source-signature", workerName };
          rememberWorkerSourceMatch(loaded.cacheKey, match);
          if (cacheKey && cacheKey !== loaded.cacheKey) {
            rememberWorkerSourceMatch(cacheKey, match);
          }
          return match;
        }
        windowsWmiSamplerEvidence.sourceSignatureMisses += 1;
        rememberWorkerSourceMatch(loaded.cacheKey, null);
        if (cacheKey && cacheKey !== loaded.cacheKey) {
          rememberWorkerSourceMatch(cacheKey, null);
        }
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

  const installExecutionReaper = ({
    connection,
    kill,
    snapshot,
    completionGraceMs: configuredCompletionGraceMs,
  }) => {
    const activeTurns = new Map();
    const completionGraceMs = configuredCompletionGraceMs ?? 1000;
    const reclaimRetryMs = 60 * 1000;
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
    let cleanupPromise = null;
    let reclaimTimer = null;
    let reclaimBarrier = null;
    let reclaimAuthorizedVersion = null;
    let disposed = false;
    let lastTurnActivityAt = Date.now();
    let turnStateVersion = 0;

    const isReclaimable = (processInfo) => {
      const command = String(processInfo?.command ?? "");
      // Configured MCP servers belong to the app-server session, not to one
      // turn. Killing them here forces Codex to reconnect and repeat capability
      // discovery (including resources/list) after every completed turn.
      return /(?:^|[/\\])node_repl(?:\.exe)?(?:\s|$)/i.test(command);
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
          const candidates = processes
            .filter(isReclaimable)
            .sort((left, right) => (right.depth ?? 0) - (left.depth ?? 0));
          let reclaimed = 0;
          let allKillsSucceeded = true;
          for (const processInfo of candidates) {
            // Yield once more immediately before each irreversible operation.
            if (!await waitForReclaimBarrier(expectedVersion, 0)) {
              break;
            }
            try {
              if (await kill(processInfo.pid) !== false) reclaimed += 1;
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

      if (method === "turn/started" && threadId != null && turnId != null) {
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
      const isCodexMainBundle =
        /[\\/]\.vite[\\/]build[\\/]main-[^\\/]+\.js$/i.test(filename);
      if (!isCodexMainBundle) {
        return Reflect.apply(originalJsExtension, this, arguments);
      }

      try {
      const fs = process.getBuiltinModule("fs");
      let source = fs.readFileSync(filename, "utf8");
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

      const attachFunctionPattern =
        /function [$A-Z_a-z][$\w]*\(\{getAuthToken:[$A-Z_a-z][$\w]*[^{}]{0,500},owner:([$A-Z_a-z][$\w]*)\}\)\{/g;
      let attachFunction = null;
      for (const candidate of source.matchAll(attachFunctionPattern)) {
        const nearby = source.slice(candidate.index, candidate.index + 2500);
        if (nearby.includes("will-attach-webview") && nearby.includes("did-attach-webview")) {
          attachFunction = candidate;
          break;
        }
      }
      if (!attachFunction) {
        throw new Error("Codey temporary WebView attach handler not found");
      }
      const attachOwnerName = attachFunction[1];
      const attachTail = source.slice(attachFunction.index, attachFunction.index + 3000);
      const shiftedEntry =
        /let ([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*\.shift\(\);if\(\1==null\)return;/.exec(attachTail);
      if (!shiftedEntry) {
        throw new Error("Codey temporary WebView attachment queue not found");
      }
      const guestReference = /webContents:([$A-Z_a-z][$\w]*)/.exec(
        attachTail.slice(shiftedEntry.index + shiftedEntry[0].length),
      );
      if (!guestReference) {
        throw new Error("Codey temporary WebView guest reference not found");
      }
      const trackOffset = attachFunction.index + shiftedEntry.index + shiftedEntry[0].length;
      source =
        source.slice(0, trackOffset) +
        `globalThis.__CODEY_TEMP_WEBVIEW_LIFECYCLE__.track(${attachOwnerName},${shiftedEntry[1]}.partition,${guestReference[1]});` +
        source.slice(trackOffset);

      const reaperAnchorPattern =
        /([$A-Z_a-z][$\w]*)\.add\(([$A-Z_a-z][$\w]*)\(\{appServerConnection:([$A-Z_a-z][$\w]*)\(\),closeActiveTurn:([$A-Z_a-z][$\w]*)\.closeActiveTurn\}\)\);/;
      const reaperAnchor = reaperAnchorPattern.exec(source);
      if (!reaperAnchor) {
        throw new Error("Codey execution reaper completion anchor not found");
      }
      const reaperTail = source.slice(reaperAnchor.index, reaperAnchor.index + 5000);
      const processManagerReference =
        /new [$A-Z_a-z][$\w]*\(([$A-Z_a-z][$\w]*)\.getBrowserSessionRegistry\(\)\)/.exec(reaperTail);
      if (!processManagerReference) {
        throw new Error("Codey execution process manager anchor not found");
      }
      const disposerName = reaperAnchor[1];
      const connectionFactoryName = reaperAnchor[3];
      const processManagerName = processManagerReference[1];
      const reaperInstall =
        `${disposerName}.add(globalThis.__CODEY_INSTALL_EXECUTION_REAPER__({` +
        `connection:${connectionFactoryName}(),` +
        `snapshot:()=>${processManagerName}.listProcessManagerSnapshot(),` +
        `kill:async pid=>(await ${processManagerName}.handlers["child-process-kill"]({pid})).killed` +
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
  const electronMainRequests = new Set(["electron", "electron/main"]);
  const installNativeIpcMainGuards = (ipcMain) => {
    if (!ipcMain) return false;
    let installed = false;
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
    electronProxy = new Proxy(loaded, {
      get(target, property, receiver) {
        if (property === "protocol" && electronProtocolProxy) return electronProtocolProxy;
        if (property === "ipcMain" && electronIpcMainProxy) return electronIpcMainProxy;
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
    fastCodexStartup,
    statsigBootstrapTimeoutMs,
    disableAppServerAnalytics: true,
    get disableDesktopCesAnalytics() {
      return !hasOptionalMainBundlePatchFailure("desktopCesAnalytics");
    },
    get appServerAnalyticsPatchCount() {
      return appServerAnalyticsPatchCount;
    },
    workflowProxyConfigured:
      process.env[workflowProxyEnvironment.enabled] === "1",
    get appServerWorkflowProxySpawnCount() {
      return appServerWorkflowProxySpawnCount;
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
    reclaimExecutionEnvironments: true,
    restoreNativeModelAndSpeedControls: true,
    destroyTemporaryWebViews: true,
    disableWindowsWmiSampler,
    get windowsWmiSampler() {
      return windowsWmiSamplerSnapshot();
    },
    get mainGitRequestGuard() {
      return mainGitRequestGuard.snapshot();
    },
  });
  setImmediate(() => {
    try { process.getBuiltinModule("inspector").close(); } catch {}
  });
  return "codey-startup-patch-installed-v22";
})()
