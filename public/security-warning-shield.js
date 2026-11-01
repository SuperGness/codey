(() => {
  if (window.__codeySecurityWarningShieldInstalled) return;
  window.__codeySecurityWarningShieldInstalled = true;

  const configEventName = "codey:config-changed";
  const injectionStatusId = "security-warning-shield";
  const injectionStatusChangedEvent = "codey-injection-status-changed";
  const dismissedAttribute = "data-codey-security-warning-dismissed";
  const actionPatterns = [
    /^hide from this session$/i,
    /^dismiss full access warning for this session$/i,
    /^don['’]t show again$/i,
    /^(?:在|于)?本次会话(?:中)?(?:隐藏|不再显示)$/,
    /^(?:隐藏|不再显示)(?:本次会话)?$/,
  ];
  const titlePatterns = [
    /full access is on/i,
    /完(?:全|整)访问权限.*(?:已开启|开启中|已打开)/,
    /enable ultra with full access/i,
    /是否启用\s*ultra\s*搭配完整访问权限/i,
  ];
  const ultraTitlePatterns = [
    /enable ultra with full access/i,
    /是否启用\s*ultra\s*搭配完整访问权限/i,
  ];
  const ultraActionPatterns = [
    /^use full access$/i,
    /^enable full access$/i,
    /^使用完整访问权限$/,
  ];
  const riskPatterns = [
    /without your permission/i,
    /without your approval/i,
    /risk of data loss/i,
    /prompt injection/i,
    /未经(?:你|您)(?:的)?(?:许可|批准)/,
    /数据丢失/,
    /提示词?注入/,
    /with ultra and full access enabled/i,
    /开启\s*ultra\s*和完整访问权限后/i,
  ];
  const conversationTurnSelector = "[data-turn-key]";
  const warningSurfaceSelector = [
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    "textarea",
    "[contenteditable='true']",
    "[contenteditable='plaintext-only']",
    "[role='textbox']",
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-radix-portal]",
    "[data-portal]",
  ].join(", ");
  let enabled = false;
  let scanTimer = 0;
  let unsubscribeMutations = null;

  const publishInjectionStatus = () => {
    const entry = window.__codeyInjectionStatus?.[injectionStatusId];
    if (!entry || entry.status === "pending") return;
    const status = enabled ? "effective" : "inactive";
    const detail = enabled
      ? "安全提示屏蔽已启用"
      : "控制器已就绪，当前屏蔽策略关闭";
    if (entry.status === status && entry.detail === detail && !entry.error) return;
    entry.status = status;
    entry.detail = detail;
    entry.error = null;
    if (
      typeof window.dispatchEvent === "function"
      && typeof window.CustomEvent === "function"
    ) {
      window.dispatchEvent(new window.CustomEvent(injectionStatusChangedEvent, {
        detail: { id: injectionStatusId, status },
      }));
    }
  };

  // innerText forces a layout flush. Read visible text and accessible labels
  // directly so icon-only controls remain layout-free as well.
  const normalizedValue = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const normalizedText = (element) => normalizedValue(element?.textContent);
  const normalizedControlLabels = (control) => [
    control?.getAttribute?.("aria-label"),
    control?.textContent,
  ].map(normalizedValue).filter(Boolean);
  const matchesAny = (value, patterns) => patterns.some((pattern) => pattern.test(value));

  const hasWarningControl = (root) => (
    [...(root.querySelectorAll?.("button, [role=button]") || [])]
      .some((control) => normalizedControlLabels(control).some((label) => (
        matchesAny(label, actionPatterns)
        || matchesAny(label, ultraActionPatterns)
      )))
  );

  const hasMountedWarning = (root) => (
    matchesAny(normalizedText(root), titlePatterns)
    || hasWarningControl(root)
  );

  const shouldSkipMountedTurnMutation = (mutation, target) => {
    if (
      (mutation.type !== "childList" && mutation.type !== "characterData")
      || !target?.closest?.(conversationTurnSelector)
    ) {
      return false;
    }
    const targetSurface = target.closest?.(warningSurfaceSelector);
    const targetPortal = target.closest?.("[data-radix-portal], [data-portal]");
    if (
      targetSurface
      && (
        targetSurface !== targetPortal
        || (
          targetPortal
          && (
            matchesAny(normalizedText(targetPortal), titlePatterns)
            || hasWarningControl(targetPortal)
          )
        )
      )
    ) {
      return false;
    }
    return ![...(mutation.addedNodes || [])].some((node) => (
      node instanceof Element
      && (
        node.matches?.(warningSurfaceSelector) && hasMountedWarning(node)
        || [...(node.children || [])].some((child) => (
          child.matches?.(warningSurfaceSelector)
          || child.querySelector?.(warningSurfaceSelector)
        ))
      )
    ));
  };

  const warningContainerFor = (control) => {
    let candidate = control?.parentElement || null;
    for (let depth = 0; candidate && depth < 8; depth += 1) {
      if (candidate === document.body || candidate === document.documentElement) break;
      const text = normalizedText(candidate);
      if (matchesAny(text, titlePatterns) && matchesAny(text, riskPatterns)) {
        return candidate;
      }
      candidate = candidate.parentElement;
    }
    return null;
  };

  const createFrame = (element) => ({
    element,
    nextChildIndex: 0,
    visited: false,
  });
  const createContinuation = (root, continuation = null) => {
    const segments = continuation?.segments
      ? [...continuation.segments]
      : [];
    segments.push([createFrame(root)]);
    return { segments };
  };
  const hasContinuation = (continuation) => (
    continuation?.segments?.some?.((segment) => segment.length > 0) === true
  );

  const controlsWithin = (
    root,
    { continuation = null, remaining = null } = {},
  ) => {
    const controls = [];
    const state = continuation || createContinuation(
      root instanceof Element ? root : document.documentElement,
    );
    let visited = 0;
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
        scannedElements += 1;
        visited += 1;
        if (element.matches?.("button, [role=button]")) controls.push(element);
      }
      if (remaining?.value <= 0) break;
      if (frame.nextChildIndex < element.children.length) {
        const child = element.children[frame.nextChildIndex];
        frame.nextChildIndex += 1;
        childExpansions += 1;
        segment.push(createFrame(child));
      } else {
        segment.pop();
      }
    }
    scanCount += 1;
    return {
      continuation: {
        segments: state.segments.filter((segment) => segment.length > 0),
      },
      controls,
      visited,
    };
  };

  let scheduleScan = () => {};
  let addPendingRoot = () => {};

  const dismissWarnings = (root = document, controls = null) => {
    if (!enabled) return 0;
    const traversal = controls
      ? { controls }
      : controlsWithin(root);
    let dismissed = 0;
    for (const control of traversal.controls) {
      if (control.disabled || control.getAttribute?.(dismissedAttribute) === "true") {
        continue;
      }
      const labels = normalizedControlLabels(control);
      const isGeneralAction = labels.some((label) => matchesAny(label, actionPatterns));
      const isUltraAction = labels.some((label) => matchesAny(label, ultraActionPatterns));
      if (!isGeneralAction && !isUltraAction) continue;
      const container = warningContainerFor(control);
      if (!container) continue;
      const isUltraConfirmation = matchesAny(normalizedText(container), ultraTitlePatterns);
      if (isUltraConfirmation && !isUltraAction) continue;
      if (!isUltraConfirmation && !isGeneralAction) continue;
      control.setAttribute?.(dismissedAttribute, "true");
      container.setAttribute?.(dismissedAttribute, "true");
      control.click?.();
      if (isUltraConfirmation) container.remove?.();
      if (container.isConnected !== false) {
        container.style?.setProperty?.("display", "none", "important");
      }
      dismissed += 1;
    }
    if (!controls && hasContinuation(traversal.continuation)) {
      pendingRootStates.set(root, traversal.continuation);
      addPendingRoot(root);
      scheduleScan();
    }
    return dismissed;
  };

  const setEnabled = (next) => {
    const wasEnabled = enabled;
    enabled = next === true;
    if (!enabled) {
      if (scanTimer) {
        window.clearTimeout?.(scanTimer);
        scanTimer = 0;
      }
      pendingRoots.clear();
      unsubscribeMutations?.();
      unsubscribeMutations = null;
    } else if (!wasEnabled) {
      ensureObserver();
      addPendingRoot(document.documentElement);
      scheduleScan();
    }
    publishInjectionStatus();
    return enabled;
  };

  const refreshConfig = async () => {
    if (typeof window.__codexSessionDeleteBridge !== "function") {
      return setEnabled(false);
    }
    try {
      const config = await window.__codexSessionDeleteBridge("/settings/get", {});
      return setEnabled(config?.hideFullAccessWarning === true);
    } catch {
      return setEnabled(false);
    }
  };

  const pendingRoots = new Set();
  const pendingRootStates = new WeakMap();
  const pendingRootLimit = 32;
  const batchElementLimit = 160;
  let scanCount = 0;
  let scannedElements = 0;
  let childExpansions = 0;

  const mergeContinuations = (...continuations) => {
    const segments = [];
    const seenElements = new Set();
    for (const continuation of continuations) {
      for (const segment of continuation?.segments || []) {
        if (!segment.some((frame) => !seenElements.has(frame.element))) continue;
        for (const frame of segment) seenElements.add(frame.element);
        segments.push(segment);
      }
    }
    return { segments };
  };

  addPendingRoot = (root, continuation = pendingRootStates.get(root)) => {
    if (!(root instanceof Element) || root.isConnected === false) return;
    for (const pendingRoot of pendingRoots) {
      if (pendingRoot === root || pendingRoot.contains?.(root)) {
        const rootState = pendingRootStates.get(root);
        const pendingState = pendingRootStates.get(pendingRoot);
        if (hasContinuation(rootState) || hasContinuation(pendingState)) {
          pendingRootStates.set(
            pendingRoot,
            mergeContinuations(
              pendingState,
              hasContinuation(rootState)
                ? rootState
                : createContinuation(root, pendingState),
            ),
          );
        }
        return;
      }
    }
    let mergedContinuation = pendingRootStates.get(root) || pendingRootStates.get(
      [...pendingRoots].find((pendingRoot) => pendingRoot.contains?.(root)),
    ) || { segments: [] };
    for (const pendingRoot of [...pendingRoots]) {
      if (!root.contains?.(pendingRoot)) continue;
      mergedContinuation = mergeContinuations(
        mergedContinuation,
        pendingRootStates.get(pendingRoot) || createContinuation(pendingRoot),
      );
      pendingRoots.delete(pendingRoot);
      pendingRootStates.delete(pendingRoot);
    }
    // Keep the bounded set local. Under a long streaming burst, retain the
    // newest roots so a warning mounted later still gets scanned.
    if (pendingRoots.size >= pendingRootLimit) {
      const evicted = pendingRoots.values().next().value;
      pendingRoots.delete(evicted);
      pendingRootStates.delete(evicted);
    }
    pendingRoots.add(root);
    mergedContinuation = mergeContinuations(mergedContinuation, continuation);
    if (hasContinuation(mergedContinuation)) {
      pendingRootStates.set(root, mergedContinuation);
    }
  };

  scheduleScan = () => {
    if (!enabled || scanTimer || !pendingRoots.size) return;
    scanTimer = window.setTimeout(() => {
      scanTimer = 0;
      const roots = [...pendingRoots];
      pendingRoots.clear();
      const remaining = { value: batchElementLimit };
      const skippedRoots = [];
      while (roots.length && remaining.value > 0) {
        const root = roots.shift();
        const state = pendingRootStates.get(root);
        pendingRootStates.delete(root);
        if (root.isConnected !== false) {
          const continuation = state || createContinuation(root);
          const traversal = controlsWithin(root, {
            continuation,
            remaining,
          });
          if (traversal.controls.length) {
            dismissWarnings(root, traversal.controls);
          }
          if (hasContinuation(traversal.continuation)) {
            pendingRootStates.set(root, traversal.continuation);
            skippedRoots.push(root);
          }
        }
      }
      skippedRoots.push(...roots);
      skippedRoots.forEach((root) => {
        addPendingRoot(root, pendingRootStates.get(root));
      });
      scheduleScan();
    }, 40);
  };

  const ensureObserver = () => {
    if (
      unsubscribeMutations
      || !window.__codeyMutationDispatcher
      || !document.documentElement
    ) {
      return;
    }
    unsubscribeMutations = window.__codeyMutationDispatcher.subscribe((mutations) => {
      if (!enabled) return;
      const batchRoots = new Map();
      const addBatchRoot = (root, immediate) => {
        if (!(root instanceof Element)) return;
        for (const [candidate, candidateImmediate] of batchRoots) {
          if (candidate === root || candidate.contains?.(root)) {
            if (!immediate && candidateImmediate) batchRoots.set(candidate, false);
            return;
          }
        }
        for (const candidate of [...batchRoots.keys()]) {
          if (root.contains?.(candidate)) batchRoots.delete(candidate);
        }
        batchRoots.set(root, immediate);
      };
      for (const mutation of mutations) {
        if (!(mutation.addedNodes?.length > 0)) continue;
        // The warning label often arrives as a text node inside an existing
        // button, so scan that button instead of its arbitrary parent subtree.
        const target = mutation.target instanceof Element
          ? mutation.target
          : mutation.target?.parentElement;
        if (shouldSkipMountedTurnMutation(mutation, target)) continue;
        const targetSurface = target?.closest?.(warningSurfaceSelector);
        const targetPortal = target?.closest?.("[data-radix-portal], [data-portal]");
        if (
          targetSurface
          && targetPortal
          && (
            matchesAny(normalizedText(targetSurface), titlePatterns)
            || hasWarningControl(targetSurface)
          )
        ) {
          addBatchRoot(targetSurface, false);
        }
        if (target) {
          const scanRoot = target.closest?.("button, [role=button]");
          if (scanRoot) addBatchRoot(scanRoot, true);
          const warningSurface = target.closest?.(warningSurfaceSelector);
          if (warningSurface) addBatchRoot(warningSurface, false);
        }
        for (const node of mutation.addedNodes) {
          if (!(node instanceof Element)) continue;
          // Check a direct control now. A wrapper can contain a large streaming
          // subtree, so defer that traversal to the next short batch.
          addBatchRoot(node, node.matches?.("button, [role=button]") === true);
        }
      }
      const remaining = { value: batchElementLimit };
      for (const [root, immediate] of batchRoots) {
        if (!immediate) {
          addPendingRoot(root);
          continue;
        }
        if (root.isConnected === false) continue;
        if (remaining.value <= 0) {
          addPendingRoot(root);
          continue;
        }
        const traversal = controlsWithin(root, { remaining });
        // Unrelated streaming subtrees usually contain no action controls.
        // Drop them here so text updates never queue another button walk.
        if (!traversal.controls.length && !hasContinuation(traversal.continuation)) {
          continue;
        }
        dismissWarnings(root, traversal.controls);
        if (hasContinuation(traversal.continuation)) {
          pendingRootStates.set(root, traversal.continuation);
          addPendingRoot(root);
        }
      }
      scheduleScan();
    }, { childList: true });
  };

  window.addEventListener?.(configEventName, (event) => {
    const config = event?.detail?.config || event?.detail;
    if (config && typeof config.hideFullAccessWarning === "boolean") {
      setEnabled(config.hideFullAccessWarning);
    } else {
      void refreshConfig();
    }
  });
  window.__codeySecurityWarningShield = {
    get enabled() {
      return enabled;
    },
    dismissWarnings,
    refreshConfig,
    setEnabled,
    snapshot: () => Object.freeze({
      batchElementLimit,
      childExpansions,
      pendingRoots: pendingRoots.size,
      scanCount,
      scannedElements,
    }),
  };

  void refreshConfig();
})();
