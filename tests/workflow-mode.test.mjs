import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(
  new URL("../public/workflow-mode.js", import.meta.url),
  "utf8",
);

const splitSelectors = (selector) =>
  String(selector)
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);

const matchesClause = (element, rawClause) => {
  let clause = rawClause.trim();
  let requiredAncestor = "";
  const descendant = clause.match(/^([a-z][\w-]*)\s+(.+)$/i);
  if (descendant) {
    requiredAncestor = descendant[1].toUpperCase();
    clause = descendant[2];
  }
  if (clause.startsWith("#")) return element.id === clause.slice(1);

  const tag = clause.match(/^([a-z][\w-]*)/i)?.[1];
  if (tag && element.tagName !== tag.toUpperCase()) return false;
  const attributes = [...clause.matchAll(/\[([^\]=*]+)(\*=|=)?['"]?([^'"\]]*)['"]?\]/g)];
  for (const [, name, operator, expected] of attributes) {
    const actual = element.getAttribute(name.trim());
    if (!operator && actual === null) return false;
    if (operator === "=" && actual !== expected) return false;
    if (operator === "*=" && !String(actual || "").includes(expected)) return false;
  }
  if (requiredAncestor) {
    let ancestor = element.parentElement;
    while (ancestor && ancestor.tagName !== requiredAncestor) {
      ancestor = ancestor.parentElement;
    }
    if (!ancestor) return false;
  }
  return true;
};

const matchesSelector = (element, selector) =>
  splitSelectors(selector).some((clause) => matchesClause(element, clause));

class FakeEvent {
  constructor(type, init = {}) {
    this.type = type;
    Object.assign(this, init);
    this.bubbles = init.bubbles ?? true;
    this.cancelable = init.cancelable ?? true;
    this.defaultPrevented = false;
    this.propagationStopped = false;
    this.immediatePropagationStopped = false;
    this.target = init.target || null;
  }

  preventDefault() {
    if (this.cancelable) this.defaultPrevented = true;
  }

  stopPropagation() {
    this.propagationStopped = true;
  }

  stopImmediatePropagation() {
    this.immediatePropagationStopped = true;
    this.propagationStopped = true;
  }
}

class FakeElement {
  constructor(tagName, ownerDocument, options = {}) {
    this.tagName = String(tagName).toUpperCase();
    this.ownerDocument = ownerDocument;
    this.parentElement = null;
    this.children = [];
    this.attributes = new Map();
    this.dataset = {};
    this.listeners = new Map();
    this.style = {};
    this.id = "";
    this.title = "";
    this.type = "";
    this.value = options.value || "";
    this.innerText = options.innerText || "";
    this.textContent = options.textContent || "";
    this.disabled = false;
    this.isContentEditable = false;
    this.isConnected = false;
    this.files = [];
    this.rect = options.rect || {
      bottom: 300,
      height: 80,
      left: 100,
      right: 800,
      top: 220,
      width: 700,
    };
  }

  addEventListener(type, handler) {
    const handlers = this.listeners.get(type) || [];
    handlers.push(handler);
    this.listeners.set(type, handlers);
  }

  dispatchLocal(event) {
    for (const handler of [...(this.listeners.get(event.type) || [])]) {
      if (event.immediatePropagationStopped) break;
      handler.call(this, event);
    }
  }

  dispatchEvent(event) {
    if (!event.target) event.target = this;
    return this.ownerDocument.dispatchToTarget(event);
  }

  click() {
    const event = new FakeEvent("click", { target: this });
    this.ownerDocument.dispatchToTarget(event);
    return event;
  }

  setConnected(connected) {
    this.isConnected = connected;
    for (const child of this.children) child.setConnected(connected);
  }

  appendChild(child) {
    child.remove();
    child.parentElement = this;
    this.children.push(child);
    child.setConnected(this.isConnected);
    return child;
  }

  insertBefore(child, reference) {
    child.remove();
    const index = this.children.indexOf(reference);
    if (index < 0) return this.appendChild(child);
    child.parentElement = this;
    this.children.splice(index, 0, child);
    child.setConnected(this.isConnected);
    return child;
  }

  remove() {
    if (!this.parentElement) {
      this.setConnected(false);
      return;
    }
    const index = this.parentElement.children.indexOf(this);
    if (index >= 0) this.parentElement.children.splice(index, 1);
    this.parentElement = null;
    this.setConnected(false);
  }

  contains(node) {
    if (node === this) return true;
    return this.children.some((child) => child.contains(node));
  }

