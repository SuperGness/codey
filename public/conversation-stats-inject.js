// 状态条：会话总轮数、最近一轮的活动统计与会话累计 Token 用量，未知数据保留空值。
// /session/stats 返回统计范围；会话 id 取自可见 composer，隐藏会话不参与。
(() => {
  const PROBE_KEY = "__codeyConversationStats";
  const VERSION = 16;
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
  const conversationTurnSelector = "[data-turn-key]";
  const contextAttributes = [
    ...sessionIdAttributes,
    "data-app-action-sidebar-thread-active",
    "data-app-action-sidebar-thread-id",
    "hidden",
    "aria-hidden",
  ];
  // 空闲时结果连续不变就逐步放慢（3s → 6s → 12s）；发送消息后立即恢复快速
  // 轮询一段时间，所以新一轮的开始不依赖空闲间隔。
  const POLL_IDLE_MS = 3_000;
  const POLL_IDLE_MAX_MS = 12_000;
  const POLL_ACTIVE_MS = 1_100;
  const SUBMIT_POLL_DELAY_MS = 300;
  const SUBMIT_WATCH_MS = 15_000;
  const RESCAN_DEBOUNCE_MS = 120;
  const compositionKeys = ["cachedInputTokens", "uncachedInputTokens", "outputTokens"];

  let statsElement = null;
  let currentSessionId = "";
  let stats = null;
  let pollTimer = 0;
  let rescanTimer = 0;
  let positionFrame = 0;
  let fetchInFlight = false;
  let lastFetchOk = false;
  let lastResult = null;
  let sessionGeneration = 0;
  let fetchAttempted = false;
  let idleStreak = 0;
  let lastStatsKey = "";
  let watchUntil = 0;
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
      ${styleScope} { --cs-text: var(--text-primary, CanvasText); --cs-muted: color-mix(in srgb, var(--cs-text) 58%, transparent); --cs-soft: color-mix(in srgb, var(--cs-text) 84%, transparent); --cs-line: color-mix(in srgb, var(--cs-text) 9%, transparent); --cs-line-strong: color-mix(in srgb, var(--cs-text) 14%, transparent); --cs-fill: color-mix(in srgb, var(--cs-text) 5%, transparent); --cs-platter: light-dark(color-mix(in srgb, var(--cs-text) 4.2%, #ffffff80), color-mix(in srgb, var(--cs-text) 5.5%, #ffffff05)); --cs-shine: light-dark(#ffffffb3, #ffffff1f); --cs-surface: light-dark(color-mix(in srgb, var(--bg-primary, Canvas) 82%, #ffffff), color-mix(in srgb, var(--bg-primary, Canvas) 80%, #242428)); --cs-live: light-dark(#248a3d, #30d158); --cs-accent: light-dark(#007aff, #0a84ff); --cs-cyan: light-dark(#0088a3, #32ade6); --cs-violet: light-dark(#8944ab, #bf5af2); --cs-amber: light-dark(#c46a00, #ff9f0a); --cs-seg-cached: color-mix(in srgb, var(--cs-text) 24%, transparent); --cs-seg-input: var(--cs-accent); --cs-seg-output: var(--cs-violet); --cs-seg-write: var(--cs-amber); --cs-seg-model: var(--cs-accent); --cs-seg-tool: var(--cs-cyan); -webkit-app-region: no-drag !important; pointer-events: auto !important; position: relative; box-sizing: border-box; width: 100%; min-width: 0; padding: 6px 4px 0; color: var(--cs-text); font: 12px/1.5 -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif; font-variant-numeric: tabular-nums; }
      ${styleScope} *, ${styleScope} *::before, ${styleScope} *::after { box-sizing: border-box; }
      ${styleScope}[hidden], ${styleScope} [hidden] { display: none !important; }
      ${styleScope} .codey-cs-groups { display: flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 2px; width: fit-content; max-width: 100%; margin: 0 auto; padding: 2px; border-radius: 999px; background: var(--cs-fill); box-shadow: inset 0 0 0 .5px var(--cs-line), inset 0 .5px 0 var(--cs-shine); }
      ${styleScope} .codey-cs-divider { width: .5px; height: 11px; margin: 0 1px; background: var(--cs-line-strong); }
      ${styleScope} button { font: inherit; color: inherit; cursor: pointer; }
      ${styleScope} .codey-cs-summary { display: inline-flex; align-items: center; justify-content: center; flex-wrap: wrap; gap: 6px; min-width: 0; max-width: 100%; min-height: 24px; padding: 2px 11px; border: 0; border-radius: 999px; background: transparent; color: var(--cs-muted); text-align: center; transition: background-color .18s cubic-bezier(.32, .72, 0, 1), color .18s cubic-bezier(.32, .72, 0, 1), box-shadow .18s cubic-bezier(.32, .72, 0, 1), opacity .2s ease; }
      ${styleScope} .codey-cs-summary:hover, ${styleScope} .codey-cs-summary[data-codey-expanded="true"] { background: color-mix(in srgb, var(--bg-primary, Canvas) 88%, var(--cs-text) 6%); color: var(--cs-text); box-shadow: 0 1px 3px #0000001f, inset 0 .5px 0 var(--cs-shine); }
      #${barId}[data-running="true"] .codey-cs-activity-summary { background: color-mix(in srgb, var(--cs-live) 9%, transparent); color: var(--cs-soft); }
      ${styleScope} button:focus-visible { outline: 2px solid var(--cs-accent); outline-offset: 2px; }
      ${styleScope} .codey-cs-icon { display: inline-block; width: 13.5px; height: 13.5px; flex: 0 0 13.5px; background: currentColor; mask: var(--cs-icon) center / contain no-repeat; transition: color .18s ease; }
      ${styleScope} .codey-cs-activity-summary:is(:hover, [data-codey-expanded="true"]) .codey-cs-icon { color: var(--cs-accent); }
      ${styleScope} .codey-cs-tokens-summary:is(:hover, [data-codey-expanded="true"]) .codey-cs-icon { color: var(--cs-violet); }
      #${barId}[data-running="true"] .codey-cs-activity-summary .codey-cs-icon { color: var(--cs-live); }
      ${styleScope} .codey-cs-metric { white-space: nowrap; }
      ${styleScope} .codey-cs-sep { opacity: .38; }
      ${styleScope} .codey-cs-speed, ${styleScope} .codey-cs-total { color: var(--cs-text); font-weight: 600; letter-spacing: -.01em; }
      ${styleScope} .codey-cs-cache { display: inline-flex; align-items: center; gap: 4.5px; }
      ${styleScope} .codey-cs-cache-rate { color: var(--cs-soft); font-weight: 560; }
      ${styleScope} .codey-cs-ring { --cs-rate: 0; width: 12px; height: 12px; flex: none; border-radius: 50%; background: conic-gradient(var(--cs-cyan) calc(var(--cs-rate) * 1%), var(--cs-line-strong) 0); mask: radial-gradient(circle, transparent 3.4px, #000 3.9px); }
      ${styleScope} .codey-cs-live { position: relative; display: none; width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--cs-live); }
      ${styleScope} .codey-cs-live::after { content: ""; position: absolute; inset: 0; border-radius: inherit; background: inherit; animation: codey-cs-pulse 1.8s cubic-bezier(.22, 1, .36, 1) infinite; }
      #${barId}[data-running="true"] .codey-cs-live { display: inline-block; }
      #${barId}[data-codey-state="stale"] .codey-cs-summary { opacity: .55; }
      @keyframes codey-cs-pulse { from { transform: scale(1); opacity: .6; } to { transform: scale(2.8); opacity: 0; } }
      @media (prefers-reduced-motion: reduce) { ${styleScope} .codey-cs-live::after { animation: none; } ${styleScope} .codey-cs-summary, ${styleScope} .codey-cs-icon { transition: none; } }
      ${styleScope} .codey-cs-details { -webkit-app-region: no-drag !important; pointer-events: auto !important; position: fixed; inset: auto; margin: 0; z-index: 1000; width: min(384px, calc(100vw - 24px)); max-height: calc(100dvh - 24px); overflow: auto; padding: 13px; color: var(--cs-text); background: var(--cs-surface); backdrop-filter: saturate(190%) blur(32px); -webkit-backdrop-filter: saturate(190%) blur(32px); border: .5px solid var(--cs-line-strong); border-radius: 20px; box-shadow: 0 18px 44px -12px #0000004d, 0 3px 10px #00000014, inset 0 .5px 0 var(--cs-shine); user-select: text; }
      ${styleScope} .codey-cs-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 1px 3px 10px; }
      ${styleScope} .codey-cs-titles { display: flex; flex-direction: column; gap: 1px; min-width: 0; line-height: 1.25; }
      ${styleScope} .codey-cs-titles strong { font-size: 13.5px; font-weight: 650; letter-spacing: -.01em; white-space: nowrap; }
      ${styleScope} .codey-cs-subtitle { color: var(--cs-muted); font-size: 11px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
      ${styleScope} .codey-cs-heading-meta { margin-left: auto; display: flex; align-items: center; justify-content: flex-end; flex-wrap: wrap; gap: 5px; }
      ${styleScope} .codey-cs-pill { display: inline-flex; align-items: center; min-height: 22px; padding: 1px 8.5px; border-radius: 999px; background: var(--cs-platter); box-shadow: inset 0 0 0 .5px var(--cs-line), inset 0 .5px 0 var(--cs-shine); color: var(--cs-soft); font-size: 11px; font-weight: 560; white-space: nowrap; }
      ${styleScope} .codey-cs-coverage-pill { color: var(--cs-muted); font-weight: 500; }
      ${styleScope} .codey-cs-coverage-pill[data-tone="warn"] { background: color-mix(in srgb, var(--cs-amber) 14%, transparent); color: var(--cs-amber); }
      ${styleScope} .codey-cs-heading-stat { margin-left: auto; display: flex; flex-direction: column; align-items: flex-end; gap: 1px; line-height: 1.2; text-align: right; }
      ${styleScope} .codey-cs-heading-caption { color: var(--cs-muted); font-size: 10px; font-weight: 500; white-space: nowrap; }
      ${styleScope} .codey-cs-heading-total { font-size: 13.5px; font-weight: 650; color: var(--cs-text); letter-spacing: -.01em; text-align: right; white-space: nowrap; }
      ${styleScope} .codey-cs-pill.codey-cs-heading-total { font-size: 11px; font-weight: 600; color: var(--cs-soft); }
      ${styleScope} .codey-cs-body { display: flex; flex-direction: column; gap: 8px; }
      ${styleScope} dl { margin: 0; }
      ${styleScope} dt { color: var(--cs-muted); }
      ${styleScope} dd { margin: 0; color: var(--cs-soft); overflow-wrap: anywhere; }
      ${styleScope} .codey-cs-kpis { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); border-radius: 13px; background: var(--cs-platter); box-shadow: inset 0 0 0 .5px var(--cs-line), inset 0 .5px 0 var(--cs-shine); overflow: hidden; }
      ${styleScope} .codey-cs-detail-row[data-tier="primary"] { display: flex; flex-direction: column; justify-content: center; gap: 3px; min-width: 0; padding: 10px 12px; }
      ${styleScope} .codey-cs-detail-row[data-tier="primary"] + .codey-cs-detail-row[data-tier="primary"] { border-left: .5px solid var(--cs-line); }
      ${styleScope} .codey-cs-detail-row[data-tier="primary"] dt { display: inline-flex; align-items: center; gap: 5.5px; font-size: 11px; font-weight: 500; line-height: 1.3; white-space: nowrap; }
      ${styleScope} .codey-cs-detail-row:is([data-key="inputTokens"], [data-key="outputTokens"]) dt::before { content: ""; display: inline-block; width: 6px; height: 6px; flex: none; border-radius: 50%; background: var(--cs-dot); }
      ${styleScope} .codey-cs-detail-row[data-tier="primary"] dd { font-size: 14px; font-weight: 650; color: var(--cs-text); line-height: 1.3; letter-spacing: -.02em; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
      ${styleScope} .codey-cs-group { display: flex; flex-direction: column; padding: 10px 12px 4px; border-radius: 13px; background: var(--cs-platter); box-shadow: inset 0 0 0 .5px var(--cs-line), inset 0 .5px 0 var(--cs-shine); }
      ${styleScope} .codey-cs-composition { display: flex; gap: 1.5px; height: 6px; margin: 0 0 5px; border-radius: 999px; overflow: hidden; background: var(--cs-fill); }
      ${styleScope} .codey-cs-seg { flex: 0 1 0; min-width: 3px; background: var(--cs-dot); }
      ${styleScope} [data-key="modelDurationMs"] { --cs-dot: var(--cs-seg-model); }
      ${styleScope} [data-key="toolDurationMs"] { --cs-dot: var(--cs-seg-tool); }
      ${styleScope} [data-key="inputTokens"] { --cs-dot: var(--cs-seg-input); }
      ${styleScope} [data-key="cachedInputTokens"] { --cs-dot: var(--cs-seg-cached); }
      ${styleScope} [data-key="uncachedInputTokens"] { --cs-dot: var(--cs-seg-input); }
      ${styleScope} [data-key="outputTokens"] { --cs-dot: var(--cs-seg-output); }
      ${styleScope} [data-key="cacheWriteInputTokens"] { --cs-dot: var(--cs-seg-write); }
      ${styleScope} [data-key="cacheRate"] { --cs-dot: var(--cs-cyan); }
      ${styleScope} .codey-cs-breakdown { display: flex; flex-direction: column; }
      ${styleScope} .codey-cs-detail-row[data-tier="breakdown"] { position: relative; display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 6px 0; }
      ${styleScope} .codey-cs-detail-row[data-tier="breakdown"] + .codey-cs-detail-row[data-tier="breakdown"]::before { content: ""; position: absolute; top: 0; left: 13.5px; right: 0; height: .5px; background: var(--cs-line); }
      ${styleScope} .codey-cs-detail-row[data-tier="breakdown"] dt { display: inline-flex; align-items: center; gap: 6.5px; min-width: 0; color: var(--cs-soft); font-size: 12px; font-weight: 500; line-height: 1.35; }
      ${styleScope} .codey-cs-detail-row[data-tier="breakdown"] dt::before { content: ""; display: inline-block; width: 7px; height: 7px; flex: none; border-radius: 50%; background: var(--cs-dot, transparent); }
      ${styleScope} .codey-cs-detail-row[data-tier="breakdown"] dd { color: var(--cs-text); font-size: 12.5px; font-weight: 600; text-align: right; white-space: nowrap; flex: none; }
      ${styleScope} .codey-cs-detail-row[data-key="cacheRate"] dd { color: var(--cs-cyan); font-weight: 650; }
      ${styleScope} .codey-cs-note { margin: 8px 3px 0; color: var(--cs-muted); font-size: 10.5px; line-height: 1.5; }
      ${styleScope} .codey-cs-note:empty { display: none; }
      @media (max-width: 480px) { ${styleScope} .codey-cs-groups { gap: 2px; } ${styleScope} .codey-cs-divider { display: none; } ${styleScope} .codey-cs-summary { padding-inline: 8px; font-size: 11px; } ${styleScope} .codey-cs-details { padding: 11px; } }
      @media (max-width: 360px) { ${styleScope} .codey-cs-kpis { grid-template-columns: 1fr; } ${styleScope} .codey-cs-detail-row[data-tier="primary"] + .codey-cs-detail-row[data-tier="primary"] { border-left: 0; border-top: .5px solid var(--cs-line); } }
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
      const refs = { overlay, buttons: {}, panels: {}, fields: {}, rows: {}, notes: {}, subtitles: {}, segments: {}, activitySegments: {}, active: null, pinned: false, closeTimer: 0 };
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
      const decorative = (className, parent, text = "") => {
        const element = make("span", className, text, parent);
        element.setAttribute("aria-hidden", "true");
        return element;
      };
      for (const [kind, title, fields] of [
        ["activity", "最近一轮", [["speed", "平均生成速度"], ["averageTtftMs", "首内容平均等待"], ["modelDurationMs", "模型请求耗时"], ["toolDurationMs", "工具耗时"]]],
        // 顶部突出总输入与总输出；下方按构成列出缓存读取、未缓存输入、缓存写入与缓存命中率。
        ["tokens", "Token 用量", [["inputTokens", "总输入"], ["outputTokens", "总输出"], ["cachedInputTokens", "缓存读取"], ["uncachedInputTokens", "未缓存输入"], ["cacheWriteInputTokens", "缓存写入"], ["cacheRate", "缓存命中率"]]],
      ]) {
        if (kind === "tokens") decorative("codey-cs-divider", groups);
        const button = make("button", `codey-cs-summary codey-cs-${kind}-summary`, "", groups);
        button.type = "button";
        // 展开状态不写 aria-expanded：Codex 的 :has([aria-expanded="true"]) 规则会让
        // 每次切换触发约 65ms 的整页样式重算。读屏的展开状态改由下方 popovertarget 关联推导。
        button.setAttribute("data-codey-expanded", "false");
        button.setAttribute("aria-controls", `${barId}-${kind}`);
        if (kind === "activity") {
          decorative("codey-cs-live", button);
          icon(kind, button);
          refs.activity = make("span", "codey-cs-metric codey-cs-activity", "本轮 — 步", button);
          refs.activity.title = "最近一轮的模型生成步数；会话总轮数见详情。";
          decorative("codey-cs-sep", button, "·");
          refs.speed = make("span", "codey-cs-metric codey-cs-speed", "—", button);
        } else {
          icon(kind, button);
          refs.total = make("span", "codey-cs-metric codey-cs-total", "—", button);
          decorative("codey-cs-sep", button, "·");
          const cache = make("span", "codey-cs-metric codey-cs-cache", "", button);
          cache.title = "缓存命中率：缓存读取占输入总量的比例";
          refs.ring = decorative("codey-cs-ring", cache);
          make("span", "", "缓存", cache);
          refs["cache-rate"] = make("span", "codey-cs-cache-rate", "—", cache);
        }
        const panel = make("section", `codey-cs-details codey-cs-${kind}-details`, "", overlay);
        panel.id = `${barId}-${kind}`;
        panel.hidden = true;
        if (typeof panel.showPopover === "function") {
          panel.setAttribute("popover", "manual");
          // 只建立关联让无障碍树随浮层开合报告展开状态；点击的默认切换在下方阻止，开合仍由 setExpanded 控制。
          button.popoverTargetElement = panel;
        }
        panel.setAttribute("aria-label", title);
        const heading = make("div", "codey-cs-heading", "", panel);
        const titles = make("div", "codey-cs-titles", "", heading);
        make("strong", "", title, titles);
        refs.subtitles[kind] = make("span", "codey-cs-subtitle", "", titles);
        const body = make("div", "codey-cs-body", "", panel);
        const kpis = make("dl", "codey-cs-kpis", "", body);
        const group = make("div", "codey-cs-group", "", body);
        if (kind === "activity") {
          const meta = make("div", "codey-cs-heading-meta", "", heading);
          refs.turnCoverage = make("span", "codey-cs-pill codey-cs-coverage-pill", "统计范围未知", meta);
          refs.headingTotal = make("span", "codey-cs-pill codey-cs-heading-total", "—", meta);
          refs.activityComposition = make("div", "codey-cs-composition codey-cs-activity-composition", "", group);
          refs.activityComposition.setAttribute("aria-hidden", "true");
          refs.activityComposition.hidden = true;
          for (const key of ["modelDurationMs", "toolDurationMs"]) {
            refs.activitySegments[key] = make("span", "codey-cs-seg", "", refs.activityComposition);
            refs.activitySegments[key].setAttribute("data-key", key);
          }
        }
        if (kind === "tokens") {
          const stat = make("div", "codey-cs-heading-stat", "", heading);
          make("span", "codey-cs-heading-caption", "累计总量", stat);
          refs.fields.totalTokens = make("span", "codey-cs-heading-total", "—", stat);
          refs.fields.totalTokens.setAttribute("data-field", "totalTokens");
          refs.composition = make("div", "codey-cs-composition", "", group);
          refs.composition.setAttribute("aria-hidden", "true");
          refs.composition.hidden = true;
          for (const key of compositionKeys) {
            refs.segments[key] = make("span", "codey-cs-seg", "", refs.composition);
            refs.segments[key].setAttribute("data-key", key);
          }
        }
        const list = make("dl", "codey-cs-breakdown", "", group);
        for (const [index, [key, label]] of fields.entries()) {
          const primary = index < 2;
          const row = make("div", "codey-cs-detail-row", "", primary ? kpis : list);
          row.setAttribute("data-key", key);
          row.setAttribute("data-tier", primary ? "primary" : "breakdown");
          make("dt", "", label, row);
          refs.rows[key] = row;
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
        button.addEventListener("click", (event) => {
          event.preventDefault();
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
    // 气泡贴近入口，鼠标可以直接移入；只覆盖编辑器，不推动页面布局。
    const available = Math.max(80, rect.top - 20);
    // 先更新高度上限再测量：窗口变高时沿用旧上限会把浮层推到入口下方。
    panel.style.maxHeight = `${Math.min(height - 24, available)}px`;
    const panelRect = panel.getBoundingClientRect();
    panel.style.left = `${Math.max(12, Math.min(rect.left + (rect.width - panelRect.width) / 2, width - panelRect.width - 12))}px`;
    panel.style.top = `${Math.max(12, rect.top - Math.min(panelRect.height, available) - 8)}px`;
  };

  // 滚动和缩放事件可能一帧多次，合并到下一帧测量一次。
  const schedulePanelPosition = (event) => {
    if (disposed || !statsElement?.__codeyCs?.active || positionFrame) return;
    // 浮层内部滚动不会移动入口。
    if (event?.type === "scroll" && containsStats(event.target)) return;
    positionFrame = window.requestAnimationFrame(() => {
      positionFrame = 0;
      if (!disposed) positionPanel();
    });
  };

  const setExpanded = (expanded, restoreFocus = false, kind = null) => {
    if (positionFrame) window.cancelAnimationFrame(positionFrame);
    positionFrame = 0;
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
      setAttribute(refs.buttons[key], "data-codey-expanded", String(visible));
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
    idleStreak = 0;
    lastStatsKey = "";
    setExpanded(false);
  };

  const formatSpeed = (value) => (
    value == null || !Number.isFinite(value) || value < 0
      ? null
      : `${value >= 100 ? Math.round(value) : value.toFixed(1)} Token/s`
  );

  // 与请求日志同一口径：缓存读占输入总量的百分比；上游未回报缓存字段时
  // 返回 null，状态条显示未知值而不是显示 0%。
  const cacheRatePercent = (input, cached) => {
    if (input == null || cached == null || input <= 0 || cached < 0 || cached > input) return null;
    return Math.round((cached / input) * 100);
  };

  const setText = (element, value) => {
    // 相同 textContent 仍会替换文本节点，触发其他页面观察器。
    if (element.textContent === value) return false;
    // 输入框所在区域内替换文本节点会引发约 10ms 的样式重算，只改已有文本节点则不会。
    const node = element.firstChild;
    if (node?.nodeType === 3 && !node.nextSibling) node.nodeValue = value;
    else element.textContent = value;
    return true;
  };

  const setValue = (target, key, value) => {
    if (target[key] !== value) target[key] = value;
  };

  const setAttribute = (element, name, value) => {
    if (element.getAttribute(name) !== value) element.setAttribute(name, value);
  };

  // CSSOM 会规范化数值写法，读回比较不可靠；按元素记住上次写入值。
  const setStyle = (element, name, value) => {
    const written = element.__codeyCsStyle ||= {};
    if (written[name] === value) return;
    written[name] = value;
    element.style.setProperty?.(name, value);
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
    const coverageLabel = ({
      runtime: "本次运行",
      recorded: "会话记录",
      recorded_and_runtime: "会话记录 + 本次运行",
      partial: "部分记录",
    })[turn?.coverage] || "范围未知";
    const coverageNote = ({
      runtime: "仅包含本次运行可统计的步骤。",
      recorded: "仅包含可恢复且有有效计时的记录。",
      recorded_and_runtime: "合并可恢复记录与本次运行统计，缺失记录不计入。",
      partial: "本轮记录不完整，仅统计可恢复部分。",
    })[turn?.coverage] || "本轮统计范围未知。";
    const speedNote = running && stale ? "实时更新失败，恢复后继续显示本轮平均生成速度"
      : `按有效步骤的生成 Token 总量与生成耗时计算，不含等待首内容和工具间隔。${coverageNote}${estimatedSpeed && speedKnown ? "包含生成中的 Token 估算。" : ""}`;
    const rateValue = cacheRatePercent(data.inputTokens, data.cachedInputTokens);
    const rate = rateValue == null ? "—" : `${rateValue}%`;
    const refs = bar.__codeyCs;
    let panelContentChanged = setText(refs.speed, speedText);
    setValue(refs.speed, "title", speedNote);
    panelContentChanged = setText(refs["cache-rate"], rate) || panelContentChanged;
    const finite = value => Number.isFinite(value) && value >= 0;
    const count = value => Number.isSafeInteger(value) && value >= 0 ? value.toLocaleString("zh-CN") : "—";
    const duration = (value, state = "") => {
      if (!finite(value)) return "—";
      const seconds = value / 1000;
      if (state === "average" || seconds < 60) return `${Number(seconds.toFixed(1))}秒`;
      const hours = Math.floor(seconds / 3600);
      const minutes = Math.floor(seconds % 3600 / 60);
      const remainder = Math.floor(seconds % 60);
      return [hours ? `${hours}小时` : "", minutes ? `${minutes}分` : "", remainder ? `${remainder}秒` : ""].filter(Boolean).join(" ");
    };
    panelContentChanged = setText(refs.activity, `本轮 ${count(turn?.stepCount)} 步`) || panelContentChanged;
    const hasData = stats && (turn || hasRuntimeRequest || [data.turnCount, data.totalTokens, data.inputTokens, data.outputTokens].some(finite));
    const unknownUsage = data.usageSource === "unknown" || data.usageScope === "unknown";
    const scopeLabel = !stats || unknownUsage ? "统计范围未知" : sessionUsage ? "会话累计" : "本次运行";
    const state = !currentSessionId ? "waiting" : stale ? "stale" : !fetchAttempted ? "loading" : !hasData ? "empty" : "ok";
    const status = {
      waiting: "等待会话", loading: "读取统计中", stale: stats ? "统计更新失败" : "统计暂不可用", empty: "暂无统计",
      ok: running ? "本轮进行中" : turn?.status === "aborted" ? "本轮已中止" : (sessionUsage ? "历史统计" : "运行期统计"),
    }[state];
    setValue(bar.dataset, "status", status);
    const tokenValue = value => finite(value) ? `${count(value)} Tokens` : "—";
    data.uncachedInputTokens = finite(data.inputTokens) && finite(data.cachedInputTokens) && data.cachedInputTokens <= data.inputTokens ? data.inputTokens - data.cachedInputTokens : null;
    const headline = data.totalTokens != null ? `总计 ${formatTokenCount(data.totalTokens)} Tokens` : "—";
    panelContentChanged = setText(refs.total, headline) || panelContentChanged;
    setValue(refs.total, "title", data.totalTokens == null ? `${scopeLabel} Token 用量未知。`
      : `${scopeLabel} Token 总量 ${tokenValue(data.totalTokens)}，包含缓存读取。`);
    for (const [key, element] of Object.entries(refs.fields)) {
      const durationState = key === "averageTtftMs" ? "average" : "";
      panelContentChanged = setText(element, key === "cacheRate" ? rate : key === "speed" ? speedText : key.endsWith("Ms") ? duration(turn?.[key], durationState) : tokenValue(data[key])) || panelContentChanged;
    }
    // 耗时与 Token 构成条均按千分比分配宽度，避免同一数据每次轮询都改写样式。
    const modelMs = finite(turn?.modelDurationMs) ? turn.modelDurationMs : 0;
    const toolMs = finite(turn?.toolDurationMs) ? turn.toolDurationMs : 0;
    const durationTotal = modelMs + toolMs;
    setValue(refs.activityComposition, "hidden", durationTotal <= 0);
    if (durationTotal > 0) {
      for (const [key, val] of [["modelDurationMs", modelMs], ["toolDurationMs", toolMs]]) {
        setValue(refs.activitySegments[key], "hidden", val <= 0);
        setStyle(refs.activitySegments[key], "flex-grow", String(Math.round((val / durationTotal) * 1000)));
      }
    }
    const parts = data.uncachedInputTokens == null || !finite(data.outputTokens) ? null
      : { cachedInputTokens: data.cachedInputTokens, uncachedInputTokens: data.uncachedInputTokens, outputTokens: data.outputTokens };
    const partsTotal = parts ? compositionKeys.reduce((sum, key) => sum + parts[key], 0) : 0;
    setValue(refs.composition, "hidden", partsTotal <= 0);
    if (partsTotal > 0) {
      for (const key of compositionKeys) {
        setValue(refs.segments[key], "hidden", parts[key] <= 0);
        setStyle(refs.segments[key], "flex-grow", String(Math.round((parts[key] / partsTotal) * 1000)));
      }
    }
    setStyle(refs.ring, "--cs-rate", String(rateValue ?? 0));
    setValue(refs.rows.cacheWriteInputTokens, "hidden", !finite(data.cacheWriteInputTokens));
    const turnState = running ? "进行中" : turn?.status === "aborted" ? "已中止" : "已完成";
    setText(refs.headingTotal, state === "ok" ? `会话 ${count(data.turnCount)} 轮` : "—");
    setValue(refs.turnCoverage, "hidden", state !== "ok" || !turn);
    setValue(refs.turnCoverage.dataset, "tone", turn?.coverage === "partial" ? "warn" : "");
    setText(refs.turnCoverage, state === "ok" && turn ? coverageLabel : status);
    setValue(refs.turnCoverage, "title", coverageNote);
    setText(refs.subtitles.activity, state === "ok" && turn ? `${count(turn.stepCount)} 步 · ${turnState}` : status);
    setText(refs.subtitles.tokens, state === "ok" ? scopeLabel : status);
    const pendingNote = stale ? (stats ? "更新失败，当前数据可能已过期。" : "暂时无法读取统计，稍后自动重试。") : !currentSessionId ? "建立会话后显示统计。" : !stats ? "暂无统计数据。" : "";
    const activitySummary = turn ? "" : "暂无可识别的最近一轮。";
    panelContentChanged = setText(refs.notes.activity, pendingNote || activitySummary) || panelContentChanged;
    panelContentChanged = setText(refs.notes.tokens, pendingNote) || panelContentChanged;
    setAttribute(refs.buttons.activity, "aria-label", `本轮 ${count(turn?.stepCount)} 步，平均生成速度 ${speedText}，${status}。会话共 ${count(data.turnCount)} 轮。查看最近一轮详情`);
    setAttribute(refs.buttons.tokens, "aria-label", `Token 用量，${scopeLabel} 总量 ${data.totalTokens == null ? "—" : `${formatTokenCount(data.totalTokens)} Tokens`}，缓存命中 ${rate}，${status}。查看 Token 详情`);
    setValue(refs.panels.activity.dataset, "running", running ? "true" : "false");
    setValue(bar.dataset, "running", running ? "true" : "false");
    setValue(bar.dataset, "codeyState", state);
    // 入口仍可能因侧栏或输入框布局变化而移动；位置和数据都不变时才跳过浮层测量。
    positionPanel(anchor, !panelContentChanged);
  };

  const schedulePoll = (delay) => {
    clearTimeout(pollTimer);
    pollTimer = 0;
    if (disposed || document.visibilityState === "hidden") return;
    pollTimer = setTimeout(poll, delay);
  };

  const pollInterval = () => {
    if (stats?.lastTurn?.status === "running" || stats?.inFlight > 0 || Date.now() < watchUntil) {
      return POLL_ACTIVE_MS;
    }
    return Math.min(POLL_IDLE_MAX_MS, POLL_IDLE_MS * 2 ** Math.min(idleStreak, 2));
  };

  // 页面重新可见、获得焦点或用户发送消息时，空闲退避作废并尽快刷新。
  const wake = (delay = 0, watch = false) => {
    idleStreak = 0;
    if (watch) watchUntil = Date.now() + SUBMIT_WATCH_MS;
    schedulePoll(delay);
  };

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
      // 轮询中不切换显示状态：数据未变时整轮请求不产生任何 DOM 写入。
      fetchInFlight = true;
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
          const statsKey = JSON.stringify(stats);
          idleStreak = statsKey === lastStatsKey ? idleStreak + 1 : 0;
          lastStatsKey = statsKey;
        } else {
          lastFetchOk = false;
          idleStreak = 0;
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
        const visibilityChanged = (
          mutation.attributeName === "hidden"
          || mutation.attributeName === "aria-hidden"
        );
        if (visibilityChanged) {
          if (target.closest?.(conversationTurnSelector)) continue;
          const host = statsElement?.parentElement;
          const touchesComposer = host
            ? target === host || host.contains(target) || target.contains(host)
            : target.matches?.(composerInputSelector)
              || target.querySelector?.(composerInputSelector);
          if (!touchesComposer) continue;
        }
        if (contextAttributes.includes(mutation.attributeName)) {
          scheduleRescan();
          return;
        }
        continue;
      }
      const target = mutation.target;
      if (containsStats(target)) continue;
      // A mounted turn is message content. Session and composer markers live
      // outside it, so streaming additions do not need selector walks here.
      if (
        target instanceof HTMLElement
        && target.closest?.(conversationTurnSelector)
      ) continue;
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
    listen(document, "visibilitychange", () => wake());
    listen(window, "focus", () => wake());
    listen(window, "pageshow", () => wake());
    // 发送入口只看当前输入卡片：Enter 提交或点击卡片内按钮。误判只多一次轮询。
    const composerHost = () => statsElement?.isConnected ? statsElement.parentElement : null;
    const inComposer = (target) => Boolean(target && !containsStats(target) && composerHost()?.contains(target));
    listen(document, "keydown", (event) => {
      if (event.key === "Enter" && !event.shiftKey && !event.isComposing && inComposer(event.target)) {
        wake(SUBMIT_POLL_DELAY_MS, true);
      }
    }, true);
    listen(document, "click", (event) => {
      if (inComposer(event.target) && event.target.closest?.("button")) wake(SUBMIT_POLL_DELAY_MS, true);
    }, true);
    listen(window, "resize", schedulePanelPosition);
    listen(document, "scroll", schedulePanelPosition, true);
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
      refresh: () => wake(),
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
