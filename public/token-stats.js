(() => {
  if (window.__codeyTokenStats?.version === 2) {
    void window.__codeyTokenStats.refresh?.();
    return;
  }
  window.__codeyTokenStats?.dispose?.();

  const patchVersion = 2;
  const bridgePath = "/token-stats";
  const settingsPath = "/settings/get";
  const configChangedEvent = "codey:config-changed";
  const requestEvent = "codex-message-from-view";
  const turnRowSelector = "[data-turn-key]";
  const styleId = "codey-token-stats-style";
  const footerClass = "codey-token-stats-footer";
  const settleMs = 2000;
  const maxResolveAttempts = 120;
  const sessionIdAttributes = [
    "data-session-id",
    "data-conversation-id",
    "data-thread-id",
    "data-request-user-input-auto-resolution-conversation-id",
    "data-response-annotation-conversation",
    "data-above-composer-conversation-id",
  ];

  const state = {
    installed: true,
    enabled: true,
    observedTurns: 0,
    observedMessages: 0,
    lastTurn: null,
  };

  let lastTurnStart = null;
  const trackedRows = typeof WeakSet === "function" ? new WeakSet() : new Set();
  const rowMessages = typeof WeakMap === "function" ? new WeakMap() : new Map();
  const pendingRows = [];
  let observer = null;
  let styleInstalled = false;
  let disposed = false;

  const callBridge = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };

  const normalizeId = (value) => String(value || "").replace(/^local:/, "").trim();

  const normalizeTurnKey = (value) => {
    const normalized = String(value || "").trim();
    const marker = ":turn:";
    const index = normalized.lastIndexOf(marker);
    return index >= 0 ? normalized.slice(index + marker.length).trim() : normalized;
  };

  const currentSessionId = () => {
    for (const attribute of sessionIdAttributes) {
      const value = document.querySelector(`[${attribute}]`)?.getAttribute(attribute);
      if (value) return normalizeId(value);
    }
    const active = document
      .querySelector('[data-app-action-sidebar-thread-active="true"]')
      ?.getAttribute("data-app-action-sidebar-thread-id");
    if (active) return normalizeId(active);
    return "";
  };

  const installStyle = () => {
    if (styleInstalled || !document.documentElement) return;
    if (document.getElementById(styleId)) {
      styleInstalled = true;
      return;
    }
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      .${footerClass} {
        margin: 4px 0 2px;
        font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Helvetica Neue", sans-serif;
        font-size: 11px;
        line-height: 1.4;
        color: color-mix(in srgb, CanvasText 62%, transparent);
        font-variant-numeric: tabular-nums;
        user-select: text;
      }
      .${footerClass}[data-state="partial"] { opacity: .82; }
      .${footerClass}[data-state="unavailable"] { opacity: .62; }
      .${footerClass} .codey-token-stats-sub { margin-top: 1px; color: color-mix(in srgb, CanvasText 52%, transparent); }
      @media (prefers-reduced-motion: reduce) {
        .${footerClass} { transition: none !important; }
      }
    `;
    document.documentElement.appendChild(style);
    styleInstalled = true;
  };

  const formatMs = (value) => {
    if (value == null) return "—";
    const number = Number(value);
    if (!Number.isFinite(number) || number < 0) return "—";
    return number < 1000 ? `${Math.round(number)}ms` : `${(number / 1000).toFixed(1)}s`;
  };

  const formatSeconds = (value) => {
    if (value == null) return "—";
    const number = Number(value);
    if (!Number.isFinite(number) || number < 0) return "—";
    return `${(number / 1000).toFixed(1)}s`;
  };

  const formatTokens = (value) => {
    if (value == null) return "—";
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? String(number) : "—";
  };

  const formatTps = (outputTokens, durationMs, ttftMs) => {
    const tokens = Number(outputTokens);
    const duration = Number(durationMs);
    const ttft = Number(ttftMs);
    if (!Number.isFinite(tokens) || tokens <= 0) return "—";
    let generationMs = duration;
    if (!Number.isFinite(generationMs) || generationMs <= 0) return "—";
    if (Number.isFinite(ttft) && ttft > 0 && duration > ttft) generationMs = duration - ttft;
    return `${(tokens / (generationMs / 1000)).toFixed(1)} tok/s`;
  };

  const footerText = (message) => {
    const total = message.totalTokens ?? (
      message.inputTokens != null && message.outputTokens != null
        ? message.inputTokens + message.outputTokens
        : null
    );
    const parts = [
      `首字 ${formatMs(message.ttftMs)}`,
      `速度 ${formatTps(message.outputTokens, message.durationMs, message.ttftMs)}`,
      `耗时 ${formatSeconds(message.durationMs)}`,
      `输入 ${formatTokens(message.inputTokens)} tokens`,
      `输出 ${formatTokens(message.outputTokens)} tokens`,
      `合计 ${formatTokens(total)} tokens`,
    ];
    return parts.join(" · ");
  };

  const subagentText = (stats) => {
    const parts = [
      "子代理",
      `输入 ${formatTokens(stats?.inputTokens)} tokens`,
      `输出 ${formatTokens(stats?.outputTokens)} tokens`,
      `合计 ${formatTokens(stats?.totalTokens)} tokens`,
    ];
    const count = Number(stats?.count);
    if (Number.isFinite(count) && count > 0) parts.push(`${count} 轮`);
    return parts.join(" · ");
  };

  const dataState = (message) => {
    if (message.durationMs == null && message.ttftMs == null) return "unavailable";
    if (message.durationMs == null) return "partial";
    return "ready";
  };

  const renderFooter = (message) => {
    if (disposed || !state.enabled || !(message.row instanceof HTMLElement)) return;
    if (!message.row.isConnected) return;
    installStyle();
    let footer = message.footer;
    if (!(footer instanceof HTMLElement)) {
      footer = document.createElement("div");
      footer.className = footerClass;
      footer.setAttribute("role", "note");
      message.footer = footer;
    }
    const text = footerText(message);
    footer.textContent = text;
    footer.dataset.state = dataState(message);
    const subagentLabel = message.subagentStats ? ` · ${subagentText(message.subagentStats)}` : "";
    footer.setAttribute("aria-label", `${text}${subagentLabel}`);
    footer.title = `turn=${message.turnKey || "—"} session=${message.sessionId || "—"} reason=${message.reasonCode ?? "pending"} t0=${message.t0 ?? "—"} first=${message.firstTokenTs ?? "—"}`;
    if (message.subagentStats) {
      const sub = document.createElement("div");
      sub.className = "codey-token-stats-sub";
      sub.textContent = subagentText(message.subagentStats);
      footer.appendChild(sub);
    } else {
      footer.querySelector?.(".codey-token-stats-sub")?.remove?.();
    }
    if (footer.parentElement !== message.row) {
      message.row.appendChild(footer);
    }
  };

  const removeAllFooters = () => {
    document.querySelectorAll?.(`.${footerClass}`)?.forEach?.((footer) => footer.remove());
    for (const message of rowMessages.values()) {
      if (message && typeof message === "object") message.footer = null;
    }
  };

  const rememberLastTurn = (message) => {
    state.lastTurn = {
      turnKey: message.turnKey ?? "",
      sessionId: message.sessionId ?? "",
      reasonCode: message.reasonCode ?? null,
      ttftMs: message.ttftMs ?? null,
      durationMs: message.durationMs ?? null,
      inputTokens: message.inputTokens ?? null,
      outputTokens: message.outputTokens ?? null,
      totalTokens: message.totalTokens ?? null,
      subagentCount: message.subagentStats?.count ?? null,
    };
  };

  const finalizeMessage = async (message) => {
    if (disposed || !state.enabled || message.finalized) return;

    const hasTurn = message.turnKey && message.sessionId;
    if (!hasTurn) {
      message.finalized = true;
      message.observer?.disconnect?.();
      rememberLastTurn(message);
      return;
    }

    message.resolveAttempts = (message.resolveAttempts || 0) + 1;
    const result = await callBridge(bridgePath, {
      turnId: message.turnKey,
      sessionId: message.sessionId,
    }).catch(() => null);

    if (!result || result.status === "failed") {
      finishOrRequeue(message, null);
      return;
    }
    if (result.status === "disabled") {
      message.finalized = true;
      message.observer?.disconnect?.();
      removeAllFooters();
      return;
    }
    // A turn is only "done" once the rollout carries its task_complete
    // duration; subagent/reasoning pauses keep the message open so a later
    // settle (or resumed streaming) can still populate the numbers.
    if (result.status === "ok" && result.durationMs != null) {
      message.durationMs = result.durationMs ?? null;
      message.inputTokens = result.inputTokens ?? null;
      message.outputTokens = result.outputTokens ?? null;
      message.totalTokens = result.totalTokens ?? null;
      message.reasonCode = result.reasonCode ?? null;
      message.subagentStats = result.subagentStats ?? null;
      message.finalized = true;
      message.observer?.disconnect?.();
      rememberLastTurn(message);
      // Only show once we actually have token data, never a placeholder row.
      if (message.inputTokens != null || message.outputTokens != null) {
        renderFooter(message);
      }
      return;
    }
    finishOrRequeue(message, result);
  };

  const finishOrRequeue = (message, result) => {
    if (message.resolveAttempts >= maxResolveAttempts) {
      // Give up silently: no placeholder footer, just stop polling.
      if (result && result.reasonCode) {
        message.reasonCode = result.reasonCode;
      }
      message.finalized = true;
      message.observer?.disconnect?.();
      rememberLastTurn(message);
      return;
    }
    scheduleSettle(message);
  };

  const markFirstToken = (message) => {
    if (message.firstTokenTs != null) return;
    message.firstTokenTs = typeof performance?.now === "function" ? performance.now() : Date.now();
    if (message.t0 != null) {
      message.ttftMs = message.firstTokenTs - message.t0;
    }
  };

  const scheduleSettle = (message) => {
    if (message.settleTimer) window.clearTimeout?.(message.settleTimer);
    message.settleTimer = window.setTimeout(() => {
      message.settleTimer = 0;
      void finalizeMessage(message);
    }, settleMs);
  };

  const confirmTrackedRow = (row, turnKey, start) => {
    if (rowMessages.has(row)) return;
    const now = typeof performance?.now === "function" ? performance.now() : Date.now();
    const message = {
      row,
      turnKey,
      sessionId: currentSessionId() || start?.threadId || "",
      t0: start?.t0 ?? null,
      firstTokenTs: null,
      ttftMs: null,
      textBaseline: (row.textContent || "").length,
      trackedAt: now,
      durationMs: null,
      inputTokens: null,
      outputTokens: null,
      totalTokens: null,
      reasonCode: null,
      subagentStats: null,
      resolveAttempts: 0,
      finalized: false,
      settleTimer: 0,
      observer: null,
      footer: null,
    };
    rowMessages.set(row, message);
    state.observedMessages += 1;

    const messageObserver = new MutationObserver(() => {
      // Only scan text growth until the first token is captured, so long
      // streaming replies don't re-read the whole row on every mutation.
      if (message.firstTokenTs == null) {
        const currentTextLength = (row.textContent || "").length;
        if (currentTextLength > message.textBaseline) {
          const nowTs = typeof performance?.now === "function"
            ? performance.now()
            : Date.now();
          // The user message and transient status text also grow the row in the
          // first few hundred milliseconds; absorb those into the baseline so
          // only the model's actual first token is measured.
          if (message.trackedAt != null && nowTs - message.trackedAt < 500) {
            message.textBaseline = currentTextLength;
          } else {
            markFirstToken(message);
          }
        }
      }
      // Active output resets the give-up countdown so long subagent/reasoning
      // pauses followed by more streaming never exhaust the retry budget.
      message.resolveAttempts = 0;
      scheduleSettle(message);
    });
    messageObserver.observe(row, {
      childList: true,
      characterData: true,
      subtree: true,
    });
    message.observer = messageObserver;
  };

  const trackTurn = (row) => {
    if (trackedRows.has(row)) return;
    trackedRows.add(row);
    const turnKey = normalizeTurnKey(row.getAttribute?.("data-turn-key") || "");
    const now = typeof performance?.now === "function" ? performance.now() : Date.now();
    const recentStart =
      lastTurnStart && lastTurnStart.at != null && now - lastTurnStart.at < 30_000
        ? lastTurnStart
        : null;
    if (!recentStart) {
      // Switching back to a thread re-renders every historical turn; park those
      // and only process them if a fresh turn/start confirms one shortly after.
      pendingRows.push({ row, turnKey, trackedAt: now });
      if (pendingRows.length > 16) pendingRows.shift();
      return;
    }
    confirmTrackedRow(row, turnKey, recentStart);
  };

  const scanAddedNode = (node) => {
    if (!(node instanceof Element)) return;
    if (node.matches?.(turnRowSelector) && typeof node.getAttribute === "function") {
      trackTurn(node);
    }
    if (typeof node.querySelectorAll !== "function") return;
    for (const row of node.querySelectorAll(turnRowSelector)) {
      trackTurn(row);
    }
  };

  const installObserver = () => {
    if (observer || typeof MutationObserver !== "function" || !document.documentElement) return;
    observer = new MutationObserver((mutations) => {
      if (!state.enabled) return;
      for (const mutation of mutations) {
        if (mutation.type !== "childList") continue;
        for (const node of mutation.addedNodes || []) scanAddedNode(node);
      }
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
  };

  const onTurnRequest = (event) => {
    if (!state.enabled) return;
    const detail = event?.detail;
    if (!detail || detail.type !== "mcp-request") return;
    const request = detail.request;
    if (!request || typeof request !== "object") return;
    let method = String(request.method || "");
    let params = request.params;
    if (method === "send-cli-request-for-host" && typeof params?.method === "string") {
      method = params.method;
      params = params.params || {};
    }
    if (method !== "turn/start") return;
    const t0 = typeof performance?.now === "function" ? performance.now() : Date.now();
    lastTurnStart = {
      t0,
      at: t0,
      threadId: normalizeId(params?.threadId || ""),
    };
    state.observedTurns += 1;
    const horizon = t0 - 5000;
    while (pendingRows.length > 0) {
      const entry = pendingRows.shift();
      if (!entry || entry.trackedAt < horizon) continue;
      confirmTrackedRow(entry.row, entry.turnKey, lastTurnStart);
    }
  };

  const reconcileFooters = () => {
    if (disposed || !state.enabled) return;
    for (const message of rowMessages.values()) {
      if (!message || !(message.footer instanceof HTMLElement)) continue;
      if (
        message.footer.parentElement !== message.row
        && message.row.isConnected
      ) {
        message.row.appendChild(message.footer);
      }
    }
  };

  const loadConfig = async () => {
    try {
      const config = await callBridge(settingsPath, {});
      if (config && typeof config === "object" && "showTokenStatsCard" in config) {
        state.enabled = config.showTokenStatsCard !== false;
      } else {
        state.enabled = true;
      }
    } catch {
      // Keep the previous value when the settings bridge is unavailable.
    }
    if (!state.enabled) removeAllFooters();
  };

  installObserver();
  if (typeof window.addEventListener === "function") {
    window.addEventListener(requestEvent, onTurnRequest, true);
    window.addEventListener(configChangedEvent, () => {
      void loadConfig();
    });
    window.addEventListener("focus", () => {
      void loadConfig();
    });
    document.addEventListener?.("visibilitychange", () => {
      if (document.visibilityState === "visible") void loadConfig();
    });
  }
  void loadConfig();
  if (typeof window.setInterval === "function") {
    window.setInterval(reconcileFooters, 2000);
  }

  window.__codeyTokenStats = Object.freeze({
    version: patchVersion,
    snapshot: () => ({
      installed: state.installed,
      enabled: state.enabled,
      observedTurns: state.observedTurns,
      observedMessages: state.observedMessages,
      lastTurn: state.lastTurn,
    }),
    refresh: loadConfig,
    dispose: () => {
      disposed = true;
      observer?.disconnect?.();
      observer = null;
      window.removeEventListener?.(requestEvent, onTurnRequest, true);
      removeAllFooters();
    },
  });
})();
