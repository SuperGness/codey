// Lightweight renderer bootstrap injected by the Codey CDP launcher.
// The heavier session/sidebar tools live in codey-inject.js and are loaded
// after sidebar interaction or once the renderer has idle time.
(() => {
  const rendererCoreAlreadyLoaded = window.__codeyRendererCoreLoaded === true;
  window.__codeyRendererModuleReady = true;

  const sessionToolsLoadPath = "/internal/codey/session-tools/load";
  const updateCheckPath = "/api/check_for_updates";
  const backendStatusPath = "/backend/status";
  const backendHealthPath = "/backend/health";
  const accountUsagePath = "/account/usage";
  const buttonId = "codey-settings-button";
  const accountUsageId = "codey-account-usage";
  const styleId = "codey-core-injected-style";
  const updateAvailableEvent = "codey-update-availability-changed";
  const runtimeHealthEvent = "codey-runtime-health-changed";
  const configChangedEvent = "codey:config-changed";
  const updateCheckIntervalMs = 30 * 60 * 1000;
  const updateCheckTimeoutMs = 10_000;
  const runtimeHealthCheckIntervalMs = 30_000;
  const runtimeHealthCheckTimeoutMs = 3_000;
  const runtimeHealthFailureRetryMs = 1_000;
  const runtimeHealthFailureThreshold = 2;
  const accountUsageRefreshIntervalMs = 60_000;
  const accountUsageTimeoutMs = 8_000;
  const sessionToolsIdleLoadTimeoutMs = 5_000;
  const sidebarSelector = [
    "[data-app-action-sidebar-scroll]",
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
  ].join(", ");
  const headerSelector = "header, nav";
  const bootstrapProbeSelector = `${headerSelector}, ${sidebarSelector}`;
  const settingsIcon = `
    <svg viewBox="0 0 350 350" aria-hidden="true" focusable="false">
      <rect x="0" y="0" width="350" height="350" rx="34" fill="#fff" stroke="none"></rect>
      <path d="M70 301c-16 0-24-18-13-30l73-77c8-8 8-20 0-28L65 101C50 86 57 61 78 57c9-2 18 1 25 8l91 91c18 18 18 46 0 64l-66 66c-6 6-2 15 7 15h183" fill="none" stroke="currentColor" stroke-width="22" stroke-linecap="round" stroke-linejoin="round"></path>
    </svg>
  `;
  let sessionToolsLoadPromise = null;
  let scanTimer = 0;
  let updateCheckTimer = 0;
  let updateCheckInFlight = false;
  let runtimeHealthTimer = 0;
  let runtimeHealthCheckInFlight = false;
  let runtimeHealthFailures = 0;
  let runtimeHealthState = "checking";
  let runtimeHealthMessage = "";
  let runtimeHealthObservedAt = 0;
  let accountUsageTimer = 0;
  let accountUsageCheckInFlight = false;
  let accountUsagePollingEnabled = true;
  let accountUsageLastResult = null;
  let sessionToolsInteractionArmed = false;
  let sessionToolsIdleLoadScheduled = false;
  let bootstrapObserver = null;
  let headerMountDirty = true;

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
    return Promise.resolve({
      status: "failed",
      code: "bridge_unavailable",
      message: "Codey bridge 不可用",
    });
  };

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
      #${buttonId} .codey-runtime-badge { position: absolute; top: -2px; right: -2px; display: grid; width: 13px; height: 13px; place-items: center; border: 2px solid Canvas; border-radius: 999px; background: #ff453a; color: #fff; font: 800 9px/1 -apple-system, BlinkMacSystemFont, sans-serif; opacity: 0; transform: scale(.65); transition: opacity .15s ease, transform .15s ease; pointer-events: none; }
      #${buttonId}[data-codey-runtime-state="unavailable"] { background: rgba(255, 69, 58, .12); color: #ff453a; opacity: 1; }
      #${buttonId}[data-codey-runtime-state="unavailable"]:hover { background: rgba(255, 69, 58, .2); }
      #${buttonId}[data-codey-runtime-state="unavailable"] .codey-runtime-badge { opacity: 1; transform: scale(1); }
      #${buttonId}::after { content: ""; position: absolute; top: 5px; right: 5px; width: 7px; height: 7px; border-radius: 999px; background: #ff3b30; box-shadow: 0 0 0 2px Canvas; opacity: 0; transform: scale(.7); transition: opacity .15s ease, transform .15s ease; pointer-events: none; }
      #${buttonId}[data-codey-update-available="true"]::after { opacity: 1; transform: scale(1); }
      #${buttonId}[data-codey-header-actions="true"]::after { top: 4px; right: 4px; }
      #${buttonId}[data-codey-runtime-state="unavailable"][data-codey-update-available="true"]::after { top: auto; right: 3px; bottom: 3px; width: 5px; height: 5px; }
      #${accountUsageId} { -webkit-app-region: no-drag !important; position: relative; display: block; box-sizing: border-box; width: 100%; min-width: 0; padding: 7px 16px 8px; background: transparent; color: CanvasText; container-type: inline-size; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Helvetica Neue", sans-serif; line-height: 1.15; pointer-events: auto !important; transition: opacity .16s ease; user-select: none; }
      #${accountUsageId}[data-state="stale"] { opacity: .58; }
      #${accountUsageId}[data-state="error"] { padding-block: 8px; color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 10px; }
      #${accountUsageId} .codey-usage-list { display: grid; min-width: 0; grid-template-columns: repeat(var(--codey-usage-window-count), minmax(0, 1fr)); gap: 12px; }
      #${accountUsageId} .codey-usage-segment { min-width: 0; }
      #${accountUsageId}[data-window-count="2"] .codey-usage-segment + .codey-usage-segment { border-inline-start: 1px solid color-mix(in srgb, CanvasText 9%, transparent); padding-inline-start: 12px; }
      #${accountUsageId} .codey-usage-overview { display: flex; min-width: 0; align-items: baseline; gap: 6px; }
      #${accountUsageId} .codey-usage-window-label { min-width: 0; overflow: hidden; color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 10px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-plan-tag { flex: 0 0 auto; overflow: hidden; max-width: 44%; border: 1px solid color-mix(in srgb, #0a84ff 32%, transparent); border-radius: 4px; padding: 0 4px; background: color-mix(in srgb, #0a84ff 13%, transparent); color: color-mix(in srgb, #0a84ff 82%, CanvasText); font-size: 8px; font-weight: 700; line-height: 1.35; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-value { margin-inline-start: auto; font-size: 12px; font-variant-numeric: tabular-nums; font-weight: 700; letter-spacing: -.01em; white-space: nowrap; }
      #${accountUsageId} .codey-usage-meter { display: block; box-sizing: border-box; width: 100%; height: 2px; min-height: 2px; max-height: 2px; margin-top: 5px; overflow: hidden; border-radius: 999px; background: color-mix(in srgb, CanvasText 8%, transparent); contain: size; line-height: 0; }
      #${accountUsageId} .codey-usage-meter > span { display: block; box-sizing: border-box; width: 100%; height: 2px; min-height: 2px; max-height: 2px; border-radius: inherit; background: #0a84ff; transform: scaleX(var(--codey-usage-remaining)); transform-origin: left center; }
      #${accountUsageId} .codey-usage-reset { display: block; overflow: hidden; margin-top: 4px; color: color-mix(in srgb, CanvasText 45%, transparent); font-size: 9px; font-variant-numeric: tabular-nums; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-segment[data-tone="healthy"] .codey-usage-meter > span { background: #34c759; }
      #${accountUsageId} .codey-usage-segment[data-tone="normal"] .codey-usage-meter > span { background: #0a84ff; }
      #${accountUsageId} .codey-usage-segment[data-tone="warning"] .codey-usage-meter > span { background: #ffcc00; }
      #${accountUsageId} .codey-usage-segment[data-tone="critical"] .codey-usage-meter > span { background: #ff453a; }
      #${accountUsageId} .codey-usage-details { position: absolute; z-index: 2; right: 12px; bottom: calc(100% - 1px); left: 12px; box-sizing: border-box; min-width: 0; padding: 10px 11px 9px; border: 1px solid rgb(255 255 255 / .12); border-radius: 9px; background: rgb(34 34 34 / .97); box-shadow: 0 8px 24px rgb(0 0 0 / .24); color: #f5f5f5; opacity: 0; pointer-events: none; transform: translateY(4px); transition: opacity .14s ease, transform .14s ease, visibility .14s ease; visibility: hidden; }
      #${accountUsageId}:hover .codey-usage-details, #${accountUsageId}:focus-visible .codey-usage-details { opacity: 1; transform: translateY(0); visibility: visible; }
      #${accountUsageId}:focus-visible { border-radius: 6px; outline: 2px solid color-mix(in srgb, #0a84ff 72%, transparent); outline-offset: -2px; }
      #${accountUsageId} .codey-usage-details-heading { display: flex; min-width: 0; align-items: center; gap: 8px; padding-bottom: 7px; }
      #${accountUsageId} .codey-usage-details-title { color: rgb(245 245 245 / .72); font-size: 10px; font-weight: 650; }
      #${accountUsageId} .codey-usage-details-list { display: grid; min-width: 0; gap: 7px; }
      #${accountUsageId} .codey-usage-detail-row { min-width: 0; padding-block-start: 7px; border-block-start: 1px solid rgb(255 255 255 / .09); }
      #${accountUsageId} .codey-usage-detail-main, #${accountUsageId} .codey-usage-detail-meta { display: flex; min-width: 0; align-items: baseline; gap: 8px; }
      #${accountUsageId} .codey-usage-detail-label { overflow: hidden; color: rgb(245 245 245 / .78); font-size: 10px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-value { margin-inline-start: auto; color: #f5f5f5; font-size: 11px; font-variant-numeric: tabular-nums; font-weight: 720; white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-meta { margin-top: 3px; color: rgb(245 245 245 / .48); font-size: 9px; font-variant-numeric: tabular-nums; }
      #${accountUsageId} .codey-usage-detail-meta span:last-child { margin-inline-start: auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-details-updated { padding-top: 7px; color: rgb(245 245 245 / .38); font-size: 9px; font-variant-numeric: tabular-nums; text-align: right; }
      @container (max-width: 220px) {
        #${accountUsageId} .codey-usage-list { grid-template-columns: minmax(0, 1fr); gap: 7px; }
        #${accountUsageId}[data-window-count="2"] .codey-usage-segment + .codey-usage-segment { border-block-start: 1px solid color-mix(in srgb, CanvasText 9%, transparent); border-inline-start: 0; padding-block-start: 7px; padding-inline-start: 0; }
      }
      @media (prefers-reduced-motion: reduce) {
        #${buttonId}, #${buttonId} *, #${accountUsageId}, #${accountUsageId} * { animation: none !important; transition: none !important; }
      }
    `;
    document.documentElement.appendChild(style);
  };

  const hasDetectedUpdate = () =>
    window.__codeyUpdateAvailability?.updateAvailable === true;

  const dispatchUpdateAvailability = () => {
    if (
      typeof window.dispatchEvent !== "function"
      || typeof CustomEvent !== "function"
    ) return;
    window.dispatchEvent(new CustomEvent(updateAvailableEvent, {
      detail: hasDetectedUpdate() ? window.__codeyUpdateAvailability : null,
    }));
  };

  const applyUpdateBadge = (button = document.getElementById(buttonId)) => {
    if (!(button instanceof HTMLElement)) return;
    button.setAttribute("data-codey-runtime-state", runtimeHealthState);
    if (hasDetectedUpdate()) {
      button.setAttribute("data-codey-update-available", "true");
    } else {
      button.removeAttribute?.("data-codey-update-available");
    }
    if (runtimeHealthState === "unavailable") {
      const detail = runtimeHealthMessage || "Codey 后端未响应";
      const updateLabel = hasDetectedUpdate() ? "，另有可用更新" : "";
      button.setAttribute(
        "aria-label",
        `Codey 进程异常或连接中断，点击查看处理提示${updateLabel}`,
      );
      button.title = `Codey 进程异常或连接中断：${detail}（点击查看处理提示）${updateLabel}`;
      return;
    }
    if (hasDetectedUpdate()) {
      button.setAttribute("aria-label", "打开 Codey 配置，有可用更新");
      button.title = "打开 Codey 配置（发现新版本）";
    } else {
      button.setAttribute("aria-label", "打开 Codey 配置");
      button.title = "打开 Codey 配置";
    }
  };

  const runtimeHealthSnapshot = () => ({
    state: runtimeHealthState,
    message: runtimeHealthMessage,
    observedAt: runtimeHealthObservedAt,
    consecutiveFailures: runtimeHealthFailures,
  });

  const setRuntimeHealthState = (state, message = "") => {
    const nextState = state === "healthy" || state === "unavailable"
      ? state
      : "checking";
    const nextMessage = String(message || "").slice(0, 160);
    const changed = runtimeHealthState !== nextState || runtimeHealthMessage !== nextMessage;
    runtimeHealthState = nextState;
    runtimeHealthMessage = nextMessage;
    runtimeHealthObservedAt = Date.now();
    window.__codeyRuntimeHealth = runtimeHealthSnapshot();
    if (changed) applyUpdateBadge();
    if (
      changed
      && typeof window.dispatchEvent === "function"
      && typeof CustomEvent === "function"
    ) {
      window.dispatchEvent(new CustomEvent(runtimeHealthEvent, {
        detail: window.__codeyRuntimeHealth,
      }));
    }
    return window.__codeyRuntimeHealth;
  };

  const setUpdateAvailability = (result, { dispatch = true } = {}) => {
    window.__codeyUpdateAvailability = result?.updateAvailable === true
      ? result
      : null;
    applyUpdateBadge();
    if (hasDetectedUpdate()) {
      window.clearTimeout(updateCheckTimer);
      updateCheckTimer = 0;
    }
    if (dispatch) dispatchUpdateAvailability();
  };

  const withTimeout = (
    promise,
    timeoutMs,
    message = "检查更新超时",
  ) => new Promise((resolve, reject) => {
    const timer = window.setTimeout(
      () => reject(new Error(message)),
      timeoutMs,
    );
    Promise.resolve(promise).then(
      (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });

  const scheduleRuntimeHealthCheck = (delayMs = runtimeHealthCheckIntervalMs) => {
    window.clearTimeout(runtimeHealthTimer);
    runtimeHealthTimer = 0;
    if (document.visibilityState === "hidden") return;
    runtimeHealthTimer = window.setTimeout(() => {
      runtimeHealthTimer = 0;
      void checkRuntimeHealth();
    }, delayMs);
  };

  const checkRuntimeHealth = async () => {
    if (document.visibilityState === "hidden") {
      scheduleRuntimeHealthCheck();
      return runtimeHealthSnapshot();
    }
    if (runtimeHealthCheckInFlight) return runtimeHealthSnapshot();
    runtimeHealthCheckInFlight = true;
    try {
      if (typeof window.__codexSessionDeleteBridge !== "function") {
        runtimeHealthFailures = runtimeHealthFailureThreshold;
        return setRuntimeHealthState("unavailable", "Codey bridge 不可用");
      }
      const result = await withTimeout(
        callBridge(backendHealthPath, {}, { timeoutMs: runtimeHealthCheckTimeoutMs }),
        runtimeHealthCheckTimeoutMs + 250,
        "Codey 后端健康检查超时",
      );
      if (result?.status === "ok") {
        runtimeHealthFailures = 0;
        return setRuntimeHealthState("healthy");
      }
      const error = new Error(result?.message || "Codey 后端未响应");
      error.code = result?.code || "backend_unavailable";
      throw error;
    } catch (error) {
      runtimeHealthFailures += 1;
      const immediate = error?.code === "bridge_unavailable";
      if (immediate) runtimeHealthFailures = runtimeHealthFailureThreshold;
      if (runtimeHealthFailures >= runtimeHealthFailureThreshold) {
        return setRuntimeHealthState("unavailable", "Codey 后端未响应");
      }
      return setRuntimeHealthState("checking", "正在确认 Codey 进程状态");
    } finally {
      runtimeHealthCheckInFlight = false;
      const nextDelay = runtimeHealthFailures > 0
        && runtimeHealthFailures < runtimeHealthFailureThreshold
        ? runtimeHealthFailureRetryMs
        : runtimeHealthCheckIntervalMs;
      scheduleRuntimeHealthCheck(nextDelay);
    }
  };

  const scheduleUpdateCheck = (delayMs = updateCheckIntervalMs) => {
    if (hasDetectedUpdate()) return;
    window.clearTimeout(updateCheckTimer);
    updateCheckTimer = window.setTimeout(() => {
      updateCheckTimer = 0;
      void checkForUpdatesSilently();
    }, delayMs);
  };

  const checkForUpdatesSilently = async () => {
    if (updateCheckInFlight || hasDetectedUpdate()) return;
    updateCheckInFlight = true;
    try {
      const result = await withTimeout(
        callBridge(updateCheckPath, {}, { timeoutMs: updateCheckTimeoutMs }),
        updateCheckTimeoutMs,
      );
      if (result?.status !== "failed" && result?.updateAvailable === true) {
        setUpdateAvailability(result);
        return;
      }
    } catch {
      // 更新地址不可达或检查超时时直接跳过，不阻塞 Codex 页面。
    } finally {
      updateCheckInFlight = false;
      if (!hasDetectedUpdate()) scheduleUpdateCheck();
    }
  };

  const hydrateUpdateAvailability = async () => {
    try {
      const status = await withTimeout(
        callBridge(backendStatusPath, {}, { timeoutMs: updateCheckTimeoutMs }),
        updateCheckTimeoutMs,
        "读取更新状态超时",
      );
      setUpdateAvailability(status?.availableUpdate || null);
    } catch {
      setUpdateAvailability(null);
    } finally {
      if (!hasDetectedUpdate()) scheduleUpdateCheck();
    }
  };

  const accountUsageWindowKind = (window) => {
    const minutes = Number(window?.windowMinutes);
    if (!Number.isFinite(minutes) || !Number.isFinite(Number(window?.usedPercent))) {
      return null;
    }
    if (minutes >= 6 * 24 * 60 && minutes <= 8 * 24 * 60) return "weekly";
    if (minutes >= 270 && minutes <= 330) return "five-hour";
    return null;
  };

  const normalizeAppServerUsageWindow = (window) => {
    const usedPercent = Number(window?.usedPercent);
    const windowMinutes = Number(window?.windowDurationMins);
    if (!Number.isFinite(usedPercent) || !Number.isFinite(windowMinutes) || windowMinutes <= 0) {
      return null;
    }
    const resetsAtValue = Number(window?.resetsAt);
    const resetsAt = Number.isFinite(resetsAtValue) && resetsAtValue > 0
      ? Math.round(resetsAtValue > 10_000_000_000 ? resetsAtValue / 1000 : resetsAtValue)
      : undefined;
    return {
      usedPercent: Math.max(0, Math.min(100, usedPercent)),
      windowMinutes: Math.max(1, Math.round(windowMinutes)),
      ...(resetsAt ? { resetsAt } : {}),
    };
  };

  const normalizeAppServerAccountUsage = (response) => {
    const payload = response?.result && typeof response.result === "object"
      ? response.result
      : response;
    if (!payload || typeof payload !== "object") {
      throw new Error("Codex 官方额度响应格式无效");
    }
    const buckets = [];
    if (payload.rateLimits && typeof payload.rateLimits === "object") {
      buckets.push(payload.rateLimits);
    }
    if (payload.rateLimitsByLimitId && typeof payload.rateLimitsByLimitId === "object") {
      for (const bucket of Object.values(payload.rateLimitsByLimitId)) {
        if (bucket && typeof bucket === "object" && !buckets.includes(bucket)) {
          buckets.push(bucket);
        }
      }
    }
    const windowsByKind = new Map();
    for (const bucket of buckets) {
      for (const rawWindow of [bucket.primary, bucket.secondary]) {
        const window = normalizeAppServerUsageWindow(rawWindow);
        const kind = accountUsageWindowKind(window);
        if (kind && !windowsByKind.has(kind)) windowsByKind.set(kind, window);
      }
    }
    const primary = windowsByKind.get("weekly") || windowsByKind.get("five-hour") || null;
    const secondary = primary === windowsByKind.get("weekly")
      ? windowsByKind.get("five-hour") || null
      : windowsByKind.get("weekly") || null;
    const credits = buckets.find((bucket) => bucket.credits)?.credits || payload.credits || null;
    if (!primary && !secondary && !credits) {
      throw new Error("Codex 官方额度响应中没有可展示的信息");
    }
    const planType = buckets
      .map((bucket) => bucket.planType)
      .find((value) => typeof value === "string" && value.trim())
      || (typeof payload.planType === "string" ? payload.planType : undefined);
    return {
      status: "ok",
      ...(planType ? { planType } : {}),
      ...(primary ? { primary } : {}),
      ...(secondary ? { secondary } : {}),
      ...(credits ? { credits } : {}),
      fetchedAt: Math.floor(Date.now() / 1000),
    };
  };

  const escapeAccountUsageText = (value) => String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");

  const accountUsageWindowLabel = (window) => {
    const kind = accountUsageWindowKind(window);
    if (kind === "weekly") return "周额度";
    if (kind === "five-hour") return "5 小时额度";
    const minutes = Math.max(1, Math.round(Number(window?.windowMinutes) || 0));
    if (minutes % (24 * 60) === 0) return `${minutes / (24 * 60)} 天额度`;
    if (minutes % 60 === 0) return `${minutes / 60} 小时额度`;
    return `${minutes} 分钟额度`;
  };

  const accountUsagePlanLabel = (planType) => {
    const raw = String(planType || "").trim();
    if (!raw) return "";
    const compact = raw.toLowerCase().replace(/[\s_$-]+/g, "");
    if (compact === "5x" || compact.includes("pro5x") || compact.includes("pro100")) {
      return "Pro 5x";
    }
    if (compact === "pro" || compact.includes("pro20x") || compact.includes("pro200")) {
      return "Pro 20x";
    }
    if (compact.includes("plus")) return "Plus";
    if (compact.includes("free")) return "Free";
    return raw
      .replace(/[_-]+/g, " ")
      .replace(/\b\w/g, (character) => character.toUpperCase());
  };

  const accountUsageResetLabel = (resetsAt) => {
    const timestamp = Number(resetsAt);
    if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
    const remainingMinutes = Math.max(
      0,
      Math.ceil((timestamp * 1000 - Date.now()) / 60_000),
    );
    if (remainingMinutes < 60) return `${remainingMinutes} 分钟后重置`;
    if (remainingMinutes < 24 * 60) {
      const hours = Math.floor(remainingMinutes / 60);
      const minutes = remainingMinutes % 60;
      return minutes ? `${hours} 小时 ${minutes} 分钟后重置` : `${hours} 小时后重置`;
    }
    const days = Math.floor(remainingMinutes / (24 * 60));
    const hours = Math.floor((remainingMinutes % (24 * 60)) / 60);
    return hours ? `${days} 天 ${hours} 小时后重置` : `${days} 天后重置`;
  };

  const accountUsageResetTimeLabel = (resetsAt) => {
    const timestamp = Number(resetsAt);
    if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
    const resetAt = new Date(timestamp * 1000);
    if (Number.isNaN(resetAt.getTime())) return "";
    const now = new Date();
    const startOfToday = new Date(
      now.getFullYear(),
      now.getMonth(),
      now.getDate(),
    ).getTime();
    const startOfResetDay = new Date(
      resetAt.getFullYear(),
      resetAt.getMonth(),
      resetAt.getDate(),
    ).getTime();
    const dayOffset = Math.round(
      (startOfResetDay - startOfToday) / (24 * 60 * 60 * 1000),
    );
    const time = resetAt.toLocaleTimeString("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
    if (dayOffset === 0) return `今天 ${time} 重置`;
    if (dayOffset === 1) return `明天 ${time} 重置`;
    return `${resetAt.getMonth() + 1}月${resetAt.getDate()}日 ${time} 重置`;
  };

  const findAccountUsageMount = () => {
    const seenNavigations = new Set();
    for (const anchor of queryWithin(document, sidebarSelector)) {
      const navigation = anchor.matches?.("nav") ? anchor : anchor.closest?.("nav");
      if (!(navigation instanceof HTMLElement) || seenNavigations.has(navigation)) continue;
      seenNavigations.add(navigation);
      if (!visibleMountRect(navigation)) continue;
      const sidebarRoot = navigation.parentElement;
      if (!(sidebarRoot instanceof HTMLElement)) continue;
      const siblings = Array.from(sidebarRoot.children || []);
      const navigationIndex = siblings.indexOf(navigation);
      for (const target of siblings.slice(navigationIndex + 1)) {
        if (!(target instanceof HTMLElement) || target.id === accountUsageId) continue;
        const controls = target.querySelectorAll?.("button, [role=button], a[href]") || [];
        if (!controls.length) continue;
        const before = Array.from(target.children || [])
          .reverse()
          .find((child) => child instanceof HTMLElement && child.id !== accountUsageId);
        if (before) return { target, before };
      }
    }
    return null;
  };

  const mountedAccountUsageIsUsable = (usage) => {
    if (!(usage instanceof HTMLElement) || usage.isConnected !== true) return false;
    const host = usage.parentElement;
    return host instanceof HTMLElement
      && host.getAttribute("data-codey-usage-host") === "true"
      && usage.nextElementSibling === usage.__codeyUsageAnchor;
  };

  const accountUsageWindowSegment = (window, kind, plan = "") => {
    if (!window || accountUsageWindowKind(window) !== kind) return null;
    const remaining = Math.max(0, Math.min(100, 100 - Number(window.usedPercent)));
    const roundedRemaining = Math.round(remaining);
    const label = kind === "weekly" ? "周额度" : "5 小时";
    const ariaLabel = kind === "weekly" ? "周额度" : "5 小时额度";
    const reset = accountUsageResetLabel(window.resetsAt);
    const resetTime = accountUsageResetTimeLabel(window.resetsAt);
    const tone = roundedRemaining <= 20
      ? "critical"
      : roundedRemaining <= 40
        ? "warning"
        : roundedRemaining <= 70
          ? "normal"
          : "healthy";
    return {
      aria: `${ariaLabel}剩余 ${roundedRemaining}%${reset ? `，${reset}` : ""}`,
      html: `
        <span class="codey-usage-segment" data-window="${kind}" data-tone="${tone}" style="--codey-usage-remaining:${remaining / 100}">
          <span class="codey-usage-overview">
            ${plan ? `<span class="codey-usage-plan-tag">${escapeAccountUsageText(plan)}</span>` : ""}
            <span class="codey-usage-window-label">${label}</span>
            <span class="codey-usage-value">${roundedRemaining}%</span>
          </span>
          <span class="codey-usage-meter" aria-hidden="true"><span></span></span>
          ${resetTime ? `<span class="codey-usage-reset">${resetTime}</span>` : ""}
        </span>
      `,
    };
  };

  const accountUsageSegments = (result) => {
    const windowsByKind = new Map();
    for (const window of [result?.primary, result?.secondary]) {
      const kind = accountUsageWindowKind(window);
      if (kind && !windowsByKind.has(kind)) windowsByKind.set(kind, window);
    }
    const visibleKinds = ["weekly", "five-hour"]
      .filter((kind) => windowsByKind.has(kind));
    const plan = accountUsagePlanLabel(result?.planType);
    return visibleKinds.map((kind, index) => accountUsageWindowSegment(
      windowsByKind.get(kind),
      kind,
      index === 0 ? plan : "",
    ));
  };

  const accountUsageDetailWindow = (window) => {
    if (!window || !Number.isFinite(Number(window.usedPercent))) return "";
    const used = Math.max(0, Math.min(100, Number(window.usedPercent)));
    const remaining = Math.max(0, Math.min(100, 100 - used));
    const reset = accountUsageResetTimeLabel(window.resetsAt);
    return `
      <div class="codey-usage-detail-row">
        <div class="codey-usage-detail-main">
          <span class="codey-usage-detail-label">${escapeAccountUsageText(accountUsageWindowLabel(window))}</span>
          <span class="codey-usage-detail-value">剩余 ${Math.round(remaining)}%</span>
        </div>
        <div class="codey-usage-detail-meta">
          <span>已用 ${Math.round(used)}%</span>
          ${reset ? `<span>${escapeAccountUsageText(reset)}</span>` : ""}
        </div>
      </div>
    `;
  };

  const accountUsageCreditsDetail = (credits) => {
    if (!credits) return "";
    const balance = credits.unlimited
      ? "不限"
      : credits.balance !== undefined && credits.balance !== null
        ? String(credits.balance)
        : credits.hasCredits
          ? "可用"
          : "0";
    return `
      <div class="codey-usage-detail-row">
        <div class="codey-usage-detail-main">
          <span class="codey-usage-detail-label">Credits 余额</span>
          <span class="codey-usage-detail-value">${escapeAccountUsageText(balance)}</span>
        </div>
      </div>
    `;
  };

  const accountUsageFetchedLabel = (fetchedAt) => {
    const timestamp = Number(fetchedAt);
    if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
    const fetched = new Date(timestamp * 1000);
    if (Number.isNaN(fetched.getTime())) return "";
    return `更新于 ${fetched.toLocaleTimeString("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    })}`;
  };

  const accountUsageDetailsMarkup = (result) => {
    const rows = [
      accountUsageDetailWindow(result?.primary),
      accountUsageDetailWindow(result?.secondary),
      accountUsageCreditsDetail(result?.credits),
    ].filter(Boolean);
    const fetched = accountUsageFetchedLabel(result?.fetchedAt);
    if (!rows.length && !fetched) return "";
    return `
      <div id="codey-account-usage-details" class="codey-usage-details" role="tooltip">
        <div class="codey-usage-details-heading">
          <span class="codey-usage-details-title">额度详情</span>
        </div>
        <div class="codey-usage-details-list">${rows.join("")}</div>
        ${fetched ? `<div class="codey-usage-details-updated">${escapeAccountUsageText(fetched)}</div>` : ""}
      </div>
    `;
  };

  const removeAccountUsage = () => {
    const usage = document.getElementById(accountUsageId);
    if (usage) {
      usage.__codeyLastUsageHtml = "";
      usage.__codeyUsageAnchor = null;
    }
    usage?.remove?.();
    document.querySelectorAll?.("[data-codey-usage-host]")?.forEach?.((host) => {
      host.removeAttribute?.("data-codey-usage-host");
    });
  };

  const accountUsageMount = () => {
    addStyle();
    let usage = document.getElementById(accountUsageId);
    if (mountedAccountUsageIsUsable(usage)) return usage;
    const mount = findAccountUsageMount();
    if (!mount) {
      usage?.remove?.();
      return null;
    }
    if (!(usage instanceof HTMLElement)) {
      usage = document.createElement("div");
      usage.id = accountUsageId;
      usage.setAttribute("role", "status");
      usage.setAttribute("aria-live", "polite");
      usage.setAttribute("aria-atomic", "true");
      usage.setAttribute("tabindex", "0");
    }
    document.querySelectorAll?.("[data-codey-usage-host]")?.forEach?.((host) => {
      if (host !== mount.target) host.removeAttribute?.("data-codey-usage-host");
    });
    mount.target.setAttribute("data-codey-usage-host", "true");
    if (usage.parentElement !== mount.target || usage.nextElementSibling !== mount.before) {
      mount.target.insertBefore(usage, mount.before);
    }
    usage.__codeyUsageAnchor = mount.before;
    return usage;
  };

  const renderAccountUsage = (result) => {
    if (!result || result.status === "disabled" || result.status === "unavailable") {
      accountUsagePollingEnabled = false;
      window.clearTimeout(accountUsageTimer);
      accountUsageTimer = 0;
      accountUsageLastResult = null;
      removeAccountUsage();
      return;
    }
    accountUsagePollingEnabled = true;
    if (result.status === "error") {
      const usage = accountUsageMount();
      if (!usage) return;
      if (accountUsageLastResult?.status === "ok") {
        usage.dataset.state = "stale";
        usage.title = "官方账号额度暂时无法更新，当前显示上次获取结果";
        return;
      }
      usage.dataset.state = "error";
      usage.setAttribute("aria-label", "官方账号额度暂不可用");
      usage.title = String(result.message || "官方账号额度暂不可用");
      usage.__codeyLastUsageHtml = "";
      usage.textContent = "额度暂不可用";
      return;
    }
    if (result.status !== "ok") return;

    const segments = accountUsageSegments(result);
    accountUsageLastResult = result;
    if (!segments.length) {
      removeAccountUsage();
      return;
    }
    const usage = accountUsageMount();
    if (!usage) return;
    const aria = segments.map((segment) => segment.aria).join("；");
    usage.dataset.state = "ready";
    usage.dataset.windowCount = String(segments.length);
    delete usage.dataset.plan;
    usage.style.setProperty("--codey-usage-window-count", String(segments.length));
    usage.setAttribute("aria-label", aria);
    usage.setAttribute("aria-describedby", "codey-account-usage-details");
    usage.removeAttribute?.("title");
    const nextHtml = `
      <div class="codey-usage-list">
        ${segments.map((segment) => segment.html).join("")}
      </div>
      ${accountUsageDetailsMarkup(result)}
    `;
    // 额度未变化时跳过重建，避免每 60 秒的轮询都触发 DOM 重排和 aria-live
    // 重复播报。
    if (usage.__codeyLastUsageHtml !== nextHtml) {
      usage.__codeyLastUsageHtml = nextHtml;
      usage.innerHTML = nextHtml;
    }
  };

  const scheduleAccountUsageCheck = (delayMs = accountUsageRefreshIntervalMs) => {
    window.clearTimeout(accountUsageTimer);
    accountUsageTimer = 0;
    if (!accountUsagePollingEnabled || document.visibilityState === "hidden") return;
    accountUsageTimer = window.setTimeout(() => {
      accountUsageTimer = 0;
      void checkAccountUsage();
    }, delayMs);
  };

  const readAccountUsageFromAppServer = async () => {
    const loaded = await loadSessionTools();
    if (!loaded || typeof window.__codeyReadAccountRateLimits !== "function") {
      throw new Error("Codex 官方额度读取接口不可用");
    }
    const response = await window.__codeyReadAccountRateLimits();
    return normalizeAppServerAccountUsage(response);
  };

  const checkAccountUsage = async () => {
    if (accountUsageCheckInFlight || document.visibilityState === "hidden") return null;
    accountUsageCheckInFlight = true;
    try {
      let result = await withTimeout(
        callBridge(accountUsagePath, {}, { timeoutMs: accountUsageTimeoutMs }),
        accountUsageTimeoutMs,
        "读取官方账号额度超时",
      );
      if (result?.status === "error") {
        try {
          result = await withTimeout(
            readAccountUsageFromAppServer(),
            accountUsageTimeoutMs,
            "读取 Codex 官方额度超时",
          );
        } catch {
          // Preserve the original backend error. It is normally more actionable
          // when the current Codex asset does not expose AppServerManager yet.
        }
      }
      renderAccountUsage(result);
      return result;
    } catch (error) {
      renderAccountUsage({
        status: "error",
        message: error instanceof Error ? error.message : String(error),
      });
      return null;
    } finally {
      accountUsageCheckInFlight = false;
      if (accountUsagePollingEnabled) scheduleAccountUsageCheck();
    }
  };

  const syncAccountUsageMount = () => {
    if (accountUsageLastResult?.status === "ok") {
      renderAccountUsage(accountUsageLastResult);
    }
  };

  const openSettings = () => {
    if (runtimeHealthState === "unavailable") {
      window.alert(
        "Codey 进程异常或已退出，当前配置面板无法连接。请退出 Codex 后重新启动 Codey。",
      );
      return;
    }
    if (window.__codeySettingsOverlay?.toggle) {
      window.__codeySettingsOverlay.toggle();
      return;
    }
    const detail = String(window.__codeyOverlayError || "").split("\n")[0];
    window.alert(detail
      ? `Codey 内嵌配置面板加载失败：${detail}`
      : "Codey 内嵌配置面板尚未加载，请退出 Codex 后重新启动 Codey");
  };

  const visibleMountRect = (element) => {
    if (!(element instanceof HTMLElement)) return null;
    if (element.closest("[hidden], [aria-hidden=true]")) return null;
    const style = window.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && rect.width > 0
      && rect.height > 0
      ? rect
      : null;
  };

  const isTopChromeMountTarget = (element) => {
    const rect = visibleMountRect(element);
    if (!rect) return false;
    const viewportWidth = Math.max(
      window.innerWidth || 0,
      document.documentElement?.clientWidth || 0,
      document.documentElement?.getBoundingClientRect?.().width || 0,
      rect.right,
    );
    return rect.top <= 96
      && rect.height <= 120
      && rect.width >= 48
      && rect.right >= viewportWidth - 48;
  };

  const findHeaderMount = () => {
    const header = [...document.querySelectorAll("header")].find(isTopChromeMountTarget)
      || [...document.querySelectorAll("nav")].find(isTopChromeMountTarget);
    if (!header) return null;

    const rightmostControl = [...header.querySelectorAll("button, [role=button], a[href]")]
      .reduce((rightmost, control) => {
        if (control.id === buttonId) return rightmost;
        const rect = visibleMountRect(control);
        if (!rect || (rightmost && rect.right <= rightmost.right)) return rightmost;
        return { control, right: rect.right };
      }, null)?.control || null;
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

  const mountedButtonIsUsable = (button) => {
    if (headerMountDirty || !(button instanceof HTMLElement) || button.isConnected !== true) {
      return false;
    }
    const parent = button.parentElement;
    if (!(parent instanceof HTMLElement) || button.closest("[hidden], [aria-hidden=true]")) {
      return false;
    }
    const validParent = parent.matches?.(headerSelector);
    const anchored = button.dataset.codeyHeaderActions !== "true"
      || (
        !!button.nextElementSibling
        && button.nextElementSibling === button.__codeyHeaderAnchor
      );
    return !!validParent && anchored;
  };

  const mountButton = () => {
    addStyle();
    const existingButton = document.getElementById(buttonId);
    if (mountedButtonIsUsable(existingButton)) return;
    const mount = findHeaderMount();
    if (!mount) {
      existingButton?.remove?.();
      return;
    }
    let button = existingButton;
    if (!button) {
      button = document.createElement("button");
      button.id = buttonId;
      button.type = "button";
      button.setAttribute("aria-label", "打开 Codey 配置");
      button.innerHTML = `${settingsIcon}<span class="codey-runtime-badge" aria-hidden="true">!</span><span class="codey-settings-label">Codey</span>`;
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
    button.__codeyHeaderAnchor = mount.before || null;
    applyUpdateBadge(button);
    headerMountDirty = false;
  };

  const finishSessionToolsLoad = () => {
    if (window.__codeySessionToolsInjectLoaded !== true) return false;
    disarmSessionToolsInteraction();
    bootstrapObserver?.disconnect();
    bootstrapObserver = null;
    return true;
  };

  const loadSessionTools = () => {
    if (finishSessionToolsLoad()) return Promise.resolve(true);
    if (sessionToolsLoadPromise) return sessionToolsLoadPromise;
    sessionToolsLoadPromise = Promise.resolve(callBridge(
      sessionToolsLoadPath,
      {},
      { timeoutMs: updateCheckTimeoutMs },
    ))
      .then((result) => {
        if (!result || result.status !== "ok") {
          throw new Error(result?.message || "会话工具加载请求失败");
        }
        if (window.__codeySessionToolsInjectLoaded !== true) {
          throw new Error(window.__codeySessionToolsError || "会话工具未完成初始化");
        }
        return finishSessionToolsLoad();
      })
      .catch((error) => {
        // Runtime.evaluate can time out while the renderer keeps executing the
        // already-started script. If initialization completed before the bridge
        // rejection reached this page, treat it as success and always release
        // the bootstrap observer/listeners.
        if (finishSessionToolsLoad()) return true;
        sessionToolsLoadPromise = null;
        console.warn("[Codey] session tools lazy load failed", error);
        return false;
      });
    return sessionToolsLoadPromise;
  };

  const loadSessionToolsFromInteraction = (event) => {
    const target = event?.target instanceof Element
      ? event.target
      : event?.target?.parentElement;
    if (!target?.closest?.(sidebarSelector)) return;
    void loadSessionTools();
  };

  const armSessionToolsInteraction = () => {
    if (
      typeof document.addEventListener !== "function"
      || sessionToolsInteractionArmed
      || sessionToolsLoadPromise
      || window.__codeySessionToolsInjectLoaded === true
    ) return;
    sessionToolsInteractionArmed = true;
    document.addEventListener("pointerover", loadSessionToolsFromInteraction, {
      capture: true,
      passive: true,
    });
    document.addEventListener("pointerdown", loadSessionToolsFromInteraction, {
      capture: true,
      passive: true,
    });
    document.addEventListener("focusin", loadSessionToolsFromInteraction, true);
  };

  const disarmSessionToolsInteraction = () => {
    if (!sessionToolsInteractionArmed) return;
    sessionToolsInteractionArmed = false;
    document.removeEventListener("pointerover", loadSessionToolsFromInteraction, true);
    document.removeEventListener("pointerdown", loadSessionToolsFromInteraction, true);
    document.removeEventListener("focusin", loadSessionToolsFromInteraction, true);
  };

  const scheduleSessionToolsIdleLoad = () => {
    if (
      sessionToolsIdleLoadScheduled
      || sessionToolsLoadPromise
      || window.__codeySessionToolsInjectLoaded === true
    ) return;
    sessionToolsIdleLoadScheduled = true;
    const run = () => {
      sessionToolsIdleLoadScheduled = false;
      void loadSessionTools();
    };
    if (typeof window.requestIdleCallback === "function") {
      window.requestIdleCallback(run, { timeout: sessionToolsIdleLoadTimeoutMs });
      return;
    }
    window.setTimeout(run, 1_000);
  };

  const scan = (root = document) => {
    mountButton();
    syncAccountUsageMount();
  };

  const scheduleScan = (root = document) => {
    window.clearTimeout(scanTimer);
    scanTimer = window.setTimeout(() => {
      scanTimer = 0;
      scan(root);
    }, 60);
  };

  const invalidateHeaderMount = (root = document) => {
    headerMountDirty = true;
    scheduleScan(root || document);
  };

  if (rendererCoreAlreadyLoaded) return;
  window.addEventListener?.(updateAvailableEvent, (event) => {
    const result = "detail" in event
      ? event.detail
      : window.__codeyUpdateAvailability;
    setUpdateAvailability(result, { dispatch: false });
    if (!hasDetectedUpdate()) scheduleUpdateCheck();
  });
  window.addEventListener?.(configChangedEvent, () => {
    accountUsagePollingEnabled = true;
    scheduleAccountUsageCheck(0);
  });
  // Arm before React mounts the sidebar. The handler itself filters to sidebar
  // targets, so this closes the observer/debounce race without moving the heavy
  // session-tools evaluation into startup.
  armSessionToolsInteraction();
  scheduleSessionToolsIdleLoad();
  scan();
  void hydrateUpdateAvailability();
  void checkRuntimeHealth();
  scheduleAccountUsageCheck(250);

  const headerNodesChanged = (nodes) => {
    for (const node of nodes || []) {
      if (
        node instanceof HTMLElement
        && node.id !== buttonId
        && node.id !== accountUsageId
      ) {
        return true;
      }
    }
    return false;
  };

  const handleBootstrapMutations = (mutations) => {
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (
        target?.id === accountUsageId
        || target?.closest?.(`#${accountUsageId}`)
      ) {
        continue;
      }
      const usageTreeChanged = accountUsageLastResult?.status === "ok"
        && [...(mutation.addedNodes || []), ...(mutation.removedNodes || [])].some((node) => {
          const element = node instanceof HTMLElement ? node : null;
          return element?.id === accountUsageId
            || !!element?.querySelector?.(`#${accountUsageId}`);
        });
      if (
        accountUsageLastResult?.status === "ok"
        && (
          usageTreeChanged
          || target?.getAttribute?.("data-codey-usage-host") === "true"
          || target?.closest?.("[data-codey-usage-host]")
        )
      ) {
        scheduleScan(target || document);
        return;
      }
      if (mutation.type === "attributes") {
        if (target?.matches?.(headerSelector) || target?.matches?.(sidebarSelector)) {
          if (target.matches?.(headerSelector)) headerMountDirty = true;
          scheduleScan(target);
          return;
        }
        continue;
      }
      const targetHeader = target?.matches?.(headerSelector)
        ? target
        : target?.closest?.(headerSelector);
      const headerChildrenChanged = targetHeader && (
        headerNodesChanged(mutation.addedNodes)
        || headerNodesChanged(mutation.removedNodes)
      );
      if (headerChildrenChanged) {
        headerMountDirty = true;
        scheduleScan(targetHeader);
        return;
      }
      for (const node of mutation.addedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) continue;
        // One combined probe rejects the overwhelmingly common streaming case
        // in two subtree walks instead of four.
        const matched = element.matches?.(bootstrapProbeSelector)
          ? element
          : element.querySelector?.(bootstrapProbeSelector);
        if (!matched) continue;
        if (element.matches?.(headerSelector) || element.querySelector?.(headerSelector)) {
          headerMountDirty = true;
        }
        scheduleScan(element);
        return;
      }
    }
  };
  const bootstrapMutationOptions = {
    attributes: true,
    attributeFilter: [
      "data-app-action-sidebar-scroll",
      "data-app-action-sidebar-section",
      "data-app-action-sidebar-thread-id",
      "data-app-action-sidebar-thread-title",
      "data-app-action-sidebar-project-id",
      "data-app-action-sidebar-project-row",
      "hidden",
      "aria-hidden",
    ],
    childList: true,
    subtree: true,
  };
  const mutationDispatcher = window.__codeyMutationDispatcher;
  if (typeof mutationDispatcher?.subscribe === "function") {
    const unsubscribe = mutationDispatcher.subscribe(
      handleBootstrapMutations,
      bootstrapMutationOptions,
    );
    if (mutationDispatcher.snapshot?.().observerInstalled) {
      bootstrapObserver = { disconnect: unsubscribe };
    } else {
      unsubscribe?.();
    }
  }
  if (!bootstrapObserver) {
    bootstrapObserver = new MutationObserver(handleBootstrapMutations);
    bootstrapObserver.observe(document.documentElement, bootstrapMutationOptions);
  }

  window.__codeyLoadSessionTools = loadSessionTools;
  window.__codeyRendererScan = scan;
  window.__codeyRendererInvalidateHeaderMount = invalidateHeaderMount;
  window.__codeyRefreshAccountUsage = checkAccountUsage;
  window.__codeyRefreshRuntimeHealth = checkRuntimeHealth;

  window.addEventListener?.("focus", () => {
    void loadSessionTools();
    scan();
    scheduleRuntimeHealthCheck(0);
    scheduleAccountUsageCheck(0);
  });
  document.addEventListener?.("visibilitychange", () => {
    if (document.visibilityState !== "hidden") void loadSessionTools();
    scheduleRuntimeHealthCheck(0);
    scheduleAccountUsageCheck(0);
  });
  window.addEventListener?.("pageshow", () => {
    void loadSessionTools();
    scan();
    scheduleRuntimeHealthCheck(0);
  });
  // Commit the idempotency marker only after every synchronous bootstrap step
  // succeeded. If an earlier step throws, CDP can inject this module again.
  window.__codeyRendererCoreLoaded = true;
})();