  closest(selector) {
    let current = this;
    while (current) {
      if (matchesSelector(current, selector)) return current;
      current = current.parentElement;
    }
    return null;
  }

  querySelectorAll(selector) {
    const matches = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (matchesSelector(child, selector)) matches.push(child);
        visit(child);
      }
    };
    visit(this);
    return matches;
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] || null;
  }

  getAttribute(name) {
    if (name === "id") return this.id || null;
    if (name === "title") return this.title || null;
    if (name === "type") return this.attributes.get(name) || this.type || null;
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }

  setAttribute(name, value) {
    const stringValue = String(value);
    this.attributes.set(name, stringValue);
    if (name === "id") this.id = stringValue;
    if (name === "title") this.title = stringValue;
    if (name === "type") this.type = stringValue;
    if (name === "contenteditable") {
      this.isContentEditable = stringValue === "true";
    }
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  getBoundingClientRect() {
    return { ...this.rect };
  }
}

class FakeMutationObserver {
  static instances = [];

  constructor(callback) {
    this.callback = callback;
    this.observed = false;
    FakeMutationObserver.instances.push(this);
  }

  observe() {
    this.observed = true;
  }

  disconnect() {
    this.observed = false;
  }
}

const defaultPermissionSnapshot = () => ({
  resolved: true,
  snapshotId: "permission-1",
  approvalPolicy: "on-request",
  sandboxMode: "workspace-write",
});

const defaultCapabilities = (overrides = {}) => ({
  status: "ok",
  enabled: true,
  schemaVersion: 1,
  protocolVersion: 1,
  bridgeHealthy: true,
  proxyHealthy: true,
  cwd: "/workspace/project",
  permissionSnapshot: defaultPermissionSnapshot(),
  capabilities: {
    composerTakeover: true,
    start: true,
    steer: true,
    bypassAudit: true,
  },
  activeWorkflow: null,
  ...overrides,
});

const durableAck = (overrides = {}) => ({
  status: "ok",
  durableAck: true,
  engineEpoch: "engine-1",
  revision: 1,
  runId: "run-1",
  ...overrides,
});

const flushTimers = () => new Promise((resolve) => setTimeout(resolve, 10));

