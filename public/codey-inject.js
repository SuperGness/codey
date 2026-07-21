// Backwards-compatible entry point for older Codey builds. The launcher now
// injects the smaller renderer-inject.js/fast-mode-fix.js/plugin-marketplace-fix.js
// modules separately, but keeping this file useful makes manual CDP testing
// straightforward.
(() => {
  if (window.__codeyRendererInjectLoaded) return;
  window.__codeyRendererInjectLoaded = true;
  const buttonId = "codey-settings-button";
  const toolbarId = "codey-message-toolbar";
  const toastId = "codey-runtime-toast";
  const styleId = "codey-injected-style";
  const selectedClass = "codey-message-selected";
  const sessionExportAttribute = "data-codey-session-export";
  const tasksImportAttribute = "data-codey-tasks-import";
  const projectImportAttribute = "data-codey-project-import";
  const sessionDeleteAttribute = "data-codey-session-delete";
  const sessionDeletePopoverId = "codey-session-delete-popover";
  const sidebarActionTooltipId = "codey-sidebar-action-tooltip";
  const threadStatusAttribute = "data-codey-thread-traffic-status";
  const settingsIcon = `
    <svg viewBox="0 0 350 350" aria-hidden="true" focusable="false">
      <path d="M70 301c-16 0-24-18-13-30l73-77c8-8 8-20 0-28L65 101C50 86 57 61 78 57c9-2 18 1 25 8l91 91c18 18 18 46 0 64l-66 66c-6 6-2 15 7 15h183" />
    </svg>
  `;
  const sessionExportIcon = `
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
      <polyline points="7 10 12 15 17 10"></polyline>
      <line x1="12" x2="12" y1="15" y2="3"></line>
    </svg>
  `;
  const projectImportIcon = `
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"></path>
      <polyline points="17 8 12 3 7 8"></polyline>
      <line x1="12" x2="12" y1="3" y2="15"></line>
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
  const sidebarTitleCache = new Map();
  let watcherWakeTimer = 0;
  let deletePopoverCleanup = null;
  let codexSignalDispatcherPromise = null;
  let sidebarActionTooltipTimer = 0;
  let sidebarActionTooltipAnchor = null;
  const hardDeletedMessageKeys = new Set();
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

  const callBridge = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload);
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
    const titles = sidebarTitles(root).filter(({ sessionId, title }) => (
      sidebarTitleCache.get(sessionId) !== title
    ));
    if (!titles.length) return;
    const previousTitles = titles.map(({ sessionId }) => (
      [sessionId, sidebarTitleCache.get(sessionId)]
    ));
    titles.forEach(({ sessionId, title }) => sidebarTitleCache.set(sessionId, title));
    void callBridge("/session/titles", { titles })
      .then((result) => {
        if (result?.status !== "failed") return;
        previousTitles.forEach(([sessionId, previousTitle], index) => {
          if (sidebarTitleCache.get(sessionId) !== titles[index].title) return;
          if (previousTitle === undefined) sidebarTitleCache.delete(sessionId);
          else sidebarTitleCache.set(sessionId, previousTitle);
        });
      })
      .catch(() => {
        previousTitles.forEach(([sessionId, previousTitle], index) => {
          if (sidebarTitleCache.get(sessionId) !== titles[index].title) return;
          if (previousTitle === undefined) sidebarTitleCache.delete(sessionId);
          else sidebarTitleCache.set(sessionId, previousTitle);
        });
      });
  };

  const wakeSessionWatcher = () => {
    if (document.visibilityState === "hidden" || watcherWakeTimer) return;
    void callBridge("/session/wake-watcher").catch(() => {});
    watcherWakeTimer = window.setTimeout(() => {
      watcherWakeTimer = 0;
    }, 3_000);
  };

  const wakeSessionWatcherFromKey = (event) => {
    if (event.key === "Enter" && !event.isComposing) wakeSessionWatcher();
  };

  const getMessageId = (row) => {
    const direct = ["data-turn-key", "data-message-id", "data-messageid", "data-item-id", "data-id"]
      .map((key) => row.getAttribute(key)).find(Boolean);
    if (direct) return direct;
    const child = row.querySelector("[data-turn-key], [data-message-id], [data-item-id], [data-id]");
    return child?.getAttribute("data-turn-key") || child?.getAttribute("data-message-id") || child?.getAttribute("data-item-id") || child?.getAttribute("data-id") || "";
  };

  const hardDeletedMessageKey = (sessionId, messageId) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    const normalizedMessageId = String(messageId || "").trim();
    return normalizedSessionId && normalizedMessageId
      ? `${normalizedSessionId}\u0000${normalizedMessageId}`
      : "";
  };

  const rememberHardDeletedMessages = (sessionId, messageIds) => {
    messageIds.forEach((messageId) => {
      const key = hardDeletedMessageKey(sessionId, messageId);
      if (key) hardDeletedMessageKeys.add(key);
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
      #${buttonId} { -webkit-app-region: no-drag !important; pointer-events: auto !important; position: relative; z-index: 2147483641; display: inline-grid; place-items: center; flex: 0 0 auto; width: 32px; height: 32px; border: 0; border-radius: 8px; padding: 0; margin-inline-start: 8px; margin-inline-end: 18px; background: transparent; color: inherit; cursor: pointer; opacity: .86; user-select: none; transition: background .15s ease, opacity .15s ease, transform .15s ease; }
      #${buttonId}[data-codey-header-actions="true"] { width: 28px; height: 28px; margin-inline-start: 0; margin-inline-end: 6px; }
      #${buttonId}:hover { background: rgba(127, 127, 127, .14); opacity: 1; }
      #${buttonId}:active { transform: translateY(1px); }
      #${buttonId}:focus-visible { outline: 2px solid rgba(139, 151, 255, .72); outline-offset: 2px; }
      #${buttonId} svg { display: block; width: 19px; height: 19px; fill: none; stroke: currentColor; stroke-width: 22; stroke-linecap: round; stroke-linejoin: round; }
      #${buttonId} .codey-settings-label { position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0; }
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
      [data-turn-key]:hover > [data-codey-message-select], [data-codey-message-select]:focus-visible, [data-codey-message-select][aria-pressed="true"] { opacity: 1; }
      [data-codey-message-select]:hover { transform: scale(1.06); }
      [data-codey-message-select][aria-pressed="true"] { background: #5968de; border-color: #a5aeff; color: white; }
      @media (max-width: 760px) { [data-codey-message-select] { left: 4px; top: -34px; } }
      #${toastId} { -webkit-app-region: no-drag !important; position: fixed; right: 20px; bottom: 22px; z-index: 2147483645; max-width: 360px; border: 1px solid rgba(124, 140, 255, .4); border-radius: 11px; padding: 10px 13px; background: rgba(20, 24, 36, .97); color: #eef2ff; box-shadow: 0 12px 36px rgba(0,0,0,.4); font: 12px/1.45 system-ui, sans-serif; }
      #${toastId}[data-tone="error"] { border-color: rgba(248, 113, 113, .6); color: #fecaca; }
      [data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title],
      [data-app-action-sidebar-project-row][data-app-action-sidebar-project-id] { position: relative; }
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}]::after { content: ""; position: absolute; top: 50%; right: 10px; z-index: 10; display: block; width: 8px; height: 8px; border-radius: 50%; transform: translateY(-50%); pointer-events: none; }
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}="running"]::after { background: #22c55e; box-shadow: 0 0 0 3px rgba(34, 197, 94, .16); animation: codey-thread-status-blink 1.1s ease-in-out infinite; }
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}="error"]::after { background: #ef4444; box-shadow: 0 0 0 3px rgba(239, 68, 68, .14); }
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}="waiting"]::after { background: #eab308; box-shadow: 0 0 0 3px rgba(234, 179, 8, .16); }
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}] [data-hover-card-open-immediately][class*="group-hover:hidden"] { visibility: hidden !important; }
      [data-codey-thread-status-indicator], [data-codey-thread-status-owned-host] { display: none !important; }
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}]:hover::after,
      [data-app-action-sidebar-thread-row][${threadStatusAttribute}]:has(:focus-visible)::after { display: none !important; }
      @keyframes codey-thread-status-blink { 0%, 100% { opacity: 1; } 50% { opacity: .24; } }
      @media (prefers-reduced-motion: reduce) { [data-app-action-sidebar-thread-row][${threadStatusAttribute}="running"]::after { animation: none; } }
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

  const openSettings = () => {
    if (window.__codeySettingsOverlay?.toggle) {
      window.__codeySettingsOverlay.toggle();
      return;
    }
    const detail = String(window.__codeyOverlayError || "").split("\n")[0];
    window.alert(detail
      ? `Codey 内嵌配置面板加载失败：${detail}`
      : "Codey 内嵌配置面板尚未加载，请退出 Codex 后重新启动 Codey");
  };

  const isVisibleMountTarget = (element) => {
    if (!(element instanceof HTMLElement)) return false;
    if (element.closest("[hidden], [aria-hidden=true]")) return false;
    const style = window.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && rect.width > 0
      && rect.height > 0;
  };

  const findHeaderMount = () => {
    const header = [...document.querySelectorAll("header")].find(isVisibleMountTarget)
      || [...document.querySelectorAll("nav")].find(isVisibleMountTarget)
      || (isVisibleMountTarget(document.querySelector("main")?.firstElementChild)
        ? document.querySelector("main").firstElementChild
        : null);
    if (!header) return null;

    const controls = [...header.querySelectorAll("button, [role=button], a[href]")]
      .filter((control) => control.id !== buttonId && isVisibleMountTarget(control));
    const rightmostControl = controls.reduce((rightmost, control) => (
      !rightmost || control.getBoundingClientRect().right > rightmost.getBoundingClientRect().right
        ? control
        : rightmost
    ), null);
    if (!rightmostControl) return { header, target: header };

    let headerChild = rightmostControl;
    while (headerChild.parentElement && headerChild.parentElement !== header) {
      headerChild = headerChild.parentElement;
    }
    const headerRect = header.getBoundingClientRect();
    const childRect = headerChild.getBoundingClientRect();
    const hasTrailingActionRegion = headerChild !== rightmostControl
      && childRect.width <= 240
      && childRect.right >= headerRect.right - 24;
    return {
      header,
      target: header,
      before: hasTrailingActionRegion ? headerChild : null,
    };
  };

  const mountButton = () => {
    addStyle();
    const mount = findHeaderMount();
    if (!mount) return;
    let button = document.getElementById(buttonId);
    if (!button) {
      button = document.createElement("button");
      button.id = buttonId;
      button.type = "button";
      button.setAttribute("aria-label", "打开 Codey 配置");
      button.innerHTML = `${settingsIcon}<span class="codey-settings-label">Codey</span>`;
      button.title = "打开 Codey 配置";
      button.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        openSettings();
      }, true);
    }
    if (mount.before) {
      button.dataset.codeyHeaderActions = "true";
    } else {
      delete button.dataset.codeyHeaderActions;
    }
    if (mount.before) {
      if (button.parentElement !== mount.target || button.nextElementSibling !== mount.before) {
        mount.target.insertBefore(button, mount.before);
      }
    } else if (button.parentElement !== mount.target) {
      mount.target.appendChild(button);
    }
  };

  const selectedRows = () => [...document.querySelectorAll(`.${selectedClass}[data-codey-message-id]`)];

  const showRuntimeToast = (message, tone = "success") => {
    document.getElementById(toastId)?.remove();
    const toast = document.createElement("div");
    toast.id = toastId;
    toast.dataset.tone = tone;
    toast.textContent = message;
    document.documentElement.appendChild(toast);
    window.setTimeout(() => toast.remove(), tone === "error" ? 8000 : 3500);
  };

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
      if (sidebarActionTooltipAnchor !== button || button.getClientRects().length === 0) return;
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

  const downloadSessionFallback = (filename, data) => {
    const blob = new Blob([data], { type: "application/json;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  const saveSessionData = async (filename, data) => {
    if (!filename || typeof data !== "string") throw new Error("导出结果不完整");
    if (typeof window.showSaveFilePicker !== "function") {
      downloadSessionFallback(filename, data);
      return "saved";
    }
    try {
      const handle = await window.showSaveFilePicker({
        suggestedName: filename,
        types: [{
          description: "Codey 会话数据",
          accept: { "application/json": [".json"] },
        }],
      });
      const writable = await handle.createWritable();
      await writable.write(data);
      await writable.close();
      return "saved";
    } catch (error) {
      if (error?.name === "AbortError") return "cancelled";
      throw error;
    }
  };

  const exportSession = async (thread, button) => {
    const sessionId = String(thread.getAttribute("data-app-action-sidebar-thread-id") || "")
      .replace(/^local:/, "")
      .trim();
    if (!sessionId) {
      showRuntimeToast("导出失败：无法识别会话 ID", "error");
      return;
    }
    button.disabled = true;
    button.dataset.busy = "true";
    try {
      const result = await callBridge("/session/export", { sessionId });
      if (result?.status === "failed") {
        throw new Error(result.message || "未知错误");
      }
      if (result?.status !== "exported" || !result.filename || typeof result.data !== "string") {
        throw new Error("导出结果不完整");
      }
      const saved = await saveSessionData(result.filename, result.data);
      if (saved === "saved") showRuntimeToast(result.message || "会话数据已导出");
    } catch (error) {
      showRuntimeToast(`导出失败：${error instanceof Error ? error.message : String(error)}`, "error");
    } finally {
      button.disabled = false;
      delete button.dataset.busy;
    }
  };

  const installSessionExportButtons = (root = document) => {
    queryWithin(root,
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    ).forEach((thread) => {
      if (!(thread instanceof HTMLElement) || thread.querySelector(`[${sessionExportAttribute}]`)) return;
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
      if (heading !== "tasks" && localizedHeading !== "任务" && localizedHeading !== "tasks") return;
      const titleRow = sectionToggle?.parentElement?.parentElement?.parentElement;
      if (!(titleRow instanceof HTMLElement)) return;
      const headerControls = [...titleRow.querySelectorAll("button, [role=button]")]
        .filter((control) => control instanceof HTMLElement && control !== sectionToggle);
      const optionsControl = headerControls.find((control) => {
        const label = String(control.getAttribute("aria-label") || "").trim();
        return /任务侧边栏选项|task sidebar options/i.test(label);
      });
      const newTaskControl = headerControls.find((control) => {
        const label = String(control.getAttribute("aria-label") || "").trim();
        return /新建任务|new task/i.test(label);
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
      optionsControl.insertAdjacentElement("afterend", button);
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

  const threadStatusFromRow = (row) => {
    const sessionId = threadSessionIdFromRow(row);
    const hostStatuses = window.__codeyHostThreadStatuses;
    if (hostStatuses && typeof hostStatuses === "object") {
      if (Object.prototype.hasOwnProperty.call(hostStatuses, sessionId)) {
        const hostStatus = hostStatuses[sessionId];
        return ["running", "error", "waiting"].includes(hostStatus) ? hostStatus : "";
      }
      if (window.__codeyHostThreadStatusesAuthoritative === true) return "";
    }
    const reactKey = Object.keys(row).find((key) => (
      key.startsWith("__reactFiber$") || key.startsWith("__reactInternalInstance$")
    ));
    let fiber = reactKey ? row[reactKey] : null;
    let running = false;
    let waiting = false;
    let hasExplicitStatus = false;
    for (let depth = 0; fiber && depth < 14; depth += 1, fiber = fiber.return) {
      const propsList = [fiber.memoizedProps, fiber.pendingProps];
      for (const props of propsList) {
        if (!props || typeof props !== "object") continue;
        const statusType = props.statusState?.type;
        if (typeof statusType === "string" && statusType) hasExplicitStatus = true;
        if (statusType === "error") return "error";
        if (statusType === "loading") running = true;
        if (props.hasPendingChildApproval === true || props.statusPill != null) waiting = true;
        if (Array.isArray(props.chips)
          && props.chips.some((chip) => chip?.id === "awaiting-approval")) {
          waiting = true;
        }
      }
    }
    if (waiting) return "waiting";
    if (running) return "running";
    if (hasExplicitStatus) return "";
    if (row.querySelector(".animate-spin")) return "running";
    const nativeStatusHost = [...row.querySelectorAll("[data-hover-card-open-immediately]")]
      .find((node) => String(node.getAttribute("class") || "").includes("group-hover:hidden"));
    if (nativeStatusHost?.querySelector(".text-token-error-foreground, [data-state=error], [data-status=failed]")) {
      return "error";
    }
    return "";
  };

  const clearThreadStatusIndicator = (row) => {
    row.removeAttribute(threadStatusAttribute);
  };

  const installThreadStatusIndicators = (root = document) => {
    queryWithin(root, "[data-app-action-sidebar-thread-row]").forEach((row) => {
      if (!(row instanceof HTMLElement)) return;
      const state = threadStatusFromRow(row);
      if (!state) {
        clearThreadStatusIndicator(row);
        return;
      }
      row.setAttribute(threadStatusAttribute, state);
    });
  };

  const codexAppAssetUrls = () => [...new Set([
    ...Array.from(document.scripts || []).map((script) => script.src),
    ...Array.from(document.querySelectorAll("link[href]") || []).map((link) => link.href),
    ...(
      typeof performance?.getEntriesByType === "function"
        ? performance.getEntriesByType("resource").map((entry) => entry.name)
        : []
    ),
  ].filter((url) => url && url.includes("/assets/") && url.split("?")[0].endsWith(".js")))];

  const signalDispatcherFromModule = (module, namedSignalAsset) => {
    const preferred = namedSignalAsset ? [module?.rn, module?.O] : [module?.O, module?.rn];
    const exports = Object.values(module || {}).filter((value) => typeof value === "function");
    return [...preferred, ...exports].find((candidate, index, candidates) => {
      if (typeof candidate !== "function" || candidates.indexOf(candidate) !== index) return false;
      if (namedSignalAsset && preferred.includes(candidate)) return true;
      let source = "";
      try {
        source = Function.prototype.toString.call(candidate);
      } catch {
        return false;
      }
      return candidate.length >= 2 && /\.sendRequest\([^)]*\)/.test(source);
    }) || null;
  };

  const loadCodexSignalDispatcher = async () => {
    if (typeof window.__codeyCodexSignalDispatcher === "function") {
      return window.__codeyCodexSignalDispatcher;
    }
    const urls = codexAppAssetUrls().sort((left, right) => (
      Number(right.includes("app-server-manager-signals-"))
      - Number(left.includes("app-server-manager-signals-"))
    ));
    for (const url of urls) {
      const namedSignalAsset = url.includes("app-server-manager-signals-");
      if (!namedSignalAsset) {
        let source = "";
        try {
          source = await fetch(url).then((response) => (response.ok ? response.text() : ""));
        } catch {
          continue;
        }
        if (!source.includes("Missing AppServer request message handler")) continue;
      }
      try {
        const module = await import(url);
        const dispatcher = signalDispatcherFromModule(module, namedSignalAsset);
        if (dispatcher) return dispatcher;
      } catch {
        continue;
      }
    }
    throw new Error("Codex 会话刷新接口不可用");
  };

  const refreshRecentLocalSessions = async () => {
    try {
      codexSignalDispatcherPromise ||= loadCodexSignalDispatcher().catch((error) => {
        codexSignalDispatcherPromise = null;
        throw error;
      });
      const dispatcher = await codexSignalDispatcherPromise;
      await dispatcher("refresh-recent-conversations-for-host", {
        hostId: "local",
        sortKey: "updated_at",
      });
      return true;
    } catch {
      return false;
    }
  };

  const reloadConversationAfterHardDelete = async (sessionId, messageIds) => {
    const normalizedSessionId = String(sessionId || "").replace(/^local:/, "").trim();
    if (!normalizedSessionId || !messageIds.length) throw new Error("缺少会话或轮次 ID");
    codexSignalDispatcherPromise ||= loadCodexSignalDispatcher().catch((error) => {
      codexSignalDispatcherPromise = null;
      throw error;
    });
    const dispatcher = await codexSignalDispatcherPromise;

    // This native path unsubscribes app-server memory while preserving the
    // active route and marking the React conversation as needing a resume.
    await dispatcher("unsubscribe-thread-for-host", {
      hostId: "local",
      threadId: normalizedSessionId,
    });

    // Closing a loaded thread may flush a final record. Reapply the hard delete
    // only after unsubscribe has completed so stale memory cannot restore it.
    const cleanup = await callBridge("/session/delete-messages", {
      sessionId: normalizedSessionId,
      messageIds,
    });
    if (cleanup?.status === "failed") {
      throw new Error(cleanup.message || "卸载会话后的持久化清理失败");
    }
    await dispatcher("maybe-resume-conversation", {
      hostId: "local",
      conversationId: normalizedSessionId,
      model: null,
      serviceTier: null,
      reasoningEffort: null,
      workspaceRoots: [],
      collaborationMode: null,
    });
    await dispatcher("refresh-recent-conversations-for-host", {
      hostId: "local",
      sortKey: "updated_at",
    });
  };

  const importSessionFile = async (projectPath, file, button) => {
    button.disabled = true;
    button.dataset.busy = "true";
    try {
      const data = await file.text();
      const result = await callBridge("/session/import", {
        projectPath,
        data,
      });
      if (result?.status === "failed") {
        throw new Error(result.message || "未知错误");
      }
      if (result?.status !== "imported" || !result.sessionId) {
        throw new Error("导入结果不完整");
      }
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

  const isTaskRunning = () => [...document.querySelectorAll("button[aria-label]")].some((button) => {
    const label = String(button.getAttribute("aria-label") || "").trim().toLowerCase();
    const runningLabel = label === "停止" || label.includes("停止生成") || label === "stop" || label.includes("stop generating");
    return runningLabel && button.getClientRects().length > 0 && !button.disabled;
  });

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

  const navigateAwayFromDeletedThread = (deletedThread) => {
    const replacement = [...document.querySelectorAll(
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    )].find((thread) => (
      thread !== deletedThread
      && thread instanceof HTMLElement
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
        return /新任务|新对话|\bnew task\b|\bnew chat\b/i.test(label);
      });
    if (newThreadAction instanceof HTMLElement) {
      newThreadAction.click();
      return true;
    }
    return false;
  };

  const deleteSidebarSession = async (thread, anchor, confirmButton) => {
    const rawSessionId = String(
      thread.getAttribute("data-app-action-sidebar-thread-id") || "",
    ).trim();
    const sessionId = rawSessionId.replace(/^local:/, "");
    const title = String(
      thread.getAttribute("data-app-action-sidebar-thread-title") || "",
    ).trim();
    if (!sessionId) {
      closeSessionDeletePopover();
      showRuntimeToast("无法识别要删除的会话", "error");
      return;
    }
    const isActive = thread.getAttribute("data-app-action-sidebar-thread-active") === "true";
    if (isActive && isTaskRunning()) {
      closeSessionDeletePopover();
      showRuntimeToast("当前会话仍在运行，请停止任务后再删除", "error");
      return;
    }

    confirmButton.disabled = true;
    confirmButton.textContent = "删除中…";
    anchor.setAttribute("aria-busy", "true");
    try {
      const result = await callBridge("/session/delete", { sessionId, title });
      if (result?.status !== "ok" || result?.deleted !== true) {
        throw new Error(result?.message || "未知错误");
      }
      closeSessionDeletePopover();
      if (isActive) {
        const navigated = navigateAwayFromDeletedThread(thread);
        if (!navigated) window.setTimeout(() => location.reload(), 180);
      }
      thread.remove();
      window.dispatchEvent(new CustomEvent("codey-session-deleted", {
        detail: { sessionId, title },
      }));
      showRuntimeToast(`已删除会话${title ? `“${title}”` : ""}`);
    } catch (error) {
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
    const title = String(
      thread.getAttribute("data-app-action-sidebar-thread-title") || "未命名会话",
    ).trim() || "未命名会话";
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
      void deleteSidebarSession(thread, anchor, confirmButton);
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
    queryWithin(root,
      "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
    ).forEach((thread) => {
      if (!(thread instanceof HTMLElement) || thread.querySelector(`[${sessionDeleteAttribute}]`)) return;
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
    const count = selectedRows().length;
    toolbar.hidden = count === 0;
    const label = toolbar.querySelector("[data-codey-count]");
    if (label) label.textContent = `已选 ${count} 轮`;
  };

  const updateSelectionButton = (row) => {
    const selected = row.classList.contains(selectedClass);
    const button = row.querySelector("[data-codey-message-select]");
    if (!button) return;
    button.setAttribute("aria-pressed", selected ? "true" : "false");
    button.textContent = selected ? "✓" : "○";
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
    const rows = [...document.querySelectorAll("[data-codey-message-id]")];
    if (event?.shiftKey && lastSelectedRow && rows.includes(lastSelectedRow)) {
      const start = rows.indexOf(lastSelectedRow);
      const end = rows.indexOf(row);
      rows.slice(Math.min(start, end), Math.max(start, end) + 1).forEach((item) => {
        item.classList.add(selectedClass);
        updateSelectionButton(item);
      });
    } else {
      row.classList.toggle(selectedClass);
      updateSelectionButton(row);
    }
    lastSelectedRow = row;
    syncSelectionGroups();
    updateToolbar();
  };

  const deleteSelected = async () => {
    const rows = selectedRows();
    const messageIds = rows.map((row) => row.dataset.codeyMessageId).filter(Boolean);
    const sessionId = getSessionId();
    if (!sessionId || !messageIds.length) {
      window.alert("无法识别当前会话或尚未选择任何一轮对话");
      return;
    }
    if (isTaskRunning()) {
      window.alert("当前任务仍在运行，请等待任务结束后再删除会话记录");
      return;
    }
    if (!window.confirm(`删除 ${messageIds.length} 轮对话？\n无法撤销。`)) return;
    const result = await callBridge("/session/delete-messages", { sessionId, messageIds });
    if (result?.status === "failed") {
      window.alert(`删除失败：${result.message || "未知错误"}`);
      return;
    }
    const deleted = Number(result?.deleted || 0);
    if (!deleted) {
      window.alert("没有在当前会话记录中找到所选轮次；会话文件未被修改");
      return;
    }
    rememberHardDeletedMessages(sessionId, messageIds);
    rows.forEach((row) => row.remove());
    lastSelectedRow = null;
    syncSelectionGroups();
    updateToolbar();
    try {
      await reloadConversationAfterHardDelete(sessionId, messageIds);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      window.alert(`消息已从会话文件永久删除，但 Codex 内存会话卸载失败。\n请重启 Codex 后再继续对话。\n\n${message}`);
      return;
    }
    window.dispatchEvent(new CustomEvent("codey-session-refresh", { detail: { sessionId, messageIds } }));
    showRuntimeToast(`已永久删除 ${deleted} 轮对话`);
  };

  const mountToolbar = () => {
    if (document.getElementById(toolbarId)) return;
    const toolbar = document.createElement("div");
    toolbar.id = toolbarId;
    toolbar.hidden = true;
    toolbar.innerHTML = '<span data-codey-count>已选 0 轮</span><button type="button" data-codey-delete data-danger>删除</button><button type="button" data-codey-clear>取消</button>';
    toolbar.querySelector("[data-codey-delete]")?.addEventListener("click", () => void deleteSelected());
    toolbar.querySelector("[data-codey-clear]")?.addEventListener("click", () => {
      selectedRows().forEach((row) => {
        row.classList.remove(selectedClass);
        updateSelectionButton(row);
      });
      syncSelectionGroups();
      updateToolbar();
    });
    document.body.appendChild(toolbar);
  };

  const installMessageSelection = (root = document) => {
    mountToolbar();
    const currentTurnRows = queryWithin(root, "[data-turn-key]");
    const rows = currentTurnRows.length
      ? currentTurnRows
      : queryWithin(root, "[data-message-author-role], [data-testid=conversation-turn], [data-message-id]");
    let installed = false;
    const sessionId = getSessionId();
    rows.forEach((row) => {
      if (!(row instanceof HTMLElement)) return;
      const messageId = getMessageId(row);
      if (!messageId) return;
      if (isHardDeletedMessage(sessionId, messageId)) {
        row.remove();
        installed = true;
        return;
      }
      if (row.querySelector("[data-codey-message-select]")) return;
      row.dataset.codeyMessageId = messageId;
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
      if (getComputedStyle(row).position === "static") row.style.position = "relative";
      row.appendChild(button);
      installed = true;
    });
    if (installed) {
      syncSelectionGroups();
      updateToolbar();
    }
  };

  const scan = (root = document, syncTitles = true, mountSettings = true) => {
    window.__codeyBlockNativePetControls?.(root);
    window.__codeyBlockNativeVoiceControls?.(root);
    if (mountSettings) mountButton();
    installSessionExportButtons(root);
    installTasksImportButton(root);
    installSessionDeleteButtons(root);
    installProjectImportButtons(root);
    installThreadStatusIndicators(root);
    installMessageSelection(root);
    if (syncTitles) syncSidebarTitles(root);
  };

  window.__codeyBridge = callBridge;
  window.__codeyGetSessionId = getSessionId;
  window.__codeyGetSessionTitle = getSessionTitle;
  window.__codeySyncSidebarTitles = syncSidebarTitles;
  window.__codeyGetMessageId = getMessageId;
  window.__codeyProjectPathFromRow = projectPathFromRow;
  window.__codeyThreadSessionIdFromRow = threadSessionIdFromRow;
  window.__codeyThreadStatusFromRow = threadStatusFromRow;
  window.__codeyInstallThreadStatusIndicators = installThreadStatusIndicators;
  window.__codeyRefreshRecentLocalSessions = refreshRecentLocalSessions;
  window.__codeyExportSession = exportSession;
  window.__codeyImportSessionFile = importSessionFile;
  window.__codeyInstallSessionDeleteButtons = installSessionDeleteButtons;
  window.__codeyOpenSessionDeletePopover = openSessionDeletePopover;
  window.__codeySyncSelectionGroups = syncSelectionGroups;
  window.__codeyDeleteSelectedMessages = deleteSelected;
  window.__codeyReloadConversationAfterHardDelete = reloadConversationAfterHardDelete;
  window.__codeyInstallMessageSelection = installMessageSelection;
  scan();

  const codeyOwnedSelector = [
    `#${buttonId}`,
    `#${toolbarId}`,
    `#${toastId}`,
    `#${sessionDeletePopoverId}`,
    `#${sidebarActionTooltipId}`,
    `[${sessionExportAttribute}]`,
    `[${tasksImportAttribute}]`,
    `[${projectImportAttribute}]`,
    `[${sessionDeleteAttribute}]`,
    "[data-codey-message-select]",
  ].join(", ");
  const scanBoundarySelector = [
    "header",
    "nav",
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-turn-key]",
    "[data-message-author-role]",
    "[data-testid=conversation-turn]",
    "[data-message-id]",
  ].join(", ");
  const relevantAddedSelector = [
    scanBoundarySelector,
    "button",
    "[role=button]",
    "[role=menuitem]",
    "[role=option]",
    "[role=switch]",
    "input",
    "label",
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
    return element.closest?.(scanBoundarySelector) || element;
  };
  const flushIncrementalScans = () => {
    scanTimer = 0;
    const roots = [...pendingScanRoots];
    pendingScanRoots.clear();
    mountButton();
    roots.forEach((root) => scan(root, true, false));
  };
  const scheduleIncrementalScan = (root) => {
    if (root) pendingScanRoots.add(root);
    window.clearTimeout(scanTimer);
    scanTimer = window.setTimeout(flushIncrementalScans, 60);
  };

  new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (target && !isCodeyOwned(target)) {
          pendingScanRoots.add(nearestScanRoot(target));
        }
        continue;
      }
      for (const node of mutation.addedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) {
          if (node?.nodeType === Node.TEXT_NODE && target && !isCodeyOwned(target)) {
            pendingScanRoots.add(nearestScanRoot(target));
          }
          continue;
        }
        if (isCodeyOwned(element) || !containsRelevantElement(element)) continue;
        pendingScanRoots.add(nearestScanRoot(element));
      }
      for (const node of mutation.removedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element || !containsRelevantElement(element)) continue;
        if (target && !isCodeyOwned(target)) pendingScanRoots.add(nearestScanRoot(target));
      }
    }
    if (pendingScanRoots.size) {
      scheduleIncrementalScan(null);
    }
  }).observe(document.documentElement, {
    attributes: true,
    attributeFilter: [
      "aria-label",
      "data-turn-key",
      "data-request-user-input-auto-resolution-conversation-id",
      "data-app-action-sidebar-thread-id",
      "data-app-action-sidebar-thread-title",
      "data-app-action-sidebar-project-id",
      "data-app-action-sidebar-project-row",
      "data-testid",
      "disabled",
    ],
    childList: true,
    subtree: true,
  });
  if (typeof document.addEventListener === "function") {
    document.addEventListener("visibilitychange", wakeSessionWatcher);
    document.addEventListener("pointerdown", wakeSessionWatcher, { capture: true, passive: true });
    document.addEventListener("keydown", wakeSessionWatcherFromKey, true);
  }
  if (typeof window.addEventListener === "function") {
    window.addEventListener("focus", wakeSessionWatcher);
    window.addEventListener("pageshow", wakeSessionWatcher);
  }
})();
