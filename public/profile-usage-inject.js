// Merges Codey-routed token usage into the Codex profile statistics page and
// renders a per-model routed usage card below the official stats.
//
// The profile page reads its numbers from the official backend endpoint
// `GET /wham/profiles/me`, which can never observe traffic that Codey routes
// to third-party providers. This script polls the Codey bridge for locally
// scanned routed usage (per UTC day and per model), caches it in localStorage,
// patches profile-stats responses at the `Response.prototype.text` layer (the
// app fetches through an IPC transport, so window.fetch is not involved; the
// transport hands the JSON parser a real `Response`, which we intercept by
// response shape — `daily_usage_buckets` — instead of URL), and injects a
// "路由模型用量" card after the Token 活动 section.
(() => {
  if (window.__codeyProfileUsageMerge?.version === 2) {
    // Re-injection in the same document: re-read cached usage instead of
    // rebuilding state, so the installed text patch never goes stale.
    window.__codeyProfileUsageMerge.refresh?.();
    return;
  }

  const patchVersion = 3;
  const bridgePath = "/routed-usage";
  const storageKey = "__codeyRoutedUsageV3";
  const legacyStorageKeys = ["__codeyRoutedTokenUsageV1", "__codeyRoutedUsageV2"];
  const profileMarker = '"daily_usage_buckets"';
  const pollIntervalMs = 60000;
  const firstPollDelayMs = 3000;
  const cardReconcileMs = 2000;
  const palette = [
    "#3b82f6", "#22c55e", "#a855f7", "#ef4444", "#f97316", "#06b6d4",
    "#eab308", "#ec4899", "#14b8a6", "#8b5cf6", "#84cc16", "#f43f5e",
  ];
  const otherColor = "#64748b";

  const callBridge = (path, payload = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };

  // Shared across re-injections so the installed text patch always sees the
  // live state instead of a stale closure.
  const state = (window.__codeyProfileUsageState ??= {
    installed: true,
    enabled: true,
    days: null,
    dayCount: 0,
    totalTokens: 0,
    models: [],
    dailyByModel: null,
    lastPollAt: 0,
    lastPollStatus: "",
    pollInFlight: false,
  });

  const readStorage = () => {
    try {
      const raw = [storageKey, "__codeyRoutedUsageV2", "__codeyRoutedTokenUsageV1"]
        .map((key) => window.localStorage.getItem(key))
        .find((raw) => raw);
      if (!raw) return null;
      const parsed = JSON.parse(raw);
      const days = parsed?.days;
      if (!days || typeof days !== "object") return null;
      return {
        days,
        models: Array.isArray(parsed?.models) ? parsed.models : [],
        dailyByModel: parsed?.dailyByModel && typeof parsed.dailyByModel === "object"
          ? parsed.dailyByModel
          : {},
      };
    } catch {
      return null;
    }
  };

  const applyPayload = (payload) => {
    state.days = payload?.days && typeof payload.days === "object" ? payload.days : null;
    state.dayCount = state.days ? Object.keys(state.days).length : 0;
    state.totalTokens = 0;
    if (state.days) {
      for (const value of Object.values(state.days)) {
        const tokens = Number(value?.total ?? value);
        if (Number.isFinite(tokens) && tokens > 0) state.totalTokens += tokens;
      }
    }
    state.models = Array.isArray(payload?.models)
      ? payload.models
        .map((entry) => ({
          name: String(entry?.name ?? "").trim(),
          total: Math.max(0, Math.round(Number(entry?.total) || 0)),
          official: entry?.official === true,
        }))
        .filter((entry) => entry.name)
      : [];
    state.dailyByModel = payload?.dailyByModel && typeof payload.dailyByModel === "object"
      ? payload.dailyByModel
      : {};
  };

  applyPayload(readStorage());

  const utcDateKey = (offsetDays = 0) => {
    const date = new Date(Date.now() - offsetDays * 86400000);
    return date.toISOString().slice(0, 10);
  };

  const formatTokens = (value) => {
    if (!Number.isFinite(value) || value <= 0) return "0";
    if (value >= 1e8) return `${(value / 1e8).toFixed(value >= 1e9 ? 1 : 2).replace(/\.?0+$/, "")}亿`;
    if (value >= 1e4) return `${(value / 1e4).toFixed(1).replace(/\.0$/, "")}万`;
    return String(Math.round(value));
  };

  // ---- profile response merge -------------------------------------------
  const dateTotals = () => {
    const totals = new Map();
    if (!state.days) return totals;
    for (const [date, value] of Object.entries(state.days)) {
      const tokens = Number(value?.total ?? value);
      if (/^\d{4}-\d{2}-\d{2}$/.test(date) && Number.isFinite(tokens) && tokens > 0) {
        totals.set(date, Math.round(tokens));
      }
    }
    return totals;
  };

  const weekStart = (date) => {
    const utc = Date.UTC(
      Number(date.slice(0, 4)),
      Number(date.slice(5, 7)) - 1,
      Number(date.slice(8, 10)),
    );
    const shifted = utc - (((new Date(utc).getUTCDay() + 6) % 7) * 86400000);
    return new Date(shifted).toISOString().slice(0, 10);
  };

  const mergeBuckets = (buckets, byDate) => {
    if (!byDate.size) return;
    const index = new Map();
    for (let i = 0; i < buckets.length; i++) {
      const key = buckets[i]?.start_date;
      if (typeof key === "string") index.set(key, i);
    }
    for (const [date, tokens] of byDate) {
      const existing = index.get(date);
      if (existing != null) {
        buckets[existing].tokens = (Number(buckets[existing].tokens) || 0) + tokens;
      } else {
        index.set(date, buckets.length);
        buckets.push({ start_date: date, tokens });
      }
    }
    buckets.sort((a, b) => String(a.start_date).localeCompare(String(b.start_date)));
  };

  const mergeProfileStats = (data) => {
    const routed = dateTotals();
    if (!routed.size) return data;
    const stats = data.stats;
    if (!stats || typeof stats !== "object") return data;

    let routedTotal = 0;
    let routedMax = 0;
    const dailyRouted = new Map();
    const weeklyRouted = new Map();
    for (const [date, tokens] of routed) {
      routedTotal += tokens;
      routedMax = Math.max(routedMax, tokens);
      dailyRouted.set(date, (dailyRouted.get(date) || 0) + tokens);
      const week = weekStart(date);
      weeklyRouted.set(week, (weeklyRouted.get(week) || 0) + tokens);
    }

    if (Array.isArray(stats.daily_usage_buckets)) {
      mergeBuckets(stats.daily_usage_buckets, dailyRouted);
    }
    if (Array.isArray(stats.weekly_usage_buckets)) {
      mergeBuckets(stats.weekly_usage_buckets, weeklyRouted);
    }
    if (Array.isArray(stats.cumulative_daily_usage_buckets)
      && stats.cumulative_daily_usage_buckets.length > 0) {
      // Cumulative buckets are running totals of the daily series; fold routed
      // tokens into every entry at or after each routed day, then extend the
      // series for routed-only days beyond the server's last bucket.
      const cumulative = stats.cumulative_daily_usage_buckets;
      const dates = [...dailyRouted.keys()].sort();
      let acc = 0;
      let cursor = 0;
      for (const bucket of cumulative) {
        while (cursor < dates.length && dates[cursor] <= bucket.start_date) {
          acc += dailyRouted.get(dates[cursor]);
          cursor += 1;
        }
        bucket.tokens = (Number(bucket.tokens) || 0) + acc;
      }
      let running = cumulative[cumulative.length - 1].tokens;
      for (; cursor < dates.length; cursor++) {
        running += dailyRouted.get(dates[cursor]);
        cumulative.push({ start_date: dates[cursor], tokens: running });
      }
    }
    if (typeof stats.lifetime_tokens === "number") {
      stats.lifetime_tokens += routedTotal;
    }
    if (typeof stats.peak_daily_tokens === "number") {
      let mergedMax = routedMax;
      const buckets = stats.daily_usage_buckets;
      if (Array.isArray(buckets)) {
        for (const bucket of buckets) {
          const tokens = Number(bucket?.tokens) || 0;
          mergedMax = Math.max(mergedMax, tokens);
        }
      }
      stats.peak_daily_tokens = Math.max(stats.peak_daily_tokens, mergedMax);
    }
    // Streaks are derived from active days; recomputing over the merged daily
    // series can only extend them, and the max() keeps official values when
    // the server's semantics differ from a plain consecutive-day count.
    if (Array.isArray(stats.daily_usage_buckets) && typeof stats.current_streak_days === "number") {
      const active = new Set(
        stats.daily_usage_buckets
          .filter((bucket) => (Number(bucket?.tokens) || 0) > 0)
          .map((bucket) => String(bucket.start_date)),
      );
      if (active.size > 0) {
        const sorted = [...active].sort();
        const dayDiff = (a, b) => {
          const ta = Date.UTC(Number(a.slice(0, 4)), Number(a.slice(5, 7)) - 1, Number(a.slice(8, 10)));
          const tb = Date.UTC(Number(b.slice(0, 4)), Number(b.slice(5, 7)) - 1, Number(b.slice(8, 10)));
          return (tb - ta) / 86400000;
        };
        let current = 1;
        for (let i = sorted.length - 1; i > 0; i--) {
          if (dayDiff(sorted[i - 1], sorted[i]) === 1) current += 1;
          else break;
        }
        let longest = 1;
        let run = 1;
        for (let i = 1; i < sorted.length; i++) {
          run = dayDiff(sorted[i - 1], sorted[i]) === 1 ? run + 1 : 1;
          longest = Math.max(longest, run);
        }
        stats.current_streak_days = Math.max(stats.current_streak_days, current);
        if (typeof stats.longest_streak_days === "number") {
          stats.longest_streak_days = Math.max(stats.longest_streak_days, longest);
        }
      }
    }
    return data;
  };

  const isProfileStatsResponse = (data) =>
    !!data
    && typeof data === "object"
    && !!data.profile
    && !!data.stats
    && typeof data.stats === "object"
    && ("daily_usage_buckets" in data.stats || "lifetime_tokens" in data.stats);

  if (typeof Response === "function" && !Response.prototype.text.__codeyProfileUsagePatched) {
    const originalText = Response.prototype.text;
    Response.prototype.text = function patchedText(...args) {
      const promise = originalText.apply(this, args);
      if (!state.enabled || state.dayCount === 0) return promise;
      return promise.then((text) => {
        try {
          if (
            typeof text !== "string"
            || text.length < 256
            || text.length > 4194304
            || !text.includes(profileMarker)
          ) {
            return text;
          }
          const data = JSON.parse(text);
          if (!isProfileStatsResponse(data)) return text;
          return JSON.stringify(mergeProfileStats(data));
        } catch {
          return text;
        }
      });
    };
    Response.prototype.text.__codeyProfileUsagePatched = true;
  }

  // ---- 路由模型用量 card --------------------------------------------------
  const CARD_ID = "codey-routed-usage-card";
  const CARD_STYLE_ID = "codey-routed-usage-card-style";
  let cardRange = "30d";
  let cardSignature = "";

  const cardStyleText = `
.codey-ruc { margin: 12px 0 0; padding: 16px; border: 1px solid rgba(255,255,255,.09);
  border-radius: 12px; background: rgba(255,255,255,.035); color: #e8e8e8;
  font-family: inherit; font-size: 13px; line-height: 1.5; }
.codey-ruc-head { display: flex; align-items: center; justify-content: space-between;
  margin-bottom: 10px; }
.codey-ruc-head strong { font-size: 14px; }
.codey-ruc-ranges { display: flex; gap: 4px; }
.codey-ruc-ranges button { border: 1px solid transparent; background: transparent;
  color: #9b9b9b; font-size: 12px; padding: 3px 10px; border-radius: 999px;
  cursor: pointer; font-family: inherit; }
.codey-ruc-ranges button.on { background: rgba(255,255,255,.1); color: #fff; }
.codey-ruc-total { color: #9b9b9b; margin-bottom: 10px; }
.codey-ruc-total b { color: #fff; font-weight: 600; }
.codey-ruc-body { display: grid; grid-template-columns: 116px 1fr; gap: 14px;
  align-items: center; }
.codey-ruc-donut { width: 116px; height: 116px; border-radius: 50%; position: relative;
  background: conic-gradient(var(--codey-ruc-donut)); }
.codey-ruc-donut::after { content: ""; position: absolute; inset: 26px;
  border-radius: 50%; background: rgba(10,10,10,.92); }
.codey-ruc-donut span { position: absolute; inset: 0; display: flex; flex-direction: column;
  align-items: center; justify-content: center; z-index: 1; color: #fff; font-size: 13px; }
.codey-ruc-donut span small { color: #9b9b9b; font-size: 10px; }
.codey-ruc-row { display: grid; grid-template-columns: 14px minmax(80px, 1fr) 64px 40px;
  gap: 8px; align-items: center; padding: 4px 0; }
.codey-ruc-row .dot { width: 8px; height: 8px; border-radius: 50%; }
.codey-ruc-row .name { color: #d6d6d6; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; }
.codey-ruc-row .bar { height: 6px; border-radius: 4px; background: rgba(255,255,255,.07);
  overflow: hidden; }
.codey-ruc-row .bar i { display: block; height: 100%; border-radius: 4px; }
.codey-ruc-row .name b { font-weight: 500; color: #d6d6d6; }
.codey-ruc-row .name em { font-style: normal; font-size: 10px; padding: 0 4px;
  border-radius: 4px; margin-left: 5px; vertical-align: 1px; }
.codey-ruc-row .name em.off { color: #79c0ff; background: rgba(121,192,255,.12); }
.codey-ruc-row .name em.rou { color: #7ee2a8; background: rgba(126,226,168,.12); }
.codey-ruc-row .tokens, .codey-ruc-row .pct { color: #9b9b9b; text-align: right;
  font-variant-numeric: tabular-nums; }
.codey-ruc-empty { color: #9b9b9b; padding: 8px 0 2px; }
`;

  const rangeCutoff = (range) => {
    if (range === "7d") return utcDateKey(6);
    if (range === "30d") return utcDateKey(29);
    return "0000-00-00";
  };

  const rangeData = () => {
    const cutoff = rangeCutoff(cardRange);
    if (cardRange === "all") {
      return state.models.map((entry) => ({ ...entry }));
    }
    // The daily series carries no flags; recover them from the aggregate
    // model list so range-filtered rows keep the 官方/路由 badge.
    const officialByName = new Map(
      state.models.map((entry) => [entry.name, entry.official === true]),
    );
    const totals = new Map();
    for (const [date, perModel] of Object.entries(state.dailyByModel || {})) {
      if (date < cutoff) continue;
      for (const [name, tokens] of Object.entries(perModel)) {
        totals.set(name, (totals.get(name) || 0) + (Number(tokens) || 0));
      }
    }
    return [...totals.entries()]
      .map(([name, total]) => ({
        name,
        total,
        official: officialByName.get(name) ?? false,
      }))
      .sort((a, b) => b.total - a.total);
  };

  const cardHtml = () => {
    const rows = rangeData();
    const total = rows.reduce((sum, row) => sum + row.total, 0);
    let cursor = 0;
    const stops = [];
    const legendRows = rows.map((row, index) => {
      const color = row.name === "其他" ? otherColor : palette[index % palette.length];
      const share = total > 0 ? row.total / total : 0;
      const start = cursor;
      cursor += share;
      const from = Math.round(start * 360);
      const to = Math.round(Math.min(1, cursor) * 360);
      if (to > from) stops.push(`${color} ${from}deg ${to}deg`);
      else if (row.total > 0) stops.push(`${color} ${from}deg ${from + 1}deg`);
      const badge = row.name === "其他"
        ? ""
        : `<em class="${row.official ? "off" : "rou"}">${row.official ? "官方" : "路由"}</em>`;
      return `<div class="codey-ruc-row">`
        + `<span class="dot" style="background:${color}"></span>`
        + `<span class="name" title="${row.name}"><b>${row.name}</b>${badge}</span>`
        + `<span class="tokens">${formatTokens(row.total)}</span>`
        + `<span class="pct">${total > 0 ? Math.max(1, Math.round(share * 100)) : 0}%</span>`
        + `</div>`;
    });
    const donutCss = stops.length
      ? stops.join(", ")
      : `${otherColor} 0deg 360deg`;
    const rangeButton = (key, label) =>
      `<button data-range="${key}" class="${cardRange === key ? "on" : ""}">${label}</button>`;
    return `<div class="codey-ruc-head"><strong>模型用量</strong>`
      + `<div class="codey-ruc-ranges">${rangeButton("7d", "近7日")}${rangeButton("30d", "近30日")}${rangeButton("all", "全部")}</div></div>`
      + `<div class="codey-ruc-total">共 <b>${formatTokens(total)}</b> tokens · 官方与路由合计（官方额度数字不受影响）</div>`
      + `<div class="codey-ruc-body">`
      + `<div class="codey-ruc-donut" style="--codey-ruc-donut:${donutCss}"><span><b>${formatTokens(total)}</b><small>tokens</small></span></div>`
      + `<div class="codey-ruc-legend">${legendRows.length ? legendRows.join("") : `<div class="codey-ruc-empty">所选范围暂无用量</div>`}</div>`
      + `</div>`;
  };

  const findActivityCard = () => {
    for (const el of document.querySelectorAll("h1,h2,h3,h4,h5,span,div,p")) {
      if (el.childElementCount !== 0) continue;
      if (el.textContent?.trim() !== "Token 活动") continue;
      let box = el;
      for (let depth = 0; depth < 6 && box.parentElement; depth++) {
        box = box.parentElement;
        if (box.querySelector("button") && box !== el.parentElement) break;
      }
      return box;
    }
    return null;
  };

  const reconcileCard = () => {
    if (!state.enabled || !state.models.length) {
      document.getElementById(CARD_ID)?.remove();
      return;
    }
    if (!document.getElementById(CARD_STYLE_ID)) {
      const style = document.createElement("style");
      style.id = CARD_STYLE_ID;
      style.textContent = cardStyleText;
      document.head?.appendChild(style);
    }
    const anchor = findActivityCard();
    if (!anchor || !anchor.parentElement) return;
    let card = document.getElementById(CARD_ID);
    if (!card) {
      card = document.createElement("div");
      card.id = CARD_ID;
      card.addEventListener("click", (event) => {
        const range = event.target?.dataset?.range;
        if (range && range !== cardRange) {
          cardRange = range;
          card.innerHTML = cardHtml();
        }
      });
    }
    const signature = `${cardRange}|${state.models.map((m) => `${m.name}:${m.total}:${m.official ? 1 : 0}`).join(",")}`
      + `|${Object.keys(state.dailyByModel || {}).length}`;
    if (card.dataset.signature !== signature) {
      card.dataset.signature = signature;
      card.innerHTML = cardHtml();
    }
    if (card.nextElementSibling !== anchor.nextElementSibling || card.parentElement !== anchor.parentElement) {
      anchor.parentElement.insertBefore(card, anchor.nextSibling);
    }
  };

  // ---- polling ------------------------------------------------------------
  const pollOnce = async () => {
    if (state.pollInFlight || document.visibilityState === "hidden") return;
    state.pollInFlight = true;
    try {
      const result = await callBridge(bridgePath, {});
      state.lastPollAt = Date.now();
      state.lastPollStatus = result?.status ?? "unknown";
      if (result?.status === "ok" && result.days && typeof result.days === "object") {
        state.enabled = true;
        applyPayload(result);
        try {
          window.localStorage.setItem(storageKey, JSON.stringify({
            version: patchVersion,
            days: result.days,
            models: result.models ?? [],
            dailyByModel: result.dailyByModel ?? {},
          }));
          for (const key of legacyStorageKeys) window.localStorage.removeItem(key);
        } catch { /* storage full or unavailable; in-memory merge still works */ }
      } else if (result?.status === "disabled") {
        state.enabled = false;
        state.models = [];
        state.dailyByModel = {};
        applyPayload(null);
        try {
          window.localStorage.removeItem(storageKey);
          for (const key of legacyStorageKeys) window.localStorage.removeItem(key);
        } catch { /* ignore */ }
      }
    } catch {
      state.lastPollStatus = "error";
    } finally {
      state.pollInFlight = false;
    }
  };

  window.setTimeout(() => void pollOnce(), firstPollDelayMs);
  window.setInterval(() => void pollOnce(), pollIntervalMs);
  window.setInterval(() => {
    try {
      reconcileCard();
    } catch { /* a render failure must not kill the poll loop */ }
  }, cardReconcileMs);
  try {
    reconcileCard();
  } catch { /* same */ }

  window.__codeyProfileUsageMerge = Object.freeze({
    version: patchVersion,
    snapshot: () => ({
      installed: state.installed,
      enabled: state.enabled,
      dayCount: state.dayCount,
      totalTokens: state.totalTokens,
      modelCount: state.models.length,
      lastPollAt: state.lastPollAt,
      lastPollStatus: state.lastPollStatus,
    }),
    // Re-reads the cached usage from localStorage; also invoked when the
    // injection bundle is re-evaluated in the same document.
    refresh: () => {
      applyPayload(readStorage());
      return { dayCount: state.dayCount, modelCount: state.models.length, enabled: state.enabled };
    },
    // Test hook: runs the merge against a synthetic profile payload.
    mergeForTest: (payload) => {
      const data = typeof payload === "string" ? JSON.parse(payload) : payload;
      return JSON.stringify(mergeProfileStats(data));
    },
  });
})();
