// Lightweight renderer bootstrap injected by the Codey CDP launcher.
// The heavier session/sidebar tools live in codey-inject.js and are loaded
// only after Codex's sidebar is present.
(() => {
  const rendererCoreAlreadyLoaded = window.__codeyRendererCoreLoaded === true;
  window.__codeyRendererCoreLoaded = true;
  window.__codeyRendererModuleReady = true;

  const sessionToolsLoadPath = "/internal/codey/session-tools/load";
  const updateCheckPath = "/api/check_for_updates";
  const buttonId = "codey-settings-button";
  const styleId = "codey-core-injected-style";
  const updateAvailableEvent = "codey-update-availability-changed";
  const updateCheckIntervalMs = 30 * 60 * 1000;
  const updateCheckTimeoutMs = 12_000;
  const sidebarSelector = [
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
  ].join(", ");
  /** Typical Windows caption cluster (min / max / close). */
  const windowControlsReservePx = 138;
  const settingsIcon = `
    <svg viewBox="0 0 350 350" aria-hidden="true" focusable="false">
      <rect x="0" y="0" width="350" height="350" rx="34" fill="#fff" stroke="none"></rect>
      <path d="M70 301c-16 0-24-18-13-30l73-77c8-8 8-20 0-28L65 101C50 86 57 61 78 57c9-2 18 1 25 8l91 91c18 18 18 46 0 64l-66 66c-6 6-2 15 7 15h183" fill="none" stroke="currentColor" stroke-width="22" stroke-linecap="round" stroke-linejoin="round"></path>
    </svg>
  `;
  const defaultChineseLocale = "zh-CN";
  const defaultChineseLanguages = [defaultChineseLocale, "zh", "en-US", "en"];
  const statsigI18nDynamicConfigId = "72216192";
  const localeReloadStorageKey = "codey.defaultChineseLocale.reload.v1";

  let sessionToolsLoadPromise = null;
  let scanTimer = 0;
  let updateCheckTimer = 0;
  let updateCheckInFlight = false;
  let sessionToolsInteractionArmed = false;
  let bootstrapObserver = null;

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

  const addStyle = () => {
    if (document.getElementById(styleId)) return;
    const style = document.createElement("style");
    style.id = styleId;
    // Sit just left of the window min/max/close cluster (titlebar overlay or
    // a Windows-sized reserve), not inside the Codex sidebar/header.
    style.textContent = `
      #${buttonId} {
        -webkit-app-region: no-drag !important;
        pointer-events: auto !important;
        position: fixed !important;
        z-index: 2147483646 !important;
        top: max(4px, env(titlebar-area-y, 4px));
        right: max(
          ${windowControlsReservePx}px,
          calc(100vw - env(titlebar-area-x, 0px) - env(titlebar-area-width, 100vw))
        );
        display: inline-grid;
        place-items: center;
        width: 28px;
        height: 28px;
        margin: 0;
        border: 0;
        border-radius: 8px;
        padding: 0;
        background: transparent;
        color: inherit;
        cursor: pointer;
        opacity: .9;
        user-select: none;
        transition: background .15s ease, opacity .15s ease, transform .15s ease;
      }
      #${buttonId}:hover { background: rgba(127, 127, 127, .16); opacity: 1; }
      #${buttonId}:active { transform: translateY(1px); }
      #${buttonId}:focus-visible { outline: 2px solid rgba(139, 151, 255, .72); outline-offset: 2px; }
      #${buttonId} svg { display: block; width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 22; stroke-linecap: round; stroke-linejoin: round; }
      #${buttonId} .codey-settings-label { position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0; }
      #${buttonId}::after { content: ""; position: absolute; top: 3px; right: 3px; width: 7px; height: 7px; border-radius: 999px; background: #ff3b30; box-shadow: 0 0 0 2px Canvas; opacity: 0; transform: scale(.7); transition: opacity .15s ease, transform .15s ease; pointer-events: none; }
      #${buttonId}[data-codey-update-available="true"]::after { opacity: 1; transform: scale(1); }
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
    if (hasDetectedUpdate()) {
      button.setAttribute("data-codey-update-available", "true");
      button.setAttribute("aria-label", "打开 Codey 配置，有可用更新");
      button.title = "打开 Codey 配置（发现新版本）";
      return;
    }
    button.removeAttribute?.("data-codey-update-available");
    button.setAttribute("aria-label", "打开 Codey 配置");
    button.title = "打开 Codey 配置";
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

  const withTimeout = (promise, timeoutMs) => new Promise((resolve, reject) => {
    const timer = window.setTimeout(
      () => reject(new Error("检查更新超时")),
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
        callBridge(updateCheckPath, {}),
        updateCheckTimeoutMs,
      );
      if (result?.status !== "failed" && result?.updateAvailable === true) {
        setUpdateAvailability(result);
        return;
      }
    } catch {
      // 后台更新检测保持静默。
    } finally {
      updateCheckInFlight = false;
      if (!hasDetectedUpdate()) scheduleUpdateCheck();
    }
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

  const installDefaultChineseLocale = () => {
    const existing = window.__codeyDefaultChineseLocale;
    if (existing?.version === 4 && existing.locale === defaultChineseLocale) {
      existing.ensureSynced?.();
      return;
    }

    const state = {
      version: 4,
      locale: defaultChineseLocale,
      navigatorPatched: false,
      statsigClientsPatched: 0,
      statsigRootPatched: false,
      settingSyncStarted: false,
      settingSynced: false,
      settingSyncInFlight: false,
      settingSyncAttempts: 0,
      settingSyncError: null,
      ensureSynced: null,
      snapshot() {
        return {
          version: this.version,
          locale: this.locale,
          rendererAssetPatched:
            globalThis.__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__ === true,
          navigatorPatched: this.navigatorPatched,
          statsigClientsPatched: this.statsigClientsPatched,
          statsigRootPatched: this.statsigRootPatched,
          settingSyncStarted: this.settingSyncStarted,
          settingSynced: this.settingSynced,
          settingSyncInFlight: this.settingSyncInFlight,
          settingSyncAttempts: this.settingSyncAttempts,
          settingSyncError: this.settingSyncError,
        };
      },
    };
    window.__codeyDefaultChineseLocale = state;

    const defineNavigatorGetter = (target, name, value) => {
      if (!target || (typeof target !== "object" && typeof target !== "function")) return false;
      try {
        Object.defineProperty(target, name, {
          configurable: true,
          get: () => value,
        });
        return true;
      } catch {
        return false;
      }
    };

    const patchNavigatorLocale = () => {
      const navigatorTargets = [];
      try {
        if (typeof Navigator === "function" && Navigator.prototype) {
          navigatorTargets.push(Navigator.prototype);
        }
      } catch {
      }
      try {
        if (window.navigator) navigatorTargets.push(window.navigator);
      } catch {
      }
      state.navigatorPatched = navigatorTargets
        .some((target) => (
          defineNavigatorGetter(target, "language", defaultChineseLocale)
          && defineNavigatorGetter(target, "languages", defaultChineseLanguages)
        ));
    };

    const patchDynamicConfig = (dynamicConfig) => {
      if (!dynamicConfig || typeof dynamicConfig !== "object") return dynamicConfig;
      const value = dynamicConfig.value && typeof dynamicConfig.value === "object"
        ? dynamicConfig.value
        : {};
      try {
        dynamicConfig.value = {
          ...value,
          enable_i18n: true,
          locale_source: "SYSTEM",
        };
      } catch {
      }
      if (typeof dynamicConfig.get === "function" && !dynamicConfig.__codeyDefaultChineseLocaleGetPatched) {
        const originalGet = dynamicConfig.get.bind(dynamicConfig);
        dynamicConfig.get = (key, fallback) => {
          if (key === "enable_i18n") return true;
          if (key === "locale_source") return "SYSTEM";
          return originalGet(key, fallback);
        };
        dynamicConfig.__codeyDefaultChineseLocaleGetPatched = true;
      }
      return dynamicConfig;
    };

    const statsigClients = () => {
      const clients = [];
      for (const root of [window.__STATSIG__, globalThis.__STATSIG__]) {
        if (!root || typeof root !== "object") continue;
        try {
          clients.push(root.firstInstance);
        } catch {
        }
        try {
          if (typeof root.instance === "function") clients.push(root.instance());
        } catch {
        }
        try {
          if (root.instances && typeof root.instances === "object") {
            clients.push(...Object.values(root.instances));
          }
        } catch {
        }
      }
      return clients.filter(
        (client, index, array) =>
          client && typeof client === "object" && array.indexOf(client) === index,
      );
    };

    const patchStatsigClient = (client) => {
      if (!client || typeof client !== "object") return;
      if (typeof client.getDynamicConfig !== "function") return;
      if (!client.__codeyDefaultChineseLocalePatched) {
        const originalGetDynamicConfig = client.getDynamicConfig.bind(client);
        try {
          client.getDynamicConfig = (name, options) => {
            const result = originalGetDynamicConfig(name, options);
            return name === statsigI18nDynamicConfigId ? patchDynamicConfig(result) : result;
          };
          client.__codeyDefaultChineseLocalePatched = true;
          state.statsigClientsPatched += 1;
        } catch {
        }
      }
      try {
        patchDynamicConfig(client.getDynamicConfig(statsigI18nDynamicConfigId, {
          disableExposureLog: true,
        }));
      } catch {
      }
    };

    const patchStatsigRoot = (root) => {
      if (!root || typeof root !== "object") return;
      if (root.__codeyDefaultChineseLocaleRootPatched) return;
      root.__codeyDefaultChineseLocaleRootPatched = true;
      state.statsigRootPatched = true;
      for (const key of ["firstInstance", "instance"]) {
        let current;
        try {
          current = root[key];
        } catch {
          continue;
        }
        patchStatsigClient(typeof current === "function" && key === "instance" ? current.call(root) : current);
        try {
          Object.defineProperty(root, key, {
            configurable: true,
            get: () => current,
            set: (next) => {
              current = next;
              patchStatsigClient(typeof next === "function" && key === "instance" ? next.call(root) : next);
            },
          });
        } catch {
        }
      }
    };

    const installStatsigRootSetter = () => {
      let descriptor;
      try {
        descriptor = Object.getOwnPropertyDescriptor(window, "__STATSIG__");
      } catch {
        descriptor = null;
      }
      if (descriptor && descriptor.configurable === false) {
        patchStatsigRoot(window.__STATSIG__);
        return;
      }
      let currentRoot = window.__STATSIG__;
      patchStatsigRoot(currentRoot);
      try {
        Object.defineProperty(window, "__STATSIG__", {
          configurable: true,
          get: () => currentRoot,
          set: (next) => {
            currentRoot = next;
            patchStatsigRoot(next);
            patchStatsigClients();
          },
        });
      } catch {
      }
    };

    const patchStatsigClients = () => {
      installStatsigRootSetter();
      patchStatsigRoot(window.__STATSIG__ || globalThis.__STATSIG__);
      for (const client of statsigClients()) patchStatsigClient(client);
    };

    const waitForElectronBridge = () => new Promise((resolve) => {
      if (typeof window.setTimeout !== "function") {
        resolve(null);
        return;
      }
      const startedAt = Date.now();
      const check = () => {
        const bridge = window.electronBridge;
        if (bridge && typeof bridge.sendMessageFromView === "function") {
          resolve(bridge);
          return;
        }
        if (Date.now() - startedAt >= 5000) {
          resolve(null);
          return;
        }
        window.setTimeout(check, 50);
      };
      check();
    });

    const callCodexSettingApi = (bridge, method, params) => new Promise((resolve, reject) => {
      const requestId = globalThis.crypto && typeof globalThis.crypto.randomUUID === "function"
        ? globalThis.crypto.randomUUID()
        : `codey-locale-${Date.now()}-${Math.random().toString(16).slice(2)}`;
      let timeout = 0;
      const cleanup = () => {
        window.clearTimeout?.(timeout);
        window.removeEventListener?.("message", onMessage);
      };
      const onMessage = (event) => {
        const message = event?.data;
        if (!message || message.type !== "fetch-response" || message.requestId !== requestId) return;
        cleanup();
        if (message.responseType !== "success") {
          reject(new Error(message.error || `Codex ${method} failed`));
          return;
        }
        try {
          resolve(JSON.parse(message.bodyJsonString || "null"));
        } catch (error) {
          reject(error);
        }
      };
      window.addEventListener?.("message", onMessage);
      timeout = window.setTimeout?.(() => {
        cleanup();
        reject(new Error(`Codex ${method} timed out`));
      }, 5000);
      const message = {
        type: "fetch",
        requestId,
        method: "POST",
        url: `vscode://codex/${method}`,
        body: JSON.stringify(params),
      };
      Promise.resolve(bridge.sendMessageFromView(message)).catch((error) => {
        cleanup();
        reject(error);
      });
    });

    const reloadAfterLocaleChange = () => {
      try {
        if (window.sessionStorage?.getItem(localeReloadStorageKey) === defaultChineseLocale) {
          return;
        }
        window.sessionStorage?.setItem(localeReloadStorageKey, defaultChineseLocale);
      } catch {
      }
      window.location?.reload?.();
    };

    const clearLocaleReloadMarker = () => {
      try {
        window.sessionStorage?.removeItem(localeReloadStorageKey);
      } catch {
      }
    };

    const syncCodexLocaleSettingOnce = async () => {
      state.settingSyncStarted = true;
      const bridge = await waitForElectronBridge();
      if (!bridge) throw new Error("Codex Electron bridge unavailable");
      const response = await callCodexSettingApi(bridge, "get-setting", { key: "localeOverride" });
      if (response?.value === defaultChineseLocale) {
        state.settingSynced = true;
        state.settingSyncError = null;
        clearLocaleReloadMarker();
        return;
      }
      await callCodexSettingApi(bridge, "set-setting", {
        key: "localeOverride",
        value: defaultChineseLocale,
      });
      const verification = await callCodexSettingApi(
        bridge,
        "get-setting",
        { key: "localeOverride" },
      );
      if (verification?.value !== defaultChineseLocale) {
        throw new Error("Codex localeOverride was not persisted");
      }
      state.settingSynced = true;
      state.settingSyncError = null;
      reloadAfterLocaleChange();
    };

    const ensureCodexLocaleSetting = () => {
      if (state.settingSynced || state.settingSyncInFlight) return;
      state.settingSyncInFlight = true;
      void (async () => {
        const retryDelays = [0, 250, 750, 1500, 3000, 5000];
        for (const delay of retryDelays) {
          if (delay > 0) {
            await new Promise((resolve) => {
              if (typeof window.setTimeout === "function") {
                window.setTimeout(resolve, delay);
              } else {
                resolve();
              }
            });
          }
          state.settingSyncAttempts += 1;
          try {
            await syncCodexLocaleSettingOnce();
            return;
          } catch (error) {
            state.settingSyncError = error instanceof Error ? error.message : String(error);
          }
        }
        console.warn(
          "[Codey] Codex 中文语言设置同步失败，将在窗口重新聚焦时重试",
          state.settingSyncError,
        );
      })().finally(() => {
        state.settingSyncInFlight = false;
      });
    };
    state.ensureSynced = ensureCodexLocaleSetting;

    patchNavigatorLocale();
    patchStatsigClients();
    ensureCodexLocaleSetting();
    window.addEventListener?.("focus", ensureCodexLocaleSetting);
    window.addEventListener?.("pageshow", ensureCodexLocaleSetting);

    const startedAt = Date.now();
    const scanStatsigUntilReady = () => {
      patchStatsigClients();
      const elapsed = Date.now() - startedAt;
      if (elapsed >= 15000) return;
      window.setTimeout?.(scanStatsigUntilReady, elapsed < 1000 ? 50 : 250);
    };
    window.setTimeout?.(scanStatsigUntilReady, 50);
  };

  const mountedButtonIsUsable = (button) => (
    button instanceof HTMLElement
    && button.isConnected === true
    && button.dataset.codeyWindowChrome === "true"
    && button.parentElement === document.documentElement
    && !button.closest?.("[hidden], [aria-hidden=true]")
  );

  const mountButton = () => {
    addStyle();
    let button = document.getElementById(buttonId);
    if (mountedButtonIsUsable(button)) {
      applyUpdateBadge(button);
      return;
    }
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
    button.dataset.codeyWindowChrome = "true";
    delete button.dataset.codeyHeaderActions;
    if (button.parentElement !== document.documentElement) {
      document.documentElement.appendChild(button);
    }
    applyUpdateBadge(button);
  };

  const sidebarDetected = (root = document) => queryWithin(root, sidebarSelector).length > 0;

  const loadSessionTools = () => {
    if (window.__codeySessionToolsInjectLoaded === true) return Promise.resolve(true);
    if (sessionToolsLoadPromise) return sessionToolsLoadPromise;
    sessionToolsLoadPromise = Promise.resolve(callBridge(sessionToolsLoadPath, {}))
      .then((result) => {
        if (!result || result.status !== "ok") {
          throw new Error(result?.message || "会话工具加载请求失败");
        }
        if (window.__codeySessionToolsInjectLoaded !== true) {
          throw new Error(window.__codeySessionToolsError || "会话工具未完成初始化");
        }
        disarmSessionToolsInteraction();
        bootstrapObserver?.disconnect();
        bootstrapObserver = null;
        return true;
      })
      .catch((error) => {
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
      sessionToolsInteractionArmed
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

  const scan = (root = document) => {
    mountButton();
    if (sidebarDetected(root)) armSessionToolsInteraction();
  };

  const scheduleScan = (root = document) => {
    window.clearTimeout(scanTimer);
    scanTimer = window.setTimeout(() => {
      scanTimer = 0;
      scan(root);
    }, 60);
  };

  const invalidateHeaderMount = (root = document) => {
    // Kept for session-tools callers; button no longer depends on header layout.
    scheduleScan(root || document);
  };

  installDefaultChineseLocale();
  if (rendererCoreAlreadyLoaded) return;
  window.addEventListener?.(updateAvailableEvent, (event) => {
    const result = "detail" in event
      ? event.detail
      : window.__codeyUpdateAvailability;
    setUpdateAvailability(result, { dispatch: false });
    if (!hasDetectedUpdate()) scheduleUpdateCheck();
  });
  scan();
  scheduleUpdateCheck(0);

  bootstrapObserver = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (target?.matches?.(sidebarSelector)) {
          scheduleScan(target);
          return;
        }
        continue;
      }
      for (const node of mutation.addedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) continue;
        const matched = element.matches?.(sidebarSelector)
          ? element
          : element.querySelector?.(sidebarSelector);
        if (!matched) continue;
        scheduleScan(element);
        return;
      }
    }
  });
  bootstrapObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: [
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
  });

  window.__codeyLoadSessionTools = loadSessionTools;
  window.__codeyRendererScan = scan;
  window.__codeyRendererInvalidateHeaderMount = invalidateHeaderMount;

  window.addEventListener?.("focus", () => scan());
  window.addEventListener?.("pageshow", () => scan());
})();
