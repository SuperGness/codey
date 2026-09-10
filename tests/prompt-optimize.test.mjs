import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { flushTimers } from "./helpers/flush.mjs";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(
  new URL("../public/prompt-optimize.js", import.meta.url),
  "utf8",
);

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", { visible = true, rect = null } = {}) {
    super(tagName);
    this.visible = visible;
    this.value = "";
    this.innerText = "";
    this.disabled = false;
    this.isContentEditable = false;
    this.readOnly = false;
    this.offsetWidth = 0;
    this.rect = rect;
  }

  closest(selector) {
    const selectors = String(selector)
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean);
    let element = this;
    while (element) {
      const matches = selectors.some((candidate) => {
        if (candidate.startsWith("#")) {
          return element.id === candidate.slice(1);
        }
        const attribute = candidate.match(
          /^\[([^=\]]+)(?:=['"]?([^'"\]]+)['"]?)?\]$/,
        );
        if (attribute) {
          const actual = element.getAttribute(attribute[1]);
          return attribute[2] === undefined
            ? actual !== null
            : actual === attribute[2];
        }
        return element.tagName === candidate.toUpperCase();
      });
      if (matches) return element;
      element = element.parentElement;
    }
    return null;
  }

  querySelectorAll() {
    return [];
  }

  getBoundingClientRect() {
    if (this.visible && this.rect) return { ...this.rect };
    return this.visible
      ? {
          bottom: 300,
          height: 120,
          left: 100,
          right: 800,
          top: 160,
          width: 700,
        }
      : { bottom: 0, height: 0, left: 0, right: 0, top: 0, width: 0 };
  }

  setAttribute(name, value) {
    super.setAttribute(name, value);
    if (name === "contenteditable") {
      this.isContentEditable = String(value) === "true";
    }
  }
}

let latestMutationObserver = null;

class FakeMutationObserver {
  constructor(callback) {
    this.callback = callback;
    this.observed = false;
    this.observeCalls = 0;
    this.disconnectCalls = 0;
    latestMutationObserver = this;
  }

  observe() {
    this.observed = true;
    this.observeCalls += 1;
  }

  disconnect() {
    this.observed = false;
    this.disconnectCalls += 1;
  }
}

class FakeEvent {
  constructor(type, init = {}) {
    this.type = type;
    this.bubbles = init.bubbles ?? false;
    this.target = null;
  }
}

const flush = flushTimers;

