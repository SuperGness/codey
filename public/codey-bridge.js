(() => {
  if (
    window.__codeyBridgeHelpersInstalled
    && window.__codeyMutationDispatcher?.createShieldLifecycle
    && window.__codeyMutationDispatcher?.controlDescriptor
    && window.__codeyMutationDispatcher?.controlsWithin
    && window.__codeySharedRuntime?.reactInternalGraphIncludes
    && window.__codeySharedRuntime?.reactInternalKeys
    && window.__codeySharedRuntime?.registerFetchInterceptor
    && window.__codeySharedRuntime?.statsigClients
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

  const reactInternalKeyPattern = /^__(?:reactProps|reactFiber|reactInternalInstance)\$.*/;
  const reactInspectableKeyPattern = /^__(?:reactProps|reactFiber|reactInternalInstance)/;
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
  let fetchBase = typeof window.fetch === "function" ? window.fetch : null;
  const dispatchFetch = function dispatchCodeyFetch(...args) {
    const receiver = this;
    const interceptors = [...fetchInterceptors.values()]
      .sort((left, right) => right.priority - left.priority);
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
    syncFetchDispatcher();
    let registered = true;
    return () => {
      if (!registered) return;
      registered = false;
      if (fetchInterceptors.get(key) === entry) fetchInterceptors.delete(key);
      syncFetchDispatcher();
    };
  };

  window.__codeySharedRuntime = Object.freeze({
    fetchSnapshot: () => Object.freeze({
      installed: window.fetch === dispatchFetch,
      interceptorCount: fetchInterceptors.size,
    }),
    objectGraphIncludes,
    reactInternalGraphIncludes,
    reactInternalKeys,
    reactInternals,
    registerFetchInterceptor,
    statsigClients,
  });

  const mutationSubscribers = new Map();
  let mutationObserver = null;
  let nextMutationSubscriberId = 1;

  // Lightweight observer telemetry: per-handler call count, mutation count and
  // wall time, exposed on window.__codeyObserverStats for performance triage.
  // No behavior change; timing falls back to Date.now() where performance is
  // unavailable (test sandboxes).
  const codeyTimed = (name, count, run) => {
    const now = () =>
      typeof performance === "object" && typeof performance.now === "function"
        ? performance.now()
        : Date.now();
    const stats = (window.__codeyObserverStats ||= {});
    const entry = (stats[name] ||= { calls: 0, items: 0, totalMs: 0, maxMs: 0 });
    const startedAt = now();
    try {
      return run();
    } finally {
      const elapsed = now() - startedAt;
      entry.calls += 1;
      entry.items += count;
      entry.totalMs += elapsed;
      if (elapsed > entry.maxMs) entry.maxMs = elapsed;
    }
  };
  const dispatchMutations = (mutations) =>
    codeyTimed("bridge.dispatchMutations", mutations?.length ?? 0, () => dispatchMutationsImpl(mutations));
  const dispatchMutationsImpl = (mutations) => {
    for (const subscriber of [...mutationSubscribers.values()]) {
      try {
        subscriber.callback(mutations);
      } catch (error) {
        window.console?.error?.("[Codey] mutation subscriber failed", error);
      }
    }
  };

  const syncMutationObserver = () => {
    mutationObserver?.disconnect();
    mutationObserver = null;
    if (
      !mutationSubscribers.size
      || typeof MutationObserver !== "function"
      || !document.documentElement
    ) {
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
    if (!attributes && !childList) return;

    const options = { attributes, childList, subtree: true };
    if (attributes && attributeOldValue) options.attributeOldValue = true;
    if (attributes && !observeAllAttributes && attributeFilter.size) {
      options.attributeFilter = [...attributeFilter];
    }
    mutationObserver = new MutationObserver(dispatchMutations);
    mutationObserver.observe(document.documentElement, options);
  };

  const subscribeMutations = (callback, options = {}) => {
    if (typeof callback !== "function") return () => {};
    const id = nextMutationSubscriberId;
    nextMutationSubscriberId += 1;
    const attributes = options.attributes === true;
    mutationSubscribers.set(id, {
      callback,
      attributes,
      attributeOldValue: attributes && options.attributeOldValue === true,
      childList: options.childList === true,
      attributeFilter: attributes && Array.isArray(options.attributeFilter)
        ? [...new Set(options.attributeFilter.map(String))]
        : null,
    });
    syncMutationObserver();

    let subscribed = true;
    return () => {
      if (!subscribed) return;
      subscribed = false;
      mutationSubscribers.delete(id);
      syncMutationObserver();
    };
  };

  const controlsWithin = (root, selector) => {
    const controls = [];
    if (root instanceof HTMLElement && root.matches?.(selector)) controls.push(root);
    if (root && typeof root.querySelectorAll === "function") {
      controls.push(...root.querySelectorAll(selector));
    }
    return controls;
  };

  const controlDescriptor = (control) => [
    control?.getAttribute?.("aria-label"),
    control?.getAttribute?.("title"),
    control?.textContent,
  ].filter(Boolean).join(" ").replace(/\s+/g, " ").trim();

  const createShieldLifecycle = ({
    attributeFilter,
    block,
    eventSelector,
    isControl,
    mutationSelector = eventSelector,
  }) => {
    let flushTimer = 0;
    let cancelPendingFlush = null;
    let active = true;
    const pendingRoots = new Set();
    const pendingRootLimit = 64;
    const documentRoot = document.documentElement || document.body;

    const addPendingRoot = (root) => {
      if (!(root instanceof HTMLElement) || pendingRoots.has(root)) return;
      if (documentRoot && pendingRoots.has(documentRoot)) return;
      if (pendingRoots.size >= pendingRootLimit && documentRoot) {
        pendingRoots.clear();
        pendingRoots.add(documentRoot);
        return;
      }
      for (const pending of pendingRoots) {
        if (pending.contains?.(root)) return;
      }
      for (const pending of [...pendingRoots]) {
        if (root.contains?.(pending)) pendingRoots.delete(pending);
      }
      pendingRoots.add(root);
    };

    const flushPendingRoots = () => {
      flushTimer = 0;
      cancelPendingFlush = null;
      if (!pendingRoots.size) return;
      const roots = [...pendingRoots];
      pendingRoots.clear();
      for (const root of roots) {
        if (root.isConnected === false) continue;
        block(root);
      }
    };

    const blockBeforePaint = (root) => {
      if (!(root instanceof HTMLElement) || root.isConnected === false) return 0;
      const hasControlCandidate =
        root.matches?.(mutationSelector) || root.querySelector?.(mutationSelector);
      return hasControlCandidate ? block(root) : 0;
    };

    const queueMutationRoot = (root) => {
      if (!(root instanceof HTMLElement)) return;
      if (blockBeforePaint(root) > 0) return;
      addPendingRoot(root);
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

    const mutationRoot = (node) => {
      if (node instanceof HTMLElement) return node;
      return node?.parentElement instanceof HTMLElement ? node.parentElement : null;
    };
    const unsubscribeMutations = document.documentElement
      ? subscribeMutations((mutations) => {
        for (const mutation of mutations) {
          const target = mutationRoot(mutation.target);
          const containingControl = target?.closest?.(mutationSelector);
          if (containingControl) queueMutationRoot(containingControl);
          if (mutation.type === "attributes") continue;
          for (const node of mutation.addedNodes || []) {
            if (node instanceof HTMLElement) queueMutationRoot(node);
          }
        }
        if (pendingRoots.size) schedulePendingFlush();
      }, {
        attributes: true,
        attributeFilter,
        childList: true,
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
        eventNames.forEach((eventName) => {
          document.removeEventListener(eventName, stopControlEvent, true);
        });
      },
      observerInstalled: unsubscribeMutations !== null,
    });
  };

  window.__codeyMutationDispatcher = Object.freeze({
    controlDescriptor,
    controlsWithin,
    createShieldLifecycle,
    snapshot: () => Object.freeze({
      observerInstalled: mutationObserver !== null,
      subscriberCount: mutationSubscribers.size,
    }),
    subscribe: subscribeMutations,
  });
  window.__codeyCall = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };
  window.__codeyRefreshSession = (detail = {}) => window.dispatchEvent(new CustomEvent("codey-session-refresh", { detail }));
})();
