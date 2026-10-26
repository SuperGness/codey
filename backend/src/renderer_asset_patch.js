// Codex 渲染进程资产源补丁。原本是主进程 protocol.handle 里对 app:// 响应
// 做改写的纯函数部分；Electron fuse 关闭后主进程注入不可用，改由 Codey 的补丁
// 服务在 Fetch 响应暂停时逐资产调用 globalThis.__codeyRendererPatchEntry 完成
// 同样的改写，结果按资产文件名缓存在 Codey 侧。
// 本文件只依赖页面 JS 环境，不使用任何 Node/Electron API。
(() => {
  const rendererPatchOptions =
    typeof globalThis.__CODEY_RENDERER_PATCH_OPTIONS__ === "object" &&
    globalThis.__CODEY_RENDERER_PATCH_OPTIONS__ !== null
      ? globalThis.__CODEY_RENDERER_PATCH_OPTIONS__
      : {};
  const disablePet = rendererPatchOptions.disablePet === true;
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
  const maxRendererPatchFingerprints = 64;
  const rendererPatchFailuresByFingerprint = new Map();
  let activeRendererPatchFailures = null;
  // 指纹只作补丁结果缓存键，不需要加密强度；原主进程版用 sha256，
  // 页面里用同步的双路 32 位 FNV-1a 即可；缓存命中时整段耗时都在这里，
  // Math.imul 比逐字符 BigInt 运算约快一倍。
  const rendererPatchFingerprint = (source) => {
    try {
      let first = 0x811c9dc5;
      let second = 0x050c5d1f;
      for (let index = 0; index < source.length; index += 1) {
        const code = source.charCodeAt(index);
        first = Math.imul(first ^ code, 0x01000193);
        second = Math.imul(second ^ code, 0x5bd1e995);
      }
      return `${source.length}:${(first >>> 0).toString(36)}:${(second >>> 0).toString(36)}`;
    } catch {
      // Fingerprinting is only an optimization. If it is unavailable, keep
      // the existing compatibility behavior instead of risking a false cache hit.
      return null;
    }
  };
  // Patching is a pure function of the asset text: reloads, renderer recovery
  // and the locale-forced first-launch reload re-request the same app-initial
  // chunk, so remember the patched output instead of rerunning every gate.
  const maxRendererPatchedOutputs = 4;
  const rendererPatchedOutputByFingerprint = new Map();
  const rememberRendererPatchedOutput = (fingerprint, patched) => {
    if (fingerprint == null) return;
    rendererPatchedOutputByFingerprint.delete(fingerprint);
    rendererPatchedOutputByFingerprint.set(fingerprint, patched);
    while (rendererPatchedOutputByFingerprint.size > maxRendererPatchedOutputs) {
      const oldest = rendererPatchedOutputByFingerprint.keys().next().value;
      rendererPatchedOutputByFingerprint.delete(oldest);
    }
  };
  const rendererPatchFailuresForFingerprint = (fingerprint) => {
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
  // 只截取传入的静态资产源码，不读取输入框、会话状态或凭据。
  const rendererGateDiagnosticAnchors = [
    "serviceTierAccess:",
    "supportsIndependentSpeedModes",
    "includeUltraReasoningEffort",
    "isCustomModelProvider",
    "getHasInProgressLocalConversation",
    "electron-app-state-snapshot-request",
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
  // Older Codex builds imported a single codex-avatar chunk from the settings
  // chunk; current builds split the preview across the mascot button, the pet
  // asset table, and the avatar option hook. One settings chunk can import
  // several of them, so every match is stubbed rather than requiring a unique
  // one.
  const petRendererResourceImport =
    /import(?:\s*([^;"']+?)\s*from)?\s*["']\.\/(?:codex-avatar|codex-pet-assets|avatar-mascot-button|use-avatar-options)(?:[~-][^/"']*)?\.js["']/;
  const petRendererResourceImportAll = new RegExp(
    petRendererResourceImport.source,
    "g",
  );
  const replacePetRendererResourceImports = (source, name) => {
    if (activeRendererPatchFailures?.has(name)) return source;
    let count = 0;
    const patched = source.replace(petRendererResourceImportAll, (...args) => {
      count += 1;
      return replacePetRendererImportWithStubs(...args);
    });
    if (count === 0) return recordIncompatibleRendererGate(source, name, count);
    return patched;
  };
  const draftFactoryPattern = /\b[$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*\([$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*=>\(\{isManuallyChanged:!1,modelSettings:null,serviceTier:null\}\)\)/g;
  const draftResolverPattern = /function ([$A-Z_a-z][$\w]*)\(\{userSavedModelString:[$A-Z_a-z][$\w]*,userSavedReasoningEffort:[$A-Z_a-z][$\w]*,listModelsData:[$A-Z_a-z][$\w]*\}\)/g;
  // Codex 26.1021 起，草稿原生保存手动选择并以 draftModelSettings 传入设置
  // hook，只需把未手动选择的新草稿的推理强度初值换成 medium。返回改写后的
  // 源码；无法唯一识别时返回匹配次数。
  const patchNativeDraftReasoningEffort = (source, resolver) => {
    const hooks = [...source.matchAll(
      /function [$A-Z_a-z][$\w]*\(([$A-Z_a-z][$\w]*)\)\{let [$A-Z_a-z][$\w]*=\(0,[$A-Z_a-z][$\w]*\.c\)\(\d+\),\{((?=[^{}]*\bmanagedNewThreadModelSettings:)(?=[^{}]*\bdraftModelSettings:)(?=[^{}]*\bisNewThreadDraft:)[^{}]*)\}=\1,/g,
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
    // 管理员下发的新会话设置和草稿内的手动选择保持原样；模型不在目录中时
    // 沿用原生结果，模型不支持 medium 时由原生解析器回退。
    const replacement = `/*codey-new-thread-draft-defaults*/((__codeyArgs,__codeyResolved=${resolver}(__codeyArgs))=>${isDraft}&&${draft}==null&&${managed}==null&&__codeyArgs.listModelsData?.models?.some(e=>e.model===__codeyResolved.model)?${resolver}({...__codeyArgs,userSavedModelString:__codeyResolved.model,userSavedReasoningEffort:\`medium\`}):__codeyResolved)(${args})`;
    return source.slice(0, callStart) + replacement + source.slice(argsEnd + 1);
  };
  // 旧版 Codex：草稿不保存手动选择，需在草稿 hook 内补目录校验并让手动
  // 选择写入草稿。返回改写后的源码；无法唯一识别时返回匹配次数。
  const patchLegacyDraftReasoningEffort = (source, resolver) => {
    const patterns = {
      catalog: /managedNewThreadModelSettings:[$A-Z_a-z][$\w]*,waitForModelList:[\s\S]{0,4000}?let\{data:([$A-Z_a-z][$\w]*),isLoading:[$A-Z_a-z][$\w]*\}=([$A-Z_a-z][$\w]*)\([$A-Z_a-z][$\w]*\),[$A-Z_a-z][$\w]*=[$A-Z_a-z][$\w]*\([$A-Z_a-z][$\w]*\?\.settings,\1\),[\s\S]{0,1500}?userSavedModelString:/g,
      model: /function [$A-Z_a-z][$\w]*\([$A-Z_a-z][$\w]*,[$A-Z_a-z][$\w]*\)\{let ([$A-Z_a-z][$\w]*)=\(0,([$A-Z_a-z][$\w]*)\.c\)\((\d+)\)[\s\S]{0,700}?let\{draftSettings:([$A-Z_a-z][$\w]*),isNewThreadDraft:[$A-Z_a-z][$\w]*,updateDraftSettings:([$A-Z_a-z][$\w]*)\}=[$A-Z_a-z][$\w]*\([$A-Z_a-z][$\w]*\),([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*\([^)]*\),\{isLunaReserveActive:[$A-Z_a-z][$\w]*\}=\6,([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*==null&&[$A-Z_a-z][$\w]*,[^;]{0,300}?,([$A-Z_a-z][$\w]*)=\7&&\4\.modelSettings!=null,([$A-Z_a-z][$\w]*)=\6\.modelSettings;(?=if\(\8\))/g,
    };
    const matches = {};
    for (const [key, pattern] of Object.entries(patterns)) {
      const found = [...source.matchAll(pattern)];
      if (found.length !== 1) return found.length;
      matches[key] = found[0];
    }
    const [, cache, , cacheSize, , update, settings, isDraft, hasOverride, model] = matches.model;
    const escapedUpdate = update.replace(/\$/g, "\\$");
    const manualPattern = new RegExp(
      `([$_A-Za-z][$\\w]*\\|\\|[$_A-Za-z][$\\w]*\\|\\|[$_A-Za-z][$\\w]*&&[$_A-Za-z][$\\w]*===\x60ultra\x60\\?)${escapedUpdate}\\([$A-Z_a-z][$\\w]*=>\\(\\{\\.\\.\\.[$A-Z_a-z][$\\w]*,isManuallyChanged:!0,modelSettings:`,
      "g",
    );
    // 只在已识别的草稿 hook 内查找回调，不能误改另一个模型选择器。
    const modelEnd = source.indexOf("}function ", matches.model.index + matches.model[0].length);
    if (modelEnd < 0) return 0;
    const modelScope = source.slice(matches.model.index, modelEnd + 1);
    const manualMatches = [...modelScope.matchAll(manualPattern)];
    if (manualMatches.length !== 1) return manualMatches.length;
    const slots = Number(cacheSize);
    const catalogHook = matches.catalog[2];
    // 新增目录 hook 始终调用，保留 React hook 顺序。未加载或模型不在目录中
    // 时保留现有设置；目录就绪后由原生解析器选择 medium 或模型支持的回退。
    const defaults = `/*codey-new-thread-draft-defaults*/let __codeyDraftCatalog=${catalogHook}({hostId:${settings}.hostId,enabled:${isDraft}}).data;if(${isDraft}&&!${hasOverride}&&__codeyDraftCatalog?.models?.some(e=>e.model===${model}.model)){let __codeyDraftEffort=${resolver}({userSavedModelString:${model}.model,userSavedReasoningEffort:\`medium\`,listModelsData:__codeyDraftCatalog}).reasoningEffort;if(${model}.reasoningEffort!==__codeyDraftEffort){let __codeyDraftSettings;${cache}[${slots}]!==${model}||${cache}[${slots + 1}]!==__codeyDraftEffort?(__codeyDraftSettings={...${model},reasoningEffort:__codeyDraftEffort},${cache}[${slots}]=${model},${cache}[${slots + 1}]=__codeyDraftEffort,${cache}[${slots + 2}]=__codeyDraftSettings):__codeyDraftSettings=${cache}[${slots + 2}],${model}=__codeyDraftSettings}}`;
    let patchedScope = modelScope.replace(matches.model[0],
      matches.model[0].replace(`.c)(${cacheSize})`, `.c)(${slots + 3})`) + defaults);
    patchedScope = patchedScope.replace(manualPattern, (match) => `${isDraft}||${match}`);
    return source.slice(0, matches.model.index) + patchedScope + source.slice(modelEnd + 1);
  };
  const patchNewThreadDraftDefaults = (source) => {
    const name = "new thread draft defaults";
    if (source.includes("/*codey-new-thread-draft-defaults*/")
      || activeRendererPatchFailures?.has(name)) return source;
    // 草稿速度初值与推理强度初值必须同时识别；缓存中的默认模型继续有效，
    // 旧推理强度不再成为下一个草稿的初值。
    const factories = [...source.matchAll(draftFactoryPattern)];
    if (factories.length !== 1) return recordIncompatibleRendererGate(source, name, factories.length);
    const resolvers = [...source.matchAll(draftResolverPattern)];
    if (resolvers.length !== 1) return recordIncompatibleRendererGate(source, name, resolvers.length);
    const patched = source.includes("draftModelSettings:")
      ? patchNativeDraftReasoningEffort(source, resolvers[0][1])
      : patchLegacyDraftReasoningEffort(source, resolvers[0][1]);
    if (typeof patched !== "string") return recordIncompatibleRendererGate(source, name, patched);
    const factory = factories[0][0];
    return patched.replace(factory, () => factory.replace("serviceTier:null", "serviceTier:{value:`default`}"));
  };
  const patchCodexRendererAsset = (source) => {
    let patched = source;
    if (source.includes("isNewThreadDraft:")
      && source.includes("modelSettingsPersistAsDefault:")
      && source.includes("userSavedReasoningEffort:")) {
      patched = patchNewThreadDraftDefaults(patched);
    }
    if (!source.includes("/*codey-app-server-manager-registry*/")
      && source.includes("inactiveThreadUnsubscriber.clearConversationStreamOwnership")
      && source.includes("getStreamRole(")
      && source.includes("updateConversationState(")) {
      // 在原生管理器被读取时登记真实实例，避免依赖模块导出名和 React 结构。
      // 登记失败不得影响原生读取，也不允许已销毁实例覆盖新管理器。
      patched = replaceUniqueRendererGate(
        patched,
        /getConversation\(([$A-Z_a-z][$\w]*)\)\{return this\.threadStore\.getConversation\(\1\)\}/g,
        (_match, id) => `getConversation(${id}){/*codey-app-server-manager-registry*/try{if(this.disposed!==true&&this.getHostId()===\`local\`){let __codeyManagers=globalThis.__codeyAppServerManagers;if(__codeyManagers==null)globalThis.__codeyAppServerManagers=__codeyManagers=new Map;if(__codeyManagers instanceof Map)__codeyManagers.set(\`local\`,this)}}catch{}return this.threadStore.getConversation(${id})}`,
        "app server manager registry",
      );
    }
    if (source.includes("codexComposerAdapter.CodexComposerAdapter.interruptThreadError")
      && !source.includes("/*codey-local-quota-gate*/")) {
      // 只改变 Codex 本机发送按钮的额度条件。保留共享额度状态、官方警告
      // 和其他禁用条件；每次渲染读取当前模型，避免模型切换后复用旧判断。
      const localSendGate = (marker, host, mode) =>
        `/*${marker}*/(()=>{try{return globalThis.__codeyQuotaUnlock?.allowsLocalSend?.({hostId:${host},model:${mode}.settings.model})!==true}catch{return true}})()`;
      let quotaGate = null;
      patched = replaceUniqueRendererGate(
        patched,
        /let\{agentMode:[^;]{0,1024}?(?<mode>[$A-Z_a-z][$\w]*)\.settings\.model===`gpt-reserve`[^;]{0,1024}?,(?<quota>[$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*\((?<host>[$A-Z_a-z][$\w]*)===`local`&&[$A-Z_a-z][$\w]*\)(?=,[\s\S]{0,10000}?activeCollaborationMode:[$A-Z_a-z][$\w]*,[\s\S]{0,2000}?resolvedHostId:\k<host>,[\s\S]{0,10000}?let (?<disabled>[$A-Z_a-z][$\w]*)=[^;]{0,512}?\|\|\k<quota>,[\s\S]{0,6000}?submitDisabled:\k<disabled>,)/g,
        (...args) => {
          quotaGate = args.at(-1);
          return `${args[0]}&&${localSendGate("codey-local-quota-gate", quotaGate.host, quotaGate.mode)}`;
        },
        "local third-party composer quota",
      );
      // Codex 26.1021 起 Team 等工作空间额度耗尽时另有按主机的硬阻断，
      // 在禁用表达式中紧挨额度条件；旧版没有该条件时保持不变。
      if (quotaGate != null && patched.includes("/*codey-local-quota-gate*/")) {
        const { disabled, host, mode, quota } = quotaGate;
        const name = value => value.replace(/\$/g, "\\$");
        const ident = "[$A-Z_a-z][$\\w]*";
        const disabledMatches = [...patched.matchAll(new RegExp(`let ${name(disabled)}=[^;]{0,512}?\\|\\|(${ident})\\|\\|${name(quota)},`, "g"))];
        const hard = disabledMatches.length === 1 ? disabledMatches[0][1] : null;
        if (hard != null
          && hard !== host
          && patched.match(new RegExp(`[,;{]${name(hard)}=${ident}\\(${ident},${name(host)}\\)[,;]`, "g"))?.length === 1) {
          const [{ 0: matched, index }] = disabledMatches;
          const term = `||${hard}||${quota},`;
          const offset = index + matched.length - term.length;
          patched = `${patched.slice(0, offset)}||${hard}&&${localSendGate("codey-local-hard-block-gate", host, mode)}||${quota},${patched.slice(offset + term.length)}`;
        }
      }
    }
    if (
      source.includes("codex-message-from-view")
      && source.includes("sendMessageFromView")
      && source.includes("Failed to send message from view")
    ) {
      // The native renderer forwards the request through Electron before it
      // emits codex-message-from-view. Event-only injections therefore see an
      // already-sent payload. Guard at the bridge boundary and rewrite only
      // after preparation. The guard suppresses early observational tracking
      // while pending; transport failures retain the renderer's original catch.
      patched = replaceUniqueRendererGate(
        patched,
        /if\(([$A-Z_a-z][$\w]*)\?\.sendMessageFromView\)\{let ([$A-Z_a-z][$\w]*)=([$A-Z_a-z][$\w]*);\1\.sendMessageFromView\(\2\)\.catch\(([$A-Z_a-z][$\w]*)=>\{/g,
        (_match, bridgeName, messageName, sourceName, errorName) =>
          `if(${bridgeName}?.sendMessageFromView){let ${messageName}=${sourceName};let __codeySend=()=>{${messageName}=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage?.(${sourceName})??${sourceName};if(globalThis.__codeyModelWhitelistPatch?.isBlockedOutgoingMessage?.(${messageName})){globalThis.__codeyModelWhitelistPatch?.notifyBlockedOutgoingMessage?.(${messageName});return Promise.resolve()}return ${bridgeName}.sendMessageFromView(${messageName})};let __codeyGuard=globalThis.__codeyModelWhitelistPatch?.guardDirectBridgeMessage?.(${sourceName},__codeySend);(__codeyGuard==null||__codeyGuard===true?__codeySend():__codeyGuard).catch(${errorName}=>{`,
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
      // they reach the app server. Keep the live client so MCP reload can send
      // config/mcpServer/reload without walking the React tree.
      patched = replaceUniqueRendererGate(
        patched,
        /(enqueueRequest\(([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*),([$A-Z_a-z][$\w]*)=[$A-Z_a-z][$\w]*=>\{this\.dispatchMessage\?\.\(`mcp-request`,\{request:[$A-Z_a-z][$\w]*,hostId:this\.hostId,[\s\S]{0,700}?widget:\4\?\.widget\}\)\},[$A-Z_a-z][$\w]*=null\)\{)let /g,
        (_match, prefix, methodName, paramsName) =>
          `${prefix}let __codeyGuard=globalThis.__codeyModelWhitelistPatch?.guardOutgoingRequest?.(this,${methodName},${paramsName},()=>this.enqueueRequest(...arguments));if(__codeyGuard)return __codeyGuard;(globalThis.__codeyAppServerRequestClients??(globalThis.__codeyAppServerRequestClients=new Map)).set(this.hostId,this);let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage?.({type:\`mcp-request\`,hostId:this.hostId,request:{method:${methodName},params:${paramsName}}});if(__codeyRoute?.request){if(globalThis.__codeyModelWhitelistPatch?.isBlockedOutgoingMessage?.(__codeyRoute)){globalThis.__codeyModelWhitelistPatch?.notifyBlockedOutgoingMessage?.(__codeyRoute);return Promise.reject(Error(\`Codey blocked cross-provider model request\`))}${methodName}=__codeyRoute.request.method??${methodName},${paramsName}=__codeyRoute.request.params??${paramsName}}let `,
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
          `${createRequest}globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage?.({type:\`mcp-request\`,hostId:this.hostId,request:${requestName}});`,
        "app server request identity tracking",
      );
      // Promise consumers update React Query before the diagnostic response
      // event runs. Rewrite model/list at the resolver boundary so the native
      // result can never replace Codey's current route catalog.
      patched = replaceUniqueRendererGate(
        patched,
        /(([$A-Z_a-z][$\w]*)\.resolve\()([$A-Z_a-z][$\w]*)(\),this\.emitRequestLifecycleEvent\(\{type:`completed`,hostId:this\.hostId,method:\2\.method)/g,
        (_match, prefix, requestName, resultName, suffix) =>
          `${prefix}${resultName}=globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult?.(${requestName}.method,${resultName},this.hostId)??${resultName}${suffix}`,
        "app server model result rewrite",
      );
    }
    if (
      disablePet
      && /settings\.(?:(?:appearance|personalization)\.)?(?:pets|mini)(?:[."`]|$)/.test(source)
      && petRendererResourceImport.test(source)
    ) {
      // Codex keeps the pets/Mini settings preview in a settings route chunk
      // and statically imports the avatar renderer, mascot button, pet asset
      // table, and avatar option hook from it. Hiding the controls after React
      // mounts is too late: those imports have already pulled every bundled
      // spritesheet into the main window. Replace each settings-side dependency
      // with inert callable/iterable bindings. The shared avatar overlay host
      // stays intact because current Codex builds also use it for voice
      // controls.
      patched = replacePetRendererResourceImports(
        patched,
        "pet settings avatar resources",
      );
    }
    if (
      !source.includes("codeyReconcileCompletedConversation")
      && source.includes("getHasInProgressLocalConversation(")
      && source.includes("inactiveThreadUnsubscriber.clearConversationStreamOwnership")
      && source.includes("getConversationStreamRevision(")
    ) {
      // A renderer can miss the terminal notification while retaining its old
      // in-progress turn and stream role. Confirm the native app-server state
      // twice, reject a concurrent stream revision, pending submission or
      // follower window, then mark the thread needs_resume and resume it the
      // same way reconnect recovery does. This hydrates history without
      // discarding, interrupting, or starting another turn. The in-progress
      // predicate is the one getHasInProgressLocalConversation already uses.
      patched = replaceUniqueRendererGate(
        patched,
        /getHasInProgressLocalConversation\(\{exceptConversationId:([$A-Z_a-z][$\w]*)\}=\{\}\)\{for\(let ([$A-Z_a-z][$\w]*) of this\.threadStore\.getRecentConversationIds\(\)\)\{if\(\2===\1\)continue;let ([$A-Z_a-z][$\w]*)=this\.conversations\.get\(\2\);if\(\3!=null&&([$A-Z_a-z][$\w]*)\(\3\)\)return!0\}return!1\}/g,
        (match, _exceptName, _idName, _conversationName, inProgressName) =>
          `async codeyReconcileCompletedConversation(__codeyParams){let __codeyId=__codeyParams?.conversationId,__codeyRunning=()=>{let __codeyConversation=__codeyId==null?null:this.getConversation(__codeyId);return __codeyConversation!=null&&__codeyConversation.unconfirmedTurnSubmissions?.some(e=>e.terminal!==!0)!==!0&&${inProgressName}(__codeyConversation)&&this.getStreamRole(__codeyId)?.role!==\`follower\`};if(!__codeyRunning())return!1;let __codeyRevision=this.getConversationStreamRevision(__codeyId),__codeyReadStatus=async()=>{let __codeyResponse=await this.sendRequest(\`thread/read\`,{threadId:__codeyId,includeTurns:!1}),__codeyStatus=__codeyResponse?.thread?.status;return typeof __codeyStatus===\`string\`?__codeyStatus:__codeyStatus?.type},__codeyStatus=await __codeyReadStatus();if(__codeyStatus!==\`idle\`&&__codeyStatus!==\`systemError\`&&__codeyStatus!==\`error\`)return!1;await new Promise(__codeyResolve=>setTimeout(__codeyResolve,250));if(await __codeyReadStatus()!==__codeyStatus||this.getConversationStreamRevision(__codeyId)!==__codeyRevision||!__codeyRunning())return!1;let __codeyCwd=this.getConversation(__codeyId)?.cwd;this.inactiveThreadUnsubscriber.clearConversationStreamOwnership(__codeyId);this.updateConversationState(__codeyId,__codeyState=>{__codeyState.resumeState=\`needs_resume\`},!1);await this.resumeConversation({...__codeyParams,workspaceRoots:__codeyParams.workspaceRoots?.length?__codeyParams.workspaceRoots:__codeyCwd?[__codeyCwd]:[\`/\`]});let __codeyConversation=this.getConversation(__codeyId);return __codeyConversation!=null&&!${inProgressName}(__codeyConversation)}${match}`,
        "completed thread reconciliation",
      );
    }
    if (
      source.includes("includeUltraReasoningEffort:") &&
      source.includes("isCustomModelProvider:") &&
      source.includes("modelCatalogPath:") &&
      source.includes("1186680773")
    ) {
      // 第三方线路按模型目录显示 Ultra，保留调用方和用户的推理等级设置。
      // 宿主服务 chunk 只把同一开关作为参数传入，没有模型目录查询，不在此改写。
      patched = replaceUniqueRendererGate(
        patched,
        /(\(\{[^{}]*\bincludeUltraReasoningEffort\s*:\s*([$A-Z_a-z][$\w]*)[^{}]*\bisCustomModelProvider\s*:\s*([$A-Z_a-z][$\w]*)[^{}]*\}\s*,\s*\{[^{}]*\bget\s*:\s*([$A-Z_a-z][$\w]*)[^{}]*\}\)\s*=>\s*\{[^{}]*\b[$A-Z_a-z][$\w]*\s*=\s*\2\s*&&\s*)(\4\(\s*[$A-Z_a-z][$\w]*\s*,\s*(["'`])1186680773\6\s*\))/g,
        (_match, prefix, _includeUltra, customProvider, _get, gate) =>
          `${prefix}(${customProvider}||${gate})`,
        "third-party Ultra reasoning",
      );
    }
    if (
      source.includes("supportsIndependentSpeedModes") &&
      source.includes("ultrafast_mode")
    ) {
      // Fast/Ultrafast permission comes only from ChatGPT account requirements.
      // Codey routes declare speed tiers in the model catalog, so the shared
      // check used by the picker, slash commands, settings and request resolver
      // allows every tier; the model's serviceTiers still decide what exists.
      patched = replaceUniqueRendererGate(
        patched,
        /if\s*\(\s*([$A-Z_a-z][$\w]*)\s*==\s*null\s*\)\s*return\s*!1\s*;\s*let\s+([$A-Z_a-z][$\w]*)\s*=\s*[$A-Z_a-z][$\w]*\(\s*([$A-Z_a-z][$\w]*)\s*\)\s*;\s*return\s+\3\s*===\s*`flex`\s*\|\|\s*\1\[\s*\2\s*\?\?\s*`fast`\s*\]/g,
        "return!0",
        "service tier access",
      );
    }
    if (/=\{serviceTierAccess:[$A-Z_a-z][$\w]*,isLoading:/.test(source)) {
      // API key logins and pending requirements yield a null access object,
      // which hides the speed control and settings entirely. Keep a fixed
      // grant so visibility follows the model's own service tiers.
      patched = replaceUniqueRendererGate(
        patched,
        /(\b([$A-Z_a-z][$\w]*)=)[$A-Z_a-z][$\w]*&&![$A-Z_a-z][$\w]*&&[$A-Z_a-z][$\w]*!=null\?[$A-Z_a-z][$\w]*\([$A-Z_a-z][$\w]*\):null(?=[^;]{0,200};let [$A-Z_a-z][$\w]*=\2,[$A-Z_a-z][$\w]*;return[^;]{0,200}?\{serviceTierAccess:)/g,
        (_match, assignment) => `${assignment}{fast:!0,ultrafast:!0}`,
        "service tier access state",
      );
    }
    if (source.includes("case`electron-app-state-snapshot-request`:")) {
      // Main asks the focused window for a full app-state snapshot every 30 s,
      // then adds process sampling, a debug log and a Sentry breadcrumb. Skip
      // only the heartbeat reason; other diagnostic triggers still reply, and
      // main expires unanswered requests on its own TTL.
      // Builds differ only in whether the switch break is labelled.
      patched = replaceUniqueRendererGate(
        patched,
        /(case`electron-app-state-snapshot-request`:\{)(?=[\s\S]{0,2000}?collectAppStateSnapshot\(([$A-Z_a-z][$\w]*)\.hostId,\2\.reason\)[\s\S]{0,2000}?dispatchMessage\(`electron-app-state-snapshot-response`[^;]{0,400};break((?: [$A-Z_a-z][$\w]*)?)\})/g,
        (_match, prefix, messageName, label) =>
          `${prefix}if(${messageName}.reason===\`heartbeat\`)break${label};`,
        "app state heartbeat snapshot",
      );
    }
    if (
      source.includes("activeInteractions=new Map") &&
      source.includes("beginCpuSampling") &&
      source.includes(
        "ensureHeartbeat(){this.heartbeatTimer??=setInterval",
      )
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
    if (patched.includes("globalThis.__codeyModelWhitelistPatch?.trackOutgoingMessage")
      && patched.includes("let __codeyRoute=globalThis.__codeyModelWhitelistPatch?.rewriteOutgoingMessage")
      && patched.includes("globalThis.__codeyModelWhitelistPatch?.rewriteIncomingResult")) {
      patched += "\n;globalThis.__codeyModelRequestSourceGateInstalled=true;\n";
    }
    return patched;
  };
  const isCodexRendererAssetUrl = (requestUrl) => {
    try {
      const url = new URL(requestUrl);
      return (
        url.protocol === "app:" &&
        url.pathname.includes("/assets/") &&
        (
          /\/(?:(?:app-initial|app-primary|app-shared|codex-composer-adapter|initialize-app-host-services)(?:[~-][^/]*)?)\.(?:c|m)?js$/i.test(
            url.pathname,
          ) ||
          (
            disablePet &&
            /\/(?:(?:pets-settings)(?:[~-][^/]*)?)\.(?:c|m)?js$/i.test(
              url.pathname,
            )
          )
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
      const fingerprint = rendererPatchFingerprint(source);
      if (
        fingerprint != null &&
        rendererPatchedOutputByFingerprint.has(fingerprint)
      ) {
        patched = rendererPatchedOutputByFingerprint.get(fingerprint);
        // Refresh insertion order so the bounded map behaves as an LRU.
        rememberRendererPatchedOutput(fingerprint, patched);
        return patched === source ? null : patched;
      }
      const previousRendererPatchFailures = activeRendererPatchFailures;
      activeRendererPatchFailures = rendererPatchFailuresForFingerprint(fingerprint);
      try {
        patched = patchCodexRendererAsset(source);
        rememberRendererPatchedOutput(fingerprint, patched);
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
      } finally {
        activeRendererPatchFailures = previousRendererPatchFailures;
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