const createEnvironment = (options = {}) => {
  const calls = [];
  const inputEvents = [];
  const documentListeners = new Map();
  const windowListeners = new Map();
  const statusEvents = [];
  let config = {
    promptOptimization: {
      enabled: options.enabled ?? true,
      apiKeyConfigured: options.apiKeyConfigured ?? true,
      mode: options.mode ?? "manual",
    },
  };
  let composerQueryCount = 0;
  const optimizeResult = options.optimizeResult ?? {
    optimized: "优化后的提示词",
  };

  const documentElement = new FakeElement("html");
  const body = new FakeElement("body");
  const anchor = new FakeElement("div");
  anchor.setAttribute(
    "data-above-composer-conversation-id",
    options.conversationId ?? "conversation-1",
  );
  const scope = new FakeElement("div");
  const textarea = new FakeElement("textarea");
  textarea.value = options.initialText ?? "";
  const newChatInput = new FakeElement("div");
  newChatInput.setAttribute("contenteditable", "true");
  newChatInput.setAttribute("role", "textbox");
  newChatInput.innerText = options.newChatInitialText ?? "";
  const composerInnerControl = new FakeElement("button", {
    rect: {
      bottom: 290,
      height: 30,
      left: 650,
      right: 790,
      top: 260,
      width: 140,
    },
  });
  composerInnerControl.textContent = "model-2026 链接";
  composerInnerControl.setAttribute("aria-haspopup", "menu");
  const toolbar = new FakeElement("div");
  const accessButton = new FakeElement("button", {
    rect: {
      bottom: 290,
      height: 36,
      left: 120,
      right: 240,
      top: 254,
      width: 120,
    },
  });
  accessButton.textContent = "完全访问";
  const modelButton = new FakeElement("button", {
    rect: {
      bottom: 290,
      height: 36,
      left: 560,
      right: 720,
      top: 254,
      width: 160,
    },
  });
  modelButton.textContent = "5.6 Sol 极高";
  modelButton.setAttribute("aria-haspopup", "menu");
  const microphoneButton = new FakeElement("button", {
    rect: {
      bottom: 290,
      height: 36,
      left: 730,
      right: 766,
      top: 254,
      width: 36,
    },
  });
  const sendButton = new FakeElement("button", {
    rect: {
      bottom: 290,
      height: 36,
      left: 776,
      right: 812,
      top: 254,
      width: 36,
    },
  });
  const dialog = new FakeElement("div");
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-modal", "true");
  const dialogInput = new FakeElement("textarea", {
    rect: {
      bottom: 700,
      height: 180,
      left: 120,
      right: 920,
      top: 520,
      width: 800,
    },
  });
  dialogInput.value = options.dialogInitialText ?? "Git 提交信息";
  const dialogToolbar = new FakeElement("div");
  const dialogControl = new FakeElement("button", {
    rect: {
      bottom: 690,
      height: 36,
      left: 960,
      right: 1160,
      top: 654,
      width: 200,
    },
  });
  dialogControl.textContent = "提交并推送";
  dialogControl.setAttribute("aria-haspopup", "menu");
  const slashCommandList = new FakeElement("div");
  slashCommandList.setAttribute("role", "listbox");
  const slashModelCommand = new FakeElement("button", {
    rect: {
      bottom: 140,
      height: 44,
      left: 120,
      right: 1160,
      top: 96,
      width: 1040,
    },
  });
  slashModelCommand.textContent = "模型";
  const slashGoalCommand = new FakeElement("button", {
    rect: {
      bottom: 96,
      height: 44,
      left: 120,
      right: 1160,
      top: 52,
      width: 1040,
    },
  });
  slashGoalCommand.textContent = "目标";
  const settingsPanel = new FakeElement("section");
  settingsPanel.setAttribute("data-testid", "settings-panel");
  const settingsInput = new FakeElement("div", {
    rect: {
      bottom: 300,
      height: 120,
      left: 100,
      right: 800,
      top: 180,
      width: 700,
    },
  });
  settingsInput.setAttribute("contenteditable", "true");
  settingsInput.setAttribute("role", "textbox");
  settingsInput.innerText = options.settingsInitialText ?? "自定义语气";
  const settingsToneControl = new FakeElement("button", {
    rect: {
      bottom: 290,
      height: 36,
      left: 980,
      right: 1120,
      top: 254,
      width: 140,
    },
  });
  settingsToneControl.textContent = "亲和";
  settingsToneControl.setAttribute("aria-haspopup", "menu");
  settingsPanel.appendChild(settingsInput);
  settingsPanel.appendChild(settingsToneControl);
  slashCommandList.appendChild(slashModelCommand);
  slashCommandList.appendChild(slashGoalCommand);
  let slashCommandsOpen = options.slashCommands === true;
  const includeComposer = options.composerDom !== false;
  documentElement.appendChild(body);
  body.appendChild(scope);
  if (includeComposer) {
    scope.appendChild(anchor);
    scope.appendChild(textarea);
    scope.appendChild(newChatInput);
    scope.appendChild(toolbar);
    toolbar.appendChild(accessButton);
    toolbar.appendChild(modelButton);
    toolbar.appendChild(microphoneButton);
    toolbar.appendChild(sendButton);
    if (options.composerInnerControl) {
      newChatInput.appendChild(composerInnerControl);
    }
  }
  if (slashCommandsOpen) {
    scope.appendChild(slashCommandList);
  }
  if (options.settingsPanel) {
    scope.appendChild(settingsPanel);
  }
  if (options.dialogComposer || options.dialogControl) {
    scope.appendChild(dialog);
  }
  if (options.dialogComposer) {
    dialog.appendChild(dialogInput);
  }
  if (options.dialogControl) {
    dialog.appendChild(dialogToolbar);
    dialogToolbar.appendChild(dialogControl);
  }
  let fallbackInputs = options.newChatComposer
    ? [newChatInput]
    : includeComposer
      ? [textarea]
      : [];
  if (options.settingsPanel) {
    fallbackInputs = options.onlySettingsPanel
      ? [settingsInput]
      : [...fallbackInputs, settingsInput];
  }
  if (options.dialogComposer) {
    fallbackInputs = options.onlyDialogComposer
      ? [dialogInput]
      : [...fallbackInputs, dialogInput];
  }
  scope.querySelectorAll = (selector) => {
    if (selector === "textarea, [contenteditable='true'], [role='textbox']") {
      return includeComposer ? [textarea] : [];
    }
    if (selector === "button, [role='button']") {
      const controls = includeComposer
        ? [accessButton, modelButton, microphoneButton, sendButton]
        : [];
      if (options.composerInnerControl) controls.push(composerInnerControl);
      if (options.dialogControl) controls.push(dialogControl);
      if (options.settingsPanel) controls.push(settingsToneControl);
      if (slashCommandsOpen) {
        controls.push(slashModelCommand, slashGoalCommand);
      }
      return controls;
    }
    return [];
  };

  const findById = (root, id) => {
    if (!root || typeof root.children?.forEach !== "function") return null;
    if (root.id === id) return root;
    for (const child of root.children) {
      const found = findById(child, id);
      if (found) return found;
    }
    return null;
  };

  const document = {
    body,
    documentElement,
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: (id) => findById(documentElement, id),
    querySelector: () => null,
    querySelectorAll: (selector) => {
      if (selector === "[data-above-composer-conversation-id]") {
        composerQueryCount += 1;
        return options.anchors === false ? [] : [anchor];
      }
      if (
        selector ===
        "main textarea, main [contenteditable='true'], main [role='textbox'], textarea, [contenteditable='true'][role='textbox']"
      ) {
        return options.fallbackTextareas === false ? [] : fallbackInputs;
      }
      return [];
    },
    addEventListener(type, handler) {
      const handlers = documentListeners.get(type) || [];
      handlers.push(handler);
      documentListeners.set(type, handlers);
    },
  };

  const window = {
    innerHeight: 800,
    innerWidth: 1280,
    location: { href: options.locationHref ?? "codex://conversation/1" },
    __codeyInjectionStatus: {
      "prompt-optimize": { status: "executed", detail: null, error: null },
    },
    addEventListener(type, handler) {
      const handlers = windowListeners.get(type) || [];
      handlers.push(handler);
      windowListeners.set(type, handlers);
    },
    CustomEvent: class {
      constructor(type, init = {}) {
        this.type = type;
        this.detail = init.detail;
      }
    },
    dispatchEvent(event) {
      statusEvents.push(event);
      return true;
    },
    getComputedStyle: () => ({ display: "block", visibility: "visible" }),
  };

  const testSetTimeout = (callback, delay, ...args) => {
    const timer = setTimeout(callback, delay, ...args);
    timer.unref?.();
    return timer;
  };
  const sandbox = {
    document,
    window,
    MutationObserver: FakeMutationObserver,
    Event: FakeEvent,
    InputEvent: FakeEvent,
    setTimeout: testSetTimeout,
    clearTimeout,
    HTMLElement: class HTMLElement {},
    HTMLTextAreaElement: class HTMLTextAreaElement {},
  };
  const bridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/settings/get") return config;
    if (path === "/api/optimize_prompt") return optimizeResult;
    return {};
  };
  if (options.bridgeReady !== false) {
    sandbox.window.__codexSessionDeleteBridge = bridge;
  }
  const context = vm.createContext(sandbox);
  vm.runInContext(source, context);
  const installedObserver = latestMutationObserver;

  return {
    anchor,
    calls,
    dialog,
    dialogControl,
    dialogInput,
    inputEvents,
    injectionStatus: window.__codeyInjectionStatus["prompt-optimize"],
    statusEvents,
    newChatInput,
    composerInnerControl,
    settingsPanel,
    settingsInput,
    settingsToneControl,
    textarea,
    toolbar,
    accessButton,
    modelButton,
    slashCommandList,
    scope,
    getElementById: (id) => findById(documentElement, id),
    getComposerQueryCount: () => composerQueryCount,
    getObserverState: () => ({
      active: installedObserver?.observed === true,
      disconnectCalls: installedObserver?.disconnectCalls ?? 0,
      observeCalls: installedObserver?.observeCalls ?? 0,
    }),
    snapshot: () => context.window.__codeyPromptOptimize.snapshot(),
    setConfig: (next) => {
      config = next;
    },
    setBridgeReady: () => {
      context.window.__codexSessionDeleteBridge = bridge;
    },
    emitConfigChanged: () => {
      for (const handler of windowListeners.get("codey:config-changed") || []) {
        handler.call(window);
      }
    },
    emitMutation: (mutations = [{ type: "childList", target: documentElement }]) => {
      if (installedObserver?.observed) installedObserver.callback(mutations);
    },
    emitInput: (target = textarea) => {
      for (const handler of documentListeners.get("input") || []) {
        handler.call(document, { type: "input", target });
      }
    },
    setFallbackInputs: (inputs) => {
      fallbackInputs = inputs;
    },
    openSlashCommands: () => {
      if (slashCommandsOpen) return;
      slashCommandsOpen = true;
      scope.appendChild(slashCommandList);
    },
    setLocationHref: (href) => {
      window.location.href = href;
    },
  };
};

