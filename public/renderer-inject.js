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
  const buttonMeasureId = "codey-settings-button-measure";
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
  // 判定中断后加密复查，看门狗修好桥接后红色状态能尽快解除。
  const runtimeHealthUnavailableRetryMs = 5_000;
  // 超时回调比预算晚到这么久，说明是页面主线程自己卡住，无法据此判断后端。
  const runtimeHealthStallToleranceMs = 1_500;
  // 系统睡眠会同时冻结页面和后端，唤醒瞬间所有进程一起恢复，
  // 3 秒的往返预算不足以判断 Codey 是否真的退出。排程间隔远超定时器
  // 周期即视为刚从睡眠恢复，在这段宽限期内放宽超时与失败阈值。
  const runtimeHealthResumeGapMs = 90_000;
  const runtimeHealthResumeGraceMs = 120_000;
  const runtimeHealthResumeTimeoutMs = 8_000;
  const runtimeHealthResumeFailureThreshold = 4;
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
  // Codex 把左侧图标栏收进 nav[data-app-navigation-rail]，图标栏底部的贴底
  // 堆栈里放着帮助与个人资料按钮，Codey 入口跟随它们落在这里。
  const navigationRailSelector = '[data-app-navigation-rail="true"]';
  const railHelpLabelPattern = /帮助|help/i;
  const railProfileLabelPattern = /个人资料|账户|账号|profile|account/i;
  const bootstrapProbeSelector = `${headerSelector}, ${sidebarSelector}, ${navigationRailSelector}`;
  const settingsIcon = `
    <img class="codey-settings-icon" src="__CODEY_APP_ICON_DATA_URL__" alt="" aria-hidden="true" draggable="false">
  `;
  let sessionToolsLoadPromise = null;
  let scanTimer = 0;
  let updateCheckTimer = 0;
  let updateCheckInFlight = false;
  let codeyUpdatePolicy = null;
  let updateCheckGeneration = 0;
  let runtimeHealthTimer = 0;
  let runtimeHealthCheckPromise = null;
  let runtimeHealthFailures = 0;
  let runtimeHealthState = "checking";
  let runtimeHealthMessage = "";
  let runtimeHealthObservedAt = 0;
  let runtimeHealthLastCheckAt = 0;
  let runtimeHealthResumeGraceUntil = 0;
  let settingsOpenPending = false;
  let accountUsageTimer = 0;
  let accountUsageCheckInFlight = false;
  let accountUsagePollingEnabled = true;
  let accountUsageLastResult = null;
  let sessionToolsInteractionArmed = false;
  let sessionToolsIdleLoadScheduled = false;
  let bootstrapObserver = null;
  let headerMountDirty = true;
  let navigationRailSeen = false;
  const openWithinAttribute = "data-codey-open-within";
  const openStateCssPatchedMarker = "--codey-css-open-state-has-patched";
  const openStateCssPatchedDatasetKey = "data-codey-open-state-patched";
  const openWithinAncestorClasses = [
    "group",
    "group/project-source",
    "has-[[data-state=open]]:opacity-100",
    "has-[[data-state=open]]:pointer-events-auto",
    "has-data-[state=open]:opacity-100",
  ];
  const openWithinButtonAncestorClass = "has-[button[data-state=open]]:opacity-100";
  const openWithinDirectCardClass = "card-ineZLT";
  const openStateCssAriaAndDataStateHasPattern =
    /:has\(\s*\[\s*aria-expanded\s*=\s*(?:"true"|'true'|true)\s*\]\s*,\s*\[\s*data-state\s*=\s*(?:"open"|'open'|open)\s*\]\s*\)/g;
  const openStateCssDataStateHasPattern =
    /:has\(\s*(?::is\(\s*)?(?:>\s*)?(?:button)?\[\s*data-state\s*=\s*(?:"open"|'open'|open)\s*\]\s*\)?\s*\)/g;
  const openStateElements = new Set();
  const syncedOpenContainers = new Set();

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
      /* Codex 原生小按钮：28×28、圆角 12.5px、图标继承按钮前景色，
         Codey 入口按同一套数值对齐，保证与相邻原生按钮同色同尺寸。 */
      #${buttonId} { --codey-icon-size: 16px; --codey-icon-color: currentColor; -webkit-app-region: no-drag !important; pointer-events: auto !important; position: relative; z-index: 2147483641; display: inline-grid; place-items: center; flex: 0 0 auto; width: 32px; height: 32px; border: 0; border-radius: 12.5px; padding: 0; margin-inline-start: 8px; margin-inline-end: 18px; background: transparent; color: inherit; cursor: pointer; opacity: .86; user-select: none; transition: background .15s ease, opacity .15s ease, transform .15s ease; }
      #${buttonId}[data-codey-header-actions="true"] { width: 28px; height: 28px; margin-inline-start: 0; margin-inline-end: 6px; }
      #${buttonId}[data-codey-native-slot="true"], #${buttonMeasureId} { box-sizing: border-box; flex: 0 0 auto; width: 28px; height: 28px; margin: 0; color: inherit; }
      #${buttonId}[data-codey-native-slot="true"] .codey-settings-icon { width: 16px; height: 16px; }
      #${buttonId}[data-codey-rail-slot="true"] { --codey-icon-size: 20px; box-sizing: border-box; flex: 0 0 auto; width: 36px; height: 36px; margin: 0; border-radius: 12.5px; color: color-mix(in srgb, CanvasText 52%, transparent); }
      #${buttonId}[data-codey-rail-slot="true"] .codey-settings-icon { width: 20px; height: 20px; }
      #${buttonId}[data-codey-rail-slot="true"]:hover { color: CanvasText; }
      #${buttonId}[data-codey-rail-slot="true"]::after { top: 4px; right: 4px; }
      #${buttonMeasureId} { display: inline-block; pointer-events: none; }
      #${buttonId}:hover { background: rgba(127, 127, 127, .14); opacity: 1; }
      #${buttonId}:active { transform: translateY(1px); }
      #${buttonId}:focus-visible { outline: 2px solid rgba(139, 151, 255, .72); outline-offset: 2px; }
      #${buttonId} .codey-settings-icon { display: block; flex: 0 0 auto; width: var(--codey-icon-size); height: var(--codey-icon-size); object-fit: contain; }
      #${buttonId} .codey-settings-label { position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0; }
      #${buttonId} .codey-runtime-badge { position: absolute; top: -2px; right: -2px; display: grid; width: 13px; height: 13px; place-items: center; border: 2px solid Canvas; border-radius: 999px; background: #ff453a; color: #fff; font: 800 9px/1 -apple-system, BlinkMacSystemFont, sans-serif; opacity: 0; transform: scale(.65); transition: opacity .15s ease, transform .15s ease; pointer-events: none; }
      #${buttonId}[data-codey-runtime-state="unavailable"] { background: rgba(255, 69, 58, .12); color: #ff453a; opacity: 1; }
      #${buttonId}[data-codey-runtime-state="unavailable"]:hover { background: rgba(255, 69, 58, .2); }
      #${buttonId}[data-codey-runtime-state="unavailable"] .codey-runtime-badge { opacity: 1; transform: scale(1); }
      #${buttonId}::after { content: ""; position: absolute; top: 5px; right: 5px; width: 7px; height: 7px; border-radius: 999px; background: #ff3b30; box-shadow: 0 0 0 2px Canvas; opacity: 0; transform: scale(.7); transition: opacity .15s ease, transform .15s ease; pointer-events: none; }
      #${buttonId}[data-codey-update-available="true"]::after { opacity: 1; transform: scale(1); }
      #${buttonId}[data-codey-header-actions="true"]::after { top: 4px; right: 4px; }
      #${buttonId}[data-codey-runtime-state="unavailable"][data-codey-update-available="true"]::after { top: auto; right: 3px; bottom: 3px; width: 5px; height: 5px; }
      /* 跟随宿主 color-scheme；明确的主题类优先于系统偏好。iOS 液态玻璃质感。 */
      #${accountUsageId} { --codey-usage-ink: light-dark(#1d1d1f, #f5f5f7); --codey-usage-muted: light-dark(#86868b, #98989d); --codey-usage-line: light-dark(rgba(60, 60, 67, .09), rgba(235, 235, 245, .09)); --codey-usage-track: light-dark(rgba(0, 0, 0, .05), rgba(255, 255, 255, .08)); -webkit-app-region: no-drag !important; position: relative; display: block; box-sizing: border-box; width: 100%; min-width: 0; padding: 13px 14px 14px; border: 0; border-radius: 12px; background: light-dark(#ffffff, #222226); box-shadow: 0 1px 5px -1px light-dark(rgba(0, 0, 0, .08), rgba(0, 0, 0, .4)); color: var(--codey-usage-ink); container-type: inline-size; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "SF Pro Display", "PingFang SC", "Helvetica Neue", sans-serif; line-height: 1.4; pointer-events: auto !important; user-select: none; transition: box-shadow .18s ease; }
      :where(.light, [data-theme="light"]) #${accountUsageId} { color-scheme: light; }
      :where(.dark, [data-theme="dark"]) #${accountUsageId} { color-scheme: dark; }
      #${accountUsageId}:hover { box-shadow: 0 3px 14px -3px light-dark(rgba(0, 0, 0, .12), rgba(0, 0, 0, .5)); }
      #${accountUsageId}[data-codey-state="stale"] .codey-usage-details-updated::before { content: "上次成功数据 · "; }
      #${accountUsageId}[data-codey-state="error"] { padding-block: 8px; color: color-mix(in srgb, CanvasText 58%, transparent); font-size: 10px; }
      #${accountUsageId} .codey-usage-list { display: grid; min-width: 0; grid-template-columns: minmax(0, 1fr); }
      #${accountUsageId} .codey-usage-segment, #${accountUsageId} .codey-usage-detail-row { --codey-usage-accent: light-dark(#00a37a, #37d9a1); --codey-usage-accent-bg: light-dark(rgba(0, 163, 122, .14), rgba(55, 217, 161, .22)); min-width: 0; }
      #${accountUsageId} .codey-usage-segment { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; column-gap: 8px; row-gap: 3px; }
      #${accountUsageId}[data-window-count="1"]:has(.codey-usage-segment[data-window="weekly"]) { padding: 15px 17px; border-radius: 16px; box-shadow: 0 3px 9px light-dark(rgba(36, 36, 38, .03), rgba(0, 0, 0, .15)), 0 1px 2px light-dark(rgba(36, 36, 38, .015), rgba(0, 0, 0, .1)); }
      #${accountUsageId}[data-window-count="1"] .codey-usage-segment[data-window="weekly"] { row-gap: 5px; }
      #${accountUsageId}[data-window-count="1"] .codey-usage-segment[data-window="weekly"] .codey-usage-value { text-shadow: none; }
      #${accountUsageId} .codey-usage-border-progress { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; pointer-events: none; }
      #${accountUsageId} .codey-usage-border-progress rect { width: calc(100% - 2px); height: calc(100% - 2px); fill: none; stroke: var(--codey-usage-accent); stroke-width: 1.6px; stroke-linecap: round; filter: drop-shadow(0 0 2px color-mix(in srgb, var(--codey-usage-accent) 42%, transparent)) drop-shadow(0 0 5px color-mix(in srgb, var(--codey-usage-accent) 16%, transparent)); }
      #${accountUsageId} .codey-usage-segment + .codey-usage-segment { margin-top: 9px; padding-top: 9px; border-top: 1px solid var(--codey-usage-line); }
      #${accountUsageId}[data-window-count="2"] .codey-usage-list { grid-template-columns: repeat(2, minmax(0, 1fr)); column-gap: 12px; }
      #${accountUsageId}[data-window-count="2"] .codey-usage-segment + .codey-usage-segment { margin-top: 0; padding-top: 0; border-top: 0; padding-inline-start: 12px; border-inline-start: 1px solid var(--codey-usage-line); }
      #${accountUsageId}[data-window-count="2"] .codey-usage-value { font-size: 24px; }
      #${accountUsageId}[data-window-count="2"] .codey-usage-list:has(.codey-usage-plan-tag) { grid-template-columns: minmax(0, 1.12fr) minmax(0, 1fr); }
      #${accountUsageId} .codey-usage-overview > * { flex-shrink: 0; }
      #${accountUsageId} .codey-usage-overview .codey-usage-window-label { flex-shrink: 1; }
      #${accountUsageId} .codey-usage-overview { display: flex; grid-area: 1 / 1; min-width: 0; min-height: 17px; align-items: center; gap: 5px; }
      #${accountUsageId} .codey-usage-window-label { min-width: 0; overflow: hidden; color: var(--codey-usage-ink); font-size: 12px; font-weight: 600; line-height: 1.3; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-plan-tag { flex: 0 0 auto; overflow: hidden; max-width: 44%; border: 1px solid color-mix(in srgb, #0a84ff 32%, transparent); border-radius: 999px; padding: 1px 6px; background: color-mix(in srgb, #0a84ff 13%, transparent); color: light-dark(#0068cc, #69b1ff); font-size: 9px; font-weight: 600; line-height: 1.35; text-overflow: ellipsis; white-space: nowrap; box-shadow: inset 0 1px 0.5px light-dark(rgba(255, 255, 255, .6), rgba(255, 255, 255, .2)); }
      #${accountUsageId} .codey-usage-plan-tag[data-plan="plus"] { border-color: light-dark(#a4d4c8, #37695e); background: light-dark(#e6f5f1, #193d37); color: light-dark(#0f766e, #91d6c1); }
      #${accountUsageId} .codey-usage-plan-tag[data-pro-tier="100"] { border-color: light-dark(#cbd5e1, #64748b); background: linear-gradient(135deg, light-dark(#f8fafc, #394452), light-dark(#e2e8f0, #262e39)); color: light-dark(#475569, #dbe4ef); box-shadow: inset 0 1px 0.5px rgb(255 255 255 / .35), 0 1px 2px rgb(0 0 0 / .06); }
      #${accountUsageId} .codey-usage-plan-tag[data-pro-tier="200"] { border-color: light-dark(#d6b86b, #8f7035); background: linear-gradient(135deg, light-dark(#fff8e7, #4b3b1f), light-dark(#f2dfb0, #302719)); color: light-dark(#78530e, #f2d58d); box-shadow: inset 0 1px 0.5px rgb(255 255 255 / .4), 0 1px 2px rgb(0 0 0 / .08); }
      #${accountUsageId} .codey-usage-plan-tag[data-pro-tier="500"] { border-color: light-dark(#b89654, #c6a663); background: linear-gradient(135deg, light-dark(#49305f, #59406e), light-dark(#25182f, #302039)); color: light-dark(#f8e5af, #ffe9b0); box-shadow: inset 0 1px 0.5px rgb(255 234 184 / .4), 0 1px 3px rgb(37 28 54 / .2); }
      #${accountUsageId} .codey-usage-value { display: block; grid-area: 1 / 2 / 3 / 3; color: var(--codey-usage-accent); font-size: 30px; font-variant-numeric: tabular-nums; font-weight: 600; letter-spacing: -.03em; line-height: 1; text-shadow: 0 0 12px color-mix(in srgb, var(--codey-usage-accent) 35%, transparent); white-space: nowrap; }
      #${accountUsageId} .codey-usage-unit { margin-inline-start: 1px; font-size: .5em; font-weight: 600; letter-spacing: 0; opacity: .85; }
      #${accountUsageId} .codey-usage-meter { display: block; box-sizing: border-box; width: 100%; height: 3px; min-height: 3px; max-height: 3px; margin-top: 6px; overflow: hidden; border-radius: 999px; background: var(--codey-usage-track); contain: size; line-height: 0; box-shadow: inset 0 0.5px 1px light-dark(rgba(0, 0, 0, .08), rgba(0, 0, 0, .25)); }
      #${accountUsageId} .codey-usage-meter > span { display: block; box-sizing: border-box; width: 100%; height: 100%; border-radius: inherit; background: linear-gradient(90deg, color-mix(in srgb, var(--codey-usage-accent) 80%, white 20%), var(--codey-usage-accent)); box-shadow: 0 0 6px color-mix(in srgb, var(--codey-usage-accent) 45%, transparent), inset 0 0.5px 0 rgba(255, 255, 255, .4); transform: scaleX(var(--codey-usage-remaining)); transform-origin: left center; transition: transform .25s ease-out; }
      #${accountUsageId} .codey-usage-reset { display: block; grid-area: 2 / 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; color: color-mix(in srgb, var(--codey-usage-ink) 62%, transparent); font-size: 11px; font-variant-numeric: tabular-nums; font-weight: 500; line-height: 1.3; white-space: nowrap; }
      #${accountUsageId} [data-tone="normal"] { --codey-usage-accent: light-dark(#3478f6, #72a8ff); --codey-usage-accent-bg: light-dark(rgba(52, 120, 246, .12), rgba(114, 168, 255, .20)); }
      #${accountUsageId} [data-tone="warning"] { --codey-usage-accent: light-dark(#d68100, #ffc05c); --codey-usage-accent-bg: light-dark(rgba(214, 129, 0, .14), rgba(255, 192, 92, .20)); }
      #${accountUsageId} [data-tone="critical"] { --codey-usage-accent: light-dark(#f04452, #ff7d89); --codey-usage-accent-bg: light-dark(rgba(240, 68, 82, .14), rgba(255, 125, 137, .22)); }
      #${accountUsageId} .codey-usage-details { position: absolute; z-index: 2; right: 8px; bottom: calc(100% + 7px); left: 8px; box-sizing: border-box; min-width: 0; padding: 14px 15px 15px; border: 0; border-radius: 14px; background: light-dark(#ffffff, #222226); box-shadow: 0 18px 44px -10px light-dark(rgba(20, 40, 70, .2), rgba(0, 0, 0, .6)), 0 4px 14px -4px light-dark(rgba(20, 40, 70, .1), rgba(0, 0, 0, .4)); color: var(--codey-usage-ink); opacity: 0; pointer-events: none; transform: translateY(6px) scale(.97); transition: opacity .2s cubic-bezier(.16, 1, .3, 1), transform .2s cubic-bezier(.16, 1, .3, 1), visibility .2s cubic-bezier(.16, 1, .3, 1); visibility: hidden; }
      #${accountUsageId}:hover .codey-usage-details, #${accountUsageId}:focus-visible .codey-usage-details { opacity: 1; transform: translateY(0) scale(1); visibility: visible; pointer-events: auto; }
      #${accountUsageId} .codey-usage-details::after { content: ""; position: absolute; inset: 100% 0 auto; height: 9px; }
      #${accountUsageId}:focus-visible { outline: 2px solid color-mix(in srgb, #0a84ff 72%, transparent); outline-offset: -2px; }
      #${accountUsageId} .codey-usage-details-heading { position: relative; z-index: 1; display: flex; min-width: 0; align-items: center; justify-content: space-between; gap: 8px; padding-bottom: 11px; }
      #${accountUsageId} .codey-usage-details-title { font-size: 12px; font-weight: 650; letter-spacing: -.01em; }
      #${accountUsageId} .codey-usage-details-updated { margin-inline-start: auto; color: var(--codey-usage-muted); font-size: 9.5px; font-variant-numeric: tabular-nums; white-space: nowrap; }
      #${accountUsageId} .codey-usage-details-list { position: relative; z-index: 1; display: grid; min-width: 0; gap: 12px; }
      #${accountUsageId} .codey-usage-detail-row { position: relative; padding-block-start: 11px; border-block-start: 1px solid var(--codey-usage-line); }
      #${accountUsageId} .codey-usage-detail-main, #${accountUsageId} .codey-usage-detail-meta { display: flex; min-width: 0; align-items: center; justify-content: space-between; gap: 8px; }
      #${accountUsageId} .codey-usage-detail-label { overflow: hidden; font-size: 11.5px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-value { margin-inline-start: auto; font-size: 13px; font-variant-numeric: tabular-nums; font-weight: 650; white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-row[data-tone] .codey-usage-detail-value { padding: 1.5px 8px; border-radius: 999px; border: 1px solid color-mix(in srgb, var(--codey-usage-accent) 30%, transparent); background: linear-gradient(180deg, light-dark(rgba(255, 255, 255, .35), rgba(255, 255, 255, .12)) 0%, transparent 100%), var(--codey-usage-accent-bg); color: var(--codey-usage-accent); font-size: 10.5px; font-weight: 650; box-shadow: inset 0 1px 0.5px light-dark(rgba(255, 255, 255, .7), rgba(255, 255, 255, .25)), 0 1px 3px light-dark(rgba(0, 0, 0, .05), rgba(0, 0, 0, .2)); }
      #${accountUsageId} .codey-usage-detail-row .codey-usage-meter { height: 5px; min-height: 5px; max-height: 5px; border-radius: 999px; background: var(--codey-usage-track); box-shadow: inset 0 1px 2px light-dark(rgba(0, 0, 0, .08), rgba(0, 0, 0, .3)); }
      #${accountUsageId} .codey-usage-detail-reset { flex: 0 1 auto; margin-inline-start: auto; overflow: hidden; color: color-mix(in srgb, var(--codey-usage-ink) 78%, transparent); font-size: 11px; font-variant-numeric: tabular-nums; font-weight: 550; text-overflow: ellipsis; white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-gauge { display: flex; min-width: 0; align-items: center; gap: 9px; margin-top: 8px; }
      #${accountUsageId} .codey-usage-detail-gauge .codey-usage-meter { flex: 1 1 auto; margin-top: 0; }
      #${accountUsageId} .codey-usage-detail-percent { flex: 0 0 auto; color: var(--codey-usage-accent); font-size: 16px; font-variant-numeric: tabular-nums; font-weight: 700; line-height: 1.1; text-shadow: 0 0 12px color-mix(in srgb, var(--codey-usage-accent) 35%, transparent); white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-meta { flex-wrap: wrap; margin-top: 6px; gap: 3px 8px; color: color-mix(in srgb, var(--codey-usage-ink) 66%, transparent); font-size: 10.5px; font-variant-numeric: tabular-nums; line-height: 1.35; }
      #${accountUsageId} .codey-usage-detail-meta span { white-space: nowrap; }
      #${accountUsageId} .codey-usage-detail-meta span + span { margin-inline-start: auto; }
      #${accountUsageId} .codey-usage-detail-meta .is-expiring { color: var(--codey-usage-accent); font-weight: 600; }
      #${accountUsageId} .codey-usage-credit-dot { flex: 0 0 auto; width: 6px; height: 6px; margin-inline-start: -2px; border-radius: 999px; background: currentColor; color: light-dark(#d70015, #ff453a); animation: codey-usage-breathe 1.6s ease-in-out infinite; }
      @keyframes codey-usage-breathe { 0%, 100% { opacity: .3; transform: scale(.72); box-shadow: 0 0 0 0 rgba(255, 59, 48, .5); } 50% { opacity: 1; transform: scale(1); box-shadow: 0 0 0 5px rgba(255, 59, 48, .06); } }
      @container (max-width: 300px) {
        #${accountUsageId}[data-window-count="2"] .codey-usage-list { column-gap: 10px; }
        #${accountUsageId}[data-window-count="2"] .codey-usage-segment { column-gap: 4px; }
        #${accountUsageId}[data-window-count="2"] .codey-usage-segment + .codey-usage-segment { padding-inline-start: 10px; }
        #${accountUsageId}[data-window-count="2"] .codey-usage-value { font-size: 20px; }
        #${accountUsageId}[data-window-count="2"] .codey-usage-reset { font-size: 10.5px; }
      }
      @media (prefers-reduced-motion: reduce) {
        #${buttonId}, #${buttonId} *, #${accountUsageId}, #${accountUsageId} * { animation: none !important; transition: none !important; }
        /* 预警灯保留仅透明度的慢速呼吸，不带位移和光环。 */
        #${accountUsageId} .codey-usage-credit-dot { animation: codey-usage-breathe 3.2s ease-in-out infinite !important; transform: none !important; box-shadow: none !important; }
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
        `Codey 连接中断，点击查看处理提示${updateLabel}`,
      );
      button.title = `Codey 连接中断：${detail}（点击查看处理提示）${updateLabel}`;
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
    window.__codeyUpdateAvailability = codeyUpdatePolicy !== "off" && (codeyUpdatePolicy !== "stable" || !result?.latestVersion?.includes("-")) && result?.updateAvailable === true
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
    // 并发调用共用同一次检查：点击入口要等到这次的结论，不能拿旧快照。
    runtimeHealthCheckPromise ||= runRuntimeHealthCheck().finally(() => {
      runtimeHealthCheckPromise = null;
    });
    return runtimeHealthCheckPromise;
  };

  const runRuntimeHealthCheck = async () => {
    let inconclusive = false;
    const observedAt = Date.now();
    const observedGap = runtimeHealthLastCheckAt > 0
      ? observedAt - runtimeHealthLastCheckAt
      : 0;
    runtimeHealthLastCheckAt = observedAt;
    if (observedGap >= runtimeHealthResumeGapMs) {
      // 页面和后端被系统一起冻结，唤醒后 Codey 需要时间重新建立与
      // Codex 的连接。恢复前记录的失败不代表当前状态，直接丢弃。
      runtimeHealthResumeGraceUntil = observedAt + runtimeHealthResumeGraceMs;
      runtimeHealthFailures = 0;
    }
    const resuming = observedAt < runtimeHealthResumeGraceUntil;
    const checkTimeoutMs = resuming
      ? runtimeHealthResumeTimeoutMs
      : runtimeHealthCheckTimeoutMs;
    const failureThreshold = resuming
      ? runtimeHealthResumeFailureThreshold
      : runtimeHealthFailureThreshold;
    try {
      if (typeof window.__codexSessionDeleteBridge !== "function") {
        runtimeHealthFailures = failureThreshold;
        return setRuntimeHealthState("unavailable", "Codey bridge 不可用");
      }
      const result = await withTimeout(
        callBridge(backendHealthPath, {}, { timeoutMs: checkTimeoutMs }),
        checkTimeoutMs + 250,
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
      const immediate = error?.code === "bridge_unavailable";
      // 看门狗重建桥接时，上一代未完成的调用以 bridge_reinstalled 结束，
      // 说明后端正在恢复；页面卡顿时超时同样不反映后端状态。两者都不计失败。
      inconclusive = !immediate && (
        error?.code === "bridge_reinstalled"
        || Date.now() - observedAt > checkTimeoutMs + runtimeHealthStallToleranceMs
      );
      if (inconclusive) return runtimeHealthSnapshot();
      runtimeHealthFailures += 1;
      if (immediate) runtimeHealthFailures = failureThreshold;
      if (runtimeHealthFailures >= failureThreshold) {
        return setRuntimeHealthState("unavailable", "Codey 后端未响应");
      }
      return setRuntimeHealthState("checking", "正在确认 Codey 进程状态");
    } finally {
      let nextDelay = runtimeHealthCheckIntervalMs;
      if (runtimeHealthState === "unavailable") nextDelay = runtimeHealthUnavailableRetryMs;
      if (inconclusive || (runtimeHealthFailures > 0 && runtimeHealthFailures < failureThreshold)) {
        nextDelay = runtimeHealthFailureRetryMs;
      }
      scheduleRuntimeHealthCheck(nextDelay);
    }
  };

  const setAutomaticUpdateChecks = (policy) => {
    if (codeyUpdatePolicy === policy) return;
    const previous = codeyUpdatePolicy;
    codeyUpdatePolicy = policy;
    if (previous !== null) setUpdateAvailability(null);
    updateCheckGeneration += 1;
    window.clearTimeout(updateCheckTimer);
    updateCheckTimer = 0;
  };

  const scheduleUpdateCheck = (delayMs = updateCheckIntervalMs) => {
    window.clearTimeout(updateCheckTimer);
    updateCheckTimer = 0;
    if (codeyUpdatePolicy === "off" || hasDetectedUpdate()) return;
    updateCheckTimer = window.setTimeout(() => {
      updateCheckTimer = 0;
      // 尚未读取到设置时先重试状态查询，避免关闭后仍发起更新请求。
      if (codeyUpdatePolicy === null) void hydrateUpdateAvailability();
      else void checkForUpdatesSilently();
    }, delayMs);
  };

  const checkForUpdatesSilently = async () => {
    if (!["stable", "experimental"].includes(codeyUpdatePolicy) || updateCheckInFlight || hasDetectedUpdate()) return;
    updateCheckInFlight = true;
    const generation = updateCheckGeneration;
    try {
      const result = await withTimeout(
        callBridge(updateCheckPath, {}, { timeoutMs: updateCheckTimeoutMs }),
        updateCheckTimeoutMs,
      );
      if (!["stable", "experimental"].includes(codeyUpdatePolicy) || generation !== updateCheckGeneration) return;
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
    const generation = updateCheckGeneration;
    try {
      const status = await withTimeout(
        callBridge(backendStatusPath, {}, { timeoutMs: updateCheckTimeoutMs }),
        updateCheckTimeoutMs,
        "读取更新状态超时",
      );
      if (generation !== updateCheckGeneration || !status || status.status === "failed") return;
      setAutomaticUpdateChecks(status.codeyUpdatePolicy ?? (status.autoCheckCodeyUpdates === false ? "off" : "stable"));
      setUpdateAvailability(status?.availableUpdate || null);
    } catch {
      if (generation === updateCheckGeneration) setUpdateAvailability(null);
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
    if (typeof window?.usedPercent !== "number" || typeof window?.windowDurationMins !== "number") return null;
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
    const unavailable = { status: "unavailable", code: "codex_usage_incompatible", message: "当前 Codex 暂时无法提供额度信息" };
    const payload = response?.result && typeof response.result === "object"
      ? response.result
      : response;
    if (!payload || typeof payload !== "object") {
      return unavailable;
    }
    // 账号摘要只读取通用额度，不能合并模型专属的 rateLimitsByLimitId。
    const buckets = [];
    if (payload.rateLimits && typeof payload.rateLimits === "object") {
      buckets.push(payload.rateLimits);
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
    const rawCredits = buckets.find((bucket) => bucket.credits)?.credits || payload.credits || null;
    const credits = rawCredits && typeof rawCredits === "object"
      && (rawCredits.unlimited === true || (rawCredits.hasCredits === true
        && (typeof rawCredits.balance === "number"
          || (typeof rawCredits.balance === "string" && rawCredits.balance.trim() !== ""))
        && Number.isFinite(Number(rawCredits.balance))))
      ? rawCredits : null;
    if (!primary && !secondary && !credits) {
      return unavailable;
    }
    const planType = buckets
      .map((bucket) => bucket.planType)
      .find((value) => typeof value === "string" && value.trim())
      || (typeof payload.planType === "string" ? payload.planType : undefined);
    const resetCredits = normalizeResetCreditsSummary(
      payload.rateLimitResetCredits ?? payload.rate_limit_reset_credits
        ?? payload.rateLimits?.rateLimitResetCredits ?? payload.rateLimits?.rate_limit_reset_credits,
    );
    return {
      status: "ok",
      ...(planType ? { planType } : {}),
      ...(primary ? { primary } : {}),
      ...(secondary ? { secondary } : {}),
      ...(credits ? { credits } : {}),
      ...(resetCredits ? { resetCredits } : {}),
      fetchedAt: Math.floor(Date.now() / 1000),
    };
  };

  // AppServer 的 rateLimitResetCredits 带剩余次数与明细行；明细行取最近一次
  // 过期时间，明细缺失时只保留次数。
  const normalizeResetCreditsSummary = (summary) => {
    if (!summary || typeof summary !== "object") return null;
    const count = Number(summary.availableCount ?? summary.available_count);
    if (!Number.isFinite(count)) return null;
    let nextExpiresAt;
    const rows = Array.isArray(summary.credits) ? summary.credits : [];
    for (const row of rows) {
      const status = String(row?.status || "");
      if (status === "redeemed" || status === "unknown") continue;
      const raw = Number(row?.expiresAt ?? row?.expires_at);
      if (!Number.isFinite(raw) || raw <= 0) continue;
      const expires = Math.round(raw > 10_000_000_000 ? raw / 1000 : raw);
      if (expires * 1000 <= Date.now()) continue;
      nextExpiresAt = nextExpiresAt === undefined ? expires : Math.min(nextExpiresAt, expires);
    }
    return {
      availableCount: Math.max(0, Math.round(count)),
      ...(nextExpiresAt ? { nextExpiresAt } : {}),
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
    const normalized = raw.toLowerCase();
    if (normalized === "prolite") return "Pro 100";
    if (normalized === "pro") return "Pro 200";
    if (normalized === "promax") return "Pro 500";
    const compact = normalized.replace(/[\s_$-]+/g, "");
    if (compact === "pro100") return "Pro 100";
    if (compact === "pro200") return "Pro 200";
    if (compact === "pro500") return "Pro 500";
    if (compact === "5x" || compact === "pro5x") return "Pro 5x";
    if (compact === "pro20x") return "Pro 20x";
    if (normalized === "plus") return "Plus";
    if (normalized === "free") return "Free";
    return raw;
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

  const accountUsageDayTimeLabel = (resetsAt, calendarDate = false) => {
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
    if (!calendarDate && dayOffset === 0) return `今天 ${time}`;
    if (!calendarDate && dayOffset === 1) return `明天 ${time}`;
    return `${resetAt.getMonth() + 1}月${resetAt.getDate()}日 ${time}`;
  };

  const accountUsageResetTimeLabel = (resetsAt) => {
    const label = accountUsageDayTimeLabel(resetsAt);
    return label ? `${label} 重置` : "";
  };

  // Codex 把侧边栏底部内容收进贴底的绝对定位 footer 容器后，容器内容由 Codex
  // 自己渲染，可能暂时为空，所以不能再要求它一定含控件。
  const sidebarFooterSlot = (element) => {
    const style = window.getComputedStyle(element);
    if (!style || style.display === "none" || style.visibility === "hidden") return false;
    return style.position === "absolute"
      && style.bottom === "0px"
      && element.getBoundingClientRect().width > 0;
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
      const trailing = siblings
        .slice(navigationIndex + 1)
        .filter((target) => target instanceof HTMLElement && target.id !== accountUsageId);
      // 老布局的 footer 自带账号控件，新布局只剩 Codex 的贴底容器；两者都接受，
      // 一律插到容器内最后一个子元素之前，让 Codex 的测量继续覆盖额度块。
      const target = trailing.find((candidate) => (candidate.querySelectorAll?.("button, [role=button], a[href]")?.length || 0) > 0)
        || trailing.find(sidebarFooterSlot);
      if (!target) continue;
      const before = Array.from(target.children || [])
        .reverse()
        .find((child) => child instanceof HTMLElement && child.id !== accountUsageId) || null;
      return { target, before };
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

  const accountUsageTone = (remaining) => remaining <= 20
    ? "critical"
    : remaining <= 40 ? "warning" : remaining <= 70 ? "normal" : "healthy";

  const accountUsageWindowSegment = (window, kind, plan = "", creditDot = false, borderProgress = false) => {
    if (!window || accountUsageWindowKind(window) !== kind) return null;
    const proTier = /^Pro (100|200|500)$/.exec(plan)?.[1];
    const remaining = Math.max(0, Math.min(100, 100 - Number(window.usedPercent)));
    const roundedRemaining = Math.round(remaining);
    const ariaLabel = kind === "weekly" ? "周额度" : "5 小时额度";
    const label = kind === "weekly" ? "每周" : "5 小时";
    const reset = accountUsageResetLabel(window.resetsAt);
    const dayTime = accountUsageDayTimeLabel(window.resetsAt, borderProgress);
    const resetTime = borderProgress
      ? dayTime && `${dayTime} 重置`
      : dayTime.replace(/^(\d+)月(\d+)日/, "$1/$2");
    const tone = accountUsageTone(roundedRemaining);
    return {
      aria: `${ariaLabel}剩余 ${roundedRemaining}%${reset ? `，${reset}` : ""}`,
      html: `
        <span class="codey-usage-segment" data-window="${kind}" data-tone="${tone}">
          <span class="codey-usage-overview">
            ${creditDot ? `<span class="codey-usage-credit-dot" aria-hidden="true"></span>` : ""}
            ${plan ? `<span class="codey-usage-plan-tag"${plan === "Plus" ? ' data-plan="plus"' : ""}${proTier ? ` data-pro-tier="${proTier}"` : ""}>${escapeAccountUsageText(plan)}</span>` : ""}
            <span class="codey-usage-window-label">${label}</span>
          </span>
          <span class="codey-usage-value">${roundedRemaining}<span class="codey-usage-unit">%</span></span>
          ${resetTime ? `<span class="codey-usage-reset">${resetTime}</span>` : ""}
          ${borderProgress ? `<svg class="codey-usage-border-progress" aria-hidden="true" focusable="false"${remaining === 0 ? ' opacity="0"' : ""}><rect x="1" y="1" rx="15" pathLength="100" stroke-dasharray="${remaining} 100" /></svg>` : ""}
        </span>
      `,
    };
  };

  const accountUsageSegments = (result, creditExpiring = false) => {
    const windowsByKind = new Map();
    for (const window of [result?.primary, result?.secondary]) {
      const kind = accountUsageWindowKind(window);
      if (kind && !windowsByKind.has(kind)) windowsByKind.set(kind, window);
    }
    const plan = accountUsagePlanLabel(result?.planType);
    return ["weekly", "five-hour"]
      .filter((kind) => windowsByKind.has(kind))
      .map((kind, index) => accountUsageWindowSegment(
        windowsByKind.get(kind),
        kind,
        index === 0 ? plan : "",
        index === 0 && creditExpiring,
        windowsByKind.size === 1 && kind === "weekly",
      ));
  };

  const accountUsageDetailWindow = (window) => {
    if (!window || !Number.isFinite(Number(window.usedPercent))) return "";
    const used = Math.max(0, Math.min(100, Number(window.usedPercent)));
    const remaining = Math.max(0, Math.min(100, 100 - used));
    const reset = accountUsageResetTimeLabel(window.resetsAt);
    return `
      <div class="codey-usage-detail-row" data-tone="${accountUsageTone(Math.round(remaining))}" style="--codey-usage-remaining:${remaining / 100}">
        <div class="codey-usage-detail-main">
          <span class="codey-usage-detail-label">${escapeAccountUsageText(accountUsageWindowLabel(window))}</span>
          ${reset ? `<span class="codey-usage-detail-reset">${escapeAccountUsageText(reset)}</span>` : ""}
        </div>
        <div class="codey-usage-detail-gauge">
          <span class="codey-usage-meter" aria-hidden="true"><span></span></span>
          <span class="codey-usage-detail-percent">${Math.round(remaining)}%</span>
        </div>
      </div>
    `;
  };

  const accountUsageCreditsDetail = (credits) => {
    if (!credits) return "";
    const numeric = Number(credits.balance);
    const explicitBalance = credits.balance !== undefined && credits.balance !== null
      && credits.balance !== "" && Number.isFinite(numeric);
    // 余额为零时不占行：显式 0 直接隐藏；无余额字段时按 hasCredits 兜底。
    if (!credits.unlimited && explicitBalance && numeric <= 0) return "";
    if (!credits.unlimited && !explicitBalance && !credits.hasCredits) return "";
    const balance = credits.unlimited
      ? "不限"
      : explicitBalance
        ? String(credits.balance).replace(/(\.\d*?)0+$/, "$1").replace(/\.$/, "")
        : "可用";
    return `
      <div class="codey-usage-detail-row">
        <div class="codey-usage-detail-main">
          <span class="codey-usage-detail-label">Credits 余额</span>
          <span class="codey-usage-detail-value">${escapeAccountUsageText(balance)}</span>
        </div>
      </div>
    `;
  };

  // 充值卡在最近一次过期 18 小时内进入预警：详情行转红色并加呼吸灯，
  // 侧栏摘要也亮一颗红点。
  const accountUsageResetExpiring = (resetCredits) => {
    if (!(Number(resetCredits?.availableCount) > 0)) return false;
    const expiresAt = Number(resetCredits?.nextExpiresAt) * 1000;
    const now = Date.now();
    return Number.isFinite(expiresAt) && expiresAt > now && expiresAt - now <= 18 * 60 * 60 * 1000;
  };

  const accountUsageResetCreditsDetail = (resetCredits) => {
    const count = Number(resetCredits?.availableCount);
    if (!Number.isFinite(count) || count <= 0) return "";
    const expiry = accountUsageDayTimeLabel(resetCredits.nextExpiresAt);
    const expiring = accountUsageResetExpiring(resetCredits);
    return `
      <div class="codey-usage-detail-row"${expiring ? ' data-tone="critical"' : ""}>
        <div class="codey-usage-detail-main">
          <span class="codey-usage-detail-label">剩余重置次数</span>
          ${expiring ? `<span class="codey-usage-credit-dot" aria-hidden="true"></span>` : ""}
          <span class="codey-usage-detail-value">${Math.max(0, Math.round(count))} 次</span>
        </div>
        ${expiry ? `<div class="codey-usage-detail-meta"><span${expiring ? ' class="is-expiring"' : ""}>最近 ${escapeAccountUsageText(expiry)} 过期</span></div>` : ""}
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
      accountUsageResetCreditsDetail(result?.resetCredits),
    ].filter(Boolean);
    const fetched = accountUsageFetchedLabel(result?.fetchedAt);
    if (!rows.length && !fetched) return "";
    return `
      <div id="codey-account-usage-details" class="codey-usage-details" role="tooltip">
        <div class="codey-usage-details-heading">
          <span class="codey-usage-details-title">额度详情</span>
          ${fetched ? `<span class="codey-usage-details-updated">${escapeAccountUsageText(fetched)}</span>` : ""}
        </div>
        <div class="codey-usage-details-list">${rows.join("")}</div>
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
        usage.dataset.codeyState = "stale";
        usage.title = "官方账号额度暂时无法更新，当前显示上次获取结果";
        return;
      }
      usage.dataset.codeyState = "error";
      delete usage.dataset.creditExpiring;
      usage.setAttribute("aria-label", "官方账号额度暂不可用");
      usage.title = String(result.message || "官方账号额度暂不可用");
      usage.__codeyLastUsageHtml = "";
      usage.textContent = "额度暂不可用";
      return;
    }
    if (result.status !== "ok") return;

    const creditExpiring = accountUsageResetExpiring(result?.resetCredits);
    const segments = accountUsageSegments(result, creditExpiring);
    accountUsageLastResult = result;
    if (!segments.length) {
      removeAccountUsage();
      return;
    }
    const usage = accountUsageMount();
    if (!usage) return;
    const aria = segments.map((segment) => segment.aria).join("；")
      + (creditExpiring ? "；重置次数即将过期" : "");
    usage.dataset.codeyState = "ready";
    usage.dataset.windowCount = String(segments.length);
    if (creditExpiring) usage.dataset.creditExpiring = "true";
    else delete usage.dataset.creditExpiring;
    delete usage.dataset.plan;
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

  const readAccountUsageFromAppServer = async (backendResult) => {
    const loaded = await loadSessionTools();
    if (!loaded || typeof window.__codeyReadAccountRateLimits !== "function") {
      return { status: "unavailable", code: "codex_usage_incompatible", message: "当前 Codex 暂时无法提供额度信息" };
    }
    const response = await window.__codeyReadAccountRateLimits();
    const snapshot = normalizeAppServerAccountUsage(response);
    if (snapshot.status !== "ok") return snapshot;
    // 同步 Codex 管理的账号额度，让官方账号面板可以复用当前快照。
    if (Number.isSafeInteger(backendResult?.authGeneration)) {
      const stored = await callBridge("/api/store_official_account_usage", {
        authGeneration: backendResult.authGeneration, snapshot,
      }, { timeoutMs: accountUsageTimeoutMs });
      if (stored?.status !== "ok") throw new Error(stored?.message || "同步官方额度失败");
      snapshot.authGeneration = backendResult.authGeneration;
    }
    return snapshot;
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
      if (result?.status === "error" || result?.stale) {
        try {
          const fallback = await withTimeout(
            readAccountUsageFromAppServer(result),
            accountUsageTimeoutMs,
            "读取 Codex 官方额度超时",
          );
          if (fallback?.status === "ok") result = fallback;
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

  const openSettings = async () => {
    if (settingsOpenPending) return;
    if (runtimeHealthState === "unavailable") {
      // 红色状态可能来自页面卡顿的误判，或桥接已恢复而下一轮检查未到；
      // 当场复查一次，仍连不上才提示。
      settingsOpenPending = true;
      let health;
      try {
        health = await checkRuntimeHealth();
      } finally {
        settingsOpenPending = false;
      }
      if (health.state === "healthy") {
        openSettingsOverlay();
        return;
      }
      window.alert(
        "Codey 与 Codex 的连接已中断，当前配置面板无法连接。"
        + "Codey 会继续尝试自动恢复；若长时间仍未恢复，请退出 Codex 后重新启动 Codey。",
      );
      return;
    }
    openSettingsOverlay();
  };

  // 配置面板按需加载；悬停停留或按下、聚焦时提前加载，点击时直接打开。
  // 掠过按钮不加载，失败也不提示，错误与重试留给真正的点击。
  const settingsPreloadDelayMs = 120;
  let settingsPreloadTimer = 0;
  const preloadSettingsOverlay = () => {
    window.clearTimeout(settingsPreloadTimer);
    settingsPreloadTimer = 0;
    const overlay = window.__codeySettingsOverlay;
    if (runtimeHealthState === "unavailable"
      || !overlay?.__codeyLazyLoader
      || typeof overlay.load !== "function") return;
    void Promise.resolve().then(() => overlay.load()).catch(() => {});
  };
  const scheduleSettingsPreload = () => {
    window.clearTimeout(settingsPreloadTimer);
    settingsPreloadTimer = window.setTimeout(preloadSettingsOverlay, settingsPreloadDelayMs);
  };
  const cancelSettingsPreload = () => {
    window.clearTimeout(settingsPreloadTimer);
    settingsPreloadTimer = 0;
  };

  const openSettingsOverlay = () => {
    if (window.__codeySettingsOverlay?.toggle) {
      window.__codeySettingsOverlay.toggle();
      return;
    }
    const detail = String(window.__codeyOverlayError || "").split("\n")[0];
    window.alert(detail
      ? `Codey 内嵌配置面板加载失败：${detail}`
      : "Codey 内嵌配置面板尚未加载，请退出 Codex 后重新启动 Codey");
  };

  const mountableRect = (element) => {
    if (!(element instanceof HTMLElement)) return null;
    const style = window.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && rect.width > 0
      && rect.height > 0
      ? rect
      : null;
  };

  const visibleMountRect = (element) =>
    element instanceof HTMLElement && element.closest("[hidden], [aria-hidden=true]")
      ? null
      : mountableRect(element);

  // Codex 打开模态对话框时只给图标栏等背景内容补 aria-hidden，布局不变；
  // 把它当成隐藏会让对话框弹出期间的扫描把入口当成失效挂载卸载。
  const railMountRect = (element) =>
    element instanceof HTMLElement && element.closest("[hidden]")
      ? null
      : mountableRect(element);

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

    // Newer shells measure an invisible copy of each action row. Participate
    // in that measurement so panel headers reserve the complete action width.
    const slot = rightmostControl.closest('[data-test-id="header-shell-slot"]');
    if (slot?.parentElement === header) {
      const measureHost = [...slot.children].find((child) => child.getAttribute("aria-hidden") === "true");
      const visibleHost = [...slot.children].find((child) => child !== measureHost && child.contains(rightmostControl));
      const measureRow = measureHost?.firstElementChild;
      const actionRow = visibleHost?.firstElementChild;
      if (measureRow && actionRow && visibleMountRect(actionRow)) {
        return { header, target: actionRow, measureRow, nativeSlot: true };
      }
      // Some shells measure the live content directly, without a hidden copy.
      // There the slot holds one measured host, and the action row sits inside it.
      const liveHost = slot.children.length === 1 ? slot.firstElementChild : null;
      const liveRowCandidate = liveHost?.firstElementChild;
      const liveRow = liveRowCandidate
        && liveRowCandidate !== rightmostControl
        && liveRowCandidate.contains(rightmostControl)
        ? liveRowCandidate
        : liveHost;
      if (!measureHost && liveRow?.contains(rightmostControl) && visibleMountRect(liveRow)) {
        return { header, target: liveRow, nativeSlot: true };
      }
    }

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

  // 图标栏底部的贴底堆栈：nav 的最后一个可见子容器 → 它的 flex 列容器，
  // 帮助菜单可能不再显示，此时将 Codey 入口放在个人资料菜单之前。
  const findRailFooterMount = () => {
    const rail = document.querySelector(navigationRailSelector);
    if (!(rail instanceof HTMLElement)) return null;
    navigationRailSeen = true;
    if (!railMountRect(rail)) return null;
    const cluster = [...rail.children]
      .filter((child) => railMountRect(child))
      .at(-1);
    const stack = cluster?.firstElementChild;
    if (!(stack instanceof HTMLElement)) return null;
    const controls = [...stack.querySelectorAll("button")]
      .filter((candidate) => candidate.id !== buttonId && railMountRect(candidate));
    const anchor = controls.find((candidate) => railHelpLabelPattern.test(candidate.getAttribute("aria-label") || ""))
      || controls.find((candidate) => railProfileLabelPattern.test(candidate.getAttribute("aria-label") || ""));
    if (!(anchor instanceof HTMLElement)) return null;
    // 菜单按钮外面可能还包着 radix 的触发层，只取 stack 的直接子容器作锚点。
    const before = [...stack.children]
      .find((child) => child instanceof HTMLElement && child.contains(anchor));
    if (!(before instanceof HTMLElement)) return null;
    return { target: stack, before };
  };

  const mountedButtonIsUsable = (button) => {
    if (headerMountDirty || !(button instanceof HTMLElement) || button.isConnected !== true) {
      return false;
    }
    const parent = button.parentElement;
    if (!(parent instanceof HTMLElement)) {
      return false;
    }
    if (button.dataset.codeyRailSlot === "true") {
      // 模态对话框给背景补的 aria-hidden 不算真实隐藏，与 railMountRect 一致。
      if (button.closest("[hidden]")) return false;
      const rail = button.closest?.(navigationRailSelector);
      return !!rail
        && rail.isConnected !== false
        && parent === button.__codeyRailStack
        && button.nextElementSibling === button.__codeyRailAnchor;
    }
    if (button.closest("[hidden], [aria-hidden=true]")) {
      return false;
    }
    // 页面先渲染顶部、后渲染侧栏时，已挂载的顶部入口也需要迁移。
    if (document.querySelector(navigationRailSelector)) return false;
    if (button.dataset.codeyNativeSlot === "true") {
      const measure = document.getElementById(buttonMeasureId);
      return parent === button.__codeyActionRow
        && button.nextElementSibling === null
        && !!parent.closest('[data-test-id="header-shell-slot"]')
        && (!button.__codeyMeasureRow || (
          measure?.parentElement === button.__codeyMeasureRow
          && measure?.isConnected === true
        ));
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
    const railMount = findRailFooterMount();
    // 仅无图标栏的旧版布局使用顶部入口，侧栏重建期间等待它恢复。
    const mount = railMount || (!navigationRailSeen ? findHeaderMount() : null);
    if (!mount) {
      existingButton?.remove?.();
      document.getElementById(buttonMeasureId)?.remove();
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
        void openSettings();
      }, true);
      button.addEventListener("pointerenter", scheduleSettingsPreload, { passive: true });
      button.addEventListener("pointerleave", cancelSettingsPreload, { passive: true });
      button.addEventListener("pointerdown", preloadSettingsOverlay, { passive: true });
      button.addEventListener("focus", preloadSettingsOverlay);
    }
    if (railMount) {
      button.dataset.codeyRailSlot = "true";
      delete button.dataset.codeyHeaderActions;
      delete button.dataset.codeyNativeSlot;
      document.getElementById(buttonMeasureId)?.remove();
      button.__codeyActionRow = null;
      button.__codeyMeasureRow = null;
      button.__codeyHeaderAnchor = null;
      button.__codeyRailStack = mount.target;
      button.__codeyRailAnchor = mount.before;
      if (button.parentElement !== mount.target || button.nextElementSibling !== mount.before) {
        mount.target.insertBefore(button, mount.before);
      }
      applyUpdateBadge(button);
      headerMountDirty = false;
      return;
    }
    delete button.dataset.codeyRailSlot;
    button.__codeyRailStack = null;
    button.__codeyRailAnchor = null;
    if (mount.before) {
      button.dataset.codeyHeaderActions = "true";
    } else {
      delete button.dataset.codeyHeaderActions;
    }
    if (mount.nativeSlot) {
      button.dataset.codeyNativeSlot = "true";
      let measure = document.getElementById(buttonMeasureId);
      if (mount.measureRow && !measure) {
        measure = document.createElement("span");
        measure.id = buttonMeasureId;
        measure.setAttribute("aria-hidden", "true");
      }
      if (mount.measureRow) {
        if (measure.parentElement !== mount.measureRow) mount.measureRow.appendChild(measure);
      } else {
        measure?.remove();
      }
      button.__codeyActionRow = mount.target;
      button.__codeyMeasureRow = mount.measureRow || null;
    } else {
      delete button.dataset.codeyNativeSlot;
      document.getElementById(buttonMeasureId)?.remove();
      button.__codeyActionRow = null;
      button.__codeyMeasureRow = null;
    }
    if (mount.before) {
      if (button.parentElement !== mount.target || button.nextElementSibling !== mount.before) {
        mount.target.insertBefore(button, mount.before);
      }
    } else if (
      button.parentElement !== mount.target
      || (mount.nativeSlot && button.nextElementSibling !== null)
    ) {
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

  const appServerRequestClient = () => {
    const clients = window.__codeyAppServerRequestClients;
    if (!clients || typeof clients.get !== "function") return null;
    const local = clients.get("local");
    if (local && typeof local.sendRequest === "function") return local;
    if (typeof clients.values !== "function") return null;
    const all = [...clients.values()].filter((client) => typeof client?.sendRequest === "function");
    return all.length === 1 ? all[0] : null;
  };
  const bootstrapReloadMcpServers = async () => {
    const client = appServerRequestClient();
    if (client) {
      await client.sendRequest("config/mcpServer/reload", {});
      return { ok: true };
    }
    const loaded = await loadSessionTools();
    if (
      loaded
      && typeof window.__codeyReloadMcpServers === "function"
      && window.__codeyReloadMcpServers !== bootstrapReloadMcpServers
    ) {
      return window.__codeyReloadMcpServers();
    }
    const error = new Error("当前 Codex 暂不支持MCP 配置刷新，请稍后重试");
    error.code = "codey_capability_unavailable";
    throw error;
  };
  if (typeof window.__codeyReloadMcpServers !== "function") {
    window.__codeyReloadMcpServers = bootstrapReloadMcpServers;
  }

  if (rendererCoreAlreadyLoaded) return;
  window.addEventListener?.(updateAvailableEvent, (event) => {
    const result = "detail" in event
      ? event.detail
      : window.__codeyUpdateAvailability;
    setUpdateAvailability(result, { dispatch: false });
    if (!hasDetectedUpdate()) scheduleUpdateCheck();
  });
  window.addEventListener?.(configChangedEvent, (event) => {
    const policy = event.detail?.config?.codeyUpdatePolicy;
    if (["off", "stable", "experimental"].includes(policy) && codeyUpdatePolicy !== policy) {
      setAutomaticUpdateChecks(policy);
      if (policy !== "off") scheduleUpdateCheck(0);
    }
    accountUsageLastResult = null;
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
        && node.id !== buttonMeasureId
        && node.id !== accountUsageId
      ) {
        return true;
      }
    }
    return false;
  };

  // Lightweight observer telemetry: per-handler call count, mutation count and
  // wall time, exposed on window.__codeyObserverStats for performance triage.
  // No behavior change; timing falls back to Date.now() where performance is
  // unavailable (test sandboxes).
  const codeyTimed = (name, count, run) => {
    const now = () =>
      typeof performance === "object" && typeof performance.now === "function"
        ? performance.now()
        : Date.now();
    const stats = (window.__codeyObserverStats ||= {});
    const entry = (stats[name] ||= { calls: 0, items: 0, totalMs: 0, maxMs: 0 });
    const startedAt = now();
    try {
      return run();
    } finally {
      const elapsed = now() - startedAt;
      entry.calls += 1;
      entry.items += count;
      entry.totalMs += elapsed;
      if (elapsed > entry.maxMs) entry.maxMs = elapsed;
    }
  };
  const elementClassText = (element) => {
    if (!element) return "";
    if (typeof element.className === "string" && element.className) {
      return element.className;
    }
    return typeof element.getAttribute === "function"
      ? String(element.getAttribute("class") || "")
      : "";
  };
  const elementHasClass = (element, token) => {
    if (!element) return false;
    if (typeof element.classList?.contains === "function") {
      return element.classList.contains(token);
    }
    const raw = elementClassText(element);
    if (!raw || !raw.includes(token)) return false;
    return raw.trim().split(/\s+/).includes(token);
  };
  const elementMatchesOpenWithinAncestor = (element, isButton) => {
    const raw = elementClassText(element);
    if (!raw) return false;
    for (const token of openWithinAncestorClasses) {
      if (raw.includes(token) && elementHasClass(element, token)) return true;
    }
    if (
      isButton
      && raw.includes(openWithinButtonAncestorClass)
      && elementHasClass(element, openWithinButtonAncestorClass)
    ) {
      return true;
    }
    return false;
  };
  const isElementConnected = (element) => {
    if (!element || element.isConnected === false) return false;
    let cursor = element.parentElement;
    while (cursor) {
      if (cursor.isConnected === false) return false;
      cursor = cursor.parentElement;
    }
    return true;
  };
  const syncOpenWithinContainers = () => {
    const nextContainers = new Set();
    for (const element of [...openStateElements]) {
      if (
        !isElementConnected(element)
        || element.getAttribute?.("data-state") !== "open"
      ) {
        openStateElements.delete(element);
        continue;
      }
      const parent = element.parentElement;
      if (
        parent
        && (
          elementHasClass(parent, openWithinDirectCardClass)
          || elementHasClass(parent.parentElement, openWithinDirectCardClass)
        )
      ) {
        nextContainers.add(parent);
      }
      const isButton = String(element.tagName || "").toUpperCase() === "BUTTON";
      let cursor = parent;
      while (cursor) {
        if (elementMatchesOpenWithinAncestor(cursor, isButton)) {
          nextContainers.add(cursor);
        }
        cursor = cursor.parentElement;
      }
    }
    for (const container of [...syncedOpenContainers]) {
      if (!nextContainers.has(container)) {
        container.removeAttribute?.(openWithinAttribute);
        syncedOpenContainers.delete(container);
      }
    }
    for (const container of nextContainers) {
      if (!syncedOpenContainers.has(container)) {
        container.setAttribute?.(openWithinAttribute, "");
        syncedOpenContainers.add(container);
      }
    }
  };
  const collectOpenStateElements = (root = document) => {
    let dirty = false;
    for (const element of queryWithin(root, '[data-state="open"]')) {
      if (!openStateElements.has(element)) {
        openStateElements.add(element);
        dirty = true;
      }
    }
    for (const element of [...openStateElements]) {
      if (
        !isElementConnected(element)
        || element.getAttribute?.("data-state") !== "open"
      ) {
        openStateElements.delete(element);
        dirty = true;
      }
    }
    if (dirty || syncedOpenContainers.size > 0) {
      syncOpenWithinContainers();
    }
  };
  const collectAddedOpenStateElements = (nodes) => {
    let dirty = false;
    for (const node of nodes || []) {
      const element = node instanceof HTMLElement ? node : null;
      if (!element) continue;
      if (element.tagName === "LINK") {
        bindStylesheetPatchOnLink(element);
      }
      if (
        element.getAttribute?.("data-state") === "open"
        && !openStateElements.has(element)
      ) {
        openStateElements.add(element);
        dirty = true;
      }
      if (typeof element.querySelectorAll === "function") {
        for (const descendant of element.querySelectorAll('[data-state="open"]')) {
          if (!openStateElements.has(descendant)) {
            openStateElements.add(descendant);
            dirty = true;
          }
        }
      }
    }
    if (dirty) {
      syncOpenWithinContainers();
    }
  };
  const rewriteOpenStateCssText = (cssText) => {
    if (typeof cssText !== "string" || !cssText.includes("data-state")) {
      return cssText;
    }
    return cssText
      .replace(
        openStateCssAriaAndDataStateHasPattern,
        `:is(:has([aria-expanded=true]),[${openWithinAttribute}])`,
      )
      .replace(openStateCssDataStateHasPattern, `[${openWithinAttribute}]`);
  };
  const selectorHasOpenStateHas = (selectorText) =>
    typeof selectorText === "string"
    && selectorText.includes(":has(")
    && selectorText.includes("data-state");
  const ruleListHasOpenStateHasSelector = (rules) => {
    if (!rules) return false;
    for (let index = 0; index < rules.length; index += 1) {
      const rule = rules[index];
      if (!rule) continue;
      if (selectorHasOpenStateHas(rule.selectorText)) return true;
      if (rule.cssRules && ruleListHasOpenStateHasSelector(rule.cssRules)) {
        return true;
      }
    }
    return false;
  };
  const patchLoadedOpenStateStylesheets = () => {
    const sheets = document.styleSheets;
    if (!sheets || typeof sheets.length !== "number") return 0;
    let patchedCount = 0;
    for (let index = 0; index < sheets.length; index += 1) {
      const sheet = sheets[index];
      const ownerNode = sheet?.ownerNode;
      if (
        !ownerNode
        || ownerNode.id === styleId
        || ownerNode.getAttribute?.(openStateCssPatchedDatasetKey) === "true"
      ) {
        continue;
      }
      let rules;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      if (!rules || typeof rules.length !== "number" || rules.length === 0) {
        continue;
      }
      const lastRuleText = String(rules[rules.length - 1]?.cssText || "");
      if (lastRuleText.includes(openStateCssPatchedMarker)) {
        ownerNode.setAttribute?.(openStateCssPatchedDatasetKey, "true");
        continue;
      }
      if (!ruleListHasOpenStateHasSelector(rules)) {
        ownerNode.setAttribute?.(openStateCssPatchedDatasetKey, "true");
        continue;
      }
      const chunks = new Array(rules.length + 1);
      for (let ruleIndex = 0; ruleIndex < rules.length; ruleIndex += 1) {
        chunks[ruleIndex] = rewriteOpenStateCssText(
          String(rules[ruleIndex]?.cssText || ""),
        );
      }
      chunks[rules.length] = `:root{${openStateCssPatchedMarker}:1}`;
      const nextCssText = chunks.join("\n");
      const tagName = String(ownerNode.tagName || "").toUpperCase();
      if (tagName === "STYLE") {
        ownerNode.textContent = nextCssText;
        ownerNode.setAttribute?.(openStateCssPatchedDatasetKey, "true");
        patchedCount += 1;
        continue;
      }
      if (tagName === "LINK" && ownerNode.parentElement) {
        const replacement = document.createElement("style");
        replacement.setAttribute?.(openStateCssPatchedDatasetKey, "true");
        const href =
          ownerNode.getAttribute?.("data-href")
          || ownerNode.getAttribute?.("href");
        if (href) replacement.setAttribute?.("data-href", href);
        const media = ownerNode.getAttribute?.("media");
        if (media) replacement.setAttribute?.("media", media);
        const nonce = ownerNode.nonce || ownerNode.getAttribute?.("nonce");
        if (nonce) replacement.setAttribute?.("nonce", nonce);
        replacement.textContent = nextCssText;
        if (typeof ownerNode.parentElement.insertBefore === "function") {
          ownerNode.parentElement.insertBefore(replacement, ownerNode);
        } else {
          ownerNode.parentElement.appendChild?.(replacement);
        }
        try {
          sheet.disabled = true;
        } catch {}
        ownerNode.remove?.();
        patchedCount += 1;
      }
    }
    return patchedCount;
  };
  const bindStylesheetPatchOnLink = (element) => {
    if (
      !element
      || String(element.tagName || "").toUpperCase() !== "LINK"
      || element.getAttribute?.(openStateCssPatchedDatasetKey) === "true"
    ) {
      return;
    }
    const rel = String(element.getAttribute?.("rel") || "").toLowerCase();
    if (rel && !rel.split(/\s+/).includes("stylesheet")) return;
    if (element.sheet) {
      patchLoadedOpenStateStylesheets();
      return;
    }
    element.addEventListener?.(
      "load",
      () => {
        patchLoadedOpenStateStylesheets();
      },
      { once: true },
    );
  };
  patchLoadedOpenStateStylesheets();
  collectOpenStateElements(document);
  const handleBootstrapMutations = (mutations) =>
    codeyTimed("renderer-inject.bootstrapMutations", mutations?.length ?? 0, () => handleBootstrapMutationsImpl(mutations));
  const handleBootstrapMutationsImpl = (mutations) => {
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (
          mutation.attributeName === "data-state"
          || (
            mutation.attributeName == null
            && target
            && (openStateElements.has(target) || target.hasAttribute?.("data-state"))
          )
        ) {
          if (target?.getAttribute?.("data-state") === "open") {
            if (!openStateElements.has(target)) {
              openStateElements.add(target);
              syncOpenWithinContainers();
            }
          } else if (target && openStateElements.delete(target)) {
            syncOpenWithinContainers();
          }
        }
        continue;
      }
      if (
        mutation.type === "childList"
        && openStateElements.size > 0
        && (mutation.removedNodes?.length ?? 0) > 0
      ) {
        syncOpenWithinContainers();
      }
      const conversationTurn = target?.closest?.("[data-turn-key]");
      if (
        mutation.type === "childList"
        && conversationTurn
        && conversationTurn.isConnected !== false
      ) {
        continue;
      }
      if (
        target?.id === accountUsageId
        || target?.closest?.(`#${accountUsageId}`)
      ) {
        continue;
      }
      if (
        mutation.type === "childList"
        && (mutation.addedNodes?.length ?? 0) > 0
      ) {
        collectAddedOpenStateElements(mutation.addedNodes);
      }
    }
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (
          mutation.attributeName === "data-state"
          || (
            mutation.attributeName == null
            && target
            && (openStateElements.has(target) || target.hasAttribute?.("data-state"))
          )
          && !target?.matches?.(headerSelector)
          && !target?.matches?.(sidebarSelector)
        ) {
          continue;
        }
      }
      const conversationTurn = target?.closest?.("[data-turn-key]");
      if (
        mutation.type === "childList"
        && conversationTurn
        && conversationTurn.isConnected !== false
      ) {
        continue;
      }
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
      "data-state",
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
  window.__codeyPatchOpenStateStylesheets = patchLoadedOpenStateStylesheets;
  window.__codeySyncOpenWithinContainers = syncOpenWithinContainers;

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
