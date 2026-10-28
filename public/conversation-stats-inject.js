// 状态条：会话总轮数、最近一轮的活动统计与会话累计 Token 用量，未知数据保留空值。
// /session/stats 返回统计范围；会话 id 取自可见 composer，隐藏会话不参与。
(() => {
  const PROBE_KEY = "__codeyConversationStats";
  const VERSION = 8;
  const previous = window[PROBE_KEY];
  if (previous?.installed && (previous.version === VERSION || !previous.dispose)) return;
  previous?.dispose?.();

  const barId = "codey-conversation-stats";
  const styleId = "codey-conversation-stats-styles";
  const overlayId = `${barId}-overlay`;
  const styleScope = `:is(#${barId}, #${overlayId})`;
  const conversationAttribute = "data-above-composer-conversation-id";
  // 与 codey-inject.js 的会话识别保持同一优先级：先看可见 composer，再看
  // 会话元素属性、活动侧边栏项，最后退回 URL。
  const sessionIdAttributes = [
    conversationAttribute,
    "data-session-id",
    "data-conversation-id",
    "data-thread-id",
    "data-request-user-input-auto-resolution-conversation-id",
    "data-response-annotation-conversation",
  ];
  const sessionAttributeSelector = sessionIdAttributes
    .map((attribute) => `[${attribute}]`)
    .join(",");
  const activeThreadSelector = '[data-app-action-sidebar-thread-active="true"]';
  const contextAttributes = [
    ...sessionIdAttributes,
    "data-app-action-sidebar-thread-active",
    "data-app-action-sidebar-thread-id",
    "hidden",
    "aria-hidden",
  ];
  const POLL_IDLE_MS = 3_000;
  const POLL_ACTIVE_MS = 1_100;
  const RESCAN_DEBOUNCE_MS = 120;

  let statsElement = null;
  let currentSessionId = "";
  let stats = null;
  let pollTimer = 0;
  let rescanTimer = 0;
  let fetchInFlight = false;
  let lastFetchOk = false;
  let lastResult = null;
  let sessionGeneration = 0;
  let fetchAttempted = false;
  let disposed = false;
  const listeners = [];
  const listen = (target, type, handler, options) => {
    target.addEventListener?.(type, handler, options);
    listeners.push(() => target.removeEventListener?.(type, handler, options));
  };

  const isRendered = (element) => (
    element instanceof HTMLElement
    && element.getClientRects().length > 0
  );

  const visibleSessionElement = (selector) => {
    const first = document.querySelector(selector);
    if (isRendered(first)) return first;
    return [...document.querySelectorAll(selector)].find(isRendered);
  };

  const composerInputSelector =
    "textarea, [contenteditable='true'], [role='textbox']";

  // 锚点是只带数据属性的标记元素，自身通常没有布局矩形，不能用
  // getClientRects 判断。当前对话的判定标准是锚点所在 composer 容器内
  // 存在视口内可见的输入框；Codex 保留的非活动会话不满足这一点。
  const anchorHostsVisibleComposer = (anchor) => {
    const host = anchor?.parentElement || anchor;
    if (!host) return false;
    const viewportHeight =
      window.innerHeight || document.documentElement.clientHeight || 0;
    for (const input of host.querySelectorAll?.(composerInputSelector) || []) {
      if (!(input instanceof HTMLElement)) continue;
      if (input.closest?.("[hidden], [aria-hidden='true']")) continue;
      const rect = input.getBoundingClientRect?.();
      if (!rect || rect.width <= 0 || rect.height <= 0) continue;
      if (viewportHeight > 0 && (rect.bottom <= 0 || rect.top >= viewportHeight)) {
        continue;
      }
      return true;
    }
    return false;
  };

  const findVisibleComposerAnchor = () => {
    for (const anchor of document.querySelectorAll(`[${conversationAttribute}]`)) {
      if (anchorHostsVisibleComposer(anchor)) return anchor;
    }
    return null;
  };

  const callBridge = (path, payload = {}, options = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload, options);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };

  const normalizeSessionId = (value) => (
    String(value || "").trim().replace(/^local:/, "").trim()
  );

  const resolveSessionId = (anchor = findVisibleComposerAnchor()) => {
    // 可见输入框的空标识表示会话尚未建立，不从旧 URL 或侧边栏取回旧会话。
    if (anchor) return normalizeSessionId(anchor.getAttribute(conversationAttribute));
    for (const attribute of sessionIdAttributes.slice(1)) {
      const value = visibleSessionElement(`[${attribute}]`)?.getAttribute(attribute);
      const normalized = normalizeSessionId(value);
      if (normalized) return normalized;
    }
    const activeThread = visibleSessionElement(activeThreadSelector)
      ?.getAttribute("data-app-action-sidebar-thread-id");
    if (activeThread) return normalizeSessionId(activeThread);
    const match = location.pathname.match(/(?:\/c\/|\/conversation\/|\/session\/)([A-Za-z0-9_-]+)/);
    if (match) return match[1];
    const params = new URLSearchParams(location.search);
    return normalizeSessionId(params.get("conversation_id") || params.get("session_id"));
  };

  // 只在同一同步阶段复用查询结果；跨过桥接请求后重新确认当前 composer。
  const resolveSessionContext = () => {
    const anchor = findVisibleComposerAnchor();
    return { anchor, sessionId: resolveSessionId(anchor) };
  };

  const ensureStyle = () => {
    if (document.getElementById(styleId)) return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      ${styleScope} { --cs-text: var(--text-primary, CanvasText); --cs-muted: color-mix(in srgb, var(--cs-text) 60%, transparent); --cs-line: color-mix(in srgb, var(--cs-text) 11%, transparent); --cs-surface: color-mix(in srgb, var(--bg-primary, Canvas) 97%, var(--cs-text)); -webkit-app-region: no-drag !important; pointer-events: auto !important; position: relative; box-sizing: border-box; width: 100%; min-width: 0; padding: 5px 4px 0; color: var(--cs-text); font: 12px/1.5 -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif; font-variant-numeric: tabular-nums; }
      ${styleScope} *, ${styleScope} *::before { box-sizing: border-box; }
      ${styleScope}[hidden], ${styleScope} [hidden] { display: none !important; }
      ${styleScope} .codey-cs-groups { display: flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 2px 10px; }
      ${styleScope} button { font: inherit; color: inherit; cursor: pointer; }
      ${styleScope} .codey-cs-summary { display: inline-flex; align-items: center; justify-content: center; flex-wrap: wrap; gap: 6px; min-width: 0; max-width: 100%; padding: 4px 9px; border: 0; border-radius: 999px; background: transparent; color: var(--cs-muted); text-align: center; }
      ${styleScope} .codey-cs-summary:hover, ${styleScope} .codey-cs-summary[aria-expanded="true"] { background: color-mix(in srgb, var(--cs-text) 5%, transparent); color: color-mix(in srgb, var(--cs-text) 72%, transparent); }
      ${styleScope} button:focus-visible { outline: 2px solid var(--cs-text); outline-offset: 2px; }
      ${styleScope} .codey-cs-icon { display: inline-block; width: 15px; height: 15px; flex: 0 0 15px; background: currentColor; mask: var(--cs-icon) center / contain no-repeat; }
      ${styleScope} .codey-cs-metric { white-space: nowrap; }
      ${styleScope} .codey-cs-details { -webkit-app-region: no-drag !important; pointer-events: auto !important; position: fixed; inset: auto; margin: 0; z-index: 1000; width: min(360px, calc(100vw - 24px)); max-height: calc(100dvh - 24px); overflow: auto; padding: 17px 18px 15px; color: var(--cs-text); background: var(--cs-surface); border: 1px solid var(--cs-line); border-radius: 15px; box-shadow: 0 5px 20px #0000000d; user-select: text; }
      ${styleScope} .codey-cs-heading { display: flex; align-items: center; gap: 8px; padding-bottom: 11px; margin-bottom: 10px; border-bottom: 1px solid var(--cs-line); }
      ${styleScope} .codey-cs-heading strong { font-size: 13px; font-weight: 550; white-space: nowrap; }
      ${styleScope} .codey-cs-heading-total { margin-left: auto; font-size: 13px; text-align: right; }
      ${styleScope} dl { margin: 0; }
      ${styleScope} .codey-cs-detail-row { display: flex; justify-content: space-between; gap: 14px; padding: 4px 0; }
      ${styleScope} dt { color: var(--cs-muted); }
      ${styleScope} dd { margin: 0; text-align: right; color: color-mix(in srgb, var(--cs-text) 72%, transparent); overflow-wrap: anywhere; }
      ${styleScope} .codey-cs-note { margin: 10px 0 0; color: var(--cs-muted); font-size: 10px; line-height: 1.6; }
      @media (max-width: 480px) { ${styleScope} .codey-cs-groups { gap: 2px; } ${styleScope} .codey-cs-summary { padding-inline: 6px; font-size: 11px; } ${styleScope} .codey-cs-details { padding: 14px; } }
    `;
    document.documentElement.appendChild(style);
  };

  // 状态条挂在 composer 标记所在的同一列容器末尾，即输入卡片下方；
  // composer 容器会随会话切换重建，所以每次都要重新核对挂载父节点。
  const ensureBar = (anchor = findVisibleComposerAnchor()) => {
    const host = anchor?.parentElement;
    if (!host) return null;
    let bar = document.getElementById(barId) || statsElement;
    if (!bar?.__codeyCs) {
      bar = document.createElement("div");
      bar.id = barId;
      bar.className = "codey-conversation-stats";
      const make = (tag, className, text, parent) => {
        const element = document.createElement(tag);
        element.className = className;
        if (text) element.textContent = text;
        parent?.appendChild(element);
        return element;
      };
      const groups = make("div", "codey-cs-groups", "", bar);
      // 浮层独立于 composer，避免 transform 改变 fixed 坐标系或 overflow 裁切。
      const overlay = make("div", "", "", document.body || document.documentElement);
      overlay.id = overlayId;
      overlay.style.display = "contents";
      const refs = { overlay, buttons: {}, panels: {}, fields: {}, notes: {}, active: null, pinned: false, closeTimer: 0 };
      // Tabler Icons v3.45.0 (MIT), matching the existing application icon family.
      const paths = {
        activity: ["M3 12a9 9 0 1 0 18 0a9 9 0 1 0 -18 0", "M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0", "M13.41 10.59l2.59 -2.59", "M7 12a5 5 0 0 1 5 -5"],
        tokens: ["M4 6a8 3 0 1 0 16 0a8 3 0 1 0 -16 0", "M4 6v6a8 3 0 0 0 16 0v-6", "M4 12v6a8 3 0 0 0 16 0v-6"],
      };
      const icon = (kind, parent) => {
        const element = make("span", "codey-cs-icon", "", parent);
        element.setAttribute("aria-hidden", "true");
        const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">${paths[kind].map(d => `<path d="${d}"/>`).join("")}</svg>`;
        element.style.setProperty?.("--cs-icon", `url("data:image/svg+xml,${encodeURIComponent(svg)}")`);
      };
      for (const [kind, title, fields] of [
        ["activity", "最近一轮统计", [["modelDurationMs", "模型用时"], ["toolDurationMs", "工具调用用时"], ["averageTtftMs", "首 token 平均（TTFT）"], ["speed", "平均速度（TPS）"]]],
        ["tokens", "Token 用量", [["cacheRate", "缓存命中"], ["uncachedInputTokens", "未缓存输入"], ["cachedInputTokens", "缓存读取"], ["outputTokens", "输出"]]],
      ]) {
        const button = make("button", `codey-cs-summary codey-cs-${kind}-summary`, "", groups);
        button.type = "button";
        button.setAttribute("aria-expanded", "false");
        button.setAttribute("aria-controls", `${barId}-${kind}`);
        icon(kind, button);
        if (kind === "activity") {
          refs.activity = make("span", "codey-cs-metric codey-cs-activity", "— 轮 · — 步", button);
          refs.activity.title = "轮数为会话总轮数，步骤和速度为最近一轮统计。";
          make("span", "", "·", button).setAttribute("aria-hidden", "true");
          refs.speed = make("span", "codey-cs-metric codey-cs-speed", "—", button);
        } else {
          refs.total = make("span", "codey-cs-metric codey-cs-total", "—", button);
          make("span", "", "·", button).setAttribute("aria-hidden", "true");
          const cache = make("span", "codey-cs-metric", "缓存命中 ", button);
          refs["cache-rate"] = make("span", "codey-cs-cache-rate", "—", cache);
        }
        const panel = make("section", "codey-cs-details", "", overlay);
        panel.id = `${barId}-${kind}`;
        panel.hidden = true;
        if (typeof panel.showPopover === "function") panel.setAttribute("popover", "manual");
        panel.setAttribute("aria-label", title);
        const heading = make("div", "codey-cs-heading", "", panel);
        icon(kind, heading);
        make("strong", "", title, heading);
        if (kind === "tokens") {
          refs.fields.totalTokens = make("span", "codey-cs-heading-total", "—", heading);
          refs.fields.totalTokens.setAttribute("data-field", "totalTokens");
        }
        const list = make("dl", "", "", panel);
        for (const [key, label] of fields) {
          const row = make("div", "codey-cs-detail-row", "", list);
          make("dt", "", label, row);
          refs.fields[key] = make("dd", "", "—", row);
          refs.fields[key].setAttribute("data-field", key);
        }
        refs.notes[kind] = make("p", `codey-cs-note codey-cs-${kind}-note`, "", panel);
        refs.buttons[kind] = button;
        refs.panels[kind] = panel;
        const preview = () => { clearTimeout(refs.closeTimer); if (!refs.pinned && !refs.restoringFocus) setExpanded(true, false, kind); };
        const leave = () => { clearTimeout(refs.closeTimer); refs.closeTimer = setTimeout(() => { if (!refs.pinned) setExpanded(false); }, 350); };
        button.addEventListener("pointerenter", preview);
        button.addEventListener("focus", preview);
        button.addEventListener("pointerleave", leave);
        button.addEventListener("blur", (event) => { if (!panel.contains(event.relatedTarget)) leave(); });
        panel.addEventListener("pointerenter", () => clearTimeout(refs.closeTimer));
        panel.addEventListener("pointerleave", leave);
        button.addEventListener("click", () => {
          const close = refs.active === kind && refs.pinned;
          setExpanded(!close, false, kind);
          refs.pinned = !close;
        });
      }
      bar.__codeyCs = refs;
    }
    if (bar.parentElement !== host) {
      if (statsElement) setExpanded(false);
      host.appendChild(bar);
    }
    const overlay = bar.__codeyCs.overlay;
    if (!overlay.isConnected) {
      setExpanded(false);
      (document.body || document.documentElement).appendChild(overlay);
    }
    statsElement = bar;
    return bar;
  };

  const containsStats = (target) => Boolean(target && (
    statsElement?.contains(target) || statsElement?.__codeyCs?.overlay.contains(target)
  ));

  const syncPanelTheme = () => {
    const overlay = statsElement?.__codeyCs?.overlay;
    const computed = statsElement && window.getComputedStyle?.(statsElement);
    if (!overlay || !computed) return;
    for (const name of ["--text-primary", "--bg-primary", "--cs-text", "--cs-muted", "--cs-line", "--cs-surface"]) {
      const value = computed.getPropertyValue(name).trim();
      if (overlay.style.getPropertyValue(name) === value) continue;
      if (value) overlay.style.setProperty(name, value);
      else overlay.style.removeProperty(name);
    }
    setValue(overlay.style, "colorScheme", computed.colorScheme);
  };

  const formatTokenCount = (value) => {
    if (value == null || !Number.isFinite(value)) return "—";
    if (value < 1_000) return `${Math.round(value)}`;
    if (value < 999_500) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)}k`;
    return `${(value / 1_000_000).toFixed(1)}M`;
  };

  const positionPanel = (anchor, onlyIfMoved = false) => {
    const refs = statsElement?.__codeyCs;
    if (!refs?.active) return;
    if (!statsElement.isConnected || !(anchor === undefined ? findVisibleComposerAnchor() : anchor)) { setExpanded(false); return; }
    syncPanelTheme();
    const panel = refs.panels[refs.active];
    const rect = refs.buttons[refs.active].getBoundingClientRect();
    const height = window.innerHeight || document.documentElement.clientHeight;
    const width = window.innerWidth || document.documentElement.clientWidth;
    const placement = [refs.active, rect.left, rect.top, rect.width, rect.height, width, height].join(":");
    if (onlyIfMoved && refs.placement === placement) return;
    refs.placement = placement;
    const panelRect = panel.getBoundingClientRect();
    // 气泡贴近入口，鼠标可以直接移入；只覆盖编辑器，不推动页面布局。
    const available = Math.max(80, rect.top - 20);
    panel.style.left = `${Math.max(12, Math.min(rect.left + (rect.width - panelRect.width) / 2, width - panelRect.width - 12))}px`;
    panel.style.maxHeight = `${Math.min(height - 24, available)}px`;
    panel.style.top = `${Math.max(12, rect.top - Math.min(panelRect.height, available) - 8)}px`;
  };

  const setExpanded = (expanded, restoreFocus = false, kind = null) => {
    const refs = statsElement?.__codeyCs;
    if (!refs) return;
    const active = kind || refs.active || "activity";
    clearTimeout(refs.closeTimer);
    refs.active = expanded ? active : null;
    if (!expanded) refs.pinned = false;
    for (const key of Object.keys(refs.panels)) {
      const panel = refs.panels[key];
      const visible = expanded && key === active;
      if (!visible && panel.hasAttribute("popover") && panel.matches(":popover-open")) panel.hidePopover();
      setValue(panel, "hidden", !visible);
      if (visible && panel.hasAttribute("popover") && !panel.matches(":popover-open")) {
        try { panel.showPopover(); } catch { panel.removeAttribute("popover"); }
      }
      setAttribute(refs.buttons[key], "aria-expanded", String(visible));
    }
    if (expanded) positionPanel();
    else if (restoreFocus) {
      refs.restoringFocus = true;
      refs.buttons[active].focus?.({ preventScroll: true });
      refs.restoringFocus = false;
    }
  };

  const resetSession = (sessionId) => {
    currentSessionId = sessionId;
    sessionGeneration += 1;
    stats = null;
    lastResult = null;
    lastFetchOk = false;
    fetchAttempted = false;
    setExpanded(false);
  };

  const formatSpeed = (value) => (
    value == null || !Number.isFinite(value) || value < 0
      ? null
      : `${value >= 100 ? Math.round(value) : value.toFixed(1)} tok/s`
  );

  // 与请求日志同一口径：缓存读占输入总量的百分比；上游未回报缓存字段时
  // 返回 null，状态条显示未知值而不是显示 0%。
  const cacheRatePercent = (input, cached) => {
    if (input == null || cached == null || input <= 0 || cached < 0 || cached > input) return null;
    return `${Math.round((cached / input) * 100)}%`;
  };

  const setText = (element, value) => {
    // 相同 textContent 仍会替换文本节点，触发其他页面观察器。
    if (element.textContent === value) return false;
    element.textContent = value;
    return true;
  };

  const setValue = (target, key, value) => {
    if (target[key] !== value) target[key] = value;
  };

  const setAttribute = (element, name, value) => {
    if (element.getAttribute(name) !== value) element.setAttribute(name, value);
  };

  const render = (anchor) => {
    if (disposed) return;
    const bar = ensureBar(anchor);
    if (!bar) {
      // 输入框不可见（例如离开会话页）时，若旧宿主仍在隐藏会话里，显式
      // 隐藏状态条，避免恢复可见时短暂展示过期数据。
      if (statsElement) { setValue(statsElement, "hidden", true); setExpanded(false); }
      return;
    }
    setValue(bar, "hidden", false);
    const data = { ...stats };
    const sessionUsage = data.usageSource === "rollout" && data.usageScope === "session";
    const runtimeRequests = data.requestsScope === "runtime"
      || (data.usageSource == null && data.usageScope == null);
    const hasRuntimeRequest = runtimeRequests && (data.requests > 0 || data.inFlight > 0);
    const turn = typeof data.lastTurn?.id === "string" && data.lastTurn.id.trim()
      ? data.lastTurn : null;
    const running = turn?.status === "running";
    const stale = fetchAttempted && !lastFetchOk;
    if (!hasRuntimeRequest) {
      for (const key of ["requests", "inFlight", "model", "lastStatus", "updatedAtUnixMs"]) data[key] = null;
    }
    // 最近一轮由后端按用户 turn 归属聚合；不回退到会话累计或单次请求字段。
    for (const key of ["totalTokens", "inputTokens", "outputTokens", "cachedInputTokens", "cacheWriteInputTokens", "requests", "inFlight"]) {
      if (!Number.isSafeInteger(data[key]) || data[key] < 0) data[key] = null;
    }
    if (!Number.isSafeInteger(data.turnCount) || data.turnCount < 0) data.turnCount = null;
    const formattedSpeed = formatSpeed(running && stale ? null : turn?.averageOutputTokensPerSecond);
    const speedKnown = formattedSpeed != null;
    const estimatedSpeed = turn?.speedIsEstimate === true;
    const speedText = speedKnown ? `${estimatedSpeed ? "≈" : ""}${formattedSpeed}` : running ? stale ? "更新暂停" : turn.speedState === "waiting" ? "等待输出" : "生成中" : "—";
    const coverageNote = ({
      runtime: "仅覆盖本轮在本次运行中可统计的步骤",
      recorded: "仅覆盖本轮可恢复且有有效计时的记录",
      recorded_and_runtime: "合并本轮可恢复记录与本次运行统计，缺失记录不计入",
      partial: "本轮记录不完整，仅统计可恢复的部分",
    })[turn?.coverage] || "本轮统计范围未知，缺失值显示 —";
    const speedNote = running && stale ? "实时统计更新失败，恢复后继续显示本轮平均速度"
      : `本轮有效步骤的输出 Token 总和 ÷ 生成耗时总和，首字节等待和工具间隔不计；${coverageNote}${estimatedSpeed && speedKnown ? "；含生成中的 Token 估算" : ""}`;
    const rate = cacheRatePercent(data.inputTokens, data.cachedInputTokens) || "—";
    const refs = bar.__codeyCs;
    let panelContentChanged = setText(refs.speed, speedText);
    setValue(refs.speed, "title", speedNote);
    panelContentChanged = setText(refs["cache-rate"], rate) || panelContentChanged;
    panelContentChanged = setText(refs.total, data.totalTokens == null ? "—" : `${formatTokenCount(data.totalTokens)} tok`) || panelContentChanged;
    const finite = value => Number.isFinite(value) && value >= 0;
    const count = value => Number.isSafeInteger(value) && value >= 0 ? value.toLocaleString("zh-CN") : "—";
    const duration = (value, average = false) => {
      if (!finite(value)) return "—";
      const seconds = value / 1000;
      if (average || seconds < 60) return `${Number(seconds.toFixed(1))}秒`;
      return `${Math.floor(seconds / 60)}分 ${Math.floor(seconds % 60)}秒`;
    };
    panelContentChanged = setText(refs.activity, `${count(data.turnCount)} 轮 · ${count(turn?.stepCount)} 步`) || panelContentChanged;
    const hasData = stats && (turn || hasRuntimeRequest || [data.turnCount, data.totalTokens, data.inputTokens, data.outputTokens].some(finite));
    const unknownUsage = data.usageSource === "unknown" || data.usageScope === "unknown";
    const scopeLabel = !stats || unknownUsage ? "统计范围未知" : sessionUsage ? "会话累计" : "本次运行";
    const state = !currentSessionId ? "waiting" : stale ? "stale" : !fetchAttempted ? "loading" : !hasData ? "empty" : "ok";
    const status = {
      waiting: "等待会话", loading: "读取统计中", stale: "统计暂不可用", empty: "暂无统计",
      ok: fetchInFlight ? "更新统计中" : running ? "本轮进行中" : turn?.status === "aborted" ? "本轮已中止" : (sessionUsage ? "历史统计" : "运行期统计"),
    }[state];
    setValue(bar.dataset, "status", status);
    const tokenValue = value => finite(value) ? `${count(value)} tok` : "—";
    setValue(refs.total, "title", data.totalTokens == null ? `${scopeLabel}，缺失数据以 — 显示。` : `${scopeLabel} ${tokenValue(data.totalTokens)}；摘要为四舍五入值，详情显示完整数量。`);
    data.uncachedInputTokens = finite(data.inputTokens) && finite(data.cachedInputTokens) && data.cachedInputTokens <= data.inputTokens ? data.inputTokens - data.cachedInputTokens : null;
    for (const [key, element] of Object.entries(refs.fields)) {
      panelContentChanged = setText(element, key === "cacheRate" ? rate : key === "speed" ? speedText : key.endsWith("Ms") ? duration(turn?.[key], key === "averageTtftMs") : tokenValue(data[key])) || panelContentChanged;
    }
    const pendingNote = stale ? (stats ? "更新失败，当前数据可能已过期。" : "暂时无法读取统计，将自动重试。") : !currentSessionId ? "建立会话后显示统计。" : !stats ? "缺失数据以 — 显示。" : "";
    const sourceNote = unknownUsage ? "Token 统计范围未知。" : sessionUsage ? "会话累计用量，缓存已包含在输入中。" : "仅统计本次运行的用量。";
    const activityNote = turn ? "一次提问为一轮，步数为该轮模型生成次数。" : "暂无可识别的最近一轮。";
    const toolNote = finite(turn?.toolDurationMs) ? "工具用时为本轮累计值，并行调用可能重叠。" : "";
    panelContentChanged = setText(refs.notes.activity, `${pendingNote}${activityNote}${toolNote}${speedNote}。`) || panelContentChanged;
    panelContentChanged = setText(refs.notes.tokens, `${pendingNote}${sourceNote}`) || panelContentChanged;
    setAttribute(refs.buttons.activity, "aria-label", `会话总轮数 ${count(data.turnCount)} 轮，最近一轮统计 ${count(turn?.stepCount)} 步，本轮平均速度 ${speedText}，${status}。${speedNote}。查看统计详情`);
    setAttribute(refs.buttons.tokens, "aria-label", `Token 用量，${scopeLabel} ${refs.total.textContent}，缓存命中 ${rate}，${status}。查看统计详情`);
    setValue(bar.dataset, "running", running ? "true" : "false");
    setValue(bar.dataset, "state", state);
    // 入口仍可能因侧栏或输入框布局变化而移动；位置和数据都不变时才跳过浮层测量。
    positionPanel(anchor, !panelContentChanged);
  };

  const schedulePoll = (delay) => {
    clearTimeout(pollTimer);
    pollTimer = 0;
    if (disposed || document.visibilityState === "hidden") return;
    pollTimer = setTimeout(poll, delay);
  };

  const pollInterval = () => stats?.lastTurn?.status === "running" || stats?.inFlight > 0
    ? POLL_ACTIVE_MS : POLL_IDLE_MS;

  const poll = async () => {
    if (disposed) return;
    if (document.visibilityState === "hidden") return;
    const { anchor, sessionId } = resolveSessionContext();
    if (sessionId !== currentSessionId) {
      resetSession(sessionId);
      render(anchor);
    }
    if (!currentSessionId) {
      schedulePoll(POLL_IDLE_MS);
      return;
    }
    if (!fetchInFlight) {
      fetchInFlight = true;
      render(anchor);
      const requestSessionId = currentSessionId;
      const requestGeneration = sessionGeneration;
      let completedContext = null;
      try {
        const result = await callBridge("/session/stats", { sessionId: requestSessionId });
        if (disposed) return;
        completedContext = resolveSessionContext();
        if (requestGeneration !== sessionGeneration || completedContext.sessionId !== requestSessionId) return;
        fetchAttempted = true;
        lastResult = result ?? null;
        if (result?.status === "ok") {
          lastFetchOk = true;
          stats = result.stats ?? null;
        } else {
          lastFetchOk = false;
        }
      } catch {
        if (!disposed) completedContext = resolveSessionContext();
        if (!disposed && requestGeneration === sessionGeneration && completedContext.sessionId === requestSessionId) {
          fetchAttempted = true;
          lastFetchOk = false;
        }
      } finally {
        fetchInFlight = false;
        if (!disposed) {
          completedContext ||= resolveSessionContext();
          schedulePoll(requestGeneration !== sessionGeneration || completedContext.sessionId !== requestSessionId ? 0 : pollInterval());
        }
      }
      render(completedContext?.anchor);
      return;
    }
    schedulePoll(pollInterval());
  };

  const scheduleRescan = () => {
    if (disposed || rescanTimer) return;
    rescanTimer = setTimeout(() => {
      rescanTimer = 0;
      const { anchor, sessionId } = resolveSessionContext();
      const changed = sessionId !== currentSessionId;
      if (changed) {
        resetSession(sessionId);
      }
      render(anchor);
      // 会话切换立即拉取，不等下一轮轮询。
      if (changed && currentSessionId) schedulePoll(0);
    }, RESCAN_DEBOUNCE_MS);
  };

  const mutationHandler = (mutations) => {
    for (const mutation of mutations || []) {
      if (mutation.type === "attributes") {
        const target = mutation.target;
        if (!(target instanceof HTMLElement)) continue;
        if (containsStats(target)) continue;
        if (contextAttributes.includes(mutation.attributeName)) {
          scheduleRescan();
          return;
        }
        continue;
      }
      if (containsStats(mutation.target)) continue;
      const nodes = [...(mutation.addedNodes || []), ...(mutation.removedNodes || [])];
      for (const node of nodes) {
        if (!(node instanceof HTMLElement)) continue;
        if (node.id === overlayId) { scheduleRescan(); continue; }
        if (node.closest?.(`#${overlayId}`)) continue;
        if (node.id === barId || node.closest?.(`#${barId}`) || node.querySelector?.(`#${barId}`)) {
          // 状态条自身被移除时也要尽快重挂。
          if (node.id === barId || node.querySelector?.(`#${barId}`)) scheduleRescan();
          continue;
        }
        if (
          node.matches?.(sessionAttributeSelector)
          || node.querySelector?.(sessionAttributeSelector)
          || node.matches?.(activeThreadSelector)
          || node.querySelector?.(activeThreadSelector)
        ) {
          scheduleRescan();
          return;
        }
        // 会话容器子树变化一律重扫，代价只是一次 debounce 查询。
        if (node.matches?.(composerInputSelector) || node.querySelector?.(composerInputSelector)) {
          scheduleRescan();
          return;
        }
      }
    }
  };

  const bootstrap = () => {
    ensureStyle();
    render();
    const options = {
      attributes: true,
      attributeFilter: contextAttributes,
      childList: true,
      subtree: true,
    };
    const dispatcher = window.__codeyMutationDispatcher;
    if (typeof dispatcher?.subscribe === "function") {
      const unsubscribe = dispatcher.subscribe(mutationHandler, options);
      if (dispatcher.snapshot?.().observerInstalled) {
        window.__codeyConversationStatsObserver = { disconnect: unsubscribe };
      } else {
        unsubscribe?.();
      }
    }
    if (!window.__codeyConversationStatsObserver) {
      const observer = new MutationObserver(mutationHandler);
      observer.observe(document.documentElement, options);
      window.__codeyConversationStatsObserver = observer;
    }
    listen(document, "visibilitychange", () => schedulePoll(0));
    listen(window, "focus", () => schedulePoll(0));
    listen(window, "pageshow", () => schedulePoll(0));
    listen(window, "resize", () => positionPanel());
    listen(document, "scroll", () => positionPanel(), true);
    listen(document, "pointerdown", (event) => {
      if (statsElement && !containsStats(event.target)) setExpanded(false);
    });
    listen(document, "keydown", (event) => {
      if (event.key !== "Escape" || !statsElement?.__codeyCs?.active) return;
      const focusedInside = containsStats(document.activeElement);
      setExpanded(false, focusedInside);
      if (focusedInside) { event.preventDefault(); event.stopPropagation(); }
    });
    schedulePoll(0);
    window[PROBE_KEY] = {
      installed: true,
      version: VERSION,
      snapshot: () => ({
        sessionId: currentSessionId,
        anchorFound: Boolean(findVisibleComposerAnchor()),
        mounted: statsElement?.isConnected === true,
        hidden: statsElement?.hidden !== false,
        stats,
        lastFetchOk,
        lastResult,
      }),
      refresh: () => schedulePoll(0),
      dispose: () => {
        if (disposed) return;
        disposed = true;
        sessionGeneration += 1;
        clearTimeout(pollTimer);
        clearTimeout(rescanTimer);
        setExpanded(false);
        window.__codeyConversationStatsObserver?.disconnect?.();
        delete window.__codeyConversationStatsObserver;
        listeners.splice(0).forEach(remove => remove());
        statsElement?.__codeyCs?.overlay.remove();
        statsElement?.remove();
        document.getElementById(styleId)?.remove();
        window[PROBE_KEY].installed = false;
      },
    };
  };

  try {
    bootstrap();
  } catch {
    // 同步引导失败不置 installed，CDP 会按 probe 重新注入。
  }
})();