test("retries config loading when the bridge becomes ready after injection", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    bridgeReady: false,
  });
  env.setBridgeReady();
  await new Promise((resolve) => setTimeout(resolve, 180));

  assert.ok(env.getElementById("codey-prompt-optimize-button"));
  assert.equal(env.snapshot().ready, true);
});

test("mounts the optimize button when enabled and an API key is configured", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: true });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button, "button should be mounted");
  assert.equal(button.dataset.codeyPromptOptimize, "true");
  assert.equal(button.dataset.codeyPromptOptimizeLayout, "model-picker");
  assert.equal(button.style.display, "inline-flex");
  assert.equal(button.disabled, true);
  assert.equal(button.getAttribute("aria-disabled"), "true");
  assert.equal(button.parentElement, env.toolbar);
  assert.deepEqual(env.toolbar.children.slice(0, 3), [
    env.accessButton,
    button,
    env.modelButton,
  ]);
  assert.equal(env.snapshot().enabled, true);
  assert.equal(env.snapshot().ready, true);
  assert.equal(env.snapshot().buttonDisabled, true);
  assert.deepEqual(env.getObserverState(), {
    active: true,
    disconnectCalls: 0,
    observeCalls: 1,
  });
});

test("mounts the optimize button for an enabled Codey route without a manual key", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: false,
    mode: "codeyRoute",
  });
  await flush();

  assert.ok(env.getElementById("codey-prompt-optimize-button"));
  assert.equal(env.snapshot().enabled, true);
});

