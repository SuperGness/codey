// Workflow Engine V1 takeover for the native Codex composer.
//
// This script is deliberately fail-closed: it only owns a send after the
// backend has explicitly confirmed the V1 protocol, bridge/proxy health, cwd,
// and an immutable permission snapshot. Unsupported input is left to Codex.
(() => {
  const existingRuntime = window.__codeyWorkflowModeRuntime;
  if (existingRuntime?.version === 1) {
    existingRuntime.scanNow?.();
    return;
  }

  const runtime = { version: 1 };
  window.__codeyWorkflowModeRuntime = runtime;

  const protocolVersion = 1;
  const capabilitiesPath = "/api/workflow_capabilities";
  const listPath = "/api/workflow_list";
  const startPath = "/api/workflow_start";
  const steerPath = "/api/workflow_steer";
  const bypassAuditPath = "/api/workflow_bypass_audit";
  const composerAnchorSelector = "[data-above-composer-conversation-id]";
  const composerCandidateSelector =
    "textarea, [contenteditable='true'], [role='textbox']";
  const composerFallbackSelector =
    "main textarea, main [contenteditable='true'], main [role='textbox'], textarea, [contenteditable='true'][role='textbox']";
  const ignoredComposerSelector =
    "dialog, [role='dialog'], [aria-modal='true'], [role='menu'], [role='listbox']";
  const controlSelector = "button, [role='button']";
  const buttonId = "codey-workflow-native-once";
  const sessionButtonId = "codey-workflow-session-button";
  const settingsButtonId = "codey-settings-button";
  const styleId = "codey-workflow-mode-style";
  const statusId = "codey-workflow-mode-status";
  const configChangedEvent = "codey:config-changed";
  const workflowChangedEvent = "codey:workflow-changed";
  const testOptions = window.__CODEY_WORKFLOW_MODE_OPTIONS__ || {};
  const capabilityTimeoutMs = Number.isFinite(testOptions.capabilityTimeoutMs)
    ? Math.max(1, testOptions.capabilityTimeoutMs)
    : 3_000;
  const commandTimeoutMs = Number.isFinite(testOptions.commandTimeoutMs)
    ? Math.max(1, testOptions.commandTimeoutMs)
    : 30_000;
  const scanDelayMs = Number.isFinite(testOptions.scanDelayMs)
    ? Math.max(0, testOptions.scanDelayMs)
    : 120;
  const sessionPollMs = Number.isFinite(testOptions.sessionPollMs)
    ? Math.max(100, testOptions.sessionPollMs)
    : 2_000;
  const commandIdTtlMs = 5 * 60 * 1_000;
  const maxCommandIds = 100;

  let inputElement = null;
  let bypassButton = null;
  let sessionButton = null;
  let sessionWorkflow = null;
  let sessionWorkflowThreadId = null;
  let sessionWorkflowRequestId = 0;
  let sessionWorkflowPollTimer = 0;
  let statusElement = null;
  let statusTimer = 0;
  let observer = null;
  let observerActive = false;
  let scanTimer = 0;
  let retryTimer = 0;
  let retryCount = 0;
  let retryDelayMs = 120;
  let nativeReplayDepth = 0;
  let oneShotBypass = false;
  let capabilityState = {
    phase: "loading",
    enabled: false,
    healthy: false,
    supportsAudit: false,
  };
  const composingInputs = new WeakSet();
  const pendingSubmissions = new Map();
  const commandIds = new Map();
  const trackedTasks = new Set();

  const trackTask = (promise) => {
    const task = Promise.resolve(promise);
    trackedTasks.add(task);
    task.then(
      () => trackedTasks.delete(task),
      () => trackedTasks.delete(task),
    );
    return task;
  };

  const callBridge = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge !== "function") {
      return Promise.reject(new Error("bridge_unavailable"));
    }
    try {
      return Promise.resolve(window.__codexSessionDeleteBridge(path, payload));
    } catch {
      return Promise.reject(new Error("bridge_unavailable"));
    }
  };

  const withTimeout = (promise, ms) => {
    let timer = 0;
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("timeout")), ms);
    });
    return Promise.race([Promise.resolve(promise), timeout]).finally(() => {
      clearTimeout(timer);
    });
  };

  const queryAll = (root, selector) => {
    if (!root || typeof root.querySelectorAll !== "function") return [];
    try {
      return [...root.querySelectorAll(selector)];
    } catch {
      return [];
    }
  };

  const isComposerInput = (element) => {
    if (!element) return false;
    if (element.tagName === "TEXTAREA") return true;
    if (element.isContentEditable === true) return true;
    if (element.getAttribute?.("contenteditable") === "true") return true;
    return element.getAttribute?.("role") === "textbox";
  };

  const isVisible = (element) => {
    if (!element || element.isConnected === false) return false;
    if (element.closest?.(ignoredComposerSelector)) return false;
    if (element.closest?.("[hidden], [aria-hidden='true']")) return false;
    if (element.disabled === true) return false;
    try {
      const style = window.getComputedStyle?.(element);
      if (style?.display === "none" || style?.visibility === "hidden") {
        return false;
      }
    } catch {
      return false;
    }
    const rect = element.getBoundingClientRect?.();
    return Boolean(rect && rect.width > 0 && rect.height > 0);
  };

  const findComposerInput = () => {
    for (const anchor of queryAll(document, composerAnchorSelector)) {
      const scope = anchor.parentElement || anchor;
      for (const candidate of queryAll(scope, composerCandidateSelector)) {
        if (isComposerInput(candidate) && isVisible(candidate)) return candidate;
      }
    }

    let best = null;
    let bestScore = Number.NEGATIVE_INFINITY;
    const viewportHeight =
      window.innerHeight || document.documentElement?.clientHeight || 0;
    for (const candidate of queryAll(document, composerFallbackSelector)) {
      if (!isComposerInput(candidate) || !isVisible(candidate)) continue;
      const rect = candidate.getBoundingClientRect();
      if (
        viewportHeight > 0 &&
        (rect.bottom <= 0 || rect.top >= viewportHeight)
      ) {
        continue;
      }
      const score =
        Math.max(0, rect.bottom) * 10_000 +
        Math.min(rect.width * rect.height, 9_999_999);
      if (score > bestScore) {
        best = candidate;
        bestScore = score;
      }
    }
    return best;
  };

  const readComposerText = (element) => {
    if (!element) return "";
    if (element.tagName === "TEXTAREA" || element.tagName === "INPUT") {
      return typeof element.value === "string" ? element.value : "";
    }
    return typeof element.innerText === "string"
      ? element.innerText
      : String(element.textContent || "");
  };

  const dispatchInput = (element) => {
    let event = null;
    try {
      const InputEventConstructor = window.InputEvent || InputEvent;
      event = new InputEventConstructor("input", {
        bubbles: true,
        inputType: "deleteContentBackward",
        data: null,
      });
    } catch {
      try {
        const EventConstructor = window.Event || Event;
        event = new EventConstructor("input", { bubbles: true });
      } catch {
        event = { type: "input", bubbles: true };
      }
    }
    element.dispatchEvent?.(event);
  };

  const clearComposerText = (element) => {
    if (!element) return;
    if (element.tagName === "TEXTAREA" || element.tagName === "INPUT") {
      const prototype =
        element.tagName === "TEXTAREA"
          ? window.HTMLTextAreaElement?.prototype
          : window.HTMLInputElement?.prototype;
      const setter =
        prototype && Object.getOwnPropertyDescriptor(prototype, "value")?.set;
      if (setter) setter.call(element, "");
      else element.value = "";
    } else {
      element.textContent = "";
      if ("innerText" in element) element.innerText = "";
    }
    dispatchInput(element);
  };

  const normalizeThreadId = (value) => {
    const normalized = String(value || "")
      .replace(/^local:/, "")
      .trim();
    if (!normalized || normalized.startsWith("client-new-thread:")) return null;
    return normalized;
  };

  const findThreadId = (element) => {
    for (const anchor of queryAll(document, composerAnchorSelector)) {
      const scope = anchor.parentElement || anchor;
      if (scope === element || scope.contains?.(element)) {
        return normalizeThreadId(
          anchor.getAttribute?.("data-above-composer-conversation-id"),
        );
      }
    }
    const activeThread = document.querySelector?.(
      '[data-app-action-sidebar-thread-active="true"]',
    );
    const activeId = normalizeThreadId(
      activeThread?.getAttribute?.("data-app-action-sidebar-thread-id"),
    );
    if (activeId) return activeId;
    const pathname = String(window.location?.pathname || "");
    const match = pathname.match(
      /(?:\/c\/|\/conversation\/|\/session\/|\/local\/|\/threads\/)([A-Za-z0-9_-]+)/,
    );
    return normalizeThreadId(match?.[1]);
  };

  const activeSessionRunStates = new Set([
    "created",
    "queued",
    "running",
    "recovering",
    "pausing",
    "canceling",
  ]);

  const normalizeSessionRun = (value, threadId) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    const runId = String(value.runId || value.workflowId || "").trim();
    if (!runId) return null;
    const originThreadId = normalizeThreadId(
      value.originThreadId || value.threadId || threadId,
    );
    if (originThreadId && originThreadId !== threadId) return null;
    return {
      runId,
      threadId,
      state: String(value.state || value.status || "running")
        .replace(/_([a-z])/g, (_, letter) => letter.toUpperCase()),
      title: String(value.title || "").trim(),
    };
  };

  const sessionRunsFrom = (result, threadId) => {
    const source = result?.data || result?.result || result || {};
    const runs = Array.isArray(source.runs)
      ? source.runs
      : Array.isArray(source.items)
        ? source.items
        : [];
    return runs
      .map((run) => normalizeSessionRun(run, threadId))
      .filter(Boolean);
  };

  const clearSessionWorkflowPoll = () => {
    clearTimeout(sessionWorkflowPollTimer);
    sessionWorkflowPollTimer = 0;
  };

  function scheduleSessionWorkflowPoll() {
    clearSessionWorkflowPoll();
    if (
      !sessionWorkflow ||
      !activeSessionRunStates.has(sessionWorkflow.state) ||
      document.hidden === true
    ) {
      return;
    }
    sessionWorkflowPollTimer = setTimeout(() => {
      sessionWorkflowPollTimer = 0;
      trackTask(refreshSessionWorkflow(sessionWorkflow.threadId, true));
    }, sessionPollMs);
  }

  async function refreshSessionWorkflow(threadId, force = false) {
    const normalizedThreadId = normalizeThreadId(threadId);
    if (!normalizedThreadId) {
      sessionWorkflowRequestId += 1;
      sessionWorkflowThreadId = null;
      sessionWorkflow = null;
      clearSessionWorkflowPoll();
      mountSessionButton();
      return null;
    }
    if (!force && normalizedThreadId === sessionWorkflowThreadId) {
      mountSessionButton();
      return sessionWorkflow;
    }

    const threadChanged = normalizedThreadId !== sessionWorkflowThreadId;
    sessionWorkflowThreadId = normalizedThreadId;
    if (threadChanged) {
      sessionWorkflow = null;
      clearSessionWorkflowPoll();
      mountSessionButton();
    }
    const requestId = sessionWorkflowRequestId + 1;
    sessionWorkflowRequestId = requestId;
    try {
      const result = await withTimeout(
        callBridge(listPath, { threadId: normalizedThreadId, limit: 50 }),
        capabilityTimeoutMs,
      );
      if (result?.status === "failed" || result?.status === "error") {
        throw new Error("workflow_list_failed");
      }
      if (
        requestId !== sessionWorkflowRequestId ||
        sessionWorkflowThreadId !== normalizedThreadId ||
        findThreadId(inputElement || findComposerInput()) !== normalizedThreadId
      ) {
        return null;
      }
      sessionWorkflow = sessionRunsFrom(result, normalizedThreadId)[0] || null;
      mountSessionButton();
      scheduleSessionWorkflowPoll();
      return sessionWorkflow;
    } catch {
      if (
        requestId === sessionWorkflowRequestId &&
        sessionWorkflowThreadId === normalizedThreadId
      ) {
        mountSessionButton();
        scheduleSessionWorkflowPoll();
      }
      return sessionWorkflow;
    }
  }

  const rememberAcceptedSessionWorkflow = (threadId, result) => {
    const normalizedThreadId = normalizeThreadId(threadId);
    const runId = String(result?.runId || "").trim();
    if (!normalizedThreadId || !runId) return;
    sessionWorkflowRequestId += 1;
    sessionWorkflowThreadId = normalizedThreadId;
    sessionWorkflow = {
      runId,
      threadId: normalizedThreadId,
      state: "running",
      title: "",
    };
    mountSessionButton();
    scheduleSessionWorkflowPoll();
  };

  const workspaceCwdHint = (element) => {
    const candidates = [];
    let current = element;
    for (let depth = 0; current && depth < 8; depth += 1) {
      for (const attribute of [
        "data-workspace-root",
        "data-project-path",
        "data-cwd",
      ]) {
        candidates.push(current.getAttribute?.(attribute));
      }
      current = current.parentElement;
    }
    try {
      if (typeof window.__codeyWorkflowWorkspaceCwd === "function") {
        candidates.push(window.__codeyWorkflowWorkspaceCwd());
      }
    } catch {
      // A missing project hint safely disables new-task takeover.
    }
    return candidates.find(isAbsoluteLocalPath) || null;
  };

  const locationKey = () => String(window.location?.href || "");

  const composerScope = (element) => {
    for (const anchor of queryAll(document, composerAnchorSelector)) {
      const scope = anchor.parentElement || anchor;
      if (scope === element || scope.contains?.(element)) return scope;
    }
    const form = element?.closest?.("form");
    if (form) return form;
    let scope = element?.parentElement || null;
    let depth = 0;
    while (scope?.parentElement && depth < 4) {
      if (queryAll(scope, controlSelector).length > 0) return scope;
      scope = scope.parentElement;
      depth += 1;
    }
    return scope || element?.parentElement || null;
  };

  const descriptor = (element) =>
    [
      element?.getAttribute?.("aria-label"),
      element?.getAttribute?.("title"),
      element?.getAttribute?.("data-testid"),
      element?.getAttribute?.("data-state"),
      element?.textContent,
      element?.innerText,
    ]
      .filter((value) => typeof value === "string" && value.trim())
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();

  const isVisibleControl = (element) =>
    Boolean(element && element.id !== buttonId && isVisible(element));

  const isSendControl = (control) => {
    if (!isVisibleControl(control)) return false;
    const text = descriptor(control);
    if (
      /附件|attach|upload|microphone|voice|record|录音|语音|模型|model|权限|access|cancel|取消|stop|停止/i.test(
        text,
      )
    ) {
      return false;
    }
    if (control.getAttribute?.("type") === "submit") return true;
    return /(^|\b)(send|submit)(\b|$)|发送(?:消息|请求|提示词)?|提交(?:消息|请求)?/i.test(
      text,
    );
  };

  const findNativeSendControl = (element) => {
    const scope = composerScope(element);
    if (!scope) return null;
    const inputRect = element.getBoundingClientRect?.() || {
      bottom: 0,
      right: 0,
    };
    let best = null;
    let bestScore = Number.NEGATIVE_INFINITY;
    for (const control of queryAll(scope, controlSelector)) {
      if (!isSendControl(control)) continue;
      const rect = control.getBoundingClientRect?.() || {
        bottom: 0,
        right: 0,
      };
      const score =
        (control.getAttribute?.("type") === "submit" ? 1_000_000 : 0) +
        Math.max(0, rect.bottom - inputRect.bottom) * 100 +
        Math.max(0, rect.right);
      if (score > bestScore) {
        best = control;
        bestScore = score;
      }
    }
    return best;
  };

  const hasAttachment = (element) => {
    const scope = composerScope(element);
    if (!scope) return false;
    for (const fileInput of queryAll(scope, "input[type='file']")) {
      if (Number(fileInput.files?.length || 0) > 0) return true;
    }
    const markers = queryAll(
      scope,
      "[data-attachment-id], [data-testid*='attachment-preview'], [data-testid*='composer-attachment'], [data-codey-attachment]",
    );
    if (markers.some((marker) => marker.isConnected !== false)) return true;
    return queryAll(scope, controlSelector).some((control) =>
      /remove attachment|attachment preview|移除附件|附件预览/i.test(
        descriptor(control),
      ),
    );
  };

  const hasVoiceOrRecording = (element) => {
    const scope = composerScope(element);
    if (!scope) return false;
    if (
      queryAll(
        scope,
        "[data-recording='true'], [data-voice-recording='true'], [data-testid*='voice-preview'], audio[data-composer-audio]",
      ).length > 0
    ) {
      return true;
    }
    return queryAll(scope, controlSelector).some((control) => {
      const text = descriptor(control);
      const active =
        control.getAttribute?.("aria-pressed") === "true" ||
        control.getAttribute?.("data-state") === "recording";
      return (
        /stop recording|recording in progress|停止录音|正在录音/i.test(text) ||
        (active && /microphone|voice|record|麦克风|语音|录音/i.test(text))
      );
    });
  };

  const captureContext = (element) => {
    const text = readComposerText(element);
    return {
      element,
      text,
      threadId: findThreadId(element),
      locationKey: locationKey(),
      hasAttachment: hasAttachment(element),
      hasVoice: hasVoiceOrRecording(element),
      slashCommand: text.trimStart().startsWith("/"),
    };
  };

  const localEligibility = (context) => {
    if (!context.text.trim()) return { eligible: false, reason: "empty" };
    if (context.hasAttachment) {
      return { eligible: false, reason: "attachment" };
    }
    if (context.hasVoice) return { eligible: false, reason: "voice" };
    if (context.slashCommand) return { eligible: false, reason: "slash_command" };
    return { eligible: true, reason: "supported_text" };
  };

  const isAbsoluteLocalPath = (value) => {
    const path = String(value || "").trim();
    return (
      path.startsWith("/") ||
      path.startsWith("\\\\") ||
      /^[A-Za-z]:[\\/]/.test(path)
    );
  };

  const validPermissionSnapshot = (snapshot) => {
    if (!snapshot || typeof snapshot !== "object" || Array.isArray(snapshot)) {
      return false;
    }
    const snapshotIdentity =
      (typeof snapshot.snapshotId === "string" && snapshot.snapshotId.trim()) ||
      (typeof snapshot.hash === "string" && snapshot.hash.trim()) ||
      (Number.isFinite(snapshot.revision) && snapshot.revision >= 0);
    return Boolean(
      snapshot.resolved === true &&
        snapshotIdentity &&
        typeof snapshot.approvalPolicy === "string" &&
        snapshot.approvalPolicy.trim() &&
        typeof snapshot.sandboxMode === "string" &&
        snapshot.sandboxMode.trim(),
    );
  };

  const normalizeCapabilities = (result) => {
    const value = result?.workflow || result || {};
    const capabilities = value.capabilities || {};
    const context = value.context || {};
    const cwd = String(value.cwd || context.cwd || "").trim();
    const permissionSnapshot =
      value.permissionSnapshot || context.permissionSnapshot || null;
    const activeWorkflow = value.activeWorkflow || context.activeWorkflow || null;
    const protocolHealthy =
      Number(value.schemaVersion) === protocolVersion &&
      Number(value.protocolVersion) === protocolVersion;
    const capabilitiesHealthy = Boolean(
      capabilities.composerTakeover === true &&
        capabilities.start === true &&
        capabilities.steer === true,
    );
    const healthy = Boolean(
      value.status === "ok" &&
        value.enabled === true &&
        value.bridgeHealthy === true &&
        value.proxyHealthy === true &&
        protocolHealthy &&
        capabilitiesHealthy &&
        isAbsoluteLocalPath(cwd) &&
        validPermissionSnapshot(permissionSnapshot),
    );
    return {
      phase: "ready",
      enabled: value.enabled === true,
      healthy,
      supportsAudit: capabilities.bypassAudit === true,
      cwd: healthy ? cwd : "",
      permissionSnapshot: healthy ? permissionSnapshot : null,
      activeWorkflow: healthy ? activeWorkflow : null,
      reason: typeof value.reason === "string" ? value.reason : "",
    };
  };

  const capabilityPayload = (context) => ({
    schemaVersion: protocolVersion,
    protocolVersion,
    source: "codex_composer",
    threadId: context?.threadId || null,
    hasThreadId: Boolean(context?.threadId),
    cwdHint: workspaceCwdHint(context?.element),
  });

  const updateCapabilityState = (nextState) => {
    capabilityState = nextState;
    if (!capabilityState.enabled) oneShotBypass = false;
    refreshComposer();
  };

  const requestCapabilities = async (context, updateGlobal = true) => {
    const result = await withTimeout(
      callBridge(capabilitiesPath, capabilityPayload(context)),
      capabilityTimeoutMs,
    );
    const normalized = normalizeCapabilities(result);
    if (updateGlobal) updateCapabilityState(normalized);
    return normalized;
  };

  const showStatus = (message, tone = "info") => {
    ensureStatusElement();
    if (!statusElement) return;
    clearTimeout(statusTimer);
    statusElement.dataset.tone = tone;
    statusElement.dataset.visible = "true";
    statusElement.setAttribute("role", tone === "error" ? "alert" : "status");
    statusElement.setAttribute(
      "aria-live",
      tone === "error" ? "assertive" : "polite",
    );
    statusElement.textContent = message;
    statusTimer = setTimeout(() => {
      if (!statusElement) return;
      statusElement.dataset.visible = "false";
      statusElement.textContent = "";
    }, tone === "error" ? 8_000 : 4_000);
  };

  const bypassMessage = (reason) => {
    const reasons = {
      attachment: "包含附件",
      voice: "包含语音或正在录音",
      slash_command: "Slash 命令由 Codex 处理",
      native_once: "已选择原生 Codex",
      capabilities_unhealthy: "工作流能力未就绪",
      binding_unconfirmed: "新会话绑定未确认",
    };
    return `本次未经过工作流：${reasons[reason] || "安全条件未满足"}`;
  };

  const capabilityFailureDetail = (reason) => {
    const normalized = String(reason || "")
      .replace(/[\u0000-\u001f\u007f]/g, " ")
      .replace(/\s+/g, " ")
      .trim();
    if (!normalized) return "";
    return normalized.length > 180 ? `${normalized.slice(0, 180)}…` : normalized;
  };

  const capabilityFailureMessage = (failure) => {
    const base = bypassMessage("capabilities_unhealthy");
    const detail = capabilityFailureDetail(failure?.reason);
    return detail ? `${base}（${detail}）` : base;
  };

  const auditBypass = (reason, context, source, commandId = null) => {
    if (
      !capabilityState.supportsAudit ||
      typeof window.__codexSessionDeleteBridge !== "function"
    ) {
      return Promise.resolve();
    }
    // Deliberately exclude text, hashes, previews, and location URLs.
    const payload = {
      schemaVersion: protocolVersion,
      reason,
      source,
      commandId,
      threadId: context.threadId || null,
      hadAttachment: context.hasAttachment === true,
      hadVoice: context.hasVoice === true,
      wasSlashCommand: context.slashCommand === true,
    };
    return callBridge(bypassAuditPath, payload).catch(() => undefined);
  };

  const stopOwnedEvent = (event) => {
    event.preventDefault?.();
    if (typeof event.stopImmediatePropagation === "function") {
      event.stopImmediatePropagation();
    } else {
      event.stopPropagation?.();
    }
  };

  const contextStillMatches = (context, confirmedThreadId = null) => {
    const element = context.element?.isConnected === false && confirmedThreadId
      ? findComposerInput()
      : context.element;
    if (!element || element.isConnected === false) return false;
    if (readComposerText(element) !== context.text) return false;
    const currentThreadId = findThreadId(element);
    if (context.threadId) return currentThreadId === context.threadId;
    if (currentThreadId) {
      if (currentThreadId === confirmedThreadId && element !== context.element) {
        context.element = element;
      }
      return currentThreadId === confirmedThreadId;
    }
    return locationKey() === context.locationKey;
  };

  const replayNativeSend = (context, source) => {
    if (!contextStillMatches(context)) return false;
    const control = findNativeSendControl(context.element);
    nativeReplayDepth += 1;
    try {
      if (control && typeof control.click === "function") {
        control.click();
        return true;
      }
      if (source === "enter" && typeof context.element.dispatchEvent === "function") {
        let event;
        try {
          const KeyboardEventConstructor = window.KeyboardEvent || KeyboardEvent;
          event = new KeyboardEventConstructor("keydown", {
            bubbles: true,
            cancelable: true,
            code: "Enter",
            key: "Enter",
          });
        } catch {
          event = { bubbles: true, code: "Enter", key: "Enter", type: "keydown" };
        }
        context.element.dispatchEvent(event);
        return true;
      }
    } finally {
      nativeReplayDepth -= 1;
    }
    return false;
  };

  const textFingerprint = (text) => {
    let hash = 2166136261;
    for (let index = 0; index < text.length; index += 1) {
      hash ^= text.charCodeAt(index);
      hash = Math.imul(hash, 16777619);
    }
    return `${text.length}:${(hash >>> 0).toString(36)}`;
  };

  const submissionKey = (context) =>
    `${context.threadId || `new:${context.locationKey}`}|${textFingerprint(
      context.text,
    )}`;

  const pruneCommandIds = () => {
    const now = Date.now();
    for (const [key, record] of commandIds) {
      if (record.expiresAt <= now) commandIds.delete(key);
    }
    while (commandIds.size > maxCommandIds) {
      commandIds.delete(commandIds.keys().next().value);
    }
  };

  const newCommandId = () => {
    try {
      if (typeof window.crypto?.randomUUID === "function") {
        return window.crypto.randomUUID();
      }
    } catch {
      // The fallback remains unique for this renderer instance.
    }
    return `wf-${Date.now().toString(36)}-${Math.random()
      .toString(36)
      .slice(2, 12)}`;
  };

  const commandIdFor = (key) => {
    pruneCommandIds();
    const existing = commandIds.get(key);
    if (existing) return existing.id;
    const record = {
      id: newCommandId(),
      expiresAt: Date.now() + commandIdTtlMs,
    };
    commandIds.set(key, record);
    return record.id;
  };

  const hasDurableAck = (result) =>
    Boolean(
      result &&
        result.status !== "failed" &&
        result.durableAck === true &&
        (typeof result.engineEpoch === "string" ||
          Number.isFinite(result.engineEpoch)) &&
        String(result.engineEpoch).length > 0 &&
        (typeof result.revision === "string" ||
          Number.isFinite(result.revision)) &&
        String(result.revision).length > 0 &&
        typeof result.runId === "string" &&
        result.runId.trim(),
    );

  const confirmedBinding = (result) => {
    const binding = result?.binding || result?.originBinding;
    if (binding?.confirmed !== true) return null;
    const threadId = normalizeThreadId(binding.threadId);
    return threadId ? { threadId } : null;
  };

  const activeWorkflowFrom = (capabilities) => {
    const active = capabilities.activeWorkflow;
    if (
      !active ||
      active.active !== true ||
      typeof active.runId !== "string" ||
      !active.runId.trim() ||
      typeof active.originTurnId !== "string" ||
      !active.originTurnId.trim() ||
      !Number.isFinite(Number(active.revision))
    ) {
      return null;
    }
    return active;
  };

  const workflowPayloadBase = (context, capabilities, commandId) => ({
    schemaVersion: protocolVersion,
    protocolVersion,
    commandId,
    threadId: context.threadId || null,
    cwd: capabilities.cwd,
    permissionSnapshot: capabilities.permissionSnapshot,
    expectedRevision: Number(capabilities.activeWorkflow?.revision ?? 0),
    source: "codex_composer",
  });

  const handleUnhealthyPreflight = (context, source, failure = null) => {
    showStatus(capabilityFailureMessage(failure), "error");
    trackTask(auditBypass("capabilities_unhealthy", context, source));
    if (!replayNativeSend(context, source)) {
      showStatus(
        "工作流未接管；输入已保留，请再次使用原生 Codex 发送",
        "error",
      );
    }
  };

  const processSubmission = async (context, source, key) => {
    let capabilities;
    try {
      capabilities = await requestCapabilities(context, true);
    } catch (error) {
      handleUnhealthyPreflight(context, source, {
        reason: error instanceof Error ? error.message : "",
      });
      return;
    }
    if (!capabilities.enabled || !capabilities.healthy) {
      handleUnhealthyPreflight(context, source, capabilities);
      return;
    }
    if (!contextStillMatches(context)) {
      showStatus("输入或会话已变化，工作流未发送并保留当前内容", "error");
      return;
    }

    const commandId = commandIdFor(key);
    const activeWorkflow = activeWorkflowFrom(capabilities);
    if (capabilities.activeWorkflow?.active === true && !activeWorkflow) {
      handleUnhealthyPreflight(context, source, capabilities);
      return;
    }
    const base = workflowPayloadBase(context, capabilities, commandId);
    const path = activeWorkflow ? steerPath : startPath;
    const payload = activeWorkflow
      ? {
          ...base,
          runId: activeWorkflow.runId,
          text: context.text,
          delivery: {
            target: "current_origin_turn",
            originTurnId: activeWorkflow.originTurnId,
            backendDecision: "steer_or_recompile",
          },
        }
      : {
          ...base,
          text: context.text,
          origin: {
            threadId: context.threadId || null,
            requiresConfirmedBinding: context.threadId === null,
          },
        };

    showStatus(
      activeWorkflow ? "正在将跟进发送给当前工作流…" : "正在创建工作流…",
    );

    let result;
    try {
      result = await withTimeout(callBridge(path, payload), commandTimeoutMs);
    } catch {
      showStatus("工作流未确认持久化；输入已保留，可安全重试", "error");
      return;
    }

    let acceptedThreadId = context.threadId;
    if (!context.threadId) {
      const binding = confirmedBinding(result);
      if (!binding) {
        if (result?.accepted === false && result?.safeToSendNative === true) {
          commandIds.delete(key);
          showStatus(bypassMessage("binding_unconfirmed"), "error");
          trackTask(
            auditBypass("binding_unconfirmed", context, source, commandId),
          );
          if (!replayNativeSend(context, source)) {
            showStatus(
              "新会话未绑定，输入已保留，请再次使用原生 Codex 发送",
              "error",
            );
          }
        } else {
          showStatus("新会话绑定未确认；输入已保留，未触发原生发送", "error");
        }
        return;
      }
      acceptedThreadId = binding.threadId;
      if (!hasDurableAck(result)) {
        showStatus("工作流未确认持久化；输入已保留，可安全重试", "error");
        return;
      }
      if (!contextStillMatches(context, binding.threadId)) {
        showStatus("工作流已接纳，但输入上下文已变化，因此未清空", "error");
        return;
      }
    } else {
      if (!hasDurableAck(result)) {
        showStatus("工作流未确认持久化；输入已保留，可安全重试", "error");
        return;
      }
      if (!contextStillMatches(context)) {
        showStatus("工作流已接纳，但输入上下文已变化，因此未清空", "error");
        return;
      }
    }

    if (activeWorkflow && result.runId !== activeWorkflow.runId) {
      showStatus("工作流 ACK 与当前运行不匹配；输入已保留", "error");
      return;
    }
    commandIds.delete(key);
    clearComposerText(context.element);
    rememberAcceptedSessionWorkflow(acceptedThreadId, result);
    showStatus(
      activeWorkflow ? "跟进已持久化到当前工作流" : "工作流已持久化并开始执行",
      "success",
    );
  };

  const beginSubmission = (event, source, element) => {
    if (nativeReplayDepth > 0 || event?.__codeyWorkflowNativeReplay === true) {
      return;
    }
    if (!capabilityState.enabled) return;
    if (!element || composingInputs.has(element) || event?.isComposing) return;

    const context = captureContext(element);
    const eligibility = localEligibility(context);
    if (eligibility.reason === "empty") return;
    if (oneShotBypass) {
      oneShotBypass = false;
      updateBypassButtonState();
      showStatus(bypassMessage("native_once"));
      trackTask(auditBypass("native_once", context, source));
      return;
    }
    if (!eligibility.eligible) {
      showStatus(bypassMessage(eligibility.reason));
      trackTask(auditBypass(eligibility.reason, context, source));
      return;
    }

    stopOwnedEvent(event);
    const key = submissionKey(context);
    if (pendingSubmissions.has(key)) {
      showStatus("此发送已在工作流接纳中，请勿重复提交");
      return;
    }
    const promise = processSubmission(context, source, key);
    pendingSubmissions.set(key, promise);
    trackTask(
      promise.finally(() => {
        if (pendingSubmissions.get(key) === promise) {
          pendingSubmissions.delete(key);
        }
      }),
    );
  };

  const inputFromTarget = (target) => {
    if (isComposerInput(target)) return target;
    const closest = target?.closest?.(composerCandidateSelector);
    return isComposerInput(closest) ? closest : null;
  };

  const onKeyDown = (event) => {
    if (
      event?.key !== "Enter" ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.keyCode === 229
    ) {
      return;
    }
    const element = inputFromTarget(event.target);
    if (!element || !isVisible(element)) return;
    beginSubmission(event, "enter", element);
  };

  const onClick = (event) => {
    if (nativeReplayDepth > 0) return;
    const control = event.target?.closest?.(controlSelector);
    if (!control || control.id === buttonId || !isSendControl(control)) return;
    const element = inputElement && isVisible(inputElement)
      ? inputElement
      : findComposerInput();
    if (!element) return;
    const scope = composerScope(element);
    if (scope !== control && !scope?.contains?.(control)) return;
    beginSubmission(event, "click", element);
  };

  const onCompositionStart = (event) => {
    const element = inputFromTarget(event.target);
    if (element) composingInputs.add(element);
  };

  const onCompositionEnd = (event) => {
    const element = inputFromTarget(event.target);
    if (element) composingInputs.delete(element);
  };

  const ensureStyle = () => {
    if (document.getElementById?.(styleId)) return;
    const style = document.createElement?.("style");
    if (!style) return;
    style.id = styleId;
    style.textContent = `
      #${sessionButtonId} {
        -webkit-app-region: no-drag !important;
        pointer-events: auto !important;
        position: relative;
        z-index: 2147483642;
        display: inline-flex;
        align-items: center;
        flex: 0 0 auto;
        gap: 6px;
        min-width: 0;
        height: 28px;
        margin-inline-end: 4px;
        padding: 0 9px;
        border: 1px solid color-mix(in srgb, CanvasText 14%, transparent);
        border-radius: 8px;
        background: color-mix(in srgb, Canvas 94%, transparent);
        color: inherit;
        box-shadow: 0 1px 3px color-mix(in srgb, CanvasText 8%, transparent);
        font: 600 12px/1 system-ui, sans-serif;
        cursor: pointer;
        opacity: .9;
        user-select: none;
        transition: background .15s ease, border-color .15s ease, opacity .15s ease;
      }
      #${sessionButtonId}:hover { background: color-mix(in srgb, CanvasText 9%, Canvas); opacity: 1; }
      #${sessionButtonId}:focus-visible { outline: 2px solid rgba(99, 102, 241, .72); outline-offset: 2px; }
      #${sessionButtonId} .codey-workflow-session-dot {
        width: 7px;
        height: 7px;
        flex: 0 0 auto;
        border-radius: 999px;
        background: #8e8e93;
      }
      #${sessionButtonId}[data-state="running"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="created"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="queued"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="recovering"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="pausing"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="canceling"] .codey-workflow-session-dot { background: #0a84ff; box-shadow: 0 0 0 3px rgba(10, 132, 255, .13); }
      #${sessionButtonId}[data-state="succeeded"] .codey-workflow-session-dot { background: #30d158; }
      #${sessionButtonId}[data-state="failed"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="canceled"] .codey-workflow-session-dot { background: #ff453a; }
      #${sessionButtonId}[data-state="needsAttention"] .codey-workflow-session-dot,
      #${sessionButtonId}[data-state="paused"] .codey-workflow-session-dot { background: #ff9f0a; }
      #${buttonId} {
        -webkit-app-region: no-drag !important;
        display: inline-flex;
        align-items: center;
        min-height: 26px;
        margin-inline-end: 6px;
        padding: 0 9px;
        border: 1px solid rgba(124, 140, 255, .38);
        border-radius: 999px;
        background: rgba(30, 34, 50, .9);
        color: inherit;
        font: 12px/1 system-ui, sans-serif;
        cursor: pointer;
      }
      #${buttonId}:focus-visible { outline: 2px solid #818cf8; outline-offset: 2px; }
      #${buttonId}[data-armed="true"] { background: rgba(79, 70, 229, .28); }
      #${buttonId}:disabled { display: none; }
      #${statusId} {
        -webkit-app-region: no-drag !important;
        position: fixed;
        right: 20px;
        bottom: 22px;
        z-index: 2147483646;
        max-width: 380px;
        padding: 10px 13px;
        border: 1px solid rgba(124, 140, 255, .45);
        border-radius: 11px;
        background: rgba(20, 24, 36, .97);
        color: #eef2ff;
        box-shadow: 0 12px 36px rgba(0, 0, 0, .4);
        font: 12px/1.45 system-ui, sans-serif;
      }
      #${statusId}[data-visible="false"] { display: none; }
      #${statusId}[data-tone="error"] { border-color: rgba(248, 113, 113, .65); color: #fecaca; }
      #${statusId}[data-tone="success"] { border-color: rgba(74, 222, 128, .55); color: #bbf7d0; }
    `;
    document.documentElement?.appendChild?.(style);
  };

  function ensureStatusElement() {
    if (statusElement && statusElement.isConnected !== false) return;
    statusElement = document.getElementById?.(statusId) || null;
    if (!statusElement) {
      statusElement = document.createElement?.("div") || null;
      if (!statusElement) return;
      statusElement.id = statusId;
      statusElement.dataset.visible = "false";
      statusElement.setAttribute("role", "status");
      statusElement.setAttribute("aria-live", "polite");
      statusElement.setAttribute("aria-atomic", "true");
      document.documentElement?.appendChild?.(statusElement);
    }
  }

  const sessionRunStateLabel = (state) => {
    const labels = {
      created: "已创建",
      queued: "排队中",
      running: "运行中",
      recovering: "恢复中",
      succeeded: "已完成",
      failed: "失败",
      needsAttention: "需要处理",
      pausing: "暂停中",
      paused: "已暂停",
      canceling: "取消中",
      canceled: "已取消",
    };
    return labels[state] || "有运行记录";
  };

  const openSessionWorkflowConsole = async () => {
    const currentThreadId = findThreadId(inputElement || findComposerInput());
    if (
      !sessionWorkflow ||
      !currentThreadId ||
      sessionWorkflow.threadId !== currentThreadId
    ) {
      mountSessionButton();
      return;
    }
    const request = {
      threadId: currentThreadId,
      runId: sessionWorkflow.runId,
    };
    try {
      const overlay = window.__codeySettingsOverlay;
      if (typeof overlay?.openWorkflow === "function") {
        overlay.openWorkflow(request);
        return;
      }
      if (typeof overlay?.load === "function") {
        const loaded = await overlay.load();
        if (typeof loaded?.openWorkflow === "function") {
          loaded.openWorkflow(request);
        } else if (typeof loaded?.open === "function") {
          loaded.open();
        } else {
          loaded?.toggle?.();
        }
        return;
      }
      throw new Error("workflow_overlay_unavailable");
    } catch {
      showStatus("工作流详情面板加载失败，请稍后重试", "error");
    }
  };

  const createSessionButton = () => {
    const button = document.createElement("button");
    button.id = sessionButtonId;
    button.type = "button";
    button.dataset.codeyWorkflowSession = "true";
    const dot = document.createElement("span");
    dot.className = "codey-workflow-session-dot";
    dot.setAttribute("aria-hidden", "true");
    const label = document.createElement("span");
    label.textContent = "工作流";
    button.appendChild(dot);
    button.appendChild(label);
    button.addEventListener(
      "click",
      (event) => {
        event.preventDefault?.();
        event.stopPropagation?.();
        trackTask(openSessionWorkflowConsole());
      },
      true,
    );
    return button;
  };

  function mountSessionButton() {
    const currentThreadId = findThreadId(inputElement || findComposerInput());
    if (
      !sessionWorkflow ||
      !currentThreadId ||
      sessionWorkflow.threadId !== currentThreadId
    ) {
      sessionButton?.remove?.();
      return;
    }
    const settingsButton = document.getElementById?.(settingsButtonId);
    const host = settingsButton?.parentElement;
    if (!settingsButton || !host?.insertBefore) {
      sessionButton?.remove?.();
      return;
    }
    if (!sessionButton) {
      sessionButton =
        document.getElementById?.(sessionButtonId) || createSessionButton();
    }
    const stateLabel = sessionRunStateLabel(sessionWorkflow.state);
    sessionButton.dataset.state = sessionWorkflow.state;
    sessionButton.setAttribute(
      "aria-label",
      `查看当前会话工作流：${stateLabel}`,
    );
    sessionButton.title = `查看当前会话工作流（${stateLabel}）`;
    if (
      sessionButton.parentElement !== host ||
      sessionButton.nextElementSibling !== settingsButton
    ) {
      host.insertBefore(sessionButton, settingsButton);
    }
  }

  function updateBypassButtonState() {
    if (!bypassButton) return;
    bypassButton.dataset.armed = String(oneShotBypass);
    bypassButton.setAttribute("aria-pressed", String(oneShotBypass));
    bypassButton.setAttribute(
      "aria-label",
      oneShotBypass
        ? "已选择：下一次使用原生 Codex"
        : "本次使用原生 Codex",
    );
    bypassButton.title = bypassButton.getAttribute("aria-label");
  }

  const createBypassButton = () => {
    const button = document.createElement("button");
    button.id = buttonId;
    button.type = "button";
    button.dataset.codeyWorkflowNativeOnce = "true";
    button.textContent = "本次使用原生 Codex";
    button.addEventListener(
      "click",
      (event) => {
        event.preventDefault?.();
        event.stopPropagation?.();
        oneShotBypass = true;
        updateBypassButtonState();
        showStatus("下一次发送将直接使用原生 Codex");
      },
      true,
    );
    return button;
  };

  const mountBypassButton = (element) => {
    if (!capabilityState.enabled) {
      if (bypassButton) bypassButton.disabled = true;
      return;
    }
    const sendControl = findNativeSendControl(element);
    const host = sendControl?.parentElement;
    if (!sendControl || !host?.insertBefore) return;
    if (!bypassButton) {
      bypassButton = document.getElementById?.(buttonId) || createBypassButton();
    }
    bypassButton.disabled = !capabilityState.healthy;
    updateBypassButtonState();
    if (bypassButton.parentElement !== host) {
      host.insertBefore(bypassButton, sendControl);
    }
  };

  function refreshComposer() {
    ensureStyle();
    ensureStatusElement();
    const nextInput = findComposerInput();
    inputElement = nextInput || null;
    if (inputElement) mountBypassButton(inputElement);
    else if (bypassButton) bypassButton.disabled = true;
    const currentThreadId = findThreadId(inputElement || nextInput);
    if (currentThreadId !== sessionWorkflowThreadId) {
      trackTask(refreshSessionWorkflow(currentThreadId));
    } else {
      mountSessionButton();
    }
  }

  const scheduleScan = () => {
    if (scanTimer) return;
    scanTimer = setTimeout(() => {
      scanTimer = 0;
      refreshComposer();
    }, scanDelayMs);
  };

  const installObserver = () => {
    if (observer) return;
    observer = new MutationObserver((mutations) => {
      const relevant = mutations.some((mutation) => {
        const target = mutation.target;
        if (target?.id === styleId || target?.id === statusId) return false;
        if (
          target === bypassButton ||
          target === sessionButton ||
          target?.closest?.(`#${buttonId}, #${sessionButtonId}`)
        ) {
          return false;
        }
        if (!inputElement?.isConnected) return true;
        if (
          mutation.type === "attributes" &&
          mutation.attributeName === "data-above-composer-conversation-id"
        ) {
          return true;
        }
        if (mutation.type === "attributes") {
          const scope = composerScope(inputElement);
          return Boolean(
            target === inputElement ||
              target?.contains?.(inputElement) ||
              inputElement?.contains?.(target) ||
              scope?.contains?.(target),
          );
        }
        if (mutation.type !== "childList") return false;
        return [...(mutation.addedNodes || []), ...(mutation.removedNodes || [])].some(
          (node) =>
            node === inputElement ||
            node === bypassButton ||
            node?.id === settingsButtonId ||
            node?.contains?.(inputElement) ||
            isComposerInput(node) ||
            node?.querySelector?.(composerCandidateSelector),
        );
      });
      if (relevant) scheduleScan();
    });
    observer.observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: [
        "aria-hidden",
        "aria-label",
        "aria-pressed",
        "class",
        "contenteditable",
        "data-attachment-id",
        "data-above-composer-conversation-id",
        "data-recording",
        "data-state",
        "data-voice-recording",
        "disabled",
        "hidden",
        "role",
        "style",
      ],
    });
    observerActive = true;
  };

  const scheduleCapabilityRetry = () => {
    if (retryTimer || retryCount >= 8) return;
    retryTimer = setTimeout(() => {
      retryTimer = 0;
      trackTask(refreshCapabilities());
    }, retryDelayMs);
    retryDelayMs = Math.min(retryDelayMs * 2, 2_000);
  };

  const refreshCapabilities = async () => {
    retryCount += 1;
    const element = inputElement || findComposerInput();
    const context = element ? captureContext(element) : null;
    try {
      const next = await requestCapabilities(context, true);
      retryCount = 0;
      retryDelayMs = 120;
      clearTimeout(retryTimer);
      retryTimer = 0;
      return next;
    } catch {
      capabilityState = {
        ...capabilityState,
        phase: "failed",
        healthy: false,
      };
      refreshComposer();
      scheduleCapabilityRetry();
      return capabilityState;
    }
  };

  const whenIdle = async () => {
    for (let pass = 0; pass < 20; pass += 1) {
      const tasks = [...trackedTasks];
      if (!tasks.length) return;
      await Promise.allSettled(tasks);
    }
  };

  document.addEventListener("keydown", onKeyDown, true);
  document.addEventListener("click", onClick, true);
  document.addEventListener("compositionstart", onCompositionStart, true);
  document.addEventListener("compositionend", onCompositionEnd, true);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden === true) {
      clearSessionWorkflowPoll();
      return;
    }
    const threadId = findThreadId(inputElement || findComposerInput());
    trackTask(refreshSessionWorkflow(threadId, true));
  });
  window.addEventListener(configChangedEvent, () => trackTask(refreshCapabilities()));
  window.addEventListener(workflowChangedEvent, () => {
    trackTask(refreshCapabilities());
    trackTask(
      refreshSessionWorkflow(
        findThreadId(inputElement || findComposerInput()),
        true,
      ),
    );
  });
  window.addEventListener("hashchange", scheduleScan);
  window.addEventListener("popstate", scheduleScan);

  ensureStyle();
  ensureStatusElement();
  refreshComposer();
  installObserver();

  const testApi = {
    version: protocolVersion,
    armNativeOnce: () => {
      oneShotBypass = true;
      updateBypassButtonState();
    },
    evaluateLocalEligibility: (element = inputElement) =>
      element ? localEligibility(captureContext(element)) : { eligible: false, reason: "empty" },
    refreshCapabilities: () => trackTask(refreshCapabilities()),
    refreshSessionWorkflow: () =>
      trackTask(
        refreshSessionWorkflow(
          findThreadId(inputElement || findComposerInput()),
          true,
        ),
      ),
    scanNow: refreshComposer,
    snapshot: () => ({
      phase: capabilityState.phase,
      enabled: capabilityState.enabled,
      healthy: capabilityState.healthy,
      supportsAudit: capabilityState.supportsAudit,
      hasInput: Boolean(inputElement && isVisible(inputElement)),
      hasButton: Boolean(bypassButton?.isConnected),
      hasSessionButton: Boolean(sessionButton?.isConnected),
      sessionRunId: sessionWorkflow?.runId || null,
      sessionThreadId: sessionWorkflow?.threadId || null,
      buttonArmed: oneShotBypass,
      observerActive,
      pendingCount: pendingSubmissions.size,
    }),
    whenIdle,
  };
  window.__CODEY_WORKFLOW_MODE_TEST__ = testApi;
  runtime.scanNow = refreshComposer;
  runtime.testApi = testApi;

  trackTask(refreshCapabilities());
})();
