// Lightweight renderer bootstrap injected by the Codey CDP launcher.
// The heavier session/sidebar tools live in codey-inject.js and are loaded
// only after Codex's sidebar is present.
(() => {
  if (window.__codeyRendererCoreLoaded) return;
  window.__codeyRendererCoreLoaded = true;
  window.__codeyRendererModuleReady = true;

  const sessionToolsLoadPath = "/internal/codey/session-tools/load";
  const buttonId = "codey-settings-button";
  const styleId = "codey-core-injected-style";
  const sidebarSelector = [
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]",
  ].join(", ");
  const headerSelector = "header, nav";
  const settingsIcon = `
    <svg viewBox="0 0 350 350" aria-hidden="true" focusable="false">
      <rect x="0" y="0" width="350" height="350" rx="34" fill="#fff" stroke="none"></rect>
      <path d="M70 301c-16 0-24-18-13-30l73-77c8-8 8-20 0-28L65 101C50 86 57 61 78 57c9-2 18 1 25 8l91 91c18 18 18 46 0 64l-66 66c-6 6-2 15 7 15h183" fill="none" stroke="currentColor" stroke-width="22" stroke-linecap="round" stroke-linejoin="round"></path>
    </svg>
  `;

  let sessionToolsLoadPromise = null;
  let scanTimer = 0;
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
    style.textContent = `
      #${buttonId} { -webkit-app-region: no-drag !important; pointer-events: auto !important; position: relative; z-index: 2147483641; display: inline-grid; place-items: center; flex: 0 0 auto; width: 32px; height: 32px; border: 0; border-radius: 8px; padding: 0; margin-inline-start: 8px; margin-inline-end: 18px; background: transparent; color: inherit; cursor: pointer; opacity: .86; user-select: none; transition: background .15s ease, opacity .15s ease, transform .15s ease; }
      #${buttonId}[data-codey-header-actions="true"] { width: 28px; height: 28px; margin-inline-start: 0; margin-inline-end: 6px; }
      #${buttonId}:hover { background: rgba(127, 127, 127, .14); opacity: 1; }
      #${buttonId}:active { transform: translateY(1px); }
      #${buttonId}:focus-visible { outline: 2px solid rgba(139, 151, 255, .72); outline-offset: 2px; }
      #${buttonId} svg { display: block; width: 19px; height: 19px; fill: none; stroke: currentColor; stroke-width: 22; stroke-linecap: round; stroke-linejoin: round; }
      #${buttonId} .codey-settings-label { position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0; }
    `;
    document.documentElement.appendChild(style);
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

  const isVisibleMountTarget = (element) => {
    if (!(element instanceof HTMLElement)) return false;
    if (element.closest("[hidden], [aria-hidden=true]")) return false;
    const style = window.getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && rect.width > 0
      && rect.height > 0;
  };

  const findHeaderMount = () => {
    const header = [...document.querySelectorAll("header")].find(isVisibleMountTarget)
      || [...document.querySelectorAll("nav")].find(isVisibleMountTarget)
      || (isVisibleMountTarget(document.querySelector("main")?.firstElementChild)
        ? document.querySelector("main").firstElementChild
        : null);
    if (!header) return null;

    const controls = [...header.querySelectorAll("button, [role=button], a[href]")]
      .filter((control) => control.id !== buttonId && isVisibleMountTarget(control));
    const rightmostControl = controls.reduce((rightmost, control) => (
      !rightmost || control.getBoundingClientRect().right > rightmost.getBoundingClientRect().right
        ? control
        : rightmost
    ), null);
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

  const mountButton = () => {
    addStyle();
    const mount = findHeaderMount();
    if (!mount) return;
    let button = document.getElementById(buttonId);
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

  scan();

  bootstrapObserver = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      const target = mutation.target instanceof HTMLElement
        ? mutation.target
        : mutation.target?.parentElement;
      if (mutation.type === "attributes") {
        if (target?.matches?.(headerSelector) || target?.matches?.(sidebarSelector)) {
          scheduleScan(target);
          return;
        }
        continue;
      }
      for (const node of mutation.addedNodes || []) {
        const element = node instanceof HTMLElement ? node : null;
        if (!element) continue;
        if (
          element.matches?.(headerSelector)
          || element.querySelector?.(headerSelector)
          || element.matches?.(sidebarSelector)
          || element.querySelector?.(sidebarSelector)
        ) {
          scheduleScan(element);
          return;
        }
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

  window.addEventListener?.("focus", () => scan());
  window.addEventListener?.("pageshow", () => scan());
})();
