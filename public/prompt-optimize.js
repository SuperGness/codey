// Prompt-optimization button injected next to the Codex composer model picker.
// The button joins the composer's native action row so it follows normal
// responsive layout instead of floating above the input. The enabled flag is
// read from /settings/get at runtime, so the console switch applies without a
// Codex restart. All API traffic goes through the Codey bridge and never
// carries the configured API key into this page.
(() => {
  const moduleLoaded = window.__codeyPromptOptimizeModuleLoaded === true;
  window.__codeyPromptOptimizeModuleLoaded = true;
  if (moduleLoaded && window.__codeyPromptOptimize) {
    return;
  }

  const settingsPath = "/settings/get";
  const optimizePath = "/api/optimize_prompt";
  const buttonId = "codey-prompt-optimize-button";
  const styleId = "codey-prompt-optimize-style";
  const toastId = "codey-runtime-toast";
  const configChangedEvent = "codey:config-changed";
  const injectionStatusId = "prompt-optimize";
  const injectionStatusChangedEvent = "codey-injection-status-changed";
  const optimizeTimeoutMs = 75_000;
  const pendingOptimizationLimit = 20;
  const scanDelayMs = 250;
  const repositionDelayMs = 100;
  const composerAnchorSelector = "[data-above-composer-conversation-id]";
  const composerCandidateSelector =
    "textarea, [contenteditable='true'], [role='textbox']";
  const composerFallbackSelector =
    "main textarea, main [contenteditable='true'], main [role='textbox'], textarea, [contenteditable='true'][role='textbox']";
  const composerControlSelector = "button, [role='button']";
  const ignoredComposerContainerSelector =
    "dialog, [role='dialog'], [aria-modal='true']";
  const ignoredControlContainerSelector =
    `${ignoredComposerContainerSelector}, [role='menu'], [role='listbox'], ` +
    "[cmdk-list], [data-radix-popper-content-wrapper]";

  let enabled = false;
  let ready = false;
  let inputElement = null;
  let button = null;
  let busy = false;

  const publishInjectionStatus = () => {
    if (!ready) return;
    const entry = window.__codeyInjectionStatus?.[injectionStatusId];
    if (!entry || entry.status === "pending") return;
    const status = enabled ? "effective" : "inactive";
    const detail = enabled ? "提示词优化按钮已就绪" : "提示词优化已关闭";
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
  let scanTimer = 0;
  let repositionTimer = 0;
  let configLoadTimer = 0;
  let configLoadBackoffMs = 120;
  let configLoadAttempts = 0;
  let observer = null;
  let observerActive = false;
  let unsubscribeMutations = null;
  const pendingOptimizations = new Map();

  const MAX_CONFIG_LOAD_ATTEMPTS = 10;

  const callBridge = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload);
    }
    return Promise.reject(new Error("Codey bridge 尚未就绪"));
  };

  const withTimeout = (promise, ms, message) => {
    let timer = 0;
    const timeout = new Promise((resolve) => {
      timer = setTimeout(() => resolve({ status: "failed", message }), ms);
    });
    return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
  };

  const addStyle = () => {
    if (document.getElementById(styleId)) return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      #${buttonId} {
        -webkit-app-region: no-drag !important;
        pointer-events: auto !important;
        position: relative !important;
        z-index: 1 !important;
        display: none;
        flex: 0 0 auto;
        align-items: center;
        gap: 4px;
        box-sizing: border-box;
        min-height: 26px !important;
        height: 26px !important;
        margin: 0 6px 0 0;
        padding: 0 8px;
        border: 0;
        border-radius: 999px;
        background: rgba(30, 30, 30, .92);
        color: #f5f5f5;
        font: 12px/1 system-ui, -apple-system, "Segoe UI", sans-serif;
        cursor: pointer;
        user-select: none;
        box-shadow: 0 1px 4px rgba(0, 0, 0, .35);
        opacity: .88;
        transition: opacity .15s ease, transform .15s ease;
      }
      #${buttonId}:hover { opacity: 1; }
      #${buttonId}:active { transform: translateY(1px); }
      #${buttonId}:disabled { cursor: not-allowed; box-shadow: none; opacity: .42; }
      #${buttonId}[data-busy="true"] { cursor: wait; opacity: .7; }
      #${buttonId} svg { flex: 0 0 auto; width: 12px; height: 12px; }
      #${buttonId} [data-codey-optimize-spinner] { display: none; animation: codey-prompt-optimize-spin .75s linear infinite; }
      #${buttonId}[data-busy="true"] [data-codey-optimize-icon] { display: none; }
      #${buttonId}[data-busy="true"] [data-codey-optimize-spinner] { display: block; }
      @keyframes codey-prompt-optimize-spin { to { transform: rotate(360deg); } }
      #${toastId} { -webkit-app-region: no-drag !important; position: fixed; right: 20px; bottom: 22px; z-index: 2147483645; max-width: 360px; border: 1px solid rgba(124, 140, 255, .4); border-radius: 11px; padding: 10px 13px; background: rgba(20, 24, 36, .97); color: #eef2ff; box-shadow: 0 12px 36px rgba(0,0,0,.4); font: 12px/1.45 system-ui, sans-serif; }
      #${toastId}[data-tone="error"] { border-color: rgba(248, 113, 113, .6); color: #fecaca; }
    `;
    document.documentElement.appendChild(style);
  };

  const createButton = () => {
    const element = document.createElement("button");
    element.id = buttonId;
    element.type = "button";
    element.dataset.codeyPromptOptimize = "true";
    element.setAttribute("contenteditable", "false");
    element.setAttribute("aria-label", "优化提示词");
    element.setAttribute("aria-disabled", "true");
    element.setAttribute("aria-busy", "false");
    element.disabled = true;
    element.innerHTML = `
      <svg data-codey-optimize-icon viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none"
        stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z"></path>
        <path d="M19 15l.9 2.4L22 18l-2.1.9L19 21l-.9-2.1L16 18l2.1-.6z"></path>
      </svg>
      <svg data-codey-optimize-spinner viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none"
        stroke="currentColor" stroke-width="2.5" stroke-linecap="round">
        <path d="M20 12a8 8 0 1 1-5.3-7.5"></path>
      </svg>
      <span>优化</span>
    `;
    element.addEventListener("click", handleClick, true);
    return element;
  };

  const installRuntimeToast = () => {
    if (typeof window.__codeyShowRuntimeToast === "function") return;
    window.__codeyShowRuntimeToast = (message, tone = "success") => {
      document.getElementById(toastId)?.remove();
      const toast = document.createElement("div");
      toast.id = toastId;
      toast.dataset.tone = tone;
      toast.setAttribute("role", tone === "error" ? "alert" : "status");
      toast.setAttribute(
        "aria-live",
        tone === "error" ? "assertive" : "polite",
      );
      toast.textContent = message;
      document.documentElement.appendChild(toast);
      setTimeout(() => toast.remove(), tone === "error" ? 8_000 : 3_500);
    };
  };

  const isComposerInput = (element) => {
    if (!element) return false;
    if (element.tagName === "TEXTAREA") return true;
    if (element.isContentEditable === true) return true;
    if (element.getAttribute?.("contenteditable") === "true") return true;
    return element.getAttribute?.("role") === "textbox";
  };

  const isVisible = (element) => {
    if (!isComposerInput(element)) return false;
    if (element.closest?.(ignoredComposerContainerSelector)) return false;
    if (element.closest?.("[hidden], [aria-hidden='true']")) return false;
    if (element.disabled || element.readOnly) return false;
    const style = window.getComputedStyle(element);
    if (style.display === "none" || style.visibility === "hidden") return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };

  const controlLooksLikeComposerAction = (control) =>
    /(^|[^a-z])(model|send|submit|attach|upload|microphone|mic|voice|full access)([^a-z]|$)|模型|发送|提交|附件|上传|语音|麦克风|完全访问/i.test(
      controlDescriptor(control),
    );

  const controlIsNearInput = (control, inputRect) => {
    const rect = control.getBoundingClientRect();
    if (rect.bottom <= inputRect.top) return false;
    const controlMiddle = rect.top + rect.height / 2;
    const inputMiddle = inputRect.top + inputRect.height / 2;
    return Math.abs(controlMiddle - inputMiddle) <= Math.max(96, inputRect.height);
  };

  const scopeHasComposerActions = (scope, inputRect) =>
    [...(scope?.querySelectorAll?.(composerControlSelector) || [])].some(
      (control) =>
        isVisibleControl(control) &&
        controlIsNearInput(control, inputRect) &&
        controlLooksLikeComposerAction(control),
    );

  const scopeHasVisibleControls = (scope, inputRect) =>
    [...(scope?.querySelectorAll?.(composerControlSelector) || [])].some(
      (control) =>
        isVisibleControl(control) && controlIsNearInput(control, inputRect),
    );

  const hasComposerActionContext = (element) => {
    if (!element?.parentElement) return false;
    const inputRect = element.getBoundingClientRect();
    let scope = element.parentElement;
    let depth = 0;
    while (scope && depth < 6) {
      if (scopeHasComposerActions(scope, inputRect)) return true;
      if (scopeHasVisibleControls(scope, inputRect)) return false;
      scope = scope.parentElement;
      depth += 1;
    }
    return false;
  };

  const findComposerInput = () => {
    const seen = new Set();
    for (const anchor of document.querySelectorAll(composerAnchorSelector)) {
      if (seen.has(anchor)) continue;
      seen.add(anchor);
      const scope = anchor.parentElement || anchor;
      const candidates = [...scope.querySelectorAll(composerCandidateSelector)];
      for (const candidate of candidates) {
        if (isVisible(candidate)) return candidate;
      }
    }
    // New conversations do not have a conversation-id anchor yet. Prefer the
    // lowest visible editable textbox, then its area, so the composer wins over
    // search fields and historical message editors.
    let best = null;
    let bestScore = -1;
    const viewportHeight =
      window.innerHeight || document.documentElement.clientHeight || 0;
    for (const candidate of document.querySelectorAll(
      composerFallbackSelector,
    )) {
      if (!isVisible(candidate)) continue;
      if (!hasComposerActionContext(candidate)) continue;
      const rect = candidate.getBoundingClientRect();
      if (
        viewportHeight > 0 &&
        (rect.bottom <= 0 || rect.top >= viewportHeight)
      )
        continue;
      const area = rect.width * rect.height;
      const score =
        Math.max(0, rect.bottom) * 10_000 + Math.min(area, 9_999_999);
      if (score > bestScore) {
        best = candidate;
        bestScore = score;
      }
    }
    return best;
  };

  const controlDescriptor = (element) =>
    [
      element?.getAttribute?.("aria-label"),
      element?.getAttribute?.("title"),
      element?.getAttribute?.("data-testid"),
      element?.textContent,
      element?.innerText,
    ]
      .filter((value) => typeof value === "string" && value.trim())
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();

  const isVisibleControl = (element) => {
    if (!element || element === button) return false;
    if (element.closest?.(ignoredControlContainerSelector)) return false;
    if (element.closest?.("[hidden], [aria-hidden='true']")) return false;
    if (element.disabled) return false;
    const style = window.getComputedStyle(element);
    if (style.display === "none" || style.visibility === "hidden") return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };

  const modelControlScore = (control, inputRect) => {
    const rect = control.getBoundingClientRect();
    if (rect.bottom <= inputRect.top) return Number.NEGATIVE_INFINITY;
    if (!controlIsNearInput(control, inputRect)) return Number.NEGATIVE_INFINITY;
    const descriptor = controlDescriptor(control);
    const visibleText = [control.textContent, control.innerText]
      .filter((value) => typeof value === "string" && value.trim())
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();
    const hasModelHint = /(^|[^a-z])model([^a-z]|$)|模型/i.test(descriptor);
    const hasModelValueHint =
      /(^|[^a-z])(gpt|codex|claude|gemini|grok|llama|qwen|deepseek|mistral|sonnet|opus|haiku|mini|sol|low|medium|high|xhigh|auto)([^a-z]|$)|\bo\d+\b|\d+(?:\.\d+)?|低|中|高|极高|自动/i.test(
        visibleText,
      );
    if (!hasModelHint && !hasModelValueHint) {
      return Number.NEGATIVE_INFINITY;
    }
    if (
      !hasModelHint &&
      /完全访问|full access|附件|attach|上传|upload|优化/i.test(descriptor)
    ) {
      return Number.NEGATIVE_INFINITY;
    }
    if (
      !hasModelHint &&
      inputRect.width > 0 &&
      rect.right < inputRect.left + inputRect.width * 0.45
    ) {
      return Number.NEGATIVE_INFINITY;
    }
    return (
      (hasModelHint ? 1_000_000 : 0) +
      (control.getAttribute?.("aria-haspopup") ? 100_000 : 0) +
      Math.max(0, rect.right) * 10 +
      Math.min(rect.width, 500)
    );
  };

  const findModelInsertionTarget = () => {
    if (!inputElement?.parentElement) return null;
    const inputRect = inputElement.getBoundingClientRect();
    const seen = new Set();
    let bestControl = null;
    let bestScore = Number.NEGATIVE_INFINITY;
    let scope = inputElement.parentElement;
    let depth = 0;
    while (scope && depth < 8) {
      for (const control of scope.querySelectorAll?.(composerControlSelector) ||
        []) {
        if (inputElement.contains?.(control)) continue;
        if (seen.has(control) || !isVisibleControl(control)) continue;
        seen.add(control);
        const score = modelControlScore(control, inputRect);
        if (score > bestScore) {
          bestControl = control;
          bestScore = score;
        }
      }
      if (bestScore >= 1_000_000) break;
      scope = scope.parentElement;
      depth += 1;
    }
    if (!bestControl) return null;
    if (!hasComposerActionContext(inputElement)) return null;

    let anchor = bestControl;
    let host = bestControl.parentElement;
    while (host?.parentElement && host.children?.length === 1) {
      anchor = host;
      host = host.parentElement;
    }
    if (!host?.insertBefore) return null;
    return { anchor, host };
  };

  const isMountedBefore = (element, anchor, host) => {
    if (element?.parentElement !== host) return false;
    const children = [...(host.children || [])];
    return children.indexOf(element) + 1 === children.indexOf(anchor);
  };

  const nodeIsInsideInputElement = (node) =>
    Boolean(
      inputElement &&
        node &&
        (node === inputElement || inputElement.contains?.(node)),
    );

  const removeButtonFromEditableInput = () => {
    if (button && nodeIsInsideInputElement(button)) {
      button.remove();
    }
  };

  const updateButtonPosition = () => {
    if (!button || !inputElement) return;
    if (!isVisible(inputElement)) {
      inputElement = null;
      button.style.display = "none";
      scheduleScan();
      return;
    }
    const target = findModelInsertionTarget();
    if (!target) {
      removeButtonFromEditableInput();
      button.style.display = "none";
      return;
    }
    if (
      nodeIsInsideInputElement(target.host) ||
      nodeIsInsideInputElement(target.anchor)
    ) {
      removeButtonFromEditableInput();
      button.style.display = "none";
      return;
    }
    if (!isMountedBefore(button, target.anchor, target.host)) {
      target.host.insertBefore(button, target.anchor);
    }
    button.style.top = "";
    button.style.left = "";
    button.style.display = "inline-flex";
    button.dataset.codeyPromptOptimizeLayout = "model-picker";
    updateButtonState();
  };

  const readComposerText = (element = inputElement) => {
    if (!element) return "";
    if (element.tagName === "TEXTAREA") {
      return element.value;
    }
    return element.innerText || "";
  };

  const findComposerConversationId = (element) => {
    if (!element) return null;
    for (const anchor of document.querySelectorAll(composerAnchorSelector)) {
      const scope = anchor.parentElement || anchor;
      if (scope === element || scope.contains?.(element)) {
        const conversationId = anchor.getAttribute?.(
          "data-above-composer-conversation-id",
        );
        return typeof conversationId === "string" && conversationId
          ? conversationId
          : null;
      }
    }
    return null;
  };

  const currentLocationKey = () =>
    typeof window.location?.href === "string" ? window.location.href : "";

  const composerContextKey = (conversationId, locationKey) =>
    conversationId
      ? `conversation:${conversationId}`
      : `location:${locationKey}`;

  const captureComposerContext = (element) => {
    const conversationId = findComposerConversationId(element);
    const locationKey = currentLocationKey();
    return {
      element,
      conversationId,
      key: composerContextKey(conversationId, locationKey),
      locationKey,
      text: readComposerText(element),
    };
  };

  const elementMatchesComposerContext = (context, element) => {
    if (!element || element.isConnected === false) return false;
    const conversationId = findComposerConversationId(element);
    if (context.conversationId !== null || conversationId !== null) {
      return conversationId === context.conversationId;
    }
    return currentLocationKey() === context.locationKey;
  };

  const isCurrentComposerContext = (context) =>
    enabled &&
    inputElement === context.element &&
    elementMatchesComposerContext(context, context.element) &&
    isVisible(context.element) &&
    readComposerText(context.element) === context.text;

  const updateButtonState = () => {
    if (!button) return;
    const empty = !readComposerText().trim();
    const disabled = busy || empty;
    button.disabled = disabled;
    button.dataset.busy = String(busy);
    button.dataset.empty = String(empty);
    button.setAttribute("aria-busy", String(busy));
    button.setAttribute("aria-disabled", String(disabled));
    button.setAttribute(
      "aria-label",
      busy ? "正在优化提示词" : empty ? "请输入内容后优化" : "优化提示词",
    );
  };

  const showError = (message) => {
    window.__codeyShowRuntimeToast?.(message, "error");
  };

  const replaceComposerText = (text, element = inputElement) => {
    if (!element) return;
    if (element.tagName === "TEXTAREA") {
      const prototype = window.HTMLTextAreaElement?.prototype;
      const setter =
        prototype && Object.getOwnPropertyDescriptor(prototype, "value")?.set;
      if (setter) {
        setter.call(element, text);
      } else {
        element.value = text;
      }
      element.dispatchEvent(new Event("input", { bubbles: true }));
      return;
    }
    element.innerText = text;
    element.dispatchEvent(
      new InputEvent("input", {
        bubbles: true,
        inputType: "insertText",
        data: text,
      }),
    );
  };

  const rememberPendingOptimization = (context, optimized) => {
    pendingOptimizations.delete(context.key);
    pendingOptimizations.set(context.key, {
      optimized,
      text: context.text,
    });
    while (pendingOptimizations.size > pendingOptimizationLimit) {
      const oldestKey = pendingOptimizations.keys().next().value;
      pendingOptimizations.delete(oldestKey);
    }
  };

  const applyPendingOptimization = (element) => {
    if (!element) return false;
    const context = captureComposerContext(element);
    const pending = pendingOptimizations.get(context.key);
    if (!pending) return false;
    pendingOptimizations.delete(context.key);
    if (readComposerText(element) !== pending.text) return false;
    replaceComposerText(pending.optimized, element);
    return true;
  };

  const deliverOptimization = (context, optimized) => {
    if (elementMatchesComposerContext(context, context.element)) {
      if (readComposerText(context.element) === context.text) {
        replaceComposerText(optimized, context.element);
      }
      return;
    }
    rememberPendingOptimization(context, optimized);
    applyPendingOptimization(inputElement);
  };

  const handleClick = (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (busy) return;
    const context = captureComposerContext(inputElement);
    const text = context.text.trim();
    if (!text) {
      updateButtonState();
      return;
    }
    busy = true;
    updateButtonState();
    const bridgeCall = Promise.resolve().then(() => callBridge(optimizePath, { text }));
    const result = withTimeout(
      bridgeCall,
      optimizeTimeoutMs,
      "优化请求超时，请稍后重试",
    );
    result
      .then((value) => {
        if (value?.status === "failed") {
          throw new Error(value.message || "优化失败");
        }
        const optimized =
          typeof value?.optimized === "string" ? value.optimized : "";
        if (!optimized) {
          throw new Error("优化结果为空");
        }
        const shouldFocus = isCurrentComposerContext(context);
        deliverOptimization(context, optimized);
        if (shouldFocus && context.element?.focus) {
          context.element.focus();
        }
      })
      .catch((error) => {
        if (!isCurrentComposerContext(context)) return;
        const message =
          error instanceof Error ? error.message : String(error || "优化失败");
        showError(message);
      })
      .finally(() => {
        busy = false;
        updateButtonState();
      });
  };

  const refreshButton = () => {
    if (!enabled) {
      if (button) button.style.display = "none";
      return;
    }
    const input = findComposerInput();
    if (input) applyPendingOptimization(input);
    if (input === inputElement && input) {
      updateButtonPosition();
      return;
    }
    inputElement = input || null;
    if (!inputElement) {
      if (button) button.style.display = "none";
      return;
    }
    if (!button) {
      button = createButton();
    }
    updateButtonPosition();
  };

  const scheduleScan = () => {
    if (!enabled || scanTimer) return;
    scanTimer = setTimeout(() => {
      scanTimer = 0;
      refreshButton();
    }, scanDelayMs);
  };

  const scheduleReposition = () => {
    if (!enabled || !button || !inputElement) return;
    clearTimeout(repositionTimer);
    repositionTimer = setTimeout(updateButtonPosition, repositionDelayMs);
  };

  const nodeTouchesTrackedComposer = (node, includeAncestors = false) => {
    if (!node || typeof node !== "object") return false;
    const buttonHost = button?.parentElement;
    return Boolean(
      node === inputElement ||
        node === buttonHost ||
        node === inputElement?.parentElement ||
        node === buttonHost?.parentElement ||
        inputElement?.contains?.(node) ||
        buttonHost?.contains?.(node) ||
        (includeAncestors &&
          (node.contains?.(inputElement) || node.contains?.(buttonHost))),
    );
  };

  const nodeContainsComposerCandidate = (node) =>
    Boolean(
      isComposerInput(node) ||
        node?.querySelector?.(composerCandidateSelector) ||
        node?.querySelector?.(composerAnchorSelector),
    );

  const mutationRequiresComposerScan = (mutation) => {
    if (!inputElement?.isConnected) return true;
    if (nodeTouchesTrackedComposer(mutation.target)) return true;
    if (
      mutation.type === "attributes" &&
      mutation.attributeName === "data-above-composer-conversation-id"
    ) {
      return true;
    }
    if (mutation.type !== "childList") return false;
    return [...(mutation.addedNodes || []), ...(mutation.removedNodes || [])].some(
      (node) =>
        nodeTouchesTrackedComposer(node, true) ||
        nodeContainsComposerCandidate(node),
    );
  };

  const handleComposerMutations = (mutations) => {
    if (!enabled) return;
    const hasExternalMutation = mutations.some((mutation) => {
      const target = mutation.target;
      if (!target) return true;
      if (target === button || target.id === toastId) return false;
      if (target.id === styleId) return false;
      return (
        !target.closest?.(`#${buttonId}, #${toastId}`) &&
        mutationRequiresComposerScan(mutation)
      );
    });
    if (hasExternalMutation) scheduleScan();
  };

  const composerMutationOptions = {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: [
      "aria-hidden",
      "class",
      "contenteditable",
      "data-above-composer-conversation-id",
      "disabled",
      "hidden",
      "readonly",
      "role",
      "style",
    ],
  };

  const installObserver = () => {
    if (typeof window.__codeyMutationDispatcher?.subscribe === "function") return;
    observer = new MutationObserver(handleComposerMutations);
  };

  const observeComposerMutations = () => {
    if (observerActive || !enabled) return;
    const mutationDispatcher = window.__codeyMutationDispatcher;
    if (typeof mutationDispatcher?.subscribe === "function") {
      const unsubscribe = mutationDispatcher.subscribe(
        handleComposerMutations,
        composerMutationOptions,
      );
      if (mutationDispatcher.snapshot?.().observerInstalled) {
        unsubscribeMutations = unsubscribe;
        observerActive = true;
        return;
      }
      unsubscribe?.();
    }
    observer ||= new MutationObserver(handleComposerMutations);
    observer.observe(document.documentElement, composerMutationOptions);
    observerActive = true;
  };

  const disconnectComposerObserver = () => {
    if (!observerActive) return;
    unsubscribeMutations?.();
    unsubscribeMutations = null;
    observer?.disconnect();
    observerActive = false;
  };

  const applyEnabledState = (nextEnabled) => {
    enabled = nextEnabled;
    if (enabled) {
      observeComposerMutations();
      refreshButton();
      return;
    }

    disconnectComposerObserver();
    clearTimeout(scanTimer);
    clearTimeout(repositionTimer);
    scanTimer = 0;
    repositionTimer = 0;
    refreshButton();
  };

  const loadConfig = () => {
    configLoadAttempts += 1;
    callBridge(settingsPath, {})
      .then((config) => {
        configLoadAttempts = 0;
        configLoadBackoffMs = 120;
        try {
          const optimization = config?.promptOptimization;
          applyEnabledState(
            optimization?.enabled === true &&
              (optimization?.mode === "codeyRoute" ||
                optimization?.apiKeyConfigured === true),
          );
        } catch (error) {
          // A script-side error must not look like a missing bridge; report
          // it once and leave the switch in its last known state.
          if (
            typeof console !== "undefined" &&
            typeof console.error === "function"
          ) {
            console.error("Codey 提示词优化脚本异常：", error);
          }
        }
        ready = true;
        publishInjectionStatus();
      })
      .catch(() => {
        // The bridge may not be ready during early startup; retry with
        // bounded backoff so the switch still applies once it is.
        if (configLoadAttempts >= MAX_CONFIG_LOAD_ATTEMPTS) return;
        clearTimeout(configLoadTimer);
        configLoadTimer = setTimeout(loadConfig, configLoadBackoffMs);
        configLoadBackoffMs = Math.min(configLoadBackoffMs * 2, 2_000);
      });
  };

  window.addEventListener(configChangedEvent, () => {
    ready = false;
    loadConfig();
  });
  window.addEventListener("scroll", scheduleReposition, true);
  window.addEventListener("resize", scheduleReposition);
  window.addEventListener("hashchange", scheduleScan);
  window.addEventListener("popstate", scheduleScan);
  document.addEventListener(
    "input",
    (event) => {
      if (event.target === inputElement) updateButtonState();
    },
    true,
  );

  addStyle();
  installRuntimeToast();
  installObserver();
  loadConfig();

  window.__codeyPromptOptimize = {
    snapshot: () => ({
      ready: ready,
      enabled: enabled,
      hasInput: Boolean(inputElement && isVisible(inputElement)),
      hasButton: Boolean(button && button.style.display !== "none"),
      buttonBusy: Boolean(button && busy),
      buttonDisabled: Boolean(button?.disabled),
    }),
  };
})();
