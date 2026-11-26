// Codex 渲染进程资产源补丁。原本是主进程 protocol.handle 里对 app:// 响应
// 做改写的纯函数部分；Electron fuse 关闭后主进程注入不可用，改由 Codey 的补丁
// 服务在 Fetch 响应暂停时逐资产调用 globalThis.__codeyRendererPatchEntry 完成
// 同样的改写，结果按资产文件名缓存在 Codey 侧。
// 本文件只依赖页面 JS 环境，不使用任何 Node/Electron API。
(() => {
  // 渲染进程没有 --codey-record-error 子进程可写：补丁失败经 CDP
  // Runtime.addBinding 暴露的 __codeyPatchLog 回传给 Rust 记录。
  const recordCodeyPatchFailure = (operation, error, context = {}) => {
    try {
      const message = error instanceof Error
        ? `${error.name}: ${error.message}${error.stack ? `\n${error.stack}` : ""}`
        : String(error || "unknown patch failure");
      const record = JSON.stringify({ operation, message, context });
      const reporter = globalThis.__codeyPatchLog;
      if (typeof reporter === "function") {
        reporter(record);
      } else {
        console.error("[Codey]", record);
      }
    } catch {}
  };
  const maxRendererPatchFailureKeys = 64;
  const rendererPatchFailuresByKey = new Map();
  let activeRendererPatchFailures = null;
  let activeRendererPatchKey = null;
  const rendererPatchFailuresForKey = (key, create = true) => {
    if (!key) return null;
    const existing = rendererPatchFailuresByKey.get(key);
    if (existing) {
      // Refresh insertion order so the bounded map behaves as an LRU.
      rendererPatchFailuresByKey.delete(key);
      rendererPatchFailuresByKey.set(key, existing);
      return existing;
    }
    if (!create) return null;
    const failures = new Set();
    rendererPatchFailuresByKey.set(key, failures);
    while (rendererPatchFailuresByKey.size > maxRendererPatchFailureKeys) {
      const oldest = rendererPatchFailuresByKey.keys().next().value;
      rendererPatchFailuresByKey.delete(oldest);
    }
    return failures;
  };
  // Each renderer gate is optional and independent. Codex bundles are minified
  // and reshape between releases, so a single drifted anchor must skip only its
  // own gate — never discard the sibling gates that are still compatible. That
  // is what previously hid the whole Fast/service-tier control on the builds
  // where one unrelated anchor moved: an exception here aborted every gate on
  // the asset. Log and return the source unchanged so the rest still apply.
  // 只截取传入的静态资产源码，不读取输入框、会话状态或凭据。
  const rendererGateDiagnosticAnchors = [
    "serviceTierAccess:",
    "supportsIndependentSpeedModes",
    "includeUltraReasoningEffort",
    "isCustomModelProvider",
    "electron-app-state-snapshot-request",
    "submitDisabledReasons:",
    "checkout-webview-presentation-changed",
    "totalASTLRUCacheSize:",
    "threadScroll.restore",
    "orbit-character-preloaded",
    "\"general-settings\",\"appearance\",\"keyboard-shortcuts\"",
    "\"orbit.navigation\"",
    "PerformanceEventTiming",
    "this.eventObserver=new PerformanceObserver",
    "EventLoopBlockRenderer",
    "ensureObserver(\"long-animation-frame\")",
    "_pending_observations",
    "Reached the post-login Statsig bootstrap deadline",
    "CodexStatsigProvider.async.identity",
    "useStatsigInternalClientFactoryAsync=function(",
    "review.fileSource.breadcrumb.ariaLabel",
    "tokenizeTimeLimit:0",
    "operationSource:\"turn_diff_analytics\"",
    ".appServices.customAvatars.load()",
    "huD:()=>5e3",
    "chatgpt-work-flat-project-sidebar-mode-v2",
    "bluetooth_signal_ready",
    "removeConversationBrowserStateIfEmpty(",
    "request-user-input-auto-resolution-changed",
  ];
  const rendererGateFailureExcerpts = (source) => {
    const excerpts = [];
    const ranges = [];
    for (const anchor of rendererGateDiagnosticAnchors) {
      if (excerpts.length >= 2) break;
      const index = source.indexOf(anchor);
      if (index < 0) continue;
      if (ranges.some(([start, end]) => index >= start && index < end)) continue;
      const start = Math.max(0, index - 120);
      const end = Math.min(source.length, start + 640);
      ranges.push([start, end]);
      excerpts.push(
        source
          .slice(start, end)
          .replace(/[^\x20-\x7E]/g, "?"),
      );
    }
    return excerpts;
  };
  const recordIncompatibleRendererGate = (source, name, matchCount) => {
    if (activeRendererPatchFailures == null && activeRendererPatchKey != null) {
      activeRendererPatchFailures = rendererPatchFailuresForKey(
        activeRendererPatchKey,
        true,
      );
    }
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
  const draftFactoryPattern = /\b[$A-Z_a-z][$\w]*=\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\)\([$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*=>\(\{isManuallyChanged:!1,modelSettings:null,serviceTier:null\}\)\)/g;
  // 把未手动选择的新草稿的推理强度初值换成 medium；管理员下发的新会话设置和
  // 草稿内的手动选择保持原样。返回改写后的源码；无法唯一识别时返回匹配次数。
  const patchNativeDraftReasoningEffort = (source) => {
    const hooks = [...source.matchAll(
      /function [$A-Z_a-z][$\w]*\(([$A-Z_a-z][$\w]*)\)\{(?:var [$\w,]+;)?let (?:[$A-Z_a-z][$\w]*,)*[$A-Z_a-z][$\w]*=\(0,[$A-Z_a-z][$\w]*\.c\)\(\d+\),\{((?=[^{}]*\bmanagedNewThreadModelSettings:)(?=[^{}]*\bdraftModelSettings:)(?=[^{}]*\bisNewThreadDraft:)[^{}]*)\}=\1,/g,
    )];
    if (hooks.length !== 1) return hooks.length;
    const hook = hooks[0];
    const bindings = Object.fromEntries(hook[2].split(",").map((entry) => entry.split(":")));
    const managed = bindings.managedNewThreadModelSettings;
    const draft = bindings.draftModelSettings;
    const isDraft = bindings.isNewThreadDraft;
    if (![managed, draft, isDraft].every((binding) => /^[$A-Z_a-z][$\w]*$/.test(binding ?? ""))) return 0;
    const hookEnd = source.indexOf("}function ", hook.index + hook[0].length);
    if (hookEnd < 0) return 0;
    const scope = source.slice(hook.index, hookEnd);
    const calls = [...scope.matchAll(/(\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\))\(\{userSavedModelString:/g)];
    if (calls.length !== 1) return calls.length;
    const resolver = calls[0][1];
    const call = `${resolver}({userSavedModelString:`;
    const callStart = source.indexOf(call, hook.index);
    const nextCall = callStart < 0 ? -1 : source.indexOf(call, callStart + 1);
    if (callStart < 0 || callStart > hookEnd || (nextCall >= 0 && nextCall < hookEnd)) return 0;
    // 参数里含有对象展开，按括号配对找到调用结尾。
    const argsStart = callStart + resolver.length + 1;
    let depth = 0;
    let argsEnd = -1;
    for (let index = argsStart; index < hookEnd; index += 1) {
      const char = source[index];
      if (char === "{" || char === "(" || char === "[") depth += 1;
      else if (char === "}" || char === ")" || char === "]") {
        depth -= 1;
        if (depth === 0) {
          argsEnd = index + 1;
          break;
        }
      }
    }
    if (argsEnd < 0 || source[argsEnd] !== ")") return 0;
    const args = source.slice(argsStart, argsEnd);
    // 模型不在目录中时沿用原生结果，模型不支持 medium 时由原生解析器回退。
    const replacement = `/*codey-new-thread-draft-defaults*/((__codeyArgs,__codeyResolved=${resolver}(__codeyArgs))=>${isDraft}&&${draft}==null&&${managed}==null&&__codeyArgs.listModelsData?.models?.some(e=>e.model===__codeyResolved.model)?${resolver}({...__codeyArgs,userSavedModelString:__codeyResolved.model,userSavedReasoningEffort:\`medium\`}):__codeyResolved)(${args})`;
    return source.slice(0, callStart) + replacement + source.slice(argsEnd + 1);
  };
  const patchNewThreadDraftDefaults = (source) => {
    const name = "new thread draft defaults";
    if (source.includes("/*codey-new-thread-draft-defaults*/")
      || activeRendererPatchFailures?.has(name)) return source;
    const patched = patchNativeDraftReasoningEffort(source);
    if (typeof patched !== "string") return recordIncompatibleRendererGate(source, name, patched);
    return patched;
  };
  // 每项改写的复合前置规则。资源一条都不命中时整段跳过；Codey 后端读取同一份
  // 规则表在 ASAR 离线扫描和把资源送进页面求值前预筛，allOf 按最长/最稀有字面量靠前排列。
  const rendererPatchRules = /*codey-renderer-patch-rules*/[
    { "allOf": ["modelSettingsPersistAsDefault:", "userSavedReasoningEffort:", "draftModelSettings:", "isNewThreadDraft:"] },
    { "allOf": ["isManuallyChanged:!1,modelSettings:null,serviceTier:null"] },
    { "allOf": ["submitDisabledReasons:", "CodexComposerAdapter", "resolvedHostId:", "let{agentMode:"] },
    { "allOf": ["Failed to send message from view", "codex-message-from-view", "sendMessageFromView"] },
    { "allOf": ["AppServerRequestClient is missing a message dispatcher", "mcp_request_enqueued"] },
    { "allOf": ["checkout-webview-presentation-changed", "isPrewarm:"] },
    { "allOf": ["Desktop host did not provide its AppServerManager service"] },
    { "allOf": ["useUserMessageAdmission:", ".sendFollowUpMessage("] },
    { "allOf": ["onRequestDispatched:", ".startTurn("] },
    { "allOf": ["includeUltraReasoningEffort:", "isCustomModelProvider:", "teamCatalog:", "1186680773"] },
    { "allOf": ["supportsIndependentSpeedModes", "ultrafast_mode"] },
    { "allOf": ["={serviceTierAccess:", "isLoading:"] },
    { "allOf": ["electron-app-state-snapshot-request", "collectAppStateSnapshot("] },
    { "allOf": ["activeInteractions=new Map", "beginCpuSampling", "ensureHeartbeat("] },
    { "allOf": ["totalASTLRUCacheSize:", "workerFactory:", "poolSize:"] },
    { "allOf": ["previousViewportHeightPx:", "threadScroll.initialRead", "threadScroll.restore"] },
    { "allOf": ["orbit-character-preloaded", "allow-same-origin"] },
    { "allOf": ["\"general-settings\",\"appearance\",\"keyboard-shortcuts\""] },
    { "allOf": ["ensureObserver(\"long-animation-frame\")", "ensureObserver(\"longtask\")"] },
    { "allOf": ["_pending_observations", "pending_age_ms", "setInterval("] },
    { "allOf": ["PerformanceEventTiming", "durationThreshold:16", "eventsSupported:"] },
    { "allOf": ["this.eventObserver=new PerformanceObserver", "durationThreshold:16"] },
    { "allOf": ["rendererHangDetectionEnabled", "EventLoopBlockRenderer"] },
    { "kind": "css", "allOf": [":has([data-state=open])"] },
    { "kind": "css", "allOf": [":has(:is(button[data-state=open]))"] },
    { "allOf": ["Timed out while fetching post-login Statsig bootstrap", "Reached the post-login Statsig bootstrap deadline"] },
    { "allOf": ["CodexStatsigProvider.async.identity"] },
    { "allOf": ["useStatsigInternalClientFactoryAsync=function(", ".initializeAsync().catch("] },
    { "allOf": ["review.fileSource.breadcrumb.ariaLabel", "currentCSSZoom", "paddingInlineEnd"] },
    { "allOf": ["tokenizeTimeLimit:0", "tokenizeMaxLineLength:", "cssVariablePrefix:"] },
    { "allOf": ["operationSource:\"turn_diff_analytics\"", "turn-diff-capture-start", "turn-diff-capture-complete", "visualizationSnapshot:"] },
    { "allOf": [".appServices.customAvatars.load()", "\"custom-avatars\""] },
    { "allOf": ["huD:()=>5e3"] },
    { "allOf": ["chatgpt-work-flat-project-sidebar-mode-v2", "SIDEBAR_PROJECT_THREAD_ORDERS", "manualThreadIds:"] },
    { "allOf": ["bluetooth_signal_ready", "hands_free", "Microphone input ended"] },
    { "allOf": ["removeConversationBrowserStateIfEmpty(", "ownerRoutePathsByConversation.delete(", "evictConversation("] },
    { "allOf": ["request-user-input-auto-resolution-changed", "request-user-input-auto-resolution-snapshot", "owlBrowserCrashCounter"] }
  ]/*end-codey-renderer-patch-rules*/;
  const jsRendererPatchRules = rendererPatchRules.filter((rule) => rule.kind !== "css");
  const hostedTurnCall = "(globalThis.__codeyModelWhitelistPatch?.hostedTurnCall??((t,m,...a)=>t[m](...a)))";
  const hostedFollowUpSend = /\b([$A-Z_a-z][$\w]*)\.sendFollowUpMessage\((?=[$A-Z_a-z][$\w]*,\{\.\.\.[$A-Z_a-z][$\w]*,useUserMessageAdmission:)/g;
  const hostedFollowUpSendAnchor = /\.sendFollowUpMessage\([$A-Z_a-z][$\w]*,\{\.\.\.[$A-Z_a-z][$\w]*,useUserMessageAdmission:/;
  const hostedTurnStart = /\b([$A-Z_a-z][$\w]*)\.startTurn\((?=[$A-Z_a-z][$\w]*,\{\.\.\.[$A-Z_a-z][$\w]*,context:\{\.\.\.[$A-Z_a-z][$\w]*\.context,onRequestDispatched:)/g;
  const hostedTurnStartAnchor = /\.startTurn\([$A-Z_a-z][$\w]*,\{\.\.\.[$A-Z_a-z][$\w]*,context:\{\.\.\.[$A-Z_a-z][$\w]*\.context,onRequestDispatched:/;
  const patchComposerQuotaGates = (patched, source) => {
    if (!(
      source.includes("CodexComposerAdapter")
      && source.includes("let{agentMode:")
      && source.includes("resolvedHostId:")
      && source.includes("submitDisabledReasons:")
      && !source.includes("/*codey-local-quota-gate*/")
    )) {
      return patched;
    }
    // Codex 26.1120 起把 usage-limit 与 reserve-limit 合并进共享额度原因原子，
    // 经 let 变量汇总进 submitDisabledReasons。仅在本机允许第三方线路发送时清空
    // 该额度原因，保留共享订阅、其他禁用原因与 React Compiler 缓存失效。
    return replaceUniqueRendererGate(
      patched,
      /(?<prefix>\{activeMode:(?<mode>[$A-Z_a-z][$\w]*),modelSettings:[\s\S]{0,8000}?resolvedHostId:(?<host>[$A-Z_a-z][$\w]*),[\s\S]{0,8000}?)(?<reasons>let [$A-Z_a-z][$\w]*=void 0!==[$A-Z_a-z][$\w]*&&[$A-Z_a-z][$\w]*&&[`"]caller-disabled[`"][\s\S]{0,2000}?\[(?:[$A-Z_a-z][$\w]*,)+(?<quota>[$A-Z_a-z][$\w]*)\]\.filter\(Boolean\)[\s\S]{0,4000}?submitDisabledReasons:[$A-Z_a-z][$\w]*,)/g,
      (...args) => {
        const { prefix, reasons, host, mode, quota } = args.at(-1);
        return `${prefix}/*codey-local-quota-gate*/if(${quota}&&(()=>{try{return globalThis.__codeyQuotaUnlock?.allowsLocalSend?.({hostId:${host},model:${mode}?.settings?.model})===true}catch{return false}})())${quota}="";${reasons}`;
      },
      "local third-party composer quota",
    );
  };

  const patchThreadScrollMeasurements = (patched, source) => {
    if (!source.includes("threadScroll.restore") || !source.includes("threadScroll.initialRead")
      || !source.includes("previousViewportHeightPx:")) return patched;

    if (!source.includes("/*codey-scroll-restore-distance*/")) {
      // 顶部和阅读锚点各有自己的恢复距离，先选分支可以省去即将被覆盖的尺寸读取。
      const distance = new RegExp([
        String.raw`,(?<distance>[$\w]+)=(?<saved>null==[$\w]+\?[$\w]+:Math\.max\(0,(?<rail>[$\w]+)\.scrollHeight-\k<rail>\.clientHeight-[$\w]+\));`,
        String.raw`(?<top>"top"===[$\w]+)\?\k<distance>=(?<maximum>Math\.max\(0,\k<rail>\.scrollHeight-\k<rail>\.clientHeight\)):`,
        String.raw`(?<anchored>[$\w]+&&null!=(?<anchor>[$\w]+\.current))&&\(\k<distance>=(?<anchoredValue>\k<anchor>\+\([$\w]+\(\)\?\.getHeightPx\(\)\?\?0\))\);`,
        String.raw`(?=let [$\w]+=\k<rail>\.scrollTop;\(0,[$\w]+\.[$\w]+\)\("threadScroll\.restore",\(\)=>\(0,[$\w]+\.[$\w]+\)\(\k<rail>,\k<distance>\)\))`,
      ].join(""), "g");
      patched = replaceUniqueRendererGate(patched, distance, (...args) => {
        const { distance, top, maximum, anchored, anchoredValue, saved } = args.at(-1);
        return `,${distance}=/*codey-scroll-restore-distance*/${top}?${maximum}:${anchored}?${anchoredValue}:${saved};`;
      }, "thread scroll restoration distance");
    }
    if (!source.includes("__codeyViewportHeight")) {
      patched = replaceUniqueRendererGate(
        patched,
        /let ([$\w]+)=([$\w]+)!==([$\w]+)\.clientHeight;if\(\2=\3\.clientHeight,\1&&0===\2\)/g,
        (_match, changed, previous, viewport) =>
          `let __codeyViewportHeight=${viewport}.clientHeight,${changed}=${previous}!==__codeyViewportHeight;${previous}=__codeyViewportHeight;if(${changed}&&0===${previous})`,
        "thread resize viewport height",
      );
    }
    if (!source.includes("/*codey-scroll-pinned-top*/")) {
      // 顶部固定时直接按可滚动范围定位，不先读取只在其他分支使用的距底距离。
      patched = replaceUniqueRendererGate(
        patched,
        /(let [$\w]+=\(0,[$\w]+\.IA\)\(([$\w]+)\);)(if\("top"===([$\w]+)&&[$\w]+\.current\[\4\]\)return void [$\w]+\(Math\.max\(0,\2\.scrollHeight-\2\.clientHeight\),"instant"\);)(?=if\("bottom"===\4&&)/g,
        (_match, distanceRead, _viewport, pinnedTop) =>
          `/*codey-scroll-pinned-top*/${pinnedTop}${distanceRead}`,
        "thread resize pinned top",
      );
    }
    return patched;
  };

  const patchPerformanceAndPrewarmGates = (patched, source) => {
    patched = patchThreadScrollMeasurements(patched, source);
    if (
      source.includes("checkout-webview-presentation-changed")
      && source.includes("isPrewarm:")
      && !source.includes("/*codey-personal-checkout-prewarm*/")
    ) {
      // 个人版定价与额度充值共用 CheckoutPersonalWebviewHost：
      // 仅在存在实际路由参数时启用宿主，并跳过伴随挂载的隐藏预热 webview。
      patched = replaceUniqueRendererGate(
        patched,
        /,([$A-Z_a-z][$\w]*)=null!=([$A-Z_a-z][$\w]*)\|\|[$A-Z_a-z][$\w]*\|\|[$A-Z_a-z][$\w]*;(?=return\s*[^;]{0,300}?\{chatGptOrigin:[$A-Z_a-z][$\w]*,enabled:\1,search:\2\})/g,
        (_match, enabled, search) => `,${enabled}=/*codey-personal-checkout-prewarm*/null!=${search};`,
        "personal checkout webview prewarm",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /(\b[$A-Z_a-z][$\w]*=)\(0,[$A-Z_a-z][$\w]*\.jsx\)\([`"]div[`"],\{[`"]aria-hidden[`"]:!0,inert:!0,className:[`"]pointer-events-none invisible fixed inset-0[`"],children:\(0,[$A-Z_a-z][$\w]*\.jsx\)\([$A-Z_a-z][$\w]*,\{isPrewarm:!0,partition:[$A-Z_a-z][$\w]*,search:[$A-Z_a-z][$\w]*\}\)\}\)/g,
        (_match, assign) => `${assign}null`,
        "personal checkout prewarm element",
      );
    }
    if (source.includes("electron-app-state-snapshot-request")
      && !source.includes("/*codey-heartbeat-snapshot-skip*/")
      && source.includes("case\"electron-app-state-snapshot-request\":")) {
      // Main asks the focused window for a full app-state snapshot every 30 s,
      // then adds process sampling, a debug log and a Sentry breadcrumb. Skip
      // only the heartbeat reason; other diagnostic triggers still reply, and
      // main expires unanswered requests on its own TTL.
      patched = replaceUniqueRendererGate(
        patched,
        /(case"electron-app-state-snapshot-request":\{)(?=[\s\S]{0,2000}?collectAppStateSnapshot\(([$A-Z_a-z][$\w]*)\.hostId,\2\.reason\)[\s\S]{0,2000}?dispatchMessage\("electron-app-state-snapshot-response"[^;]{0,400};break((?: [$A-Z_a-z][$\w]*)?)\})/g,
        (_match, prefix, messageName, label) =>
          `${prefix}/*codey-heartbeat-snapshot-skip*/if("heartbeat"===${messageName}.reason)break${label};`,
        "app state heartbeat snapshot",
      );
    }
    if (
      source.includes("activeInteractions=new Map") &&
      source.includes("beginCpuSampling") &&
      source.includes("ensureHeartbeat(){null==this.heartbeatTimer&&(this.heartbeatTimer=setInterval")
    ) {
      // Codey launches app-server with analytics.enabled=false, so renderer
      // interaction telemetry is discarded after paying for main/renderer CPU
      // snapshots and a 1 Hz heartbeat. Preserve span lifecycle semantics while
      // removing only those two recurring/IPC costs.
      patched = replaceUniqueRendererGate(
        patched,
        /cpuSampling:"dropped"===[$A-Z_a-z][$\w]*\|\|!0===[$A-Z_a-z][$\w]*\.backfilled\?null:this\.beginCpuSampling\(\)/g,
        "cpuSampling:null",
        "interaction CPU sampling",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /ensureHeartbeat\(\)\{null==this\.heartbeatTimer&&\(this\.heartbeatTimer=setInterval\(\(\)=>\{let ([$A-Z_a-z][$\w]*)=this\.now\(\),([$A-Z_a-z][$\w]*)=this\.wallNow\(\);for\(let ([$A-Z_a-z][$\w]*) of this\.activeInteractions\.values\(\)\)this\.recordHeartbeat\(\3,\1,\2\)\},[$\w.]+\)\)\}/g,
        "ensureHeartbeat(){}",
        "interaction heartbeat",
      );
    }
    if (
      !source.includes("/*codey-pierre-pool-size*/")
      && source.includes("totalASTLRUCacheSize:")
      && (/poolSize:[2-9]\d*/.test(source) || /totalASTLRUCacheSize:(?:3[3-9]|[4-9]\d|\d{3,})\b/.test(source))
    ) {
      // 代码高亮与 diff 池收敛到单常驻 Web Worker 且限制 AST LRU 缓存容量，减少线程与内存占用。
      patched = replaceUniqueRendererGate(
        patched,
        /(\{workerFactory:[$A-Z_a-z][$\w]*,poolSize:)[1-9]\d*(,totalASTLRUCacheSize:)\d+(\})/g,
        (_match, prefix, mid, suffix) => `${prefix}/*codey-pierre-pool-size*/1${mid}32${suffix}`,
        "syntax highlight worker pool",
      );
    }
    if (
      !source.includes("/*codey-shiki-tokenize-time-limit*/")
      && source.includes("tokenizeTimeLimit:0")
      && source.includes("tokenizeMaxLineLength:")
      && source.includes("cssVariablePrefix:")
    ) {
      // 恢复 Shiki 单行语法高亮超时上限（250ms），避免超长单行或病理正则让 Worker 持续满载卡死（#55225）。
      patched = replaceUniqueRendererGate(
        patched,
        /(cssVariablePrefix:(?:\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\)|[$A-Z_a-z][$\w]*)\("token"\),tokenizeMaxLineLength:[$A-Z_a-z][$\w]*,tokenizeTimeLimit:)0\b/g,
        (_match, prefix) => `${prefix}/*codey-shiki-tokenize-time-limit*/250`,
        "shiki tokenize time limit",
      );
    }
    if (
      !source.includes("/*codey-orbit-preload-skip*/")
      && source.includes("orbit-character-preloaded")
      && source.includes("allow-same-origin")
    ) {
      // Orbit 角色预热会挂载隐藏的 cross-origin-isolated iframe 拉起独立 WASM 线程，改为按需打开再加载。
      patched = replaceUniqueRendererGate(
        patched,
        /function ([$A-Z_a-z][$\w]*)\(\)\{let [$A-Z_a-z][$\w]*=\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\)\(\);if\([`"]u[`"]<typeof document\|\|null==[$A-Z_a-z][$\w]*\|\|\([$A-Z_a-z][$\w]*\.prefetch\(\),[$A-Z_a-z][$\w]*\)\)return;[\s\S]{0,1200}?[`"]orbit-character-preloaded[`"]===[$A-Z_a-z][$\w]*\.data&&[$A-Z_a-z][$\w]*\(\)\},\{signal:[$A-Z_a-z][$\w]*\.signal\}\),window\.addEventListener\([`"]pagehide[`"],[$A-Z_a-z][$\w]*,\{once:!0,signal:[$A-Z_a-z][$\w]*\.signal\}\),document\.body\.append\([$A-Z_a-z][$\w]*\)\}/g,
        (_match, fnName) => `function ${fnName}(){/*codey-orbit-preload-skip*/}`,
        "orbit character preload iframe",
      );
    }
    if (
      !source.includes("/*codey-startup-prefetch-skip*/")
      && /[`"]general-settings[`"],[`"]appearance[`"],[`"]keyboard-shortcuts[`"]/.test(source)
    ) {
      // 启动阶段会批量预加载数十个设置页与对话框分包，改为打开对应页面时再按需拉取。
      patched = replaceUniqueRendererGate(
        patched,
        /function ([$A-Z_a-z][$\w]*)\(\)\{[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\.prefetch\(\),function\(\)\{for\(let [$A-Z_a-z][$\w]* of\([^;]{0,800}?\[[`"]general-settings[`"],[`"]appearance[`"],[`"]keyboard-shortcuts[`"]\]\)\)[\s\S]{0,2000}?\.then\([$A-Z_a-z][$\w]*\.bind\([$A-Z_a-z][$\w]*,[`"][^`"]+[`"]\)\)\.catch\(\(\)=>void 0\)\}\(\)\}/g,
        (_match, fnName) => `function ${fnName}(){/*codey-startup-prefetch-skip*/}`,
        "startup secondary route prefetch",
      );
    }
    if (
      !source.includes("/*codey-orbit-nav-prefetch-skip*/")
      && source.includes("\"orbit.navigation\"")
      && source.includes(".prefetch()")
      && /[`"]general-settings[`"],[`"]appearance[`"],[`"]keyboard-shortcuts[`"]/.test(source)
    ) {
      // 启动阶段会为 orbit 导航额外预取多个分包与状态，改为进入页面时再按需拉取。
      patched = replaceUniqueRendererGate(
        patched,
        /(let ([$A-Z_a-z][$\w]*)=\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\)\([$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*,\{name:[`"]orbit\.navigation[`"]\}\)\?\.isCapable;[$A-Z_a-z][$\w]*\[\d+\]!==\2\?\(([$A-Z_a-z][$\w]*)=\(\)=>\{)\2&&\([$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\.prefetch\(\),[\s\S]{0,200}?\.catch\(\(\)=>void 0\)\)(\},)/g,
        (_match, prefix, _capable, _effectVar, suffix) =>
          `${prefix}/*codey-orbit-nav-prefetch-skip*/${suffix}`,
        "orbit navigation startup prefetch",
      );
    }
    if (
      !source.includes("/*codey-frame-gap-observer-skip*/")
      && source.includes("durationThreshold:16")
      && source.includes("PerformanceEventTiming")
      && source.includes("eventsSupported:")
    ) {
      // 交互卡顿采样器在每个交互期间维持全帧率 rAF 循环和 16ms 事件观察器；遥测已关闭，直接返回空句柄。
      patched = replaceUniqueRendererGate(
        patched,
        /Z:function\(([$A-Z_a-z][$\w]*)\)\{return 0===([$A-Z_a-z][$\w]*)\.size&&\([$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*\([`"]event[`"],[$A-Z_a-z][$\w]*\),document\.addEventListener\([`"]visibilitychange[`"],[$A-Z_a-z][$\w]*\),[$A-Z_a-z][$\w]*\(\)\),\2\.add\(\1\),\{eventsSupported:null!=[$A-Z_a-z][$\w]*,stop:\(\)=>\{\2\.has\(\1\)&&\([\s\S]{0,240}?document\.removeEventListener\([`"]visibilitychange[`"],[$A-Z_a-z][$\w]*\),[$A-Z_a-z][$\w]*\(\)\)\)\}\}\}/g,
        "Z:function(){/*codey-frame-gap-observer-skip*/return{eventsSupported:!1,stop(){}}}",
        "frame gap event observer",
      );
    }
    if (
      !source.includes("/*codey-startup-event-observer-skip*/")
      && source.includes("durationThreshold:16")
      && source.includes("this.eventObserver=new PerformanceObserver")
    ) {
      // 启动阶段会创建 30 秒的 16ms 交互事件 PerformanceObserver 收集首屏遥测，停用观察器但保留启动完成流程。
      patched = replaceUniqueRendererGate(
        patched,
        /try\{[`"]u[`"]>typeof PerformanceObserver&&PerformanceObserver\.supportedEntryTypes\?\.includes\([`"]event[`"]\)&&\(this\.eventObserver=new PerformanceObserver\([$A-Z_a-z][$\w]*=>this\.addEvents\([$A-Z_a-z][$\w]*\.getEntries\(\)\)\),this\.eventObserver\.observe\(\{type:[`"]event[`"],buffered:!0,durationThreshold:16\}\),this\.interactions\.supported=!0\)\}catch\{this\.eventObserver\?\.disconnect\(\),this\.eventObserver=void 0\}/g,
        "/*codey-startup-event-observer-skip*/void 0;",
        "startup event observer",
      );
    }
    if (
      !source.includes("/*codey-hang-detection-skip*/")
      && source.includes("EventLoopBlockRenderer")
      && source.includes("rendererHangDetectionEnabled")
    ) {
      // 禁用渲染进程事件循环卡顿探测（每 500ms 发送 IPC 心跳并触发 V8 栈采样）。
      patched = replaceUniqueRendererGate(
        patched,
        /[$A-Z_a-z][$\w]*\.rendererHangDetectionEnabled&&(?=function\([$A-Z_a-z][$\w]*\)\{[\s\S]{0,240}?name:[`"]EventLoopBlockRenderer[`"])/g,
        "/*codey-hang-detection-skip*/!1&&",
        "renderer hang detection",
      );
    }
    if (
      !source.includes("/*codey-interaction-loaf-skip*/")
      && /ensureObserver\([`"]long-animation-frame[`"]\)/.test(source)
      && /ensureObserver\([`"]longtask[`"]\)/.test(source)
    ) {
      // 交互跟踪器每次启动都额外创建 longtask / LoAF PerformanceObserver 并缓存记录，直接返回不支持。
      patched = replaceUniqueRendererGate(
        patched,
        /ensureObserver\(([$A-Z_a-z][$\w]*)\)\{try\{if\(!this\.isSupported\(\1\)\)return!1\}catch\{return!1\}if\(null!=\([`"]longtask[`"]===\1\?this\.longTaskObserver:this\.longAnimationFrameObserver\)\)return!0;[\s\S]{0,400}?this\.longAnimationFrameObserver=[$A-Z_a-z][$\w]*,!0\}catch\{return!1\}\}/g,
        (_match, kind) => `ensureObserver(${kind}){/*codey-interaction-loaf-skip*/return!1}`,
        "interaction loaf observers",
      );
    }
    if (
      !source.includes("/*codey-pending-observations-sampler-skip*/")
      && source.includes("_pending_observations")
      && source.includes("pending_age_ms")
    ) {
      // 协同编辑观测跟踪器在存在未结束观测时会每 10s 轮询并绑定整组文档可见性监听，保留 ping 心跳，停用定时采样。
      patched = replaceUniqueRendererGate(
        patched,
        /1===([$A-Z_a-z][$\w]*)\.size&&\([$A-Z_a-z][$\w]*=setInterval\(\(\)=>\{for\(let [$A-Z_a-z][$\w]* of\[\.\.\.\1\]\)[$A-Z_a-z][$\w]*\.sample\(\)\},1e4\),[\s\S]{0,240}?window\.addEventListener\([`"]pagehide[`"],[$A-Z_a-z][$\w]*,!0\)\)/g,
        "/*codey-pending-observations-sampler-skip*/0",
        "codex pages pending observation sampler",
      );
    }
    if (
      !source.includes("/*codey-statsig-bootstrap-deadline*/")
      && source.includes("Reached the post-login Statsig bootstrap deadline")
      && source.includes("Timed out while fetching post-login Statsig bootstrap")
    ) {
      // 登录态 Statsig 启动拉取默认阻塞最多 62.5s 才渲染路由，收敛上限让弱网快速回退默认配置。
      patched = replaceUniqueRendererGate(
        patched,
        /(Error\([`"]Reached the post-login Statsig bootstrap deadline[`"]\)\)\)\},)(?:[1-9]\d{4,}|[1-9]\d*e[4-9])(\);[\s\S]{0,600}?Error\([`"]Timed out while fetching post-login Statsig bootstrap[`"]\)\)\},)(?:[1-9]\d{3,}|[1-9]\d*e[4-9])(\)\})/g,
        (_match, deadlinePrefix, attemptPrefix, suffix) =>
          `${deadlinePrefix}/*codey-statsig-bootstrap-deadline*/12e3${attemptPrefix}/*codey-statsig-bootstrap-attempt*/8e3${suffix}`,
        "statsig post-login bootstrap timeout",
      );
    }
    if (
      !source.includes("/*codey-statsig-identity-nonblocking*/")
      && source.includes("CodexStatsigProvider.async.identity")
    ) {
      // 登录态 bootstrap 失败回退时只等本地令牌解析，不再串行阻塞等待同域名的远端账号检查。
      patched = replaceUniqueRendererGate(
        patched,
        /if\(([$A-Z_a-z][$\w]*)\|\|[$A-Z_a-z][$\w]*&&![$A-Z_a-z][$\w]*\)(?=\{let [$A-Z_a-z][$\w]*;return[^}]{0,260}?debugName:[`"]CodexStatsigProvider\.async\.identity[`"])/g,
        (_match, localAccountLoading) =>
          `if(${localAccountLoading}/*codey-statsig-identity-nonblocking*/)`,
        "statsig fallback identity non-blocking",
      );
    }
    if (
      !source.includes("/*codey-statsig-async-init-cap*/")
      && source.includes("useStatsigInternalClientFactoryAsync=function(")
      && source.includes(".initializeAsync().catch(")
    ) {
      // 回退或非登录态的异步客户端默认阻塞重试约 32s；超时后先以空评估置为 Ready 挂载界面，保留后台请求与自动刷新。
      patched = replaceUniqueRendererGate(
        patched,
        /(\buseStatsigInternalClientFactoryAsync=function\([$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*\)\{let ([$A-Z_a-z][$\w]*)=\(0,[$A-Z_a-z][$\w]*\.useMemo\)\([\s\S]{0,240}?\[[$A-Z_a-z][$\w]*,([$A-Z_a-z][$\w]*)\]=\(0,[$A-Z_a-z][$\w]*\.useState\)\([`"]Ready[`"]!==\2\.loadingStatus\);return\(0,[$A-Z_a-z][$\w]*\.useMemo\)\(\(\)=>\{)[`"]Ready[`"]!==\2\.loadingStatus&&\2\.initializeAsync\(\)\.catch\(([$A-Z_a-z][$\w]*\.Log\.error)\)\.finally\(\(\)=>\3\(!1\)\)(\},\[\]\),\{client:\2,isLoading:[$A-Z_a-z][$\w]*\}\})/g,
        (_match, prefix, clientName, setLoadingName, logErrorExpr, suffix) =>
          `${prefix}"Ready"!==${clientName}.loadingStatus&&(()=>{/*codey-statsig-async-init-cap*/let __codeyDone=!1,__codeyFinish=()=>{__codeyDone||(__codeyDone=!0,clearTimeout(__codeyTimer),"Loading"===${clientName}.loadingStatus&&typeof ${clientName}._finalizeUpdate==="function"&&${clientName}._finalizeUpdate(null),${setLoadingName}(!1))},__codeyTimer=setTimeout(__codeyFinish,3000);${clientName}.initializeAsync().catch(${logErrorExpr}).finally(__codeyFinish)})()${suffix}`,
        "statsig async client init cap",
      );
    }
    if (
      !source.includes("/*codey-breadcrumb-subpixel-guard*/")
      && source.includes("review.fileSource.breadcrumb.ariaLabel")
      && source.includes("currentCSSZoom")
      && source.includes("paddingInlineEnd")
    ) {
      // 文件预览面包屑增加 0.5px 亚像素溢出容差，并在恢复可见路径段循环之后再计算前导省略号 showRoot，防止 ResizeObserver 振荡死循环（#62669）。
      patched = replaceUniqueRendererGate(
        patched,
        /,(?<overflow>[$A-Z_a-z][$\w]*)=(?<avail>[$A-Z_a-z][$\w]*)<(?<widths>[$A-Z_a-z][$\w]*)\.reduce\((?<sumFn>[$A-Z_a-z][$\w]*|\([^)]*\)=>[^,]+),0\)\?\k<widths>\.length-1:0,(?<rem>[$A-Z_a-z][$\w]*)=\k<avail>-(?<lastWidth>[$A-Z_a-z][$\w]*)-(?<ellipsisWidth>[$A-Z_a-z][$\w]*),(?<root>[$A-Z_a-z][$\w]*)=\k<overflow>>0&&\k<rem>>=0;(?<loop>for\(;\k<overflow>>0&&\k<rem>>=Math\.min\(\k<widths>\[\k<overflow>-1\],\k<ellipsisWidth>\+[$A-Z_a-z][$\w]*\);\)\k<overflow>-=1,\k<rem>-=\k<widths>\[\k<overflow>\];)(?=let [$A-Z_a-z][$\w]*=`\$\{[$A-Z_a-z][$\w]*\/(?:\()?[$A-Z_a-z][$\w]*\.currentCSSZoom)/g,
        (...args) => {
          const { overflow, avail, widths, sumFn, rem, lastWidth, ellipsisWidth, root, loop } = args.at(-1);
          return `,${overflow}=/*codey-breadcrumb-subpixel-guard*/(${avail}+.5)<${widths}.reduce(${sumFn},0)?${widths}.length-1:0,${rem}=${avail}-${lastWidth}-${ellipsisWidth};${loop}let ${root}=${overflow}>0&&${rem}>=0;`;
        },
        "file breadcrumb resize observer guard",
      );
    }
    if (
      !source.includes("/*codey-turn-diff-analytics-skip*/")
      && source.includes("operationSource:\"turn_diff_analytics\"")
      && source.includes("turn-diff-capture-start")
      && source.includes("turn-diff-capture-complete")
      && source.includes("visualizationSnapshot:")
    ) {
      // 停用每轮遥测专用的后台 Git 全仓快照与可视化文件扫描，保留手动 Review 面板的 review_model 与 git 操作（#54955）。
      patched = replaceUniqueRendererGate(
        patched,
        /capture:async function\([$\w,]+\)\{try\{return await \(0,[$\w]+\.[$\w]+\)\("git"\)\.request\(\{method:"turn-diff-capture-start",params:\{[^{}]*operationSource:"turn_diff_analytics"[^{}]*\}\}\)\}catch\([$\w]+\)\{return [$\w.]+\("Failed to start turn diff analytics capture",[\s\S]{0,160}?,null\}\}\([^;]{0,200}?\),visualizationSnapshot:\(async function\([$\w,]+\)\{[\s\S]{0,2400}?"Failed to start visualization turn diff snapshot",\{safe:\{\},sensitive:\{error:[$\w]+\}\}\),null\)\)/g,
        "capture:/*codey-turn-diff-analytics-skip*/Promise.resolve(null),visualizationSnapshot:Promise.resolve(null)",
        "turn diff analytics start capture",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /(let\[(?<diffResult>[$A-Z_a-z][$\w]*),[$A-Z_a-z][$\w]*\]=await Promise\.all\(\[)\(0,[$\w]+\.[$\w]+\)\("git"\)\.request\(\{method:"turn-diff-capture-complete",params:\{[^{}]*operationSource:"turn_diff_analytics"[^{}]*\}\}\),null==[$A-Z_a-z][$\w]*\|\|![$A-Z_a-z][$\w]*\(\)\?null:\(async function\([$\w,=]+\)\{[\s\S]{0,2400}?"Failed to complete visualization turn diff snapshot",\{safe:\{\},sensitive:\{error:[$\w]+\}\}\),null\)\)\](\);if\()(?![\s\S]{0,24}null==)/g,
        (...args) => {
          const [, prefix, , suffix] = args;
          const { diffResult } = args.at(-1);
          return `${prefix}/*codey-turn-diff-complete-skip*/Promise.resolve(null),null]${suffix}null==${diffResult}||`;
        },
        "turn diff analytics complete capture",
      );
    }
    if (
      !source.includes("/*codey-custom-avatars-slim-skip*/")
      && source.includes(".appServices.customAvatars.load()")
      && source.includes("\"custom-avatars\"")
    ) {
      // 宠物精简开启时短路 customAvatars.load()，避免解码超大自定义精灵图导致开屏 512MiB RPC 卡死（#61498）。
      patched = replaceUniqueRendererGate(
        patched,
        /(\{queryKey:[$A-Z_a-z][$\w]*,queryFn:\(\)=>)(?<call>[$A-Z_a-z][$\w]*\.appServices\.customAvatars\.load\(\))(?=,enabled:!0,networkMode:"always")/g,
        (_match, prefix, call) =>
          `${prefix}/*codey-custom-avatars-slim-skip*/(globalThis.__codeyPetControlShield?.enabled?Promise.resolve([]):${call})`,
        "custom avatars slim short-circuit",
      );
    }
    if (
      !source.includes("/*codey-paste-attachment-threshold*/")
      && source.includes("huD:()=>5e3")
    ) {
      // 将粘贴文本自动转 .txt/.md 附件的阈值从 5,000 字提升至 500,000 字（#25144）。
      patched = replaceUniqueRendererGate(
        patched,
        /\bhuD:\(\)=>5e3\b/g,
        "huD:()=>/*codey-paste-attachment-threshold*/5e5",
        "paste attachment character threshold",
      );
    }
    if (
      !source.includes("/*codey-project-groups-updated-sort*/")
      && source.includes("chatgpt-work-flat-project-sidebar-mode-v2")
      && source.includes("SIDEBAR_PROJECT_THREAD_ORDERS")
      && source.includes("manualThreadIds:")
    ) {
      // 侧边栏切换为按最近更新排序（updated_at）时，同步按组内最新会话时间排列未固定项目组（#31836）。
      patched = replaceUniqueRendererGate(
        patched,
        /(?<groupsMap>\b(?<groups>[$A-Z_a-z][$\w]*)=\k<groups>\.map\([$A-Z_a-z][$\w]*=>\{let [$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*\(\{getRecencyAt:(?<recency>[$A-Z_a-z][$\w]*),items:[\s\S]{0,320}?,sortMode:(?<sortMode>[$A-Z_a-z][$\w]*)\}\),[$A-Z_a-z][$\w]*=\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\)\([$A-Z_a-z][$\w]*\.threadKeys,[$A-Z_a-z][$\w]*\);return [$A-Z_a-z][$\w]*===[$A-Z_a-z][$\w]*\.threadKeys\?[$A-Z_a-z][$\w]*:\{\.\.\.[$A-Z_a-z][$\w]*,threadKeys:[$A-Z_a-z][$\w]*\}\}\)),(?="manual"===[$A-Z_a-z][$\w]*&&)/g,
        (...args) => {
          const { groupsMap, groups, recency, sortMode } = args.at(-1);
          return `${groupsMap},/*codey-project-groups-updated-sort*/("updated_at"===${sortMode}&&(${groups}=${groups}.map((__g,__i)=>({__g,__i,__r:__g.threadKeys.length>0?${recency}(__g.threadKeys[0]):-1/0})).sort((__a,__b)=>__b.__r-__a.__r||__a.__i-__b.__i).map(__e=>__e.__g))),`;
        },
        "sidebar project groups updated_at sort",
      );
    }
    if (
      !source.includes("/*codey-bluetooth-clock-stopped-guard*/")
      && source.includes("bluetooth_signal_ready")
      && source.includes("hands_free")
      && source.includes("Microphone input ended")
    ) {
      // AirPods 等蓝牙麦克风已持续产出有效非静音 PCM 帧（E>=3）但 CoreAudio 时钟回报 stopped/unavailable 时立即放行，避免 10s 误超时（#63267）。
      patched = replaceUniqueRendererGate(
        patched,
        /(?<head>if\([$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*,(?<ready>[$A-Z_a-z][$\w]*)=!1,null!=[$A-Z_a-z][$\w]*\)\{[\s\S]{0,240}?)(?<stable>[$A-Z_a-z][$\w]*)=(?<prev>[$A-Z_a-z][$\w]*)===(?<curr>[$A-Z_a-z][$\w]*)&&\("recognized"===\k<curr>\|\|"hands_free"===\k<curr>\)(?<mid>;\k<ready>=\k<stable>&&(?<streak>[$A-Z_a-z][$\w]*)>=3\|\|"number"==typeof \k<prev>&&"number"==typeof \k<curr>&&\k<curr>>\k<prev>[\s\S]{0,180}?else \k<ready>=!1\})/g,
        (...args) => {
          const { head, ready, stable, prev, curr, mid, streak } = args.at(-1);
          return `${head}${stable}=/*codey-bluetooth-clock-stopped-guard*/${prev}===${curr}&&(null===${curr}||"unavailable"===${curr}||"recognized"===${curr}||"hands_free"===${curr})${mid}else ${ready}=${streak}>=3;`;
        },
        "bluetooth dictation clock stopped fallback",
      );
    }
    if (
      !source.includes("/*codey-browser-sidebar-dispose*/")
      && source.includes("removeConversationBrowserStateIfEmpty(")
      && source.includes("ownerRoutePathsByConversation.delete(")
      && source.includes("evictConversation(")
    ) {
      // 会话删除、归档或丢弃时同步清理渲染侧浏览器边栏宿主与媒体标签状态，避免后台残留 webview（#61121）。
      patched = replaceUniqueRendererGate(
        patched,
        /([$\w.]+\.subscribe\("browser-sidebar-destroy-webview",)/g,
        (_match, subscribeCall) =>
          `globalThis.__codeyDisposeBrowserSidebarConversation=__id=>{try{this.removeConversationTabs(__id),this.emitMediaChange()}catch{}},${subscribeCall}`,
        "browser sidebar conversation disposer export",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /(evictConversation\(([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*)\)\{)(?=switch\(\3\.reason\)\{case"archive":case"archive-notification":)/g,
        (_match, prefix, convId, opts) =>
          `${prefix}/*codey-browser-sidebar-dispose*/("delete"===${opts}.reason||"archive"===${opts}.reason||"archive-notification"===${opts}.reason||"discard"===${opts}.reason)&&globalThis.__codeyDisposeBrowserSidebarConversation?.(${convId});`,
        "evict conversation browser sidebar cleanup",
      );
    }
    if (
      !source.includes("/*codey-user-input-auto-resolve-disable*/")
      && source.includes("request-user-input-auto-resolution-changed")
      && source.includes("request-user-input-auto-resolution-snapshot")
      && source.includes("owlBrowserCrashCounter")
    ) {
      // 禁用 AI 提问卡片超时自动跳过：向宿主登记 snooze 取消倒计时，并将前端状态固定为 snoozed（#29702/#31651）。
      patched = replaceUniqueRendererGate(
        patched,
        /(([$A-Z_a-z][$\w]*)\.appServices\.owlBrowserCrashCounter\.subscribe)/g,
        (_match, expr, servicesHolder) => `(globalThis.__codeyAppServices=${servicesHolder}.appServices,${expr})`,
        "desktop appServices reference for input snooze",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /(case"request-user-input-auto-resolution-changed":)(if\("timed-out"===([$A-Z_a-z][$\w]*)\.change\.kind\)\{[\s\S]{0,240}?;break\}\(0,([$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*)\)\(([$A-Z_a-z][$\w]*),\3\.hostId,\3\.change\),"removed"===\3\.change\.kind&&\([^;]+\);break;case"request-user-input-auto-resolution-snapshot":for\(let\{conversationId:([$A-Z_a-z][$\w]*),requestId:([$A-Z_a-z][$\w]*),resolutionState:([$A-Z_a-z][$\w]*)\}of \3\.pendingRequests\)\(0,\4\)\(\5,\3\.hostId,\{kind:"updated",conversationId:\6,requestId:\7,resolutionState:\8\}\);break)/g,
        (_match, caseHead, body, msg, _applyState, _store, snapConv, snapReq, snapState) =>
          `${caseHead}/*codey-user-input-auto-resolve-disable*/(()=>{let __s=globalThis.__codeySnoozedInputRequests??(globalThis.__codeySnoozedInputRequests=new Set),__k=\`\${${msg}.hostId}:\${${msg}.change.requestId}\`;if("removed"===${msg}.change.kind)__s.delete(__k);else if("updated"===${msg}.change.kind&&"snoozed"!==${msg}.change.resolutionState?.status&&!__s.has(__k)){__s.size>=256&&__s.clear(),__s.add(__k);try{globalThis.__codeyAppServices?.requestUserInputAutoResolution?.snooze?.({conversationId:${msg}.change.conversationId,hostId:${msg}.hostId,requestId:${msg}.change.requestId})}catch{}}})();${body.replace(`for(let{conversationId:${snapConv},requestId:${snapReq},resolutionState:${snapState}}of ${msg}.pendingRequests)`, `for(let{conversationId:${snapConv},requestId:${snapReq},resolutionState:${snapState}}of ${msg}.pendingRequests){let __s=globalThis.__codeySnoozedInputRequests??(globalThis.__codeySnoozedInputRequests=new Set),__k=\`\${${msg}.hostId}:\${${snapReq}}\`;if("snoozed"!==${snapState}?.status&&!__s.has(__k)){__s.size>=256&&__s.clear(),__s.add(__k);try{globalThis.__codeyAppServices?.requestUserInputAutoResolution?.snooze?.({conversationId:${snapConv},hostId:${msg}.hostId,requestId:${snapReq}})}catch{}}}`)}`,
        "user input auto resolution snooze dispatch",
      );
      patched = replaceUniqueRendererGate(
        patched,
        /(case"removed":\{let [$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*\.get\([$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*\);[$A-Z_a-z][$\w]*\?\.requestId===([$A-Z_a-z][$\w]*)\.requestId&&[$A-Z_a-z][$\w]*\.set\([$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*,null\);break\}case"updated":[$A-Z_a-z][$\w]*\.set\([$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*,\{requestId:\2\.requestId,resolutionState:)\2\.resolutionState(\}\))/g,
        (_match, prefix, _change, suffix) => `${prefix}{status:"snoozed"}${suffix}`,
        "user input auto resolution state snoozed",
      );
    }
    return patched;
  };
  const patchCodexRendererAsset = (source, requestUrl) => {
    if (!jsRendererPatchRules.some((rule) => rule.allOf.every((anchor) => source.includes(anchor)))) {
      return source;
    }
    const previousRendererPatchFailures = activeRendererPatchFailures;
    const previousRendererPatchKey = activeRendererPatchKey;
    const patchKey = `${requestUrl}:${source.length}`;
    activeRendererPatchKey = patchKey;
    activeRendererPatchFailures = rendererPatchFailuresByKey.size > 0
      ? rendererPatchFailuresForKey(patchKey, false)
      : null;
    try {
    let patched = source;
    if (source.includes("isNewThreadDraft:")
      && source.includes("modelSettingsPersistAsDefault:")
      && source.includes("userSavedReasoningEffort:")
      && source.includes("draftModelSettings:")) {
      patched = patchNewThreadDraftDefaults(patched);
    }
    if (source.includes("isManuallyChanged:!1,modelSettings:null,serviceTier:null")) {
      patched = replaceUniqueRendererGate(
        patched,
        draftFactoryPattern,
        (factory) => factory.replace("serviceTier:null", "serviceTier:{value:`default`}"),
        "new thread draft speed default",
      );
    }
    patched = patchComposerQuotaGates(patched, source);
    if (
      source.includes("codex-message-from-view")
      && source.includes("sendMessageFromView")
      && source.includes("Failed to send message from view")
      && !source.includes("__codeyModelWhitelistPatch?.guardDirectBridgeMessage")
    ) {
      // The native renderer forwards the request through Electron before it
      // emits codex-message-from-view. Event-only injections therefore see an
      // already-sent payload. Guard at the bridge boundary and rewrite only
      // after preparation. The guard suppresses early observational tracking
      // while pending; transport failures retain the renderer's original catch.
      const guardedBridgeSend = (bridgeName, sourceName) =>
        `let __codeySend=()=>{let __codeyMessage=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage?.(${sourceName})??${sourceName};if(globalThis.__codeyModelWhitelistPatch?.isBlockedOutgoingMessage?.(__codeyMessage)){globalThis.__codeyModelWhitelistPatch?.notifyBlockedOutgoingMessage?.(__codeyMessage);return Promise.resolve()}return ${bridgeName}.sendMessageFromView(__codeyMessage)};let __codeyGuard=globalThis.__codeyModelWhitelistPatch?.guardDirectBridgeMessage?.(${sourceName},__codeySend);return __codeyGuard==null||__codeyGuard===true?__codeySend():__codeyGuard`;
      patched = replaceUniqueRendererGate(
        patched,
        /\b([$A-Z_a-z][$\w]*)\?\.sendMessageFromView&&\(\1\.sendMessageFromView\(([$A-Z_a-z][$\w]*)\)\.catch\(/g,
        (_match, bridgeName, sourceName) =>
          `${bridgeName}?.sendMessageFromView&&((()=>{${guardedBridgeSend(bridgeName, sourceName)}})().catch(`,
        "model route bridge preflight",
      );
    }
    if (
      source.includes("AppServerRequestClient is missing a message dispatcher")
      && source.includes("mcp_request_enqueued")
      && !source.includes("globalThis.__codeyModelRequestSourceGateInstalled=true")
      && /this\.dispatchMessage\?\.\([`"]mcp-request[`"]/.test(source)
    ) {
      // Current Codex can create threads through AppServerRequestClient without
      // touching the renderer bridge helper above. Rewrite at enqueue time so
      // thread/start and prewarm requests bind the selected Codey route before
      // they reach the app server. Keep the live client so MCP reload can send
      // config/mcpServer/reload without walking the React tree.
      patched = replaceUniqueRendererGate(
        patched,
        /(enqueueRequest\(([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*=>\{this\.dispatchMessage\?\.\([`"]mcp-request[`"],\{request:[$A-Z_a-z][$\w]*,hostId:this\.hostId,[\s\S]{0,1500}?widget:\4\?\.widget\}\)\},[$A-Z_a-z][$\w]*=null,([$A-Z_a-z][$\w]*)\)\{if\(\6\?\.aborted\)return Promise\.reject\([^;]{0,200}\);)let /g,
        (_match, prefix, methodName, paramsName) =>
          `${prefix}let __codeyGuard=globalThis.__codeyModelWhitelistPatch?.guardOutgoingRequest?.(this,${methodName},${paramsName},()=>this.enqueueRequest(...arguments));if(__codeyGuard)return __codeyGuard;(globalThis.__codeyAppServerRequestClients??(globalThis.__codeyAppServerRequestClients=new Map)).set(this.hostId,this);let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage?.({type:\`mcp-request\`,hostId:this.hostId,request:{method:${methodName},params:${paramsName}}});if(__codeyRoute?.request){if(globalThis.__codeyModelWhitelistPatch?.isBlockedOutgoingMessage?.(__codeyRoute)){globalThis.__codeyModelWhitelistPatch?.notifyBlockedOutgoingMessage?.(__codeyRoute);return Promise.reject(Error(\`Codey blocked cross-provider model request\`))}${methodName}=__codeyRoute.request.method??${methodName},${paramsName}=__codeyRoute.request.params??${paramsName}}let `,
        "app server request route preflight",
      );
      // AppServerRequestClient runs the preflight before createRequest assigns
      // an id. Register the concrete request afterwards so the reply can
      // confirm the thread's actual runtime provider and selected route.
      patched = replaceUniqueRendererGate(
        patched,
        /(let\{request:([$A-Z_a-z][$\w]*),promise:[$A-Z_a-z][$\w]*\}=this\.createRequest\([^;]{1,256}\);)/g,
        (_match, createRequest, requestName) =>
          `${createRequest}globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage?.({type:\`mcp-request\`,hostId:this.hostId,request:${requestName}});`,
        "app server request identity tracking",
      );
      // Promise consumers update React Query before the diagnostic response
      // event runs. Rewrite model/list at the resolver boundary so the native
      // result can never replace Codey's current route catalog.
      patched = replaceUniqueRendererGate(
        patched,
        /(([$A-Z_a-z][$\w]*)\.resolve\()([$A-Z_a-z][$\w]*)(\),this\.emitRequestLifecycleEvent\(\{type:[`"]completed[`"],hostId:this\.hostId,method:\2\.method)/g,
        (_match, prefix, requestName, resultName, suffix) =>
          `${prefix}${resultName}=globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult?.(${requestName}.method,${resultName},this.hostId)??${resultName}${suffix}`,
        "app server model result rewrite",
      );
    }
    if (
      source.includes("Desktop host did not provide its AppServerManager service")
      && !source.includes("__codeyAppServerManagerHosted")
    ) {
      // AppServerManager 运行在桌面宿主进程，页面只拿到 RPC 存根，在宿主服务
      // 实际接入时留下标记供诊断。
      patched = replaceUniqueRendererGate(
        patched,
        /(if\(null==[$A-Z_a-z][$\w]*\)throw Error\("Desktop host did not provide its AppServerManager service"\);)/g,
        (_match, check) => `${check}globalThis.__codeyAppServerManagerHosted=true;`,
        "hosted app server manager marker",
      );
    }
    // 宿主托管时输入框发送经 RPC 存根直达宿主，页面请求客户端上的跨线路交接
    // 不再经过。把两个发送入口改为先交给模型请求校验，缺少校验时原样调用。
    // 以字面量开头的预检让大包快速跳过；只有命中的小模块才运行带接收者的匹配。
    if (!source.includes("__codeyModelWhitelistPatch?.hostedTurnCall")
      && source.includes(".sendFollowUpMessage(")
      && hostedFollowUpSendAnchor.test(source)) {
      patched = replaceUniqueRendererGate(
        patched,
        hostedFollowUpSend,
        (_match, rpc) => `${hostedTurnCall}(${rpc},"sendFollowUpMessage",`,
        "hosted follow-up portable handoff",
      );
    }
    if (!source.includes("__codeyModelWhitelistPatch?.hostedTurnCall")
      && source.includes(".startTurn(")
      && hostedTurnStartAnchor.test(source)) {
      patched = replaceUniqueRendererGate(
        patched,
        hostedTurnStart,
        (_match, rpc) => `${hostedTurnCall}(${rpc},"startTurn",`,
        "hosted turn start portable handoff",
      );
    }
    if (
      !source.includes("/*codey-third-party-ultra*/") &&
      source.includes("includeUltraReasoningEffort:") &&
      source.includes("isCustomModelProvider:") &&
      source.includes("teamCatalog:") &&
      source.includes("1186680773")
    ) {
      // 第三方线路按模型目录显示 Ultra，保留调用方和用户的推理等级设置。
      // 宿主服务 chunk 不含 teamCatalog 模型目录查询，不在此改写。
      patched = replaceUniqueRendererGate(
        patched,
        /(\(\{[^{}]*\bincludeUltraReasoningEffort\s*:\s*(?<ultra>[$A-Z_a-z][$\w]*)(?:=!0)?[^{}]*\}\s*,\s*\{[^{}]*\bget\s*:\s*(?<get>[$A-Z_a-z][$\w]*)[^{}]*\}\)\s*=>\s*\{[\s\S]{0,1600}?\b(?<flag>[$A-Z_a-z][$\w]*)\s*=\s*\k<ultra>\s*&&\s*)(?<gate>\k<get>\(\s*[$A-Z_a-z][$\w]*(?:\.[$A-Z_a-z][$\w]*)?\s*,\s*(?<quote>["'`])1186680773\k<quote>\s*\))(?=[\s\S]{0,2000}?\bincludeUltraReasoningEffort\s*:\s*\k<flag>\s*,\s*isCustomModelProvider\s*:\s*(?<custom>[$A-Z_a-z][$\w]*)\b)/g,
        (_match, prefix, ...rest) => {
          const { custom, gate } = rest.at(-1);
          return `${prefix}(${custom}||${gate})/*codey-third-party-ultra*/`;
        },
        "third-party Ultra reasoning",
      );
    }
    if (
      source.includes("supportsIndependentSpeedModes") &&
      source.includes("ultrafast_mode") &&
      !source.includes("/*codey-service-tier-access*/")
    ) {
      // Fast/Ultrafast permission comes only from ChatGPT account requirements.
      // Codey routes declare speed tiers in the model catalog, so the shared
      // check used by the picker, slash commands, settings and request resolver
      // allows every tier; the model's serviceTiers still decide what exists.
      patched = replaceUniqueRendererGate(
        patched,
        /if\(null==([$A-Z_a-z][$\w]*)\)return!1;let ([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*\(([$A-Z_a-z][$\w]*)\);return"flex"===\3\|\|\1\[\2\?\?"fast"\]/g,
        "/*codey-service-tier-access*/return!0",
        "service tier access",
      );
    }
    if (source.includes("={serviceTierAccess:")
      && !source.includes("{fast:!0,ultrafast:!0}")
      && /=\{serviceTierAccess:[$A-Z_a-z][$\w]*,isLoading:/.test(source)) {
      // API key logins and pending requirements yield a null access object,
      // which hides the speed control and settings entirely. Keep a fixed
      // grant so visibility follows the model's own service tiers.
      patched = replaceUniqueRendererGate(
        patched,
        /(\b([$A-Z_a-z][$\w]*)=)[$A-Z_a-z][$\w]*&&![$A-Z_a-z][$\w]*&&null!=[$A-Z_a-z][$\w]*\?\(0,[$A-Z_a-z][$\w]*\.[$A-Z_a-z][$\w]*\)\([$A-Z_a-z][$\w]*\):null(?=[^;]{0,200};let [$A-Z_a-z][$\w]*=\2(?:,[$A-Z_a-z][$\w]*)?;return[^;]{0,200}?\{serviceTierAccess:)/g,
        (_match, assignment) => `${assignment}{fast:!0,ultrafast:!0}`,
        "service tier access state",
      );
    }
    patched = patchPerformanceAndPrewarmGates(patched, source);
    if (!patched.includes("globalThis.__codeyModelRequestSourceGateInstalled=true")
      && patched.includes("globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage")
      && patched.includes("let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage")
      && patched.includes("globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult")) {
      patched += "\n;globalThis.__codeyModelRequestSourceGateInstalled=true;\n";
    }
    return patched;
    } finally {
      activeRendererPatchFailures = previousRendererPatchFailures;
      activeRendererPatchKey = previousRendererPatchKey;
    }
  };
  // Codex 样式表中的 :has([data-state=open]) 等规则会让 data-state 进入 Blink
  // RuleFeatureSet 的 :has() 失效集，悬停侧栏行切换提示气泡 data-state 时触发
  // 整页样式重算。改写为普通属性选择器 [data-codey-open-within]，并把命中规则的
  // 容器类名直接写进 :root 自定义属性，免去页面启动时扫描数千条 CSSRule。
  const openStateCssPatchedMarker = "--codey-css-open-state-has-patched";
  const openWithinClassesProperty = "--codey-open-within-classes";
  const openStateCssDataStateHasPattern =
    /:has\(\s*(?::is\(\s*)?(?:button)?\[\s*data-state\s*=\s*(?:"open"|'open'|open)\s*\]\s*\)?\s*\)/g;
  const openWithinDirectSelectorClassPattern =
    /\.((?:\\[0-9a-fA-F]{1,6}\s?|\\.|[A-Za-z0-9_-])+)\s*>\s*\*\s*\)/g;
  const openWithinAncestorSelectorClassPattern =
    /\.((?:\\[0-9a-fA-F]{1,6}\s?|\\.|[A-Za-z0-9_-])+)(?:\)|\s)*?(?=:is\([^)]*\[data-codey-open-within\]|\[data-codey-open-within\])/g;
  const unescapeCssClassToken = (rawToken) =>
    String(rawToken || "")
      .replace(/\\([0-9a-fA-F]{1,6})\s?/g, (_, hex) => {
        const codePoint = Number.parseInt(hex, 16);
        return Number.isFinite(codePoint) ? String.fromCodePoint(codePoint) : "";
      })
      .replace(/\\(.)/g, "$1");
  const patchCodexRendererCssAsset = (source) => {
    if (
      source.includes(openStateCssPatchedMarker) ||
      !source.includes("data-state") ||
      !source.includes(":has(")
    ) {
      return source;
    }
    const directClasses = new Set();
    const ancestorClasses = new Set();
    const buttonAncestorClasses = new Set();
    let replaced = false;
    const patched = source.replace(
      openStateCssDataStateHasPattern,
      (_match, offset) => {
        replaced = true;
        let start = offset;
        while (start > 0) {
          const ch = source.charCodeAt(start - 1);
          if (ch === 125 || ch === 123 || ch === 59) break;
          start -= 1;
        }
        const snippet = `${source.slice(start, offset)}[data-codey-open-within]`;
        for (const match of snippet.matchAll(openWithinDirectSelectorClassPattern)) {
          const token = unescapeCssClassToken(match[1]);
          if (token) {
            directClasses.add(token);
            ancestorClasses.add(token);
          }
        }
        for (const match of snippet.matchAll(openWithinAncestorSelectorClassPattern)) {
          const token = unescapeCssClassToken(match[1]);
          if (!token) continue;
          if (token.includes("button[data-state=open]")) {
            buttonAncestorClasses.add(token);
          } else {
            ancestorClasses.add(token);
          }
        }
        return "[data-codey-open-within]";
      },
    );
    if (!replaced) return source;
    const meta = encodeURIComponent(JSON.stringify({
      d: [...directClasses],
      a: [...ancestorClasses],
      b: [...buttonAncestorClasses],
    }));
    return `${patched}\n:root{${openStateCssPatchedMarker}:1;${openWithinClassesProperty}:"${meta}"}\n`;
  };
  const isCodexRendererCssAssetUrl = (requestUrl) => {
    try {
      const url = new URL(requestUrl);
      if (url.protocol !== "app:") return false;
      const match = /^\/assets\/(?:async\/)?([a-z0-9_~.-]+)\.css$/i.exec(
        url.pathname,
      );
      if (!match) return false;
      const stem = match[1];
      if (!stem || stem.startsWith(".")) return false;
      return stem
        .split(/[-~.]/)
        .some((segment) => /^[a-f0-9]{8,32}$/i.test(segment));
    } catch {
      return false;
    }
  };
  const isCodexRendererAssetUrl = (requestUrl) => {
    try {
      const url = new URL(requestUrl);
      return (
        url.protocol === "app:" &&
        (
          /^\/assets\/(?:async\/)?\d{1,10}\.[a-f0-9]{8,32}\.js$/i.test(
            url.pathname,
          ) ||
          isCodexRendererCssAssetUrl(requestUrl)
        )
      );
    } catch {
      return false;
    }
  };
  // CDP Fetch 拦截入口：输入响应体文本，返回改写后的文本；null 表示放行原响应。
  globalThis.__codeyRendererPatchEntry = (requestUrl, source) => {
    try {
      if (typeof requestUrl !== "string" || typeof source !== "string") {
        return null;
      }
      if (!isCodexRendererAssetUrl(requestUrl)) return null;
      let patched;
      try {
        patched = isCodexRendererCssAssetUrl(requestUrl)
          ? patchCodexRendererCssAsset(source)
          : patchCodexRendererAsset(source, requestUrl);
      } catch (error) {
        // Codex renderer bundles are minified implementation details and their
        // shapes change between releases. These UI restorations are optional:
        // never turn a stale patch anchor into a failed app:// module request,
        // otherwise Codex remains on its static startup loader forever.
        recordCodeyPatchFailure("patch_codex_renderer_asset", error, {
          requestUrl,
        });
        try {
          console.error("Codey skipped an incompatible Codex renderer patch", error);
        } catch {}
        return null;
      }
      return patched === source ? null : patched;
    } catch (error) {
      try {
        console.error("[Codey] renderer asset patch entry failed", error);
      } catch {}
      return null;
    }
  };
  globalThis.__codeyRendererPatchInstalled = true;
})();