const createEnvironment = async (options = {}) => {
  FakeMutationObserver.instances = [];
  const documentListeners = new Map();
  const windowListeners = new Map();
  const calls = [];
  let nativeSends = 0;
  let uuid = 0;
  let capabilityResponse = options.capabilities || defaultCapabilities();
  let workflowListResponse = options.workflowList || {
    engineEpoch: "engine-1",
    runs: [],
  };
  let startResponse = options.startResponse || durableAck();
  let steerResponse = options.steerResponse || durableAck();
  const workflowOpenRequests = [];

  const document = {
    documentElement: null,
    body: null,
    createElement(tagName) {
      return new FakeElement(tagName, document);
    },
    getElementById(id) {
      if (document.documentElement.id === id) return document.documentElement;
      return document.documentElement.querySelector(`#${id}`);
    },
    querySelectorAll(selector) {
      const matches = [];
      if (matchesSelector(document.documentElement, selector)) {
        matches.push(document.documentElement);
      }
      matches.push(...document.documentElement.querySelectorAll(selector));
      return matches;
    },
    querySelector(selector) {
      return document.querySelectorAll(selector)[0] || null;
    },
    addEventListener(type, handler) {
      const handlers = documentListeners.get(type) || [];
      handlers.push(handler);
      documentListeners.set(type, handlers);
    },
    dispatchToTarget(event) {
      for (const handler of [...(documentListeners.get(event.type) || [])]) {
        if (event.immediatePropagationStopped) break;
        handler.call(document, event);
      }
      if (!event.propagationStopped) event.target?.dispatchLocal?.(event);
      return !event.defaultPrevented;
    },
  };

  const html = new FakeElement("html", document);
  html.setConnected(true);
  const body = new FakeElement("body", document);
  const header = new FakeElement("header", document);
  const settingsButton = new FakeElement("button", document);
  settingsButton.id = "codey-settings-button";
  const main = new FakeElement("main", document);
  const scope = new FakeElement("section", document);
  const anchor = new FakeElement("div", document);
  anchor.setAttribute(
    "data-above-composer-conversation-id",
    options.threadId === null ? "" : options.threadId || "thread-1",
  );
  let textarea = new FakeElement("textarea", document, {
    value: options.text ?? "implement the workflow",
  });
  const toolbar = new FakeElement("div", document);
  const sendButton = new FakeElement("button", document, {
    rect: {
      bottom: 308,
      height: 36,
      left: 810,
      right: 846,
      top: 272,
      width: 36,
    },
  });
  sendButton.setAttribute("aria-label", "Send message");
  sendButton.setAttribute("type", "submit");
  sendButton.addEventListener("click", () => {
    nativeSends += 1;
  });
  textarea.addEventListener("keydown", (event) => {
    if (event.key === "Enter") nativeSends += 1;
  });

  html.appendChild(body);
  body.appendChild(header);
  header.appendChild(settingsButton);
  body.appendChild(main);
  main.appendChild(scope);
  scope.appendChild(anchor);
  scope.appendChild(textarea);
  scope.appendChild(toolbar);
  toolbar.appendChild(sendButton);
  document.documentElement = html;
  document.body = body;

  if (options.attachment) {
    const attachment = new FakeElement("div", document);
    attachment.setAttribute("data-attachment-id", "attachment-1");
    toolbar.insertBefore(attachment, sendButton);
  }
  if (options.voice) {
    const recording = new FakeElement("div", document);
    recording.setAttribute("data-recording", "true");
    toolbar.insertBefore(recording, sendButton);
  }

  const window = {
    innerHeight: 800,
    location: {
      href:
        options.threadId === null
          ? "codex://new-task"
          : `codex://conversation/${options.threadId || "thread-1"}`,
      pathname:
        options.threadId === null
          ? "/new"
          : `/conversation/${options.threadId || "thread-1"}`,
    },
    crypto: {
      randomUUID() {
        uuid += 1;
        return `command-${uuid}`;
      },
    },
    addEventListener(type, handler) {
      const handlers = windowListeners.get(type) || [];
      handlers.push(handler);
      windowListeners.set(type, handlers);
    },
    getComputedStyle() {
      return { display: "block", visibility: "visible" };
    },
    Event: FakeEvent,
    InputEvent: FakeEvent,
    KeyboardEvent: FakeEvent,
    HTMLTextAreaElement: class HTMLTextAreaElement {},
    HTMLInputElement: class HTMLInputElement {},
    __CODEY_WORKFLOW_MODE_OPTIONS__: {
      capabilityTimeoutMs: options.capabilityTimeoutMs || 50,
      commandTimeoutMs: options.commandTimeoutMs || 80,
      scanDelayMs: 0,
      sessionPollMs: options.sessionPollMs || 1_000,
    },
    __codeySettingsOverlay: {
      openWorkflow(request) {
        workflowOpenRequests.push(request);
      },
    },
  };
  window.window = window;

  const resolveResponse = async (response, payload) =>
    typeof response === "function" ? response(payload) : response;
  window.__codexSessionDeleteBridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/api/workflow_capabilities") {
      return resolveResponse(capabilityResponse, payload);
    }
    if (path === "/api/workflow_list") {
      return resolveResponse(workflowListResponse, payload);
    }
    if (path === "/api/workflow_start") {
      return resolveResponse(startResponse, payload);
    }
    if (path === "/api/workflow_steer") {
      return resolveResponse(steerResponse, payload);
    }
    if (path === "/api/workflow_bypass_audit") return { status: "ok" };
    return { status: "failed" };
  };

  const testSetTimeout = (callback, delay, ...args) => {
    const timer = setTimeout(callback, delay, ...args);
    timer.unref?.();
    return timer;
  };
  const sandbox = {
    clearTimeout,
    console: { error() {}, log() {}, warn() {} },
    document,
    Event: FakeEvent,
    InputEvent: FakeEvent,
    KeyboardEvent: FakeEvent,
    MutationObserver: FakeMutationObserver,
    setTimeout: testSetTimeout,
    window,
  };
  const context = vm.createContext(sandbox);
  vm.runInContext(source, context);
  const api = window.__CODEY_WORKFLOW_MODE_TEST__;
  await api.whenIdle();

  const dispatch = (type, target, init = {}) => {
    const event = new FakeEvent(type, { ...init, target });
    document.dispatchToTarget(event);
    return event;
  };

  return {
    api,
    anchor,
    calls,
    context,
    dispatch,
    document,
    emitMutation(mutation) {
      for (const observer of FakeMutationObserver.instances) {
        if (observer.observed) observer.callback([mutation]);
      }
    },
    getNativeSends: () => nativeSends,
    getWorkflowOpenRequests: () => workflowOpenRequests,
    getTextarea: () => textarea,
    listenerCount: (type) => (documentListeners.get(type) || []).length,
    observerCount: () => FakeMutationObserver.instances.length,
    repeatScript() {
      vm.runInContext(source, context);
    },
    replaceTextarea(text) {
      const previous = textarea;
      const replacement = new FakeElement("textarea", document, { value: text });
      replacement.addEventListener("keydown", (event) => {
        if (event.key === "Enter") nativeSends += 1;
      });
      previous.remove();
      scope.insertBefore(replacement, toolbar);
      textarea = replacement;
      this.emitMutation({
        type: "childList",
        target: scope,
        addedNodes: [replacement],
        removedNodes: [previous],
      });
      return replacement;
    },
    sendButton,
    setCapabilities(value) {
      capabilityResponse = value;
    },
    setStartResponse(value) {
      startResponse = value;
    },
    setWorkflowList(value) {
      workflowListResponse = value;
    },
    setSteerResponse(value) {
      steerResponse = value;
    },
    toolbar,
    window,
  };
};

