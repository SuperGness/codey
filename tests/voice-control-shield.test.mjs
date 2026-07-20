import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const template = readFileSync(
  new URL("../public/voice-control-shield.js", import.meta.url),
  "utf8",
);

class FakeElement {
  constructor(text = "") {
    this.textContent = text;
    this.attributes = new Map();
    this.disabled = false;
    this.style = {
      setProperty: (name, value, priority) => {
        this.style[name] = `${value}:${priority}`;
      },
    };
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }

  closest() {
    return this;
  }
}

function loadShield(enabled) {
  const semantic = new FakeElement();
  semantic.__reactProps$test = {
    children: { props: { id: "codex.command.composer.startDictation" } },
  };
  const settings = new FakeElement();
  settings.__reactFiber$test = {
    memoizedProps: { id: "settings.general.globalDictationHotkey.label" },
  };
  const localized = new FakeElement("开始听写");
  const unrelated = new FakeElement("打开设置");
  const controls = [semantic, settings, localized, unrelated];
  const listeners = new Map();
  const document = {
    querySelectorAll: () => controls,
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const mediaCalls = [];
  const fetchCalls = [];
  const webSocketCalls = [];
  const nativeGetUserMedia = (constraints) => {
    mediaCalls.push(constraints);
    return Promise.resolve("native-media");
  };
  const nativeFetch = (input) => {
    fetchCalls.push(input);
    return Promise.resolve("native-fetch");
  };
  class NativeWebSocket {
    constructor(url) {
      this.url = url;
      webSocketCalls.push(url);
    }
  }
  const window = {
    fetch: nativeFetch,
    navigator: { mediaDevices: { getUserMedia: nativeGetUserMedia } },
    WebSocket: NativeWebSocket,
  };
  window.window = window;
  vm.runInNewContext(
    template.replace("__CODEY_SLIM_VOICE__", enabled ? "true" : "false"),
    { document, Element: FakeElement, HTMLElement: FakeElement, URL, window },
  );
  return {
    fetchCalls,
    listeners,
    localized,
    mediaCalls,
    nativeFetch,
    nativeGetUserMedia,
    NativeWebSocket,
    semantic,
    settings,
    unrelated,
    webSocketCalls,
    window,
  };
}

test("voice slim mode blocks composer, settings, and localized voice controls", () => {
  const runtime = loadShield(true);

  for (const control of [runtime.semantic, runtime.settings, runtime.localized]) {
    assert.equal(control.getAttribute("data-codey-voice-control-blocked"), "true");
    assert.equal(control.disabled, true);
    assert.equal(control.style.display, "none:important");
  }
  assert.equal(runtime.unrelated.getAttribute("data-codey-voice-control-blocked"), null);

  let prevented = false;
  let stopped = false;
  runtime.listeners.get("click")({
    target: runtime.semantic,
    preventDefault: () => { prevented = true; },
    stopPropagation: () => { stopped = true; },
    stopImmediatePropagation: () => {},
  });
  assert.equal(prevented, true);
  assert.equal(stopped, true);
});

test("disabling voice slim mode restores native voice controls", () => {
  const runtime = loadShield(false);

  assert.equal(runtime.window.__codeyVoiceControlShield.enabled, false);
  assert.equal(runtime.window.__codeyVoiceControlShield.resourceGuardsInstalled, 0);
  assert.equal(runtime.semantic.getAttribute("data-codey-voice-control-blocked"), null);
  assert.equal(runtime.localized.getAttribute("data-codey-voice-control-blocked"), null);
  assert.equal(runtime.window.__codeyBlockNativeVoiceControls(), 0);
  assert.equal(runtime.window.navigator.mediaDevices.getUserMedia, runtime.nativeGetUserMedia);
  assert.equal(runtime.window.fetch, runtime.nativeFetch);
  assert.equal(runtime.window.WebSocket, runtime.NativeWebSocket);
});

test("voice slim mode blocks audio capture and dictation network resources", async () => {
  const runtime = loadShield(true);

  await assert.rejects(
    runtime.window.navigator.mediaDevices.getUserMedia({ audio: true }),
    (error) => error?.name === "NotAllowedError",
  );
  assert.deepEqual(runtime.mediaCalls, []);

  assert.equal(
    await runtime.window.navigator.mediaDevices.getUserMedia({ video: true }),
    "native-media",
  );
  assert.deepEqual(runtime.mediaCalls, [{ video: true }]);

  await assert.rejects(
    runtime.window.fetch("https://chatgpt.com/backend-api/codex/dictation-stream-connect-info"),
    (error) => error?.name === "NotAllowedError",
  );
  assert.equal(await runtime.window.fetch("https://chatgpt.com/backend-api/models"), "native-fetch");
  assert.deepEqual(runtime.fetchCalls, ["https://chatgpt.com/backend-api/models"]);

  assert.throws(
    () => new runtime.window.WebSocket("wss://chatgpt.com/dictation/stream"),
    (error) => error?.name === "NotAllowedError",
  );
  const socket = new runtime.window.WebSocket("wss://chatgpt.com/other-stream");
  assert.equal(socket.url, "wss://chatgpt.com/other-stream");
  assert.deepEqual(runtime.webSocketCalls, ["wss://chatgpt.com/other-stream"]);
  assert.equal(runtime.window.__codeyVoiceControlShield.resourceGuardsInstalled, 3);

  runtime.window.__codeyVoiceControlShieldCleanup();
  assert.equal(runtime.window.navigator.mediaDevices.getUserMedia, runtime.nativeGetUserMedia);
  assert.equal(runtime.window.fetch, runtime.nativeFetch);
  assert.equal(runtime.window.WebSocket, runtime.NativeWebSocket);
});
