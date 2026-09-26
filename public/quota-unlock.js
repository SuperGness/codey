// ============================================================
// Codey 用户脚本:额度用尽仍可发送(Quota Unlock)
// ------------------------------------------------------------
// Codey 在本地路由启用时经 CDP 以 document-start 注入 Codex
// 桌面端渲染进程主世界,先于页面全部业务脚本执行。
//
// 原理:官方额度用尽后,Codex 前端把 UI(发送按钮、编辑旧消息、
// composer 输入区)整体锁死——判定数据全部来自渲染进程内解析的
// /wham/usage 轮询响应与 /wham/usage/stream 的 usage.snapshot
// SSE 事件(React Query ['rate-limit-status'] 缓存)。这些 JSON
// 都在渲染进程里用 JSON.parse 解析,所以在 JSON.parse 层把
// 限流字段漂白成"健康",所有锁定判定(bkn/Tkn/Vkn →
// sendBlocked/inert)即恢复原状,第三方线路照常发请求。
//
// 关闭:在 Codex 页面 DevTools 执行
//   localStorage.setItem("codeyQuotaUnlock","off") 后刷新;
//   或 window.__codeyQuotaUnlock.disable()
// 状态:window.__codeyQuotaUnlock.status()
// ============================================================
(() => {
  "use strict";
  if (window.__codeyQuotaUnlock) return;

  let enabled = true;
  try {
    enabled = localStorage.getItem("codeyQuotaUnlock") !== "off";
  } catch {}

  const STATE = {
    version: "1.0.0",
    enabled,
    installedAt: new Date().toISOString(),
    parsePatched: false,
    wsPatched: false,
    sanitized: 0,
    lastSanitizedAt: null,
  };
  if (!enabled) {
    window.__codeyQuotaUnlock = {
      version: STATE.version,
      status() { return { ...STATE, note: "已停用(off),刷新/重启后不拦截" }; },
      enable() { try { localStorage.removeItem("codeyQuotaUnlock"); } catch {} return "已移除 off 标记,刷新或重启生效"; },
      disable() { try { localStorage.setItem("codeyQuotaUnlock", "off"); } catch {} return "已写入 off 标记,刷新或重启生效"; },
    };
    return;
  }

  const nativeParse = JSON.parse;

  // 把任意"限流窗口"对象的用量压到 3%、reset_at 修成未来 2 小时
  function fixWindow(w) {
    if (!w || typeof w !== "object") return;
    if (typeof w.used_percent === "number") w.used_percent = Math.min(w.used_percent, 3);
    if (typeof w.usedPercent === "number") w.usedPercent = Math.min(w.usedPercent, 3);
    if ("reset_at" in w || "resetAt" in w) {
      const t = Number(w.reset_at ?? w.resetAt);
      if (!Number.isFinite(t) || t * 1000 <= Date.now()) {
        const future = Math.floor(Date.now() / 1000) + 7200;
        if ("reset_at" in w) w.reset_at = future;
        if ("resetAt" in w) w.resetAt = future;
      }
    }
  }

  // 递归漂白:只改 OpenAI 限流语义的键,其余数据原样保留
  function sanitize(node, depth) {
    if (!node || typeof node !== "object" || depth > 12) return;
    if (Array.isArray(node)) {
      for (const x of node) sanitize(x, depth + 1);
      return;
    }
    // 1) 限流布尔判定
    const rl = node.rate_limit;
    if (rl && typeof rl === "object") {
      if ("allowed" in rl) rl.allowed = true;
      if ("limit_reached" in rl) rl.limit_reached = false;
    }
    if ("rate_limit_reached_type" in node) node.rate_limit_reached_type = null;
    if ("rateLimitReachedType" in node) node.rateLimitReachedType = null;
    if (("primary_window" in node || "secondary_window" in node) && ("allowed" in node || "limit_reached" in node)) {
      node.allowed = true;
      node.limit_reached = false;
    }
    // 2) 额度 / 花费控制
    const sc = node.spend_control;
    if (sc && typeof sc === "object" && "reached" in sc) sc.reached = false;
    const cr = node.credits;
    if (cr && typeof cr === "object") {
      if ("has_credits" in cr) cr.has_credits = true;
      if ("unlimited" in cr) cr.unlimited = true;
      if (typeof cr.balance === "number") cr.balance = Math.max(cr.balance, 1000000);
    }
    // 3) 横幅 / 侧边栏警告数据源
    if ("rate_limit_upsell" in node) node.rate_limit_upsell = null;
    if ("rate_limit_warning" in node) node.rate_limit_warning = null;
    if ("sidebar_usage_warnings" in node) node.sidebar_usage_warnings = [];
    // 4) /conversation/init 的发送闸门
    if (Array.isArray(node.blocked_features)) {
      node.blocked_features = node.blocked_features.filter(
        (f) => !(f && typeof f === "object" && (f.name === "send" || f.name === "tpp_send"))
      );
    }
    if (Array.isArray(node.limits_progress)) {
      node.limits_progress = node.limits_progress.filter(
        (f) => !(f && typeof f === "object" && typeof f.remaining === "number" && f.remaining <= 0)
      );
    }
    // 5) 用量窗口数值
    fixWindow(node.primary_window);
    fixWindow(node.secondary_window);
    fixWindow(node.primary);
    fixWindow(node.secondary);
    // 6) 继续走子节点(覆盖 additional_rate_limits / code_review_rate_limit 等)
    for (const k in node) {
      const v = node[k];
      if (v && typeof v === "object") sanitize(v, depth + 1);
    }
  }

  // 浅层标记:只有命中 OpenAI 限流语义键才做深漂白,避免误伤普通解析
  const MARKERS = [
    "rate_limit",
    "rateLimits",
    "rateLimitsByLimitId",
    "rate_limit_upsell",
    "rate_limit_warning",
    "sidebar_usage_warnings",
    "blocked_features",
    "limits_progress",
    "spend_control",
    "code_review_rate_limit",
  ];
  function looksRelevant(root) {
    if (!root || typeof root !== "object" || Array.isArray(root)) return false;
    for (const m of MARKERS) if (m in root) return true;
    return false;
  }

  function markSanitized() {
    STATE.sanitized += 1;
    STATE.lastSanitizedAt = new Date().toISOString();
  }

  // 主拦截:JSON.parse(渲染进程内所有 REST/WS/SSE JSON 都经它)
  try {
    JSON.parse = function (text, reviver) {
      const v = nativeParse.call(JSON, text, reviver);
      try {
        if (v && typeof v === "object" && (looksRelevant(v) || (typeof v.usage === "object" && v.usage !== null && looksRelevant(v.usage)))) {
          sanitize(v, 0);
          markSanitized();
        }
      } catch {}
      return v;
    };
    STATE.parsePatched = true;
  } catch {}

  // 兜底:WebSocket JSON-RPC 帧(account/rateLimits/read、rateLimits/updated)
  try {
    const NativeWS = window.WebSocket;
    function PatchedWebSocket(url, protocols) {
      const ws = protocols === undefined ? new NativeWS(url) : new NativeWS(url, protocols);
      const pending = new Map();
      const origSend = ws.send.bind(ws);
      ws.send = function (data) {
        try {
          if (typeof data === "string" && data.includes('"method"')) {
            const m = nativeParse(data);
            if (m && typeof m === "object" && m.method && m.id != null) pending.set(m.id, m.method);
          }
        } catch {}
        return origSend(data);
      };
      ws.addEventListener("message", (ev) => {
        try {
          if (typeof ev.data !== "string" || ev.data.indexOf("rateLimit") === -1) return;
          const m = nativeParse(ev.data);
          if (!m || typeof m !== "object") return;
          let touched = false;
          if (m.id != null && (m.result !== undefined || m.error !== undefined)) {
            const method = pending.get(m.id);
            pending.delete(m.id);
            if (method === "account/rateLimits/read" && m.result) {
              sanitize(m.result, 0);
              touched = true;
            }
          }
          if (m.method === "account/rateLimits/updated" && m.params) {
            sanitize(m.params, 0);
            touched = true;
          }
          if (touched) {
            Object.defineProperty(ev, "data", { value: JSON.stringify(m) });
            markSanitized();
          }
        } catch {}
      });
      return ws;
    }
    PatchedWebSocket.prototype = NativeWS.prototype;
    for (const k of ["CONNECTING", "OPEN", "CLOSING", "CLOSED"]) PatchedWebSocket[k] = NativeWS[k];
    window.WebSocket = PatchedWebSocket;
    STATE.wsPatched = true;
  } catch {}

  window.__codeyQuotaUnlock = {
    version: STATE.version,
    status() { return { ...STATE }; },
    enable() {
      try { localStorage.removeItem("codeyQuotaUnlock"); } catch {}
      return "已移除 off 标记,刷新或重启 Codex 生效";
    },
    disable() {
      try { localStorage.setItem("codeyQuotaUnlock", "off"); } catch {}
      return "已写入 off 标记,刷新或重启 Codex 生效";
    },
  };
})();