test("keeps the original dark treatment at a 26px height", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: true });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  const style = env.getElementById("codey-prompt-optimize-style");
  assert.match(style.textContent, /height: 26px !important/);
  assert.match(style.textContent, /background: rgba\(30, 30, 30, \.92\)/);
  assert.doesNotMatch(style.textContent, /--codey-ai-/);
  assert.doesNotMatch(button.innerHTML, /codey-prompt-optimize-ai-gradient/);
});

test("enables the optimize button only while the composer has content", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: true });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");

  env.textarea.value = "   ";
  env.emitInput();
  assert.equal(button.disabled, true);

  env.textarea.value = "需要优化的提示词";
  env.emitInput();
  assert.equal(button.disabled, false);
  assert.equal(button.getAttribute("aria-disabled"), "false");

  env.textarea.value = "";
  env.emitInput();
  assert.equal(button.disabled, true);
});

test("does not mount the button when the feature is disabled", async () => {
  const env = createEnvironment({ enabled: false, apiKeyConfigured: true });
  await flush();

  assert.equal(env.getElementById("codey-prompt-optimize-button"), null);
  assert.equal(env.snapshot().enabled, false);
  assert.deepEqual(env.getObserverState(), {
    active: false,
    disconnectCalls: 0,
    observeCalls: 0,
  });
});

