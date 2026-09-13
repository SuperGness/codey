// Merges Codey-routed token usage into the Codex profile statistics page.
//
// The profile page reads its numbers from the official backend endpoint
// `GET /wham/profiles/me`, which can never observe traffic that Codey routes
// to third-party providers. This script polls the Codey bridge for locally
// scanned routed usage per UTC day, caches it in localStorage, and patches
// profile-stats responses at the `Response.prototype.text` layer (the app
// fetches through an IPC transport, so window.fetch is not involved; the
// transport hands the JSON parser a real `Response`, which we intercept by
// response shape — `daily_usage_buckets` — instead of URL).
(() => {
  if (window.__codeyProfileUsageMerge?.version === 1) {
    // Re-injection in the same document: re-read cached usage instead of
    // rebuilding state, so the installed text patch never goes stale.
    window.__codeyProfileUsageMerge.refresh?.();
    return;
  }

  const patchVersion = 1;
  const bridgePath = "/routed-usage";
  const storageKey = "__codeyRoutedTokenUsageV1";
  const profileMarker = '"daily_usage_buckets"';
  const pollIntervalMs = 60000;
  const firstPollDelayMs = 3000;

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
    lastPollAt: 0,
    lastPollStatus: "",
    pollInFlight: false,
  });

  const readStorage = () => {
    try {
      const raw = window.localStorage.getItem(storageKey);
      if (!raw) return null;
      const parsed = JSON.parse(raw);
      const days = parsed?.days;
      return days && typeof days === "object" ? days : null;
    } catch {
      return null;
    }
  };

  const applyDays = (days) => {
    state.days = days && typeof days === "object" ? days : null;
    state.dayCount = state.days ? Object.keys(state.days).length : 0;
    state.totalTokens = 0;
    if (state.days) {
      for (const value of Object.values(state.days)) {
        const tokens = Number(value?.total ?? value);
        if (Number.isFinite(tokens) && tokens > 0) state.totalTokens += tokens;
      }
    }
  };

  applyDays(readStorage());

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
        let current = 1;
        for (let i = sorted.length - 1; i > 0; i--) {
          const previous = new Date(Date.UTC(
            Number(sorted[i - 1].slice(0, 4)),
            Number(sorted[i - 1].slice(5, 7)) - 1,
            Number(sorted[i - 1].slice(8, 10)),
          ));
          const day = new Date(Date.UTC(
            Number(sorted[i].slice(0, 4)),
            Number(sorted[i].slice(5, 7)) - 1,
            Number(sorted[i].slice(8, 10)),
          ));
          if ((day - previous) / 86400000 === 1) current += 1;
          else break;
        }
        let longest = 1;
        let run = 1;
        for (let i = 1; i < sorted.length; i++) {
          const previous = new Date(Date.UTC(
            Number(sorted[i - 1].slice(0, 4)),
            Number(sorted[i - 1].slice(5, 7)) - 1,
            Number(sorted[i - 1].slice(8, 10)),
          ));
          const day = new Date(Date.UTC(
            Number(sorted[i].slice(0, 4)),
            Number(sorted[i].slice(5, 7)) - 1,
            Number(sorted[i].slice(8, 10)),
          ));
          run = (day - previous) / 86400000 === 1 ? run + 1 : 1;
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

  const pollOnce = async () => {
    if (state.pollInFlight || document.visibilityState === "hidden") return;
    state.pollInFlight = true;
    try {
      const result = await callBridge(bridgePath, {});
      state.lastPollAt = Date.now();
      state.lastPollStatus = result?.status ?? "unknown";
      if (result?.status === "ok" && result.days && typeof result.days === "object") {
        state.enabled = true;
        applyDays(result.days);
        try {
          window.localStorage.setItem(
            storageKey,
            JSON.stringify({ version: patchVersion, days: result.days }),
          );
        } catch { /* storage full or unavailable; in-memory merge still works */ }
      } else if (result?.status === "disabled") {
        state.enabled = false;
        applyDays(null);
        try {
          window.localStorage.removeItem(storageKey);
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

  window.__codeyProfileUsageMerge = Object.freeze({
    version: patchVersion,
    snapshot: () => ({
      installed: state.installed,
      enabled: state.enabled,
      dayCount: state.dayCount,
      totalTokens: state.totalTokens,
      lastPollAt: state.lastPollAt,
      lastPollStatus: state.lastPollStatus,
    }),
    // Re-reads the cached usage from localStorage; also invoked when the
    // injection bundle is re-evaluated in the same document.
    refresh: () => {
      applyDays(readStorage());
      return { dayCount: state.dayCount, enabled: state.enabled };
    },
    // Test hook: runs the merge against a synthetic profile payload.
    mergeForTest: (payload) => {
      const data = typeof payload === "string" ? JSON.parse(payload) : payload;
      return JSON.stringify(mergeProfileStats(data));
    },
  });
})();
