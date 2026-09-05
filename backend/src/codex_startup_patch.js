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
          codex: readCodexAppVersion(),
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
  const Module = process.getBuiltinModule("module");
  const originalLoad = Module._load;
  const readCodexAppVersion = () => {
    try {
      const parent = typeof module === "object" ? module : undefined;
      const electron = Reflect.apply(originalLoad, Module, ["electron", parent, false]);
      const version = electron?.app?.getVersion?.();
      return typeof version === "string" && version.trim()
        ? version.trim()
        : undefined;
    } catch {
      return undefined;
    }
  };
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
      // Promise consumers update React Query before the diagnostic response
      // event runs. Rewrite model/list at the resolver boundary so the native
      // result can never replace Codey's current route catalog.
      patched = replaceUniqueRendererGate(
        patched,
        /(([$A-Z_a-z][$\w]*)\.resolve\()([$A-Z_a-z][$\w]*)(\),this\.emitRequestLifecycleEvent\(\{type:`completed`,hostId:this\.hostId,method:\2\.method)/g,
        (_match, prefix, requestName, resultName, suffix) =>
          `${prefix}${resultName}=globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult?.(${requestName}.method,${resultName})??${resultName}${suffix}`,
        "app server model result rewrite",
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
  const nativeRuntimeConfigOverrides = Array.isArray(codeyRuntimeConfigOverrides)
    ? codeyRuntimeConfigOverrides.filter(
        (entry) => typeof entry === "string" && entry.length > 0,
      )
    : [];
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
  const runtimeConfigValue = (configs, key) => {
    for (let index = configs.length - 1; index >= 0; index -= 1) {
      const config = configs[index];
      if (runtimeOverrideKey(config) !== key) continue;
      const value = config.slice(config.indexOf("=") + 1).trim();
      try {
        return JSON.parse(value);
      } catch {
        return value.replace(/^'(.*)'$/s, "$1");
      }
    }
    return null;
  };
  const localRouterRuntimeEnabled = runtimeConfigValue(nativeRuntimeConfigOverrides, "model_provider") === "codey_router";
  if (localRouterRuntimeEnabled) {
    // A shared daemon or external WebSocket can retain another provider and
    // never read this launch's overrides. Use Codex's own process-local mode.
    process.env.CODEX_APP_SERVER_FORCE_CLI = "1";
  }
  const threadTitleModelId = "gpt-5.6-luna";
  const selectThreadTitleModel = (
    configs = nativeRuntimeConfigOverrides,
    suppliedCatalogModels = null,
  ) => {
    const providerId = String(runtimeConfigValue(configs, "model_provider") ?? "").trim();
    const defaultModel = String(runtimeConfigValue(configs, "model") ?? "").trim();
    const officialAccountAvailable =
      providerId === "openai" ||
      runtimeConfigValue(
        configs,
        `model_providers.${providerId}.requires_openai_auth`,
      ) === true;
    if (officialAccountAvailable) return threadTitleModelId;

    let catalogModels = suppliedCatalogModels;
    if (!Array.isArray(catalogModels)) {
      try {
        const catalogPath = runtimeConfigValue(configs, "model_catalog_json");
        const catalog = JSON.parse(
          process.getBuiltinModule("fs").readFileSync(catalogPath, "utf8"),
        );
        catalogModels = catalog?.models;
      } catch {
        catalogModels = [];
      }
    }
    const modelByKey = new Map(
      catalogModels
        .map((model) => String(model?.slug ?? "").trim())
        .filter(Boolean)
        .map((model) => [model.toLowerCase(), model]),
    );
    const routeSeparator = defaultModel.indexOf("/");
    const thirdPartyLuna = routeSeparator > 0
      ? `${defaultModel.slice(0, routeSeparator)}/${threadTitleModelId}`
      : threadTitleModelId;
    return modelByKey.get(thirdPartyLuna.toLowerCase()) || defaultModel;
  };
  const threadTitleModel = selectThreadTitleModel();
  Object.defineProperty(globalThis, "__CODEY_THREAD_TITLE_MODEL__", {
    configurable: false,
    value: threadTitleModel,
    writable: false,
  });
  Object.defineProperty(globalThis, "__CODEY_SELECT_THREAD_TITLE_MODEL__", {
    configurable: false,
    value: selectThreadTitleModel,
    writable: false,
  });
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
    return null;
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
  const createSubagentGateRuntimeId = () => typeof randomUuid === "function"
    ? randomUuid()
    : `${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  const rewriteCodexAppServerArgs = (args) => {
    if (!Array.isArray(args)) return args;
    const appServerIndexes = args
      .map((argument, index) => argument === "app-server" ? index : -1)
      .filter((index) => index >= 0);
    if (appServerIndexes.length !== 1) return args;
    if (localRouterRuntimeEnabled && args.some((arg) => arg === "proxy" || arg === "daemon")) {
      throw new Error("本地路由模式不能使用 app-server proxy/daemon；请移除自定义后台服务启动命令");
    }

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
    // Keep Codey's overrides in the app-server command's own config layer.
    // Apply them last: a later parent-table override can otherwise replace
    // model_providers.codey_router even though every managed key is present.
    rewritten.push(
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
    return args;
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
      const runtimeId = createSubagentGateRuntimeId();
      const options = rest[0];
      if (options == null) {
        return [{
          env: {
            ...process.env,
            [subagentGateRuntimeEnv]: "1",
            [subagentGateRuntimeIdEnv]: runtimeId,
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
          [subagentGateRuntimeIdEnv]: runtimeId,
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

  // Codex fixes metadata generation to Luna. Keep that choice for an available
  // official account, otherwise use the selected third-party route's Luna or
  // its default model. The native caller already preserves its provisional
  // local title when metadata generation fails.
  const patchCodexMainThreadTitleModel = (source) => {
    const titleCalls = [...source.matchAll(
      /await\s+([$A-Z_a-z][$\w]*)\(\{[^{}]{0,1000}\bfeature:(`thread_title`|"thread_title"|'thread_title')/g,
    )];
    if (titleCalls.length !== 1) {
      throw new Error(`Codey thread title call matched ${titleCalls.length} times`);
    }
    const helperName = titleCalls[0][1];
    const helperStart = source.indexOf(`async function ${helperName}({`);
    const signatureEnd = source.indexOf("}){", helperStart);
    const helperEnd = source.indexOf("}function ", signatureEnd);
    if (helperStart < 0 || signatureEnd < 0 || helperEnd < 0) {
      throw new Error("Codey thread title metadata helper not found");
    }
    const helper = source.slice(helperStart, helperEnd + 1);
    const featureName = /\bfeature:([$A-Z_a-z][$\w]*)/.exec(
      source.slice(helperStart, signatureEnd),
    )?.[1];
    const nativeModelName = /\bmodel:([$A-Z_a-z][$\w]*)/.exec(helper)?.[1];
    if (!featureName || !nativeModelName) {
      throw new Error("Codey thread title metadata fields not found");
    }
    const escapedNativeModelName = nativeModelName.replace(/[$]/g, "\\$&");
    const nativeModelPattern = new RegExp(
      `\\bmodel:${escapedNativeModelName}\\b`,
      "g",
    );
    const modelMatches = helper.match(nativeModelPattern)?.length ?? 0;
    if (modelMatches !== 3) {
      throw new Error(
        `Codey thread title metadata model matched ${modelMatches} times`,
      );
    }
    const selectedModel =
      `${featureName}===\`thread_title\`?` +
      `globalThis.__CODEY_THREAD_TITLE_MODEL__||${nativeModelName}:` +
      nativeModelName;
    const patchedHelper = helper.replace(
      nativeModelPattern,
      `model:${selectedModel}`,
    );
    return source.slice(0, helperStart) + patchedHelper + source.slice(helperEnd + 1);
  };
  Object.defineProperty(
    globalThis,
    "__CODEY_PATCH_CODEX_MAIN_THREAD_TITLE_MODEL__",
    {
      configurable: false,
      value: patchCodexMainThreadTitleModel,
      writable: false,
    },
  );

  const workerThreads = process.getBuiltinModule("worker_threads");
  const NativeWorker = workerThreads.Worker;
  if (!NativeWorker.__codeyNoInspectWrapper) {
    class CodeyNoInspectWorker extends NativeWorker {
      constructor(filename, options = {}) {
        super(filename, { ...options, execArgv: options.execArgv ?? [] });
      }
    }
    Object.defineProperty(CodeyNoInspectWorker, "__codeyNoInspectWrapper", {
      value: true,
    });
    workerThreads.Worker = CodeyNoInspectWorker;
    try {
      Module.syncBuiltinESMExports?.();
    } catch (error) {
      recordCodeyPatchFailure("sync_worker_threads_esm_exports", error);
    }
  }

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

  // Install the main-bundle telemetry and model patches before compilation.
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
      source = applyOptionalMainBundlePatch(
        "threadTitleModel",
        patchCodexMainThreadTitleModel,
        source,
      );
      globalThis.__CODEY_EXTERNAL_PLUGIN_FOCUS_RECONCILE_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("externalPluginFocusReconcile");
      globalThis.__CODEY_DESKTOP_ANALYTICS_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("desktopCesAnalytics");
      globalThis.__CODEY_APP_STATE_HEARTBEAT_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("appStateHeartbeat");
      globalThis.__CODEY_THREAD_TITLE_MODEL_SOURCE_PATCHED__ =
        !hasOptionalMainBundlePatchFailure("threadTitleModel");
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
  const electronMainRequests = new Set(["electron", "electron/main"]);
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
    electronProxy = new Proxy(loaded, {
      get(target, property, receiver) {
        if (property === "protocol" && electronProtocolProxy) return electronProtocolProxy;
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
    get routeThreadTitleModel() {
      return !hasOptionalMainBundlePatchFailure("threadTitleModel");
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
    restoreNativeModelAndSpeedControls: true,
  });
  setImmediate(() => {
    if (requireAppServerRuntimeOverrideValidation) return;
    try { process.getBuiltinModule("inspector").close(); } catch {}
  });
  return "codey-startup-patch-installed-v38";
})()
