(() => {
  if (
    window.__codeyBridgeHelpersInstalled
    && window.__codeyMutationDispatcher?.createShieldLifecycle
    && window.__codeyMutationDispatcher?.controlDescriptor
    && window.__codeySharedRuntime?.appServerRequestClient
    && window.__codeySharedRuntime?.callBridge
    && window.__codeySharedRuntime?.codeyTimed
    && window.__codeySharedRuntime?.publishInjectionStatus
    && window.__codeySharedRuntime?.reactFiberFor
    && window.__codeySharedRuntime?.reactInternalGraphIncludes
    && window.__codeySharedRuntime?.reactInternalKeys
    && window.__codeySharedRuntime?.registerFetchInterceptor
    && window.__codeySharedRuntime?.showRuntimeToast
    && window.__codeySharedRuntime?.statsigClients
    && window.__codeySharedRuntime?.walkElementsBudgeted
  ) return;
  window.__codeyBridgeHelpersInstalled = true;

  const statsigClients = () => {
    const clients = [];
    const roots = [window.__STATSIG__, globalThis.__STATSIG__]
      .filter((root, index, values) => root && values.indexOf(root) === index);
    for (const root of roots) {
      if (typeof root !== "object") continue;
      try {
        clients.push(root.firstInstance);
      } catch {
      }
      try {
        if (typeof root.instance === "function") clients.push(root.instance());
      } catch {
      }
      try {
        if (root.instances && typeof root.instances === "object") {
          clients.push(...Object.values(root.instances));
        }
      } catch {
      }
    }
    return clients.filter(
      (client, index, values) =>
        client && typeof client === "object" && values.indexOf(client) === index,
    );
  };

  const reactInternalKeyPattern = /^__(?:reactProps|reactFiber)\$.*/;
  const reactInspectableKeyPattern = /^__(?:reactProps|reactFiber)/;
  const reactContainerKeyPattern = /^__reactContainer/;
  const reactInternalKeys = (element, options = {}) => {
    if (!element || (typeof element !== "object" && typeof element !== "function")) return [];
    try {
      return Object.keys(element)
        .filter((key) => (
          (options.includeContainer === true
            ? reactInspectableKeyPattern.test(key)
            : reactInternalKeyPattern.test(key))
          || (options.includeContainer === true && reactContainerKeyPattern.test(key))
        ));
    } catch {
      return [];
    }
  };
  const reactFiberFor = (element) => {
    if (!element || (typeof element !== "object" && typeof element !== "function")) return null;
    try {
      const keys = Object.keys(element);
      for (let index = 0; index < keys.length; index += 1) {
        if (keys[index].startsWith("__reactFiber$")) return element[keys[index]] || null;
      }
    } catch {
    }
    return null;
  };
  const reactInternals = (element, options = {}) =>
    reactInternalKeys(element, options).flatMap((key) => {
      try {
        return [element[key]];
      } catch {
        return [];
      }
    });

  const objectGraphIncludes = (value, predicate, options = {}) => {
    const ignoredKeys = options.ignoredKeys instanceof Set
      ? options.ignoredKeys
      : new Set(options.ignoredKeys || []);
    const maxDepth = Number.isFinite(options.maxDepth) ? options.maxDepth : 7;
    const seen = new WeakSet();
    const visit = (current, depth) => {
      if (typeof current === "string") return predicate(current);
      if (
        !current
        || typeof current !== "object"
        || depth > maxDepth
        || seen.has(current)
      ) return false;
      seen.add(current);
      let entries;
      try {
        entries = Object.entries(current);
      } catch {
        return false;
      }
      for (const [key, child] of entries) {
        if (ignoredKeys.has(key)) continue;
        if (visit(child, depth + 1)) return true;
      }
      return false;
    };
    return visit(value, 0);
  };

  const defaultReactTraversalIgnoredKeys =
    new Set(["return", "child", "sibling", "stateNode", "_owner"]);
  const reactInternalGraphIncludes = (element, predicate, options = {}) => {
    const ignoredKeys = options.ignoredKeys ?? defaultReactTraversalIgnoredKeys;
    const ancestorIgnoredKeys = options.ancestorIgnoredKeys ?? ignoredKeys;
    const maxDepth = Number.isFinite(options.maxDepth) ? options.maxDepth : 7;
    const ancestorDepth = Number.isFinite(options.ancestorDepth)
      ? Math.max(0, options.ancestorDepth)
      : 0;
    return reactInternals(element).some((internal) => {
      try {
        if (objectGraphIncludes(internal?.memoizedProps ?? internal, predicate, {
          ignoredKeys,
          maxDepth,
        })) {
          return true;
        }
        let ancestor = internal?.memoizedProps && ancestorDepth > 0
          ? internal.return
          : null;
        for (let depth = 0; ancestor && depth < ancestorDepth; depth += 1) {
          if (objectGraphIncludes(ancestor.memoizedProps, predicate, {
            ignoredKeys: ancestorIgnoredKeys,
            maxDepth,
          })) {
            return true;
          }
          ancestor = ancestor.return;
        }
        return false;
      } catch {
        return false;
      }
    });
  };

  const fetchInterceptors = new Map();
  let sortedFetchInterceptors = [];
  let fetchBase = typeof window.fetch === "function" ? window.fetch : null;
  const rebuildSortedFetchInterceptors = () => {
    sortedFetchInterceptors = [...fetchInterceptors.values()]
      .sort((left, right) => right.priority - left.priority);
  };
  const dispatchFetch = function dispatchCodeyFetch(...args) {
    const receiver = this;
    const interceptors = sortedFetchInterceptors;
    const invoke = (index, currentArgs) => {
      const entry = interceptors[index];
      if (!entry) {
        return Reflect.apply(fetchBase, receiver, currentArgs);
      }
      const next = (...nextArgs) => invoke(
        index + 1,
        nextArgs.length ? nextArgs : currentArgs,
      );
      return entry.interceptor(next, ...currentArgs);
    };
    return invoke(0, args);
  };

  const syncFetchDispatcher = () => {
    if (typeof window.fetch !== "function") return;
    if (fetchInterceptors.size === 0) {
      if (window.fetch === dispatchFetch && fetchBase) window.fetch = fetchBase;
      return;
    }
    if (window.fetch !== dispatchFetch) {
      fetchBase = window.fetch;
      window.fetch = dispatchFetch;
    }
  };

  const registerFetchInterceptor = (id, interceptor, priority = 0) => {
    if (typeof interceptor !== "function" || typeof window.fetch !== "function") {
      return () => {};
    }
    const key = String(id);
    const entry = { interceptor, priority: Number(priority) || 0 };
    fetchInterceptors.set(key, entry);
    rebuildSortedFetchInterceptors();
    syncFetchDispatcher();
    let registered = true;
    return () => {
      if (!registered) return;
      registered = false;
      if (fetchInterceptors.get(key) === entry) {
        fetchInterceptors.delete(key);
        rebuildSortedFetchInterceptors();
      }
      syncFetchDispatcher();
    };
  };

  const codeyNow = () =>
    typeof performance === "object" && typeof performance.now === "function"
      ? performance.now()
      : Date.now();

  // Lightweight observer telemetry: per-handler call count, mutation count and
  // wall time, exposed on window.__codeyObserverStats for performance triage.
  const codeyTimed = (name, count, run) => {
    const stats = (window.__codeyObserverStats ||= {});
    const entry = (stats[name] ||= { calls: 0, items: 0, totalMs: 0, maxMs: 0 });
    const startedAt = codeyNow();
    try {
      return run();
    } finally {
      const elapsed = codeyNow() - startedAt;
      entry.calls += 1;
      entry.items += count;
      entry.totalMs += elapsed;
      if (elapsed > entry.maxMs) entry.maxMs = elapsed;
    }
  };

  const callBridge = (path, payload = {}, options = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload, options);
    }
    if (options?.rejectOnUnavailable) {
      return Promise.reject(new Error("Codey bridge 尚未就绪"));
    }
    return Promise.resolve({
      status: "failed",
      code: "bridge_unavailable",
      message: "Codey bridge 不可用",
    });
  };

  const appServerRequestClient = () => {
    const clients = window.__codeyAppServerRequestClients;
    if (!clients || typeof clients.get !== "function") return null;
    const local = clients.get("local");
    if (local && typeof local.sendRequest === "function") return local;
    if (typeof clients.values !== "function") return null;
    const all = [...clients.values()].filter((client) => typeof client?.sendRequest === "function");
    return all.length === 1 ? all[0] : null;
  };

  const runtimeToastId = "codey-runtime-toast";
  const runtimeToastStyleId = "codey-runtime-toast-style";
  const showRuntimeToast = (message, tone = "success") => {
    if (!document?.documentElement) return;
    if (!document.getElementById?.(runtimeToastStyleId) && typeof document.createElement === "function") {
      const style = document.createElement("style");
      style.id = runtimeToastStyleId;
      style.textContent = `#${runtimeToastId} { -webkit-app-region: no-drag !important; position: fixed; right: 20px; bottom: 22px; z-index: 2147483645; max-width: 360px; border: 1px solid rgba(124, 140, 255, .4); border-radius: 11px; padding: 10px 13px; background: rgba(20, 24, 36, .97); color: #eef2ff; box-shadow: 0 12px 36px rgba(0,0,0,.4); font: 12px/1.45 system-ui, sans-serif; } #${runtimeToastId}[data-tone="error"] { border-color: rgba(248, 113, 113, .6); color: #fecaca; }`;
      document.documentElement.appendChild(style);
    }
    document.getElementById?.(runtimeToastId)?.remove?.();
    const toast = document.createElement("div");
    toast.id = runtimeToastId;
    toast.dataset.tone = tone;
    toast.setAttribute("role", tone === "error" ? "alert" : "status");
    toast.setAttribute("aria-live", tone === "error" ? "assertive" : "polite");
    toast.textContent = message;
    document.documentElement.appendChild(toast);
    const schedule = typeof window.setTimeout === "function" ? window.setTimeout.bind(window) : globalThis.setTimeout;
    schedule?.(() => toast.remove?.(), tone === "error" ? 8_000 : 3_500);
  };
  if (typeof window.__codeyShowRuntimeToast !== "function") {
    window.__codeyShowRuntimeToast = showRuntimeToast;
  }

  const injectionStatusChangedEvent = "codey-injection-status-changed";
  const publishInjectionStatus = (id, status, detail) => {
    const entry = window.__codeyInjectionStatus?.[id];
    if (!entry || entry.status === "pending") return;
    if (entry.status === status && entry.detail === detail && !entry.error) return;
    entry.status = status;
    entry.detail = detail;
    entry.error = null;
    if (
      typeof window.dispatchEvent === "function"
      && typeof window.CustomEvent === "function"
    ) {
      window.dispatchEvent(new window.CustomEvent(injectionStatusChangedEvent, {
        detail: { id, status },
      }));
    }
  };

  const createElementFrame = (element) => ({
    element,
    nextChildIndex: 0,
    visited: false,
  });
  const createElementContinuation = (root, continuation = null) => {
    const segments = continuation?.segments
      ? [...continuation.segments]
      : [];
    segments.push([createElementFrame(root)]);
    return { segments };
  };
  const hasElementContinuation = (continuation) => (
    continuation?.segments?.some?.((segment) => segment.length > 0) === true
  );
  const walkElementsBudgeted = (root, visitor, { continuation = null, remaining = null } = {}) => {
    const state = continuation || createElementContinuation(
      root instanceof Element ? root : document.documentElement,
    );
    let visited = 0;
    let childExpansions = 0;
    while (remaining ? remaining.value > 0 : true) {
      const segment = state.segments[0];
      if (!segment?.length) {
        state.segments.shift();
        if (!state.segments.length) break;
        continue;
      }
      const frame = segment[segment.length - 1];
      const element = frame.element;
      if (!(element instanceof Element)) {
        segment.pop();
        continue;
      }
      if (!frame.visited) {
        frame.visited = true;
        if (remaining) remaining.value -= 1;
        visited += 1;
        if (visitor(element) === false) {
          segment.pop();
          continue;
        }
      }
      if (remaining?.value <= 0) break;
      if (frame.nextChildIndex < element.children.length) {
        const child = element.children[frame.nextChildIndex];
        frame.nextChildIndex += 1;
        childExpansions += 1;
        segment.push(createElementFrame(child));
      } else {
        segment.pop();
      }
    }
    return {
      childExpansions,
      continuation: {
        segments: state.segments.filter((segment) => segment.length > 0),
      },
      visited,
    };
  };

  window.__codeySharedRuntime = Object.freeze({
    appServerRequestClient,
    callBridge,
    codeyTimed,
    createElementContinuation,
    fetchSnapshot: () => Object.freeze({
      installed: window.fetch === dispatchFetch,
      interceptorCount: fetchInterceptors.size,
    }),
    hasElementContinuation,
    objectGraphIncludes,
    publishInjectionStatus,
    reactFiberFor,
    reactInternalGraphIncludes,
    reactInternalKeys,
    reactInternals,
    registerFetchInterceptor,
    showRuntimeToast,
    statsigClients,
    walkElementsBudgeted,
  });

  const mutationSubscribers = new Map();
  let mutationObserver = null;
  let installedMutationOptions = null;
  let nextMutationSubscriberId = 1;
  const pendingMutationBatches = [];
  let mutationDispatchRunning = false;
  let mutationDispatchScheduled = false;
  const mutationDispatchBudgetMs = 8;
  const maxMutationSubscribersPerFrame = 16;

  const dispatchMutations = (mutations) => {
    codeyTimed("bridge.dispatchMutations", mutations?.length ?? 0, () => {
      dispatchMutationsImpl(mutations);
    });
  };
  // The shared observer unions every subscriber's options, so a class flip
  // anywhere in the document would otherwise wake the childList-only shields.
  // Hand each subscriber only the records its own options would have produced;
  // records of an unknown type are delivered to everyone (fail open).
  const mutationBelongsTo = (mutation, subscriber) => {
    const type = mutation?.type;
    if (type === "childList") return subscriber.childList;
    if (type === "attributes") {
      if (!subscriber.attributes) return false;
      if (subscriber.attributeFilter === null) return true;
      return subscriber.attributeFilter.has(mutation.attributeName);
    }
    return true;
  };
  const mutationsFor = (mutations, subscriber) => {
    if (!Array.isArray(mutations)) return mutations;
    let filtered = null;
    for (let index = 0; index < mutations.length; index += 1) {
      const belongs = mutationBelongsTo(mutations[index], subscriber);
      if (filtered === null) {
        if (belongs) continue;
        filtered = mutations.slice(0, index);
      } else if (belongs) {
        filtered.push(mutations[index]);
      }
    }
    return filtered === null ? mutations : filtered;
  };

  let dispatchMutationsFrame = null;
  const scheduleMutationDispatch = () => {
    if (
      mutationDispatchScheduled
      || mutationDispatchRunning
      || !pendingMutationBatches.length
    ) return;
    if (typeof window.requestAnimationFrame === "function") {
      mutationDispatchScheduled = true;
      window.requestAnimationFrame(() => {
        mutationDispatchScheduled = false;
        dispatchMutationsFrame();
      });
      return;
    }
    const scheduleTimer =
      typeof window.setTimeout === "function"
        ? window.setTimeout.bind(window)
        : typeof globalThis.setTimeout === "function"
          ? globalThis.setTimeout
          : null;
    if (!scheduleTimer) return;
    mutationDispatchScheduled = true;
    scheduleTimer(() => {
      mutationDispatchScheduled = false;
      dispatchMutationsFrame();
    }, 0);
  };

  const batchSynchronousIndex = (batch) => {
    const cached = batch.synchronousIndex;
    if (cached === -1) return cached;
    if (cached !== undefined && cached >= batch.index) {
      const candidate = batch.subscribers[cached];
      if (mutationSubscribers.get(candidate.id) === candidate) return cached;
    }
    let found = -1;
    for (let index = batch.index; index < batch.subscribers.length; index += 1) {
      const candidate = batch.subscribers[index];
      if (mutationSubscribers.get(candidate.id) !== candidate) continue;
      try {
        if (candidate.requiresSynchronousDelivery?.(batch.mutations) === true) {
          found = index;
          break;
        }
      } catch {
      }
    }
    batch.synchronousIndex = found;
    return found;
  };
  const nextSynchronousDelivery = () => {
    for (const batch of pendingMutationBatches) {
      const index = batchSynchronousIndex(batch);
      if (index >= 0) return { batch, index };
    }
    return null;
  };

  dispatchMutationsFrame = () => {
    if (mutationDispatchRunning) return;
    mutationDispatchRunning = true;
    const deadline = codeyNow() + mutationDispatchBudgetMs;
    let processedSubscribers = 0;
    try {
      while (pendingMutationBatches.length) {
        const batch = pendingMutationBatches[0];
        if (batch.index >= batch.subscribers.length) {
          pendingMutationBatches.shift();
          continue;
        }
        if (
          processedSubscribers >= maxMutationSubscribersPerFrame
          || codeyNow() >= deadline
        ) {
          // Direct controls must keep their pre-paint path. Continue earlier
          // batches in order until that subscriber has run, then resume the
          // normal per-frame budget.
          const synchronous = nextSynchronousDelivery();
          if (!synchronous) break;
        }

        const subscriber = batch.subscribers[batch.index];
        batch.index += 1;
        processedSubscribers += 1;
        if (mutationSubscribers.get(subscriber.id) !== subscriber) continue;
        const own = mutationsFor(batch.mutations, subscriber);
        if (Array.isArray(own) && own.length === 0) continue;
        try {
          subscriber.callback(own);
        } catch (error) {
          window.console?.error?.("[Codey] mutation subscriber failed", error);
        }
      }
    } finally {
      mutationDispatchRunning = false;
    }
    if (pendingMutationBatches.length) scheduleMutationDispatch();
  };
  const dispatchMutationsImpl = (mutations) => {
    const batch = {
      index: 0,
      mutations,
      subscribers: [...mutationSubscribers.values()],
      synchronousIndex: undefined,
    };
    pendingMutationBatches.push(batch);
    const hasSynchronousDelivery = batchSynchronousIndex(batch) >= 0;
    if (
      !mutationDispatchRunning
      && (!mutationDispatchScheduled || hasSynchronousDelivery)
    ) {
      dispatchMutationsFrame();
    }
  };

  const syncMutationObserver = () => {
    const stopObserving = () => {
      mutationObserver?.disconnect();
      mutationObserver = null;
      installedMutationOptions = null;
    };
    if (
      !mutationSubscribers.size
      || typeof MutationObserver !== "function"
      || !document.documentElement
    ) {
      stopObserving();
      return;
    }

    let attributes = false;
    let attributeOldValue = false;
    let childList = false;
    let observeAllAttributes = false;
    const attributeFilter = new Set();
    for (const subscriber of mutationSubscribers.values()) {
      childList ||= subscriber.childList;
      attributes ||= subscriber.attributes;
      attributeOldValue ||= subscriber.attributeOldValue;
      if (!subscriber.attributes) continue;
      if (subscriber.attributeFilter === null) {
        observeAllAttributes = true;
      } else {
        subscriber.attributeFilter.forEach((attribute) => attributeFilter.add(attribute));
      }
    }
    if (!attributes && !childList) {
      stopObserving();
      return;
    }

    const options = { attributes, childList, subtree: true };
    if (attributes && attributeOldValue) options.attributeOldValue = true;
    if (attributes && !observeAllAttributes && attributeFilter.size) {
      options.attributeFilter = [...attributeFilter];
    }
    // 对同一节点重新 observe 只替换选项；disconnect 会丢掉尚未投递的记录。
    mutationObserver ||= new MutationObserver(dispatchMutations);
    mutationObserver.observe(document.documentElement, options);
    installedMutationOptions = options;
  };

  const createScopedObserver = (callback, options = {}) => {
    let scopedObserver = null;
    let observedTarget = null;
    const disconnect = () => {
      scopedObserver?.disconnect();
      observedTarget = null;
    };
    const observe = (target) => {
      if (!target || typeof target !== "object") {
        disconnect();
        return false;
      }
      if (target === observedTarget && scopedObserver) return true;
      if (typeof MutationObserver !== "function") return false;
      scopedObserver?.disconnect();
      scopedObserver ||= new MutationObserver(callback);
      scopedObserver.observe(target, options);
      observedTarget = target;
      return true;
    };
    return Object.freeze({
      currentTarget: () => observedTarget,
      disconnect,
      observe,
    });
  };

  const subscribeMutations = (callback, options = {}) => {
    if (typeof callback !== "function") return () => {};
    const id = nextMutationSubscriberId;
    nextMutationSubscriberId += 1;
    const attributes = options.attributes === true;
    const subscriber = {
      callback,
      attributes,
      attributeOldValue: attributes && options.attributeOldValue === true,
      childList: options.childList === true,
      attributeFilter: attributes && Array.isArray(options.attributeFilter)
        ? new Set(options.attributeFilter.map(String))
        : null,
      id,
      requiresSynchronousDelivery: typeof options.requiresSynchronousDelivery === "function"
        ? options.requiresSynchronousDelivery
        : null,
    };
    mutationSubscribers.set(id, subscriber);
    syncMutationObserver();

    let subscribed = true;
    return () => {
      if (!subscribed) return;
      subscribed = false;
      mutationSubscribers.delete(id);
      syncMutationObserver();
    };
  };

  const controlDescriptor = (control) => [
    control?.getAttribute?.("aria-label"),
    control?.getAttribute?.("title"),
    control?.textContent,
  ].filter(Boolean).join(" ").replace(/\s+/g, " ").trim();

  const defaultStreamingTurnSelector = "[data-turn-key]";
  const defaultCriticalContextSelector = [
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    "[data-codey-critical-context]",
    "aside",
    "[data-above-composer-conversation-id]",
    "[data-app-action-sidebar-thread-active]",
    "[data-app-action-sidebar-thread-id]",
    "[data-sidebar]",
    "[data-testid*='sidebar']",
    "[data-testid*='composer']",
    "[data-radix-portal]",
    "[data-testid*='portal']",
    "[data-testid*='dialog']",
    "[data-testid*='modal']",
    "[data-slot='sidebar']",
    "[data-slot='dialog']",
    "[data-slot='popover']",
    "[data-slot='sheet']",
    "[data-slot='drawer']",
  ].join(", ");

  const createShieldLifecycle = ({
    attributeFilter,
    block,
    criticalContextSelector = defaultCriticalContextSelector,
    eventSelector,
    frameRootLimit = 24,
    nodeLimitPerRoot = 256,
    isControl,
    mutationSelector = eventSelector,
    pendingRootLimit = 64,
    probeNodeLimit = 0,
    streamingTurnSelector = defaultStreamingTurnSelector,
  }) => {
    let flushTimer = 0;
    let cancelPendingFlush = null;
    let active = true;
    let flushing = false;
    const pendingRoots = new Set();
    const resumedRoots = new Set();
    const normalizedPendingRootLimit = Number.isInteger(pendingRootLimit)
      ? Math.max(1, pendingRootLimit)
      : 64;
    const normalizedFrameRootLimit = Number.isInteger(frameRootLimit)
      ? Math.max(1, frameRootLimit)
      : 24;
    const normalizedNodeLimit = Number.isInteger(nodeLimitPerRoot)
      ? Math.max(1, nodeLimitPerRoot)
      : 256;
    const normalizedProbeNodeLimit = Number.isInteger(probeNodeLimit)
      ? Math.max(0, probeNodeLimit)
      : 0;
    const nextTraversalCursors = new WeakMap();
    const clearTraversalCursor = (root) => {
      nextTraversalCursors.delete(root);
    };
    const addPendingRoot = (root) => {
      if (!(root instanceof HTMLElement) || pendingRoots.has(root)) return;
      for (const pending of pendingRoots) {
        if (pending.contains?.(root)) return;
      }
      for (const pending of [...pendingRoots]) {
        if (root.contains?.(pending)) {
          clearTraversalCursor(pending);
          pendingRoots.delete(pending);
        }
      }
      while (pendingRoots.size >= normalizedPendingRootLimit) {
        const evicted = pendingRoots.values().next().value;
        clearTraversalCursor(evicted);
        pendingRoots.delete(evicted);
      }
      pendingRoots.add(root);
    };
    const scanRoot = (
      root,
      { maxNodes = normalizedNodeLimit, restart = false } = {},
    ) => {
      if (!(root instanceof HTMLElement)) {
        return { blocked: 0, progressed: false };
      }
      const startCursor = restart ? null : nextTraversalCursors.get(root) || null;
      let progress = null;
      const blocked = block(root, {
        maxNodes,
        maxControls: maxNodes,
        onProgress: (nextProgress) => {
          progress = nextProgress;
        },
        startCursor,
      });
      const nextCursor = progress?.nextCursor;
      if (!progress || progress.complete) {
        clearTraversalCursor(root);
        return { blocked, progressed: Boolean(progress) };
      }
      if (nextCursor == null) {
        const nextIndex = Number.isInteger(progress.nextIndex)
          ? progress.nextIndex
          : -1;
        if (nextIndex <= (Number.isInteger(startCursor) ? startCursor : -1)) {
          clearTraversalCursor(root);
          return { blocked, progressed: false };
        }
        nextTraversalCursors.set(root, nextIndex);
      } else {
        if (nextCursor === startCursor) {
          clearTraversalCursor(root);
          return { blocked, progressed: false };
        }
        nextTraversalCursors.set(root, nextCursor);
      }
      if (flushing) resumedRoots.add(root);
      else addPendingRoot(root);
      return { blocked, progressed: true };
    };

    const schedulePendingFlush = () => {
      if (flushTimer) return;
      if (typeof window.requestAnimationFrame === "function") {
        flushTimer = window.requestAnimationFrame(flushPendingRoots);
        cancelPendingFlush = () => window.cancelAnimationFrame?.(flushTimer);
        return;
      }
      if (typeof window.setTimeout !== "function") {
        flushPendingRoots();
        return;
      }
      flushTimer = window.setTimeout(flushPendingRoots, 0);
      cancelPendingFlush = () => window.clearTimeout?.(flushTimer);
    };

    const flushPendingRoots = () => {
      flushTimer = 0;
      cancelPendingFlush = null;
      let remaining = normalizedFrameRootLimit;
      flushing = true;
      try {
        while (pendingRoots.size && remaining > 0) {
          const root = pendingRoots.values().next().value;
          pendingRoots.delete(root);
          remaining -= 1;
          if (root.isConnected === false) {
            clearTraversalCursor(root);
            continue;
          }
          scanRoot(root);
        }
      } finally {
        flushing = false;
      }
      for (const root of resumedRoots) addPendingRoot(root);
      resumedRoots.clear();
      if (pendingRoots.size && active) schedulePendingFlush();
    };

    const directControlFor = (root) => (
      root instanceof HTMLElement && root.matches?.(mutationSelector) ? root : null
    );
    const mutationRoot = (node) => {
      if (node instanceof HTMLElement) return node;
      return node?.parentElement instanceof HTMLElement ? node.parentElement : null;
    };
    const hasDirectControlMutation = (mutations) => {
      for (const mutation of mutations || []) {
        const target = mutationRoot(mutation?.target);
        if (directControlFor(target?.closest?.(mutationSelector))) return true;
        if (mutation?.type === "attributes") continue;
        for (const node of mutation?.addedNodes || []) {
          if (directControlFor(node)) return true;
        }
      }
      return false;
    };

    const blockBeforePaint = (root) => {
      if (!(root instanceof HTMLElement) || root.isConnected === false) return 0;
      const directControl = directControlFor(root);
      if (directControl) {
        if (!isControl(directControl)) return 0;
        scanRoot(root, { restart: true });
        return 1;
      }
      if (normalizedProbeNodeLimit === 0) return 0;
      // Probe a bounded prefix before deferring. A deep querySelector() would
      // still traverse the entire streamed subtree even though the following
      // inspection is budgeted, which defeats the frame budget on large roots.
      const result = scanRoot(root, {
        maxNodes: Math.min(normalizedNodeLimit, normalizedProbeNodeLimit),
        restart: true,
      });
      // Block callbacks without cursor progress use an opaque full-root scan.
      // Keep their previous deferred behavior instead of assuming no controls.
      return result.progressed ? 1 : 0;
    };

    const queueMutationRoot = (root, { probeBudget } = {}) => {
      if (!(root instanceof HTMLElement) || !active) return;
      if (directControlFor(root)) {
        if (isControl(root)) scanRoot(root, { restart: true });
        return;
      }
      if (probeBudget && probeBudget.remaining > 0) {
        probeBudget.remaining -= 1;
        if (blockBeforePaint(root) > 0) return;
      }
      addPendingRoot(root);
    };

    const unsubscribeMutations = document.documentElement
      ? subscribeMutations((mutations) => {
        const probeBudget = { remaining: 8 };
        for (const mutation of mutations) {
          const target = mutationRoot(mutation.target);
          const targetTurn = target?.closest?.(streamingTurnSelector);
          const targetCritical = Boolean(target?.closest?.(criticalContextSelector));
          const containingControl = target?.closest?.(mutationSelector);
          if (containingControl && (containingControl === target || !targetTurn)) {
            queueMutationRoot(containingControl, { probeBudget });
          }
          if (mutation.type === "attributes") continue;

          for (const node of mutation.addedNodes || []) {
            if (!(node instanceof HTMLElement)) continue;
            const nodeCritical = Boolean(
              node.matches?.(criticalContextSelector)
                || node.closest?.(criticalContextSelector),
            );
            const nodeTurn = targetTurn || node.closest?.(streamingTurnSelector);
            const nodeDirectControl = directControlFor(node);
            if (nodeTurn && !targetCritical && !nodeCritical && !nodeDirectControl) {
              continue;
            }
            queueMutationRoot(node, {
              probeBudget,
            });
          }
        }
        if (pendingRoots.size) schedulePendingFlush();
      }, {
        attributes: true,
        attributeFilter,
        childList: true,
        requiresSynchronousDelivery: hasDirectControlMutation,
      })
      : null;

    const stopControlEvent = (event) => {
      const control = event.target instanceof Element
        ? event.target.closest(eventSelector)
        : null;
      if (!isControl(control)) return;
      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation?.();
    };
    const eventNames = ["pointerdown", "click", "keydown"];
    eventNames.forEach((eventName) => {
      document.addEventListener(eventName, stopControlEvent, true);
    });

    return Object.freeze({
      cleanup: () => {
        if (!active) return;
        active = false;
        unsubscribeMutations?.();
        if (flushTimer) cancelPendingFlush?.();
        flushTimer = 0;
        cancelPendingFlush = null;
        pendingRoots.clear();
        resumedRoots.clear();
        eventNames.forEach((eventName) => {
          document.removeEventListener(eventName, stopControlEvent, true);
        });
      },
      observerInstalled: unsubscribeMutations !== null,
    });
  };

  window.__codeyMutationDispatcher = Object.freeze({
    controlDescriptor,
    createScopedObserver,
    createShieldLifecycle,
    snapshot: () => Object.freeze({
      attributeFilter: installedMutationOptions?.attributeFilter
        ? [...installedMutationOptions.attributeFilter]
        : null,
      attributeOldValue: Boolean(installedMutationOptions?.attributeOldValue),
      observerInstalled: mutationObserver !== null,
      subscriberCount: mutationSubscribers.size,
    }),
    subscribe: subscribeMutations,
  });
  window.__codeyCall = (path, payload = {}, options = {}) => callBridge(path, payload, options);
})();
