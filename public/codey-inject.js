// Sidebar/session tools loaded by renderer-inject.js after Codex's sidebar is
// present. This file also remains useful as a backwards-compatible manual CDP
// testing entry point.
(() => {
  if (window.__codeySessionToolsInjectLoaded) return;
  if (window.__codeySessionToolsInjectLoading) return;
  window.__codeySessionToolsInjectLoading = true;
  let disposeInstall;
  try {
  // Release a failed installation before publishing any replacement callbacks.
  window.__codeySessionToolsInstall?.dispose?.();
  // 旧安装已销毁，先摘除引用：中途抛错时全局不会留下一个已销毁的安装对象，
  // 下一轮加载也不会对着它重复销毁。
  delete window.__codeySessionToolsInstall;
  let disposed = false;
  const rendererSettingsButtonSelector = "#codey-settings-button";
  const toolbarId = "codey-message-toolbar";
  const toastId = "codey-runtime-toast";
  const styleId = "codey-injected-style";
  const selectedClass = "codey-message-selected";
  const sessionExportAttribute = "data-codey-session-export";
  const tasksImportAttribute = "data-codey-tasks-import";
  const projectImportAttribute = "data-codey-project-import";
  const sessionDeleteAttribute = "data-codey-session-delete";
  const sidebarActionTooltipId = "codey-sidebar-action-tooltip";
  const threadUpdatedAtAttribute = "data-codey-thread-updated-at";
  const threadUpdatedAtMsAttribute = "data-codey-thread-updated-at-ms";
  const threadRunningAttribute = "data-codey-thread-running";
  const sessionExportIcon = `
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
      <polyline points="17 8 12 3 7 8"></polyline>
      <line x1="12" x2="12" y1="3" y2="15"></line>
    </svg>
  `;
  const projectImportIcon = `
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
      <polyline points="7 10 12 15 17 10"></polyline>
      <line x1="12" x2="12" y1="15" y2="3"></line>
    </svg>
  `;
  const sessionDeleteIcon = `
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <path d="M3 6h18"></path>
      <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"></path>
      <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
      <line x1="10" x2="10" y1="11" y2="17"></line>
      <line x1="14" x2="14" y1="11" y2="17"></line>
    </svg>
  `;
  let lastSelectedRow = null;
  let messageDeleteInFlight = false;
  let scanTimer = 0;
  let initialScanHandle = 0;
  let initialScanUsesIdleCallback = false;
  let scanDeadline = 0;
  const scanDebounceMs = 60;
  const maxScanLatencyMs = 250;
  const sidebarTitleCache = new Map();
  let watcherWakeTimer = 0;
  let completionReconcileInFlight = false;
  let completionNextReconcileAt = 0;
  let completionReconcileSessionId = "";
  let completionCandidateSessionId = "";
  let completionReconcileCheckTimer = 0;
  let sidebarActionTooltipTimer = 0;
  let sidebarActionTooltipAnchor = null;
  let threadUpdatedAtFetchTimer = 0;
  let threadUpdatedAtFetchInFlight = false;
  const threadUpdatedAtCache = new Map();
  const threadWorkStateByRow = new WeakMap();
  // 同一主机和会话可出现在多个列表中。运行状态统一维护，行记录只保留
  // 原生状态观测，不能用已显示的置顶标记恢复上一轮任务。
  const threadRunningStateByCacheKey = new Map();
  const threadRunningRecheckTimers = new Map();
  const projectRunningRecoveryClickedAt = new WeakMap();
  const threadUpdatedAtRequestedAt = new Map();
  const pendingThreadUpdatedAtRefs = new Map();
  const threadUpdatedAtRows = new Set();
  const hardDeletedMessageKeys = new Set();
  const messageSelectButtons = new WeakMap();
  const messageLogicalAnchorByRow = new WeakMap();
  const messageLogicalRowsByAnchor = new WeakMap();
  const messageLogicalGroupIdsByKey = new Map();
  const selectedLogicalMessageIdsByKey = new Map();
  const conversationTurnSelector = [
    "[data-turn-key]",
    "[data-message-author-role]",
    "[data-testid=conversation-turn]",
    "[data-message-id]",
  ].join(", ");
  const canonicalConversationTurnSelector = "[data-turn-key]";
  const sessionIdentityAttributes = [
    "data-above-composer-conversation-id",
    "data-session-id",
    "data-conversation-id",
    "data-thread-id",
    "data-request-user-input-auto-resolution-conversation-id",
    "data-response-annotation-conversation",
  ];
  const sessionIdentitySelector = sessionIdentityAttributes
    .map((attribute) => `[${attribute}]`)
    .join(", ");
  const sidebarScanRootSelector = [
    "header",
    "nav",
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-app-action-sidebar-project-list-id]",
  ].join(", ");
  const sidebarThreadRowSelector = "[data-app-action-sidebar-thread-row]";
  const sidebarProjectListSelector = "[data-app-action-sidebar-project-list-id]";
  const sidebarProjectShowAllAttribute = "data-app-action-sidebar-project-show-all";
  const taskListSectionHeadings = new Set(["task", "tasks", "recent", "recents", "任务", "最近"]);
  const threadRunningLossGraceMs = 2_000;
  const threadTimestampRefreshIntervalMs = 60_000;
  // 桥接应答丢失（如注入重试期间桥接被重装）时不能永久占住 inFlight，
  // 一次超时置失败后按常规周期重试即可。
  const threadTimestampFetchTimeoutMs = 10_000;
  const completedTaskReconcileIntervalMs = 15_000;
  // getSessionId() reads layout (getClientRects); calling it inside every
  // mutation batch forced a synchronous reflow per streamed chunk.
  const completionReconcileMutationDelayMs = 1_000;
  const threadTimestampBridgePath = "/session/timestamps";
  const maxPendingThreadTimestampRefs = 200;
  const fallbackSessionExportMaxBytes = 64 * 1024 * 1024;
  const maxSessionCacheEntries = 2_048;
  const maxHardDeletedMessageKeys = 10_000;
  const maxPendingScanRoots = 64;
  const maxIncrementalScanRootsPerFlush = 8;
  const maxIncrementalScanDurationMs = 8;
  const maxIncrementalMutationRecordsPerBatch = 512;
  const maxIncrementalNodesPerBatch = 256;
  const maxDeferredMutationTasks = 2_048;
  const maxDeferredMutationBatches = 64;
  const maxDeferredMutationNodes = 16_384;
  const maxCriticalMutationProbeNodes = 16;
  const maxAdjacentMessageSelectionRows = 9;
  const maxMessageLogicalGroupKeys = 4_096;
  const maxSelectedLogicalMessageKeys = 2_048;
  const projectRunningRecoveryClickCooldownMs = 1_000;
  const rememberBoundedMapValue = (cache, key, value, limit = maxSessionCacheEntries) => {
    cache.delete(key);
    cache.set(key, value);
    while (cache.size > limit) {
      cache.delete(cache.keys().next().value);
    }
  };
  const rememberBoundedSetValue = (set, value, limit) => {
    set.delete(value);
    set.add(value);
    while (set.size > limit) {
      set.delete(set.values().next().value);
    }
  };
  const queryWithin = (root, selector) => {
    const matches = [];
    if (root instanceof HTMLElement && typeof root.matches === "function" && root.matches(selector)) {
      matches.push(root);
    }
    if (root && typeof root.querySelectorAll === "function") {
      matches.push(...root.querySelectorAll(selector));
    }
    return matches;
  };

  const callBridge = (path, payload = {}, options = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload, options);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };

  const isRenderedSessionElement = (element) => (
    element instanceof HTMLElement
    && element.getClientRects().length > 0
  );

  const visibleSessionElement = (selector) => {
    const first = document.querySelector(selector);
    if (isRenderedSessionElement(first)) return first;
    return [...document.querySelectorAll(selector)].find(isRenderedSessionElement);
  };

  const visibleSessionIdentityValue = () => {
    // Codex keeps inactive conversations mounted. Query the combined selector
    // once, then apply the attribute order only to visible candidates.
    let candidates = [...(document.querySelectorAll?.(sessionIdentitySelector) || [])];
    if (!candidates.length) {
      // A few test DOMs and older renderer shims do not implement compound
      // selector matching. Keep the production fast path and fall back per
      // attribute so a visible session switch still invalidates the interval.
      candidates = sessionIdentityAttributes.flatMap((attribute) => {
        const selector = `[${attribute}]`;
        const first = document.querySelector?.(selector);
        const all = [...(document.querySelectorAll?.(selector) || [])];
        return first && !all.includes(first) ? [first, ...all] : all;
      });
    }
    const visibleCandidates = candidates.filter(isRenderedSessionElement);
    for (const attribute of sessionIdentityAttributes) {
      const value = visibleCandidates
        .find((element) => element.hasAttribute?.(attribute))
        ?.getAttribute(attribute);
      if (value) return String(value).replace(/^local:/, "");
    }
    return "";
  };

  const getSessionId = () => {
    const sessionIdentity = visibleSessionIdentityValue();
    if (sessionIdentity) return sessionIdentity;
    const activeThread = visibleSessionElement('[data-app-action-sidebar-thread-active="true"]')
      ?.getAttribute("data-app-action-sidebar-thread-id");
    if (activeThread) return activeThread.replace(/^local:/, "");
    const match = location.pathname.match(/(?:\/c\/|\/conversation\/|\/session\/)([A-Za-z0-9_-]+)/);
    if (match) return match[1];
    return new URLSearchParams(location.search).get("conversation_id") || new URLSearchParams(location.search).get("session_id") || "";
  };
  const sessionIdentityCandidate = visibleSessionIdentityValue;

  const sidebarTitles = (root = document) => queryWithin(root,
    "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
  ).map((thread) => ({
    sessionId: String(thread.getAttribute("data-app-action-sidebar-thread-id") || "").replace(/^local:/, "").trim(),
    title: String(thread.getAttribute("data-app-action-sidebar-thread-title") || "").trim(),
  })).filter(({ sessionId, title }) => sessionId && title);

  const getSessionTitle = (sessionId) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "");
    return sidebarTitleCache.get(normalizedSessionId)
      || sidebarTitles().find((thread) => thread.sessionId === normalizedSessionId)?.title
      || "";
  };

  const syncSidebarTitles = (root = document) => {
    if (disposed) return;
    const titles = sidebarTitles(root).filter(({ sessionId, title }) => (
      sidebarTitleCache.get(sessionId) !== title
    ));
    if (!titles.length) return;
    const previousTitles = titles.map(({ sessionId }) => (
      [sessionId, sidebarTitleCache.get(sessionId)]
    ));
    titles.forEach(({ sessionId, title }) => (
      rememberBoundedMapValue(sidebarTitleCache, sessionId, title)
    ));
    void callBridge("/session/titles", { titles })
      .then((result) => {
        if (disposed) return;
        if (result?.status !== "failed") return;
        previousTitles.forEach(([sessionId, previousTitle], index) => {
          if (sidebarTitleCache.get(sessionId) !== titles[index].title) return;
          if (previousTitle === undefined) sidebarTitleCache.delete(sessionId);
          else rememberBoundedMapValue(sidebarTitleCache, sessionId, previousTitle);
        });
      })
      .catch(() => {
        if (disposed) return;
        previousTitles.forEach(([sessionId, previousTitle], index) => {
          if (sidebarTitleCache.get(sessionId) !== titles[index].title) return;
          if (previousTitle === undefined) sidebarTitleCache.delete(sessionId);
          else rememberBoundedMapValue(sidebarTitleCache, sessionId, previousTitle);
        });
      });
  };

  const wakeSessionWatcher = () => {
    if (disposed || document.visibilityState === "hidden" || watcherWakeTimer) return;
    void callBridge("/session/wake-watcher").catch(() => {});
    watcherWakeTimer = window.setTimeout(() => {
      watcherWakeTimer = 0;
    }, 30_000);
  };

  const wakeSessionWatcherFromKey = (event) => {
    if (event.key === "Enter" && !event.isComposing) wakeSessionWatcher();
  };

  const normalizeMessageId = (value) => {
    const normalized = String(value || "").trim();
    const turnMarker = ":turn:";
    const markerIndex = normalized.lastIndexOf(turnMarker);
    return markerIndex >= 0
      ? normalized.slice(markerIndex + turnMarker.length).trim()
      : normalized;
  };

  const usableMessageId = (value) => {
    const normalized = normalizeMessageId(value);
    if (!normalized || normalized === "conversation-turn") return "";
    return normalized;
  };

  const isHistoryTailMessageId = (value) => (
    /(?:^|:)tail(?::|$)/i.test(String(value || ""))
  );

  const reactStateKeys = (element) => Object.keys(element).filter((key) => (
    key.startsWith("__reactFiber")
    || key.startsWith("__reactInternalInstance")
    || key.startsWith("__reactProps")
  ));

  const messageIdFromReactState = (element, stableTurnOnly = false) => {
    const visited = new WeakSet();
    const stack = reactStateKeys(element).map((key) => ({
      value: element[key],
      depth: 0,
      path: key.toLowerCase(),
    }));
    const candidates = [];
    const addCandidate = (value, score, turnCandidate = false) => {
      const messageId = usableMessageId(value);
      if (
        stableTurnOnly
        && (!turnCandidate || isHistoryTailMessageId(messageId))
      ) return;
      if (messageId) candidates.push({ messageId, score });
    };
    let scanned = 0;
    while (stack.length && candidates.length < 32 && scanned < 240) {
      const { value, depth, path } = stack.pop();
      if (!value || typeof value !== "object" || visited.has(value) || depth > 7) continue;
      if (value instanceof HTMLElement || value === window || value === document) continue;
      visited.add(value);
      scanned += 1;
      for (const key of Object.keys(value).slice(0, 80)) {
        let child;
        try {
          child = value[key];
        } catch {
          continue;
        }
        const loweredKey = key.toLowerCase();
        const nextPath = `${path}.${loweredKey}`;
        const keyLooksLikeTurnId = /^(turnkey|turn_key|turnid|turn_id)$/.test(loweredKey);
        const keyLooksLikeMessageId = /^(messageid|message_id|itemid|item_id)$/.test(loweredKey);
        const genericIdScore = path.includes("turn")
          ? 4
          : /(message|item)/.test(path)
            ? 2
            : /(response|entry)/.test(path)
              ? 1
              : 0;
        if (typeof child === "string" || typeof child === "number") {
          if (keyLooksLikeTurnId) addCandidate(child, 5, true);
          else if (keyLooksLikeMessageId) addCandidate(child, 3);
          else if (loweredKey === "id" && genericIdScore) {
            addCandidate(child, genericIdScore, path.includes("turn"));
          } else if (loweredKey === "key" && genericIdScore) {
            addCandidate(child, genericIdScore, path.includes("turn"));
          }
          continue;
        }
        if (child && typeof child === "object") {
          stack.push({ value: child, depth: depth + 1, path: nextPath });
        }
      }
    }
    candidates.sort((left, right) => right.score - left.score);
    return candidates[0]?.messageId || "";
  };

  const normalizedTurnStatus = (value) => String(value || "")
    .trim()
    .replace(/[_\s-]+/g, "")
    .toLowerCase();

  const continuationReferenceFromReactTurn = (entry, turn) => {
    const fields = [
      "resumedFromTurnId",
      "resumed_from_turn_id",
      "continuedFromTurnId",
      "continued_from_turn_id",
      "continuationOfTurnId",
      "continuation_of_turn_id",
    ];
    for (const source of [turn, entry]) {
      if (!source || typeof source !== "object") continue;
      for (const field of fields) {
        if (!Object.prototype.hasOwnProperty.call(source, field)) continue;
        return {
          continuationFromMessageId: usableMessageId(source[field]),
          hasContinuationReference: true,
        };
      }
    }
    return {
      continuationFromMessageId: "",
      hasContinuationReference: false,
    };
  };

  const metadataFromReactTurnEntry = (entry, expectedMessageId) => {
    if (!entry || typeof entry !== "object") return null;
    const turn = entry.turn;
    if (!turn || typeof turn !== "object") return null;
    const candidateMessageId = usableMessageId(
      entry.turnId
      || entry.turnKey
      || turn.turnId
      || turn.turnKey
      || turn.id
      || "",
    );
    if (candidateMessageId !== expectedMessageId) return null;
    const items = Array.isArray(turn.items) ? turn.items : null;
    const status = normalizedTurnStatus(turn.status || entry.status);
    if (!items && !status) return null;
    const continuation = continuationReferenceFromReactTurn(entry, turn);
    return {
      ...continuation,
      hasUserMessage: items?.length
        ? items.some((item) => item?.type === "userMessage")
        : null,
      status,
    };
  };

  const messageTurnMetadataFromReactState = (element, messageId) => {
    const expectedMessageId = usableMessageId(messageId);
    if (!expectedMessageId) {
      return {
        continuationFromMessageId: "",
        hasContinuationReference: false,
        hasUserMessage: null,
        status: "",
      };
    }
    for (const key of reactStateKeys(element)) {
      const reactState = element[key];
      const directEntries = [
        reactState?.pendingProps?.children?.props?.entry,
        reactState?.memoizedProps?.children?.props?.entry,
        reactState?.children?.props?.entry,
        reactState?.pendingProps?.entry,
        reactState?.memoizedProps?.entry,
        reactState?.entry,
      ];
      for (const entry of directEntries) {
        const metadata = metadataFromReactTurnEntry(entry, expectedMessageId);
        if (metadata) return metadata;
      }
    }
    const visited = new WeakSet();
    const stack = reactStateKeys(element).map((key) => ({
      value: element[key],
      depth: 0,
      path: key.toLowerCase(),
    }));
    const candidates = [];
    let scanned = 0;
    while (stack.length && candidates.length < 16 && scanned < 240) {
      const { value, depth, path } = stack.pop();
      if (!value || typeof value !== "object" || visited.has(value) || depth > 7) continue;
      if (value instanceof HTMLElement || value === window || value === document) continue;
      visited.add(value);
      scanned += 1;
      const metadata = metadataFromReactTurnEntry(value, expectedMessageId);
      if (metadata) {
        const score = path.includes("pendingprops")
          ? 3
          : path.includes("__reactprops")
            ? 2
            : path.includes("memoizedprops")
              ? 1
              : 0;
        candidates.push({ ...metadata, score });
      }
      for (const key of Object.keys(value).slice(0, 80)) {
        let child;
        try {
          child = value[key];
        } catch {
          continue;
        }
        if (child && typeof child === "object") {
          stack.push({
            value: child,
            depth: depth + 1,
            path: `${path}.${key.toLowerCase()}`,
          });
        }
      }
    }
    candidates.sort((left, right) => right.score - left.score);
    return candidates[0] || {
      continuationFromMessageId: "",
      hasContinuationReference: false,
      hasUserMessage: null,
      status: "",
    };
  };

  const messageTurnMetadata = (row, messageId) => {
    const metadata = messageTurnMetadataFromReactState(row, messageId);
    if (metadata.hasUserMessage !== null) return metadata;
    const hasUserMessage = row.matches?.('[data-message-author-role="user"]')
      || Boolean(row.querySelector?.('[data-message-author-role="user"]'));
    return hasUserMessage ? { ...metadata, hasUserMessage: true } : metadata;
  };

  const preferStableReactTurnId = (row, messageId) => (
    isHistoryTailMessageId(messageId)
      ? messageIdFromReactState(row, true) || messageId
      : messageId
  );

  const getMessageId = (row) => {
    const direct = ["data-turn-key", "data-message-id", "data-messageid", "data-item-id", "data-id"]
      .map((key) => row.getAttribute(key)).find(Boolean);
    if (direct) return preferStableReactTurnId(row, usableMessageId(direct));
    const child = row.querySelector("[data-turn-key], [data-message-id], [data-item-id], [data-id]");
    const childMessageId = usableMessageId(
      child?.getAttribute("data-turn-key")
      || child?.getAttribute("data-message-id")
      || child?.getAttribute("data-item-id")
      || child?.getAttribute("data-id")
      || "",
    );
    return childMessageId
      ? preferStableReactTurnId(row, childMessageId)
      : messageIdFromReactState(row);
  };

  const hardDeletedMessageKey = (sessionId, messageId) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    const normalizedMessageId = normalizeMessageId(messageId);
    return normalizedSessionId && normalizedMessageId
      ? `${normalizedSessionId}\u0000${normalizedMessageId}`
      : "";
  };

  const cachedLogicalMessageIds = (sessionId, messageId) => {
    const key = hardDeletedMessageKey(sessionId, messageId);
    const cached = key ? messageLogicalGroupIdsByKey.get(key) : null;
    if (!Array.isArray(cached) || !cached.includes(normalizeMessageId(messageId))) return [];
    return [...cached];
  };

  const forgetLogicalMessageIds = (sessionId, messageIds) => {
    const pending = [...new Set(messageIds.map(normalizeMessageId).filter(Boolean))];
    const visited = new Set();
    while (pending.length) {
      const messageId = pending.pop();
      if (!messageId || visited.has(messageId)) continue;
      visited.add(messageId);
      const key = hardDeletedMessageKey(sessionId, messageId);
      const cached = key ? messageLogicalGroupIdsByKey.get(key) : null;
      if (key) messageLogicalGroupIdsByKey.delete(key);
      if (Array.isArray(cached)) {
        cached.forEach((cachedMessageId) => {
          const normalizedMessageId = normalizeMessageId(cachedMessageId);
          if (normalizedMessageId && !visited.has(normalizedMessageId)) {
            pending.push(normalizedMessageId);
          }
        });
      }
    }
  };

  const rememberLogicalMessageIds = (sessionId, messageIds) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    const normalizedMessageIds = [...new Set(messageIds.map(normalizeMessageId).filter(Boolean))];
    if (!normalizedSessionId || normalizedMessageIds.length < 2) return normalizedMessageIds;
    forgetLogicalMessageIds(normalizedSessionId, normalizedMessageIds);
    const cachedGroup = Object.freeze([...normalizedMessageIds]);
    normalizedMessageIds.forEach((messageId) => {
      const key = hardDeletedMessageKey(normalizedSessionId, messageId);
      if (!key) return;
      messageLogicalGroupIdsByKey.delete(key);
      messageLogicalGroupIdsByKey.set(key, cachedGroup);
    });
    while (messageLogicalGroupIdsByKey.size > maxMessageLogicalGroupKeys) {
      const oldestKey = messageLogicalGroupIdsByKey.keys().next().value;
      const oldestGroup = messageLogicalGroupIdsByKey.get(oldestKey);
      const separatorIndex = String(oldestKey || "").indexOf("\u0000");
      const oldestSessionId = separatorIndex > 0 ? oldestKey.slice(0, separatorIndex) : "";
      if (oldestSessionId && Array.isArray(oldestGroup)) {
        oldestGroup.forEach((messageId) => {
          messageLogicalGroupIdsByKey.delete(hardDeletedMessageKey(oldestSessionId, messageId));
        });
      } else {
        messageLogicalGroupIdsByKey.delete(oldestKey);
      }
    }
    return normalizedMessageIds;
  };

  const logicalMessageSelectionGroups = (sessionId) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    if (!normalizedSessionId) return [];
    const prefix = `${normalizedSessionId}\u0000`;
    return [...selectedLogicalMessageIdsByKey.entries()]
      .filter(([key]) => key.startsWith(prefix))
      .map(([, messageIds]) => [...messageIds]);
  };

  const intersectingLogicalMessageSelections = (sessionId, messageIds) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    const memberIds = new Set(messageIds.map(normalizeMessageId).filter(Boolean));
    if (!normalizedSessionId || !memberIds.size) return [];
    const prefix = `${normalizedSessionId}\u0000`;
    return [...selectedLogicalMessageIdsByKey.entries()].filter(([key, selectedIds]) => (
      key.startsWith(prefix)
      && selectedIds.some((messageId) => memberIds.has(messageId))
    ));
  };

  const selectedLogicalMessageIdsForMember = (sessionId, messageId) => {
    const selections = intersectingLogicalMessageSelections(sessionId, [messageId]);
    if (!selections.length) return [];
    selections.sort((left, right) => right[1].length - left[1].length);
    return [...selections[0][1]];
  };

  const knownLogicalMessageIds = (sessionId, messageId) => {
    const cachedIds = cachedLogicalMessageIds(sessionId, messageId);
    if (cachedIds.length > 1) return cachedIds;
    const selectedIds = selectedLogicalMessageIdsForMember(sessionId, messageId);
    return selectedIds.length > 1 ? selectedIds : [];
  };

  const logicalMessageGroupIsSelected = (sessionId, messageIds) => (
    intersectingLogicalMessageSelections(sessionId, messageIds).length > 0
  );

  const rememberLogicalMessageSelection = (sessionId, messageIds, selected) => {
    const normalizedMessageIds = [...new Set(messageIds.map(normalizeMessageId).filter(Boolean))];
    const key = hardDeletedMessageKey(sessionId, normalizedMessageIds[0]);
    if (!key) return;
    intersectingLogicalMessageSelections(sessionId, normalizedMessageIds)
      .forEach(([selectedKey]) => selectedLogicalMessageIdsByKey.delete(selectedKey));
    if (!selected) {
      return;
    }
    rememberBoundedMapValue(
      selectedLogicalMessageIdsByKey,
      key,
      Object.freeze([...normalizedMessageIds]),
      maxSelectedLogicalMessageKeys,
    );
  };

  const forgetLogicalMessageSelections = (sessionId, messageIds) => {
    intersectingLogicalMessageSelections(sessionId, messageIds)
      .forEach(([key]) => selectedLogicalMessageIdsByKey.delete(key));
  };

  const clearLogicalMessageSelections = (sessionId) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    if (!normalizedSessionId) return;
    const prefix = `${normalizedSessionId}\u0000`;
    for (const key of selectedLogicalMessageIdsByKey.keys()) {
      if (key.startsWith(prefix)) selectedLogicalMessageIdsByKey.delete(key);
    }
  };

  const replaceLogicalMessageId = (sessionId, previousMessageId, messageId) => {
    const previousId = normalizeMessageId(previousMessageId);
    const nextId = normalizeMessageId(messageId);
    if (!previousId || !nextId || previousId === nextId) return;
    const cachedIds = cachedLogicalMessageIds(sessionId, previousId);
    if (cachedIds.length > 1) {
      const replacedIds = [...new Set(cachedIds.map((candidate) => (
        candidate === previousId ? nextId : candidate
      )))];
      forgetLogicalMessageIds(sessionId, cachedIds);
      rememberLogicalMessageIds(sessionId, replacedIds);
    }
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    const prefix = normalizedSessionId ? `${normalizedSessionId}\u0000` : "";
    if (!prefix) return;
    for (const [key, selectedIds] of [...selectedLogicalMessageIdsByKey.entries()]) {
      if (!key.startsWith(prefix) || !selectedIds.includes(previousId)) continue;
      selectedLogicalMessageIdsByKey.delete(key);
      const replacedIds = [...new Set(selectedIds.map((candidate) => (
        candidate === previousId ? nextId : candidate
      )))];
      rememberLogicalMessageSelection(sessionId, replacedIds, true);
    }
  };

  const invalidateLogicalMessageGroup = (sessionId, messageIds) => {
    const normalizedMessageIds = [...new Set(messageIds.map(normalizeMessageId).filter(Boolean))];
    if (logicalMessageGroupIsSelected(sessionId, normalizedMessageIds)) {
      rememberLogicalMessageSelection(sessionId, normalizedMessageIds.slice(0, 1), true);
    }
    forgetLogicalMessageIds(sessionId, normalizedMessageIds);
  };

  const rememberHardDeletedMessages = (sessionId, messageIds) => {
    messageIds.forEach((messageId) => {
      const key = hardDeletedMessageKey(sessionId, messageId);
      if (key) {
        rememberBoundedSetValue(
          hardDeletedMessageKeys,
          key,
          maxHardDeletedMessageKeys,
        );
      }
    });
  };

  const isHardDeletedMessage = (sessionId, messageId) => (
    hardDeletedMessageKeys.has(hardDeletedMessageKey(sessionId, messageId))
  );

  const addStyle = () => {
    if (document.getElementById(styleId)) return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      #${toolbarId} { -webkit-app-region: no-drag !important; position: fixed; right: 18px; top: 60px; z-index: 2147483644; display: flex; align-items: center; gap: 7px; padding: 6px 8px; border: 1px solid rgba(124, 140, 255, .44); border-radius: 999px; background: rgba(20, 24, 36, .68); color: rgba(238, 242, 255, .94); box-shadow: 0 8px 24px rgba(0,0,0,.18); backdrop-filter: blur(10px); font: 12px/1 system-ui, sans-serif; }
      #${toolbarId}[hidden] { display: none; }
      #${toolbarId} button { border: 1px solid rgba(120, 140, 180, .34); border-radius: 999px; padding: 4px 8px; background: rgba(40, 50, 70, .48); color: inherit; cursor: pointer; font: 12px/1 system-ui, sans-serif; }
      #${toolbarId} button[data-danger] { border-color: rgba(248, 113, 113, .68); background: rgba(185, 28, 28, .42); color: #fff1f2; font-weight: 650; }
      .${selectedClass} { border-radius: 18px; box-sizing: border-box !important; outline: none !important; }
      .${selectedClass}::before { content: ""; position: absolute; inset: -12px; z-index: 29; box-sizing: border-box; border: 3px solid #7c8cff; border-radius: 18px; pointer-events: none; }
      .${selectedClass}[data-codey-selected-previous="true"]::before { border-top: 0; border-top-left-radius: 0; border-top-right-radius: 0; }
      .${selectedClass}[data-codey-selected-next="true"]::before { border-bottom: 0; border-bottom-left-radius: 0; border-bottom-right-radius: 0; }
      [data-codey-message-id] { overflow: visible !important; }
      [data-codey-message-select] { -webkit-app-region: no-drag !important; position: absolute; left: -48px; top: 8px; z-index: 30; display: grid; place-items: center; width: 24px; height: 24px; border: 1px solid rgba(139, 151, 255, .42); border-radius: 999px; padding: 0; background: rgba(22, 26, 39, .66); color: #dce2ff; cursor: pointer; font: 700 13px/1 system-ui, sans-serif; opacity: .24; pointer-events: auto !important; transition: opacity .15s ease, background .15s ease, transform .15s ease; }
      [data-codey-message-row]:hover > [data-codey-message-select], [data-codey-message-select]:focus-visible, [data-codey-message-select][aria-pressed="true"] { opacity: 1; }
      [data-codey-message-select]:hover { transform: scale(1.06); }
      [data-codey-message-select][aria-pressed="true"] { background: #5968de; border-color: #a5aeff; color: white; }
      @media (max-width: 760px) { [data-codey-message-select] { left: 4px; top: -34px; } }
      #${toastId} { -webkit-app-region: no-drag !important; position: fixed; right: 20px; bottom: 22px; z-index: 2147483645; max-width: 360px; border: 1px solid rgba(124, 140, 255, .4); border-radius: 11px; padding: 10px 13px; background: rgba(20, 24, 36, .97); color: #eef2ff; box-shadow: 0 12px 36px rgba(0,0,0,.4); font: 12px/1.45 system-ui, sans-serif; }
      #${toastId}[data-tone="error"] { border-color: rgba(248, 113, 113, .6); color: #fecaca; }
      [data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title],
      [data-app-action-sidebar-project-row][data-app-action-sidebar-project-id] { position: relative; }
      :where([data-codey-message-row]) { position: relative; }
      [data-app-action-sidebar-thread-row] [${threadUpdatedAtAttribute}] { display: block; flex: 0 0 auto; min-width: 26px; margin-inline-start: auto; color: inherit; font: 400 12px/16px system-ui, sans-serif; font-variant-numeric: tabular-nums; letter-spacing: 0; text-align: end; opacity: .52; pointer-events: none; white-space: nowrap; }
      [data-app-action-sidebar-thread-row]:hover [${threadUpdatedAtAttribute}],
      [data-app-action-sidebar-thread-row]:has(:focus-visible) [${threadUpdatedAtAttribute}] { opacity: 0; }
      [role="list"] > [${threadRunningAttribute}="true"],
      [data-app-action-sidebar-project-list-id] > [${threadRunningAttribute}="true"] { order: -1 !important; }
      [${sessionExportAttribute}], [${tasksImportAttribute}], [${sessionDeleteAttribute}] { -webkit-app-region: no-drag !important; flex: 0 0 auto; pointer-events: auto !important; }
      [${projectImportAttribute}] { -webkit-app-region: no-drag !important; position: absolute; top: 50%; right: 62px; z-index: 35; flex: 0 0 auto; transform: translateY(-50%); opacity: 0; pointer-events: auto !important; transition: opacity .15s ease; }
      [data-app-action-sidebar-project-row][data-app-action-sidebar-project-id]:hover > [${projectImportAttribute}],
      [${projectImportAttribute}]:focus-visible,
      [${projectImportAttribute}][data-busy="true"] { opacity: .9; }
      [${projectImportAttribute}]:hover { opacity: 1 !important; }
      [data-codey-session-action-row] { display: inline-flex !important; align-items: center !important; flex: 0 0 auto !important; flex-flow: row nowrap !important; gap: 1px !important; width: auto !important; min-width: max-content !important; white-space: nowrap !important; }
      #${sidebarActionTooltipId} { position: fixed; z-index: 2147483647; max-width: min(20rem, calc(100vw - 16px)); pointer-events: none; }
      [data-codey-pet-control-blocked="true"] { display: none !important; pointer-events: none !important; }
    `;
    document.documentElement.appendChild(style);
  };

  const logicalAnchorForMessageRow = (row) => messageLogicalAnchorByRow.get(row) || row;

  const logicalRowsForMessageRow = (row) => {
    const anchor = logicalAnchorForMessageRow(row);
    const groupedRows = messageLogicalRowsByAnchor.get(anchor);
    return Array.isArray(groupedRows) && groupedRows.length ? groupedRows : [anchor];
  };

  const logicalMessageIdsForRow = (row) => {
    const anchor = logicalAnchorForMessageRow(row);
    let storedIds = [];
    try {
      const parsed = JSON.parse(anchor.dataset.codeyMessageIds || "[]");
      if (Array.isArray(parsed)) storedIds = parsed;
    } catch {
      storedIds = [];
    }
    const candidateIds = storedIds.length
      ? storedIds
      : logicalRowsForMessageRow(anchor).map((item) => item.dataset.codeyMessageId);
    return [...new Set(candidateIds.map(normalizeMessageId).filter(Boolean))];
  };

  const logicalSelectionRows = () => [...document.querySelectorAll("[data-codey-message-id]")]
    .filter(isRenderedSessionElement)
    .filter((row) => row.dataset.codeyLogicalTurn !== "continuation");

  const selectedRows = () => logicalSelectionRows()
    .filter((row) => row.classList.contains(selectedClass));

  const showRuntimeToast = (message, tone = "success") => {
    const sharedToast = window.__codeyShowRuntimeToast;
    if (typeof sharedToast === "function" && sharedToast !== showRuntimeToast) {
      sharedToast(message, tone);
      return;
    }
    if (disposed) return;
    document.getElementById(toastId)?.remove();
    const toast = document.createElement("div");
    toast.id = toastId;
    toast.dataset.tone = tone;
    toast.setAttribute("role", tone === "error" ? "alert" : "status");
    toast.setAttribute("aria-live", tone === "error" ? "assertive" : "polite");
    toast.textContent = message;
    document.documentElement.appendChild(toast);
    window.setTimeout(() => toast.remove(), tone === "error" ? 8000 : 3500);
  };
  if (typeof window.__codeyShowRuntimeToast !== "function") {
    window.__codeyShowRuntimeToast = showRuntimeToast;
  }

  const stopSidebarActionEvent = (event) => {
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation?.();
  };

  // Codex 原生按钮的尺寸与前景色由 data-* 属性驱动（data-size 决定 --button-icon-size，
  // data-color/data-variant 决定次级前景色），只复制 class 会让注入按钮退回默认值、
  // 图标偏大且是纯黑，所以按参考按钮逐项复制属性，并补上内层容器。
  const inheritNativeButtonClass = (button, reference) => {
    if (!(reference instanceof HTMLElement)) return;
    const className = String(reference.getAttribute("class") || "").trim();
    if (className) button.setAttribute("class", className);
    for (const name of ["data-color", "data-variant", "data-pill", "data-uniform", "data-size", "data-icon-size"]) {
      const value = reference.getAttribute(name);
      if (value === null) button.removeAttribute(name);
      else button.setAttribute(name, value);
    }
  };

  // 原生图标统一放在 _ButtonInner 内：它负责 flex 居中，并把图标压到
  // --button-icon-size（3xs 按钮即 14px）。
  const wrapNativeButtonIcon = (button, reference) => {
    const icon = button.querySelector("svg");
    if (!icon) return;
    const template = reference instanceof HTMLElement
      ? reference.querySelector("span")
      : null;
    const inner = template instanceof HTMLElement
      ? document.createElement(template.tagName.toLowerCase())
      : document.createElement("span");
    if (template instanceof HTMLElement) {
      const innerClass = String(template.getAttribute("class") || "").trim();
      if (innerClass) inner.setAttribute("class", innerClass);
    } else {
      inner.className = "_ButtonInner_f5tnh_2";
    }
    inner.appendChild(icon);
    button.appendChild(inner);
  };

  const hideSidebarActionTooltip = () => {
    if (sidebarActionTooltipTimer) {
      window.clearTimeout(sidebarActionTooltipTimer);
      sidebarActionTooltipTimer = 0;
    }
    document.getElementById(sidebarActionTooltipId)?.remove();
    if (sidebarActionTooltipAnchor?.getAttribute("aria-describedby") === sidebarActionTooltipId) {
      sidebarActionTooltipAnchor.removeAttribute("aria-describedby");
    }
    sidebarActionTooltipAnchor = null;
  };

  const scheduleSidebarActionTooltip = (button, label, delay) => {
    if (disposed) return;
    hideSidebarActionTooltip();
    sidebarActionTooltipAnchor = button;
    sidebarActionTooltipTimer = window.setTimeout(() => {
      sidebarActionTooltipTimer = 0;
      if (disposed || sidebarActionTooltipAnchor !== button) return;
      if (button.isConnected === false || button.getClientRects().length === 0) {
        hideSidebarActionTooltip();
        return;
      }
      const tooltip = document.createElement("div");
      tooltip.setAttribute("id", sidebarActionTooltipId);
      tooltip.setAttribute("role", "tooltip");
      tooltip.setAttribute("data-side", "top");
      tooltip.setAttribute(
        "class",
        "z-50 w-fit select-none text-sm whitespace-normal break-words rounded-lg border border-token-border bg-token-dropdown-background text-token-foreground px-2 py-1",
      );
      const row = document.createElement("div");
      row.setAttribute("class", "flex items-center gap-2");
      const text = document.createElement("div");
      text.setAttribute("class", "min-w-0");
      text.textContent = label;
      row.appendChild(text);
      tooltip.appendChild(row);
      document.body.appendChild(tooltip);

      const anchorRect = button.getBoundingClientRect();
      const tooltipRect = tooltip.getBoundingClientRect();
      const viewportWidth = window.innerWidth || document.documentElement.clientWidth || 1024;
      const viewportHeight = window.innerHeight || document.documentElement.clientHeight || 768;
      const left = Math.min(
        viewportWidth - tooltipRect.width - 8,
        Math.max(8, anchorRect.left + ((anchorRect.width - tooltipRect.width) / 2)),
      );
      const topAbove = anchorRect.top - tooltipRect.height - 8;
      const placeAbove = topAbove >= 8;
      const top = placeAbove
        ? topAbove
        : Math.min(viewportHeight - tooltipRect.height - 8, anchorRect.bottom + 8);
      tooltip.setAttribute("data-side", placeAbove ? "top" : "bottom");
      tooltip.style.left = `${left}px`;
      tooltip.style.top = `${Math.max(8, top)}px`;
      button.setAttribute("aria-describedby", sidebarActionTooltipId);
    }, delay);
  };

  const attachSidebarActionTooltip = (button, label) => {
    // Existing action buttons survive reinjection. Dispatch their interactions
    // through the current installation instead of reviving a disposed timer.
    const show = (delay) => window.__codeySessionToolsInstall?.tooltip?.show(button, label, delay);
    const hide = () => window.__codeySessionToolsInstall?.tooltip?.hide(button);
    const hideAll = () => window.__codeySessionToolsInstall?.tooltip?.hide();
    button.addEventListener("mouseenter", () => {
      show(400);
    });
    button.addEventListener("mouseleave", hide);
    button.addEventListener("focus", () => {
      show(0);
    });
    button.addEventListener("blur", hide);
    button.addEventListener("pointerdown", hideAll);
    button.addEventListener("click", hideAll);
  };

  const encodeBase64Bytes = (bytes) => {
    let binary = "";
    for (let offset = 0; offset < bytes.length; offset += 0x8000) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
    }
    return btoa(binary);
  };

  const decodeBase64Bytes = (encoded) => {
    const binary = atob(String(encoded || ""));
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  };

  const downloadSessionFallback = (filename, chunks) => {
    const blob = new Blob(chunks, { type: "application/json;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  const openSessionExportWriter = async (filename) => {
    if (typeof window.showSaveFilePicker !== "function") return null;
    const handle = await window.showSaveFilePicker({
      suggestedName: filename,
      types: [{
        description: "Codey 会话数据",
        accept: { "application/json": [".json"] },
      }],
    });
    return handle.createWritable();
  };

  const exportSession = async (thread, button) => {
    const sessionId = threadSessionIdFromRow(thread);
    if (!sessionId || sessionId.startsWith("client-new-thread:")) {
      showRuntimeToast("导出失败：无法识别会话 ID", "error");
      return;
    }
    button.disabled = true;
    button.dataset.busy = "true";
    let transferId = "";
    let writable = null;
    try {
      const start = await callBridge("/session/export/start", { sessionId });
      if (start?.status === "failed") {
        throw new Error(start.message || "未知错误");
      }
      if (start?.status !== "ready" || !start.transferId || !start.filename) {
        throw new Error("导出准备结果不完整");
      }
      transferId = start.transferId;
      try {
        writable = await openSessionExportWriter(start.filename);
      } catch (error) {
        if (error?.name === "AbortError") return;
        throw error;
      }
      const exportSize = Number(start.size);
      if (!Number.isSafeInteger(exportSize) || exportSize < 0) {
        throw new Error("导出文件大小无效");
      }
      if (!writable && exportSize > fallbackSessionExportMaxBytes) {
        throw new Error("当前环境不支持大文件流式保存，请升级 Codex 后重试");
      }

      const fallbackChunks = [];
      let offset = 0;
      while (true) {
        const chunk = await callBridge("/session/export/chunk", {
          transferId,
          offset,
        });
        if (chunk?.status === "failed") {
          throw new Error(chunk.message || "读取导出分块失败");
        }
        if (chunk?.status !== "ok" || chunk.offset !== offset || typeof chunk.data !== "string") {
          throw new Error("导出分块结果不完整");
        }
        const bytes = decodeBase64Bytes(chunk.data);
        if (writable) await writable.write(bytes);
        else fallbackChunks.push(bytes);
        const nextOffset = Number(chunk.nextOffset);
        if (
          !Number.isSafeInteger(nextOffset)
          || nextOffset !== offset + bytes.length
          || nextOffset > exportSize
          || Boolean(chunk.done) !== (nextOffset === exportSize)
        ) {
          throw new Error("导出分块偏移无效");
        }
        offset = nextOffset;
        if (chunk.done) break;
      }
      if (writable) {
        await writable.close();
        writable = null;
      } else {
        downloadSessionFallback(start.filename, fallbackChunks);
      }
      const finish = await callBridge("/session/export/finish", { transferId });
      if (finish?.status !== "ok") {
        throw new Error(finish?.message || "清理导出临时文件失败");
      }
      transferId = "";
      showRuntimeToast(`已导出会话：${start.filename}`);
    } catch (error) {
      try {
        await writable?.abort?.();
      } catch {}
      showRuntimeToast(`导出失败：${error instanceof Error ? error.message : String(error)}`, "error");
    } finally {
      if (transferId) {
        void callBridge("/session/export/abort", { transferId }).catch(() => {});
      }
      button.disabled = false;
      delete button.dataset.busy;
    }
  };

  const installSessionExportButtons = (root = document) => {
    queryWithin(root,
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    ).forEach((thread) => {
      if (
        !(thread instanceof HTMLElement)
        || thread.querySelector(`[${sessionExportAttribute}]`)
      ) return;
      const sessionId = String(thread.getAttribute("data-app-action-sidebar-thread-id") || "").trim();
      if (!sessionId) return;
      const archiveControl = findArchiveControl(thread);
      if (!(archiveControl instanceof HTMLElement)) return;
      const placementTarget = archivePlacementTarget(thread, archiveControl);
      if (placementTarget.parentElement instanceof HTMLElement && placementTarget.parentElement !== thread) {
        placementTarget.parentElement.setAttribute("data-codey-session-action-row", "true");
      }
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute(sessionExportAttribute, "true");
      button.setAttribute("aria-label", "导出会话数据");
      inheritNativeButtonClass(button, archiveControl);
      button.innerHTML = sessionExportIcon;
      wrapNativeButtonIcon(button, archiveControl);
      attachSidebarActionTooltip(button, "导出会话数据");
      ["pointerdown", "mousedown", "mouseup", "touchstart"].forEach((eventName) => {
        button.addEventListener(eventName, stopSidebarActionEvent, true);
      });
      button.addEventListener("click", (event) => {
        stopSidebarActionEvent(event);
        void exportSession(thread, button);
      }, true);
      placementTarget.insertAdjacentElement("beforebegin", button);
    });
  };

  const installTasksImportButton = (root = document) => {
    queryWithin(root, "[data-app-action-sidebar-section]").forEach((section) => {
      if (!(section instanceof HTMLElement) || section.querySelector(`[${tasksImportAttribute}]`)) return;
      const heading = String(
        section.getAttribute("data-app-action-sidebar-section-heading") || "",
      ).trim().toLowerCase();
      const sectionToggle = section.querySelector("[data-app-action-sidebar-section-toggle]");
      const localizedHeading = String(sectionToggle?.textContent || "").trim().toLowerCase();
      if (!taskListSectionHeadings.has(heading) && !taskListSectionHeadings.has(localizedHeading)) return;
      const titleRow = sectionToggle?.parentElement?.parentElement?.parentElement;
      if (!(titleRow instanceof HTMLElement)) return;
      const headerControls = [...titleRow.querySelectorAll("button, [role=button]")]
        .filter((control) => control instanceof HTMLElement && control !== sectionToggle);
      const optionsControl = headerControls.find((control) => {
        const label = String(control.getAttribute("aria-label") || "").trim();
        return /任务侧边栏选项|聊天侧边栏选项|task sidebar options|chat sidebar options/i.test(label);
      });
      const newTaskControl = headerControls.find((control) => {
        const label = String(control.getAttribute("aria-label") || "").trim();
        return /新建任务|新对话|new task|new chat/i.test(label);
      });
      if (!(optionsControl instanceof HTMLElement) || !(optionsControl.parentElement instanceof HTMLElement)) return;
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute(tasksImportAttribute, "true");
      button.setAttribute("aria-label", "导入会话数据");
      inheritNativeButtonClass(button, newTaskControl || optionsControl);
      button.innerHTML = projectImportIcon;
      wrapNativeButtonIcon(button, newTaskControl || optionsControl);
      attachSidebarActionTooltip(button, "导入会话数据");
      ["pointerdown", "mousedown", "mouseup", "touchstart"].forEach((eventName) => {
        button.addEventListener(eventName, stopSidebarActionEvent, true);
      });
      button.addEventListener("click", (event) => {
        stopSidebarActionEvent(event);
        chooseSessionImportFile("", button);
      }, true);
      optionsControl.insertAdjacentElement("beforebegin", button);
    });
  };

  const isLocalProjectPath = (value) => {
    const path = String(value || "").trim();
    return path.startsWith("/") || path.startsWith("\\\\") || /^[A-Za-z]:[\\/]/.test(path);
  };

  const projectPathFromReactValue = (value, projectId, depth = 0, seen = new WeakSet()) => {
    if (!value || (typeof value !== "object" && typeof value !== "function") || depth > 6) return "";
    if (seen.has(value)) return "";
    seen.add(value);
    const valueProjectId = String(value.projectId || value.id || "");
    if (valueProjectId === projectId) {
      const path = [
        value.path,
        value.rootPaths?.[0],
        value.repoPath,
        value.cwd,
      ].find(isLocalProjectPath);
      if (path) return String(path).trim();
    }
    const priorityKeys = ["group", "groups", "actions", "children", "tooltipContent"];
    const keys = [
      ...priorityKeys.filter((key) => Object.prototype.hasOwnProperty.call(value, key)),
      ...Object.keys(value).filter((key) => !priorityKeys.includes(key)),
    ].slice(0, 120);
    for (const key of keys) {
      if (["return", "child", "sibling", "stateNode", "_owner"].includes(key)) continue;
      let path = "";
      try {
        path = projectPathFromReactValue(value[key], projectId, depth + 1, seen);
      } catch {
        continue;
      }
      if (path) return path;
    }
    return "";
  };

  const projectPathFromRow = (project) => {
    const projectId = String(project.getAttribute("data-app-action-sidebar-project-id") || "").trim();
    if (isLocalProjectPath(projectId)) return projectId;
    const reactKey = Object.keys(project).find((key) => (
      key.startsWith("__reactFiber$") || key.startsWith("__reactInternalInstance$")
    ));
    let fiber = reactKey ? project[reactKey] : null;
    for (let depth = 0; fiber && depth < 18; depth += 1, fiber = fiber.return) {
      const path = projectPathFromReactValue(fiber.memoizedProps, projectId)
        || projectPathFromReactValue(fiber.pendingProps, projectId);
      if (path) return path;
    }
    return "";
  };

  const normalizeThreadSessionId = (value) => (
    String(value || "").trim().replace(/^local:/, "")
  );

  const isCanonicalThreadSessionId = (value) => (
    /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value)
  );

  const canonicalThreadSessionIdFromReactValue = (
    value,
    depth = 0,
    seen = new WeakSet(),
  ) => {
    if (!value || typeof value !== "object" || depth > 5 || seen.has(value)) return "";
    seen.add(value);
    const direct = normalizeThreadSessionId(value.conversationId);
    if (isCanonicalThreadSessionId(direct)) return direct;
    if (Array.isArray(value)) {
      for (const item of value.slice(0, 32)) {
        const nested = canonicalThreadSessionIdFromReactValue(item, depth + 1, seen);
        if (nested) return nested;
      }
      return "";
    }
    for (const key of ["entry", "tooltipContent", "children", "props"]) {
      const nested = canonicalThreadSessionIdFromReactValue(value[key], depth + 1, seen);
      if (nested) return nested;
    }
    return "";
  };

  const threadSessionIdFromRow = (row) => {
    const rowSessionId = normalizeThreadSessionId(
      row.getAttribute("data-app-action-sidebar-thread-id"),
    );
    if (!rowSessionId.startsWith("client-new-thread:")) return rowSessionId;
    const reactKey = Object.keys(row).find((key) => (
      key.startsWith("__reactFiber$") || key.startsWith("__reactInternalInstance$")
    ));
    let fiber = reactKey ? row[reactKey] : null;
    for (let depth = 0; fiber && depth < 18; depth += 1, fiber = fiber.return) {
      const sessionId = canonicalThreadSessionIdFromReactValue(fiber.memoizedProps)
        || canonicalThreadSessionIdFromReactValue(fiber.pendingProps);
      if (sessionId) return sessionId;
    }
    return rowSessionId;
  };

  const threadIdentityNode = (row) => (
    row?.hasAttribute?.("data-app-action-sidebar-thread-id")
      ? row
      : row?.querySelector?.("[data-app-action-sidebar-thread-id]")
  );

  const numericThreadTimestamp = (value) => {
    const timestamp = Number(value);
    return Number.isFinite(timestamp) && timestamp > 0 ? timestamp : 0;
  };

  const threadTimestampValueToMs = (value) => {
    const timestamp = numericThreadTimestamp(value);
    if (!timestamp) return 0;
    return timestamp < 1_000_000_000_000 ? timestamp * 1_000 : timestamp;
  };

  const uuidV7ThreadTimestampMs = (sessionId) => {
    const id = normalizeThreadSessionId(sessionId).replaceAll("-", "");
    if (!/^[0-9a-fA-F]{12}/.test(id)) return 0;
    const timestamp = Number.parseInt(id.slice(0, 12), 16);
    return Number.isFinite(timestamp) && timestamp > 0 ? timestamp : 0;
  };

  const threadTimestampMsFromPayload = (payload) => (
    numericThreadTimestamp(payload?.recency_at_ms ?? payload?.recencyAtMs)
    || threadTimestampValueToMs(payload?.recency_at ?? payload?.recencyAt)
    || numericThreadTimestamp(payload?.updated_at_ms ?? payload?.updatedAtMs)
    || threadTimestampValueToMs(payload?.updated_at ?? payload?.updatedAt)
    || numericThreadTimestamp(payload?.created_at_ms ?? payload?.createdAtMs)
    || threadTimestampValueToMs(payload?.created_at ?? payload?.createdAt)
    || uuidV7ThreadTimestampMs(
      payload?.id ?? payload?.thread_id ?? payload?.threadId
      ?? payload?.conversation_id ?? payload?.conversationId
      ?? payload?.session_id ?? payload?.sessionId,
    )
  );

  const formatRelativeThreadTime = (timestampMs, nowMs = Date.now()) => {
    const timestamp = numericThreadTimestamp(timestampMs);
    if (!timestamp) return "";
    const elapsedSeconds = Math.max(0, Math.floor((nowMs - timestamp) / 1_000));
    if (elapsedSeconds < 60) return "刚刚";
    const minutes = Math.floor(elapsedSeconds / 60);
    if (minutes < 60) return `${minutes} 分`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours} 小时`;
    const days = Math.floor(hours / 24);
    if (days < 7) return `${days} 天`;
    const weeks = Math.floor(days / 7);
    if (weeks < 5) return `${weeks} 周`;
    const months = Math.floor(days / 30);
    if (days < 365) return `${Math.max(1, months)} 月`;
    return `${Math.max(1, Math.floor(days / 365))} 年`;
  };

  const threadUpdatedAtPlacement = (row, label) => {
    const contentRoot = [...(row.children || [])].find((child) => (
      String(child.className || "").includes("h-full w-full items-center")
    ));
    if (contentRoot) {
      const children = [...(contentRoot.children || [])].filter((child) => child !== label);
      const mainContentIndex = children.findIndex((child) => {
        const className = String(child.className || "");
        return className.includes("min-w-0") && className.includes("flex-1");
      });
      const trailing = mainContentIndex >= 0 ? children.slice(mainContentIndex + 1) : [];
      return {
        before: mainContentIndex >= 0 ? children[mainContentIndex + 1] || null : null,
        mount: contentRoot,
        statusRail: trailing[0] || null,
        trailing,
      };
    }
    const titleNode = row.querySelector?.(
      "[data-thread-title], [data-app-action-sidebar-thread-title], .truncate.select-none, .truncate.text-base",
    );
    return { before: null, mount: titleNode?.parentElement || row, statusRail: null, trailing: [] };
  };

  const hasNativeThreadStatus = (row, label) => {
    if (nativeReactThreadStatusVisible(row)) return true;
    const { trailing } = threadUpdatedAtPlacement(row, label);
    const candidates = (trailing || []).filter((child) => (
      child instanceof HTMLElement
      && child !== label
      && !child.hasAttribute?.(threadUpdatedAtAttribute)
    ));
    for (const candidate of candidates) {
      if (nativeElementLooksLikeThreadStatus(candidate)) return true;
    }
    return false;
  };

  const nativeReactThreadStatusState = (row) => {
    if (!(row instanceof HTMLElement)) return null;
    // Codex owns the canonical loading/unread flags even when its status icon
    // is moved or updated without adding a new element to the trailing rail.
    const fiberKey = Object.keys(row).find((key) => key.startsWith("__reactFiber$"));
    let fiber = fiberKey ? row[fiberKey] : null;
    for (let depth = 0; fiber && depth < 12; depth += 1, fiber = fiber.return) {
      const statusState = fiber.memoizedProps?.statusState || fiber.pendingProps?.statusState;
      if (!statusState || typeof statusState !== "object") continue;
      return statusState;
    }
    return null;
  };

  const nativeThreadStatusTypeLooksActive = (value) => (
    /^(?:loading|processing|running|working|streaming|generating|in(?:[_ -]?progress))$/i
      .test(String(value || "").trim())
  );

  const nativeReactThreadStatusVisible = (row) => {
    const statusState = nativeReactThreadStatusState(row);
    if (!statusState) return false;
    return statusState.unread === true || nativeThreadStatusTypeLooksActive(statusState.type);
  };

  const nativeThreadStatusClassPattern = /\b(?:animate-|spinner)\b/i;

  const nativeElementLooksLikeThreadStatus = (element) => {
    if (!(element instanceof HTMLElement)) return false;
    if (element.matches?.("button, [role=button], [role=menuitem]")) return false;
    const statusText = [
      element.textContent || "",
      element.getAttribute?.("aria-label") || "",
      element.getAttribute?.("role") || "",
      element.getAttribute?.("title") || "",
      element.title || "",
    ].join(" ");
    // A completed marker can remain on the active row until another thread is
    // selected. Only active work should temporarily displace the timestamp.
    if (/(running|processing|working|loading|streaming|generating|in progress|运行中|进行中|处理中|加载中|生成中)/i.test(statusText)) return true;
    const className = String(element.className || "");
    if (nativeThreadStatusClassPattern.test(className)) return true;
    return [...(element.children || [])].some((child) => nativeElementLooksLikeThreadStatus(child));
  };

  const nativeThreadWorkState = (row) => {
    const statusState = nativeReactThreadStatusState(row);
    const completed = /^(?:complete(?:d)?|finished|done|error|failed|failure|interrupted|cancelled|canceled|stopped)$/i
      .test(String(statusState?.type || "").trim());
    const { trailing } = threadUpdatedAtPlacement(row);
    return {
      completed,
      workInProgress: !completed && (
        nativeThreadStatusTypeLooksActive(statusState?.type)
        || (trailing || []).some((candidate) => nativeElementLooksLikeThreadStatus(candidate))
      ),
    };
  };

  const threadKindFromRow = (row) => {
    const identity = threadIdentityNode(row);
    return String(
      identity?.getAttribute?.("data-app-action-sidebar-thread-kind")
      || row?.getAttribute?.("data-app-action-sidebar-thread-kind")
      || "",
    ).trim();
  };

  const threadHostIdFromRow = (row) => {
    if (threadKindFromRow(row) === "remote") return "remote";
    const identity = threadIdentityNode(row);
    return String(
      identity?.getAttribute?.("data-app-action-sidebar-thread-host-id")
      || row?.getAttribute?.("data-app-action-sidebar-thread-host-id")
      || "local",
    ).trim() || "local";
  };

  const remoteThreadTaskFromRow = (row, sessionId) => {
    const identity = threadIdentityNode(row) || row;
    if (!(identity instanceof HTMLElement)) return null;
    const expectedTaskId = normalizeThreadSessionId(sessionId).replace(/^remote:/, "");
    const reactKey = Object.keys(identity).find((key) => (
      key.startsWith("__reactFiber$") || key.startsWith("__reactInternalInstance$")
    ));
    let fiber = reactKey ? identity[reactKey] : null;
    for (let depth = 0; fiber && depth < 18; depth += 1, fiber = fiber.return) {
      for (const props of [fiber.memoizedProps, fiber.pendingProps]) {
        const task = props?.task ?? props?.entry?.task;
        if (!task || typeof task !== "object") continue;
        const taskId = String(task.id || "").trim();
        if (!expectedTaskId || !taskId || taskId === expectedTaskId) return task;
      }
    }
    return null;
  };

  const threadTimestampCacheKey = (hostId, sessionId) => (
    `${String(hostId || "local").trim() || "local"}\u0000${normalizeThreadSessionId(sessionId)}`
  );

  const threadProjectListIdFromRow = (row) => String(
    row?.closest?.(sidebarProjectListSelector)
      ?.getAttribute?.("data-app-action-sidebar-project-list-id")
    || "",
  ).trim();

  const cancelThreadRunningRecheck = (cacheKey) => {
    if (!threadRunningRecheckTimers.has(cacheKey)) return;
    window.clearTimeout(threadRunningRecheckTimers.get(cacheKey));
    threadRunningRecheckTimers.delete(cacheKey);
  };

  const threadObservationMatchesRow = (row, observation, cacheKey) => (
    observation?.cacheKey === cacheKey
    && observation.hostId === threadHostIdFromRow(row)
    && observation.sessionId === threadSessionIdFromRow(threadIdentityNode(row) || row)
  );

  const syncThreadRunningRows = (cacheKey, workInProgress) => {
    forEachTrackedThreadRow((row) => {
      const observation = threadWorkStateByRow.get(row);
      if (!threadObservationMatchesRow(row, observation, cacheKey)) return;
      // 其他列表未更新的运行图标仍属于上一轮，需先观测到非运行状态，
      // 该行后续的运行信号才可启动新一轮。
      if (!workInProgress && observation.detectedWorkInProgress) observation.suppressed = true;
      updateThreadRunningPriority(row, workInProgress);
    });
  };

  const finishThreadRunningState = (cacheKey, state) => {
    state.workInProgress = false;
    state.missingSince = null;
    state.completionVersion = (state.completionVersion || 0) + 1;
    rememberBoundedMapValue(threadRunningStateByCacheKey, cacheKey, state);
    cancelThreadRunningRecheck(cacheKey);
    syncThreadRunningRows(cacheKey, false);
    return false;
  };

  const hasObservedThreadWorkInProgress = (cacheKey) => {
    let running = false;
    forEachTrackedThreadRow((row) => {
      const observation = threadWorkStateByRow.get(row);
      if (observation?.detectedWorkInProgress && !observation.suppressed
        && threadObservationMatchesRow(row, observation, cacheKey)) running = true;
    });
    return running;
  };

  const scheduleThreadRunningRecheck = (cacheKey, delayMs) => {
    if (disposed || !cacheKey || threadRunningRecheckTimers.has(cacheKey)) return;
    const timer = window.setTimeout(() => {
      threadRunningRecheckTimers.delete(cacheKey);
      if (disposed) return;
      const state = threadRunningStateByCacheKey.get(cacheKey);
      if (!state || !Number.isFinite(state.missingSince)) return;
      const remainingMs = threadRunningLossGraceMs - (Date.now() - state.missingSince);
      if (remainingMs > 0) {
        scheduleThreadRunningRecheck(cacheKey, remainingMs);
        return;
      }
      refreshTrackedThreadUpdatedTimes();
      const current = threadRunningStateByCacheKey.get(cacheKey);
      if (current?.workInProgress && Number.isFinite(current.missingSince)
        && Date.now() - current.missingSince >= threadRunningLossGraceMs) {
        finishThreadRunningState(cacheKey, current);
      }
    }, Math.max(0, Number(delayMs) || 0));
    threadRunningRecheckTimers.set(cacheKey, timer);
  };

  const stableThreadWorkInProgress = (
    cacheKey,
    detectedWorkInProgress,
    completed = false,
    runningContext = {},
    now = Date.now(),
  ) => {
    if (!cacheKey) return detectedWorkInProgress;
    const state = threadRunningStateByCacheKey.get(cacheKey);
    if (completed) return finishThreadRunningState(cacheKey, { ...state, ...runningContext });
    if (detectedWorkInProgress) {
      rememberBoundedMapValue(
        threadRunningStateByCacheKey,
        cacheKey,
        {
          ...(threadRunningStateByCacheKey.get(cacheKey) || {}),
          ...runningContext,
          missingSince: null,
          workInProgress: true,
        },
      );
      cancelThreadRunningRecheck(cacheKey);
      return true;
    }
    if (!state?.workInProgress) return false;
    if (runningContext.sessionId) state.sessionId = runningContext.sessionId;
    if (runningContext.hostId) state.hostId = runningContext.hostId;
    if (runningContext.projectListId) state.projectListId = runningContext.projectListId;
    if (hasObservedThreadWorkInProgress(cacheKey)) {
      state.missingSince = null;
      cancelThreadRunningRecheck(cacheKey);
      return true;
    }
    if (!Number.isFinite(state.missingSince)) {
      state.missingSince = now;
      rememberBoundedMapValue(threadRunningStateByCacheKey, cacheKey, state);
    }
    const remainingMs = threadRunningLossGraceMs - (now - state.missingSince);
    if (remainingMs > 0) {
      scheduleThreadRunningRecheck(cacheKey, remainingMs);
      return true;
    }
    return finishThreadRunningState(cacheKey, state);
  };

  const sidebarThreadTimestampState = (row, now = Date.now()) => {
    const nativeState = nativeThreadWorkState(row);
    const detectedWorkInProgress = nativeState.workInProgress;
    const identity = threadIdentityNode(row);
    if (!(identity instanceof HTMLElement)) {
      return {
        cacheKey: "",
        completedWork: false,
        hostId: "local",
        sessionId: "",
        workInProgress: detectedWorkInProgress,
      };
    }
    const sessionId = normalizeThreadSessionId(threadSessionIdFromRow(identity));
    const hostId = threadHostIdFromRow(row);
    const kind = threadKindFromRow(row);
    if (!sessionId) {
      return {
        cacheKey: "",
        completedWork: false,
        hostId,
        kind,
        sessionId: "",
        workInProgress: detectedWorkInProgress,
      };
    }
    const cacheKey = threadTimestampCacheKey(hostId, sessionId);
    const previous = threadWorkStateByRow.get(row);
    // 仅在临时会话获得正式 ID 时迁移状态；复用行或切换主机不能迁移其他
    // 会话的运行状态，也不能取消其复查定时器。
    if (previous?.cacheKey !== cacheKey && previous?.hostId === hostId
      && previous?.sessionId?.startsWith("client-new-thread:")
      && !sessionId.startsWith("client-new-thread:")) {
      const previousRunningState = threadRunningStateByCacheKey.get(previous.cacheKey);
      if (previousRunningState && !threadRunningStateByCacheKey.has(cacheKey)) {
        rememberBoundedMapValue(
          threadRunningStateByCacheKey,
          cacheKey,
          { ...previousRunningState, hostId, sessionId },
        );
      }
      threadRunningStateByCacheKey.delete(previous.cacheKey);
      cancelThreadRunningRecheck(previous.cacheKey);
    }
    const shared = threadRunningStateByCacheKey.get(cacheKey);
    const sameIdentity = previous?.cacheKey === cacheKey;
    const observation = {
      cacheKey,
      completionVersion: shared?.completionVersion || 0,
      detectedWorkInProgress,
      hostId,
      nativeCompleted: nativeState.completed,
      sessionId,
      suppressed: detectedWorkInProgress && sameIdentity && (
        previous.suppressed === true
        || (previous.detectedWorkInProgress && (
          shared?.workInProgress === false
          || (previous.completionVersion || 0) < (shared?.completionVersion || 0)
        ))
      ),
    };
    threadWorkStateByRow.set(row, observation);
    const previouslyRunning = shared?.workInProgress === true;
    const workInProgress = stableThreadWorkInProgress(
      cacheKey,
      detectedWorkInProgress && !observation.suppressed,
      nativeState.completed && (!sameIdentity || !previous.nativeCompleted),
      {
        hostId,
        projectListId: threadProjectListIdFromRow(row),
        sessionId,
      },
      now,
    );
    const completedWork = previouslyRunning && !workInProgress;
    if (!previouslyRunning && workInProgress) syncThreadRunningRows(cacheKey, true);
    return {
      cacheKey,
      completedWork,
      hostId,
      kind,
      sessionId,
      workInProgress,
    };
  };

  const syncRemoteThreadUpdatedAt = (row, state) => {
    if (state.kind !== "remote") return false;
    pendingThreadUpdatedAtRefs.delete(state.cacheKey);
    threadUpdatedAtRequestedAt.delete(state.cacheKey);
    const task = remoteThreadTaskFromRow(row, state.sessionId);
    if (!task) return true;
    const timestamp = threadTimestampMsFromPayload(task);
    if (!timestamp) {
      threadUpdatedAtCache.delete(state.cacheKey);
    } else {
      rememberBoundedMapValue(threadUpdatedAtCache, state.cacheKey, timestamp);
    }
    updateThreadUpdatedAt(row, timestamp);
    return true;
  };

  const sidebarThreadListItem = (row) => {
    let current = threadIdentityNode(row) || row;
    while (current instanceof HTMLElement) {
      if (
        current.getAttribute?.("role") === "listitem"
        && !current.querySelector?.("[data-app-action-sidebar-project-row]")
      ) return current;
      const parent = current.parentElement;
      if (!(parent instanceof HTMLElement)) break;
      if (
        parent.getAttribute?.("role") === "list"
        || parent.hasAttribute?.("data-app-action-sidebar-project-list-id")
      ) return current;
      current = parent;
    }
    return row instanceof HTMLElement ? row : null;
  };

  const updateThreadRunningPriority = (row, workInProgress) => {
    const item = sidebarThreadListItem(row);
    if (!(item instanceof HTMLElement)) return;
    if (workInProgress) {
      if (item.getAttribute(threadRunningAttribute) !== "true") {
        item.setAttribute(threadRunningAttribute, "true");
      }
    } else if (item.hasAttribute(threadRunningAttribute)) {
      item.removeAttribute(threadRunningAttribute);
    }
  };

  const projectThreadSessionIdsFromReact = (projectList) => {
    const sessionIds = new Set();
    if (!(projectList instanceof HTMLElement)) return sessionIds;
    const reactKey = Object.keys(projectList).find((key) => (
      key.startsWith("__reactFiber$") || key.startsWith("__reactInternalInstance$")
    ));
    let fiber = reactKey ? projectList[reactKey] : null;
    for (let depth = 0; fiber && depth < 8; depth += 1, fiber = fiber.return) {
      for (const props of [fiber.memoizedProps, fiber.pendingProps]) {
        const threadKeys = Array.isArray(props?.threadKeys)
          ? props.threadKeys
          : props?.group?.threadKeys;
        if (!Array.isArray(threadKeys)) continue;
        threadKeys.slice(0, maxSessionCacheEntries).forEach((threadKey) => {
          const sessionId = normalizeThreadSessionId(
            typeof threadKey === "string"
              ? threadKey
              : threadKey?.sessionId ?? threadKey?.threadId ?? threadKey?.id,
          );
          if (sessionId) sessionIds.add(sessionId);
        });
        return sessionIds;
      }
    }
    return sessionIds;
  };

  const visibleProjectThreadSessionIds = (projectList) => new Set(
    queryWithin(projectList, sidebarThreadRowSelector)
      .map((row) => threadIdentityNode(row) || row)
      .filter((row) => row instanceof HTMLElement)
      .map((row) => normalizeThreadSessionId(threadSessionIdFromRow(row)))
      .filter(Boolean),
  );

  const projectListIsExpanded = (projectList) => {
    const projectItem = projectList.closest?.('[role="listitem"]');
    if (!(projectItem instanceof HTMLElement)) return true;
    const toggle = queryWithin(projectItem, "button, [role=button]").find((button) => (
      !button.closest?.(sidebarProjectListSelector)
      && button.getAttribute?.("aria-expanded") != null
    ));
    return !toggle || toggle.getAttribute("aria-expanded") === "true";
  };

  const projectShowAllButtonText = (button) => String(
    button?.textContent
    || button?.innerText
    || button?.getAttribute?.("aria-label")
    || button?.getAttribute?.("title")
    || "",
  ).replace(/\s+/g, " ").trim();

  const projectShowAllButton = (projectList) => queryWithin(
    projectList,
    "button, [role=button]",
  ).find((button) => (
    !button.disabled
    && button.getAttribute?.("aria-disabled") !== "true"
    && !button.closest?.(sidebarThreadRowSelector)
    && /^(?:展开显示|继续展开|显示更多|查看更多|加载更多|全部显示|显示全部|Show more|Load more|Show all)$/i
      .test(projectShowAllButtonText(button))
  )) || null;

  const projectListsForRunningRecovery = (root) => {
    const directLists = queryWithin(root, sidebarProjectListSelector);
    if (directLists.length || !(root instanceof HTMLElement)) return directLists;
    const projectItem = root.closest?.('[role="listitem"]');
    return projectItem instanceof HTMLElement
      ? queryWithin(projectItem, sidebarProjectListSelector)
      : directLists;
  };

  const recoverHiddenRunningThreads = (root = document) => {
    if (!threadRunningStateByCacheKey.size) return;
    const runningStates = [...threadRunningStateByCacheKey.values()]
      .filter((state) => state?.sessionId && state.workInProgress);
    if (!runningStates.length) return;
    const now = Date.now();
    projectListsForRunningRecovery(root).forEach((projectList) => {
      if (!(projectList instanceof HTMLElement)) return;
      if (projectList.getAttribute(sidebarProjectShowAllAttribute) === "true") return;
      if (!projectListIsExpanded(projectList)) return;
      const projectListId = String(
        projectList.getAttribute("data-app-action-sidebar-project-list-id") || "",
      ).trim();
      const allSessionIds = projectThreadSessionIdsFromReact(projectList);
      const visibleSessionIds = visibleProjectThreadSessionIds(projectList);
      const hasHiddenRunningThread = runningStates.some((state) => {
        const sessionId = normalizeThreadSessionId(state.sessionId);
        if (!sessionId || visibleSessionIds.has(sessionId)) return false;
        if (allSessionIds.has(sessionId)) return true;
        return !allSessionIds.size
          && Boolean(projectListId)
          && state.projectListId === projectListId;
      });
      if (!hasHiddenRunningThread) return;
      const lastClickedAt = projectRunningRecoveryClickedAt.get(projectList) || 0;
      if (now - lastClickedAt < projectRunningRecoveryClickCooldownMs) return;
      const button = projectShowAllButton(projectList);
      if (!(button instanceof HTMLElement) || typeof button.click !== "function") return;
      projectRunningRecoveryClickedAt.set(projectList, now);
      try {
        button.click();
      } catch {
        projectRunningRecoveryClickedAt.delete(projectList);
      }
    });
  };

  const placeThreadUpdatedAt = (row, label) => {
    const { before, mount } = threadUpdatedAtPlacement(row, label);
    if (!(mount instanceof HTMLElement)) return;
    const children = [...(mount.children || [])];
    const labelIndex = children.indexOf(label);
    if (before instanceof HTMLElement) {
      const beforeIndex = children.indexOf(before);
      if (label.parentElement !== mount || labelIndex !== beforeIndex - 1) {
        mount.insertBefore(label, before);
      }
    } else if (label.parentElement !== mount || labelIndex !== children.length - 1) {
      mount.appendChild(label);
    }
  };

  const updateThreadUpdatedAt = (row, timestampMs) => {
    if (!(row instanceof HTMLElement)) return;
    const timestamp = numericThreadTimestamp(timestampMs);
    const labels = [...(row.querySelectorAll?.(`[${threadUpdatedAtAttribute}]`) || [])];
    let label = labels.shift() || null;
    labels.forEach((duplicate) => duplicate.remove());
    if (!timestamp) {
      label?.remove();
      return;
    }
    if (hasNativeThreadStatus(row, label)) {
      label?.remove();
      return;
    }
    if (!(label instanceof HTMLElement)) {
      label = document.createElement("time");
      label.setAttribute(threadUpdatedAtAttribute, "true");
    }
    // Codex reserves the native status/action rail with trailing siblings and
    // absolutely positioned icons. Keep the time immediately after the flexible
    // title region so it stays before that rail instead of covering its icons.
    placeThreadUpdatedAt(row, label);
    const relative = formatRelativeThreadTime(timestamp);
    const timestampText = String(timestamp);
    if (
      label.getAttribute(threadUpdatedAtMsAttribute) === timestampText
      && label.textContent === relative
    ) return;
    const date = new Date(timestamp);
    const fullTime = Number.isNaN(date.getTime()) ? "" : date.toLocaleString();
    const datetime = Number.isNaN(date.getTime()) ? "" : date.toISOString();
    const ariaLabel = `最后消息：${relative}${fullTime ? `（${fullTime}）` : ""}`;
    const title = fullTime ? `最后消息：${fullTime}` : "最后消息时间";
    label.setAttribute(threadUpdatedAtMsAttribute, timestampText);
    label.setAttribute("datetime", datetime);
    label.setAttribute("aria-label", ariaLabel);
    label.title = title;
    label.textContent = relative;
  };

  const renderCachedThreadUpdatedAt = (row) => {
    const identity = threadIdentityNode(row);
    if (!(identity instanceof HTMLElement)) return "";
    const sessionId = normalizeThreadSessionId(threadSessionIdFromRow(identity));
    if (!sessionId) return "";
    threadUpdatedAtRows.add(row);
    const timestamp = threadUpdatedAtCache.get(
      threadTimestampCacheKey(threadHostIdFromRow(row), sessionId),
    );
    updateThreadUpdatedAt(row, timestamp || 0);
    return sessionId;
  };

  const forEachTrackedThreadRow = (callback) => {
    threadUpdatedAtRows.forEach((row) => {
      if (!(row instanceof HTMLElement) || row.isConnected === false) {
        threadUpdatedAtRows.delete(row);
        return;
      }
      callback(row);
    });
  };

  const flushThreadUpdatedAtFetch = async () => {
    if (disposed || threadUpdatedAtFetchInFlight || !pendingThreadUpdatedAtRefs.size) return;
    const refs = [...pendingThreadUpdatedAtRefs.values()].slice(0, maxPendingThreadTimestampRefs);
    refs.forEach(({ cacheKey }) => pendingThreadUpdatedAtRefs.delete(cacheKey));
    threadUpdatedAtFetchInFlight = true;
    try {
      const sessionIds = [...new Set(refs.map(({ sessionId }) => sessionId))];
      const result = await callBridge(
        threadTimestampBridgePath,
        { sessionIds },
        { timeoutMs: threadTimestampFetchTimeoutMs },
      );
      if (disposed) return;
      if (result?.status !== "ok" || !result.timestamps || typeof result.timestamps !== "object") {
        throw new Error(result?.message || "Codey thread timestamp bridge is unavailable");
      }
      const refreshedCacheKeys = new Set();
      refs.forEach((ref) => {
        const timestamp = threadTimestampValueToMs(result.timestamps[ref.sessionId]);
        if (!timestamp) {
          threadUpdatedAtCache.delete(ref.cacheKey);
        } else {
          rememberBoundedMapValue(threadUpdatedAtCache, ref.cacheKey, timestamp);
        }
        rememberBoundedMapValue(threadUpdatedAtRequestedAt, ref.cacheKey, Date.now());
        refreshedCacheKeys.add(ref.cacheKey);
      });
      forEachTrackedThreadRow((row) => {
        const identity = threadIdentityNode(row);
        const sessionId = identity instanceof HTMLElement
          ? normalizeThreadSessionId(threadSessionIdFromRow(identity))
          : "";
        const cacheKey = threadTimestampCacheKey(threadHostIdFromRow(row), sessionId);
        if (refreshedCacheKeys.has(cacheKey)) renderCachedThreadUpdatedAt(row);
      });
    } catch {
      // A failed read keeps the previous label and waits for the ordinary
      // one-minute refresh. Never retry in a tight loop on the renderer thread.
      if (disposed) return;
      refs.forEach(({ cacheKey }) => {
        rememberBoundedMapValue(threadUpdatedAtRequestedAt, cacheKey, Date.now());
      });
    } finally {
      threadUpdatedAtFetchInFlight = false;
      if (!disposed && pendingThreadUpdatedAtRefs.size) {
        threadUpdatedAtFetchTimer = window.setTimeout(() => {
          threadUpdatedAtFetchTimer = 0;
          void flushThreadUpdatedAtFetch();
        }, 40);
      }
    }
  };

  const scheduleThreadUpdatedAtFetch = () => {
    if (disposed || threadUpdatedAtFetchTimer || threadUpdatedAtFetchInFlight || !pendingThreadUpdatedAtRefs.size) return;
    threadUpdatedAtFetchTimer = window.setTimeout(() => {
      threadUpdatedAtFetchTimer = 0;
      void flushThreadUpdatedAtFetch();
    }, 40);
  };

  const refreshThreadUpdatedAtRow = (row, now, forceRefresh = false) => {
    if (!(row instanceof HTMLElement)) return;
    const {
      cacheKey,
      completedWork,
      hostId,
      kind,
      sessionId,
      workInProgress,
    } = sidebarThreadTimestampState(row, now);
    updateThreadRunningPriority(row, workInProgress);
    renderCachedThreadUpdatedAt(row);
    if (!sessionId || sessionId.startsWith("client-new-thread:")) return;
    if (syncRemoteThreadUpdatedAt(row, {
      cacheKey,
      hostId,
      kind,
      sessionId,
    })) return;
    if (completedWork) threadUpdatedAtRequestedAt.delete(cacheKey);
    if (
      !forceRefresh
      && now - (threadUpdatedAtRequestedAt.get(cacheKey) || 0)
        < threadTimestampRefreshIntervalMs
    ) return;
    pendingThreadUpdatedAtRefs.set(cacheKey, {
      cacheKey,
      hostId,
      sessionId,
    });
    rememberBoundedMapValue(threadUpdatedAtRequestedAt, cacheKey, now);
  };

  const installThreadUpdatedTimes = (root = document, forceRefresh = false) => {
    const now = Date.now();
    // Virtualized sidebar rows can be replaced without another metadata
    // response. Release detached rows whenever an incremental/full scan runs.
    forEachTrackedThreadRow(() => {});
    queryWithin(root, "[data-app-action-sidebar-thread-row]").forEach((row) => {
      refreshThreadUpdatedAtRow(row, now, forceRefresh);
    });
    scheduleThreadUpdatedAtFetch();
  };

  const refreshTrackedThreadUpdatedTimes = (forceRefresh = false) => {
    if (disposed) return;
    const now = Date.now();
    // The mutation observer and deferred initial scan register visible rows.
    // Periodic and focus/page recovery only revisit those tracked rows, keeping
    // full-document work away from interaction-sensitive renderer events.
    forEachTrackedThreadRow((row) => {
      refreshThreadUpdatedAtRow(row, now, forceRefresh);
    });
    scheduleThreadUpdatedAtFetch();
  };

  const codexAppAssetUrls = () => {
    const performanceUrls = (
      typeof performance !== "undefined"
      && typeof performance.getEntriesByType === "function"
    )
      ? performance.getEntriesByType("resource").map((entry) => entry.name)
      : [];
    return [...new Set([
      ...Array.from(document.scripts || []).map((script) => script.src),
      ...Array.from(document.querySelectorAll("link[href]") || []).map((link) => link.href),
      ...performanceUrls,
    ].filter((url) => (
      url
      && url.includes("/assets/")
      && url.split("?")[0].endsWith(".js")
    )))];
  };

  const codexAssetReferencesFromSource = (source, baseUrl) => {
    const references = [];
    const pattern = /["']((?:\.\/(?:assets\/)?|\/assets\/)(?:app-initial|app-server-manager-signals|app-shared)(?:-[^"'?#/]+)?\.js(?:\?[^"']*)?)["']/g;
    for (const match of String(source || "").matchAll(pattern)) {
      try {
        const resolved = new URL(match[1], baseUrl).href;
        if (!references.includes(resolved)) references.push(resolved);
      } catch {
        continue;
      }
    }
    return references;
  };
  window.__codeyCodexAssetReferencesFromSource = codexAssetReferencesFromSource;

  const discoverCodexAppAssetUrls = async () => {
    const loadedUrls = codexAppAssetUrls();
    const discoveredUrls = loadedUrls.filter((url) => (
      url.includes("app-server-manager-signals-")
      || url.includes("app-initial-")
      || url.includes("app-shared-")
    ));
    const fetchAsset = typeof window.fetch === "function"
      ? window.fetch.bind(window)
      : (typeof fetch === "function" ? fetch : null);
    if (!fetchAsset) return discoveredUrls;

    const scriptUrls = [...new Set([
      ...Array.from(document.scripts || []).map((script) => script.src),
      ...loadedUrls,
    ])].filter((url) => (
      url
      && !url.includes("app-server-manager-signals-")
      && !url.includes("app-initial-")
      && !url.includes("app-shared-")
    )).slice(0, 6);
    for (const scriptUrl of scriptUrls) {
      try {
        const response = await fetchAsset(scriptUrl);
        if (!response?.ok || typeof response.text !== "function") continue;
        const source = await response.text();
        for (const assetUrl of codexAssetReferencesFromSource(source, scriptUrl)) {
          if (!discoveredUrls.includes(assetUrl)) discoveredUrls.push(assetUrl);
        }
      } catch {
        continue;
      }
    }
    return discoveredUrls;
  };

  const signalDispatcherFromModule = (module, namedSignalAsset) => {
    const preferred = namedSignalAsset ? [module?.rn, module?.O] : [module?.O, module?.rn];
    const candidates = [...preferred, ...Object.values(module || {})].filter((
      candidate,
      index,
      values,
    ) => typeof candidate === "function" && values.indexOf(candidate) === index);
    const matches = candidates.filter((candidate) => {
      let source = "";
      try {
        source = Function.prototype.toString.call(candidate);
      } catch {
        return false;
      }
      return (
        candidate.length >= 2
        && candidate.length <= 3
        && !/\bthis\.[\w$]+\.sendRequest\(/.test(source)
        && /(?:\breturn\b|=>)[^{};]{0,240}\b[A-Za-z_$][\w$]*\.sendRequest\(\s*[A-Za-z_$][\w$]*\s*,\s*[A-Za-z_$][\w$]*/.test(source)
      );
    });
    const preferredMatches = matches.filter((candidate) => preferred.includes(candidate));
    if (namedSignalAsset && preferredMatches.length === 1) return preferredMatches[0];
    return matches.length === 1 ? matches[0] : null;
  };
  window.__codeySignalDispatcherFromModule = signalDispatcherFromModule;

  const appServerManagerResolverFromModule = (module) => {
    const candidates = [...new Set(Object.values(module || {}).filter((candidate) => (
      typeof candidate === "function"
    )))];
    const matches = candidates.filter((candidate) => {
      let source = "";
      try {
        source = Function.prototype.toString.call(candidate);
      } catch {
        return false;
      }
      return (
        candidate.length >= 2
        && candidate.length <= 3
        && /AppServerManager RPC is not connected/.test(source)
        && /\.get\(/.test(source)
        && /\.forHost\(/.test(source)
      );
    });
    return matches.length === 1 ? matches[0] : null;
  };
  window.__codeyAppServerManagerResolverFromModule = appServerManagerResolverFromModule;

  const reactFiberFromElement = (element) => {
    if (!(element instanceof HTMLElement)) return null;
    const key = Object.keys(element).find((candidate) => (
      candidate.startsWith("__reactFiber$")
      || candidate.startsWith("__reactInternalInstance$")
    ));
    return key ? element[key] : null;
  };

  const collectAppServerScopeCandidates = (root, candidates, seen) => {
    const queue = [{ depth: 0, value: root }];
    let cursor = 0;
    let inspected = 0;
    while (cursor < queue.length && inspected < 600 && candidates.length < 48) {
      const { depth, value } = queue[cursor];
      cursor += 1;
      if (
        !value
        || (typeof value !== "object" && typeof value !== "function")
        || seen.has(value)
      ) continue;
      seen.add(value);
      inspected += 1;
      let descriptors;
      try {
        descriptors = Object.getOwnPropertyDescriptors(value);
      } catch {
        continue;
      }
      const hasFunction = (name) => {
        if (typeof descriptors[name]?.value === "function") return true;
        try {
          return typeof value[name] === "function";
        } catch {
          return false;
        }
      };
      if (
        hasFunction("get")
        && hasFunction("set")
        && hasFunction("watch")
        && hasFunction("when")
        && Object.prototype.hasOwnProperty.call(descriptors, "query")
      ) {
        candidates.push(value);
      }
      if (depth >= 6) continue;
      for (const [key, descriptor] of Object.entries(descriptors)) {
        if (
          !Object.prototype.hasOwnProperty.call(descriptor, "value")
          || ["return", "child", "sibling", "alternate", "stateNode", "_owner"].includes(key)
        ) continue;
        const nested = descriptor.value;
        if (nested && (typeof nested === "object" || typeof nested === "function")) {
          queue.push({ depth: depth + 1, value: nested });
        }
      }
    }
  };

  const appServerManagerFromReact = (resolver) => {
    const elements = [...new Set([
      ...Array.from(document.querySelectorAll("[data-app-action-sidebar-thread-row]") || []),
      ...Array.from(document.querySelectorAll("[data-turn-key]") || []),
      document.querySelector("[data-app-action-sidebar-section]"),
      document.querySelector("[data-app-action-sidebar-project-row]"),
      document.querySelector("#root"),
      document.body,
    ].filter((element) => element instanceof HTMLElement))].slice(0, 32);
    const seenScopes = new WeakSet();
    for (const element of elements) {
      let fiber = reactFiberFromElement(element);
      for (let depth = 0; fiber && depth < 24; depth += 1, fiber = fiber.return) {
        const candidates = [];
        const seenValues = new WeakSet();
        collectAppServerScopeCandidates(fiber.memoizedState, candidates, seenValues);
        collectAppServerScopeCandidates(fiber.memoizedProps, candidates, seenValues);
        collectAppServerScopeCandidates(fiber.dependencies, candidates, seenValues);
        collectAppServerScopeCandidates(fiber.updateQueue, candidates, seenValues);
        for (const scope of candidates) {
          if (seenScopes.has(scope)) continue;
          seenScopes.add(scope);
          try {
            const manager = resolver(scope, "local");
            if (
              manager
              && ["sendRequest", "refreshRecentConversations", "codeyReconcileCompletedConversation"]
                .some((method) => typeof manager[method] === "function")
            ) return manager;
          } catch {
            continue;
          }
        }
      }
    }
    return null;
  };
  window.__codeyAppServerManagerFromReact = appServerManagerFromReact;

  const legacySessionController = (dispatcher) => ({
    kind: "signals",
    dispatcher,
    refreshRecentConversations: () => dispatcher("refresh-recent-conversations-for-host", {
      hostId: "local",
    }),
  });

  const declaredMethod = (object, name) => {
    for (let depth = 0; object && depth < 8; depth += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(object, name);
      if (descriptor) return typeof descriptor.value === "function" ? descriptor.value : null;
      object = Object.getPrototypeOf(object);
    }
    return null;
  };

  // 新版 Codex 把 AppServerManager 解析器放在 app-shared 里，app-initial
  // 只剩单参辅助函数；保留原有优先级，只在 app-initial 无法解析时下沉。
  const managerAssetPriority = (url) => (
    url.includes("app-initial-")
      ? 3
      : url.includes("app-shared-")
        ? 2
        : Number(url.includes("app-server-manager-signals-"))
  );

  const appServerManagerRegistryResolverFromModule = (module) => {
    const lookups = [...new Set(Object.values(module || {}))].flatMap((candidate) => {
      if (typeof candidate !== "function") return [];
      const prototype = Object.getOwnPropertyDescriptor(candidate, "prototype")?.value;
      if (!prototype || !declaredMethod(prototype, "getAll")) return [];
      const lookup = declaredMethod(prototype, "getForHostId");
      if (!lookup || !Function.prototype.toString.call(lookup).includes(".scope.get(")) return [];
      return [lookup];
    });
    return lookups.length === 1
      ? (scope, hostId) => lookups[0].call({ scope }, hostId) : null;
  };
  window.__codeyAppServerManagerRegistryResolverFromModule = appServerManagerRegistryResolverFromModule;

  const cachedAppServerManagerFromScope = (scope, hostId) => {
    if (hostId !== "local") return null;
    const managers = new Set();
    const seenFamilies = new Set();
    const nodes = new Set([scope.node, ...scope.chain?.values?.() || []]);
    for (const node of nodes) {
      for (const [family, bindings] of node?.familyBindings?.entries?.() || []) {
        // New builds keep the concrete manager in a signal without exporting
        // its registry. Read existing local bindings only; probing unused
        // families can initialize managers or create deferred RPC connections.
        if (family?.kind !== "signal-family" || !bindings?.has?.(hostId) || seenFamilies.has(family)) continue;
        seenFamilies.add(family);
        try {
          const manager = scope.get(family, hostId);
          const client = manager && Object.getOwnPropertyDescriptor(manager, "requestClient")?.value;
          if (client?.hostId !== hostId || !declaredMethod(manager, "sendRequest")
            || !["enqueueRequest", "createRequest", "onResult"].every((method) => declaredMethod(client, method))) continue;
          managers.add(manager);
        } catch {
          continue;
        }
      }
    }
    return managers.size === 1 ? managers.values().next().value : null;
  };

  const appServerManagerRegistryResolverFromSource = (source, module) => {
    // 新版不再导出注册表类，但仍导出它读取的两个状态定义。只解析同一
    // 原生查找表达式的引用，不执行源码，也不依赖压缩后的变量名。
    const lookups = [...String(source).matchAll(/getForHostId\s*\(\s*([$A-Z_a-z][$\w]*)\s*\)\s*\{\s*return\s+this\.scope\.get\(\s*([$A-Z_a-z][$\w]*)\s*\)\.includes\(\s*\1\s*\)\s*\?\s*this\.scope\.get\(\s*([$A-Z_a-z][$\w]*)\s*,\s*\1\s*\)\s*:\s*null\s*;?\s*\}/g)];
    const bindings = [...new Set(lookups.map((match) => `${match[2]},${match[3]}`))];
    if (bindings.length !== 1) return null;
    const exports = [...String(source).matchAll(/\bexport\s*\{([^{}]+)\}/g)].at(-1);
    if (!exports) return null;
    const names = exports[1].split(",").map((entry) => (
      entry.trim().match(/^([$A-Z_a-z][$\w]*)(?:\s+as\s+([$A-Z_a-z][$\w]*))?$/)
    )).filter(Boolean);
    const resolve = (name) => {
      const values = [...new Set(names.filter((entry) => entry[1] === name)
        .map((entry) => module?.[entry[2] || entry[1]]))];
      return values.length === 1 && values[0] && typeof values[0] === "object"
        ? values[0] : null;
    };
    const [hosts, managers] = bindings[0].split(",").map(resolve);
    if (!hosts || !managers || hosts === managers) return null;
    return (scope, hostId) => {
      const registeredHosts = scope.get(hosts);
      return Array.isArray(registeredHosts) && registeredHosts.includes(hostId)
        ? scope.get(managers, hostId) : null;
    };
  };
  window.__codeyAppServerManagerRegistryResolverFromSource = appServerManagerRegistryResolverFromSource;

  const managerSourceResolvers = new WeakMap();
  const loadManagerSourceResolver = async (url, module) => {
    if (managerSourceResolvers.has(module)) return managerSourceResolvers.get(module);
    // 只读取客户端自身已加载的资产；能力探测不依赖 Debugger 改写权限。
    if (new URL(url).protocol !== "app:") return null;
    const fetchAsset = typeof window.fetch === "function" ? window.fetch.bind(window) : null;
    if (!fetchAsset) return null;
    const response = await fetchAsset(url);
    if (!response?.ok || typeof response.text !== "function") return null;
    const resolver = appServerManagerRegistryResolverFromSource(await response.text(), module);
    if (resolver) managerSourceResolvers.set(module, resolver);
    return resolver;
  };

  const messageDeletionMethods = {
    async prepare(conversationId) {
      if (!isLocalMessageDeletionManager(this)) throw unavailableCapability("deleteMessages");
      const conversation = this.getConversation(conversationId);
      const history = conversation?.turnHistory;
      if (
        !Array.isArray(conversation?.turns)
        || (history != null && (history.kind !== "canonical"
          || !Array.isArray(history.history?.islands)
          || !history.history.islands.every((island) => Array.isArray(island.entries))
          || !Number.isInteger(history.history.generation)
          || !history.history.entitiesByKey
          || typeof history.history.entitiesByKey !== "object"))
      ) throw new Error("当前会话不支持局部删除");
      const turns = [...conversation.turns, ...Object.values(history?.history?.entitiesByKey || {})];
      if (
        conversation.threadRuntimeStatus?.type === "active"
        || conversation.requests?.length
        || turns.some((turn) => turn?.status === "inProgress")
      ) throw new Error("当前任务仍在运行，请等待任务结束后再删除");
      if (this.getStreamRole(conversationId)?.role === "follower") {
        throw new Error("请在拥有当前会话的窗口中删除消息");
      }
      await this.sendRequest("thread/unsubscribe", { threadId: conversationId });
    },
    finish(conversationId, messageIds) {
      if (!isLocalMessageDeletionManager(this)) throw unavailableCapability("deleteMessages");
      const selected = new Set(messageIds);
      this.updateConversationState(conversationId, (conversation) => {
        if (selected.size) {
          const deletedTurns = new Set();
          const history = conversation.turnHistory?.kind === "canonical"
            ? conversation.turnHistory.history : null;
          if (history) {
            history.generation += 1;
            const deletedKeys = new Set(Object.keys(history.entitiesByKey).filter((key) => (
              selected.has(history.entitiesByKey[key]?.turnId)
              || selected.has(key)
              || selected.has(`history-content:${key}`)
            )));
            for (const island of history.islands) {
              island.entries = island.entries.filter((entry) => !deletedKeys.has(entry.value));
            }
            for (const key of deletedKeys) {
              deletedTurns.add(history.entitiesByKey[key]);
              delete history.entitiesByKey[key];
            }
          }
          conversation.turns = conversation.turns.filter((turn) => (
            !selected.has(turn.turnId) && !deletedTurns.has(turn)
          ));
        }
        conversation.resumeState = "needs_resume";
      });
      this.inactiveThreadUnsubscriber.clearConversationStreamOwnership(conversationId);
    },
  };
  const isLocalMessageDeletionManager = (manager) => {
    try {
      if (!manager || manager.disposed === true
        || !["getHostId", "getConversation", "getStreamRole", "sendRequest", "updateConversationState"]
          .every((name) => declaredMethod(manager, name))) return false;
      const unsubscriber = Object.getOwnPropertyDescriptor(manager, "inactiveThreadUnsubscriber")?.value;
      return Boolean(declaredMethod(unsubscriber, "clearConversationStreamOwnership"))
        && declaredMethod(manager, "getHostId").call(manager) === "local";
    } catch {
      return false;
    }
  };

  const managerSessionController = (manager) => {
    const canDeleteMessages = isLocalMessageDeletionManager(manager);
    return {
      kind: "manager",
      manager,
      prepareMessageDeletion: canDeleteMessages ? messageDeletionMethods.prepare.bind(manager) : null,
      finishMessageDeletion: canDeleteMessages ? messageDeletionMethods.finish.bind(manager) : null,
      refreshRecentConversations: typeof manager.refreshRecentConversations === "function"
        ? () => manager.refreshRecentConversations() : null,
      reconcileCompletedConversation: typeof manager.codeyReconcileCompletedConversation === "function"
        ? (payload) => manager.codeyReconcileCompletedConversation(payload)
        : null,
    };
  };

  const registeredMessageDeletionController = (feature) => {
    if (feature !== "deleteMessages") return null;
    try {
      const registry = globalThis.__codeyAppServerManagers;
      const manager = registry instanceof Map ? registry.get("local") : null;
      return isLocalMessageDeletionManager(manager) ? managerSessionController(manager) : null;
    } catch {
      return null;
    }
  };

  const sessionControllerLooksUsable = (controller, feature = "session") => {
    if (!controller) return false;
    if (feature === "modelRouting") {
      const client = controller.manager?.requestClient;
      return client?.hostId === "local"
        && ["enqueueRequest", "createRequest", "onResult"].every((method) => declaredMethod(client, method));
    }
    if (feature === "usage" || feature === "mcpReload") return typeof controller.manager?.sendRequest === "function";
    if (feature === "reconcile") return sessionControllerCanReconcileCompletedConversation(controller);
    const methods = feature === "deleteMessages"
      ? ["prepareMessageDeletion", "finishMessageDeletion"]
      : feature === "refresh" ? ["refreshRecentConversations"] : [];
    return methods.length ? methods.every((method) => typeof controller[method] === "function")
      : controller.kind === "manager" || controller.kind === "signals";
  };

  const capabilityProbes = new Map();
  const capabilityLabels = { usage: "官方额度读取", mcpReload: "MCP 配置刷新", reconcile: "完成状态同步", deleteMessages: "消息删除", modelRouting: "模型请求校验", refresh: "会话列表刷新", session: "会话管理" };
  const capabilityMessage = (feature) => `当前 Codex 暂不支持${capabilityLabels[feature] || "此功能"}，请稍后重试`;
  const pageCapabilities = Object.create(null);
  window.__codeyPageCapabilities = pageCapabilities;
  const publishCapability = (feature, available) => {
    if (disposed) return;
    pageCapabilities[feature] = {
      status: available ? "available" : "unavailable",
      message: available ? "" : capabilityMessage(feature),
    };
  };
  const unavailableCapability = (feature) => {
    publishCapability(feature, false);
    const error = new Error(capabilityMessage(feature));
    error.code = "codey_capability_unavailable";
    return error;
  };

  const sessionControllerCanReconcileCompletedConversation = (controller) => (
    controller?.kind === "manager"
    && typeof controller.reconcileCompletedConversation === "function"
  );

  const discoverCodexSessionController = async (feature) => {
    if (disposed) throw unavailableCapability(feature);
    const registered = registeredMessageDeletionController(feature);
    if (registered) {
      window.__codeyCodexSessionController = registered;
      return registered;
    }
    const requireCompletionReconcile = feature === "reconcile";
    // Rebuild wrappers so methods added by a late renderer patch become usable.
    if (window.__codeyCodexSessionController?.manager) {
      window.__codeyCodexSessionController = managerSessionController(window.__codeyCodexSessionController.manager);
    }
    if (sessionControllerLooksUsable(window.__codeyCodexSessionController, feature)) {
      return window.__codeyCodexSessionController;
    }
    const cachedManager = appServerManagerFromReact(cachedAppServerManagerFromScope);
    const cachedController = cachedManager && managerSessionController(cachedManager);
    if (sessionControllerLooksUsable(cachedController, feature)) {
      window.__codeyCodexSessionController = cachedController;
      return cachedController;
    }
    let fallbackDispatcher = typeof window.__codeyCodexSignalDispatcher === "function"
      ? window.__codeyCodexSignalDispatcher
      : null;
    const urls = (await discoverCodexAppAssetUrls())
      .sort((left, right) => managerAssetPriority(right) - managerAssetPriority(left));
    if (disposed) throw unavailableCapability(feature);
    for (const url of urls) {
      const namedSignalAsset = url.includes("app-server-manager-signals-");
      try {
        const module = typeof window.__codeyImportCodexAsset === "function"
          ? await window.__codeyImportCodexAsset(url)
          : await import(url);
        // 旧安装的迟到探测不能替换新安装已确认的接口。
        if (disposed) throw unavailableCapability(feature);
        const registered = registeredMessageDeletionController(feature);
        if (registered) {
          window.__codeyCodexSessionController = registered;
          return registered;
        }
        const resolvers = [
          feature === "deleteMessages" || feature === "modelRouting" ? appServerManagerRegistryResolverFromModule(module) : null,
          appServerManagerResolverFromModule(module),
        ].filter(Boolean);
        for (const resolver of resolvers) {
          const manager = appServerManagerFromReact(resolver);
          if (!manager) continue;
          const controller = managerSessionController(manager);
          if (sessionControllerLooksUsable(controller, feature)) {
            window.__codeyCodexSessionController = controller;
            return controller;
          }
          window.__codeyCodexSessionController ||= controller;
        }
        if (feature === "deleteMessages" || feature === "modelRouting") {
          const resolver = await loadManagerSourceResolver(url, module);
          if (disposed) throw unavailableCapability(feature);
          const registered = registeredMessageDeletionController(feature);
          const manager = !registered && resolver ? appServerManagerFromReact(resolver) : null;
          const controller = registered || (manager && managerSessionController(manager));
          if (sessionControllerLooksUsable(controller, feature)) {
            window.__codeyCodexSessionController = controller;
            return controller;
          }
        }
        fallbackDispatcher ||= signalDispatcherFromModule(module, namedSignalAsset);
      } catch (error) {
        if (disposed) throw error;
        continue;
      }
    }
    if (fallbackDispatcher && !requireCompletionReconcile && feature !== "usage" && feature !== "mcpReload" && feature !== "deleteMessages" && feature !== "modelRouting") {
      window.__codeyCodexSignalDispatcher = fallbackDispatcher;
      const controller = legacySessionController(fallbackDispatcher);
      window.__codeyCodexSessionController = controller;
      return controller;
    }
    throw unavailableCapability(feature);
  };
  const loadCodexSessionController = async ({ requireCompletionReconcile = false, feature = "session" } = {}) => {
    if (requireCompletionReconcile) feature = "reconcile";
    if (disposed) throw unavailableCapability(feature);
    const cached = window.__codeyCodexSessionController;
    // 新管理器可能在此前探测失败后才登记，必须先于缓存和失败退避检查。
    const current = registeredMessageDeletionController(feature)
      || (cached?.manager ? managerSessionController(cached.manager) : cached);
    if (sessionControllerLooksUsable(current, feature)) {
      window.__codeyCodexSessionController = current;
      capabilityProbes.delete(feature);
      publishCapability(feature, true);
      return current;
    }
    const state = capabilityProbes.get(feature) || { failures: 0, retryAt: 0, pending: null };
    capabilityProbes.set(feature, state);
    if (state.pending) return state.pending;
    // modelRouting 的调用方只有请求入口校验，重试节奏由宿主注入流程控制；
    // 这里套用指数退避会让一次早期探测失败冻结后续所有注入尝试。
    if (feature !== "modelRouting" && Date.now() < state.retryAt) throw unavailableCapability(feature);
    const discover = () => discoverCodexSessionController(feature);
    // MCP 刷新由宿主 CDP 预算约束；5 秒发现上限会让慢机或尚未 hydrate 的
    // 页面在导入/启停后误报失败，只能重启 Codex。
    state.pending = (feature === "mcpReload"
      ? Promise.resolve().then(discover)
      : waitForNativeSessionOperation(discover)).then((controller) => {
      if (disposed) throw unavailableCapability(feature);
      controller = registeredMessageDeletionController(feature) || controller;
      window.__codeyCodexSessionController = controller;
      state.failures = 0;
      state.retryAt = 0;
      publishCapability(feature, true);
      return controller;
    }, (error) => {
      const registered = !disposed && registeredMessageDeletionController(feature);
      if (registered) {
        window.__codeyCodexSessionController = registered;
        state.failures = 0;
        state.retryAt = 0;
        publishCapability(feature, true);
        return registered;
      }
      state.failures += 1;
      state.retryAt = Date.now() + Math.min(30_000, 1_000 * 2 ** Math.min(state.failures - 1, 5));
      const capabilityError = unavailableCapability(feature);
      // 探测超时、资源导入失败等真实原因要带进抛出的错误里：capability 状态
      // 对用户保持通用文案，但错误日志需要能区分结构漂移和探测失败。
      const reason = String(error?.message || error || "");
      if (reason && reason !== capabilityError.message) {
        capabilityError.message = `${capabilityError.message}（${reason}）`;
      }
      throw capabilityError;
    }).finally(() => { state.pending = null; });
    return state.pending;
  };
  window.__codeyLoadCodexSessionController = loadCodexSessionController;
  window.__codeyLoadCodexRequestClient = async () => (
    (await loadCodexSessionController({ feature: "modelRouting" })).manager.requestClient
  );

  // 模块级快路径：DI scope 是模块导出的单例，app 初始化（约 React 挂载前
  // 几秒）就填充完毕——scope + registry 直接给出 manager.requestClient，
  // 不必遍历 React fiber 等组件挂载。任一环节缺失即抛错，由调用方回落。
  const appServerScopeLike = (scope) => (
    !!scope
    && typeof scope === "object"
    && typeof scope.get === "function"
    && typeof scope.set === "function"
    && typeof scope.watch === "function"
    && typeof scope.when === "function"
    && typeof scope.query === "function"
  );
  const appServerScopeFromModule = (module) => {
    for (const value of Object.values(module || {})) {
      if (!value || typeof value !== "object") continue;
      const scope = value.scope;
      if (appServerScopeLike(scope)) return scope;
      // 诊断：mQt 存在但 scope 还没到 DI 形态时，记录原始类型与前几帧状态。
      if (module.mQt === value) {
        window.__codeyModuleClientProbe ||= {};
        window.__codeyModuleClientProbe.mQtScopeRaw = scope === null
          ? "null" : typeof scope;
      }
    }
    return null;
  };

  let moduleClientUrls = null;
  window.__codeyLoadCodexRequestClientFromModule = async () => {
    const probe = (window.__codeyModuleClientProbe ||= { attempts: 0 });
    probe.attempts += 1;
    if (!moduleClientUrls) {
      const discovered = (await discoverCodexAppAssetUrls())
        .sort((left, right) => managerAssetPriority(right) - managerAssetPriority(left));
      // 文档解析早期脚本标签可能还没挂进 DOM：空结果不缓存，下一轮重新发现。
      if (discovered.length) moduleClientUrls = discovered;
      if (probe.urlsN == null) probe.urlsN = moduleClientUrls?.length ?? 0;
    }
    for (const url of moduleClientUrls || []) {
      try {
        const module = typeof window.__codeyImportCodexAsset === "function"
          ? await window.__codeyImportCodexAsset(url)
          : await import(url);
        if (disposed) throw unavailableCapability("modelRouting");
        if (probe.importedMs == null) {
          probe.importedMs = Math.round(performance.now());
          probe.importedUrl = url.split("/").pop();
        }
        const scope = appServerScopeFromModule(module);
        const lookup = appServerManagerRegistryResolverFromModule(module);
        if (scope && probe.scopeSeenMs == null) {
          probe.scopeSeenMs = Math.round(performance.now());
        }
        if (!scope || !lookup) {
          if (probe.scopeNullMs == null) {
            probe.scopeNullMs = Math.round(performance.now());
            probe.scopeNullUrl = url.split("/").pop();
            probe.lookupFound = !!lookup;
          }
          continue;
        }
        const client = lookup(scope, "local")?.requestClient;
        if (
          client?.hostId === "local"
          && ["enqueueRequest", "createRequest", "onResult"].every(
            (method) => declaredMethod(client, method),
          )
        ) {
          if (probe.clientSeenMs == null) {
            probe.clientSeenMs = Math.round(performance.now());
          }
          return client;
        }
      } catch (error) {
        if (disposed) throw error;
        probe.lastError = String(error).slice(0, 200);
        continue;
      }
    }
    throw unavailableCapability("modelRouting");
  };
  // Native macOS uses a frozen bridge. Install the request-client guard after
  // these module/scope discovery helpers are available, including after reload.
  void window.__codeyModelWhitelistPatch?.ensureRequestGate?.().catch(() => {});

  const loadCodexSignalDispatcher = async () => {
    const controller = await loadCodexSessionController();
    if (controller.kind === "signals" && typeof controller.dispatcher === "function") {
      return controller.dispatcher;
    }
    throw new Error("当前 Codex 使用 AppServerManager 会话接口");
  };
  window.__codeyLoadCodexSignalDispatcher = loadCodexSignalDispatcher;

  const getCodexSessionController = (feature) => loadCodexSessionController({ feature });

  const readAccountRateLimits = async () => {
    let controller;
    try {
      controller = await getCodexSessionController("usage");
    } catch (error) {
      if (error?.code !== "codey_capability_unavailable") throw error;
      return { status: "unavailable", code: error.code, message: error.message };
    }
    // Managed ChatGPT authentication, token refresh, credential-store access,
    // and request serialization stay inside Codex's own AppServerManager.
    return controller.manager.sendRequest("account/rateLimits/read");
  };
  window.__codeyReadAccountRateLimits = readAccountRateLimits;

  const appServerRequestClient = () => {
    const clients = window.__codeyAppServerRequestClients;
    if (!clients || typeof clients.get !== "function") return null;
    const local = clients.get("local");
    if (local && typeof local.sendRequest === "function") return local;
    if (typeof clients.values !== "function") return null;
    const all = [...clients.values()].filter((client) => typeof client?.sendRequest === "function");
    return all.length === 1 ? all[0] : null;
  };

  const reloadMcpServers = async () => {
    capabilityProbes.delete("mcpReload");
    const sendReload = async (target) => {
      // Codex 协议要求 params 为对象；原生实现会把配置应用到已加载会话的下一轮。
      await target.sendRequest("config/mcpServer/reload", {});
      publishCapability("mcpReload", true);
      return { ok: true };
    };
    const client = appServerRequestClient();
    if (client) {
      try {
        return await sendReload(client);
      } catch (error) {
        if (/unknown method/i.test(String(error?.message || error))) throw error;
      }
    }
    const controller = await loadCodexSessionController({ feature: "mcpReload" });
    if (disposed) throw unavailableCapability("mcpReload");
    try {
      return await sendReload(controller.manager);
    } catch (error) {
      window.__codeyCodexSessionController = null;
      throw error;
    }
  };
  window.__codeyReloadMcpServers = reloadMcpServers;

  const reconcileStaleCompletedTask = async () => {
    if (disposed || document.visibilityState === "hidden") return false;
    const now = Date.now();
    if (completionReconcileInFlight) return false;
    const candidateSessionId = sessionIdentityCandidate();
    const candidateChanged = (
      candidateSessionId
      && candidateSessionId !== completionCandidateSessionId
    );
    // A cheap attribute lookup detects session switches. The geometry-based
    // getSessionId() lookup stays on the throttled/reconcile path.
    if (!candidateChanged && now < completionNextReconcileAt) return false;
    completionCandidateSessionId = candidateSessionId;
    const sessionId = candidateSessionId || getSessionId();
    if (!sessionId) {
      completionReconcileSessionId = "";
      return false;
    }
    if (completionReconcileSessionId !== sessionId) {
      completionReconcileSessionId = sessionId;
      completionNextReconcileAt = 0;
    }
    if (now < completionNextReconcileAt) return false;
    completionReconcileInFlight = true;
    completionNextReconcileAt = now + completedTaskReconcileIntervalMs;
    try {
      return await callNativeCompletionReconcileOperation(async (controller) => {
        if (
          disposed
          || controller?.kind !== "manager"
          || typeof controller.reconcileCompletedConversation !== "function"
          || document.visibilityState === "hidden"
          || getSessionId() !== sessionId
        ) return false;
        const reconciled = await controller.reconcileCompletedConversation({
          conversationId: sessionId,
          model: null,
          serviceTier: null,
          reasoningEffort: null,
          workspaceRoots: [],
          collaborationMode: null,
          showThreadGoalResumeConfirmation: false,
        });
        return reconciled === true
          && !disposed
          && document.visibilityState !== "hidden"
          && getSessionId() === sessionId;
      });
    } catch {
      // Asset discovery can race renderer patch injection. Retry on the next
      // lifecycle or DOM event instead of waiting for the 15 s interval.
      completionNextReconcileAt = Date.now() + 1_000;
      return false;
    } finally {
      completionReconcileInFlight = false;
      // A navigation event can arrive while the previous task is reconciling.
      // Check the newly visible task as soon as that operation settles.
      if (!disposed && getSessionId() !== sessionId) {
        completionNextReconcileAt = 0;
        void reconcileStaleCompletedTask();
      }
    }
  };

  const waitForNativeSessionOperation = (operation) => new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error("Codex 会话接口响应超时")), 5_000);
    Promise.resolve().then(operation).then(resolve, reject).finally(() => window.clearTimeout(timer));
  });

  const callNativeCompletionReconcileOperation = async (operation) => {
    const controller = await loadCodexSessionController({ requireCompletionReconcile: true });
    return waitForNativeSessionOperation(() => operation(controller));
  };

  const refreshRecentLocalSessions = async () => {
    try {
      const controller = await getCodexSessionController("refresh");
      await waitForNativeSessionOperation(() => controller.refreshRecentConversations());
      return true;
    } catch {
      return false;
    }
  };

  const importSessionFile = async (projectPath, file, button) => {
    button.disabled = true;
    button.dataset.busy = "true";
    let transferId = "";
    try {
      const start = await callBridge("/session/import/start", {});
      if (start?.status === "failed") {
        throw new Error(start.message || "无法准备会话导入");
      }
      if (start?.status !== "ready" || !start.transferId || !start.chunkSize) {
        throw new Error("导入准备结果不完整");
      }
      transferId = start.transferId;
      const chunkSize = Number(start.chunkSize);
      if (!Number.isSafeInteger(chunkSize) || chunkSize <= 0) {
        throw new Error("导入分块大小无效");
      }
      const fileSize = Number(file?.size);
      if (
        Number.isFinite(fileSize)
        && Number.isFinite(Number(start.maxBytes))
        && fileSize > Number(start.maxBytes)
      ) {
        throw new Error(`导入文件超过 ${Math.floor(Number(start.maxBytes) / 1024 / 1024)} MB`);
      }

      let offset = 0;
      if (
        typeof file?.slice === "function"
        && Number.isSafeInteger(fileSize)
        && fileSize >= 0
      ) {
        while (offset < fileSize) {
          const bytes = new Uint8Array(
            await file.slice(offset, Math.min(offset + chunkSize, fileSize)).arrayBuffer(),
          );
          const progress = await callBridge("/session/import/chunk", {
            transferId,
            offset,
            data: encodeBase64Bytes(bytes),
          });
          if (progress?.status === "failed") {
            throw new Error(progress.message || "写入导入分块失败");
          }
          if (progress?.status !== "ok" || progress.nextOffset !== offset + bytes.length) {
            throw new Error("导入分块进度不一致");
          }
          offset = progress.nextOffset;
        }
      } else {
        const bytes = typeof file?.arrayBuffer === "function"
          ? new Uint8Array(await file.arrayBuffer())
          : new TextEncoder().encode(await file.text());
        while (offset < bytes.length) {
          const chunk = bytes.subarray(offset, Math.min(offset + chunkSize, bytes.length));
          const progress = await callBridge("/session/import/chunk", {
            transferId,
            offset,
            data: encodeBase64Bytes(chunk),
          });
          if (progress?.status === "failed") {
            throw new Error(progress.message || "写入导入分块失败");
          }
          if (progress?.status !== "ok" || progress.nextOffset !== offset + chunk.length) {
            throw new Error("导入分块进度不一致");
          }
          offset = progress.nextOffset;
        }
      }

      const result = await callBridge("/session/import/finish", {
        transferId,
        projectPath,
      });
      if (result?.status === "failed") {
        throw new Error(result.message || "未知错误");
      }
      if (result?.status !== "imported" || !result.sessionId) {
        throw new Error("导入结果不完整");
      }
      transferId = "";
      const refreshed = await refreshRecentLocalSessions();
      showRuntimeToast(result.message || "会话数据已导入");
      if (!disposed && !refreshed) window.setTimeout(() => {
        if (!disposed) location.reload();
      }, 700);
    } catch (error) {
      showRuntimeToast(`导入失败：${error instanceof Error ? error.message : String(error)}`, "error");
    } finally {
      if (transferId) {
        void callBridge("/session/import/abort", { transferId }).catch(() => {});
      }
      button.disabled = false;
      delete button.dataset.busy;
    }
  };

  const chooseSessionImportFile = (projectPath, button) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json,application/json";
    input.hidden = true;
    input.addEventListener("change", () => {
      const file = input.files?.[0];
      input.remove();
      if (file) void importSessionFile(projectPath, file, button);
    }, { once: true });
    document.body.appendChild(input);
    input.click();
    window.setTimeout(() => {
      if (!input.files?.length) input.remove();
    }, 60_000);
  };

  const installProjectImportButtons = (root = document) => {
    queryWithin(root,
      "[data-app-action-sidebar-project-row][data-app-action-sidebar-project-id]",
    ).forEach((project) => {
      if (!(project instanceof HTMLElement) || project.querySelector(`[${projectImportAttribute}]`)) return;
      const projectPath = projectPathFromRow(project);
      if (!projectPath) return;
      project.dataset.codeyProjectPath = projectPath;
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute(projectImportAttribute, "true");
      button.setAttribute("aria-label", "导入会话数据到此项目");
      inheritNativeButtonClass(button, findProjectActionControl(project));
      button.innerHTML = projectImportIcon;
      wrapNativeButtonIcon(button, findProjectActionControl(project));
      attachSidebarActionTooltip(button, "导入会话数据到此项目");
      const refreshPosition = () => positionProjectImportButton(project, button);
      project.addEventListener("mouseenter", refreshPosition);
      project.addEventListener("focusin", refreshPosition);
      refreshPosition();
      ["pointerdown", "mousedown", "mouseup", "touchstart"].forEach((eventName) => {
        button.addEventListener(eventName, stopSidebarActionEvent, true);
      });
      button.addEventListener("click", (event) => {
        stopSidebarActionEvent(event);
        chooseSessionImportFile(projectPath, button);
      }, true);
      project.appendChild(button);
    });
  };

  const nativeTaskControlSelector = "button[aria-label], button[title], button[data-testid]";
  const nativeTaskControlIsRunning = (button) => {
    if (!(button instanceof HTMLElement) || button.disabled) return false;
    const label = [
      button.getAttribute("aria-label"),
      button.getAttribute("title"),
    ].filter(Boolean).join(" ").trim().toLowerCase();
    const testId = String(button.getAttribute("data-testid") || "").trim().toLowerCase();
    const runningLabel = label === "停止"
      || label.includes("停止生成")
      || label.includes("停止回答")
      || label === "stop"
      || label.includes("stop generating")
      || label.includes("stop response");
    const runningTestId = /(?:^|[-_])(?:stop|cancel)(?:[-_](?:generating|generation|response|task|turn))?(?:[-_]button)?$/.test(testId)
      || /(?:^|[-_])(?:generating|generation|response|task|turn)[-_](?:stop|cancel)(?:[-_]button)?$/.test(testId);
    return (runningLabel || runningTestId) && button.getClientRects().length > 0;
  };
  const isTaskRunning = () => [...document.querySelectorAll(nativeTaskControlSelector)]
    .some(nativeTaskControlIsRunning);

  // Codex owns permanent deletion: the row menu knows the conversation and the
  // host, so the injected icon reuses that action instead of deleting through
  // the backend on its own.
  const sessionDeleteMenuItemId = "delete-thread";

  const sidebarRowMenuItems = async (row) => {
    const fiberKey = Object.keys(row).find((key) => key.startsWith("__reactFiber$"));
    for (let fiber = fiberKey ? row[fiberKey] : null; fiber; fiber = fiber.return) {
      const getItems = fiber.memoizedProps?.getItems;
      if (typeof getItems !== "function") continue;
      try {
        const items = await getItems();
        if (Array.isArray(items)) return items;
      } catch {
        return null;
      }
    }
    return null;
  };

  const findMenuItemById = (items, id) => {
    for (const item of items || []) {
      if (!item || item.type === "separator") continue;
      if (item.id === id) return item;
      const nested = Array.isArray(item.submenu) ? findMenuItemById(item.submenu, id) : null;
      if (nested) return nested;
    }
    return null;
  };

  const openOfficialSessionDelete = async (row) => {
    if (disposed) return false;
    try {
      const menuItem = findMenuItemById(await sidebarRowMenuItems(row), sessionDeleteMenuItemId);
      if (disposed) return false;
      if (typeof menuItem?.onSelect === "function" && !menuItem.disabled) {
        await menuItem.onSelect();
        return true;
      }
    } catch {
      if (!disposed) showRuntimeToast("无法打开 Codex 永久删除确认，请重试", "error");
      return false;
    }
    showRuntimeToast("当前 Codex 版本未提供永久删除入口", "error");
    return false;
  };

  const findArchiveControl = (thread) => [...thread.querySelectorAll("button, [role=button]")]
    .find((control) => {
      if (
        !(control instanceof HTMLElement)
        || control.hasAttribute(sessionExportAttribute)
        || control.hasAttribute(sessionDeleteAttribute)
      ) return false;
      const descriptor = [
        control.getAttribute("aria-label"),
        control.getAttribute("title"),
        control.getAttribute("data-testid"),
        control.getAttribute("data-app-action"),
        control.textContent,
      ].filter(Boolean).join(" ");
      return /归档|取消归档|\barchive\b|\bunarchive\b/i.test(descriptor);
    });

  const projectActionControls = (project) => [...project.querySelectorAll("button, [role=button]")]
    .filter((control) => {
      if (!(control instanceof HTMLElement) || control.hasAttribute(projectImportAttribute)) return false;
      if (control.hasAttribute("data-app-action-sidebar-select-project")) return false;
      const className = String(control.getAttribute("class") || "").trim();
      const classes = className.split(/\s+/);
      return Boolean(className) && !classes.includes("sr-only") && control.getClientRects().length > 0;
    });

  const findProjectActionControl = (project) => projectActionControls(project)[0];

  const positionProjectImportButton = (project, button) => {
    const projectRect = project.getBoundingClientRect();
    const actionRects = projectActionControls(project)
      .map((control) => control.getBoundingClientRect())
      .filter((rect) => rect.width > 0 && rect.height > 0);
    if (projectRect.width <= 0 || actionRects.length === 0) return;
    const leftmostAction = Math.min(...actionRects.map((rect) => rect.left));
    const right = Math.ceil(projectRect.right - leftmostAction + 4);
    if (Number.isFinite(right) && right > 0) button.style.right = `${right}px`;
  };

  const archivePlacementTarget = (thread, archiveControl) => {
    const wrapper = archiveControl.parentElement;
    if (!(wrapper instanceof HTMLElement) || wrapper === thread) return archiveControl;
    // 新版按钮直接位于浮动操作栏内；仅跳过旧版的单按钮提示包装层，
    // 避免把整个操作栏当成按钮包装层，将图标插到聊天行下方。
    const style = window.getComputedStyle?.(wrapper);
    const isActionRow = /^(?:inline-)?(?:flex|grid)$/.test(style?.display || "")
      || /^(?:absolute|fixed)$/.test(style?.position || "")
      || wrapper.querySelectorAll("button, [role=button]").length !== 1;
    return isActionRow ? archiveControl : wrapper;
  };

  const installSessionDeleteButtons = (root = document) => {
    queryWithin(root,
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    ).forEach((thread) => {
      if (
        !(thread instanceof HTMLElement)
        || thread.querySelector(`[${sessionDeleteAttribute}]`)
      ) return;
      const archiveControl = findArchiveControl(thread);
      if (!(archiveControl instanceof HTMLElement)) return;
      const placementTarget = archivePlacementTarget(thread, archiveControl);
      if (placementTarget.parentElement instanceof HTMLElement && placementTarget.parentElement !== thread) {
        placementTarget.parentElement.setAttribute("data-codey-session-action-row", "true");
      }
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute(sessionDeleteAttribute, "true");
      button.setAttribute("aria-label", "永久删除");
      inheritNativeButtonClass(button, archiveControl);
      button.innerHTML = sessionDeleteIcon;
      wrapNativeButtonIcon(button, archiveControl);
      attachSidebarActionTooltip(button, "永久删除");
      ["pointerdown", "mousedown", "mouseup", "touchstart"].forEach((eventName) => {
        button.addEventListener(eventName, stopSidebarActionEvent, true);
      });
      button.addEventListener("click", async (event) => {
        stopSidebarActionEvent(event);
        if (button.disabled) return;
        button.disabled = true;
        try {
          await window.__codeySessionToolsInstall?.openOfficialSessionDelete?.(thread);
        } finally {
          button.disabled = false;
        }
      }, true);
      placementTarget.insertAdjacentElement("afterend", button);
    });
  };

  const updateToolbar = () => {
    const toolbar = document.getElementById(toolbarId);
    if (!toolbar) return;
    const sessionId = selectedLogicalMessageIdsByKey.size ? getSessionId() : "";
    const persistedCount = sessionId ? logicalMessageSelectionGroups(sessionId).length : 0;
    const count = persistedCount || selectedRows().length;
    toolbar.hidden = count === 0;
    const label = toolbar.querySelector("[data-codey-count]");
    if (label) label.textContent = `已选 ${count} 轮`;
  };

  const updateSelectionButton = (row) => {
    const anchor = logicalAnchorForMessageRow(row);
    const selected = anchor.classList.contains(selectedClass);
    const button = messageSelectButtons.get(anchor)
      || anchor.querySelector("[data-codey-message-select]");
    if (!button) return;
    button.setAttribute("aria-pressed", selected ? "true" : "false");
    button.textContent = selected ? "✓" : "○";
  };

  const setLogicalRowSelected = (row, selected) => {
    const anchor = logicalAnchorForMessageRow(row);
    rememberLogicalMessageSelection(
      getSessionId(),
      logicalMessageIdsForRow(anchor),
      selected,
    );
    logicalRowsForMessageRow(anchor).forEach((item) => {
      if (selected) item.classList.add(selectedClass);
      else item.classList.remove(selectedClass);
    });
    updateSelectionButton(anchor);
  };

  const syncSelectionGroups = () => {
    const rows = [...document.querySelectorAll("[data-codey-message-id]")]
      .filter(isRenderedSessionElement);
    rows.forEach((row, index) => {
      delete row.dataset.codeySelectedPrevious;
      delete row.dataset.codeySelectedNext;
      if (!row.classList?.contains(selectedClass)) return;
      if (rows[index - 1]?.classList?.contains(selectedClass)) {
        row.dataset.codeySelectedPrevious = "true";
      }
      if (rows[index + 1]?.classList?.contains(selectedClass)) {
        row.dataset.codeySelectedNext = "true";
      }
    });
  };

  const selectRow = (row, event) => {
    const anchor = logicalAnchorForMessageRow(row);
    const rows = logicalSelectionRows();
    if (event?.shiftKey && lastSelectedRow && rows.includes(lastSelectedRow)) {
      const start = rows.indexOf(lastSelectedRow);
      const end = rows.indexOf(anchor);
      rows.slice(Math.min(start, end), Math.max(start, end) + 1).forEach((item) => {
        setLogicalRowSelected(item, true);
      });
    } else {
      setLogicalRowSelected(anchor, !anchor.classList.contains(selectedClass));
    }
    lastSelectedRow = anchor;
    syncSelectionGroups();
    updateToolbar();
  };

  const deleteSelected = async () => {
    if (messageDeleteInFlight) return;
    const rows = selectedRows();
    const sessionId = String(getSessionId() || "").replace(/^local:/, "").trim();
    const selectedGroupsByAnchor = new Map();
    logicalMessageSelectionGroups(sessionId).forEach((messageIds) => {
      if (messageIds[0]) selectedGroupsByAnchor.set(messageIds[0], messageIds);
    });
    rows.forEach((row) => {
      const messageIds = logicalMessageIdsForRow(row);
      if (messageIds[0]) selectedGroupsByAnchor.set(messageIds[0], messageIds);
    });
    const selectedGroups = [...selectedGroupsByAnchor.values()];
    const logicalCount = selectedGroups.length;
    const messageIds = [...new Set(selectedGroups.flat())];
    const physicalRows = [...new Set(rows.flatMap(logicalRowsForMessageRow))];
    if (!sessionId || !messageIds.length) {
      window.alert("无法识别当前会话或尚未选择任何一轮对话");
      return;
    }
    if (isTaskRunning()) {
      window.alert("当前任务仍在运行，请等待任务结束后再删除会话记录");
      return;
    }
    if (!window.confirm(`删除 ${logicalCount} 轮对话？\n无法撤销。`)) return;
    messageDeleteInFlight = true;
    const deleteButton = document.getElementById(toolbarId)?.querySelector?.("[data-codey-delete]");
    if (deleteButton) deleteButton.disabled = true;
    let controller;
    let prepared = false;
    try {
      showRuntimeToast(`正在永久删除 ${logicalCount} 轮对话…`);
      let result;
      try {
        controller = await getCodexSessionController("deleteMessages");
        await controller.prepareMessageDeletion(sessionId);
        prepared = true;
        result = await callBridge("/session/delete-messages", { sessionId, messageIds });
      } catch (error) {
        const message = typeof error?.message === "string" ? error.message : String(error);
        if (error?.code === "codey_capability_unavailable") {
          showRuntimeToast(message, "error");
          return;
        }
        window.alert(`删除失败：${message}`);
        return;
      }
      if (result?.status === "failed") {
        window.alert(`删除失败：${result.message || "未知错误"}`);
        return;
      }
      if (Array.isArray(result?.unsupportedDatabases) && result.unsupportedDatabases.length) {
        window.alert(
          `会话文件已处理，但索引数据库未完成清理：${result.unsupportedDatabases.join("、")}。请退出 Codex 后重试。`,
        );
        return;
      }
      const deleted = Number(result?.deleted || 0);
      if (deleted !== messageIds.length) {
        const partialMessage = logicalCount === messageIds.length
          ? `只永久删除了 ${deleted}/${messageIds.length} 轮对话。`
          : `所选 ${logicalCount} 轮对话包含 ${messageIds.length} 个连续记录片段，只永久删除了 ${deleted} 个。`;
        window.alert(
          deleted
            ? `${partialMessage}页面不会隐藏未确认删除的轮次，请重启 Codex 刷新会话后重试。`
            : "未在会话文件中找到所选轮次，页面不会再假装删除。请更新或重启 Codey 后重试。",
        );
        return;
      }
      const resolvedMessageIds = Array.isArray(result?.resolvedMessageIds)
        && result.resolvedMessageIds.length === messageIds.length
        ? result.resolvedMessageIds.map(normalizeMessageId).filter(Boolean)
        : messageIds;
      const deletedIds = [...new Set([...messageIds, ...resolvedMessageIds])];
      try {
        prepared = false;
        await controller.finishMessageDeletion(sessionId, deletedIds);
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        window.alert(`消息已永久删除，但页面状态更新失败，请重新打开当前对话。\n\n${message}`);
        return;
      }
      forgetLogicalMessageSelections(sessionId, deletedIds);
      forgetLogicalMessageIds(sessionId, deletedIds);
      rememberHardDeletedMessages(sessionId, deletedIds);
      physicalRows.forEach((row) => row.classList.remove(selectedClass));
      lastSelectedRow = null;
      syncSelectionGroups();
      updateToolbar();
      showRuntimeToast(`已永久删除 ${logicalCount} 轮对话`);
    } finally {
      if (prepared) {
        try {
          await controller.finishMessageDeletion(sessionId, []);
        } catch {
          showRuntimeToast("会话状态更新失败，请重新打开当前对话", "error");
        }
      }
      messageDeleteInFlight = false;
      if (deleteButton) deleteButton.disabled = false;
    }
  };

  const mountToolbar = () => {
    if (document.getElementById(toolbarId)) return;
    const toolbar = document.createElement("div");
    toolbar.id = toolbarId;
    toolbar.hidden = true;
    toolbar.innerHTML = '<span data-codey-count>已选 0 轮</span><button type="button" data-codey-delete data-danger>删除</button><button type="button" data-codey-clear>取消</button>';
    toolbar.querySelector("[data-codey-delete]")?.addEventListener("click", () => void deleteSelected());
    toolbar.querySelector("[data-codey-clear]")?.addEventListener("click", () => {
      clearLogicalMessageSelections(getSessionId());
      selectedRows().forEach((row) => {
        setLogicalRowSelected(row, false);
      });
      lastSelectedRow = null;
      syncSelectionGroups();
      updateToolbar();
    });
    document.body.appendChild(toolbar);
  };

  const hasCanonicalTurnAncestor = (row) => {
    let parent = row.parentElement;
    while (parent) {
      if (parent.matches?.(canonicalConversationTurnSelector)) return true;
      parent = parent.parentElement;
    }
    return false;
  };

  const messageSelectionRowsWithin = (root) => {
    // A direct canonical boundary is the common streaming path. Keep this fast
    // path, but never let a generic message wrapper stand in for multiple
    // sibling turns contained inside it.
    if (
      root instanceof HTMLElement
      && root.matches?.(canonicalConversationTurnSelector)
    ) {
      return hasCanonicalTurnAncestor(root) || !isRenderedSessionElement(root) ? [] : [root];
    }
    const candidates = queryWithin(root, conversationTurnSelector);
    const canonicalRows = new Set(candidates.filter((row) => (
      row.matches?.(canonicalConversationTurnSelector)
      && !hasCanonicalTurnAncestor(row)
    )));
    // Older Codex renderers can expose only message/test-id wrappers. Keep
    // those independent fallbacks, but never allow one to absorb a canonical
    // turn above or below it.
    const eligibleFallbackRows = candidates.filter((row) => (
      !row.matches?.(canonicalConversationTurnSelector)
      && !hasCanonicalTurnAncestor(row)
      && !row.querySelector?.(canonicalConversationTurnSelector)
    ));
    const eligibleFallbackSet = new Set(eligibleFallbackRows);
    const fallbackRows = new Set(eligibleFallbackRows.filter((row) => {
      let parent = row.parentElement;
      while (parent) {
        if (eligibleFallbackSet.has(parent)) return false;
        parent = parent.parentElement;
      }
      return true;
    }));
    return candidates.filter((row) => (
      isRenderedSessionElement(row) && (canonicalRows.has(row) || fallbackRows.has(row))
    ));
  };

  const messageSelectionRecord = (row) => {
    if (!(row instanceof HTMLElement)) return null;
    const messageId = getMessageId(row);
    if (!messageId) return null;
    const metadata = messageTurnMetadata(row, messageId);
    return {
      messageId,
      metadata,
      row,
      signature: JSON.stringify([
        messageId,
        metadata.status,
        metadata.hasUserMessage,
        metadata.hasContinuationReference,
        metadata.continuationFromMessageId,
      ]),
    };
  };

  const groupMessageSelectionRecords = (records, sessionId) => {
    const mergeMessageIds = (...groups) => [...new Set(
      groups.flat().map(normalizeMessageId).filter(Boolean),
    )];
    const cachedGroupObservations = new Map();
    records.forEach((record, recordIndex) => {
      if (!record) return;
      const cachedIds = knownLogicalMessageIds(sessionId, record.messageId);
      if (cachedIds.length < 2) return;
      const cacheKey = hardDeletedMessageKey(sessionId, cachedIds[0]);
      const observation = cachedGroupObservations.get(cacheKey) || {
        cachedIds,
        members: [],
      };
      observation.members.push({
        cachedIndex: cachedIds.indexOf(record.messageId),
        recordIndex,
      });
      cachedGroupObservations.set(cacheKey, observation);
    });
    cachedGroupObservations.forEach(({ cachedIds, members }) => {
      const topologyChanged = members.some((member, index) => {
        const next = members[index + 1];
        return next && (
          next.recordIndex !== member.recordIndex + 1
          || next.cachedIndex <= member.cachedIndex
        );
      });
      if (topologyChanged) invalidateLogicalMessageGroup(sessionId, cachedIds);
    });

    records.filter(Boolean).forEach((record) => {
      const cachedIds = knownLogicalMessageIds(sessionId, record.messageId);
      const cachedIndex = cachedIds.indexOf(record.messageId);
      if (cachedIndex < 0) return;
      const expectedPredecessorId = cachedIndex > 0 ? cachedIds[cachedIndex - 1] : "";
      const continuationReferenceContradictsCache = (
        record.metadata.hasContinuationReference
        && record.metadata.continuationFromMessageId !== expectedPredecessorId
      );
      const newlyConfirmedUserTurn = (
        !record.metadata.hasContinuationReference
        && record.metadata.hasUserMessage === true
        && cachedIndex > 0
      );
      if (continuationReferenceContradictsCache || newlyConfirmedUserTurn) {
        invalidateLogicalMessageGroup(sessionId, cachedIds);
      }
    });

    const groups = [];
    let previousGroup = null;
    let previousRecord = null;
    records.forEach((record) => {
      if (!record) {
        previousGroup = null;
        previousRecord = null;
        return;
      }
      const cachedIds = knownLogicalMessageIds(sessionId, record.messageId);
      const previousRecordClosesGroup = (
        previousGroup?.messageIds[previousGroup.messageIds.length - 1]
        === previousRecord?.messageId
      );
      const explicitlyContinuesPrevious = (
        previousRecordClosesGroup
        && record.metadata.hasContinuationReference
        && Boolean(record.metadata.continuationFromMessageId)
        && record.metadata.continuationFromMessageId === previousRecord?.messageId
      );
      const cachedWithPrevious = (
        cachedIds.length > 1
        && Boolean(previousRecord)
        && cachedIds.includes(previousRecord.messageId)
      );
      const heuristicallyContinuesInterruptedRequest = (
        previousRecordClosesGroup
        && !record.metadata.hasContinuationReference
        && record.metadata.hasUserMessage === false
        && previousRecord?.metadata.status === "interrupted"
        && previousGroup?.originHasUserMessage === true
      );
      if (
        previousGroup
        && (explicitlyContinuesPrevious
          || cachedWithPrevious
          || heuristicallyContinuesInterruptedRequest)
      ) {
        previousGroup.records.push(record);
        previousGroup.messageIds = mergeMessageIds(
          previousGroup.messageIds,
          cachedIds,
          explicitlyContinuesPrevious ? record.metadata.continuationFromMessageId : [],
          record.messageId,
        );
      } else {
        previousGroup = {
          messageIds: mergeMessageIds(
            cachedIds.length > 1 ? cachedIds : [record.messageId],
          ),
          originHasUserMessage: (
            record.metadata.hasUserMessage === true
            || cachedIds.length > 1
          ),
          records: [record],
        };
        groups.push(previousGroup);
      }
      previousRecord = record;
    });
    groups.forEach((group) => {
      if (group.messageIds.length > 1) {
        group.messageIds = rememberLogicalMessageIds(sessionId, group.messageIds);
      }
    });
    return groups;
  };

  const messageSelectButtonForRow = (row) => {
    const cachedButton = messageSelectButtons.get(row);
    if (
      cachedButton
      && cachedButton.isConnected !== false
      && cachedButton.parentElement === row
    ) return cachedButton;
    return row.querySelector("[data-codey-message-select]");
  };

  const installMessageSelectButton = (row) => {
    const existingButton = messageSelectButtonForRow(row);
    if (existingButton) {
      messageSelectButtons.set(row, existingButton);
      return { button: existingButton, installed: false };
    }
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.codeyMessageSelect = "true";
    button.setAttribute("aria-pressed", row.classList.contains(selectedClass) ? "true" : "false");
    button.setAttribute("aria-label", "选择这一轮对话");
    button.title = "选择这一轮对话；按住 Shift 可连续选择";
    button.textContent = row.classList.contains(selectedClass) ? "✓" : "○";
    button.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      selectRow(row, event);
    });
    // 行的定位交给零特异性的 :where 规则：默认 static 的行得到 relative，
    // Codex 自己定位过的行不受影响；避免在安装循环里读取布局。
    row.dataset.codeyMessageRow = "true";
    row.appendChild(button);
    messageSelectButtons.set(row, button);
    return { button, installed: true };
  };

  const adjacentMessageSelectionRows = (root) => {
    if (!(root instanceof HTMLElement)) return [];
    const nearestRows = (elements) => {
      const rows = [];
      for (const element of elements) {
        rows.push(...messageSelectionRowsWithin(element));
      }
      return rows.filter((row, index) => (
        isRenderedSessionElement(row)
        && rows.indexOf(row) === index
        && !hasCanonicalTurnAncestor(row)
      ));
    };
    let current = root;
    for (let level = 0; level < 4 && current; level += 1) {
      const precedingSiblings = [];
      const followingSiblings = [];
      let sibling = current.previousElementSibling;
      while (sibling && precedingSiblings.length < maxAdjacentMessageSelectionRows) {
        precedingSiblings.push(sibling);
        sibling = sibling.previousElementSibling;
      }
      sibling = current.nextElementSibling;
      while (sibling && followingSiblings.length < maxAdjacentMessageSelectionRows) {
        followingSiblings.push(sibling);
        sibling = sibling.nextElementSibling;
      }
      const preceding = nearestRows(precedingSiblings).at(-1);
      const following = nearestRows(followingSiblings)[0];
      if (preceding || following) {
        // Keep a short run of nearby turns. This covers virtualized wrappers
        // and logical continuations without returning to a full-page scan.
        const siblings = current.parentElement?.children || [];
        const currentIndex = siblings.indexOf(current);
        const nearby = currentIndex >= 0
          ? siblings.slice(
            Math.max(0, currentIndex - maxAdjacentMessageSelectionRows),
            currentIndex + maxAdjacentMessageSelectionRows + 1,
          )
          : [current];
        const nearbyRows = nearestRows(nearby);
        return [...new Set([preceding, ...nearbyRows, root, following].filter(Boolean))];
      }
      current = current.parentElement;
    }
    return [root];
  };

  const directMessageSelectionRoot = (root) => (
    root instanceof HTMLElement
    && root.matches?.(canonicalConversationTurnSelector)
    && !hasCanonicalTurnAncestor(root)
    && isRenderedSessionElement(root)
  );

  const installMessageSelection = (root = document) => {
    mountToolbar();
    if (lastSelectedRow?.isConnected === false) lastSelectedRow = null;
    let rows = directMessageSelectionRoot(root)
      ? [root]
      : messageSelectionRowsWithin(root);
    let installed = false;
    const directCanonicalRow = (
      rows.length === 1
      && rows[0] === root
      && root instanceof HTMLElement
      && root.matches?.(canonicalConversationTurnSelector)
    );
    let sessionId = "";
    if (directCanonicalRow) {
      const record = messageSelectionRecord(root);
      const cachedButton = messageSelectButtons.get(root);
      const cachedButtonMounted = (
        cachedButton
        && cachedButton.isConnected !== false
        && cachedButton.parentElement === root
      );
      const previousLogicalAnchor = logicalAnchorForMessageRow(root);
      const logicalTopologyChanged = (
        previousLogicalAnchor !== root
        && previousLogicalAnchor.isConnected === false
      );
      sessionId = hardDeletedMessageKeys.size ? getSessionId() : "";
      if (record && isHardDeletedMessage(sessionId, record.messageId)) {
        root.remove();
        syncSelectionGroups();
        updateToolbar();
        return;
      }
      if (
        record
        && cachedButtonMounted
        && root.dataset.codeySelectionSignature === record.signature
        && !logicalTopologyChanged
      ) return;
      // A new, hydrated, completed, or virtualized physical row can change its
      // relationship with the immediately preceding turn. Recompute the
      // mounted conversation once for those bounded transitions; stable
      // streaming scans retain the direct-row fast path above.
      const adjacentRows = adjacentMessageSelectionRows(root);
      if (adjacentRows.includes(root)) rows = adjacentRows;
    }
    // Stable scans return through the direct-row signature fast path above.
    // Resolve the session only for rows whose identity or logical grouping may
    // have changed, so the virtualization cache stays scoped to one task.
    if (!sessionId) sessionId = getSessionId();
    const records = rows.map((row) => {
      const record = messageSelectionRecord(row);
      if (!record) return null;
      if (isHardDeletedMessage(sessionId, record.messageId)) {
        row.remove();
        installed = true;
        return null;
      }
      const previousMessageId = row.dataset.codeyMessageId || "";
      if (previousMessageId !== record.messageId) {
        const previousNormalizedMessageId = normalizeMessageId(previousMessageId);
        const tailHydrated = isHistoryTailMessageId(previousMessageId)
          && !isHistoryTailMessageId(record.messageId);
        if (tailHydrated) {
          replaceLogicalMessageId(sessionId, previousMessageId, record.messageId);
        }
        if (
          !tailHydrated
          && previousNormalizedMessageId
          && previousNormalizedMessageId !== record.messageId
        ) {
          const previousAnchor = logicalAnchorForMessageRow(row);
          if (previousAnchor === row) {
            setLogicalRowSelected(previousAnchor, false);
            if (lastSelectedRow === previousAnchor) lastSelectedRow = null;
          } else {
            row.classList.remove(selectedClass);
            updateSelectionButton(previousAnchor);
            if (lastSelectedRow === row) lastSelectedRow = previousAnchor;
          }
        }
        row.dataset.codeyMessageId = record.messageId;
        installed = true;
      }
      row.dataset.codeyMessageRow = "true";
      const buttonResult = installMessageSelectButton(row);
      record.button = buttonResult.button;
      installed = installed || buttonResult.installed;
      return record;
    });

    groupMessageSelectionRecords(records, sessionId).forEach((group) => {
      const anchor = group.records[0].row;
      const groupedRows = group.records.map((record) => record.row);
      const groupedMessageIds = group.messageIds;
      const groupSelected = (
        logicalMessageGroupIsSelected(sessionId, groupedMessageIds)
        || groupedRows.some((row) => (
          row.classList.contains(selectedClass)
          && row.dataset.codeyLogicalTurn !== "continuation"
        ))
      );
      if (groupSelected) {
        rememberLogicalMessageSelection(sessionId, groupedMessageIds, true);
      }
      messageLogicalRowsByAnchor.set(anchor, groupedRows);
      group.records.forEach((record, index) => {
        const { button, row } = record;
        const logicalTurn = index === 0 ? "anchor" : "continuation";
        if (row.dataset.codeyLogicalTurn !== logicalTurn) installed = true;
        messageLogicalAnchorByRow.set(row, anchor);
        row.dataset.codeyLogicalTurn = logicalTurn;
        row.dataset.codeySelectionSignature = record.signature;
        if (groupSelected) row.classList.add(selectedClass);
        else row.classList.remove(selectedClass);
        button.hidden = index !== 0;
        button.tabIndex = index === 0 ? 0 : -1;
        if (index === 0) button.removeAttribute("aria-hidden");
        else button.setAttribute("aria-hidden", "true");
        if (lastSelectedRow === row && index !== 0) lastSelectedRow = anchor;
        if (index === 0) row.dataset.codeyMessageIds = JSON.stringify(groupedMessageIds);
        else delete row.dataset.codeyMessageIds;
      });
      updateSelectionButton(anchor);
    });
    if (installed || records.some(Boolean)) {
      syncSelectionGroups();
      updateToolbar();
    }
  };

  const scan = (root = document, syncTitles = true) => {
    // Streaming output makes conversation turns by far the most frequent scan
    // root. Sidebar controls can never live inside a turn, so running their
    // installers there is a guaranteed-miss walk of the whole turn subtree.
    if (root instanceof HTMLElement) {
      const directConversationRoot = root.matches?.(conversationTurnSelector)
        && !root.matches?.(sidebarScanRootSelector);
      const conversationOnlyWrapper = (
        !root.matches?.(sidebarScanRootSelector)
        && !root.querySelector?.(sidebarScanRootSelector)
        && Boolean(root.querySelector?.(conversationTurnSelector))
      );
      if (directConversationRoot || conversationOnlyWrapper) {
        installMessageSelection(root);
        return;
      }
    }
    installSessionExportButtons(root);
    installTasksImportButton(root);
    installSessionDeleteButtons(root);
    installProjectImportButtons(root);
    installThreadUpdatedTimes(root);
    recoverHiddenRunningThreads(root);
    installMessageSelection(root);
    if (syncTitles) syncSidebarTitles(root);
  };

  window.__codeyBridge = callBridge;
  window.__codeyGetSessionId = getSessionId;
  window.__codeyGetSessionTitle = getSessionTitle;
  window.__codeySyncSidebarTitles = syncSidebarTitles;
  window.__codeyGetMessageId = getMessageId;
  window.__codeyReconcileStaleCompletedTask = reconcileStaleCompletedTask;
  window.__codeyProjectPathFromRow = projectPathFromRow;
  window.__codeyFormatRelativeThreadTime = formatRelativeThreadTime;
  window.__codeyThreadTimestampMsFromPayload = threadTimestampMsFromPayload;
  window.__codeyUpdateThreadUpdatedAt = updateThreadUpdatedAt;
  window.__codeyInstallThreadUpdatedTimes = installThreadUpdatedTimes;
  window.__codeyHasNativeThreadStatus = hasNativeThreadStatus;
  window.__codeyUpdateThreadRunningPriority = updateThreadRunningPriority;
  window.__codeyRecoverHiddenRunningThreads = recoverHiddenRunningThreads;
  window.__codeyRefreshRecentLocalSessions = refreshRecentLocalSessions;
  window.__codeyExportSession = exportSession;
  window.__codeyImportSessionFile = importSessionFile;
  window.__codeyInstallSessionDeleteButtons = installSessionDeleteButtons;
  window.__codeySyncSelectionGroups = syncSelectionGroups;
  window.__codeyDeleteSelectedMessages = deleteSelected;
  window.__codeyInstallMessageSelection = installMessageSelection;

  const codeyOwnedSelector = [
    rendererSettingsButtonSelector,
    `#${toolbarId}`,
    `#${toastId}`,
    `#${sidebarActionTooltipId}`,
    `[${sessionExportAttribute}]`,
    `[${tasksImportAttribute}]`,
    `[${projectImportAttribute}]`,
    `[${sessionDeleteAttribute}]`,
    `[${threadUpdatedAtAttribute}]`,
    "[data-codey-message-select]",
    "[data-codey-prompt-optimize]",
  ].join(", ");
  const scanBoundarySelector = [
    sidebarScanRootSelector,
    conversationTurnSelector,
  ].join(", ");
  const interactiveControlSelector = [
    "button",
    "[role=button]",
    "[role=menuitem]",
    "[role=option]",
    "[role=switch]",
    "input",
    "label",
  ].join(", ");
  const relevantAddedSelector = [
    scanBoundarySelector,
    interactiveControlSelector,
  ].join(", ");
  const pendingScanRoots = new Set();
  let pendingScanRootsByNode = new WeakMap();
  const pendingScanRootsByIdentity = new Map();
  const deferredMutationBatches = [];
  const deferredMutationSourceQueue = [];
  const deferredMutationInputQueue = [];
  const deferredMutationIdentityIndex = new Map();
  let deferredMutationSequence = 0;
  const deferredMutationSequences = new WeakMap();
  let deferredMutationTaskCount = 0;
  let deferredMutationNodeCount = 0;
  let deferredMutationInputRecordCount = 0;
  let deferredMutationInputNodeCount = 0;
  let deferredMutationFlushScheduled = false;
  let deferredMutationFlushHandle = 0;
  let deferredMutationFlushUsesAnimationFrame = false;
  const deferredMutationTaskSet = new WeakSet();
  const nowMs = () => (
    typeof globalThis.performance === "object"
      && typeof globalThis.performance?.now === "function"
      ? globalThis.performance.now()
      : Date.now()
  );
  const sessionMutationTelemetryKey = "codey-inject.sessionToolMutations";
  const sessionMutationTelemetry = () => (
    (window.__codeyObserverStats ||= {})[sessionMutationTelemetryKey]
    || ((window.__codeyObserverStats[sessionMutationTelemetryKey] = {}))
  );
  const nextSessionMutationStats = () => {
    const existing = sessionMutationTelemetry();
    const codeyTiming = existing.__codeyTiming || {};
    return {
      ...existing,
      calls: Number(codeyTiming.calls) || 0,
      items: Number(codeyTiming.items) || 0,
      totalMs: Number(codeyTiming.totalMs) || 0,
      maxMs: Number(codeyTiming.maxMs) || 0,
      mutations: Number(existing.mutations) || 0,
      nodes: Number(existing.nodes) || 0,
      processedNodes: Number(existing.processedNodes) || 0,
      pending: deferredMutationTaskCount + deferredMutationInputRecordCount + pendingScanRoots.size,
      pendingNodes: deferredMutationNodeCount + deferredMutationInputNodeCount,
      skipped: Number(existing.skipped) || 0,
      timing: {
        handleMs: Number(existing.timing?.handleMs) || 0,
        flushMs: Number(existing.timing?.flushMs) || 0,
        scanMs: Number(existing.timing?.scanMs) || 0,
      },
    };
  };
  const recordSessionMutationStats = (changes = {}) => {
    const stats = nextSessionMutationStats();
    const timing = changes.timing || {};
    Object.assign(stats, changes);
    stats.timing = {
      handleMs: (stats.timing.handleMs || 0) + (Number(timing.handleMs) || 0),
      flushMs: (stats.timing.flushMs || 0) + (Number(timing.flushMs) || 0),
      scanMs: (stats.timing.scanMs || 0) + (Number(timing.scanMs) || 0),
    };
    if (changes.timing) stats.__codeyTiming = { ...stats.timing };
    stats.pending = deferredMutationTaskCount + deferredMutationInputRecordCount + pendingScanRoots.size;
    stats.pendingNodes = deferredMutationNodeCount + deferredMutationInputNodeCount;
    (window.__codeyObserverStats ||= {})["codey-inject.sessionToolMutations"] = stats;
    return stats;
  };
  const updateSessionMutationPending = () => {
    recordSessionMutationStats();
  };

  const isCodeyOwned = (element) => (
    element instanceof HTMLElement
    && (
      element.matches?.(codeyOwnedSelector)
      || element.closest?.(codeyOwnedSelector)
    )
  );
  const containsRelevantElement = (
    element,
    relevance = new WeakMap(),
    selector = relevantAddedSelector,
  ) => {
    if (!(element instanceof HTMLElement)) return false;
    const cached = relevance.get(element);
    if (cached?.has(selector)) return cached.get(selector);
    // One containment query per candidate subtree. Scoping the cache by
    // selector keeps interactive-only additions separate from removed turns.
    const relevantDescendant = element.querySelector?.(selector);
    const found = Boolean(element.matches?.(selector) || relevantDescendant);
    let current = relevantDescendant || element;
    while (current) {
      let currentResults = relevance.get(current);
      if (!currentResults) {
        currentResults = new Map();
        relevance.set(current, currentResults);
      }
      currentResults.set(selector, true);
      if (current === element) break;
      current = current.parentElement;
    }
    if (!relevantDescendant) {
      const currentResults = relevance.get(element) || new Map();
      currentResults.set(selector, false);
      relevance.set(element, currentResults);
    }
    return found;
  };
  const nearestScanRoot = (element) => {
    if (!(element instanceof HTMLElement)) return null;
    return element.closest?.(scanBoundarySelector)
      || element.querySelector?.(scanBoundarySelector)
      || null;
  };
  const addedSubtreeScanRoot = (element) => {
    if (!(element instanceof HTMLElement)) return null;
    const enclosingBoundary = element.closest?.(scanBoundarySelector);
    if (enclosingBoundary) return enclosingBoundary;
    // Keep the whole newly-added wrapper when it contains several sibling
    // boundaries. nearestScanRoot() intentionally returns only one node and is
    // still appropriate for attribute/removal mutations.
    return element.querySelector?.(scanBoundarySelector) ? element : null;
  };
  const threadClassMutationMayAffectStatus = (target, threadRow, oldClassName) => (
    target === threadRow
    || nativeThreadStatusClassPattern.test(String(oldClassName || ""))
    || nativeThreadStatusClassPattern.test(String(target?.className || ""))
  );
  const scanRootPriority = (root) => {
    if (root === document.documentElement || root === document.body) return 4;
    if (root.matches?.("header, nav, [data-app-action-sidebar-section]")) return 3;
    if (root.matches?.(sidebarThreadRowSelector)) return 2;
    if (root.matches?.(canonicalConversationTurnSelector)) return 0;
    return 1;
  };
  const scanRootIdentity = (root) => {
    const turnKey = root.getAttribute?.("data-turn-key");
    if (turnKey) return `turn:${turnKey}`;
    const threadId = root.getAttribute?.("data-app-action-sidebar-thread-id");
    if (threadId) return `thread:${threadId}`;
    const projectId = root.getAttribute?.("data-app-action-sidebar-project-id");
    if (projectId) return `project:${projectId}`;
    return "";
  };
  const removePendingScanRoot = (root) => {
    if (!pendingScanRoots.delete(root)) return;
    const metadata = pendingScanRootsByNode.get(root);
    const identity = metadata?.identity;
    if (identity && pendingScanRootsByIdentity.get(identity) === root) {
      pendingScanRootsByIdentity.delete(identity);
    }
  };
  const addPendingScanRoot = (root) => {
    if (!(root instanceof HTMLElement)) return false;
    // Header mounts must stay fresh even while the root budget is saturated,
    // otherwise the settings button can go stale for a whole mutation storm.
    if (root.matches?.("header, nav")) {
      window.__codeyRendererInvalidateHeaderMount?.(root);
    }
    // Unknown/new Codex DOM shapes must degrade by skipping optional controls,
    // not by turning a burst of virtualized rows into a synchronous full-page
    // scan on the renderer thread.
    if (pendingScanRoots.has(root)) return false;
    const identity = scanRootIdentity(root);
    const sameIdentityRoot = identity
      ? pendingScanRootsByIdentity.get(identity)
      : null;
    if (sameIdentityRoot && sameIdentityRoot !== root) {
      removePendingScanRoot(sameIdentityRoot);
      pendingScanRoots.add(root);
      pendingScanRootsByNode.set(root, { identity });
      pendingScanRootsByIdentity.set(identity, root);
      updateSessionMutationPending();
      return true;
    }
    if (pendingScanRoots.size >= maxPendingScanRoots) {
      // A later mutation for the same priority replaces stale work so the
      // newest identity, topology or interaction change is retained.
      const staleRoot = [...pendingScanRoots]
        .sort((left, right) => scanRootPriority(left) - scanRootPriority(right))[0];
      if (!staleRoot || scanRootPriority(staleRoot) > scanRootPriority(root)) return false;
      removePendingScanRoot(staleRoot);
    }
    for (const pendingRoot of pendingScanRoots) {
      if (pendingRoot === root || pendingRoot.contains?.(root)) return false;
      if (root.contains?.(pendingRoot)) removePendingScanRoot(pendingRoot);
    }
    pendingScanRoots.add(root);
    pendingScanRootsByNode.set(root, { identity });
    if (identity) pendingScanRootsByIdentity.set(identity, root);
    updateSessionMutationPending();
    return true;
  };
  const flushIncrementalScans = () => {
    if (disposed) return;
    scanTimer = 0;
    scanDeadline = 0;
    // addPendingScanRoot() maintains a nested-free queue, so the flush must
    // not repeat the same containment checks over every pending root.
    const roots = [...pendingScanRoots].filter((root) => root.isConnected !== false);
    pendingScanRoots.clear();
    pendingScanRootsByIdentity.clear();
    updateSessionMutationPending();
    const startedAt = nowMs();
    let scannedRoots = 0;
    for (let index = 0; index < roots.length; index += 1) {
      const root = roots[index];
      if (scannedRoots > 0) {
        const elapsed = nowMs() - startedAt;
        if (
          elapsed >= maxIncrementalScanDurationMs
          || scannedRoots >= maxIncrementalScanRootsPerFlush
        ) {
          roots.slice(index).forEach(addPendingScanRoot);
          break;
        }
      }
      scan(root, true);
      scannedRoots += 1;
    }
    const stats = nextSessionMutationStats();
    recordSessionMutationStats({
      scannedRoots: (Number(stats.scannedRoots) || 0) + scannedRoots,
      timing: { scanMs: nowMs() - startedAt },
    });
    if (pendingScanRoots.size) scheduleIncrementalScan(null);
  };
  const scheduleIncrementalScan = (root) => {
    if (disposed) return;
    if (root === null) {
      // A flush already left its overflow roots in the queue. Do not let the
      // root-only scheduling entry point consume the budget a second time.
      if (!pendingScanRoots.size) return;
    } else {
      addPendingScanRoot(root);
    }
    const now = Date.now();
    if (!scanDeadline) scanDeadline = now + maxScanLatencyMs;
    // The debounce restarts on every batch, so a sustained mutation stream
    // could otherwise defer the flush indefinitely while roots pile up.
    const delay = Math.max(0, Math.min(scanDebounceMs, scanDeadline - now));
    window.clearTimeout(scanTimer);
    scanTimer = window.setTimeout(flushIncrementalScans, delay);
  };
  const scheduleInitialScan = () => {
    const run = () => {
      initialScanHandle = 0;
      if (disposed) return;
      try {
        scan();
      } catch (error) {
        window.__codeySessionToolsError = error instanceof Error
          ? `${error.name}: ${error.message}`
          : String(error);
        console.error("[Codey] deferred session tools scan failed", error);
      }
    };
    if (typeof window.requestIdleCallback === "function") {
      // Do not force this optional full-page discovery through a timeout: on a
      // continuously scrolling/animating renderer that would move the same
      // synchronous work back onto a latency-sensitive frame.
      initialScanUsesIdleCallback = true;
      initialScanHandle = window.requestIdleCallback(run);
    } else {
      initialScanHandle = window.setTimeout(run, 0);
    }
  };

  // Lightweight observer telemetry: per-handler call count, mutation count and
  // wall time, exposed on window.__codeyObserverStats for performance triage.
  // No behavior change; timing falls back to Date.now() where performance is
  // unavailable (test sandboxes).
  const codeyTimed = (name, count, run) => {
    const observerStats = (window.__codeyObserverStats ||= {});
    const entry = (observerStats[name] ||= { calls: 0, items: 0, totalMs: 0, maxMs: 0 });
    const startedAt = nowMs();
    try {
      return run();
    } finally {
      const elapsed = nowMs() - startedAt;
      const isSessionTelemetry = name === sessionMutationTelemetryKey;
      const current = isSessionTelemetry ? sessionMutationTelemetry() : entry;
      const previous = isSessionTelemetry ? current.__codeyTiming || {} : current;
      const next = {
        calls: (Number(previous.calls) || 0) + 1,
        items: (Number(previous.items) || 0) + count,
        totalMs: (Number(previous.totalMs) || 0) + elapsed,
        maxMs: Math.max(Number(previous.maxMs) || 0, elapsed),
      };
      if (isSessionTelemetry) {
        observerStats[name] = {
          ...next,
          mutations: Number(current.mutations) || 0,
          nodes: Number(current.nodes) || 0,
          processedNodes: Number(current.processedNodes) || 0,
          pending: deferredMutationTaskCount + pendingScanRoots.size,
          pendingNodes: deferredMutationNodeCount,
          skipped: Number(current.skipped) || 0,
          timing: {
            ...(current.timing || {}),
            handleMs: (Number(current.timing?.handleMs) || 0) + elapsed,
          },
          __codeyTiming: next,
        };
      } else {
        Object.assign(entry, next);
      }
    }
  };
  const mutationNodeCount = (mutation) => (
    (mutation.addedNodes?.length || 0) + (mutation.removedNodes?.length || 0)
  );
  const mutationSequenceFor = (mutation) => {
    if (!mutation || typeof mutation !== "object") return ++deferredMutationSequence;
    let sequence = deferredMutationSequences.get(mutation);
    if (sequence === undefined) {
      sequence = ++deferredMutationSequence;
      deferredMutationSequences.set(mutation, sequence);
    }
    return sequence;
  };
  const mutationHasBoundaryNode = (nodes, selector, probeBudget) => {
    const length = nodes?.length || 0;
    for (let index = 0; index < length; index += 1) {
      if (probeBudget.remaining <= 0) return false;
      const node = nodes[index];
      if (!(node instanceof HTMLElement)) continue;
      probeBudget.remaining -= 1;
      if (node.matches?.(selector) || node.querySelector?.(selector)) return true;
    }
    return false;
  };
  const isCriticalMutation = (mutation, probeBudget) => (
    mutation.type === "childList"
    && (
      mutationHasBoundaryNode(
        mutation.addedNodes,
        scanBoundarySelector,
        probeBudget,
      )
      || mutationHasBoundaryNode(
        mutation.removedNodes,
        conversationTurnSelector,
        probeBudget,
      )
    )
  );
  const mutationTaskNodeCount = (task) => {
    const addedStart = Number(task.addedOffset) || 0;
    const removedStart = Number(task.removedOffset) || 0;
    return Math.max(0, (task.addedNodes?.length || 0) - addedStart)
      + Math.max(0, (task.removedNodes?.length || 0) - removedStart);
  };
  const arraySlice = (values, start, end) => (
    Array.prototype.slice.call(values || [], start, end)
  );
  const sliceMutationTask = (task, nodeLimit) => {
    const addedOffset = Number(task.addedOffset) || 0;
    const removedOffset = Number(task.removedOffset) || 0;
    const addedCount = Math.min(
      Math.max(0, (task.addedNodes?.length || 0) - addedOffset),
      nodeLimit,
    );
    const removedCount = Math.min(
      Math.max(0, (task.removedNodes?.length || 0) - removedOffset),
      nodeLimit - addedCount,
    );
    const batchAddedNodes = arraySlice(
      task.addedNodes,
      addedOffset,
      addedOffset + addedCount,
    );
    const batchRemovedNodes = arraySlice(
      task.removedNodes,
      removedOffset,
      removedOffset + removedCount,
    );
    const nextAddedOffset = addedOffset + addedCount;
    const nextRemovedOffset = removedOffset + removedCount;
    return {
      batch: {
        sequence: task.sequence,
        critical: task.critical,
        deferredIdentity: task.deferredIdentity,
        addedNodes: batchAddedNodes,
        removedNodes: batchRemovedNodes,
        mutation: {
          type: "childList",
          target: task.mutation.target,
          addedNodes: batchAddedNodes,
          removedNodes: batchRemovedNodes,
        },
      },
      tail: mutationTaskNodeCount({
        ...task,
        addedOffset: nextAddedOffset,
        removedOffset: nextRemovedOffset,
      }) > 0 ? {
        ...task,
        addedOffset: nextAddedOffset,
        removedOffset: nextRemovedOffset,
      } : null,
    };
  };
  const mutationBatchNodeCount = (batch) => batch
    .reduce((count, task) => count + mutationTaskNodeCount(task), 0);
  const deferredMutationTaskIdentity = (task) => {
    if (task.deferredIdentity !== undefined) return task.deferredIdentity;
    const boundaryIdentity = scanRootIdentity(task.mutation?.target);
    if (boundaryIdentity) {
      task.deferredIdentity = boundaryIdentity;
      return boundaryIdentity;
    }
    let nodeIdentity = "";
    const addedStart = Number(task.addedOffset) || 0;
    const removedStart = Number(task.removedOffset) || 0;
    const addedEnd = Math.min(task.addedNodes?.length || 0, addedStart + 16);
    const removedEnd = Math.min(task.removedNodes?.length || 0, removedStart + 16);
    for (let index = addedStart; index < addedEnd && !nodeIdentity; index += 1) {
      const node = task.addedNodes[index];
      nodeIdentity = scanRootIdentity(node instanceof HTMLElement ? node : null);
    }
    for (let index = removedStart; index < removedEnd && !nodeIdentity; index += 1) {
      const node = task.removedNodes[index];
      nodeIdentity = scanRootIdentity(node instanceof HTMLElement ? node : null);
    }
    task.deferredIdentity = nodeIdentity || "";
    return task.deferredIdentity;
  };
  const deferredMutationEntryIdentity = (task) => {
    if (!deferredMutationTaskSet.has(task)) return task.deferredIdentity || "";
    const identity = deferredMutationTaskIdentity(task);
    if (identity) task.deferredIdentity = identity;
    return identity;
  };
  const deferredMutationTaskGroups = () => (
    deferredMutationBatches.length || !deferredMutationSourceQueue.length
      ? [...deferredMutationBatches, deferredMutationSourceQueue]
      : [deferredMutationSourceQueue]
  );
  const forEachDeferredMutationTask = (callback) => {
    for (const batch of deferredMutationTaskGroups()) {
      for (const task of batch) callback(task);
    }
  };
  const rebuildDeferredMutationIdentityIndex = () => {
    deferredMutationIdentityIndex.clear();
    forEachDeferredMutationTask((pending) => {
      const identity = deferredMutationEntryIdentity(pending);
      const indexed = identity ? deferredMutationIdentityIndex.get(identity) : null;
      if (identity && (!indexed || indexed.sequence < pending.sequence)) {
        deferredMutationIdentityIndex.set(identity, pending);
      }
    });
  };
  const oldestDeferredMutationTask = (predicate = () => true) => {
    let selected = null;
    forEachDeferredMutationTask((task) => {
      if (
        predicate(task)
        && (!selected || Number(task.sequence) < Number(selected.sequence))
      ) {
        selected = task;
      }
    });
    return selected;
  };
  const newestDeferredMutationTaskForIdentity = (identity) => {
    let selected = null;
    forEachDeferredMutationTask((task) => {
      if (
        deferredMutationEntryIdentity(task) === identity
        && (!selected || Number(task.sequence) > Number(selected.sequence))
      ) {
        selected = task;
      }
    });
    return selected;
  };
  const removeDeferredMutationTask = (task) => {
    const batch = task?.deferredBatch;
    const taskIndex = batch?.indexOf(task) ?? -1;
    if (taskIndex < 0) return false;
    const identity = deferredMutationEntryIdentity(task);
    batch.splice(taskIndex, 1);
    deferredMutationTaskCount = Math.max(0, deferredMutationTaskCount - 1);
    deferredMutationNodeCount = Math.max(
      0,
      deferredMutationNodeCount - mutationTaskNodeCount(task),
    );
    if (batch !== deferredMutationSourceQueue && !batch.length) {
      const batchIndex = deferredMutationBatches.indexOf(batch);
      if (batchIndex >= 0) deferredMutationBatches.splice(batchIndex, 1);
    }
    task.deferredBatch = null;
    if (identity && deferredMutationIdentityIndex.get(identity) === task) {
      deferredMutationIdentityIndex.delete(identity);
      const replacement = newestDeferredMutationTaskForIdentity(identity);
      if (replacement) deferredMutationIdentityIndex.set(identity, replacement);
    }
    return true;
  };
  const trimDeferredMutationQueue = () => {
    while (
      deferredMutationBatches.length > maxDeferredMutationBatches
      || deferredMutationTaskCount > maxDeferredMutationTasks
      || deferredMutationNodeCount > maxDeferredMutationNodes
    ) {
      // Recomputable optional UI work goes first. For keyed work, retain the
      // newest task of each identity and evict one older task at a time so a
      // crowded batch cannot take unrelated identities with it.
      const task = oldestDeferredMutationTask((pending) => !pending.critical)
        || oldestDeferredMutationTask((pending) => (
          deferredMutationIdentityIndex.get(deferredMutationEntryIdentity(pending)) !== pending
        ))
        || oldestDeferredMutationTask();
      if (!task) break;
      removeDeferredMutationTask(task);
    }
    rebuildDeferredMutationIdentityIndex();
    updateSessionMutationPending();
  };
  const enqueueMutationBatchTask = (pending, identity) => {
    const lastBatch = deferredMutationBatches.at(-1);
    const pendingNodes = mutationTaskNodeCount(pending);
    if (identity) pending.deferredIdentity = identity;
    deferredMutationTaskSet.add(pending);
    pending.sequence ??= mutationSequenceFor(pending.mutation);
    if (
      lastBatch?.length < maxIncrementalMutationRecordsPerBatch
      && mutationBatchNodeCount(lastBatch) + pendingNodes <= maxIncrementalNodesPerBatch
    ) {
      lastBatch.push(pending);
      pending.deferredBatch = lastBatch;
    } else {
      const nextBatch = [pending];
      pending.deferredBatch = nextBatch;
      deferredMutationBatches.push(nextBatch);
    }
    deferredMutationTaskCount += 1;
    deferredMutationNodeCount += pendingNodes;
    if (identity) deferredMutationIdentityIndex.set(identity, pending);
  };
  const enqueueMutationSourceTask = (task, identity) => {
    const pendingNodes = mutationTaskNodeCount(task);
    if (!pendingNodes) return;
    if (identity) task.deferredIdentity = identity;
    deferredMutationTaskSet.add(task);
    task.sequence ??= mutationSequenceFor(task.mutation);
    task.deferredBatch = deferredMutationSourceQueue;
    deferredMutationSourceQueue.push(task);
    deferredMutationTaskCount += 1;
    deferredMutationNodeCount += pendingNodes;
    if (identity) deferredMutationIdentityIndex.set(identity, task);
  };
  const enqueueMutationTask = (task) => {
    const sourceIdentity = deferredMutationTaskIdentity(task);
    const split = task.mutation.type === "childList"
      ? sliceMutationTask(task, maxIncrementalNodesPerBatch)
      : { batch: task, tail: null };
    if (task.mutation.type !== "childList" || mutationTaskNodeCount(split.batch)) {
      enqueueMutationBatchTask(split.batch, sourceIdentity);
    }
    if (split.tail) enqueueMutationSourceTask(split.tail, sourceIdentity);
    trimDeferredMutationQueue();
    const stats = nextSessionMutationStats();
    recordSessionMutationStats({ skipped: stats.skipped + 1 });
  };
  const queueDeferredMutationInputs = (mutations, start) => {
    let records = 0;
    let nodes = 0;
    for (let index = start; index < mutations.length; index += 1) {
      records += 1;
      nodes += mutationNodeCount(mutations[index]);
    }
    if (!records) return 0;
    deferredMutationInputQueue.push({
      mutations,
      offset: start,
      sequence: mutationSequenceFor(mutations[start]),
    });
    deferredMutationInputRecordCount += records;
    deferredMutationInputNodeCount += nodes;
    return nodes;
  };
  const nextDeferredMutationTask = () => {
    let selected = null;
    for (const batch of deferredMutationTaskGroups()) {
      for (const task of batch) {
        if (!selected || Number(task.sequence) < Number(selected.sequence)) {
          selected = task;
        }
      }
    }
    return selected;
  };
  const takeDeferredMutationInput = () => {
    const entry = deferredMutationInputQueue[0];
    if (!entry) return null;
    const end = Math.min(
      entry.mutations.length,
      entry.offset + maxIncrementalMutationRecordsPerBatch,
    );
    const mutations = arraySlice(entry.mutations, entry.offset, end);
    let nodes = 0;
    for (const mutation of mutations) nodes += mutationNodeCount(mutation);
    deferredMutationInputRecordCount = Math.max(
      0,
      deferredMutationInputRecordCount - mutations.length,
    );
    deferredMutationInputNodeCount = Math.max(
      0,
      deferredMutationInputNodeCount - nodes,
    );
    entry.offset = end;
    if (entry.offset >= entry.mutations.length) {
      deferredMutationInputQueue.shift();
    } else {
      entry.sequence = mutationSequenceFor(entry.mutations[entry.offset]);
    }
    return mutations;
  };
  const runDeferredMutationFlush = () => {
    deferredMutationFlushScheduled = false;
    deferredMutationFlushHandle = 0;
    deferredMutationFlushUsesAnimationFrame = false;
    if (disposed) return;
    let mutations = null;
    const nextTask = nextDeferredMutationTask();
    const nextInput = deferredMutationInputQueue[0];
    if (
      nextTask
      && (!nextInput || Number(nextTask.sequence) <= Number(nextInput.sequence))
    ) {
      const identity = deferredMutationEntryIdentity(nextTask);
      removeDeferredMutationTask(nextTask);
      const split = nextTask.mutation.type === "childList"
        ? sliceMutationTask(nextTask, maxIncrementalNodesPerBatch)
        : { batch: nextTask, tail: null };
      if (split.tail) enqueueMutationSourceTask(split.tail, identity);
      mutations = [split.batch.mutation];
    } else {
      mutations = takeDeferredMutationInput();
    }
    if (!mutations?.length) {
      updateSessionMutationPending();
      return;
    }
    rebuildDeferredMutationIdentityIndex();
    updateSessionMutationPending();
    const startedAt = nowMs();
    handleSessionToolMutations(mutations, {
      deferred: true,
      countTotals: false,
    });
    if (
      deferredMutationBatches.length
      || deferredMutationSourceQueue.length
      || deferredMutationInputQueue.length
    ) scheduleDeferredMutationFlush({ yieldToFrame: true });
  };
  const scheduleDeferredMutationFlush = ({ yieldToFrame = false } = {}) => {
    if (
      disposed
      || deferredMutationFlushScheduled
      || (!deferredMutationBatches.length
        && !deferredMutationSourceQueue.length
        && !deferredMutationInputQueue.length)
    ) return;
    deferredMutationFlushScheduled = true;
    if (yieldToFrame && typeof window.requestAnimationFrame === "function") {
      deferredMutationFlushUsesAnimationFrame = true;
      deferredMutationFlushHandle = window.requestAnimationFrame(runDeferredMutationFlush);
    } else if (yieldToFrame || typeof window.queueMicrotask !== "function") {
      deferredMutationFlushUsesAnimationFrame = false;
      deferredMutationFlushHandle = window.setTimeout(runDeferredMutationFlush, 0);
    } else {
      window.queueMicrotask(runDeferredMutationFlush);
    }
  };
  const handleSessionToolMutations = (
    mutations,
    { deferred = false, countTotals = !deferred } = {},
  ) => {
    if (!mutations?.length) {
      if (
        !deferred
        && (
          deferredMutationBatches.length
          || deferredMutationSourceQueue.length
          || deferredMutationInputQueue.length
        )
      ) scheduleDeferredMutationFlush();
      return;
    }
    const startedAt = nowMs();
    const inputCount = mutations.length;
    const handleCount = Math.min(inputCount, maxIncrementalMutationRecordsPerBatch);
    const currentMutations = handleCount < inputCount
      ? arraySlice(mutations, 0, handleCount)
      : mutations;
    // Sequence the records that run now before assigning a sequence to the
    // overflow entry. Otherwise later overflow work can jump ahead of an
    // earlier record whose large node list needs a deferred tail.
    for (const mutation of currentMutations) mutationSequenceFor(mutation);
    const overflowNodes = handleCount < inputCount
      ? queueDeferredMutationInputs(mutations, handleCount)
      : 0;
    const taskCount = currentMutations.length;
    const nodes = currentMutations.reduce(
      (count, mutation) => count + mutationNodeCount(mutation),
      0,
    );
    if (countTotals) {
      const initialStats = nextSessionMutationStats();
      recordSessionMutationStats({
        mutations: initialStats.mutations + inputCount,
        nodes: initialStats.nodes + nodes + overflowNodes,
      });
    }
    try {
      const accepted = [];
      const deferredTasks = [];
      const probeBudget = { remaining: maxCriticalMutationProbeNodes };
      let acceptedNodes = 0;
      let deferRemainingTasks = false;
      const tasks = currentMutations.map((mutation) => {
        const sequence = mutationSequenceFor(mutation);
        if (mutation.type !== "childList") {
          return {
            mutation,
            sequence,
            critical: false,
            addedNodes: [],
            removedNodes: [],
          };
        }
        return {
          mutation,
          sequence,
          critical: isCriticalMutation(mutation, probeBudget),
          addedNodes: mutation.addedNodes || [],
          removedNodes: mutation.removedNodes || [],
        };
      });
      for (const task of tasks) {
        // Once one record needs a deferred tail, queue every later record too.
        // This preserves MutationRecord order while still allowing the first
        // record to use the current batch budget.
        if (deferRemainingTasks) {
          deferredTasks.push(task);
          continue;
        }
        const taskNodes = mutationTaskNodeCount(task);
        const nodeCapacity = Math.max(0, maxIncrementalNodesPerBatch - acceptedNodes);
        if (task.mutation.type !== "childList") {
          accepted.push(task);
        } else if (taskNodes <= nodeCapacity) {
          const { batch } = sliceMutationTask(task, nodeCapacity);
          accepted.push(batch);
          acceptedNodes += mutationTaskNodeCount(batch);
        } else if (nodeCapacity > 0 && taskNodes > 0) {
          const { batch, tail } = sliceMutationTask(task, nodeCapacity);
          accepted.push(batch);
          acceptedNodes += mutationTaskNodeCount(batch);
          deferRemainingTasks = true;
          if (tail) deferredTasks.push(tail);
        } else {
          deferRemainingTasks = true;
          deferredTasks.push(task);
        }
      }
      for (const task of deferredTasks) enqueueMutationTask(task);
      const processedNodes = codeyTimed(
        "codey-inject.sessionToolMutations",
        taskCount,
        () => handleSessionToolMutationsImpl(accepted.map((task) => task.mutation)),
      );
      const stats = nextSessionMutationStats();
      recordSessionMutationStats({
        processedNodes: stats.processedNodes + processedNodes,
      });
    } finally {
      recordSessionMutationStats({
        timing: countTotals
          ? { handleMs: nowMs() - startedAt }
          : { flushMs: nowMs() - startedAt },
      });
      if (
        !deferred
        && (
          deferredMutationBatches.length
          || deferredMutationSourceQueue.length
          || deferredMutationInputQueue.length
        )
      ) scheduleDeferredMutationFlush();
    }
  };
  const handleSessionToolMutationsImpl = (mutations) => {
    if (disposed) return 0;
    let processedNodes = 0;
    const removalTargets = new Set();
    const relevanceCache = new WeakMap();
    const removalRelevanceCache = new WeakMap();
    const targetContextCache = new WeakMap();
    const contextForTarget = (target) => {
      if (!(target instanceof HTMLElement)) {
        return { conversationTurn: null, threadRow: null };
      }
      const cached = targetContextCache.get(target);
      if (cached) return cached;
      const context = {
        conversationTurn: target.closest?.(canonicalConversationTurnSelector) || null,
        threadRow: target.closest?.(sidebarThreadRowSelector) || null,
      };
      targetContextCache.set(target, context);
      return context;
    };
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (target && !isCodeyOwned(target)) {
          const { threadRow } = contextForTarget(target);
          const relevantThreadClassChange = threadRow
            && mutation.attributeName === "class"
            && threadClassMutationMayAffectStatus(target, threadRow, mutation.oldValue);
          const pureUiAttributeChange = [
            "aria-label",
            "aria-hidden",
            "disabled",
            "hidden",
          ].includes(mutation.attributeName);
          if (relevantThreadClassChange || (
            mutation.attributeName !== "class"
            && !pureUiAttributeChange
          )) {
            const topologyRoot = (
              threadRow
              || nearestScanRoot(target)
              || (sessionIdentityAttributes.includes(mutation.attributeName)
                ? document.body
                : null)
            );
            if (addPendingScanRoot(topologyRoot)) {
              processedNodes += 1;
            }
          }
        }
        continue;
      }
      const targetContext = contextForTarget(target);
      // Mutation storms inside one mounted turn do not need sidebar or toolbar
      // discovery. Rejecting the whole record before inspecting every added or
      // removed node keeps streaming churn off the optional tool path.
      if (targetContext.conversationTurn) continue;
      // Depends only on mutation.target, so it is identical for every node in
      // this record; streaming appends many text nodes per record.
      let interactiveRoot;
      const interactiveRootFor = () => {
        if (interactiveRoot === undefined) {
          interactiveRoot = target?.closest?.(interactiveControlSelector) || null;
        }
        return interactiveRoot;
      };
      for (const node of mutation.addedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) {
          if (node?.nodeType !== Node.TEXT_NODE) continue;
          const root = interactiveRootFor();
          if (root && !isCodeyOwned(root)) {
            if (addPendingScanRoot(root)) processedNodes += 1;
          }
          continue;
        }
        if (isCodeyOwned(element)) continue;
        const threadRow = element.closest?.(sidebarThreadRowSelector)
          || targetContext.threadRow;
        if (threadRow) {
          if (addPendingScanRoot(threadRow)) processedNodes += 1;
          continue;
        }
        if (!containsRelevantElement(element, relevanceCache)) continue;
        if (addPendingScanRoot(addedSubtreeScanRoot(element))) processedNodes += 1;
      }
      for (const node of mutation.removedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) continue;
        if (targetContext.threadRow && !isCodeyOwned(target)) {
          if (addPendingScanRoot(targetContext.threadRow)) processedNodes += 1;
          continue;
        }
        if (!containsRelevantElement(element, removalRelevanceCache, conversationTurnSelector)) {
          continue;
        }
        if (target && !isCodeyOwned(target)) removalTargets.add(target);
      }
    }
    removalTargets.forEach((target) => {
      const topologyRoot = addedSubtreeScanRoot(target) || nearestScanRoot(target);
      if (!topologyRoot) return;
      messageSelectionRowsWithin(topologyRoot).forEach((row) => {
        delete row.dataset.codeySelectionSignature;
      });
      if (addPendingScanRoot(topologyRoot)) processedNodes += 1;
    });
    if (pendingScanRoots.size) {
      scheduleIncrementalScan(null);
    }
    if (!completionReconcileCheckTimer && !completionReconcileInFlight) {
      completionReconcileCheckTimer = window.setTimeout(() => {
        completionReconcileCheckTimer = 0;
        void reconcileStaleCompletedTask();
      }, completionReconcileMutationDelayMs);
    }
    return processedNodes;
  };
  const sessionToolMutationOptions = {

    attributes: true,
    attributeOldValue: true,
    attributeFilter: [
      "aria-expanded",
      "class",
      "data-turn-key",
      "data-message-id",
      "data-messageid",
      "data-item-id",
      "data-id",
      "data-message-author-role",
      "data-testid",
      "data-above-composer-conversation-id",
      "data-session-id",
      "data-conversation-id",
      "data-thread-id",
      "data-request-user-input-auto-resolution-conversation-id",
      "data-response-annotation-conversation",
      "data-app-action-sidebar-thread-active",
      "data-app-action-sidebar-thread-host-id",
      "data-app-action-sidebar-thread-id",
      "data-app-action-sidebar-thread-kind",
      "data-app-action-sidebar-thread-title",
      "data-app-action-sidebar-project-id",
      "data-app-action-sidebar-project-list-id",
      "data-app-action-sidebar-project-row",
      "data-app-action-sidebar-section-heading",
      "data-app-action-sidebar-section-toggle",
      sidebarProjectShowAllAttribute,
    ],
    childList: true,
    subtree: true,
  };
  const mutationDispatcher = window.__codeyMutationDispatcher;
  let sessionToolObserver = null;
  const installedIntervals = [];
  const installedListeners = [];
  disposeInstall = () => {
    if (disposed) return;
    disposed = true;
    try { sessionToolObserver?.disconnect?.(); } catch {}
    for (const [target, type, handler, options] of installedListeners) {
      try { target.removeEventListener?.(type, handler, options); } catch {}
    }
    for (const id of installedIntervals) window.clearInterval?.(id);
    for (const id of [
      scanTimer,
      watcherWakeTimer,
      threadUpdatedAtFetchTimer,
      completionReconcileCheckTimer,
    ]) {
      window.clearTimeout(id);
    }
    if (deferredMutationFlushUsesAnimationFrame) {
      window.cancelAnimationFrame?.(deferredMutationFlushHandle);
    } else {
      window.clearTimeout(deferredMutationFlushHandle);
    }
    if (initialScanUsesIdleCallback) window.cancelIdleCallback?.(initialScanHandle);
    else window.clearTimeout(initialScanHandle);
    hideSidebarActionTooltip();
    for (const id of threadRunningRecheckTimers.values()) window.clearTimeout(id);
    threadRunningRecheckTimers.clear();
    pendingScanRoots.clear();
    pendingScanRootsByIdentity.clear();
    pendingScanRootsByNode = new WeakMap();
    deferredMutationBatches.length = 0;
    deferredMutationSourceQueue.length = 0;
    deferredMutationInputQueue.length = 0;
    deferredMutationTaskCount = 0;
    deferredMutationNodeCount = 0;
    deferredMutationInputRecordCount = 0;
    deferredMutationInputNodeCount = 0;
    deferredMutationFlushScheduled = false;
    deferredMutationFlushUsesAnimationFrame = false;
    deferredMutationIdentityIndex.clear();
    updateSessionMutationPending();
    pendingThreadUpdatedAtRefs.clear();
    threadUpdatedAtRows.clear();
    // Native-operation timeout promises must still settle after disposal.
    // Resource cleanup timers (toast, file input, object URL) also finish.
    if (window.__codeyShowRuntimeToast === showRuntimeToast) delete window.__codeyShowRuntimeToast;
    if (window.__codeyReadAccountRateLimits === readAccountRateLimits) delete window.__codeyReadAccountRateLimits;
    if (window.__codeyReloadMcpServers === reloadMcpServers) delete window.__codeyReloadMcpServers;
    window.__codeySessionToolsInjectLoaded = false;
  };
  window.__codeySessionToolsInstall = {
    dispose: disposeInstall,
    openOfficialSessionDelete,
    tooltip: {
      show: scheduleSidebarActionTooltip,
      hide(button) {
        if (!button || sidebarActionTooltipAnchor === button) hideSidebarActionTooltip();
      },
    },
  };
  addStyle();
  if (typeof mutationDispatcher?.subscribe === "function") {
    const unsubscribe = mutationDispatcher.subscribe(
      handleSessionToolMutations,
      sessionToolMutationOptions,
    );
    sessionToolObserver = { disconnect: unsubscribe };
    if (!mutationDispatcher.snapshot?.().observerInstalled) {
      unsubscribe?.();
      sessionToolObserver = null;
    }
  }
  if (!sessionToolObserver) {
    sessionToolObserver = new MutationObserver(handleSessionToolMutations);
    sessionToolObserver.observe(document.documentElement, sessionToolMutationOptions);
  }
  // forceRefresh bypasses the per-session throttle and re-fetches official
  // thread metadata for every sidebar row, so alt-tabbing must stay debounced.
  let lastForcedThreadTimeRefresh = 0;
  const forcedThreadTimeRefreshIntervalMs = 10_000;
  const refreshThreadUpdatedTimesOnReturn = () => {
    const now = Date.now();
    if (now - lastForcedThreadTimeRefresh < forcedThreadTimeRefreshIntervalMs) return;
    lastForcedThreadTimeRefresh = now;
    refreshTrackedThreadUpdatedTimes(true);
  };
  const reconcileOnVisible = () => {
    if (document.visibilityState !== "hidden") {
      refreshThreadUpdatedTimesOnReturn();
      void reconcileStaleCompletedTask();
    }
  };
  if (typeof document.addEventListener === "function") {
    const pointerdownOptions = { capture: true, passive: true };
    installedListeners.push(
      [document, "visibilitychange", wakeSessionWatcher, undefined],
      [document, "visibilitychange", reconcileOnVisible, undefined],
      [document, "pointerdown", wakeSessionWatcher, pointerdownOptions],
      [document, "keydown", wakeSessionWatcherFromKey, true],
    );
    document.addEventListener("visibilitychange", wakeSessionWatcher);
    document.addEventListener("visibilitychange", reconcileOnVisible);
    document.addEventListener("pointerdown", wakeSessionWatcher, pointerdownOptions);
    document.addEventListener("keydown", wakeSessionWatcherFromKey, true);
  }
  if (typeof window.addEventListener === "function") {
    for (const type of ["focus", "pageshow"]) {
      for (const handler of [wakeSessionWatcher, refreshThreadUpdatedTimesOnReturn, reconcileStaleCompletedTask]) {
        installedListeners.push([window, type, handler, undefined]);
      }
    }
    window.addEventListener("focus", wakeSessionWatcher);
    window.addEventListener("focus", refreshThreadUpdatedTimesOnReturn);
    window.addEventListener("focus", reconcileStaleCompletedTask);
    window.addEventListener("pageshow", wakeSessionWatcher);
    window.addEventListener("pageshow", refreshThreadUpdatedTimesOnReturn);
    window.addEventListener("pageshow", reconcileStaleCompletedTask);
  }
  if (typeof window.setInterval === "function") {
    installedIntervals.push(window.setInterval(() => {
      void reconcileStaleCompletedTask();
    }, completedTaskReconcileIntervalMs));
    installedIntervals.push(window.setInterval(() => {
      if (document.visibilityState === "hidden") return;
      refreshTrackedThreadUpdatedTimes(false);
    }, threadTimestampRefreshIntervalMs));
  }
  window.__codeyRendererInjectLoaded = true;
  window.__codeySessionToolsInjectLoaded = true;
  window.__codeySessionToolsInjectLoading = false;
  void reconcileStaleCompletedTask();
  scheduleInitialScan();
  } catch (error) {
    window.__codeySessionToolsInjectLoading = false;
    disposeInstall?.();
    // 已销毁或尚未完成的安装都不该继续对外可见。
    delete window.__codeySessionToolsInstall;
    throw error;
  }
})();