test("disconnects the document observer when prompt optimization is turned off", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: true });
  await flush();
  assert.equal(env.getObserverState().active, true);

  env.setConfig({
    promptOptimization: { enabled: false, apiKeyConfigured: true },
  });
  env.emitConfigChanged();
  await flush();

  const scansAfterDisable = env.getComposerQueryCount();
  assert.deepEqual(env.getObserverState(), {
    active: false,
    disconnectCalls: 1,
    observeCalls: 1,
  });
  assert.equal(
    env.getElementById("codey-prompt-optimize-button").style.display,
    "none",
  );
  env.emitMutation();
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.equal(env.getComposerQueryCount(), scansAfterDisable);
});

test("does not mount the button when no API key is configured yet", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: false });
  await flush();

  assert.equal(env.getElementById("codey-prompt-optimize-button"), null);
});

test("keeps the button hidden when no composer input is found", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    fallbackTextareas: false,
  });
  await flush();

  assert.equal(env.getElementById("codey-prompt-optimize-button"), null);
});

test("does not mount the button for settings textboxes", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    composerDom: false,
    settingsPanel: true,
    onlySettingsPanel: true,
  });
  await flush();

  assert.equal(env.getElementById("codey-prompt-optimize-button"), null);
  assert.equal(env.snapshot().hasInput, false);
});

test("does not use settings controls as insertion targets", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    settingsPanel: true,
  });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button, "the normal composer should still receive the button");
  assert.equal(button.parentElement, env.toolbar);
  assert.equal(env.settingsToneControl.parentElement, env.settingsPanel);
  assert.equal(env.settingsToneControl.parentElement.contains(button), false);
});

test("ignores Git commit textboxes inside modal dialogs", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    dialogComposer: true,
    initialText: "正常对话提示词",
  });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button, "the normal composer should still receive the button");
  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await flush();

  const optimizeCall = env.calls.find(
    (call) => call.path === "/api/optimize_prompt",
  );
  assert.equal(optimizeCall?.payload.text, "正常对话提示词");
  assert.equal(env.textarea.value, "优化后的提示词");
  assert.equal(env.dialogInput.value, "Git 提交信息");
});

test("does not use controls inside modal dialogs as insertion targets", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    dialogControl: true,
  });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button);
  assert.equal(button.parentElement, env.toolbar);
  assert.equal(env.dialogControl.parentElement.parentElement, env.dialog);
});

test("keeps the optimize button in the composer when slash commands open", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: true });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");
  assert.equal(button.parentElement, env.toolbar);

  env.openSlashCommands();
  env.emitMutation([
    {
      type: "childList",
      target: env.scope,
      addedNodes: [env.slashCommandList],
      removedNodes: [],
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  assert.equal(button.parentElement, env.toolbar);
  assert.equal(env.slashCommandList.contains(button), false);
});

test("mounts the optimize button for a new-chat contenteditable composer", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    newChatComposer: true,
  });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button, "new-chat composer should receive the optimize button");
  assert.equal(button.style.display, "inline-flex");
  assert.equal(button.getAttribute("contenteditable"), "false");
  assert.equal(env.snapshot().hasInput, true);
});

