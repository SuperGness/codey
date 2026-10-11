// 当前对话 ID 仅用于查询；工作区、文件范围和提交内容均由后台确定。
(() => {
  if (window.__codeyConversationGit) return;
  const id = "codey-conversation-git";
  let enabled = false;
  let ready = false;
  let sessionId = null;
  let generation = 0;
  const checks = new Map();
  const statusCache = new Map();
  const statusCacheTtlMs = 30_000;
  const statusCacheLimit = 20;
  const maxChecks = 2;
  let refreshPending = false;
  let busy = false;
  let status = null;
  let panel = null;
  let timer = 0;
  let timerDelay = 0;
  let missingContextAt = null;
  let contextRetryTimer = 0;
  let contextLocation = "";
  const contextGraceMs = 1_000;
  const locationKey = () => window.location?.href || "";
  const context = () => window.__codeyPromptOptimize?.composerContext?.();
  const call = async (name, payload) => {
    if (typeof window.__codexSessionDeleteBridge !== "function") throw new Error("Codey bridge 尚未就绪");
    const result = await window.__codexSessionDeleteBridge(name, payload);
    if (result?.status === "failed") throw new Error(result.message || "Git 操作失败");
    return result;
  };
  const button = document.createElement("button");
  button.id = id;
  button.type = "button";
  button.setAttribute("aria-label", "当前对话 Git 提交与推送");
  button.setAttribute("aria-haspopup", "dialog");
  button.setAttribute("aria-expanded", "false");
  button.style.display = "none";
  button.innerHTML = `
    <svg class="codey-git-icon" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <circle cx="4" cy="4" r="2.2"></circle>
      <circle cx="4" cy="12" r="2.2"></circle>
      <circle cx="12" cy="6" r="2.2"></circle>
      <path d="M4 6.2v3.6"></path>
      <path d="M6 12h2a4 4 0 0 0 4-4V8.2"></path>
    </svg>
    <svg class="codey-git-spinner" viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true" focusable="false">
      <path d="M20 12a8 8 0 1 1-5.3-7.5"></path>
    </svg>
    <span>Git 推送</span>
  `;
  if (!button.textContent) button.textContent = "Git 推送";
  const style = document.createElement("style");
  style.textContent = `
    #${id} {
      -webkit-app-region: no-drag !important;
      pointer-events: auto !important;
      position: relative !important;
      z-index: 1 !important;
      display: none;
      flex: 0 0 auto;
      align-items: center;
      gap: 5px;
      box-sizing: border-box;
      min-height: 26px !important;
      height: 26px !important;
      margin: 0 0 0 6px;
      padding: 0 9px;
      border: 1px solid light-dark(rgba(0,0,0,.12), rgba(255,255,255,.16));
      border-radius: 999px;
      background: light-dark(#24292f, rgba(255,255,255,.12));
      color: light-dark(#ffffff, #f0f6fc);
      font: 500 12px/1 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      cursor: pointer;
      user-select: none;
      box-shadow: 0 1px 3px light-dark(rgba(0,0,0,.15), rgba(0,0,0,.4));
      opacity: .92;
      transition: opacity .15s ease, background-color .15s ease, transform .15s ease, box-shadow .15s ease;
    }
    #${id}:hover {
      opacity: 1;
      background: light-dark(#32383f, rgba(255,255,255,.2));
      border-color: light-dark(rgba(0,0,0,.2), rgba(255,255,255,.26));
      transform: translateY(-0.5px);
      box-shadow: 0 2px 6px light-dark(rgba(0,0,0,.2), rgba(0,0,0,.5));
    }
    #${id}:active {
      transform: translateY(0.5px);
      box-shadow: 0 1px 2px light-dark(rgba(0,0,0,.1), rgba(0,0,0,.3));
    }
    #${id}:disabled {
      opacity: .45;
      cursor: wait;
      box-shadow: none;
    }
    #${id} .codey-git-icon {
      flex: 0 0 auto;
      width: 13px;
      height: 13px;
    }
    #${id} .codey-git-spinner {
      display: none;
      flex: 0 0 auto;
      width: 13px;
      height: 13px;
      animation: codey-git-spin .75s linear infinite;
    }
    #${id}[data-busy="true"] .codey-git-icon {
      display: none;
    }
    #${id}[data-busy="true"] .codey-git-spinner {
      display: block;
    }
    @keyframes codey-git-spin {
      to { transform: rotate(360deg); }
    }
    #${id}-panel {
      -webkit-app-region: no-drag;
      position: fixed;
      z-index: 2147483646;
      box-sizing: border-box;
      max-width: calc(100vw - 32px);
      max-height: 78vh;
      overflow: auto;
      padding: 16px 18px;
      border: 1px solid light-dark(rgba(0,0,0,.15), rgba(255,255,255,.15));
      border-radius: 12px;
      background: light-dark(#ffffff, #1e1e1e);
      color: light-dark(#1f2328, #e6edf3);
      box-shadow: 0 16px 40px light-dark(rgba(0,0,0,.18), rgba(0,0,0,.55));
      color-scheme: light dark;
      font: 13px/1.55 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    #${id}-panel .codey-git-header {
      display: flex;
      align-items: baseline;
      flex-wrap: wrap;
      gap: 8px;
      margin-bottom: 12px;
      padding-bottom: 10px;
      border-bottom: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.1));
    }
    #${id}-panel .codey-git-header strong {
      font-size: 14px;
      font-weight: 600;
      color: light-dark(#1f2328, #f0f6fc);
    }
    #${id}-panel .codey-git-remote-info {
      font-size: 12px;
      color: light-dark(#656d76, #8b949e);
    }
    #${id}-panel .codey-git-section-title {
      font-weight: 600;
      font-size: 12px;
      margin: 10px 0 4px;
      color: light-dark(#57606a, #8b949e);
      text-transform: uppercase;
      letter-spacing: .3px;
    }
    #${id}-panel .codey-git-file-list {
      margin: 4px 0 10px;
      padding-left: 20px;
      overflow-wrap: anywhere;
      max-height: 120px;
      overflow: auto;
      color: light-dark(#24292f, #c9d1d9);
    }
    #${id}-panel .codey-git-file-list li {
      margin: 2px 0;
      font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
      font-size: 12px;
    }
    #${id}-panel .codey-git-partial-notice {
      margin: 6px 0 10px;
      padding: 6px 10px;
      border-radius: 6px;
      background: light-dark(rgba(234, 179, 8, .12), rgba(234, 179, 8, .16));
      border: 1px solid light-dark(rgba(202, 138, 4, .3), rgba(234, 179, 8, .3));
      color: light-dark(#854d0e, #fef08a);
      font-size: 12px;
    }
    #${id}-panel pre {
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      max-height: 28vh;
      overflow: auto;
      padding: 10px 12px;
      border: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.08));
      border-radius: 8px;
      background: light-dark(#f6f8fa, #161b22);
      color: light-dark(#1f2328, #e6edf3);
      font: 12px/1.5 ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    }
    #${id}-panel .codey-git-commit-msg {
      border-left: 3px solid light-dark(#0969da, #388bfd);
    }
    #${id}-panel .codey-git-diff-details {
      margin-top: 10px;
    }
    #${id}-panel .codey-git-diff-summary {
      display: flex;
      align-items: center;
      justify-content: space-between;
      list-style: none;
      cursor: pointer;
      user-select: none;
      padding: 4px 0 8px;
      margin: 0;
    }
    #${id}-panel .codey-git-diff-summary::-webkit-details-marker,
    #${id}-panel .codey-git-diff-summary::marker {
      display: none;
    }
    #${id}-panel .codey-git-diff-summary-left {
      display: flex;
      align-items: center;
      gap: 6px;
      flex-wrap: wrap;
    }
    #${id}-panel .codey-git-diff-chevron {
      display: inline-block;
      width: 5px;
      height: 5px;
      border-right: 2px solid light-dark(#57606a, #8b949e);
      border-bottom: 2px solid light-dark(#57606a, #8b949e);
      transform: rotate(-45deg);
      transition: transform .18s ease;
      margin: 0 2px 0 1px;
    }
    #${id}-panel details[open] .codey-git-diff-chevron {
      transform: rotate(45deg);
    }
    #${id}-panel .codey-git-diff-summary-title {
      font-size: 12px;
      font-weight: 600;
      color: light-dark(#1f2328, #f0f6fc);
      transition: color .15s ease;
    }
    #${id}-panel .codey-git-diff-summary:hover .codey-git-diff-summary-title {
      color: light-dark(#0969da, #58a6ff);
    }
    #${id}-panel .codey-git-stat-badge {
      display: inline-flex;
      align-items: center;
      font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
      font-size: 11px;
      font-weight: 600;
      padding: 0 6px;
      height: 18px;
      line-height: 18px;
      border-radius: 999px;
    }
    #${id}-panel .codey-git-stat-add {
      background: light-dark(rgba(46, 160, 67, .14), rgba(46, 160, 67, .2));
      color: light-dark(#1a7f37, #3fb950);
    }
    #${id}-panel .codey-git-stat-del {
      background: light-dark(rgba(248, 81, 73, .14), rgba(248, 81, 73, .2));
      color: light-dark(#cf222e, #f85149);
    }
    #${id}-panel .codey-git-stat-files {
      background: light-dark(rgba(0, 0, 0, .05), rgba(255, 255, 255, .08));
      color: light-dark(#57606a, #8b949e);
      font-family: system-ui, -apple-system, sans-serif;
      font-weight: 500;
    }
    #${id}-panel .codey-git-copy-btn {
      display: inline-flex;
      align-items: center;
      gap: 4px;
      padding: 3px 8px;
      font-size: 11px;
      font-weight: 500;
      line-height: 1.2;
      border: 1px solid light-dark(rgba(0,0,0,.12), rgba(255,255,255,.16));
      border-radius: 6px;
      background: light-dark(#f6f8fa, #21262d);
      color: light-dark(#57606a, #8b949e);
      cursor: pointer;
      transition: all .15s ease;
    }
    #${id}-panel .codey-git-copy-btn:hover {
      background: light-dark(#eef0f3, #30363d);
      color: light-dark(#1f2328, #f0f6fc);
      border-color: light-dark(rgba(0,0,0,.22), rgba(255,255,255,.26));
    }
    #${id}-panel .codey-git-copy-btn.codey-git-copied {
      color: light-dark(#1a7f37, #3fb950);
      border-color: light-dark(rgba(46,160,67,.4), rgba(46,160,67,.5));
      background: light-dark(rgba(46,160,67,.08), rgba(46,160,67,.14));
    }
    #${id}-panel .codey-git-diff {
      white-space: pre;
      overflow-wrap: normal;
      word-break: normal;
      overflow: auto;
      max-height: 32vh;
      margin: 0;
      padding: 0;
      border: 1px solid light-dark(rgba(0,0,0,.12), rgba(255,255,255,.14));
      border-radius: 8px;
      background: light-dark(#ffffff, #0d1117);
      color: light-dark(#1f2328, #e6edf3);
      font: 12px/1.55 ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
      tab-size: 2;
      box-shadow: inset 0 1px 2px light-dark(rgba(0,0,0,.03), rgba(0,0,0,.2));
    }
    #${id}-panel .codey-diff-file-header {
      position: sticky;
      top: 0;
      z-index: 2;
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
      padding: 6px 10px;
      background: light-dark(#f6f8fa, #161b22);
      border-bottom: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.1));
      font-size: 12px;
      user-select: text;
    }
    #${id}-panel .codey-diff-file-header:not(:first-child) {
      border-top: 1px solid light-dark(rgba(0,0,0,.12), rgba(255,255,255,.14));
      margin-top: 6px;
    }
    #${id}-panel .codey-diff-file-title {
      display: flex;
      align-items: center;
      gap: 6px;
      min-width: 0;
    }
    #${id}-panel .codey-diff-file-icon {
      flex: 0 0 auto;
      display: inline-flex;
      align-items: center;
      color: light-dark(#656d76, #8b949e);
    }
    #${id}-panel .codey-diff-file-path {
      font-weight: 600;
      color: light-dark(#1f2328, #f0f6fc);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    #${id}-panel .codey-diff-file-badge {
      font-size: 10px;
      padding: 1px 5px;
      border-radius: 4px;
      font-weight: 500;
      flex: 0 0 auto;
    }
    #${id}-panel .codey-diff-file-badge-new {
      background: light-dark(rgba(46,160,67,.14), rgba(46,160,67,.2));
      color: light-dark(#1a7f37, #3fb950);
    }
    #${id}-panel .codey-diff-file-badge-del {
      background: light-dark(rgba(248,81,73,.14), rgba(248,81,73,.2));
      color: light-dark(#cf222e, #f85149);
    }
    #${id}-panel .codey-diff-file-badge-ren {
      background: light-dark(rgba(9,105,218,.12), rgba(56,139,253,.18));
      color: light-dark(#0969da, #58a6ff);
    }
    #${id}-panel .codey-diff-file-stats {
      display: inline-flex;
      align-items: center;
      gap: 4px;
      font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
      font-size: 11px;
      font-weight: 600;
      flex: 0 0 auto;
    }
    #${id}-panel .codey-diff-file-meta {
      display: none;
    }
    #${id}-panel .codey-diff-hunk {
      display: flex;
      align-items: baseline;
      gap: 8px;
      padding: 3px 10px;
      background: light-dark(#ddf4ff, rgba(56, 139, 253, 0.14));
      border-top: 1px solid light-dark(rgba(9, 105, 218, 0.1), rgba(56, 139, 253, 0.16));
      border-bottom: 1px solid light-dark(rgba(9, 105, 218, 0.1), rgba(56, 139, 253, 0.16));
      font-size: 11.5px;
      user-select: text;
    }
    #${id}-panel .codey-diff-hunk-tag {
      font-weight: 600;
      color: light-dark(#0969da, #58a6ff);
      flex: 0 0 auto;
    }
    #${id}-panel .codey-diff-hunk-ctx {
      color: light-dark(#57606a, #8b949e);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: pre;
    }
    #${id}-panel .codey-diff-row {
      display: flex;
      min-width: 100%;
      box-sizing: border-box;
      line-height: 20px;
      transition: background-color .1s ease;
    }
    #${id}-panel .codey-diff-gutter {
      flex: 0 0 34px;
      box-sizing: border-box;
      text-align: right;
      padding-right: 8px;
      user-select: none;
      font-size: 11px;
      color: light-dark(#8c959f, #6e7681);
      opacity: .75;
    }
    #${id}-panel .codey-git-diff:not([data-has-hunk="true"]) .codey-diff-gutter {
      display: none;
    }
    #${id}-panel .codey-diff-gutter[data-num]::before {
      content: attr(data-num);
    }
    #${id}-panel .codey-diff-sign {
      flex: 0 0 16px;
      box-sizing: border-box;
      text-align: center;
      user-select: none;
      font-weight: 600;
    }
    #${id}-panel .codey-diff-text {
      flex: 1 1 auto;
      white-space: pre;
      padding-right: 12px;
    }
    #${id}-panel .codey-diff-row-add {
      background: light-dark(#e6ffec, rgba(46, 160, 67, 0.16));
      color: light-dark(#1a7f37, #3fb950);
    }
    #${id}-panel .codey-diff-row-add .codey-diff-gutter {
      background: light-dark(#dafbe1, rgba(46, 160, 67, 0.22));
      color: light-dark(#1a7f37, #3fb950);
    }
    #${id}-panel .codey-diff-row-add .codey-diff-sign {
      color: light-dark(#1a7f37, #3fb950);
    }
    #${id}-panel .codey-diff-row-add:hover {
      background: light-dark(#d1f8d8, rgba(46, 160, 67, 0.22));
    }
    #${id}-panel .codey-diff-row-del {
      background: light-dark(#ffebe9, rgba(248, 81, 73, 0.16));
      color: light-dark(#cf222e, #f85149);
    }
    #${id}-panel .codey-diff-row-del .codey-diff-gutter {
      background: light-dark(#ffdcd7, rgba(248, 81, 73, 0.22));
      color: light-dark(#cf222e, #f85149);
    }
    #${id}-panel .codey-diff-row-del .codey-diff-sign {
      color: light-dark(#cf222e, #f85149);
    }
    #${id}-panel .codey-diff-row-del:hover {
      background: light-dark(#ffceca, rgba(248, 81, 73, 0.22));
    }
    #${id}-panel .codey-diff-row-ctx:hover {
      background: light-dark(rgba(0, 0, 0, .03), rgba(255, 255, 255, .03));
    }
    #${id}-panel .codey-diff-row-notice {
      background: light-dark(#fff8c5, rgba(210, 153, 34, 0.15));
      color: light-dark(#9a6700, #e3b341);
      font-style: italic;
      padding-left: 68px;
    }
    #${id}-panel .codey-diff-empty {
      padding: 24px;
      text-align: center;
      color: light-dark(#656d76, #8b949e);
      font-size: 13px;
    }
    #${id}-panel .codey-git-actions {
      display: flex;
      gap: 8px;
      justify-content: flex-end;
      align-items: center;
      margin-top: 16px;
      padding-top: 12px;
      border-top: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.1));
    }
    #${id}-panel button {
      box-sizing: border-box;
      border: 1px solid light-dark(rgba(0,0,0,.15), rgba(255,255,255,.2));
      border-radius: 6px;
      padding: 6px 14px;
      background: light-dark(#f6f8fa, #21262d);
      color: light-dark(#24292f, #c9d1d9);
      font: 500 12px/1.4 system-ui, -apple-system, sans-serif;
      cursor: pointer;
      transition: all .15s ease;
    }
    #${id}-panel button:hover {
      background: light-dark(#eef0f3, #30363d);
      border-color: light-dark(rgba(0,0,0,.25), rgba(255,255,255,.3));
    }
    #${id}-panel button:disabled {
      opacity: .5;
      cursor: wait;
    }
    #${id}-panel .codey-git-btn-primary {
      background: light-dark(#1f883d, #238636);
      border-color: light-dark(#1a7f37, #2ea043);
      color: #ffffff;
      font-weight: 600;
    }
    #${id}-panel .codey-git-btn-primary:hover {
      background: light-dark(#1a7f37, #2ea043);
      border-color: light-dark(#166c2f, #3fb950);
    }
    #${id}-panel .codey-git-btn-secondary {
      background: light-dark(#24292f, #30363d);
      border-color: light-dark(#1b1f24, #3a424b);
      color: #ffffff;
      font-weight: 600;
    }
    #${id}-panel .codey-git-btn-secondary:hover {
      background: light-dark(#32383f, #3c444d);
    }
    #${id}-panel .codey-git-loading {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 16px 4px;
      color: light-dark(#57606a, #8b949e);
      font-size: 13px;
    }
    #${id}-panel .codey-git-loading-spinner {
      width: 14px;
      height: 14px;
      border: 2px solid light-dark(rgba(0,0,0,.15), rgba(255,255,255,.2));
      border-top-color: light-dark(#0969da, #58a6ff);
      border-radius: 50%;
      animation: codey-git-spin .75s linear infinite;
    }
    #${id}-panel [role=alert] {
      color: light-dark(#cf222e, #ff7b72);
      white-space: pre-wrap;
      background: light-dark(#ffebe9, rgba(248,81,73,.1));
      border: 1px solid light-dark(#ff818266, #f8514940);
      border-radius: 6px;
      padding: 8px 12px;
    }
  `;
  document.documentElement.appendChild(style);
  const close = () => { panel?.remove(); panel = null; button.setAttribute("aria-expanded", "false"); };
  const valid = (current, epoch) => enabled && generation === epoch && context()?.sessionId === current;
  const element = (tag, text) => {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const action = (text, handler) => {
    const node = element("button", text); node.type = "button";
    node.addEventListener("click", handler); return node;
  };
  const makePanel = (wide = true) => {
    close();
    panel = element("div"); panel.id = `${id}-panel`;
    panel.setAttribute("role", "dialog"); panel.setAttribute("aria-label", "当前对话 Git 提交与推送");
    const rect = button.getBoundingClientRect();
    panel.style.width = wide ? "680px" : "280px";
    panel.style.left = `${Math.max(16, Math.min(rect.left, window.innerWidth - (wide ? 696 : 296)))}px`;
    panel.style.bottom = `${Math.max(16, window.innerHeight - rect.top + 8)}px`;
    document.body.appendChild(panel);
    button.setAttribute("aria-expanded", "true");
    return panel;
  };
  const error = (container, message) => {
    container.replaceChildren();
    const text = element("p", message); text.setAttribute("role", "alert"); container.appendChild(text);
    const actions = element("div"); actions.className = "codey-git-actions";
    actions.appendChild(action("关闭", () => { close(); button.focus(); }));
    container.appendChild(actions);
    actions.querySelector("button")?.focus();
  };
  const createSvg = (tag) => typeof document.createElementNS === "function"
    ? document.createElementNS("http://www.w3.org/2000/svg", tag)
    : document.createElement(tag);
  const makeCopyIcon = () => {
    const svg = createSvg("svg");
    svg.setAttribute("viewBox", "0 0 16 16");
    svg.setAttribute("width", "12");
    svg.setAttribute("height", "12");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "1.6");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    const rect = createSvg("rect");
    rect.setAttribute("x", "5"); rect.setAttribute("y", "5");
    rect.setAttribute("width", "9"); rect.setAttribute("height", "9");
    rect.setAttribute("rx", "1.5");
    svg.appendChild(rect);
    const path = createSvg("path");
    path.setAttribute("d", "M3 11V3a1.5 1.5 0 0 1 1.5-1.5H11");
    svg.appendChild(path);
    return svg;
  };
  const makeCheckIcon = () => {
    const svg = createSvg("svg");
    svg.setAttribute("viewBox", "0 0 16 16");
    svg.setAttribute("width", "12");
    svg.setAttribute("height", "12");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    const path = createSvg("path");
    path.setAttribute("d", "M3 8.5l3.5 3.5 6.5-7");
    svg.appendChild(path);
    return svg;
  };
  const makeFileIcon = () => {
    const svg = createSvg("svg");
    svg.setAttribute("viewBox", "0 0 16 16");
    svg.setAttribute("width", "13");
    svg.setAttribute("height", "13");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "1.5");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    const path1 = createSvg("path");
    path1.setAttribute("d", "M9 2H4a1.5 1.5 0 0 0-1.5 1.5v9A1.5 1.5 0 0 0 4 14h8a1.5 1.5 0 0 0 1.5-1.5V6L9 2z");
    svg.appendChild(path1);
    const path2 = createSvg("path");
    path2.setAttribute("d", "M9 2v4h4");
    svg.appendChild(path2);
    return svg;
  };
  const parseDiff = (diffText) => {
    if (!diffText || !diffText.trim()) return [];
    const lines = diffText.split(/\r?\n/);
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    const files = [];
    let currentFile = null;

    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (line.startsWith("diff --git ")) {
        const match = line.match(/^diff --git a\/(.+?) b\/(.+)$/);
        const path = match ? (match[2] !== "/dev/null" ? match[2] : match[1]) : line.slice(11);
        currentFile = { path, isNew: false, isDeleted: false, isRenamed: false, metaLines: [], hunks: [], additions: 0, deletions: 0 };
        files.push(currentFile);
        continue;
      }
      if (!currentFile) {
        currentFile = { path: "", isNew: false, isDeleted: false, isRenamed: false, metaLines: [], hunks: [], additions: 0, deletions: 0 };
        files.push(currentFile);
      }
      if (line.startsWith("new file mode ")) currentFile.isNew = true;
      if (line.startsWith("deleted file mode ")) currentFile.isDeleted = true;
      if (line.startsWith("rename from ") || line.startsWith("rename to ")) currentFile.isRenamed = true;

      if (currentFile.hunks.length === 0 && (
        line.startsWith("index ") || line.startsWith("--- ") || line.startsWith("+++ ") ||
        line.startsWith("new file mode ") || line.startsWith("deleted file mode ") ||
        line.startsWith("old mode ") || line.startsWith("new mode ") ||
        line.startsWith("similarity index ") || line.startsWith("rename from ") ||
        line.startsWith("rename to ") || line.startsWith("Binary files ")
      )) {
        currentFile.metaLines.push(line);
        continue;
      }

      const hunkMatch = line.match(/^@@ (-[0-9]+(?:,[0-9]+)? \+[0-9]+(?:,[0-9]+)?) @@(.*)$/);
      if (hunkMatch) {
        const range = hunkMatch[1].match(/-(\d+)(?:,\d+)? \+(\d+)(?:,\d+)?/);
        currentFile.hunks.push({
          oldStart: range ? parseInt(range[1], 10) : 0,
          newStart: range ? parseInt(range[2], 10) : 0,
          rawHeader: `@@ ${hunkMatch[1]} @@`,
          heading: hunkMatch[2] || "",
          lines: [],
        });
        continue;
      }

      let currentHunk = currentFile.hunks[currentFile.hunks.length - 1];
      if (!currentHunk) {
        currentHunk = { oldStart: 0, newStart: 0, rawHeader: "", heading: "", lines: [] };
        currentFile.hunks.push(currentHunk);
      }
      if (line.startsWith("+") && !line.startsWith("+++")) currentFile.additions++;
      else if (line.startsWith("-") && !line.startsWith("---")) currentFile.deletions++;
      currentHunk.lines.push(line);
    }
    return files;
  };
  const renderDiff = (pre, diffText) => {
    pre.replaceChildren();
    const files = parseDiff(diffText);
    if (!files.length) {
      const empty = element("div", "无差异内容");
      empty.className = "codey-diff-empty";
      pre.appendChild(empty);
      return { additions: 0, deletions: 0, fileCount: 0 };
    }

    let totalAdditions = 0;
    let totalDeletions = 0;
    let hasAnyHunk = false;
    let namedCount = 0;

    files.forEach((file) => {
      totalAdditions += file.additions;
      totalDeletions += file.deletions;
      if (file.path) namedCount++;
      if (file.hunks.some((h) => Boolean(h.rawHeader))) hasAnyHunk = true;
    });

    if (hasAnyHunk) pre.setAttribute("data-has-hunk", "true");

    files.forEach((file) => {
      if (file.path) {
        const header = element("div");
        header.className = "codey-diff-file-header";

        const title = element("div");
        title.className = "codey-diff-file-title";
        title.appendChild(makeFileIcon());
        const pathSpan = element("span", file.path);
        pathSpan.className = "codey-diff-file-path";
        title.appendChild(pathSpan);

        if (file.isNew) {
          const badge = element("span", "新增");
          badge.className = "codey-diff-file-badge codey-diff-file-badge-new";
          title.appendChild(badge);
        } else if (file.isDeleted) {
          const badge = element("span", "已删除");
          badge.className = "codey-diff-file-badge codey-diff-file-badge-del";
          title.appendChild(badge);
        } else if (file.isRenamed) {
          const badge = element("span", "重命名");
          badge.className = "codey-diff-file-badge codey-diff-file-badge-ren";
          title.appendChild(badge);
        }
        header.appendChild(title);

        const stats = element("div");
        stats.className = "codey-diff-file-stats";
        if (file.additions > 0) {
          const add = element("span", `+${file.additions}`);
          add.className = "codey-git-stat-add";
          stats.appendChild(add);
        }
        if (file.deletions > 0) {
          const del = element("span", `-${file.deletions}`);
          del.className = "codey-git-stat-del";
          stats.appendChild(del);
        }
        header.appendChild(stats);
        pre.appendChild(header);
      }

      file.metaLines.forEach((meta) => {
        const metaRow = element("div", meta);
        metaRow.className = "codey-diff-file-meta";
        pre.appendChild(metaRow);
      });

      file.hunks.forEach((hunk) => {
        if (hunk.rawHeader) {
          const hunkRow = element("div");
          hunkRow.className = "codey-diff-hunk";
          const tag = element("span", hunk.rawHeader);
          tag.className = "codey-diff-hunk-tag";
          hunkRow.appendChild(tag);
          if (hunk.heading) {
            const ctx = element("span", hunk.heading);
            ctx.className = "codey-diff-hunk-ctx";
            hunkRow.appendChild(ctx);
          }
          pre.appendChild(hunkRow);
        }

        let oldNum = hunk.oldStart;
        let newNum = hunk.newStart;

        hunk.lines.forEach((line) => {
          const row = element("div");
          row.className = "codey-diff-row";

          let curOld = "";
          let curNew = "";
          let sign = " ";
          let type = "ctx";

          if (line.startsWith("+") && !line.startsWith("+++")) {
            type = "add";
            sign = "+";
            if (hunk.rawHeader) curNew = String(newNum++);
          } else if (line.startsWith("-") && !line.startsWith("---")) {
            type = "del";
            sign = "-";
            if (hunk.rawHeader) curOld = String(oldNum++);
          } else if (line.startsWith("\\")) {
            type = "notice";
            sign = "\\";
          } else {
            type = "ctx";
            sign = " ";
            if (hunk.rawHeader) {
              curOld = String(oldNum++);
              curNew = String(newNum++);
            }
          }

          row.classList.add(`codey-diff-row-${type}`);

          const gutterOld = element("span");
          gutterOld.className = "codey-diff-gutter codey-diff-gutter-old";
          if (curOld) gutterOld.setAttribute("data-num", curOld);
          row.appendChild(gutterOld);

          const gutterNew = element("span");
          gutterNew.className = "codey-diff-gutter codey-diff-gutter-new";
          if (curNew) gutterNew.setAttribute("data-num", curNew);
          row.appendChild(gutterNew);

          const signSpan = element("span", sign);
          signSpan.className = "codey-diff-sign";
          row.appendChild(signSpan);

          const hasPrefix = line.startsWith("+") || line.startsWith("-") || line.startsWith(" ");
          const text = hasPrefix ? line.slice(1) : line;
          const textSpan = element("span", text);
          textSpan.className = "codey-diff-text";
          row.appendChild(textSpan);

          pre.appendChild(row);
        });
      });
    });

    return { additions: totalAdditions, deletions: totalDeletions, fileCount: namedCount };
  };
  const preview = async () => {
    if (busy || !sessionId) return;
    const current = sessionId, epoch = generation;
    const container = makePanel(true);
    busy = true;
    button.disabled = true;
    button.dataset.busy = "true";

    const loading = element("div"); loading.className = "codey-git-loading";
    const spinner = element("span"); spinner.className = "codey-git-loading-spinner";
    loading.appendChild(spinner);
    loading.appendChild(element("span", "正在分析当前对话改动并生成提交说明…"));
    container.appendChild(loading);

    const loadingActions = element("div"); loadingActions.className = "codey-git-actions";
    const cancelInitial = action("取消", () => { close(); button.focus(); });
    cancelInitial.className = "codey-git-btn codey-git-btn-cancel";
    loadingActions.appendChild(cancelInitial);
    container.appendChild(loadingActions);
    cancelInitial.focus();

    try {
      const result = await call("/api/conversation_git_preview", { sessionId: current });
      if (!valid(current, epoch) || panel !== container) return;
      if (!result?.token || !result.message || !Array.isArray(result.files) || !result.diff) {
        throw new Error("提交预览不完整，请重试");
      }
      container.replaceChildren();

      const header = element("div"); header.className = "codey-git-header";
      header.appendChild(element("strong", `提交到 ${result.branch}`));
      if (result.remote && result.upstreamBranch) {
        const remoteInfo = element("span", ` · 推送到 ${result.remote}/${result.upstreamBranch}`);
        remoteInfo.className = "codey-git-remote-info";
        header.appendChild(remoteInfo);
      }
      container.appendChild(header);

      const filesHeader = element("div", `变更文件 (${result.files.length})`);
      filesHeader.className = "codey-git-section-title";
      container.appendChild(filesHeader);

      const files = element("ul"); files.className = "codey-git-file-list";
      result.files.forEach((file) => files.appendChild(element("li", file)));
      container.appendChild(files);

      if (Array.isArray(result.partialFiles) && result.partialFiles.length) {
        const partial = element("p", `以下文件仅提交本对话的改动块，其他改动继续保留在工作区：${result.partialFiles.join("、")}`);
        partial.className = "codey-git-partial-notice";
        container.appendChild(partial);
      }

      const logHeader = element("div", "提交说明");
      logHeader.className = "codey-git-section-title";
      container.appendChild(logHeader);

      const logPre = element("pre", result.message);
      logPre.className = "codey-git-commit-msg";
      container.appendChild(logPre);

      const details = element("details"); details.open = true;
      details.className = "codey-git-diff-details";

      const summary = element("summary");
      summary.className = "codey-git-diff-summary";

      const summaryLeft = element("div");
      summaryLeft.className = "codey-git-diff-summary-left";

      const chevron = element("span");
      chevron.className = "codey-git-diff-chevron";
      summaryLeft.appendChild(chevron);

      const summaryTitle = element("span", "本次完整改动");
      summaryTitle.className = "codey-git-diff-summary-title";
      summaryLeft.appendChild(summaryTitle);

      const diffPre = element("pre", result.diff);
      diffPre.className = "codey-git-diff";
      const stats = renderDiff(diffPre, result.diff);

      if (stats.additions > 0) {
        const addBadge = element("span", `+${stats.additions}`);
        addBadge.className = "codey-git-stat-badge codey-git-stat-add";
        summaryLeft.appendChild(addBadge);
      }
      if (stats.deletions > 0) {
        const delBadge = element("span", `-${stats.deletions}`);
        delBadge.className = "codey-git-stat-badge codey-git-stat-del";
        summaryLeft.appendChild(delBadge);
      }
      if (stats.fileCount > 1) {
        const filesBadge = element("span", `${stats.fileCount} 个文件`);
        filesBadge.className = "codey-git-stat-badge codey-git-stat-files";
        summaryLeft.appendChild(filesBadge);
      }
      summary.appendChild(summaryLeft);

      const summaryRight = element("div");
      summaryRight.className = "codey-git-diff-summary-right";
      const copyBtn = element("button");
      copyBtn.type = "button";
      copyBtn.className = "codey-git-copy-btn";
      copyBtn.appendChild(makeCopyIcon());
      const copyLabel = element("span", "复制 Diff");
      copyBtn.appendChild(copyLabel);
      copyBtn.addEventListener("click", async (e) => {
        e?.stopPropagation?.();
        e?.preventDefault?.();
        try {
          if (typeof navigator !== "undefined" && navigator?.clipboard?.writeText) {
            await navigator.clipboard.writeText(result.diff);
          }
          copyBtn.replaceChildren(makeCheckIcon(), element("span", "已复制"));
          copyBtn.classList.add("codey-git-copied");
          setTimeout(() => {
            copyBtn.replaceChildren(makeCopyIcon(), element("span", "复制 Diff"));
            copyBtn.classList.remove("codey-git-copied");
          }, 1800);
        } catch {
          // ignore copy failure
        }
      });
      summaryRight.appendChild(copyBtn);
      summary.appendChild(summaryRight);

      details.appendChild(summary);
      details.appendChild(diffPre);
      container.appendChild(details);

      const actions = element("div"); actions.className = "codey-git-actions";
      const cancel = action("取消", () => { close(); button.focus(); });
      cancel.className = "codey-git-btn codey-git-btn-cancel";

      const commitAction = async (push) => {
        if (busy) return;
        if (!valid(current, epoch)) { close(); return; }
        statusCache.clear();
        busy = true;
        cancel.disabled = true;
        commitOnly.disabled = true;
        commitAndPush.disabled = true;
        button.disabled = true;
        button.dataset.busy = "true";
        if (push) {
          commitAndPush.textContent = "正在提交并推送…";
        } else {
          commitOnly.textContent = "正在提交…";
        }
        try {
          const outcome = await call("/api/conversation_git_execute", {
            sessionId: current,
            token: result.token,
            push,
          });
          if (panel === container) {
            container.replaceChildren(element("p", outcome.message || "Git 操作已结束"));
            if (outcome.commit) container.appendChild(element("code", outcome.commit));
            const closeActions = element("div"); closeActions.className = "codey-git-actions";
            closeActions.appendChild(action("关闭", () => { close(); button.focus(); }));
            container.appendChild(closeActions);
            closeActions.querySelector("button")?.focus();
          }
        } catch (failure) {
          if (panel === container) error(container, String(failure?.message || failure));
        } finally {
          busy = false;
          button.disabled = false;
          delete button.dataset.busy;
          // 提交可能改变整个工作区，丢弃提交前的显示缓存和在途查询结果。
          statusCache.clear();
          generation += 1;
          status = null;
          void refresh();
        }
      };

      const commitOnly = action("提交", () => void commitAction(false));
      commitOnly.className = "codey-git-btn codey-git-btn-secondary";

      const commitAndPush = action("提交并推送", () => void commitAction(true));
      commitAndPush.className = "codey-git-btn codey-git-btn-primary";

      actions.append(cancel, commitOnly, commitAndPush);
      container.appendChild(actions);
      commitAndPush.focus();
    } catch (failure) {
      if (valid(current, epoch) && panel === container) error(container, String(failure?.message || failure));
    } finally {
      busy = false;
      button.disabled = false;
      delete button.dataset.busy;
      if (refreshPending) void refresh();
    }
  };
  button.addEventListener("click", (event) => {
    event.preventDefault(); event.stopPropagation();
    const current = context();
    if (busy || !status?.visible || current?.sessionId !== sessionId || !current?.target) return;
    if (panel) { close(); return; }
    void preview();
  });
  const renderButton = (current = context()) => {
    const target = current?.target;
    if (enabled && status?.visible && !target && missingContextAt !== null &&
        locationKey() === contextLocation && Date.now() - missingContextAt < contextGraceMs) return;
    if (!enabled || !status?.visible || !target) { button.style.display = "none"; return; }
    const optimizer = document.getElementById("codey-prompt-optimize-button");
    const anchor = optimizer?.parentElement === target.host && optimizer.style.display !== "none" ? optimizer : target.anchor;
    if (anchor.nextElementSibling !== button) target.host.insertBefore(button, anchor.nextElementSibling);
    button.style.display = "inline-flex";
  };
  const syncContext = () => {
    const current = context();
    const location = locationKey();
    const incomplete = !current?.sessionId || !current?.target;
    // 同一路由的输入框短暂重建时保留展示，操作仍要求实时会话身份一致。
    if (incomplete && sessionId && (!current?.sessionId || current.sessionId === sessionId) && location === contextLocation) {
      missingContextAt ??= Date.now();
      if (Date.now() - missingContextAt < contextGraceMs) {
        button.disabled = true;
        if (!contextRetryTimer) contextRetryTimer = setTimeout(() => {
          contextRetryTimer = 0;
          void refresh();
        }, 100);
        return null;
      }
    } else {
      missingContextAt = null;
      if (contextRetryTimer) { clearTimeout(contextRetryTimer); contextRetryTimer = 0; }
    }
    contextLocation = location;
    button.disabled = busy;
    const nextSession = current?.sessionId || null;
    if (nextSession !== sessionId) {
      sessionId = nextSession; generation += 1; status = null; close(); button.style.display = "none";
      const cached = statusCache.get(sessionId);
      if (cached && Date.now() - cached.at < statusCacheTtlMs) status = cached.result;
      else statusCache.delete(sessionId);
    }
    renderButton(current);
    return current;
  };
  const refresh = async () => {
    if (timer) { clearTimeout(timer); timer = 0; }
    const current = syncContext();
    if (!enabled || !sessionId || !current?.target) return;
    if (document.hidden) return;
    const active = checks.get(sessionId);
    // 同一会话合并查询，新会话可使用第二个名额，快速导航只保留最后一次补查。
    if (active?.epoch === generation) return;
    if (active || checks.size >= maxChecks || (busy && panel)) { refreshPending = true; return; }
    refreshPending = false;
    const epoch = generation, selected = sessionId;
    checks.set(selected, { epoch });
    try {
      const result = await call("/api/conversation_git_status", { sessionId: selected });
      if (result?.unavailable) throw new Error(result.reason || "Git 状态暂时不可用");
      if (!valid(selected, epoch)) {
        if (context()?.sessionId !== sessionId || (selected === sessionId && epoch !== generation)) refreshPending = true;
        return;
      }
      status = result;
      statusCache.delete(selected);
      if (result?.visible) {
        statusCache.set(selected, { result, at: Date.now() });
        if (statusCache.size > statusCacheLimit) statusCache.delete(statusCache.keys().next().value);
      }
      renderButton();
    } catch (failure) {
      if (valid(selected, epoch)) {
        const cached = statusCache.get(selected);
        const visible = status?.visible === true && cached && Date.now() - cached.at < statusCacheTtlMs;
        status = { visible: Boolean(visible), reason: String(failure?.message || failure) };
        if (!visible) statusCache.delete(selected);
        renderButton();
      }
    } finally {
      checks.delete(selected);
      if (context()?.sessionId !== sessionId) refreshPending = true;
      if (refreshPending) void refresh();
    }
  };
  const schedule = () => {
    // 导航时立即清理旧状态，当前会话优先查询；普通变化合并处理。
    const previous = sessionId;
    syncContext();
    if (!enabled) return;
    const delay = previous !== sessionId || status === null ? 0 : 300;
    if (timer) {
      if (delay >= timerDelay) return;
      clearTimeout(timer);
    }
    timerDelay = delay;
    timer = setTimeout(() => { timer = 0; void refresh(); }, delay);
  };
  const load = async () => {
    try {
      const config = await call("/settings/get", {});
      enabled = config?.conversationGit?.enabled === true; ready = true;
      statusCache.clear();
      generation += 1; status = null; close(); button.style.display = "none";
      await refresh();
    } catch { ready = false; enabled = false; button.style.display = "none"; }
  };
  window.addEventListener("codey:config-changed", load);
  for (const event of ["popstate", "hashchange", "focus"]) window.addEventListener(event, schedule);
  document.addEventListener("visibilitychange", schedule);
  document.addEventListener("keydown", (event) => {
    if (!panel) return;
    if (event.key === "Escape" && !busy) { close(); button.focus(); }
    if (event.key === "Tab") {
      const controls = [...panel.querySelectorAll("button:not(:disabled), summary")];
      const first = controls[0], last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }
  });
  document.addEventListener("pointerdown", (event) => {
    if (panel && !busy && !panel.contains(event.target) && !button.contains(event.target)) close();
  });
  const mutationHandler = (mutations) => {
    const ownNode = (node) => node?.id === id || node?.id === `${id}-panel` || node?.closest?.(`#${id}, #${id}-panel`);
    if (mutations.some((mutation) => {
      if (ownNode(mutation.target)) return false;
      if (!button.isConnected && [...(mutation.removedNodes || [])].includes(button)) return true;
      const nodes = [...(mutation.addedNodes || []), ...(mutation.removedNodes || [])];
      return !nodes.length || nodes.some((node) => !ownNode(node));
    })) schedule();
  };
  const options = { childList: true, subtree: true, attributes: true, attributeFilter: ["data-above-composer-conversation-id"] };
  if (window.__codeyMutationDispatcher?.subscribe) window.__codeyMutationDispatcher.subscribe(mutationHandler, options);
  else new MutationObserver(mutationHandler).observe(document.documentElement, options);
  setInterval(() => { if (!ready) void load(); else if (enabled) void refresh(); }, 10_000);
  window.__codeyConversationGit = { snapshot: () => ({ ready, enabled, sessionId, visible: button.style.display !== "none", reason: status?.reason || "", busy }) };
  void load();
})();
