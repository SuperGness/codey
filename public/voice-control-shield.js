(() => {
  window.__codeyVoiceControlShieldCleanup?.();

  const enabled = "__CODEY_SLIM_VOICE__" === "true";
  const voiceControlIds = new Set([
    "codex.command.composer.startDictation",
    "codex.command.composer.startVoiceMode",
    "codex.commandDescription.composer.startDictation",
    "codex.commandDescription.composer.startVoiceMode",
    "codex.commandDescription.globalDictationHold",
    "codex.commandDescription.globalDictationToggle",
    "codex.commandMenuTitle.composer.startDictation",
    "codex.command.globalDictationHold",
    "codex.command.globalDictationToggle",
    "composer.startDictation",
    "composer.startVoiceMode",
    "globalDictationHold",
    "globalDictationToggle",
    "settings.general.globalDictationHotkey",
    "settings.general.globalDictationToggleHotkey",
    "settings.general.globalDictationKeepVisible",
    "settings.general.dictation",
    "settings.nav.voice",
    "settings.section.voice",
    "settings.general.voice",
  ]);
  const voiceControlIdPrefixes = [
    "settings.general.globalDictationHotkey.",
    "settings.general.globalDictationToggleHotkey.",
    "settings.general.globalDictation.",
    "settings.general.globalDictationHistory.",
    "settings.general.dictationDictionary.",
    "composer.dictation.",
    "settings.voice.",
  ];
  const fallbackLabelPattern = /^(?:voice|voice mode|start voice mode|open the voice control window|start or stop voice mode|dictate|dictation|start dictation|click to dictate or hold|hold(?:-| )to(?:-| )dictate|toggle dictation|global dictation|语音|语音模式|开始语音模式|打开语音控制窗口|听写|开始听写|全局听写|按住听写|切换听写|語音|語音模式|開始語音模式|開啟語音控制視窗|聽寫|開始聽寫|全域聽寫)(?:\s*[(:（].*)?$/i;
  const reactInternalKeyPattern = /^__(?:reactProps|reactFiber|reactInternalInstance)\$.*/;
  const dictationRequestPattern = /(?:\/codex\/dictation-stream-connect-info|\/dictation\/stream)(?:[/?#]|$)/i;
  const restoreResourceGuards = [];

  const disabledVoiceError = () => {
    const error = new Error("Codex voice is disabled by Codey");
    error.name = "NotAllowedError";
    return error;
  };

  if (enabled) {
    const mediaDevices = window.navigator?.mediaDevices;
    const nativeGetUserMedia = mediaDevices?.getUserMedia;
    if (typeof nativeGetUserMedia === "function") {
      const guardedGetUserMedia = function guardedGetUserMedia(constraints) {
        if (constraints?.audio) return Promise.reject(disabledVoiceError());
        return Reflect.apply(nativeGetUserMedia, mediaDevices, arguments);
      };
      try {
        mediaDevices.getUserMedia = guardedGetUserMedia;
        restoreResourceGuards.push(() => {
          if (mediaDevices.getUserMedia === guardedGetUserMedia) {
            mediaDevices.getUserMedia = nativeGetUserMedia;
          }
        });
      } catch {}
    }

    const nativeFetch = window.fetch;
    if (typeof nativeFetch === "function") {
      const guardedFetch = function guardedFetch(input) {
        const url = typeof input === "string" || input instanceof URL
          ? String(input)
          : String(input?.url ?? "");
        if (dictationRequestPattern.test(url)) return Promise.reject(disabledVoiceError());
        return Reflect.apply(nativeFetch, this, arguments);
      };
      window.fetch = guardedFetch;
      restoreResourceGuards.push(() => {
        if (window.fetch === guardedFetch) window.fetch = nativeFetch;
      });
    }

    const NativeWebSocket = window.WebSocket;
    if (typeof NativeWebSocket === "function") {
      const GuardedWebSocket = new Proxy(NativeWebSocket, {
        construct(target, argumentsList, newTarget) {
          if (dictationRequestPattern.test(String(argumentsList[0] ?? ""))) {
            throw disabledVoiceError();
          }
          return Reflect.construct(target, argumentsList, newTarget);
        },
      });
      window.WebSocket = GuardedWebSocket;
      restoreResourceGuards.push(() => {
        if (window.WebSocket === GuardedWebSocket) window.WebSocket = NativeWebSocket;
      });
    }
  }

  const isVoiceControlId = (value) =>
    voiceControlIds.has(value) ||
    voiceControlIdPrefixes.some((prefix) => value.startsWith(prefix));

  const containsVoiceControlId = (value, depth = 0, seen = new WeakSet()) => {
    if (typeof value === "string") return isVoiceControlId(value);
    if (!value || typeof value !== "object" || depth > 7 || seen.has(value)) return false;
    seen.add(value);
    for (const [key, child] of Object.entries(value)) {
      if (["return", "child", "sibling", "stateNode", "_owner"].includes(key)) continue;
      if (containsVoiceControlId(child, depth + 1, seen)) return true;
    }
    return false;
  };

  const isVoiceControl = (control) => {
    if (!(control instanceof HTMLElement)) return false;
    const descriptor = [
      control.getAttribute("aria-label"),
      control.getAttribute("title"),
      control.textContent,
    ].filter(Boolean).join(" ").replace(/\s+/g, " ").trim();
    if (fallbackLabelPattern.test(descriptor)) return true;

    return Object.keys(control)
      .filter((key) => reactInternalKeyPattern.test(key))
      .some((key) => {
        try {
          const internal = control[key];
          return containsVoiceControlId(internal?.memoizedProps ?? internal);
        } catch {
          return false;
        }
      });
  };

  const controlsWithin = (root, selector) => {
    const controls = [];
    if (root instanceof HTMLElement && root.matches?.(selector)) controls.push(root);
    if (root && typeof root.querySelectorAll === "function") {
      controls.push(...root.querySelectorAll(selector));
    }
    return controls;
  };

  const block = (root = document) => {
    if (!enabled) return 0;
    let blocked = 0;
    controlsWithin(
      root,
      "button, [role=button], [role=menuitem], [role=option], [role=switch], input, label",
    ).forEach((control) => {
      if (!isVoiceControl(control)) return;
      const fullyBlocked = control.getAttribute("data-codey-voice-control-blocked") === "true"
        && control.getAttribute("aria-hidden") === "true"
        && control.getAttribute("tabindex") === "-1"
        && control.getAttribute("inert") !== null
        && String(control.style.display || "").startsWith("none")
        && (!("disabled" in control) || control.disabled);
      if (!fullyBlocked) {
        control.setAttribute("data-codey-voice-control-blocked", "true");
        control.setAttribute("aria-hidden", "true");
        control.setAttribute("tabindex", "-1");
        control.setAttribute("inert", "");
        control.style.setProperty("display", "none", "important");
        if ("disabled" in control && !control.disabled) control.disabled = true;
      }
      blocked += 1;
    });
    return blocked;
  };

  if (!enabled) {
    window.__codeyBlockNativeVoiceControls = () => 0;
    window.__codeyVoiceControlShield = Object.freeze({
      enabled,
      block: () => 0,
      isVoiceControl,
      resourceGuardsInstalled: 0,
    });
    window.__codeyVoiceControlShieldCleanup = () => {
      delete window.__codeyBlockNativeVoiceControls;
      delete window.__codeyVoiceControlShield;
      delete window.__codeyVoiceControlShieldCleanup;
    };
    return;
  }

  const stopVoiceControlEvent = (event) => {
    const control = event.target instanceof Element
      ? event.target.closest(
          "button, [role=button], [role=menuitem], [role=option], [role=switch], input, label",
        )
      : null;
    if (!isVoiceControl(control)) return;
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation?.();
  };

  const eventNames = ["pointerdown", "click", "keydown"];
  eventNames.forEach((eventName) => {
    document.addEventListener(eventName, stopVoiceControlEvent, true);
  });
  window.__codeyBlockNativeVoiceControls = block;
  window.__codeyVoiceControlShield = Object.freeze({
    enabled,
    block,
    isVoiceControl,
    resourceGuardsInstalled: restoreResourceGuards.length,
  });
  window.__codeyVoiceControlShieldCleanup = () => {
    eventNames.forEach((eventName) => {
      document.removeEventListener(eventName, stopVoiceControlEvent, true);
    });
    restoreResourceGuards.splice(0).reverse().forEach((restore) => restore());
    delete window.__codeyBlockNativeVoiceControls;
    delete window.__codeyVoiceControlShield;
    delete window.__codeyVoiceControlShieldCleanup;
  };
  block();
})();