test("does not insert the optimize button into contenteditable link controls", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    newChatComposer: true,
    newChatInitialText: "第一段\n第二段\nhttps://example.com/model-2026",
    composerInnerControl: true,
  });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button, "contenteditable composer should still receive the button");
  assert.equal(button.parentElement, env.toolbar);
  assert.equal(env.newChatInput.contains(button), false);
  assert.equal(env.composerInnerControl.parentElement, env.newChatInput);
});

test("removes the optimize button if rich paste moves its toolbar into the contenteditable composer", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    newChatComposer: true,
    newChatInitialText: "第一段\n第二段\nhttps://example.com/model-2026",
  });
  await flush();

  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button, "button starts in the safe composer toolbar");
  assert.equal(button.parentElement, env.toolbar);
  assert.equal(env.newChatInput.contains(button), false);

  env.newChatInput.appendChild(env.toolbar);
  env.emitMutation([
    {
      type: "childList",
      target: env.newChatInput,
      addedNodes: [env.toolbar],
      removedNodes: [],
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  assert.equal(env.getElementById("codey-prompt-optimize-button"), null);
  assert.equal(env.newChatInput.contains(button), false);
  assert.equal(env.snapshot().hasButton, false);
});

test("rescans when a connected composer is replaced during navigation", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
  });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");
  assert.ok(button);

  env.textarea.visible = false;
  const nextInput = new FakeElement("div");
  nextInput.setAttribute("contenteditable", "true");
  nextInput.setAttribute("role", "textbox");
  nextInput.innerText = "新对话里的提示词";
  env.scope.appendChild(nextInput);
  env.setFallbackInputs([env.textarea, nextInput]);
  env.emitMutation([
    {
      type: "childList",
      target: env.scope,
      addedNodes: [nextInput],
      removedNodes: [],
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await flush();

  assert.equal(nextInput.innerText, "优化后的提示词");
  assert.equal(env.textarea.value, "");
});

test("ignores unrelated DOM mutations while the composer remains connected", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
  });
  await flush();
  const queryCount = env.getComposerQueryCount();
  const unrelated = new FakeElement("aside");

  env.emitMutation([
    {
      type: "attributes",
      target: unrelated,
      attributeName: "class",
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  assert.equal(env.getComposerQueryCount(), queryCount);
});

test("clicking the button calls the bridge and replaces the composer text", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    initialText: "写一个关于 Rust 的博客",
  });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");
  assert.equal(button.disabled, false);

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await flush();

  const optimizeCall = env.calls.find(
    (call) => call.path === "/api/optimize_prompt",
  );
  assert.ok(
    optimizeCall,
    "optimize_prompt should be called through the bridge",
  );
  assert.equal(optimizeCall.payload.text, "写一个关于 Rust 的博客");
  assert.equal(env.textarea.value, "优化后的提示词");
  assert.equal(button.dataset.busy, "false");
});

test("shows a disabled loading state while optimization is pending", async () => {
  let resolveOptimization;
  const optimizeResult = new Promise((resolve) => {
    resolveOptimization = resolve;
  });
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    initialText: "原始提示词",
    optimizeResult,
  });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  assert.equal(button.disabled, true);
  assert.equal(button.dataset.busy, "true");
  assert.equal(button.getAttribute("aria-busy"), "true");
  assert.equal(env.snapshot().buttonBusy, true);

  resolveOptimization({ optimized: "优化完成" });
  await flush();

  assert.equal(button.disabled, false);
  assert.equal(button.dataset.busy, "false");
  assert.equal(button.getAttribute("aria-busy"), "false");
  assert.equal(env.textarea.value, "优化完成");
});

