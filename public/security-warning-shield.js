(() => {
  if (window.__codeySecurityWarningShieldInstalled) return;
  window.__codeySecurityWarningShieldInstalled = true;

  const configEventName = "codey:config-changed";
  const dismissedAttribute = "data-codey-security-warning-dismissed";
  const actionPatterns = [
    /^hide from this session$/i,
    /^(?:在|于)?本次会话(?:中)?(?:隐藏|不再显示)$/,
    /^(?:隐藏|不再显示)(?:本次会话)?$/,
  ];
  const titlePatterns = [
    /full access is on/i,
    /完全访问权限.*(?:已开启|开启中|已打开)/,
  ];
  const riskPatterns = [
    /without your permission/i,
    /prompt injection/i,
    /未经(?:你|您)的许可/,
    /提示词?注入/,
  ];
  let enabled = false;
  let scanTimer = 0;

  const normalizedText = (element) => String(
    element?.innerText || element?.textContent || "",
  ).replace(/\s+/g, " ").trim();

  const matchesAny = (value, patterns) => patterns.some((pattern) => pattern.test(value));

  const warningContainerFor = (control) => {
    let candidate = control?.parentElement || null;
    for (let depth = 0; candidate && depth < 8; depth += 1) {
      if (candidate === document.body || candidate === document.documentElement) break;
      const text = normalizedText(candidate);
      if (matchesAny(text, titlePatterns) && matchesAny(text, riskPatterns)) {
        return candidate;
      }
      candidate = candidate.parentElement;
    }
    return null;
  };

  const actionControls = (root = document) => {
    const controls = [];
    if (root instanceof Element && root.matches?.("button, [role=button]")) {
      controls.push(root);
    }
    if (typeof root?.querySelectorAll === "function") {
      controls.push(...root.querySelectorAll("button, [role=button]"));
    }
    return controls;
  };

  const dismissWarnings = (root = document) => {
    if (!enabled) return 0;
    let dismissed = 0;
    for (const control of actionControls(root)) {
      if (
        control.disabled
        || control.getAttribute?.(dismissedAttribute) === "true"
        || !matchesAny(normalizedText(control), actionPatterns)
      ) {
        continue;
      }
      const container = warningContainerFor(control);
      if (!container) continue;
      control.setAttribute?.(dismissedAttribute, "true");
      container.setAttribute?.(dismissedAttribute, "true");
      control.click?.();
      if (container.isConnected !== false) {
        container.style?.setProperty?.("display", "none", "important");
      }
      dismissed += 1;
    }
    return dismissed;
  };

  const setEnabled = (next) => {
    enabled = next === true;
    if (enabled) dismissWarnings();
    return enabled;
  };

  const refreshConfig = async () => {
    if (typeof window.__codexSessionDeleteBridge !== "function") {
      return setEnabled(false);
    }
    try {
      const config = await window.__codexSessionDeleteBridge("/settings/get", {});
      return setEnabled(config?.hideFullAccessWarning === true);
    } catch {
      return setEnabled(false);
    }
  };

  const scheduleScan = () => {
    if (!enabled || scanTimer) return;
    scanTimer = window.setTimeout(() => {
      scanTimer = 0;
      dismissWarnings();
    }, 40);
  };

  const observer = new MutationObserver((mutations) => {
    if (!enabled) return;
    if (mutations.some((mutation) => (mutation.addedNodes?.length || 0) > 0)) {
      scheduleScan();
    }
  });
  observer.observe(document.documentElement, { childList: true, subtree: true });

  window.addEventListener?.(configEventName, (event) => {
    const config = event?.detail?.config || event?.detail;
    if (config && typeof config.hideFullAccessWarning === "boolean") {
      setEnabled(config.hideFullAccessWarning);
    } else {
      void refreshConfig();
    }
  });
  window.addEventListener?.("focus", refreshConfig);
  window.addEventListener?.("pageshow", refreshConfig);

  window.__codeySecurityWarningShield = {
    get enabled() {
      return enabled;
    },
    dismissWarnings,
    refreshConfig,
    setEnabled,
  };

  void refreshConfig();
})();
