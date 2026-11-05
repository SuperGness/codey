// Adds an accessible close control to the native Codex and Work quota notice.
(() => {
  if (window.__codeyQuotaNoticeDismissInstalled) return;
  window.__codeyQuotaNoticeDismissInstalled = true;

  const styleId = "codey-quota-notice-dismiss-style";
  const noticeAttribute = "data-codey-quota-notice";
  const dismissedAttribute = "data-codey-quota-notice-dismissed";
  const closeAttribute = "data-codey-quota-notice-close";
  const controlSelector = "button, [role=button], a[href]";
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
  const scanningRootLimit = 24;
  const batchElementLimit = 160;
  const scanDelayMs = 40;
  const controlLabels = [
    /^(?:reset\s+usage(?:\s+limit)?|reset\s+(?:my\s+)?(?:rate\s+limit|limit)|reset\s+limit)$/i,
    /^(?:重新?置使用量|使用一次(?:速率限制)?重置(?:机会)?|重置(?:速率限制|使用量))$/,
  ];
  const titlePatterns = [
    /codex\s*(?:和|与|及|and|&)\s*(?:work|工作(?:区)?)[\s\S]{0,80}(?:使用\s*)?额度[\s\S]{0,40}(?:用完|耗尽|用尽|上限|达到限制|exhausted|reached|used\s+up)/i,
    /codex\s*(?:和|与|及|and|&)\s*(?:work|工作(?:区)?)[\s\S]{0,80}(?:usage\s+limits?|limits?|quota)[\s\S]{0,40}(?:exhausted|reached|used\s+up)/i,
  ];
  const resetSupportPatterns = [
    /(?:速率限制|使用额度|额度|配额)[\s\S]{0,80}(?:重置|恢复)/,
    /(?:重置使用量|使用一次[\s\S]{0,30}重置机会)/,
    /(?:rate\s+limit|usage\s+limit|quota)[\s\S]{0,100}(?:reset|renew)/i,
    /(?:reset\s+usage|rate\s+limit\s+reset|reset\s+opportunity)/i,
  ];
  const pendingRoots = new Set();
  const pendingRootStates = new WeakMap();
  const dismissedNotices = new Set();
  let unsubscribeMutations = null;
  let scanTimer = 0;
  let disposed = false;
  let scanCount = 0;
  let scannedElements = 0;
  let childExpansions = 0;

  const normalizedValue = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const normalizedText = (element) => normalizedValue(element?.textContent);
  const matchesAny = (value, patterns) => patterns.some((pattern) => pattern.test(value));
  const directText = (element) => {
    if (!element) return "";
    if (element.childNodes) {
      return normalizedValue([...element.childNodes]
        .filter((node) => node.nodeType === 3 || node.nodeType === 4)
        .map((node) => node.textContent || "")
        .join(" "));
    }
    return element.children?.length ? "" : normalizedValue(element.textContent);
  };

  const controlLabel = (control) => normalizedValue([
    control?.getAttribute?.("aria-label"),
    control?.getAttribute?.("title"),
    control?.textContent,
  ].filter(Boolean).join(" "));

  const isResetControl = (control) => (
    control instanceof Element
    && control.matches?.(controlSelector)
    && control.getAttribute?.(closeAttribute) !== "true"
    && matchesAny(controlLabel(control), controlLabels)
  );

  const shouldSkipMountedTurnMutation = (mutation, target, isNotice) => {
    if (
      (mutation.type !== "childList" && mutation.type !== "characterData")
      || !target?.closest?.(conversationTurnSelector)
    ) {
      return false;
    }
    const targetSurface = target.closest?.(warningSurfaceSelector);
    const targetPortal = target.closest?.("[data-radix-portal], [data-portal]");
    if (targetSurface && targetSurface !== targetPortal) {
      return false;
    }
    if (targetPortal && isNotice(targetPortal)) return false;
    return ![...(mutation.addedNodes || [])].some((node) => (
      node instanceof Element
      && (
        node.matches?.(warningSurfaceSelector)
        || [...(node.children || [])].some((child) => (
          child.matches?.(warningSurfaceSelector)
          || child.querySelector?.(warningSurfaceSelector)
        ))
      )
    ));
  };

  const hasResetControl = (element) => (
    typeof element?.querySelectorAll === "function"
    && [...element.querySelectorAll(controlSelector)].some(isResetControl)
  );

  const isQuotaNotice = (element) => {
    if (!(element instanceof Element)) return false;
    if (element === document.body || element === document.documentElement) return false;
    const text = normalizedText(element);
    if (!matchesAny(text, titlePatterns)) return false;
    return matchesAny(text, resetSupportPatterns) || hasResetControl(element);
  };

  const createNoticeDetector = () => {
    const resetControlCache = new WeakMap();
    const textCache = new WeakMap();
    return (element) => {
      if (!(element instanceof Element)) return false;
      if (element === document.body || element === document.documentElement) return false;
      let text = textCache.get(element);
      if (text === undefined) {
        text = normalizedText(element);
        textCache.set(element, text);
      }
      if (!matchesAny(text, titlePatterns)) return false;
      if (matchesAny(text, resetSupportPatterns)) return true;
      let hasReset = resetControlCache.get(element);
      if (hasReset === undefined) {
        hasReset = hasResetControl(element);
        resetControlCache.set(element, hasReset);
      }
      return hasReset;
    };
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

  const anchorsWithin = (root, { continuation = null, remaining = null } = {}) => {
    const titleAnchors = [];
    const resetControls = [];
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
        visited += 1;
        if (matchesAny(directText(element), titlePatterns)) titleAnchors.push(element);
        if (isResetControl(element)) resetControls.push(element);
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
    scannedElements += visited;
    return {
      continuation: {
        segments: state.segments.filter((segment) => segment.length > 0),
      },
      resetControls,
      titleAnchors,
    };
  };

  const nearestNotice = (start, isNotice = isQuotaNotice) => {
    let candidate = start instanceof Element ? start : start?.parentElement;
    for (let depth = 0; candidate && depth < 12; depth += 1) {
      if (candidate === document.body || candidate === document.documentElement) break;
      if (isNotice(candidate)) return candidate;
      candidate = candidate.parentElement;
    }
    return null;
  };

  const innermostNotices = (notices) => {
    const candidates = [...new Set(notices)].filter(Boolean);
    return candidates.filter((candidate) => !candidates.some((other) => (
      other !== candidate && candidate.contains?.(other)
    )));
  };

  const resolveNotice = (start) => {
    const isNotice = createNoticeDetector();
    const notice = nearestNotice(start, isNotice);
    if (!notice) return null;
    const {
      resetControls,
      titleAnchors,
    } = anchorsWithin(notice);
    const nested = innermostNotices(
      [...titleAnchors, ...resetControls]
        .map((anchor) => nearestNotice(anchor, isNotice))
        .filter(Boolean),
    );
    return completeNotice(nested[0] || notice, isNotice);
  };

  // 标题和说明文字组成的子块会单独命中通知规则，真正的重置按钮在兄弟
  // 容器里。把关闭按钮锚到最近一个带重置控件的祖先，而不是文字块的角落。
  const completeNotice = (resolved, isNotice) => {
    if (!resolved || hasResetControl(resolved)) return resolved;
    return nearestNotice(resolved.parentElement, (candidate) => (
      isNotice(candidate) && hasResetControl(candidate)
    )) || resolved;
  };

  // 圆角外框常是通知外面一层没有自身内容的纯包裹；只隐藏内层会留下空壳，
  // 继续向上找真正画出提示框的最外层节点。
  const surfaceStopSelector = [
    "main",
    "[role='main']",
    "form",
    "nav",
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    "textarea",
    "[contenteditable='true']",
    "[contenteditable='plaintext-only']",
    "[role='textbox']",
    conversationTurnSelector,
  ].join(", ");
  const ignorableShellSibling = (element) => (
    element instanceof Element
    && (
      element.getAttribute?.(closeAttribute) === "true"
      || ["SCRIPT", "STYLE", "TEMPLATE", "NOSCRIPT", "LINK", "META", "BR", "HR"]
        .includes(element.tagName)
      || (
        ["DIV", "SPAN", "SECTION", "ASIDE", "P"].includes(element.tagName)
        && element.children.length === 0
        && !directText(element)
      )
    )
  );
  // 已定位的浮层本身就是可见表面，停在它身上，不并入外面的纯包裹。
  const positionedSurfaceSelector = [
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    "[data-radix-portal]",
    "[data-portal]",
  ].join(", ");
  const noticeSurface = (resolved) => {
    let surface = resolved;
    for (let depth = 0; depth < 12; depth += 1) {
      if (surface?.matches?.(positionedSurfaceSelector)) break;
      const parent = surface?.parentElement;
      if (!(parent instanceof Element)
        || parent === document.body
        || parent === document.documentElement
        || parent.matches?.(surfaceStopSelector)
        || directText(parent)) break;
      if ([...(parent.children || [])].some((child) => (
        child !== surface && !ignorableShellSibling(child)
      ))) break;
      surface = parent;
    }
    return surface || resolved;
  };

  const addStyle = () => {
    if (document.getElementById?.(styleId)) return;
    const parent = document.head || document.documentElement;
    if (!parent || typeof document.createElement !== "function") return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      button[${closeAttribute}="true"] {
        all: unset !important;
        position: absolute !important;
        z-index: 6 !important;
        inset-block-start: -8px !important;
        inset-inline-end: -8px !important;
        display: grid !important;
        place-items: center !important;
        box-sizing: border-box !important;
        width: 30px !important;
        height: 30px !important;
        padding: 0 !important;
        border: 1px solid light-dark(rgba(0, 0, 0, .10), rgba(255, 255, 255, .16)) !important;
        border-radius: 999px !important;
        background: light-dark(#ffffff, #2d2f33) !important;
        color: light-dark(#55575c, #ececef) !important;
        box-shadow: 0 2px 8px light-dark(rgba(27, 31, 38, .12), rgba(0, 0, 0, .34)) !important;
        color-scheme: light dark !important;
        cursor: pointer !important;
        font: 500 21px/1 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif !important;
        pointer-events: auto !important;
        touch-action: manipulation !important;
        user-select: none !important;
        transition: background-color .14s ease, border-color .14s ease, transform .14s ease !important;
        -webkit-app-region: no-drag !important;
      }
      button[${closeAttribute}="true"]:hover {
        background: light-dark(#f2f3f5, #3a3d42) !important;
        border-color: light-dark(rgba(0, 0, 0, .17), rgba(255, 255, 255, .24)) !important;
      }
      button[${closeAttribute}="true"]:active {
        transform: scale(.94) !important;
      }
      button[${closeAttribute}="true"]:focus-visible {
        outline: 2px solid light-dark(#0a84ff, #69b1ff) !important;
        outline-offset: 2px !important;
      }
      @media (max-width: 520px) {
        button[${closeAttribute}="true"] {
          inset-block-start: -7px !important;
          inset-inline-end: -7px !important;
          width: 28px !important;
          height: 28px !important;
          font-size: 19px !important;
        }
      }
    `;
    parent.appendChild(style);
  };

  const hideNotice = (notice) => {
    if (!notice) return false;
    const surface = noticeSurface(notice);
    dismissedNotices.add(notice);
    dismissedNotices.add(surface);
    surface.querySelector?.(`[${closeAttribute}="true"]`)?.remove?.();
    notice.setAttribute?.(noticeAttribute, "true");
    notice.setAttribute?.(dismissedAttribute, "true");
    surface.setAttribute?.(dismissedAttribute, "true");
    surface.setAttribute?.("aria-hidden", "true");
    surface.style?.setProperty?.("display", "none", "important");
    return true;
  };

  const dismiss = (notice) => {
    const resolved = resolveNotice(notice);
    return resolved ? hideNotice(resolved) : false;
  };

  const ensureCloseButton = (notice) => {
    const surface = noticeSurface(notice);
    if (dismissedNotices.has(notice) || dismissedNotices.has(surface)) {
      hideNotice(notice);
      return null;
    }
    const existing = surface.querySelector?.(`[${closeAttribute}="true"]`);
    if (existing) return existing;

    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute("type", "button");
    button.setAttribute(closeAttribute, "true");
    button.setAttribute("aria-label", "关闭使用额度提示");
    button.setAttribute("title", "关闭使用额度提示");
    button.textContent = "\u00d7";
    button.addEventListener("click", () => {
      hideNotice(notice);
    });
    notice.setAttribute?.(noticeAttribute, "true");
    const computedPosition = window.getComputedStyle?.(surface)?.position;
    if (!computedPosition || computedPosition === "static") {
      surface.style?.setProperty?.("position", "relative", "important");
    }
    surface.insertBefore?.(button, surface.firstChild || null);
    return button;
  };

  const scanRoot = (root, {
    continuation = null,
    detector = createNoticeDetector(),
    remaining = null,
  } = {}) => {
    if (disposed || !root) return { continuation: { segments: [] }, found: 0 };
    scanCount += 1;
    addStyle();
    const {
      continuation: nextContinuation,
      resetControls,
      titleAnchors,
    } = anchorsWithin(root, { continuation, remaining });
    const starts = [
      ...titleAnchors,
      ...resetControls,
    ];
    const matches = innermostNotices(
      starts.map((start) => nearestNotice(start, detector)).filter(Boolean),
    );
    let found = 0;
    for (const notice of matches) {
      const resolved = completeNotice(notice, detector);
      if (resolved.isConnected === false) continue;
      if (dismissedNotices.has(resolved)) {
        hideNotice(resolved);
        continue;
      }
      if (ensureCloseButton(resolved)) found += 1;
    }
    return {
      continuation: nextContinuation,
      found,
    };
  };

  const scan = (root = document, { bounded = false } = {}) => {
    if (disposed || !root) return 0;
    return scanRoot(root, {
      remaining: bounded ? { value: batchElementLimit } : null,
    }).found;
  };

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

  const addPendingRoot = (root) => {
    if (!(root instanceof Element) || root.isConnected === false) return;
    for (const pending of pendingRoots) {
      if (pending === root || pending.contains?.(root)) {
        // A nested change under a partially scanned root may sit after the
        // ancestor's saved traversal position. Add that child as a resumed
        // root instead of dropping the mutation or restarting from the top.
        const continuation = pendingRootStates.get(pending);
        if (hasContinuation(continuation)) {
          pendingRootStates.set(
            pending,
            mergeContinuations(
              continuation,
              createContinuation(root, continuation),
            ),
          );
        }
        return;
      }
    }
    let mergedContinuation = pendingRootStates.get(root) || pendingRootStates.get(
      [...pendingRoots].find((pending) => pending.contains?.(root)),
    ) || { segments: [] };
    for (const pending of [...pendingRoots]) {
      if (root.contains?.(pending)) {
        mergedContinuation = mergeContinuations(
          mergedContinuation,
          pendingRootStates.get(pending) || createContinuation(pending),
        );
        pendingRoots.delete(pending);
        pendingRootStates.delete(pending);
      }
    }
    if (pendingRoots.size >= scanningRootLimit) {
      // 新版节点仍应优先于旧节点。绝不在这里退化为整页扫描，流式输出会因此
      // 反复构造页面文本并阻塞主线程。
      const evicted = pendingRoots.values().next().value;
      pendingRoots.delete(evicted);
      pendingRootStates.delete(evicted);
    }
    pendingRoots.add(root);
    if (hasContinuation(mergedContinuation)) {
      pendingRootStates.set(root, mergedContinuation);
    }
  };

  const scheduleScan = () => {
    if (disposed || scanTimer) return;
    scanTimer = window.setTimeout(() => {
      scanTimer = 0;
      const roots = [...pendingRoots];
      pendingRoots.clear();
      const detector = createNoticeDetector();
      const remaining = { value: batchElementLimit };
      for (let index = 0; index < roots.length; index += 1) {
        const root = roots[index];
        if (root.isConnected === false) continue;
        if (remaining.value <= 0) {
          roots.slice(index).forEach(addPendingRoot);
          break;
        }
        const { continuation } = scanRoot(root, {
          continuation: pendingRootStates.get(root),
          detector,
          remaining,
        });
        if (hasContinuation(continuation)) {
          pendingRootStates.set(root, continuation);
          roots.slice(index + 1).forEach(addPendingRoot);
          addPendingRoot(root);
          break;
        }
        pendingRootStates.delete(root);
      }
    }, scanDelayMs);
  };

  const pruneRemovedNotices = (removedNodes) => {
    if (!dismissedNotices.size || !removedNodes?.length) return;
    for (const notice of [...dismissedNotices]) {
      if (
        notice.isConnected === false
        || [...removedNodes].some((node) => node === notice || node?.contains?.(notice))
      ) {
        dismissedNotices.delete(notice);
      }
    }
  };

  const handleMutations = (mutations) => {
    if (disposed) return;
    let changed = false;
    const detector = createNoticeDetector();
    for (const mutation of mutations) {
      pruneRemovedNotices(mutation.removedNodes);
      const target = mutation.target instanceof Element
        ? mutation.target
        : mutation.target?.parentElement;
      if (shouldSkipMountedTurnMutation(mutation, target, detector)) continue;
      if (mutation.type === "attributes" && target) {
        addPendingRoot(target);
        changed = true;
      }
      let addedText = false;
      for (const node of mutation.addedNodes || []) {
        if (node instanceof Element) {
          addPendingRoot(node);
          changed = true;
        } else if (node?.nodeType === 3 || node?.nodeType === 4) {
          addedText = true;
        }
      }
      if (addedText && target) {
        addPendingRoot(target);
        changed = true;
      }
    }
    if (changed) scheduleScan();
  };

  const ensureObserver = () => {
    if (
      disposed
      || unsubscribeMutations
      || typeof window.__codeyMutationDispatcher?.subscribe !== "function"
      || !document.documentElement
    ) return;
    unsubscribeMutations = window.__codeyMutationDispatcher.subscribe(
      handleMutations,
      {
        attributes: true,
        attributeFilter: ["aria-label", "title"],
        childList: true,
      },
    );
  };

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    if (scanTimer) window.clearTimeout?.(scanTimer);
    scanTimer = 0;
    pendingRoots.clear();
    dismissedNotices.clear();
    unsubscribeMutations?.();
    unsubscribeMutations = null;
  };

  window.__codeyQuotaNoticeDismiss = Object.freeze({
    dismiss,
    dispose,
    isQuotaNotice,
    scan,
    snapshot: () => Object.freeze({
      dismissedNotices: dismissedNotices.size,
      batchElementLimit,
      childExpansions,
      observerInstalled: Boolean(unsubscribeMutations),
      pendingRoots: pendingRoots.size,
      scanCount,
      scannedElements,
    }),
  });

  addStyle();
  ensureObserver();
  addPendingRoot(document.documentElement);
  scheduleScan();
})();
