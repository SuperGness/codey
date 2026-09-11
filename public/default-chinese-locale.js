// Default Chinese locale bootstrap injected before renderer-inject.js.
(() => {
  const defaultChineseLocale = "zh-CN";
  const defaultChineseLanguages = [defaultChineseLocale, "zh", "en-US", "en"];
  const statsigI18nDynamicConfigId = "72216192";
  const localeReloadStorageKey = "codey.defaultChineseLocale.reload.v1";

  const installDefaultChineseLocale = () => {
    const existing = window.__codeyDefaultChineseLocale;
    if (existing?.version === 5 && existing.locale === defaultChineseLocale) {
      existing.ensureSynced?.();
      return;
    }

    const state = {
      version: 5,
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

    const statsigClients = window.__codeySharedRuntime.statsigClients;

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

    const wrapStatsigInstances = (instances) => {
      if (!instances || typeof instances !== "object") return instances;
      if (instances.__codeyDefaultChineseLocaleInstancesPatched) return instances;
      try {
        Object.defineProperty(instances, "__codeyDefaultChineseLocaleInstancesPatched", {
          configurable: true,
          enumerable: false,
          value: true,
        });
      } catch {
        return instances;
      }
      if (typeof Proxy !== "function") return instances;
      try {
        return new Proxy(instances, {
          set(target, property, value) {
            target[property] = value;
            if (property !== "__codeyDefaultChineseLocaleInstancesPatched") {
              patchStatsigClient(value);
            }
            return true;
          },
        });
      } catch {
        return instances;
      }
    };

    const wrapStatsigRootInstances = (root) => {
      let instances;
      try {
        instances = root.instances;
      } catch {
        return;
      }
      const assignInstances = (next) => {
        instances = wrapStatsigInstances(next);
        if (!instances || typeof instances !== "object") return;
        try {
          for (const client of Object.values(instances)) patchStatsigClient(client);
        } catch {
        }
      };
      assignInstances(instances);
      try {
        Object.defineProperty(root, "instances", {
          configurable: true,
          get: () => instances,
          set: assignInstances,
        });
      } catch {
      }
    };

    const patchStatsigRoot = (root) => {
      if (!root || typeof root !== "object") return;
      if (root.__codeyDefaultChineseLocaleRootPatched) {
        state.statsigRootPatched = true;
        wrapStatsigRootInstances(root);
        return;
      }
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
      wrapStatsigRootInstances(root);
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
      if (Date.now() - startedAt >= 15000) return;
      // Once the first client is patched, the root setter catches clients
      // replaced later; keep a slower sweep only for clients added in place.
      window.setTimeout?.(scanStatsigUntilReady, state.statsigClientsPatched > 0 ? 1000 : 250);
    };
    window.setTimeout?.(scanStatsigUntilReady, 250);
  };

  installDefaultChineseLocale();
})();