test("restores a pending optimization to its original composer after navigation", async () => {
  let resolveOptimization;
  const optimizeResult = new Promise((resolve) => {
    resolveOptimization = resolve;
  });
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    anchors: false,
    initialText: "旧会话提示词",
    optimizeResult,
  });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  env.setLocationHref("codex://conversation/2");
  env.textarea.visible = false;
  const nextInput = new FakeElement("div");
  nextInput.setAttribute("contenteditable", "true");
  nextInput.setAttribute("role", "textbox");
  nextInput.innerText = "新会话提示词";
  env.scope.appendChild(nextInput);
  env.setFallbackInputs([env.textarea, nextInput]);
  env.emitMutation([
    {
      type: "childList",
      target: env.scope,
      addedNodes: [nextInput],
      removedNodes: [],
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  resolveOptimization({ optimized: "旧会话优化结果" });
  await flush();

  assert.equal(nextInput.innerText, "新会话提示词");
  assert.equal(env.textarea.value, "旧会话提示词");
  assert.equal(button.dataset.busy, "false");

  env.setLocationHref("codex://conversation/1");
  nextInput.visible = false;
  env.textarea.visible = true;
  env.setFallbackInputs([nextInput, env.textarea]);
  env.emitMutation([
    {
      type: "childList",
      target: env.scope,
      addedNodes: [],
      removedNodes: [],
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  assert.equal(env.textarea.value, "旧会话优化结果");
  assert.equal(nextInput.innerText, "新会话提示词");
});

test("restores a pending optimization when the original conversation reuses a composer", async () => {
  let resolveOptimization;
  const optimizeResult = new Promise((resolve) => {
    resolveOptimization = resolve;
  });
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    initialText: "相同提示词",
    optimizeResult,
  });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  env.anchor.setAttribute(
    "data-above-composer-conversation-id",
    "conversation-2",
  );

  resolveOptimization({ optimized: "旧会话优化结果" });
  await flush();

  assert.equal(env.textarea.value, "相同提示词");
  assert.equal(button.dataset.busy, "false");

  env.anchor.setAttribute(
    "data-above-composer-conversation-id",
    "conversation-1",
  );
  env.emitMutation([
    {
      type: "attributes",
      target: env.anchor,
      attributeName: "data-above-composer-conversation-id",
    },
  ]);
  await new Promise((resolve) => setTimeout(resolve, 280));

  assert.equal(env.textarea.value, "旧会话优化结果");
});

test("failed optimization keeps the original text and uses the global toast", async () => {
  const env = createEnvironment({
    enabled: true,
    apiKeyConfigured: true,
    initialText: "原文",
    optimizeResult: { status: "failed", message: "API Key 无效" },
  });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await flush();

  assert.equal(env.textarea.value, "原文");
  const toast = env.getElementById("codey-runtime-toast");
  assert.ok(toast, "global error toast should be created");
  assert.equal(toast.dataset.tone, "error");
  assert.equal(toast.getAttribute("role"), "alert");
  assert.equal(toast.textContent, "API Key 无效");
});

test("an empty composer keeps the button disabled without showing an error", async () => {
  const env = createEnvironment({ enabled: true, apiKeyConfigured: true });
  await flush();
  const button = env.getElementById("codey-prompt-optimize-button");

  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await flush();

  assert.equal(
    env.calls.some((call) => call.path === "/api/optimize_prompt"),
    false,
  );
  assert.equal(button.disabled, true);
  assert.equal(env.getElementById("codey-runtime-toast"), null);
});

test("re-applies the switch when the console saves config", async () => {
  const env = createEnvironment({ enabled: false, apiKeyConfigured: true });
  await flush();
  assert.equal(env.getElementById("codey-prompt-optimize-button"), null);
  assert.equal(env.injectionStatus.status, "inactive");

  env.setConfig({
    promptOptimization: { enabled: true, apiKeyConfigured: true },
  });
  env.emitConfigChanged();
  await flush();

  assert.ok(env.getElementById("codey-prompt-optimize-button"));
  assert.equal(env.snapshot().enabled, true);
  assert.equal(env.injectionStatus.status, "effective");
  assert.deepEqual(
    { ...env.statusEvents.at(-1).detail },
    { id: "prompt-optimize", status: "effective" },
  );
  assert.equal(env.getObserverState().active, true);
  assert.equal(env.getObserverState().observeCalls, 1);
});