const commandCalls = (env, path) =>
  env.calls.filter((call) => call.path === path);

test("Enter is exclusively taken over and clears only after a durable ACK", async () => {
  const env = await createEnvironment();
  const event = env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  assert.equal(event.defaultPrevented, true);
  await env.api.whenIdle();

  assert.equal(env.getNativeSends(), 0);
  assert.equal(commandCalls(env, "/api/workflow_start").length, 1);
  const start = commandCalls(env, "/api/workflow_start")[0];
  assert.equal(start.payload.text, "implement the workflow");
  assert.equal(start.payload.commandId, "command-1");
  assert.equal(start.payload.cwd, "/workspace/project");
  assert.equal(start.payload.permissionSnapshot.resolved, true);
  assert.equal(env.getTextarea().value, "");
  const status = env.document.getElementById("codey-workflow-mode-status");
  assert.equal(status.getAttribute("aria-live"), "polite");
  assert.match(status.textContent, /工作流已持久化/);
});

test("click and repeated send events share one pending command", async () => {
  let resolveStart;
  const startPromise = new Promise((resolve) => {
    resolveStart = resolve;
  });
  const env = await createEnvironment({ startResponse: () => startPromise });

  const click = env.sendButton.click();
  assert.equal(click.defaultPrevented, true);
  env.sendButton.click();
  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await flushTimers();
  assert.equal(commandCalls(env, "/api/workflow_start").length, 1);
  assert.equal(env.getNativeSends(), 0);

  resolveStart(durableAck());
  await env.api.whenIdle();
  assert.equal(env.getTextarea().value, "");
});

test("a failed admission retry reuses its commandId", async () => {
  let attempt = 0;
  const env = await createEnvironment({
    startResponse: () => {
      attempt += 1;
      return attempt === 1
        ? { status: "failed", accepted: false }
        : durableAck();
    },
  });
  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();
  assert.equal(env.getTextarea().value, "implement the workflow");

  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();
  const starts = commandCalls(env, "/api/workflow_start");
  assert.equal(starts.length, 2);
  assert.equal(starts[0].payload.commandId, starts[1].payload.commandId);
  assert.equal(env.getTextarea().value, "");
});

test("missing durable ACK and command timeout both preserve composer text", async (t) => {
  await t.test("invalid ACK", async () => {
    const env = await createEnvironment({
      startResponse: { status: "ok", runId: "run-1" },
    });
    env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
    await env.api.whenIdle();
    assert.equal(env.getTextarea().value, "implement the workflow");
    assert.equal(env.getNativeSends(), 0);
  });

  await t.test("timeout", async () => {
    const env = await createEnvironment({
      commandTimeoutMs: 15,
      startResponse: () => new Promise(() => {}),
    });
    env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
    await new Promise((resolve) => setTimeout(resolve, 25));
    await env.api.whenIdle();
    assert.equal(env.getTextarea().value, "implement the workflow");
    assert.equal(env.getNativeSends(), 0);
  });
});

