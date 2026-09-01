// Sidebar/session tools loaded by renderer-inject.js after Codex's sidebar is
// present. This file also remains useful as a backwards-compatible manual CDP
// testing entry point.
(() => {
  if (window.__codeySessionToolsInjectLoaded) return;
  if (window.__codeySessionToolsInjectLoading) return;
  window.__codeySessionToolsInjectLoading = true;
  const rendererSettingsButtonSelector = "#codey-settings-button";
  const toolbarId = "codey-message-toolbar";
  const toastId = "codey-runtime-toast";
  const styleId = "codey-injected-style";
  const selectedClass = "codey-message-selected";
  const sessionExportAttribute = "data-codey-session-export";
  const tasksImportAttribute = "data-codey-tasks-import";
  const projectImportAttribute = "data-codey-project-import";
  const sessionDeleteAttribute = "data-codey-session-delete";
  const sessionDeleteStateAttribute = "data-codey-session-delete-state";
  const sessionDeletePopoverId = "codey-session-delete-popover";
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
  let scanTimer = 0;
  let scanDeadline = 0;
  const scanDebounceMs = 60;
  const maxScanLatencyMs = 250;
  const sidebarTitleCache = new Map();
  let watcherWakeTimer = 0;
  let deletePopoverCleanup = null;
  let codexSessionControllerPromise = null;
  let completionRunningObservation = null;
  let completionProbeInFlight = false;
  let completionNextProbeAt = 0;
  let sidebarActionTooltipTimer = 0;
  let sidebarActionTooltipAnchor = null;
  let threadUpdatedAtFetchTimer = 0;
  let threadUpdatedAtFetchInFlight = false;
  const threadUpdatedAtCache = new Map();
  const threadWorkStateByRow = new WeakMap();
  // React can briefly detach the native status rail or replace a virtualized
  // row. Preserve the last confirmed running state until a delayed rescan.
  const threadRunningStateByCacheKey = new Map();
  const threadRunningRecheckTimers = new Map();
  const projectRunningRecoveryClickedAt = new WeakMap();
  const threadUpdatedAtRequestedAt = new Map();
  const pendingThreadUpdatedAtRefs = new Map();
  const threadUpdatedAtRows = new Set();
  // A permanently deleted thread ID cannot become valid again unless an
  // import explicitly restores it. Keep the tombstone for this renderer's
  // lifetime so a stale native/virtualized row cannot reappear later.
  const deletedSidebarSessionIds = new Set();
  const pendingSidebarSessionDeleteIds = new Set();
  const hardDeletedMessageKeys = new Set();
  const completionRecoveryStateByKey = new Map();
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
  // Rich conversation tooltips (notably Hooks details) can be taller than the
  // collision-limited tooltip box. Clip the overflowing children inside that
  // box so they cannot cover their trigger and create a pointer enter/leave
  // loop. Codex has shipped both native button and focusable-span triggers;
  // aria-describedby is present only while the native tooltip is open.
  // Toggle a body class from the session-tools observer instead of body:has()
  // so streaming characterData/childList invalidation does not re-match the
  // four descendant :has() selectors on every mutation.
  const conversationRichTooltipOpenClass = "codey-rich-tooltip-open";
  const conversationRichTooltipTriggerSelector = "button, [role=\"button\"], span[tabindex=\"0\"]";
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
  const stuckCompletionGraceMs = 30_000;
  const stuckCompletionProbeIntervalMs = 15_000;
  const stuckCompletionProbeTimeoutMs = 10_000;
  const stuckCompletionRecoveryRetryMs = 30_000;
  const stuckCompletionRecoveryCooldownMs = 60_000;
  const stuckCompletionRecoveryResetMs = 5 * 60_000;
  const stuckCompletionRecoveryMaxAttempts = 3;
  const stuckRunningStreamGraceMs = 30_000;
  const stuckRunningStreamMinActivityAdvances = 2;
  const threadTimestampBridgePath = "/session/timestamps";
  const stuckCompletionBridgePath = "/session/completion-state";
  const maxPendingThreadTimestampRefs = 200;
  const fallbackSessionExportMaxBytes = 64 * 1024 * 1024;
  const maxSessionCacheEntries = 2_048;
  const maxHardDeletedMessageKeys = 10_000;
  const maxCompletionRecoveryKeys = 512;
  const maxPendingScanRoots = 64;
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

  const getSessionId = () => {
    const attributes = [
      "data-session-id",
      "data-conversation-id",
      "data-thread-id",
      "data-request-user-input-auto-resolution-conversation-id",
      "data-response-annotation-conversation",
      "data-above-composer-conversation-id",
    ];
    for (const attribute of attributes) {
      const value = document.querySelector(`[${attribute}]`)?.getAttribute(attribute);
      if (value) return value.replace(/^local:/, "");
    }
    const activeThread = document.querySelector('[data-app-action-sidebar-thread-active="true"]')
      ?.getAttribute("data-app-action-sidebar-thread-id");
    if (activeThread) return activeThread.replace(/^local:/, "");
    const match = location.pathname.match(/(?:\/c\/|\/conversation\/|\/session\/)([A-Za-z0-9_-]+)/);
    if (match) return match[1];
    return new URLSearchParams(location.search).get("conversation_id") || new URLSearchParams(location.search).get("session_id") || "";
  };

  const sidebarTitles = (root = document) => queryWithin(root,
    "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
  ).filter((thread) => !isDeletedSidebarThread(thread)).map((thread) => ({
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
        if (result?.status !== "failed") return;
        previousTitles.forEach(([sessionId, previousTitle], index) => {
          if (sidebarTitleCache.get(sessionId) !== titles[index].title) return;
          if (previousTitle === undefined) sidebarTitleCache.delete(sessionId);
          else rememberBoundedMapValue(sidebarTitleCache, sessionId, previousTitle);
        });
      })
      .catch(() => {
        previousTitles.forEach(([sessionId, previousTitle], index) => {
          if (sidebarTitleCache.get(sessionId) !== titles[index].title) return;
          if (previousTitle === undefined) sidebarTitleCache.delete(sessionId);
          else rememberBoundedMapValue(sidebarTitleCache, sessionId, previousTitle);
        });
      });
  };

  const wakeSessionWatcher = () => {
    if (document.visibilityState === "hidden" || watcherWakeTimer) return;
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

  const usableCompletionTurnId = (value) => {
    const normalized = usableMessageId(value);
    return normalized && !isHistoryTailMessageId(normalized)
      ? normalized
      : "";
  };

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

  const getCurrentTurnId = () => {
    const turnRows = Array.from(document.querySelectorAll?.("[data-turn-key]") || []);
    for (let index = turnRows.length - 1; index >= 0; index -= 1) {
      let parent = turnRows[index].parentElement;
      let nested = false;
      while (parent) {
        if (parent.matches?.("[data-turn-key]")) {
          nested = true;
          break;
        }
        parent = parent.parentElement;
      }
      if (nested) continue;
      // A history tail key is only a temporary DOM position. Current Codex
      // hydrates the real rollout turn id into that same row's React state, so
      // resolve the row before deciding whether to fall back to an older turn.
      const turnId = usableCompletionTurnId(getMessageId(turnRows[index]));
      if (turnId) return turnId;
    }

    // Older renderer shapes may omit data-turn-key. Keep the React/message
    // fallback for those builds, but never let a tail placeholder override a
    // canonical rollout turn from the first pass.
    const rows = Array.from(document.querySelectorAll?.(conversationTurnSelector) || []);
    for (let index = rows.length - 1; index >= 0; index -= 1) {
      let parent = rows[index].parentElement;
      let nested = false;
      while (parent) {
        if (parent.matches?.(conversationTurnSelector)) {
          nested = true;
          break;
        }
        parent = parent.parentElement;
      }
      if (nested) continue;
      const turnId = usableCompletionTurnId(getMessageId(rows[index]));
      if (turnId) return turnId;
    }
    return "";
  };

  const currentTurnRenderFingerprint = (turnId) => {
    const normalizedTurnId = usableCompletionTurnId(turnId);
    if (!normalizedTurnId) return "";
    const rows = Array.from(document.querySelectorAll?.("[data-turn-key]") || []);
    for (let index = rows.length - 1; index >= 0; index -= 1) {
      const row = rows[index];
      let parent = row.parentElement;
      let nested = false;
      while (parent) {
        if (parent.matches?.("[data-turn-key]")) {
          nested = true;
          break;
        }
        parent = parent.parentElement;
      }
      if (nested || getMessageId(row) !== normalizedTurnId) continue;
      const text = String(row.textContent || "");
      const childCount = Number(row.childElementCount ?? row.children?.length ?? 0) || 0;
      return [
        String(row.getAttribute("data-turn-key") || ""),
        childCount,
        text.length,
        text.slice(-512),
      ].join("\u0000");
    }
    return "";
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
      body.${conversationRichTooltipOpenClass} [role="tooltip"] { overflow-x: hidden !important; overflow-y: auto !important; overscroll-behavior: contain; }
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
      [${sessionDeleteStateAttribute}] { display: none !important; }
      [${sessionExportAttribute}], [${tasksImportAttribute}], [${sessionDeleteAttribute}] { -webkit-app-region: no-drag !important; flex: 0 0 auto; pointer-events: auto !important; }
      [${projectImportAttribute}] { -webkit-app-region: no-drag !important; position: absolute; top: 50%; right: 62px; z-index: 35; flex: 0 0 auto; transform: translateY(-50%); opacity: 0; pointer-events: auto !important; transition: opacity .15s ease; }
      [data-app-action-sidebar-project-row][data-app-action-sidebar-project-id]:hover > [${projectImportAttribute}],
      [${projectImportAttribute}]:focus-visible,
      [${projectImportAttribute}][data-busy="true"] { opacity: .9; }
      [${projectImportAttribute}]:hover { opacity: 1 !important; }
      [data-codey-session-action-row] { display: inline-flex !important; align-items: center !important; flex: 0 0 auto !important; flex-flow: row nowrap !important; gap: 1px !important; width: auto !important; min-width: max-content !important; white-space: nowrap !important; }
      #${sidebarActionTooltipId} { position: fixed; z-index: 2147483647; max-width: min(20rem, calc(100vw - 16px)); pointer-events: none; }
      #${sessionDeletePopoverId} { -webkit-app-region: no-drag !important; position: fixed; z-index: 2147483646; width: min(248px, calc(100vw - 24px)); box-sizing: border-box; border: 1px solid rgba(127, 127, 127, .28); border-radius: 12px; padding: 13px; background: rgba(30, 31, 35, .98); color: #f7f7f8; box-shadow: 0 14px 38px rgba(0, 0, 0, .32); font: 13px/1.45 system-ui, sans-serif; }
      #${sessionDeletePopoverId}::before { content: ""; position: absolute; top: -5px; right: var(--codey-popover-arrow-right, 15px); width: 9px; height: 9px; border-left: 1px solid rgba(127, 127, 127, .28); border-top: 1px solid rgba(127, 127, 127, .28); background: rgba(30, 31, 35, .98); transform: rotate(45deg); }
      #${sessionDeletePopoverId}[data-placement="top"]::before { top: auto; bottom: -5px; border: 0; border-right: 1px solid rgba(127, 127, 127, .28); border-bottom: 1px solid rgba(127, 127, 127, .28); }
      #${sessionDeletePopoverId} .codey-session-delete-title { display: block; margin: 0 0 4px; overflow: hidden; color: inherit; font-size: 13px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
      #${sessionDeletePopoverId} .codey-session-delete-copy { margin: 0; color: rgba(235, 235, 245, .66); font-size: 12px; }
      #${sessionDeletePopoverId} .codey-session-delete-actions { display: flex; justify-content: flex-end; gap: 7px; margin-top: 12px; }
      #${sessionDeletePopoverId} button { min-width: 52px; height: 28px; border: 1px solid rgba(127, 127, 127, .28); border-radius: 7px; padding: 0 10px; background: rgba(255, 255, 255, .06); color: inherit; cursor: pointer; font: 600 12px/1 system-ui, sans-serif; }
      #${sessionDeletePopoverId} button:hover { background: rgba(255, 255, 255, .11); }
      #${sessionDeletePopoverId} button[data-danger] { border-color: rgba(239, 68, 68, .48); background: #dc2626; color: #fff; }
      #${sessionDeletePopoverId} button[data-danger]:hover { background: #ef4444; }
      #${sessionDeletePopoverId} button:focus-visible { outline: 2px solid rgba(139, 151, 255, .8); outline-offset: 1px; }
      #${sessionDeletePopoverId} button:disabled { cursor: wait; opacity: .62; }
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
    .filter((row) => row.dataset.codeyLogicalTurn !== "continuation");

  const selectedRows = () => logicalSelectionRows()
    .filter((row) => row.classList.contains(selectedClass));

  const showRuntimeToast = (message, tone = "success") => {
    const sharedToast = window.__codeyShowRuntimeToast;
    if (typeof sharedToast === "function" && sharedToast !== showRuntimeToast) {
      sharedToast(message, tone);
      return;
    }
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

  const inheritNativeButtonClass = (button, reference) => {
    const className = reference instanceof HTMLElement
      ? String(reference.getAttribute("class") || "").trim()
      : "";
    if (className) button.setAttribute("class", className);
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
    hideSidebarActionTooltip();
    sidebarActionTooltipAnchor = button;
    sidebarActionTooltipTimer = window.setTimeout(() => {
      sidebarActionTooltipTimer = 0;
      if (sidebarActionTooltipAnchor !== button) return;
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
    button.addEventListener("mouseenter", () => {
      scheduleSidebarActionTooltip(button, label, 400);
    });
    button.addEventListener("mouseleave", () => {
      if (sidebarActionTooltipAnchor === button) hideSidebarActionTooltip();
    });
    button.addEventListener("focus", () => {
      scheduleSidebarActionTooltip(button, label, 0);
    });
    button.addEventListener("blur", () => {
      if (sidebarActionTooltipAnchor === button) hideSidebarActionTooltip();
    });
    button.addEventListener("pointerdown", hideSidebarActionTooltip);
    button.addEventListener("click", hideSidebarActionTooltip);
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
        || isDeletedSidebarThread(thread)
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

  const rememberDeletedSidebarSession = (sessionId) => {
    const normalizedSessionId = normalizeThreadSessionId(sessionId);
    if (!normalizedSessionId || normalizedSessionId.startsWith("client-new-thread:")) return "";
    pendingSidebarSessionDeleteIds.delete(normalizedSessionId);
    rememberBoundedSetValue(
      deletedSidebarSessionIds,
      normalizedSessionId,
      maxSessionCacheEntries,
    );
    sidebarTitleCache.delete(normalizedSessionId);
    [...threadUpdatedAtCache.keys()].forEach((key) => {
      if (key.endsWith(`\u0000${normalizedSessionId}`)) threadUpdatedAtCache.delete(key);
    });
    [...threadUpdatedAtRequestedAt.keys()].forEach((key) => {
      if (key.endsWith(`\u0000${normalizedSessionId}`)) threadUpdatedAtRequestedAt.delete(key);
    });
    [...pendingThreadUpdatedAtRefs.keys()].forEach((key) => {
      if (key.endsWith(`\u0000${normalizedSessionId}`)) pendingThreadUpdatedAtRefs.delete(key);
    });
    [...threadRunningStateByCacheKey.keys()].forEach((key) => {
      if (!key.endsWith(`\u0000${normalizedSessionId}`)) return;
      threadRunningStateByCacheKey.delete(key);
      cancelThreadRunningRecheck(key);
    });
    return normalizedSessionId;
  };

  const isDeletedSidebarSession = (sessionId) => {
    const normalizedSessionId = normalizeThreadSessionId(sessionId);
    return deletedSidebarSessionIds.has(normalizedSessionId);
  };

  const isDeletedSidebarThread = (row) => {
    const identity = threadIdentityNode(row);
    if (!(identity instanceof HTMLElement)) return false;
    const sessionId = normalizeThreadSessionId(threadSessionIdFromRow(identity));
    const pending = pendingSidebarSessionDeleteIds.has(sessionId);
    const deleted = isDeletedSidebarSession(sessionId);
    const item = sidebarThreadListItem(row);
    if (item instanceof HTMLElement) {
      if (pending || deleted) {
        item.setAttribute(sessionDeleteStateAttribute, pending ? "pending" : "deleted");
      } else {
        item.removeAttribute?.(sessionDeleteStateAttribute);
      }
    }
    return pending || deleted;
  };

  const beginSidebarSessionDelete = (row, sessionId) => {
    const normalizedSessionId = normalizeThreadSessionId(sessionId);
    if (!normalizedSessionId || normalizedSessionId.startsWith("client-new-thread:")) return "";
    pendingSidebarSessionDeleteIds.add(normalizedSessionId);
    isDeletedSidebarThread(row);
    return normalizedSessionId;
  };

  const rollbackSidebarSessionDelete = (row, sessionId) => {
    const normalizedSessionId = normalizeThreadSessionId(sessionId);
    pendingSidebarSessionDeleteIds.delete(normalizedSessionId);
    isDeletedSidebarThread(row);
    renderCachedThreadUpdatedAt(row);
    queryWithin(
      document,
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    ).forEach((candidate) => {
      if (
        candidate instanceof HTMLElement
        && normalizeThreadSessionId(threadSessionIdFromRow(candidate)) === normalizedSessionId
      ) {
        isDeletedSidebarThread(candidate);
        if (candidate !== row) {
          renderCachedThreadUpdatedAt(candidate);
        }
      }
    });
  };

  // Codex owns and virtualizes sidebar rows. Removing one behind React's back
  // leaves its measured spacer behind and compounds the gap on every remount.
  const shouldIgnoreDeletedSidebarSessionRoot = (root = document) => (
    root instanceof HTMLElement && isDeletedSidebarThread(root)
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

  const nativeThreadWorkInProgress = (row) => {
    const statusState = nativeReactThreadStatusState(row);
    if (nativeThreadStatusTypeLooksActive(statusState?.type)) {
      return true;
    }
    const { trailing } = threadUpdatedAtPlacement(row);
    return (trailing || []).some((candidate) => nativeElementLooksLikeThreadStatus(candidate));
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

  const scheduleThreadRunningRecheck = (cacheKey, delayMs) => {
    if (!cacheKey || threadRunningRecheckTimers.has(cacheKey)) return;
    const timer = window.setTimeout(() => {
      threadRunningRecheckTimers.delete(cacheKey);
      const state = threadRunningStateByCacheKey.get(cacheKey);
      if (!state || !Number.isFinite(state.missingSince)) return;
      const remainingMs = threadRunningLossGraceMs - (Date.now() - state.missingSince);
      if (remainingMs > 0) {
        scheduleThreadRunningRecheck(cacheKey, remainingMs);
        return;
      }
      refreshTrackedThreadUpdatedTimes();
      const current = threadRunningStateByCacheKey.get(cacheKey);
      if (current === state && current?.missingSince === state.missingSince) {
        threadRunningStateByCacheKey.delete(cacheKey);
      }
    }, Math.max(0, Number(delayMs) || 0));
    threadRunningRecheckTimers.set(cacheKey, timer);
  };

  const stableThreadWorkInProgress = (
    cacheKey,
    detectedWorkInProgress,
    previouslyRunning = false,
    runningContext = {},
    now = Date.now(),
  ) => {
    if (!cacheKey) return detectedWorkInProgress;
    if (detectedWorkInProgress) {
      rememberBoundedMapValue(
        threadRunningStateByCacheKey,
        cacheKey,
        {
          ...(threadRunningStateByCacheKey.get(cacheKey) || {}),
          ...runningContext,
          missingSince: null,
        },
      );
      cancelThreadRunningRecheck(cacheKey);
      return true;
    }
    let state = threadRunningStateByCacheKey.get(cacheKey);
    if (!state && previouslyRunning) {
      state = { ...runningContext, missingSince: now };
      rememberBoundedMapValue(threadRunningStateByCacheKey, cacheKey, state);
    }
    if (!state) return false;
    if (runningContext.sessionId) state.sessionId = runningContext.sessionId;
    if (runningContext.hostId) state.hostId = runningContext.hostId;
    if (runningContext.projectListId) state.projectListId = runningContext.projectListId;
    if (!Number.isFinite(state.missingSince)) {
      state.missingSince = now;
      rememberBoundedMapValue(threadRunningStateByCacheKey, cacheKey, state);
    }
    const remainingMs = threadRunningLossGraceMs - (now - state.missingSince);
    if (remainingMs > 0) {
      scheduleThreadRunningRecheck(cacheKey, remainingMs);
      return true;
    }
    threadRunningStateByCacheKey.delete(cacheKey);
    cancelThreadRunningRecheck(cacheKey);
    return false;
  };

  const sidebarThreadTimestampState = (row, now = Date.now()) => {
    const detectedWorkInProgress = nativeThreadWorkInProgress(row);
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
    if (previous?.cacheKey && previous.cacheKey !== cacheKey) {
      const previousRunningState = threadRunningStateByCacheKey.get(previous.cacheKey);
      if (previousRunningState && !threadRunningStateByCacheKey.has(cacheKey)) {
        rememberBoundedMapValue(
          threadRunningStateByCacheKey,
          cacheKey,
          previousRunningState,
        );
      }
      threadRunningStateByCacheKey.delete(previous.cacheKey);
      cancelThreadRunningRecheck(previous.cacheKey);
    }
    const workInProgress = stableThreadWorkInProgress(
      cacheKey,
      detectedWorkInProgress,
      previous?.workInProgress === true,
      {
        hostId,
        projectListId: threadProjectListIdFromRow(row),
        sessionId,
      },
      now,
    );
    const completedWork = Boolean(
      previous
      && previous.cacheKey === cacheKey
      && previous.workInProgress
      && !workInProgress
    );
    threadWorkStateByRow.set(row, { cacheKey, workInProgress });
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
    if (isDeletedSidebarSession(state.sessionId) || !timestamp) {
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
      .filter((state) => state?.sessionId);
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
    if (threadUpdatedAtFetchInFlight || !pendingThreadUpdatedAtRefs.size) return;
    const refs = [...pendingThreadUpdatedAtRefs.values()].slice(0, maxPendingThreadTimestampRefs);
    refs.forEach(({ cacheKey }) => pendingThreadUpdatedAtRefs.delete(cacheKey));
    threadUpdatedAtFetchInFlight = true;
    try {
      const sessionIds = [...new Set(refs.map(({ sessionId }) => sessionId))];
      const result = await callBridge(threadTimestampBridgePath, { sessionIds });
      if (result?.status !== "ok" || !result.timestamps || typeof result.timestamps !== "object") {
        throw new Error(result?.message || "Codey thread timestamp bridge is unavailable");
      }
      const refreshedCacheKeys = new Set();
      refs.forEach((ref) => {
        const timestamp = threadTimestampValueToMs(result.timestamps[ref.sessionId]);
        if (isDeletedSidebarSession(ref.sessionId) || !timestamp) {
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
        if (
          refreshedCacheKeys.has(cacheKey)
          && !isDeletedSidebarSession(sessionId)
        ) renderCachedThreadUpdatedAt(row);
      });
    } catch {
      // A failed read keeps the previous label and waits for the ordinary
      // one-minute refresh. Never retry in a tight loop on the renderer thread.
      refs.forEach(({ cacheKey, sessionId }) => {
        if (!isDeletedSidebarSession(sessionId)) {
          rememberBoundedMapValue(threadUpdatedAtRequestedAt, cacheKey, Date.now());
        }
      });
    } finally {
      threadUpdatedAtFetchInFlight = false;
      if (pendingThreadUpdatedAtRefs.size) {
        threadUpdatedAtFetchTimer = window.setTimeout(() => {
          threadUpdatedAtFetchTimer = 0;
          void flushThreadUpdatedAtFetch();
        }, 40);
      }
    }
  };

  const scheduleThreadUpdatedAtFetch = () => {
    if (threadUpdatedAtFetchTimer || threadUpdatedAtFetchInFlight || !pendingThreadUpdatedAtRefs.size) return;
    threadUpdatedAtFetchTimer = window.setTimeout(() => {
      threadUpdatedAtFetchTimer = 0;
      void flushThreadUpdatedAtFetch();
    }, 40);
  };

  const refreshThreadUpdatedAtRow = (row, now, forceRefresh = false) => {
    if (!(row instanceof HTMLElement) || isDeletedSidebarThread(row)) return;
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
    const pattern = /["']((?:\.\/(?:assets\/)?|\/assets\/)(?:app-initial|app-server-manager-signals)(?:-[^"'?#/]+)?\.js(?:\?[^"']*)?)["']/g;
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
              && typeof manager.discardConversationFromCache === "function"
              && typeof manager.handleThreadDeletion === "function"
              && typeof manager.refreshRecentConversations === "function"
              && typeof manager.resumeConversation === "function"
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
    discardConversation: (sessionId) => dispatcher("unsubscribe-thread-for-host", {
      hostId: "local",
      threadId: sessionId,
    }),
    notifyConversationDeleted: (sessionId) => dispatcher(
      "handle-app-server-notification-for-host",
      {
        hostId: "local",
        notification: {
          method: "thread/deleted",
          params: { threadId: sessionId },
        },
      },
    ),
    refreshRecentConversations: () => dispatcher("refresh-recent-conversations-for-host", {
      hostId: "local",
    }),
    resumeConversation: (payload) => {
      const {
        showThreadGoalResumeConfirmation: _showThreadGoalResumeConfirmation,
        ...legacyPayload
      } = payload;
      return dispatcher("maybe-resume-conversation", {
        hostId: "local",
        ...legacyPayload,
      });
    },
  });

  const managerSessionController = (manager) => ({
    kind: "manager",
    manager,
    discardConversation: (sessionId) => manager.discardConversationFromCache(sessionId),
    notifyConversationDeleted: (sessionId) => manager.handleThreadDeletion([sessionId]),
    refreshRecentConversations: () => manager.refreshRecentConversations(),
    rehydrateRunningConversation: typeof manager.codeyRehydrateRunningConversation === "function"
      ? (payload) => manager.codeyRehydrateRunningConversation(payload)
      : null,
    resumeConversation: (payload) => manager.resumeConversation(payload),
  });

  const sessionControllerLooksUsable = (controller) => (
    controller
    && typeof controller.discardConversation === "function"
    && typeof controller.notifyConversationDeleted === "function"
    && typeof controller.refreshRecentConversations === "function"
    && typeof controller.resumeConversation === "function"
  );

  const loadCodexSessionController = async () => {
    if (sessionControllerLooksUsable(window.__codeyCodexSessionController)) {
      return window.__codeyCodexSessionController;
    }
    if (typeof window.__codeyCodexSignalDispatcher === "function") {
      const controller = legacySessionController(window.__codeyCodexSignalDispatcher);
      window.__codeyCodexSessionController = controller;
      return controller;
    }
    const signalAssetPriority = (url) => (
      url.includes("app-server-manager-signals-")
        ? 2
        : Number(url.includes("app-initial-"))
    );
    const urls = (await discoverCodexAppAssetUrls())
      .sort((left, right) => signalAssetPriority(right) - signalAssetPriority(left));
    for (const url of urls) {
      const namedSignalAsset = url.includes("app-server-manager-signals-");
      try {
        const module = typeof window.__codeyImportCodexAsset === "function"
          ? await window.__codeyImportCodexAsset(url)
          : await import(url);
        const dispatcher = signalDispatcherFromModule(module, namedSignalAsset);
        if (dispatcher) {
          window.__codeyCodexSignalDispatcher = dispatcher;
          const controller = legacySessionController(dispatcher);
          window.__codeyCodexSessionController = controller;
          return controller;
        }
        const resolver = appServerManagerResolverFromModule(module);
        const manager = resolver ? appServerManagerFromReact(resolver) : null;
        if (manager) {
          const controller = managerSessionController(manager);
          window.__codeyCodexSessionController = controller;
          return controller;
        }
      } catch {
        continue;
      }
    }
    throw new Error("Codex 会话管理接口不可用");
  };
  window.__codeyLoadCodexSessionController = loadCodexSessionController;

  const loadCodexSignalDispatcher = async () => {
    const controller = await loadCodexSessionController();
    if (controller.kind === "signals" && typeof controller.dispatcher === "function") {
      return controller.dispatcher;
    }
    throw new Error("当前 Codex 使用 AppServerManager 会话接口");
  };
  window.__codeyLoadCodexSignalDispatcher = loadCodexSignalDispatcher;

  const getCodexSessionController = async () => {
    if (sessionControllerLooksUsable(window.__codeyCodexSessionController)) {
      return window.__codeyCodexSessionController;
    }
    codexSessionControllerPromise ||= loadCodexSessionController().catch((error) => {
      codexSessionControllerPromise = null;
      throw error;
    });
    return codexSessionControllerPromise;
  };

  const readAccountRateLimits = async () => {
    const controller = await getCodexSessionController();
    if (
      controller?.kind !== "manager"
      || typeof controller.manager?.sendRequest !== "function"
    ) {
      throw new Error("当前 Codex 不支持官方额度读取接口");
    }
    // Managed ChatGPT authentication, token refresh, credential-store access,
    // and request serialization stay inside Codex's own AppServerManager.
    return controller.manager.sendRequest("account/rateLimits/read");
  };
  window.__codeyReadAccountRateLimits = readAccountRateLimits;

  const completionProbeTargetStillCurrent = (sessionId, turnId) => (
    document.visibilityState !== "hidden"
    && isTaskRunning()
    && getSessionId() === sessionId
    && getCurrentTurnId() === turnId
  );

  const completionRecoveryIsBlocked = (completionKey, now) => (
    now < (completionRecoveryStateByKey.get(completionKey)?.retryAt || 0)
  );

  const rememberCompletionRecoveryAttempt = (completionKey, retryDelayMs) => {
    const previousAttempts = completionRecoveryStateByKey.get(completionKey)?.attempts || 0;
    const attempts = previousAttempts + 1;
    const attemptsExhausted = attempts >= stuckCompletionRecoveryMaxAttempts;
    rememberBoundedMapValue(
      completionRecoveryStateByKey,
      completionKey,
      {
        attempts: attemptsExhausted ? 0 : attempts,
        retryAt: Date.now() + (
          attemptsExhausted ? stuckCompletionRecoveryResetMs : retryDelayMs
        ),
      },
      maxCompletionRecoveryKeys,
    );
  };

  const resetRunningStreamEvidence = (
    observation,
    activityRevision = "",
    renderFingerprint = "",
  ) => {
    if (!observation) return;
    observation.activityRevision = activityRevision;
    observation.renderFingerprint = renderFingerprint;
    observation.activityAdvanceCount = 0;
    observation.streamStalledSince = 0;
  };

  const observeRunningStreamDetachment = (result, turnId, now) => {
    const observation = completionRunningObservation;
    const activityRevision = typeof result?.activityRevision === "string"
      ? result.activityRevision.trim()
      : "";
    const renderFingerprint = currentTurnRenderFingerprint(turnId);
    if (!observation || !activityRevision || !renderFingerprint) {
      resetRunningStreamEvidence(observation, activityRevision, renderFingerprint);
      return { confirmed: false, renderFingerprint };
    }
    if (observation.renderFingerprint !== renderFingerprint) {
      resetRunningStreamEvidence(observation, activityRevision, renderFingerprint);
      return { confirmed: false, renderFingerprint };
    }
    if (!observation.activityRevision) {
      observation.activityRevision = activityRevision;
      return { confirmed: false, renderFingerprint };
    }
    if (observation.activityRevision === activityRevision) {
      return { confirmed: false, renderFingerprint };
    }
    observation.activityRevision = activityRevision;
    observation.activityAdvanceCount += 1;
    observation.streamStalledSince ||= now;
    return {
      confirmed: observation.activityAdvanceCount >= stuckRunningStreamMinActivityAdvances
        && now - observation.streamStalledSince >= stuckRunningStreamGraceMs,
      renderFingerprint,
    };
  };

  const probeStuckTaskCompletion = async () => {
    if (document.visibilityState === "hidden") {
      completionRunningObservation = null;
      return false;
    }
    const now = Date.now();
    if (!isTaskRunning()) {
      if (completionRunningObservation?.key) {
        completionRecoveryStateByKey.delete(completionRunningObservation.key);
      }
      completionRunningObservation = null;
      return false;
    }
    const sessionId = getSessionId();
    const turnId = getCurrentTurnId();
    if (!sessionId || !turnId) {
      completionRunningObservation = null;
      return false;
    }
    const completionKey = `${sessionId}\u0000${turnId}`;
    if (completionRunningObservation?.key !== completionKey) {
      completionRunningObservation = {
        activityAdvanceCount: 0,
        activityRevision: "",
        key: completionKey,
        renderFingerprint: currentTurnRenderFingerprint(turnId),
        since: now,
        streamStalledSince: 0,
      };
      return false;
    }
    if (now - completionRunningObservation.since < stuckCompletionGraceMs) return false;
    if (
      completionProbeInFlight
      || now < completionNextProbeAt
      || completionRecoveryIsBlocked(completionKey, now)
    ) return false;

    completionProbeInFlight = true;
    completionNextProbeAt = now + stuckCompletionProbeIntervalMs;
    let recoveryAttempted = false;
    try {
      const result = await callBridge(
        stuckCompletionBridgePath,
        { sessionId, turnId },
        { timeoutMs: stuckCompletionProbeTimeoutMs },
      );
      if (result?.status !== "ok") {
        rememberCompletionRecoveryAttempt(
          completionKey,
          stuckCompletionRecoveryCooldownMs,
        );
        return false;
      }
      const authoritativeTargetMatches = result.sessionId === sessionId
        && result.turnId === turnId
        && result.sessionKnown === true
        && result.turnKnown === true;
      if (!authoritativeTargetMatches) {
        resetRunningStreamEvidence(completionRunningObservation);
        return false;
      }
      if (result.lifecycle === "running" && result.terminal === false) {
        const streamEvidence = observeRunningStreamDetachment(result, turnId, Date.now());
        if (!streamEvidence.confirmed) return false;

        recoveryAttempted = true;
        const controller = await getCodexSessionController();
        if (typeof controller.rehydrateRunningConversation !== "function") {
          rememberCompletionRecoveryAttempt(
            completionKey,
            stuckCompletionRecoveryCooldownMs,
          );
          return false;
        }
        // Asset discovery can take long enough for React to receive another
        // stream event. Recheck both identity and the exact rendered tail before
        // clearing the stale local owner role.
        if (
          !completionProbeTargetStillCurrent(sessionId, turnId)
          || currentTurnRenderFingerprint(turnId) !== streamEvidence.renderFingerprint
        ) return false;
        const rehydrated = await controller.rehydrateRunningConversation({
          conversationId: sessionId,
          model: null,
          serviceTier: null,
          reasoningEffort: null,
          workspaceRoots: [],
          collaborationMode: null,
          showThreadGoalResumeConfirmation: false,
        });
        if (rehydrated !== true) {
          rememberCompletionRecoveryAttempt(
            completionKey,
            stuckCompletionRecoveryCooldownMs,
          );
          return false;
        }
        resetRunningStreamEvidence(
          completionRunningObservation,
          String(result.activityRevision || ""),
          currentTurnRenderFingerprint(turnId),
        );
        rememberCompletionRecoveryAttempt(
          completionKey,
          stuckCompletionRecoveryRetryMs,
        );
        return true;
      }
      resetRunningStreamEvidence(
        completionRunningObservation,
        typeof result.activityRevision === "string" ? result.activityRevision : "",
        currentTurnRenderFingerprint(turnId),
      );
      const lifecycleIsTerminal = result?.lifecycle === "idle" || result?.lifecycle === "error";
      const terminalKindIsKnown = result?.terminalKind === "completed"
        || result?.terminalKind === "aborted";
      if (
        result.terminal !== true
        || !terminalKindIsKnown
        || !lifecycleIsTerminal
        || !completionProbeTargetStillCurrent(sessionId, turnId)
      ) return false;

      recoveryAttempted = true;
      const controller = await getCodexSessionController();
      // Controller discovery can import renderer assets. Recheck the exact
      // task and turn immediately before the first native state mutation.
      if (!completionProbeTargetStillCurrent(sessionId, turnId)) return false;
      await controller.discardConversation(sessionId);
      await controller.resumeConversation({
        conversationId: sessionId,
        model: null,
        serviceTier: null,
        reasoningEffort: null,
        workspaceRoots: [],
        collaborationMode: null,
        showThreadGoalResumeConfirmation: false,
      });
      await controller.refreshRecentConversations();
      // Native refresh promises can resolve before React drops the stale Stop
      // state. Revisit the same exact task after a short grace period; clearing
      // the Stop state removes this retry record at the top of the next probe.
      rememberCompletionRecoveryAttempt(
        completionKey,
        stuckCompletionRecoveryRetryMs,
      );
      return true;
    } catch {
      rememberCompletionRecoveryAttempt(
        completionKey,
        recoveryAttempted
          ? stuckCompletionRecoveryCooldownMs
          : stuckCompletionProbeIntervalMs,
      );
      return false;
    } finally {
      completionProbeInFlight = false;
    }
  };

  const waitForNativeSessionOperation = (operation) => new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error("Codex 会话接口响应超时")), 5_000);
    Promise.resolve().then(operation).then(resolve, reject).finally(() => window.clearTimeout(timer));
  });

  const callNativeSessionOperation = async (operation) => {
    // Bound discovery separately: a late import must not start a destructive
    // native operation after the caller has already reported a timeout.
    const controller = await waitForNativeSessionOperation(getCodexSessionController);
    return waitForNativeSessionOperation(() => operation(controller));
  };

  const refreshRecentLocalSessions = async () => {
    try {
      await callNativeSessionOperation((controller) => controller.refreshRecentConversations());
      return true;
    } catch {
      return false;
    }
  };

  const unsubscribeNativeSidebarSession = async (sessionId) => {
    const normalizedSessionId = normalizeThreadSessionId(sessionId);
    if (!normalizedSessionId || normalizedSessionId.startsWith("client-new-thread:")) return false;
    try {
      await callNativeSessionOperation((controller) => {
        if (isSidebarSessionActive(normalizedSessionId)) {
          throw new Error("请先切离要删除的会话");
        }
        return controller.discardConversation(normalizedSessionId);
      });
      return true;
    } catch {
      return false;
    }
  };

  const notifyNativeSidebarSessionDeleted = async (sessionId) => {
    const normalizedSessionId = normalizeThreadSessionId(sessionId);
    if (!normalizedSessionId || normalizedSessionId.startsWith("client-new-thread:")) return false;
    try {
      await callNativeSessionOperation((controller) => {
        // The user can navigate back while the backend deletion is in flight.
        // Do not evict a conversation that became active again in that window.
        if (isSidebarSessionActive(normalizedSessionId)) {
          throw new Error("会话已重新打开，暂不清理当前页面缓存");
        }
        return controller.notifyConversationDeleted(normalizedSessionId);
      });
      return true;
    } catch {
      return false;
    }
  };

  const reloadConversationAfterHardDelete = async (sessionId, messageIds) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    if (!normalizedSessionId || !messageIds.length) throw new Error("缺少会话或轮次 ID");
    const controller = await getCodexSessionController();

    // Current Codex exposes AppServerManager through its renderer scope. Its
    // discard path unsubscribes app-server and evicts React's conversation
    // snapshot; older builds keep using the equivalent native signal.
    await controller.discardConversation(normalizedSessionId);

    // Closing a loaded thread may flush a final record. Reapply the hard delete
    // only after unsubscribe has completed so stale memory cannot restore it.
    const cleanup = await callBridge("/session/delete-messages", {
      sessionId: normalizedSessionId,
      messageIds,
    });
    if (cleanup?.status === "failed") {
      throw new Error(cleanup.message || "卸载会话后的持久化清理失败");
    }
    await controller.resumeConversation({
      conversationId: normalizedSessionId,
      model: null,
      serviceTier: null,
      reasoningEffort: null,
      workspaceRoots: [],
      collaborationMode: null,
      showThreadGoalResumeConfirmation: false,
    });
    await controller.refreshRecentConversations();
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
      deletedSidebarSessionIds.delete(normalizeThreadSessionId(result.sessionId));
      const refreshed = await refreshRecentLocalSessions();
      showRuntimeToast(result.message || "会话数据已导入");
      const importedProjectPath = result.projectPath || projectPath;
      window.dispatchEvent(new CustomEvent("codey-session-refresh", {
        detail: { sessionId: result.sessionId, projectPath: importedProjectPath, imported: true },
      }));
      if (!refreshed) window.setTimeout(() => location.reload(), 700);
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

  const closeSessionDeletePopover = () => {
    deletePopoverCleanup?.();
    deletePopoverCleanup = null;
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
    return wrapper instanceof HTMLElement && wrapper !== thread
      ? wrapper
      : archiveControl;
  };

  const positionSessionDeletePopover = (popover, anchor) => {
    const anchorRect = anchor.getBoundingClientRect();
    const popoverRect = popover.getBoundingClientRect();
    const viewportWidth = window.innerWidth || document.documentElement.clientWidth || 1024;
    const viewportHeight = window.innerHeight || document.documentElement.clientHeight || 768;
    const left = Math.min(
      viewportWidth - popoverRect.width - 12,
      Math.max(12, anchorRect.right - popoverRect.width),
    );
    const fitsBelow = anchorRect.bottom + 8 + popoverRect.height <= viewportHeight - 12;
    const top = fitsBelow
      ? anchorRect.bottom + 8
      : Math.max(12, anchorRect.top - popoverRect.height - 8);
    const arrowRight = Math.max(
      13,
      Math.min(popoverRect.width - 22, left + popoverRect.width - anchorRect.right + 7),
    );
    popover.style.left = `${left}px`;
    popover.style.top = `${top}px`;
    popover.style.setProperty("--codey-popover-arrow-right", `${arrowRight}px`);
    popover.dataset.placement = fitsBelow ? "bottom" : "top";
  };

  const isSidebarSessionActive = (sessionId) => {
    // Sidebar selection can update before the routed conversation unmounts.
    // Check the URL independently instead of letting getSessionId's DOM
    // fallback hide a route that still points at the deletion target.
    const routeSessionId = location.pathname.match(/(?:\/c\/|\/conversation\/|\/session\/)([A-Za-z0-9_-]+)/)?.[1]
      || new URLSearchParams(location.search).get("conversation_id")
      || new URLSearchParams(location.search).get("session_id");
    return normalizeThreadSessionId(getSessionId()) === sessionId
      || normalizeThreadSessionId(routeSessionId) === sessionId
      || [...document.querySelectorAll(
        "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
      )].some((thread) => (
        thread.getAttribute("data-app-action-sidebar-thread-active") === "true"
        && threadSessionIdFromRow(thread) === sessionId
      ));
  };

  const navigateAwayFromDeletedThread = (deletedThread, sessionId) => {
    const replacement = [...document.querySelectorAll(
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    )].find((thread) => (
      thread !== deletedThread
      && thread instanceof HTMLElement
      && threadSessionIdFromRow(thread) !== sessionId
      && !isDeletedSidebarThread(thread)
      && thread.getClientRects().length > 0
    ));
    if (replacement instanceof HTMLElement) {
      const target = replacement.querySelector("a[href]") || replacement;
      target.click();
      return true;
    }
    const newThreadAction = [...document.querySelectorAll("button, [role=button], a")]
      .find((control) => {
        if (!(control instanceof HTMLElement) || control.getClientRects().length === 0) return false;
        const label = `${control.getAttribute("aria-label") || ""} ${control.textContent || ""}`;
        return /新(?:建)?任务|新(?:建)?对话|\bnew task\b|\bnew chat\b/i.test(label);
      });
    if (newThreadAction instanceof HTMLElement) {
      newThreadAction.click();
      return true;
    }
    return false;
  };

  const leaveSidebarSessionBeforeDelete = async (thread, sessionId) => {
    if (!isSidebarSessionActive(sessionId)) return;
    if (!navigateAwayFromDeletedThread(thread, sessionId)) {
      throw new Error("无法切离当前会话，请先打开其他会话后再删除");
    }
    // A click can start an asynchronous React navigation. Never evict the
    // conversation snapshot until both the page and sidebar have left it.
    for (let attempt = 0; attempt < 30; attempt += 1) {
      if (!isSidebarSessionActive(sessionId)) return;
      await new Promise((resolve) => window.setTimeout(resolve, 50));
    }
    throw new Error("切换会话尚未完成，未执行删除，请稍后重试");
  };

  const isSessionAlreadyDeletedMessage = (value) => (
    /Thread not found in local storage/i.test(String(value || ""))
  );

  const completeSidebarSessionDelete = (
    thread,
    sessionId,
    title,
    alreadyDeleted,
    nativeDeletionNotified,
  ) => {
    const normalizedSessionId = rememberDeletedSidebarSession(sessionId) || sessionId;
    isDeletedSidebarThread(thread);
    closeSessionDeletePopover();
    window.dispatchEvent(new CustomEvent("codey-session-deleted", {
      detail: { sessionId: normalizedSessionId, title, alreadyDeleted },
    }));
    showRuntimeToast(
      alreadyDeleted
        ? `会话${title ? `“${title}”` : ""}已不存在，已从列表移除`
        : `已删除会话${title ? `“${title}”` : ""}`,
    );
    void refreshRecentLocalSessions().then((refreshed) => {
      if (!nativeDeletionNotified || !refreshed) {
        showRuntimeToast("会话已删除，列表同步暂未完成；可稍后手动刷新");
      }
    });
  };

  const deleteSidebarSession = async (thread, anchor, confirmButton, target) => {
    const { sessionId, title } = target;
    if (!sessionId || sessionId.startsWith("client-new-thread:")) {
      closeSessionDeletePopover();
      showRuntimeToast("无法识别要删除的会话", "error");
      return;
    }
    // Virtualized rows may be reused while the confirmation remains open.
    // The confirmed identity must never be read again from a reused row.
    if (threadSessionIdFromRow(thread) !== sessionId) {
      closeSessionDeletePopover();
      showRuntimeToast("会话列表已更新，请重新确认要删除的会话", "error");
      return;
    }
    if (pendingSidebarSessionDeleteIds.has(sessionId) || isDeletedSidebarSession(sessionId)) return;
    const isActive = isSidebarSessionActive(sessionId);
    if (isActive && isTaskRunning()) {
      closeSessionDeletePopover();
      showRuntimeToast("当前会话仍在运行，请停止任务后再删除", "error");
      return;
    }

    confirmButton.disabled = true;
    confirmButton.textContent = "删除中…";
    anchor.setAttribute("aria-busy", "true");
    beginSidebarSessionDelete(thread, sessionId);
    closeSessionDeletePopover();
    try {
      await leaveSidebarSessionBeforeDelete(thread, sessionId);
      if (!await unsubscribeNativeSidebarSession(sessionId)) {
        throw new Error("Codex 尚未释放会话，未执行删除，请稍后重试");
      }
      if (isSidebarSessionActive(sessionId)) {
        throw new Error("要删除的会话已重新打开，未执行删除，请切离后重试");
      }
      const result = await callBridge("/session/delete", { sessionId, title });
      const alreadyDeleted = isSessionAlreadyDeletedMessage(result?.message);
      if (
        (result?.status !== "ok" || result?.deleted !== true)
        && !alreadyDeleted
      ) {
        throw new Error(result?.message || "未知错误");
      }
      const nativeDeletionNotified = await notifyNativeSidebarSessionDeleted(sessionId);
      completeSidebarSessionDelete(
        thread,
        sessionId,
        title,
        alreadyDeleted,
        nativeDeletionNotified,
      );
    } catch (error) {
      if (isSessionAlreadyDeletedMessage(error instanceof Error ? error.message : error)) {
        const nativeDeletionNotified = await notifyNativeSidebarSessionDeleted(sessionId);
        completeSidebarSessionDelete(
          thread,
          sessionId,
          title,
          true,
          nativeDeletionNotified,
        );
        return;
      }
      rollbackSidebarSessionDelete(thread, sessionId);
      confirmButton.disabled = false;
      confirmButton.textContent = "删除";
      showRuntimeToast(
        `删除失败：${error instanceof Error ? error.message : String(error)}`,
        "error",
      );
    } finally {
      anchor.removeAttribute("aria-busy");
    }
  };

  const openSessionDeletePopover = (thread, anchor) => {
    closeSessionDeletePopover();
    const target = {
      sessionId: threadSessionIdFromRow(thread),
      title: String(thread.getAttribute("data-app-action-sidebar-thread-title") || "").trim(),
    };
    const title = target.title || "未命名会话";
    const popover = document.createElement("div");
    popover.id = sessionDeletePopoverId;
    popover.setAttribute("role", "dialog");
    popover.setAttribute("aria-modal", "false");
    popover.setAttribute("aria-label", "确认删除会话");

    const heading = document.createElement("strong");
    heading.className = "codey-session-delete-title";
    heading.textContent = `删除“${title}”？`;
    const copy = document.createElement("p");
    copy.className = "codey-session-delete-copy";
    copy.textContent = "会话及本地记录将被删除，此操作无法在会话列表中撤销。";
    const actions = document.createElement("div");
    actions.className = "codey-session-delete-actions";
    const cancelButton = document.createElement("button");
    cancelButton.type = "button";
    cancelButton.textContent = "取消";
    const confirmButton = document.createElement("button");
    confirmButton.type = "button";
    confirmButton.setAttribute("data-danger", "true");
    confirmButton.setAttribute("data-codey-session-delete-confirm", "true");
    confirmButton.textContent = "删除";
    actions.append(cancelButton, confirmButton);
    popover.append(heading, copy, actions);
    document.body.appendChild(popover);
    anchor.setAttribute("aria-expanded", "true");
    positionSessionDeletePopover(popover, anchor);

    const close = () => {
      document.removeEventListener("pointerdown", onOutsidePointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", close, true);
      window.removeEventListener("scroll", close, true);
      anchor.setAttribute("aria-expanded", "false");
      popover.remove();
      if (deletePopoverCleanup === close) deletePopoverCleanup = null;
    };
    const onOutsidePointerDown = (event) => {
      const path = event.composedPath?.() || [];
      if (!path.includes(popover) && !path.includes(anchor)) close();
    };
    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
        anchor.focus();
      }
    };
    deletePopoverCleanup = close;
    cancelButton.addEventListener("click", close);
    confirmButton.addEventListener("click", () => {
      void deleteSidebarSession(thread, anchor, confirmButton, target);
    });
    window.setTimeout(() => {
      if (deletePopoverCleanup !== close) return;
      document.addEventListener("pointerdown", onOutsidePointerDown, true);
      document.addEventListener("keydown", onKeyDown, true);
      window.addEventListener("resize", close, true);
      window.addEventListener("scroll", close, true);
      confirmButton.focus();
    }, 0);
  };

  const installSessionDeleteButtons = (root = document) => {
    if (shouldIgnoreDeletedSidebarSessionRoot(root)) return;
    queryWithin(root,
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    ).forEach((thread) => {
      if (
        !(thread instanceof HTMLElement)
        || isDeletedSidebarThread(thread)
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
      button.setAttribute("aria-label", "删除会话");
      button.setAttribute("aria-haspopup", "dialog");
      button.setAttribute("aria-expanded", "false");
      inheritNativeButtonClass(button, archiveControl);
      button.innerHTML = sessionDeleteIcon;
      attachSidebarActionTooltip(button, "删除会话");
      ["pointerdown", "mousedown", "mouseup", "touchstart"].forEach((eventName) => {
        button.addEventListener(eventName, stopSidebarActionEvent, true);
      });
      button.addEventListener("click", (event) => {
        stopSidebarActionEvent(event);
        if (button.getAttribute("aria-expanded") === "true") {
          closeSessionDeletePopover();
          return;
        }
        openSessionDeletePopover(thread, button);
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
    const rows = [...document.querySelectorAll("[data-codey-message-id]")];
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
    const rows = selectedRows();
    const sessionId = getSessionId();
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
    showRuntimeToast(`正在永久删除 ${logicalCount} 轮对话…`);
    let result;
    try {
      result = await callBridge("/session/delete-messages", { sessionId, messageIds });
    } catch (error) {
      const message = typeof error?.message === "string" ? error.message : String(error);
      window.alert(`删除失败：${message}`);
      return;
    }
    if (result?.status === "failed") {
      window.alert(`删除失败：${result.message || "未知错误"}`);
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
    forgetLogicalMessageSelections(sessionId, [...messageIds, ...resolvedMessageIds]);
    forgetLogicalMessageIds(sessionId, [...messageIds, ...resolvedMessageIds]);
    rememberHardDeletedMessages(sessionId, [...messageIds, ...resolvedMessageIds]);
    physicalRows.forEach((row) => row.remove());
    lastSelectedRow = null;
    syncSelectionGroups();
    updateToolbar();
    try {
      await reloadConversationAfterHardDelete(sessionId, resolvedMessageIds);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      window.alert(`消息已从会话文件永久删除，但 Codex 内存会话卸载失败。\n请重启 Codex 后再继续对话。\n\n${message}`);
      return;
    }
    window.dispatchEvent(new CustomEvent("codey-session-refresh", {
      detail: { sessionId, messageIds: resolvedMessageIds },
    }));
    showRuntimeToast(`已永久删除 ${logicalCount} 轮对话`);
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
      return hasCanonicalTurnAncestor(root) ? [] : [root];
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
    return candidates.filter((row) => canonicalRows.has(row) || fallbackRows.has(row));
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

  const installMessageSelection = (root = document) => {
    mountToolbar();
    if (lastSelectedRow?.isConnected === false) lastSelectedRow = null;
    let rows = messageSelectionRowsWithin(root);
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
      const documentRows = messageSelectionRowsWithin(document);
      if (documentRows.includes(root)) rows = documentRows;
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
    if (shouldIgnoreDeletedSidebarSessionRoot(root)) return;
    // Streaming output makes conversation turns by far the most frequent scan
    // root. Sidebar controls can never live inside a turn, so running their
    // installers there is a guaranteed-miss walk of the whole turn subtree.
    if (
      root instanceof HTMLElement
      && root.matches?.(conversationTurnSelector)
      && !root.matches?.(sidebarScanRootSelector)
    ) {
      installMessageSelection(root);
      return;
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
  window.__codeyProbeStuckTaskCompletion = probeStuckTaskCompletion;
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
  window.__codeyOpenSessionDeletePopover = openSessionDeletePopover;
  window.__codeyPruneDeletedSidebarSessions = shouldIgnoreDeletedSidebarSessionRoot;
  window.__codeySyncSelectionGroups = syncSelectionGroups;
  window.__codeyDeleteSelectedMessages = deleteSelected;
  window.__codeyReloadConversationAfterHardDelete = reloadConversationAfterHardDelete;
  window.__codeyInstallMessageSelection = installMessageSelection;
  addStyle();

  const codeyOwnedSelector = [
    rendererSettingsButtonSelector,
    `#${toolbarId}`,
    `#${toastId}`,
    `#${sessionDeletePopoverId}`,
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

  const isCodeyOwned = (element) => (
    element instanceof HTMLElement
    && (
      element.matches?.(codeyOwnedSelector)
      || element.closest?.(codeyOwnedSelector)
    )
  );
  const containsRelevantElement = (element) => (
    element instanceof HTMLElement
    && (
      element.matches?.(relevantAddedSelector)
      || element.querySelector?.(relevantAddedSelector)
    )
  );
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
  const addPendingScanRoot = (root) => {
    if (!(root instanceof HTMLElement)) return;
    // Header mounts must stay fresh even while the root budget is saturated,
    // otherwise the settings button can go stale for a whole mutation storm.
    if (root.matches?.("header, nav")) {
      window.__codeyRendererInvalidateHeaderMount?.(root);
    }
    // Unknown/new Codex DOM shapes must degrade by skipping optional controls,
    // not by turning a burst of virtualized rows into a synchronous full-page
    // scan on the renderer thread.
    if (pendingScanRoots.size >= maxPendingScanRoots) return;
    for (const pendingRoot of pendingScanRoots) {
      if (pendingRoot === root || pendingRoot.contains?.(root)) return;
      if (root.contains?.(pendingRoot)) pendingScanRoots.delete(pendingRoot);
    }
    pendingScanRoots.add(root);
  };
  const flushIncrementalScans = () => {
    scanTimer = 0;
    scanDeadline = 0;
    const roots = [...pendingScanRoots]
      .filter((root) => root.isConnected !== false)
      .filter((root, index, candidates) => !candidates.some((
        candidate,
        candidateIndex,
      ) => candidateIndex !== index && candidate.contains?.(root)));
    pendingScanRoots.clear();
    roots.forEach((root) => scan(root, true));
  };
  const scheduleIncrementalScan = (root) => {
    addPendingScanRoot(root);
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
      window.requestIdleCallback(run);
    } else {
      window.setTimeout(run, 0);
    }
  };

  const isConversationRichTooltipTriggerShape = (element) => (
    Boolean(element?.matches?.(conversationRichTooltipTriggerSelector))
    && Boolean(element.closest?.(conversationTurnSelector))
  );
  const conversationHasOpenRichTooltip = () => {
    const turns = document.querySelectorAll?.(conversationTurnSelector);
    if (!turns) return false;
    for (const turn of turns) {
      const candidates = turn.querySelectorAll?.(conversationRichTooltipTriggerSelector);
      if (!candidates) continue;
      for (const candidate of candidates) {
        if (candidate.hasAttribute?.("aria-describedby")) return true;
      }
    }
    return false;
  };
  const syncConversationRichTooltipOpen = (target) => {
    const body = document.body;
    if (!body?.classList || typeof body.classList.toggle !== "function") return;
    if (target && isConversationRichTooltipTriggerShape(target)) {
      if (target.hasAttribute?.("aria-describedby")) {
        body.classList.add(conversationRichTooltipOpenClass);
        return;
      }
      if (!body.classList.contains(conversationRichTooltipOpenClass)) return;
    } else if (target) {
      return;
    }
    body.classList.toggle(conversationRichTooltipOpenClass, conversationHasOpenRichTooltip());
  };

  const handleSessionToolMutations = (mutations) => {
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (mutation.attributeName === "aria-describedby") {
          syncConversationRichTooltipOpen(target);
          continue;
        }
        if (target && !isCodeyOwned(target)) {
          const threadRow = target.closest?.(sidebarThreadRowSelector) || null;
          const relevantThreadClassChange = threadRow
            && mutation.attributeName === "class"
            && threadClassMutationMayAffectStatus(target, threadRow, mutation.oldValue);
          if (relevantThreadClassChange || mutation.attributeName !== "class") {
            addPendingScanRoot(threadRow || nearestScanRoot(target));
          }
        }
        continue;
      }
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
            addPendingScanRoot(root);
          }
          continue;
        }
        if (isCodeyOwned(element)) continue;
        const threadRow = element.closest?.(sidebarThreadRowSelector)
          || target?.closest?.(sidebarThreadRowSelector)
          || null;
        if (threadRow) {
          addPendingScanRoot(threadRow);
          continue;
        }
        if (!containsRelevantElement(element)) continue;
        addPendingScanRoot(addedSubtreeScanRoot(element));
      }
      for (const node of mutation.removedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) continue;
        const threadRow = target?.closest?.(sidebarThreadRowSelector) || null;
        if (threadRow && !isCodeyOwned(target)) {
          addPendingScanRoot(threadRow);
          continue;
        }
        if (!containsRelevantElement(element)) continue;
        if (target && !isCodeyOwned(target)) {
          const topologyRoot = addedSubtreeScanRoot(target) || nearestScanRoot(target);
          messageSelectionRowsWithin(topologyRoot).forEach((row) => {
            delete row.dataset.codeySelectionSignature;
          });
          addPendingScanRoot(topologyRoot);
        }
      }
    }
    if (pendingScanRoots.size) {
      scheduleIncrementalScan(null);
    }
  };
  const sessionToolMutationOptions = {
    attributes: true,
    attributeOldValue: true,
    attributeFilter: [
      "aria-label",
      "aria-expanded",
      "aria-hidden",
      "aria-describedby",
      "data-turn-key",
      "data-request-user-input-auto-resolution-conversation-id",
      "data-app-action-sidebar-thread-host-id",
      "data-app-action-sidebar-thread-id",
      "data-app-action-sidebar-thread-kind",
      "data-app-action-sidebar-thread-title",
      "data-app-action-sidebar-project-id",
      "data-app-action-sidebar-project-list-id",
      "data-app-action-sidebar-project-row",
      sidebarProjectShowAllAttribute,
      "data-testid",
      "disabled",
      "hidden",
      "class",
    ],
    childList: true,
    subtree: true,
  };
  const mutationDispatcher = window.__codeyMutationDispatcher;
  let sessionToolObserver = null;
  if (typeof mutationDispatcher?.subscribe === "function") {
    const unsubscribe = mutationDispatcher.subscribe(
      handleSessionToolMutations,
      sessionToolMutationOptions,
    );
    if (mutationDispatcher.snapshot?.().observerInstalled) {
      sessionToolObserver = { disconnect: unsubscribe };
    } else {
      unsubscribe?.();
    }
  }
  if (!sessionToolObserver) {
    sessionToolObserver = new MutationObserver(handleSessionToolMutations);
    sessionToolObserver.observe(document.documentElement, sessionToolMutationOptions);
  }
  syncConversationRichTooltipOpen();
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
  if (typeof document.addEventListener === "function") {
    document.addEventListener("visibilitychange", wakeSessionWatcher);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState !== "hidden") {
        refreshThreadUpdatedTimesOnReturn();
        void probeStuckTaskCompletion();
      }
    });
    document.addEventListener("pointerdown", wakeSessionWatcher, { capture: true, passive: true });
    document.addEventListener("keydown", wakeSessionWatcherFromKey, true);
  }
  if (typeof window.addEventListener === "function") {
    window.addEventListener("focus", wakeSessionWatcher);
    window.addEventListener("focus", refreshThreadUpdatedTimesOnReturn);
    window.addEventListener("focus", probeStuckTaskCompletion);
    window.addEventListener("pageshow", wakeSessionWatcher);
    window.addEventListener("pageshow", refreshThreadUpdatedTimesOnReturn);
    window.addEventListener("pageshow", probeStuckTaskCompletion);
  }
  if (typeof window.setInterval === "function") {
    window.setInterval(() => {
      void probeStuckTaskCompletion();
    }, stuckCompletionProbeIntervalMs);
    window.setInterval(() => {
      if (document.visibilityState === "hidden") return;
      refreshTrackedThreadUpdatedTimes(false);
    }, threadTimestampRefreshIntervalMs);
  }
  window.__codeyRendererInjectLoaded = true;
  window.__codeySessionToolsInjectLoaded = true;
  window.__codeySessionToolsInjectLoading = false;
  void probeStuckTaskCompletion();
  scheduleInitialScan();
})();