test("new threads require confirmed origin binding", async (t) => {
  await t.test("confirmed binding clears after durable ACK", async () => {
    const env = await createEnvironment({
      threadId: null,
      startResponse: durableAck({
        binding: { confirmed: true, threadId: "thread-created" },
      }),
    });
    env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
    await env.api.whenIdle();
    const start = commandCalls(env, "/api/workflow_start")[0];
    assert.equal(start.payload.threadId, null);
    assert.equal(start.payload.origin.requiresConfirmedBinding, true);
    assert.equal(env.getTextarea().value, "");
    assert.equal(env.getNativeSends(), 0);
  });

  await t.test("explicit non-acceptance safely falls back to native", async () => {
    const env = await createEnvironment({
      threadId: null,
      startResponse: {
        status: "failed",
        accepted: false,
        safeToSendNative: true,
        binding: { confirmed: false },
      },
    });
    env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
    await env.api.whenIdle();
    assert.equal(commandCalls(env, "/api/workflow_start").length, 1);
    assert.equal(env.getNativeSends(), 1);
    assert.equal(env.getTextarea().value, "implement the workflow");
  });
});

test("active workflow follow-ups use workflow_steer and current origin turn", async () => {
  const env = await createEnvironment({
    capabilities: defaultCapabilities({
      activeWorkflow: {
        active: true,
        runId: "run-active",
        originTurnId: "origin-turn-7",
        revision: 7,
      },
    }),
    steerResponse: durableAck({ runId: "run-active", revision: 8 }),
  });
  env.sendButton.click();
  await env.api.whenIdle();

  assert.equal(commandCalls(env, "/api/workflow_start").length, 0);
  assert.equal(commandCalls(env, "/api/workflow_steer").length, 1);
  const steer = commandCalls(env, "/api/workflow_steer")[0];
  assert.equal(steer.payload.runId, "run-active");
  assert.equal(steer.payload.expectedRevision, 7);
  assert.deepEqual(
    { ...steer.payload.delivery },
    {
      target: "current_origin_turn",
      originTurnId: "origin-turn-7",
      backendDecision: "steer_or_recompile",
    },
  );
  assert.equal(env.getTextarea().value, "");
});

test("current-thread workflow button only appears for a linked run", async () => {
  const env = await createEnvironment({
    workflowList: {
      engineEpoch: "engine-1",
      threadId: "thread-1",
      runs: [
        {
          runId: "run-thread-1",
          originThreadId: "thread-1",
          state: "succeeded",
          title: "linked workflow",
        },
      ],
    },
  });

  const list = commandCalls(env, "/api/workflow_list")[0];
  assert.equal(list.payload.threadId, "thread-1");
  assert.equal(env.api.snapshot().hasSessionButton, true);
  assert.equal(env.api.snapshot().sessionRunId, "run-thread-1");

  env.document.getElementById("codey-workflow-session-button").click();
  await env.api.whenIdle();
  assert.deepEqual(
    env.getWorkflowOpenRequests().map((request) => ({ ...request })),
    [{ threadId: "thread-1", runId: "run-thread-1" }],
  );
});

test("switching to a thread without workflow history removes the session button", async () => {
  const env = await createEnvironment({
    workflowList: {
      engineEpoch: "engine-1",
      runs: [
        {
          runId: "run-thread-1",
          originThreadId: "thread-1",
          state: "succeeded",
        },
      ],
    },
  });
  assert.equal(env.api.snapshot().hasSessionButton, true);

  env.setWorkflowList({ engineEpoch: "engine-1", runs: [] });
  env.anchor.setAttribute("data-above-composer-conversation-id", "thread-2");
  env.window.location.href = "codex://conversation/thread-2";
  env.window.location.pathname = "/conversation/thread-2";
  env.api.scanNow();
  await env.api.whenIdle();

  assert.equal(env.api.snapshot().sessionThreadId, null);
  assert.equal(env.api.snapshot().hasSessionButton, false);
  assert.equal(
    commandCalls(env, "/api/workflow_list").at(-1).payload.threadId,
    "thread-2",
  );
});

test("durable workflow admission exposes the current-thread button immediately", async () => {
  const env = await createEnvironment();
  assert.equal(env.api.snapshot().hasSessionButton, false);

  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();

  assert.equal(env.api.snapshot().hasSessionButton, true);
  assert.equal(env.api.snapshot().sessionRunId, "run-1");
});

test("attachments, voice, and Slash commands remain native", async (t) => {
  const cases = [
    { name: "attachment", options: { attachment: true }, reason: "attachment" },
    { name: "voice", options: { voice: true }, reason: "voice" },
    {
      name: "Slash command",
      options: { text: "/model gpt-5" },
      reason: "slash_command",
    },
  ];
  for (const item of cases) {
    await t.test(item.name, async () => {
      const env = await createEnvironment(item.options);
      const event = env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
      await env.api.whenIdle();
      assert.equal(event.defaultPrevented, false);
      assert.equal(env.getNativeSends(), 1);
      assert.equal(commandCalls(env, "/api/workflow_start").length, 0);
      const audit = commandCalls(env, "/api/workflow_bypass_audit")[0];
      assert.equal(audit.payload.reason, item.reason);
      assert.equal(Object.hasOwn(audit.payload, "text"), false);
    });
  }
});

test("one-shot native bypass is consumed once", async () => {
  const env = await createEnvironment();
  const button = env.document.getElementById("codey-workflow-native-once");
  assert.ok(button);
  button.click();
  assert.equal(env.api.snapshot().buttonArmed, true);

  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();
  assert.equal(env.getNativeSends(), 1);
  assert.equal(commandCalls(env, "/api/workflow_start").length, 0);
  assert.equal(env.api.snapshot().buttonArmed, false);

  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();
  assert.equal(commandCalls(env, "/api/workflow_start").length, 1);
  assert.equal(env.getTextarea().value, "");
});

test("IME composition is never taken over", async () => {
  const env = await createEnvironment();
  env.dispatch("compositionstart", env.getTextarea());
  const composingEvent = env.dispatch("keydown", env.getTextarea(), {
    isComposing: true,
    key: "Enter",
  });
  await env.api.whenIdle();
  assert.equal(composingEvent.defaultPrevented, false);
  assert.equal(commandCalls(env, "/api/workflow_start").length, 0);

  env.dispatch("compositionend", env.getTextarea());
  env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();
  assert.equal(commandCalls(env, "/api/workflow_start").length, 1);
});

test("unhealthy preflight replays the same send natively without workflow_start", async () => {
  const env = await createEnvironment();
  env.setCapabilities(
    defaultCapabilities({ proxyHealthy: false, reason: "proxy unavailable" }),
  );
  const event = env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  assert.equal(event.defaultPrevented, true);
  await env.api.whenIdle();

  assert.equal(commandCalls(env, "/api/workflow_start").length, 0);
  assert.equal(env.getNativeSends(), 1);
  assert.equal(env.getTextarea().value, "implement the workflow");
  assert.match(
    env.document.getElementById("codey-workflow-mode-status").textContent,
    /本次未经过工作流.*proxy unavailable/,
  );
});

test("workflow mode is disabled by default when capabilities do not enable it", async () => {
  const env = await createEnvironment({
    capabilities: defaultCapabilities({ enabled: false }),
  });
  const event = env.dispatch("keydown", env.getTextarea(), { key: "Enter" });
  await env.api.whenIdle();

  assert.equal(env.api.snapshot().enabled, false);
  assert.equal(event.defaultPrevented, false);
  assert.equal(env.getNativeSends(), 1);
  assert.equal(commandCalls(env, "/api/workflow_start").length, 0);
  assert.equal(env.document.getElementById("codey-workflow-native-once"), null);
});

test("repeat injection and composer remount do not duplicate observers, listeners, or buttons", async () => {
  const env = await createEnvironment();
  const initialKeydownListeners = env.listenerCount("keydown");
  const initialClickListeners = env.listenerCount("click");
  env.repeatScript();
  assert.equal(env.observerCount(), 1);
  assert.equal(env.listenerCount("keydown"), initialKeydownListeners);
  assert.equal(env.listenerCount("click"), initialClickListeners);
  assert.equal(env.api.snapshot().hasButton, true);

  const replacement = env.replaceTextarea("message after remount");
  await flushTimers();
  env.api.scanNow();
  const buttons = env.document.querySelectorAll("#codey-workflow-native-once");
  assert.equal(buttons.length, 1);

  env.dispatch("keydown", replacement, { key: "Enter" });
  await env.api.whenIdle();
  assert.equal(commandCalls(env, "/api/workflow_start").length, 1);
  assert.equal(replacement.value, "");
});

test("bypass audit never contains request body or a derived preview", async () => {
  const secret = "/run secret-body-never-audit";
  const env = await createEnvironment({ text: secret });
  env.sendButton.click();
  await env.api.whenIdle();

  const audit = commandCalls(env, "/api/workflow_bypass_audit")[0];
  const serialized = JSON.stringify(audit.payload);
  assert.equal(serialized.includes(secret), false);
  assert.equal(serialized.includes("secret-body"), false);
  assert.deepEqual(
    Object.keys(audit.payload).sort(),
    [
      "commandId",
      "hadAttachment",
      "hadVoice",
      "reason",
      "schemaVersion",
      "source",
      "threadId",
      "wasSlashCommand",
    ],
  );
});
