import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { flushMicrotasks } from "./helpers/flush.mjs";
import { TextEncoder } from "node:util";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {
  constructor(attributes = {}) {
    super("div", { attributes });
    this.removed = false;
    this.querySelectorAllCalls = [];
    const classes = new Set();
    this.classList = {
      add: (className) => classes.add(className),
      contains: (className) => classes.has(className),
      remove: (className) => classes.delete(className),
      toggle: (className) => (
        classes.has(className) ? (classes.delete(className), false) : (classes.add(className), true)
      ),
    };
  }

  querySelector(selector) {
    if (selector === "[data-local-conversation-final-assistant]") return {};
    return super.querySelector(selector);
  }

  querySelectorAll(selector) {
    this.querySelectorAllCalls.push(selector);
    if (this.getAttribute("data-terminal-error") === "true") {
      return [new FakeElement({ "data-status": "failed" })];
    }
    return [];
  }

  matches(selector) {
    const selectors = String(selector).split(",").map((candidate) => candidate.trim());
    return selectors.some((candidate) => (
      candidate === "[data-turn-key]" && this.hasAttribute("data-turn-key")
    ) || (
      candidate === "[data-message-author-role]" && this.hasAttribute("data-message-author-role")
    ) || (
      candidate === "[data-testid=conversation-turn]" && this.getAttribute("data-testid") === "conversation-turn"
    ) || (
      candidate === "[data-testid=\"conversation-turn\"]" && this.getAttribute("data-testid") === "conversation-turn"
    ) || (
      candidate === "[data-message-id]" && this.hasAttribute("data-message-id")
    ));
  }

  closest() {
    return null;
  }

  getClientRects() {
    return this.layoutHidden ? [] : [1];
  }

  appendChild() {}

  remove() {
    this.removed = true;
  }
}

class TreeElement extends FakeElement {
  querySelectorAll(selector) {
    this.querySelectorAllCalls.push(selector);
    return FakeElementCore.prototype.querySelectorAll.call(this, selector);
  }

  closest(selector) {
    return FakeElementCore.prototype.closest.call(this, selector);
  }

  appendChild(child) {
    if (child.parentElement) {
      const siblings = child.parentElement.children;
      const index = siblings.indexOf(child);
      if (index >= 0) siblings.splice(index, 1);
    }
    child.parentElement = this;
    child.isConnected = this.isConnected;
    child.removed = false;
    this.children.push(child);
    return child;
  }
}

function attachReactTurn(row, {
  items = [],
  status = "completed",
  turnId = row.getAttribute("data-turn-key"),
} = {}) {
  row.__reactProps$test = {
    children: {
      props: {
        entry: {
          turnId,
          turnKey: turnId,
          turn: { items, status },
        },
      },
    },
  };
  return row;
}

const messageSelectButton = (row) => row.children.find(
  (child) => child.dataset.codeyMessageSelect === "true",
) || null;

function loadInjection({
  initialNow = 1_000_000,
  initialSessionId = "session-1",
  turnIds = ["turn-1"],
  sessionTitle = "排查飞书通知",
  bridgeHandler = null,
  codexSessionController = null,
  codexSignalDispatcher = null,
  discoveredAppServerManager = null,
  localAppServerManager = null,
  selectedTurnIds = [],
  hiddenTurnIds = [],
  sessionElements = {},
} = {}) {
  const rows = turnIds.map((turnId) => new FakeElement({ "data-turn-key": turnId }));
  rows.forEach((row) => {
    row.layoutHidden = hiddenTurnIds.includes(row.getAttribute("data-turn-key"));
    row.dataset.codeyMessageId = row.getAttribute("data-turn-key");
    if (selectedTurnIds.includes(row.dataset.codeyMessageId)) {
      row.classList.add("codey-message-selected");
    }
  });
  const sidebarThread = new FakeElement({
    "data-app-action-sidebar-thread-id": "local:session-1",
    "data-app-action-sidebar-thread-title": sessionTitle,
  });
  let now = initialNow;
  let sessionId = initialSessionId;
  const bridgeCalls = [];
  const alerts = [];
  const confirmations = [];
  let reloadCount = 0;
  const timers = [];
  const toolbar = new FakeElement();
  const placeholder = new FakeElement();
  const documentElement = new FakeElement();
  const documentBody = new FakeElement();
  const managerAssetUrl = "app://-/assets/app-initial-completion-reconcile.js";
  let managerModule = null;
  if (discoveredAppServerManager || localAppServerManager) {
    const scope = {
      query: null,
      get(_key, hostId) { return hostId === "local" ? localAppServerManager : null; },
      set() {},
      watch() {},
      when() {},
      forHost() {},
    };
    documentBody.__reactFiber$test = {
      dependencies: null,
      memoizedProps: null,
      memoizedState: { scope },
      return: null,
      updateQueue: null,
    };
    function resolveManager(candidateScope, hostId) {
      candidateScope.get();
      candidateScope.forHost();
      if (hostId !== "local") throw new Error("AppServerManager RPC is not connected");
      return discoveredAppServerManager;
    }
    managerModule = { resolveManager };
    if (localAppServerManager) {
      managerModule.LocalRegistry = class {
        constructor() { throw new Error("registry constructors must not run during discovery"); }
        getAll() { return this.scope.get("managers"); }
        getForHostId(hostId) { return this.scope.get("manager", hostId); }
      };
    }
  }
  const document = {
    documentElement,
    body: documentBody,
    scripts: managerModule ? [{ src: managerAssetUrl }] : [],
    visibilityState: "visible",
    getElementById(id) {
      if (id === "codey-injected-style" || id === "codey-settings-button") return placeholder;
      if (id === "codey-message-toolbar") return toolbar;
      return null;
    },
    querySelector(selector) {
      if (selector in sessionElements) return sessionElements[selector][0] || null;
      if (selector === "[data-session-id]") {
        return new FakeElement({ "data-session-id": sessionId });
      }
      return null;
    },
    querySelectorAll(selector) {
      if (selector in sessionElements) return sessionElements[selector];
      if (selector === "[data-turn-key]") {
        return rows.filter((row) => !row.removed && row.hasAttribute("data-turn-key"));
      }
      if (selector === "[data-turn-key], [data-message-author-role], [data-testid=conversation-turn], [data-message-id]") {
        return rows.filter((row) => !row.removed && row.matches(selector));
      }
      if (selector === "[data-codey-message-id]") {
        return rows.filter((row) => !row.removed && row.dataset.codeyMessageId);
      }
      if (
        selector === "[data-message-id]"
        || selector === "[data-testid=conversation-turn]"
        || selector === "[data-testid=\"conversation-turn\"]"
      ) {
        return rows.filter((row) => !row.removed && row.matches(selector));
      }
      if (selector === ".codey-message-selected[data-codey-message-id]") {
        return rows.filter((row) => (
          !row.removed
          && row.dataset.codeyMessageId
          && row.classList.contains("codey-message-selected")
        ));
      }
      if (selector === "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]") {
        return [sidebarThread];
      }
      return [];
    },
    createElement() {
      return new FakeElement();
    },
  };
  let mutationHandler = null;
  const window = {
    __codexSessionDeleteBridge: async (path, payload, options = {}) => {
      bridgeCalls.push({ options, path, payload });
      if (bridgeHandler) return bridgeHandler(path, payload, options);
      return { status: "ok" };
    },
    __codeyCodexSessionController: codexSessionController,
    __codeyCodexSignalDispatcher: codexSignalDispatcher,
    addEventListener: () => {},
    alert: (message) => alerts.push(String(message)),
    clearTimeout: () => {},
    confirm: (message) => {
      confirmations.push(String(message));
      return true;
    },
    dispatchEvent: () => true,
    getComputedStyle: () => ({ display: "block", visibility: "visible" }),
    requestIdleCallback: (callback) => {
      callback({ didTimeout: false, timeRemaining: () => 50 });
      return 1;
    },
    setTimeout: (callback) => {
      timers.push(callback);
      return timers.length;
    },
    localStorage: {
      length: 0,
      key: () => null,
      getItem: () => null,
      setItem: () => {},
    },
  };
  if (managerModule) {
    window.__codeyImportCodexAsset = async (url) => {
      assert.equal(url, managerAssetUrl);
      return managerModule;
    };
  }
  const renderDeletion = (targetSessionId, messageIds) => {
    if (targetSessionId !== window.__codeyGetSessionId()) return;
    for (const row of rows) {
      if (!row.layoutHidden && messageIds.includes(window.__codeyGetMessageId(row))) row.remove();
    }
  };
  const controller = window.__codeyCodexSessionController;
  if (typeof controller?.finishMessageDeletion === "function") {
    const finish = controller.finishMessageDeletion;
    controller.finishMessageDeletion = async (targetSessionId, messageIds) => {
      const result = await finish(targetSessionId, messageIds);
      renderDeletion(targetSessionId, messageIds);
      return result;
    };
  }
  window.window = window;
  const MutationObserver = class {
    constructor(handler) {
      mutationHandler = handler;
    }

    observe() {}
  };
  class ControlledDate extends Date {
    constructor(...args) {
      super(...(args.length ? args : [now]));
    }

    static now() {
      return now;
    }
  }
  vm.runInNewContext(source, {
    atob: (value) => Buffer.from(value, "base64").toString("binary"),
    btoa: (value) => Buffer.from(value, "binary").toString("base64"),
    console,
    CustomEvent: class {
      constructor(type, options = {}) {
        this.type = type;
        this.detail = options.detail;
      }
    },
    Date: ControlledDate,
    document,
    HTMLElement: FakeElement,
    location: {
      pathname: "/",
      search: "",
      reload: () => {
        reloadCount += 1;
      },
    },
    MutationObserver,
    TextEncoder,
    URLSearchParams,
    window,
  });
  rows.forEach((row) => {
    row.dataset.codeyMessageId = window.__codeyGetMessageId(row);
  });
  return {
    advanceTime: (milliseconds) => {
      now += milliseconds;
    },
    appendTurn: (turnId) => {
      const row = new FakeElement({ "data-turn-key": turnId });
      rows.push(row);
      return row;
    },
    appendExistingRow: (row) => {
      rows.push(row);
      return row;
    },
    alerts,
    bridgeCalls,
    confirmations,
    emitMutations: (mutations) => mutationHandler?.(mutations),
    flushTimers: () => {
      while (timers.length) timers.shift()();
    },
    getReloadCount: () => reloadCount,
    getTurnRow: (index = 0) => rows[index] || null,
    getVisibleTurnIds: () => rows
      .filter((row) => !row.removed)
      .map((row) => row.getAttribute("data-turn-key")),
    setSessionId: (value) => {
      sessionId = String(value);
    },
    setTurnText: (index, value) => {
      const row = rows[index];
      if (row) row.textContent = String(value);
    },
    window,
  };
}

const createLocalDeletionManager = (overrides = {}) => ({
  getHostId() { return "local"; },
  getConversation() { return { turns: [], resumeState: "resumed" }; },
  getStreamRole() { return { role: "owner" }; },
  async sendRequest() {},
  updateConversationState() {},
  inactiveThreadUnsubscriber: { clearConversationStreamOwnership() {} },
  ...overrides,
});

const createRecoveryController = (events, overrides = {}) => ({
  kind: "manager",
  async prepareMessageDeletion() {},
  async finishMessageDeletion() {},
  async refreshRecentConversations() {},
  async reconcileCompletedConversation(payload) {
    events.push({ payload, type: "reconcile" });
    return true;
  },
  ...overrides,
});

test("reconciles the current session through AppServerManager without a completion bridge", async () => {
  const events = [];
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
  });

  await flushMicrotasks();

  assert.deepEqual(JSON.parse(JSON.stringify(events)), [{
    payload: {
      collaborationMode: null,
      conversationId: "session-1",
      model: null,
      reasoningEffort: null,
      serviceTier: null,
      showThreadGoalResumeConfirmation: false,
      workspaceRoots: [],
    },
    type: "reconcile",
  }]);
  assert.equal(
    runtime.bridgeCalls.some((call) => call.path === "/session/completion-state"),
    false,
  );
  assert.equal(await runtime.window.__codeyReconcileStaleCompletedTask(), false);
  assert.equal(events.length, 1);
});

test("reconciles when the conversation appears after renderer startup", async () => {
  const events = [];
  const runtime = loadInjection({
    initialSessionId: "",
    codexSessionController: createRecoveryController(events),
  });

  await flushMicrotasks();
  assert.equal(events.length, 0);

  runtime.setSessionId("session-1");
  runtime.emitMutations([{
    type: "childList",
    target: new FakeElement(),
    addedNodes: [],
    removedNodes: [],
  }]);
  await flushMicrotasks();

  assert.deepEqual(events.map((event) => event.payload.conversationId), ["session-1"]);
});

test("usage-only manager remains available when deletion and reconciliation are missing", async () => {
  const requests = [];
  const runtime = loadInjection({
    discoveredAppServerManager: { sendRequest(method) { requests.push(method); return { rateLimits: {} }; } },
    selectedTurnIds: ["turn-1"],
  });
  await flushMicrotasks();
  assert.equal(runtime.window.__codeyPageCapabilities.reconcile.status, "unavailable");
  assert.ok((await runtime.window.__codeyReadAccountRateLimits()).rateLimits);
  assert.deepEqual(requests, ["account/rateLimits/read"]);
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.equal(runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"), false);
  assert.equal(runtime.alerts.length, 0);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-1"]);
  assert.equal(runtime.window.__codeyPageCapabilities.deleteMessages.status, "unavailable");
});

test("native model routing discovers the local request client without sending a request", async () => {
  const client = { hostId: "local", enqueueRequest() { throw Error("must not send"); }, createRequest() {}, onResult() {} };
  const manager = { requestClient: client, sendRequest() { throw Error("must not send"); } };
  const runtime = loadInjection({ localAppServerManager: manager, initialSessionId: "" });
  assert.equal(await runtime.window.__codeyLoadCodexRequestClient(), client);
  assert.equal(runtime.window.__codeyPageCapabilities.modelRouting.status, "available");
});

test("native model routing cannot use a remote or incomplete client", async () => {
  const manager = { requestClient: { hostId: "remote" }, getConversation() { return null; } };
  const runtime = loadInjection({ localAppServerManager: manager, initialSessionId: "" });
  await assert.rejects(runtime.window.__codeyLoadCodexRequestClient(), /模型请求校验/);
});

test("MCP reload prefers the patched AppServerRequestClient over fiber discovery", async () => {
  const requests = [];
  const runtime = loadInjection({ initialSessionId: "" });
  runtime.window.__codeyAppServerRequestClients = new Map([
    ["local", {
      sendRequest(...args) {
        requests.push(args);
        return {};
      },
    }],
  ]);
  assert.equal((await runtime.window.__codeyReloadMcpServers()).ok, true);
  assert.equal(JSON.stringify(requests), JSON.stringify([["config/mcpServer/reload", {}]]));
  assert.equal(runtime.window.__codeyCodexSessionController, null);
});

test("MCP reload awaits the native request without restarting or resuming a task", async () => {
  const requests = [];
  let finish;
  const runtime = loadInjection({
    initialSessionId: "",
    discoveredAppServerManager: {
      sendRequest(...args) {
        requests.push(args);
        return new Promise((resolve) => { finish = resolve; });
      },
    },
  });
  let completed = false;
  const reload = runtime.window.__codeyReloadMcpServers().then((value) => {
    completed = true;
    return value;
  });
  await flushMicrotasks();
  assert.equal(JSON.stringify(requests), JSON.stringify([["config/mcpServer/reload", {}]]));
  assert.equal(completed, false);
  finish({});
  assert.equal((await reload).ok, true);
  assert.equal(runtime.window.__codeyPageCapabilities.mcpReload.status, "available");
});

test("MCP reload propagates protocol errors and can be retried", async () => {
  let fail = true;
  const runtime = loadInjection({
    initialSessionId: "",
    discoveredAppServerManager: {
      sendRequest() {
        return fail ? Promise.reject(new Error("Unknown method")) : Promise.resolve({});
      },
    },
  });
  await assert.rejects(runtime.window.__codeyReloadMcpServers(), /Unknown method/);
  fail = false;
  assert.equal((await runtime.window.__codeyReloadMcpServers()).ok, true);
});

test("MCP reload reports an unavailable manager instead of using legacy session signals", async () => {
  const runtime = loadInjection({ initialSessionId: "" });
  runtime.window.__codeyCodexSignalDispatcher = () => { throw new Error("unexpected signal"); };
  await assert.rejects(runtime.window.__codeyReloadMcpServers(), { code: "codey_capability_unavailable" });
  assert.equal(runtime.window.__codeyPageCapabilities.mcpReload.status, "unavailable");
});

test("MCP reload discovery is not capped by the native session timeout", async () => {
  const requests = [];
  const manager = {
    sendRequest(...args) {
      requests.push(args);
      return {};
    },
  };
  const runtime = loadInjection({
    discoveredAppServerManager: manager,
    initialSessionId: "",
  });
  runtime.window.__codeyCodexSessionController = null;
  const originalImport = runtime.window.__codeyImportCodexAsset;
  let release;
  runtime.window.__codeyImportCodexAsset = (...args) => new Promise((resolve) => {
    release = () => resolve(originalImport(...args));
  });
  const reload = runtime.window.__codeyReloadMcpServers();
  await flushMicrotasks();
  runtime.flushTimers();
  await flushMicrotasks();
  assert.equal(requests.length, 0);
  release();
  assert.equal((await reload).ok, true);
  assert.equal(JSON.stringify(requests), JSON.stringify([["config/mcpServer/reload", {}]]));
});

test("message deletion requires the complete local manager before releasing or persisting", async () => {
  for (const missing of ["getHostId", "getConversation", "getStreamRole", "sendRequest",
    "updateConversationState", "inactiveThreadUnsubscriber"]) {
    let releases = 0;
    const manager = createLocalDeletionManager({ sendRequest() { releases += 1; } });
    delete manager[missing];
    const runtime = loadInjection({ localAppServerManager: manager, selectedTurnIds: ["turn-1"] });
    await flushMicrotasks();
    await runtime.window.__codeyDeleteSelectedMessages();
    assert.equal(releases, 0, missing);
    assert.equal(runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"), false, missing);
  }
});

test("RPC function stubs do not advertise unavailable in-place deletion methods", async () => {
  const calls = [];
  const rpc = new Proxy({}, {
    get(_target, method) {
      return async () => {
        calls.push(method);
        throw new TypeError(`'${String(method)}' is not a function.`);
      };
    },
  });
  const runtime = loadInjection({
    initialSessionId: "",
    discoveredAppServerManager: rpc,
    selectedTurnIds: ["turn-1"],
  });
  await assert.rejects(
    runtime.window.__codeyLoadCodexSessionController({ feature: "deleteMessages" }),
    { code: "codey_capability_unavailable" },
  );
  runtime.setSessionId("session-1");
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.deepEqual(calls, []);
  assert.equal(runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"), false);
});

test("deletion uses the local registry instead of RPC stubs without custom native methods", async () => {
  const rpcCalls = [];
  const rpc = new Proxy({}, {
    get(_target, method) {
      return async () => {
        rpcCalls.push(method);
        throw new TypeError(`'${String(method)}' is not a function.`);
      };
    },
  });
  const retainedTurn = { turnId: "turn-2", status: "completed" };
  const conversation = {
    turns: [{ turnId: "turn-1", status: "completed" }, retainedTurn],
    resumeState: "resumed",
  };
  const events = [];
  let runtime;
  const manager = {
    getHostId() { return "local"; },
    getConversation() { return conversation; },
    getStreamRole() { return { role: "owner" }; },
    async sendRequest(method, params) {
      assert.equal(method, "thread/unsubscribe");
      assert.equal(params.threadId, "session-1");
      events.push("unsubscribe");
    },
    updateConversationState(sessionId, update) {
      assert.equal(sessionId, "session-1");
      update(conversation);
      for (let index = 0; index < 2; index += 1) {
        const row = runtime.getTurnRow(index);
        if (!conversation.turns.some((turn) => turn.turnId === row.getAttribute("data-turn-key"))) row.remove();
      }
      events.push("update");
    },
    inactiveThreadUnsubscriber: {
      clearConversationStreamOwnership() { events.push("clear"); },
    },
    discardConversationFromCache() { assert.fail("must not evict the conversation"); },
    resumeConversation() { assert.fail("must not resume the conversation"); },
  };
  runtime = loadInjection({
    initialSessionId: "",
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1"],
    codexSessionController: { kind: "manager", manager: rpc },
    discoveredAppServerManager: rpc,
    localAppServerManager: manager,
    bridgeHandler: async (path) => {
      if (path !== "/session/delete-messages") return { status: "ok" };
      events.push("persist");
      return { status: "ok", deleted: 1 };
    },
  });
  runtime.setSessionId("session-1");
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.equal(runtime.window.__codeyCodexSessionController.manager, manager);
  assert.deepEqual(events, ["unsubscribe", "persist", "update", "clear"]);
  assert.deepEqual(rpcCalls, []);
  assert.equal(conversation.turns[0], retainedTurn);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
  assert.deepEqual(runtime.alerts, []);
  assert.equal(runtime.getReloadCount(), 0);
});

test("failed discovery backs off independently and recovers when a manager appears", async () => {
  let imports = 0;
  // Exercise discovery through an already discoverable manager asset.
  const manager = { sendRequest() { return { rateLimits: {} }; } };
  const discovered = loadInjection({ discoveredAppServerManager: manager, initialSessionId: "" });
  const originalImport = discovered.window.__codeyImportCodexAsset;
  discovered.window.__codeyImportCodexAsset = async (...args) => { imports += 1; return originalImport(...args); };
  await assert.rejects(discovered.window.__codeyLoadCodexSessionController({ feature: "deleteMessages" }), { code: "codey_capability_unavailable" });
  const firstImports = imports;
  await assert.rejects(discovered.window.__codeyLoadCodexSessionController({ feature: "deleteMessages" }));
  assert.equal(imports, firstImports);
  discovered.advanceTime(1_000);
  await assert.rejects(discovered.window.__codeyLoadCodexSessionController({ feature: "deleteMessages" }));
  assert.ok(imports > firstImports);
  const secondImports = imports;
  discovered.advanceTime(1_000);
  await assert.rejects(discovered.window.__codeyLoadCodexSessionController({ feature: "deleteMessages" }));
  assert.equal(imports, secondImports, "second failure waits two seconds");
  Object.assign(manager, createLocalDeletionManager({ sendRequest: manager.sendRequest }));
  const controller = await discovered.window.__codeyLoadCodexSessionController({ feature: "deleteMessages" });
  assert.equal(controller.manager, manager);
  assert.equal(discovered.window.__codeyPageCapabilities.deleteMessages.status, "available");
  assert.ok((await discovered.window.__codeyReadAccountRateLimits()).rateLimits);
});

test("timed out discovery cannot later issue a message deletion", async () => {
  const manager = createLocalDeletionManager();
  const runtime = loadInjection({ discoveredAppServerManager: manager, initialSessionId: "", selectedTurnIds: ["turn-1"] });
  const originalImport = runtime.window.__codeyImportCodexAsset;
  let release;
  runtime.window.__codeyImportCodexAsset = (...args) => new Promise((resolve) => { release = () => resolve(originalImport(...args)); });
  runtime.setSessionId("session-1");
  const deletion = runtime.window.__codeyDeleteSelectedMessages();
  await flushMicrotasks();
  runtime.flushTimers();
  await deletion;
  release();
  await flushMicrotasks();
  assert.equal(runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"), false);
  assert.equal(runtime.alerts.length, 0);
});

test("disposed capability discovery cannot replace a new installation's controller or status", async () => {
  const manager = { sendRequest() { return { rateLimits: {} }; } };
  const runtime = loadInjection({ discoveredAppServerManager: manager, initialSessionId: "" });
  const originalImport = runtime.window.__codeyImportCodexAsset;
  let release;
  runtime.window.__codeyImportCodexAsset = (...args) => new Promise((resolve) => {
    release = () => resolve(originalImport(...args));
  });
  const pending = runtime.window.__codeyLoadCodexSessionController({ feature: "usage" });
  await flushMicrotasks();
  runtime.window.__codeySessionToolsInstall.dispose();
  const replacement = { kind: "manager", manager: { sendRequest() {} } };
  const replacementStatus = { usage: { status: "available", message: "" } };
  runtime.window.__codeyCodexSessionController = replacement;
  runtime.window.__codeyPageCapabilities = replacementStatus;
  release();
  await assert.rejects(pending, { code: "codey_capability_unavailable" });
  assert.equal(runtime.window.__codeyCodexSessionController, replacement);
  assert.equal(runtime.window.__codeyPageCapabilities, replacementStatus);
  assert.equal(replacementStatus.usage.status, "available");
});

test("rediscovers a patched manager cached before completion reconciliation was available", async () => {
  const managerEvents = [];
  const discoveredAppServerManager = {
    codeyReconcileCompletedConversation(payload) {
      managerEvents.push(payload.conversationId);
      return Promise.resolve(true);
    },
    discardConversationFromCache() {},
    handleThreadDeletion() {},
    refreshRecentConversations() {},
    resumeConversation() {},
  };
  loadInjection({
    codexSessionController: {
      kind: "manager",
      discardConversation() {},
      notifyConversationDeleted() {},
      refreshRecentConversations() {},
      resumeConversation() {},
    },
    discoveredAppServerManager,
  });

  await flushMicrotasks();

  assert.deepEqual(managerEvents, ["session-1"]);
});

test("resets the reconciliation interval when the visible session changes", async () => {
  const events = [];
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
  });
  await flushMicrotasks();

  assert.equal(events.length, 1);
  runtime.setSessionId("session-2");
  assert.equal(await runtime.window.__codeyReconcileStaleCompletedTask(), true);
  assert.deepEqual(
    events.map((event) => event.payload.conversationId),
    ["session-1", "session-2"],
  );
});

test("rejects a stale result and reconciles the task opened while a request was in flight", async () => {
  const events = [];
  let deferReconcile = false;
  let resolveReconcile;
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events, {
      async reconcileCompletedConversation(payload) {
        events.push({ payload, type: "reconcile" });
        if (!deferReconcile || payload.conversationId !== "session-1") return false;
        return new Promise((resolve) => {
          resolveReconcile = resolve;
        });
      },
    }),
  });
  await flushMicrotasks();

  deferReconcile = true;
  runtime.advanceTime(15_000);
  const reconciliation = runtime.window.__codeyReconcileStaleCompletedTask();
  await flushMicrotasks();
  runtime.setSessionId("session-2");
  runtime.emitMutations([{
    type: "childList",
    target: new FakeElement(),
    addedNodes: [],
    removedNodes: [],
  }]);
  await flushMicrotasks();
  assert.deepEqual(events.map((event) => event.payload.conversationId), ["session-1", "session-1"]);
  resolveReconcile(true);

  assert.equal(await reconciliation, false);
  await flushMicrotasks();
  assert.deepEqual(
    events.map((event) => event.payload.conversationId),
    ["session-1", "session-1", "session-2"],
  );
});

test("retries AppServerManager discovery when a signals controller was cached first", async () => {
  const managerEvents = [];
  const signalEvents = [];
  const discoveredAppServerManager = {
    codeyReconcileCompletedConversation(payload) {
      managerEvents.push("reconcile:" + payload.conversationId);
      return Promise.resolve(true);
    },
    discardConversationFromCache() {},
    handleThreadDeletion() {},
    refreshRecentConversations() {},
    resumeConversation() {},
  };
  const runtime = loadInjection({
    codexSessionController: {
      kind: "signals",
      discardConversation: () => signalEvents.push("discard"),
      notifyConversationDeleted: () => signalEvents.push("delete"),
      refreshRecentConversations: () => signalEvents.push("refresh"),
      resumeConversation: () => signalEvents.push("resume"),
    },
    discoveredAppServerManager,
  });

  await flushMicrotasks();

  assert.deepEqual(managerEvents, ["reconcile:session-1"]);
  assert.deepEqual(signalEvents, []);
  assert.equal(runtime.window.__codeyCodexSessionController.kind, "manager");
});

test("deletes selected turns in place without evicting or resuming the conversation", async () => {
  const events = [];
  let runtime;
  runtime = loadInjection({
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1"],
    codexSessionController: createRecoveryController([], {
      async prepareMessageDeletion(sessionId) {
        assert.equal(sessionId, "session-1");
        events.push("prepare");
        assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-1", "turn-2"]);
      },
      async finishMessageDeletion(sessionId, messageIds) {
        assert.equal(sessionId, "session-1");
        assert.deepEqual([...messageIds], ["turn-1"]);
        events.push("finish");
      },
      async discardConversation() { assert.fail("must not evict the conversation"); },
      async resumeConversation() { assert.fail("must not resume the conversation"); },
      async refreshRecentConversations() { assert.fail("must not reload the sidebar"); },
    }),
    bridgeHandler: async (path) => {
      if (path !== "/session/delete-messages") return { status: "ok" };
      events.push("persist");
      assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-1", "turn-2"]);
      return { status: "ok", deleted: 1 };
    },
  });
  const remainingRow = runtime.getTurnRow(1);
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.deepEqual(events, ["prepare", "persist", "finish"]);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
  assert.equal(runtime.getTurnRow(1), remainingRow);
  assert.equal(runtime.getReloadCount(), 0);
  assert.deepEqual(runtime.alerts, []);
});

test("persistent deletion waits for the native subscription to be released", async () => {
  let release;
  let deleteCalls = 0;
  const runtime = loadInjection({
    selectedTurnIds: ["turn-1"],
    codexSessionController: createRecoveryController([], {
      prepareMessageDeletion() { return new Promise((resolve) => { release = resolve; }); },
    }),
    bridgeHandler: async (path) => {
      if (path === "/session/delete-messages") deleteCalls += 1;
      return { status: "ok", deleted: 1 };
    },
  });
  const pending = runtime.window.__codeyDeleteSelectedMessages();
  await flushMicrotasks();
  assert.equal(deleteCalls, 0);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-1"]);
  release();
  await pending;
  assert.equal(deleteCalls, 1);
});

test("reports a failed in-place update without reloading the conversation", async () => {
  const runtime = loadInjection({
    selectedTurnIds: ["turn-1"],
    codexSessionController: createRecoveryController([], {
      async finishMessageDeletion() { throw new Error("state update failed"); },
      async resumeConversation() { assert.fail("must not resume after failure"); },
    }),
    bridgeHandler: async () => ({ status: "ok", deleted: 1 }),
  });
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.equal(runtime.alerts.length, 1);
  assert.match(runtime.alerts[0], /页面状态更新失败/);
  assert.equal(runtime.getReloadCount(), 0);
});

test("deletion uses the visible composer and excludes selected rows in cached conversations", async () => {
  const hiddenComposer = new FakeElement({ "data-above-composer-conversation-id": "old-session" });
  hiddenComposer.layoutHidden = true;
  const visibleComposer = new FakeElement({ "data-above-composer-conversation-id": "local:current-session" });
  const hiddenAnnotation = new FakeElement({ "data-response-annotation-conversation": "old-session" });
  hiddenAnnotation.layoutHidden = true;
  const discarded = [];
  const runtime = loadInjection({
    initialSessionId: "stale-session",
    turnIds: ["old-turn", "current-turn"],
    hiddenTurnIds: ["old-turn"],
    selectedTurnIds: ["old-turn", "current-turn"],
    sessionElements: {
      "[data-above-composer-conversation-id]": [hiddenComposer, visibleComposer],
      "[data-response-annotation-conversation]": [hiddenAnnotation],
    },
    codexSessionController: createRecoveryController([], {
      async prepareMessageDeletion(sessionId) { discarded.push(sessionId); },
    }),
    bridgeHandler: async () => ({ status: "ok", deleted: 1 }),
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.equal(runtime.window.__codeyGetSessionId(), "current-session");
  assert.deepEqual(discarded, ["current-session"]);
  const requests = runtime.bridgeCalls.filter((call) => call.path === "/session/delete-messages");
  assert.equal(requests.length, 1);
  for (const request of requests) {
    assert.deepEqual(JSON.parse(JSON.stringify(request.payload)), {
      sessionId: "current-session", messageIds: ["current-turn"],
    });
  }
  assert.equal(runtime.getTurnRow(0).removed, false);
  assert.equal(runtime.getTurnRow(1).removed, true);
  assert.deepEqual(runtime.alerts, []);
});

test("switching retained conversations never copies selections into another session", async () => {
  const first = new FakeElement({ "data-above-composer-conversation-id": "first-session" });
  const second = new FakeElement({ "data-above-composer-conversation-id": "second-session" });
  second.layoutHidden = true;
  const runtime = loadInjection({
    turnIds: ["first-turn", "second-turn"],
    hiddenTurnIds: ["second-turn"],
    selectedTurnIds: ["first-turn"],
    sessionElements: { "[data-above-composer-conversation-id]": [first, second] },
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async () => ({ status: "ok", deleted: 1 }),
  });
  first.layoutHidden = true;
  second.layoutHidden = false;
  runtime.getTurnRow(0).layoutHidden = true;
  runtime.getTurnRow(1).layoutHidden = false;
  runtime.window.__codeyInstallMessageSelection();
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.equal(runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"), false);

  runtime.getTurnRow(1).classList.add("codey-message-selected");
  runtime.window.__codeyInstallMessageSelection();
  await runtime.window.__codeyDeleteSelectedMessages();
  const request = runtime.bridgeCalls.find((call) => call.path === "/session/delete-messages");
  assert.deepEqual(JSON.parse(JSON.stringify(request.payload)), {
    sessionId: "second-session", messageIds: ["second-turn"],
  });
  assert.equal(runtime.getTurnRow(0).removed, false);
});

test("serializes concurrent partial deletion requests", async () => {
  let release;
  let dispatchCalls = 0;
  let deleteCalls = 0;
  const runtime = loadInjection({
    turnIds: ["turn-1"], selectedTurnIds: ["turn-1"],
    codexSessionController: createRecoveryController([], {
      prepareMessageDeletion() {
        dispatchCalls += 1;
        return new Promise((resolve) => { release = resolve; });
      },
    }),
    bridgeHandler: async (path) => {
      if (path === "/session/delete-messages") deleteCalls += 1;
      return { status: "ok", deleted: 1 };
    },
  });

  const first = runtime.window.__codeyDeleteSelectedMessages();
  await flushMicrotasks();
  const second = runtime.window.__codeyDeleteSelectedMessages();
  await flushMicrotasks();

  assert.equal(dispatchCalls, 1);
  assert.equal(deleteCalls, 0);
  release();
  await Promise.all([first, second]);
  assert.equal(deleteCalls, 1);
});

test("does not hide a row when an index database remains unsupported", async () => {
  const runtime = loadInjection({
    turnIds: ["turn-unsupported"],
    selectedTurnIds: ["turn-unsupported"],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 1,
          unsupportedDatabases: ["thread_history_1.sqlite"],
        }
        : { status: "ok" }
    ),
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-unsupported"]);
  assert.match(runtime.alerts[0], /索引数据库未完成清理/);
});

test("failed host release never sends a destructive delete request", async () => {
  const runtime = loadInjection({
    turnIds: ["turn-1"], selectedTurnIds: ["turn-1"],
    codexSessionController: createRecoveryController([], {
      async prepareMessageDeletion() { throw new Error("unsubscribe failed"); },
    }),
  });
  await runtime.window.__codeyDeleteSelectedMessages();
  assert.equal(runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"), false);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-1"]);
  assert.match(runtime.alerts[0], /unsubscribe failed/);
});

test("removes a hard-deleted turn and rejects a stale React rerender", async () => {
  let deleteCalls = 0;
  const runtime = loadInjection({
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1"],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => {
      if (path !== "/session/delete-messages") return { status: "ok" };
      deleteCalls += 1;
      return { status: "ok", deleted: deleteCalls === 1 ? 1 : 0 };
    },
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
  assert.equal(runtime.getReloadCount(), 0);

  runtime.appendTurn("turn-1");
  runtime.window.__codeyInstallMessageSelection();
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
});

test("removes a hard-deleted turn remounted during the in-place update", async () => {
  let runtime;
  const controller = createRecoveryController([], {
    async finishMessageDeletion() {
      runtime.appendTurn("turn-1");
    },
  });
  runtime = loadInjection({
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1"],
    codexSessionController: controller,
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? { status: "ok", deleted: 1 }
        : { status: "ok" }
    ),
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
  runtime.flushTimers();
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
});

test("removes a hard-deleted fallback row before showing success", async () => {
  let runtime;
  let remountedRow;
  const controller = createRecoveryController([], {
    async finishMessageDeletion() {
      remountedRow = new FakeElement({
        "data-message-id": "turn-1",
        "data-testid": "conversation-turn",
      });
      runtime.appendExistingRow(remountedRow);
    },
  });
  runtime = loadInjection({
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1"],
    codexSessionController: controller,
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? { status: "ok", deleted: 1 }
        : { status: "ok" }
    ),
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.equal(remountedRow.removed, true);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-2"]);
});

test("updates both the resolved turn id and original tail selector without another deletion request", async () => {
  const tailKey = "history-content:tail:0:local:temporary-id";
  let deleteCalls = 0;
  let updatedIds;
  const runtime = loadInjection({
    turnIds: [tailKey],
    selectedTurnIds: [tailKey],
    codexSessionController: createRecoveryController([], {
      finishMessageDeletion(_sessionId, messageIds) { updatedIds = [...messageIds]; },
    }),
    bridgeHandler: async (path) => {
      if (path !== "/session/delete-messages") return { status: "ok" };
      deleteCalls += 1;
      return {
        status: "ok",
        deleted: deleteCalls === 1 ? 1 : 0,
        resolvedMessageIds: ["stable-last-turn"],
      };
    },
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  const deletions = runtime.bridgeCalls.filter(
    (call) => call.path === "/session/delete-messages",
  );
  assert.equal(deletions.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(deletions[0].payload.messageIds)), [tailKey]);
  assert.deepEqual(updatedIds, [tailKey, "stable-last-turn"]);
  assert.deepEqual(runtime.getVisibleTurnIds(), []);
  assert.deepEqual(runtime.alerts, []);
});

test("keeps a turn visible when no persisted turn was deleted", async () => {
  let deleteCalls = 0;
  let dispatcherCalls = 0;
  const runtime = loadInjection({
    turnIds: ["failed-turn"],
    selectedTurnIds: ["failed-turn"],
    codexSessionController: createRecoveryController([], {
      prepareMessageDeletion() { dispatcherCalls += 1; },
      finishMessageDeletion(_sessionId, messageIds) { assert.deepEqual([...messageIds], []); },
    }),
    bridgeHandler: async (path) => {
      if (path !== "/session/delete-messages") return { status: "ok" };
      deleteCalls += 1;
      return { status: "ok", deleted: 0 };
    },
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.equal(deleteCalls, 1);
  assert.equal(dispatcherCalls, 1);
  assert.deepEqual(runtime.getVisibleTurnIds(), ["failed-turn"]);
  assert.equal(runtime.alerts.length, 1);
  assert.match(runtime.alerts[0], /未在会话文件中找到所选轮次/);

  runtime.appendTurn("failed-turn");
  runtime.window.__codeyInstallMessageSelection();
  assert.deepEqual(runtime.getVisibleTurnIds(), ["failed-turn", "failed-turn"]);
});

test("reports a rejected delete bridge call without hiding the selected turn", async () => {
  const runtime = loadInjection({
    turnIds: ["bridge-failed-turn"],
    selectedTurnIds: ["bridge-failed-turn"],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => {
      if (path === "/session/delete-messages") throw new Error("bridge stopped");
      return { status: "ok" };
    },
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.deepEqual(runtime.getVisibleTurnIds(), ["bridge-failed-turn"]);
  assert.equal(runtime.alerts.length, 1);
  assert.match(runtime.alerts[0], /删除失败：bridge stopped/);
});

test("keeps all selected rows visible when only part of a delete is confirmed", async () => {
  const runtime = loadInjection({
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1", "turn-2"],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? { status: "ok", deleted: 1 }
        : { status: "ok" }
    ),
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-1", "turn-2"]);
  assert.equal(runtime.alerts.length, 1);
  assert.match(runtime.alerts[0], /只永久删除了 1\/2 轮对话/);
});

test("normalizes Codex history-content turn keys to rollout turn ids", () => {
  const runtime = loadInjection();
  const row = new FakeElement({
    "data-turn-key": "history-content:turn:019ff498-5f1c-7452-aac5-88e4eb99e657",
  });

  assert.equal(
    runtime.window.__codeyGetMessageId(row),
    "019ff498-5f1c-7452-aac5-88e4eb99e657",
  );
});

test("deletes an interrupted turn and its userless continuation as one logical round", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["stable-interrupted-turn", "stable-continued-turn"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement({ "data-message-author-role": "assistant" });
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "history-content:tail:1:local:temporary-interrupted",
  }), {
    items: [{ type: "userMessage" }, { type: "commandExecution" }],
    status: "interrupted",
    turnId: "stable-interrupted-turn",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "history-content:tail:0:local:temporary-continued",
  }), {
    items: [{ type: "commandExecution" }, { type: "final_answer" }],
    status: "completed",
    turnId: "stable-continued-turn",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);

  runtime.window.__codeyInstallMessageSelection(wrapper);

  assert.equal(interrupted.dataset.codeyMessageId, "stable-interrupted-turn");
  assert.equal(continued.dataset.codeyMessageId, "stable-continued-turn");
  assert.equal(interrupted.dataset.codeyLogicalTurn, "anchor");
  assert.equal(continued.dataset.codeyLogicalTurn, "continuation");
  const interruptedButton = messageSelectButton(interrupted);
  const continuedButton = messageSelectButton(continued);
  assert.equal(interruptedButton.hidden, false);
  assert.equal(continuedButton.hidden, true);

  interruptedButton.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  assert.equal(interrupted.classList.contains("codey-message-selected"), true);
  assert.equal(continued.classList.contains("codey-message-selected"), true);

  await runtime.window.__codeyDeleteSelectedMessages();

  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["stable-interrupted-turn", "stable-continued-turn"],
  });
  assert.match(runtime.confirmations[0], /删除 1 轮对话/);
  assert.deepEqual(runtime.getVisibleTurnIds(), []);
});

test("does not mistake a response id for a stable tail turn id", () => {
  const runtime = loadInjection();
  const tailKey = "history-content:tail:0:local:temporary-tail";
  const row = new FakeElement({ "data-turn-key": tailKey });
  row.__reactFiber$test = {
    memoizedProps: {
      response: { id: "resp-not-a-rollout-turn" },
    },
    return: null,
  };

  assert.equal(runtime.window.__codeyGetMessageId(row), tailKey);
});

test("upgrades an installed tail selector when React hydrates its stable turn id", () => {
  const runtime = loadInjection({ turnIds: [] });
  const tailKey = "history-content:tail:0:local:temporary-tail";
  const row = new TreeElement({ "data-turn-key": tailKey });
  row.isConnected = true;

  runtime.window.__codeyInstallMessageSelection(row);
  assert.equal(row.dataset.codeyMessageId, tailKey);

  row.__reactProps$test = {
    children: { props: { entry: { turnId: "hydrated-stable-turn" } } },
  };
  runtime.window.__codeyInstallMessageSelection(row);

  assert.equal(row.dataset.codeyMessageId, "hydrated-stable-turn");
});

test("regroups a selected interrupted request when its tail continuation hydrates", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-before-tail",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const tailKey = "history-content:tail:0:local:temporary-continuation";
  const continued = wrapper.appendChild(new TreeElement({ "data-turn-key": tailKey }));

  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");

  attachReactTurn(continued, {
    items: [{ type: "commandExecution" }],
    status: "completed",
    turnId: "hydrated-continuation",
  });
  runtime.window.__codeyInstallMessageSelection(wrapper);

  assert.equal(continued.dataset.codeyMessageId, "hydrated-continuation");
  assert.equal(continued.dataset.codeyLogicalTurn, "continuation");
  assert.equal(messageSelectButton(continued).hidden, true);
  assert.equal(interrupted.classList.contains("codey-message-selected"), true);
  assert.equal(continued.classList.contains("codey-message-selected"), true);
});

test("replaces a grouped tail placeholder with its hydrated stable turn id", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["turn-tail-origin", "turn-tail-stable"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-tail-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const tailKey = "history-content:tail:0:local:temporary-grouped-tail";
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": tailKey,
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
    turnId: tailKey,
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  assert.deepEqual(JSON.parse(interrupted.dataset.codeyMessageIds), [
    "turn-tail-origin",
    tailKey,
  ]);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  attachReactTurn(continued, {
    items: [{ type: "commandExecution" }, { type: "final_answer" }],
    status: "completed",
    turnId: "turn-tail-stable",
  });
  runtime.window.__codeyInstallMessageSelection();

  assert.deepEqual(JSON.parse(interrupted.dataset.codeyMessageIds), [
    "turn-tail-origin",
    "turn-tail-stable",
  ]);
  assert.equal(continued.classList.contains("codey-message-selected"), true);
  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-tail-origin", "turn-tail-stable"],
  });
});

test("waits for non-user continuation content before grouping an empty hydrated tail", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-empty-tail-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-empty-tail",
  }), {
    items: [],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);

  runtime.window.__codeyInstallMessageSelection(wrapper);
  assert.equal(interrupted.dataset.codeyLogicalTurn, "anchor");
  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");

  attachReactTurn(continued, {
    items: [{ type: "commandExecution" }],
    status: "completed",
  });
  runtime.window.__codeyInstallMessageSelection();

  assert.equal(continued.dataset.codeyLogicalTurn, "continuation");
  assert.deepEqual(JSON.parse(interrupted.dataset.codeyMessageIds), [
    "turn-empty-tail-origin",
    "turn-empty-tail",
  ]);
});

test("normalizes a selected standalone tail when hydration merges it into the origin", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["turn-selected-tail-origin", "turn-selected-tail"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-selected-tail-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-selected-tail",
  }), {
    items: [],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(continued).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  attachReactTurn(continued, {
    items: [{ type: "commandExecution" }],
    status: "completed",
  });
  runtime.window.__codeyInstallMessageSelection();
  assert.equal(interrupted.classList.contains("codey-message-selected"), true);
  assert.equal(continued.classList.contains("codey-message-selected"), true);

  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-selected-tail-origin", "turn-selected-tail"],
  });
  assert.match(runtime.confirmations[0], /删除 1 轮对话/);
});

test("deselecting a hydrated logical group clears the earlier standalone selection", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-cancel-tail-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-cancel-tail",
  }), {
    items: [],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(continued).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  attachReactTurn(continued, {
    items: [{ type: "commandExecution" }],
    status: "completed",
  });
  runtime.window.__codeyInstallMessageSelection();

  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await runtime.window.__codeyDeleteSelectedMessages();

  assert.equal(interrupted.classList.contains("codey-message-selected"), false);
  assert.equal(continued.classList.contains("codey-message-selected"), false);
  assert.equal(
    runtime.bridgeCalls.some((call) => call.path === "/session/delete-messages"),
    false,
  );
  assert.match(runtime.alerts.at(-1), /尚未选择任何一轮对话/);
});

test("retains every logical turn id when a continuation is virtualized away", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-virtual-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-virtual-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);

  runtime.window.__codeyInstallMessageSelection(wrapper);
  continued.remove();
  runtime.window.__codeyInstallMessageSelection();

  assert.deepEqual(JSON.parse(interrupted.dataset.codeyMessageIds), [
    "turn-virtual-origin",
    "turn-virtual-continuation",
  ]);
});

test("restores a complete logical group when only its continuation remounts", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["turn-remount-origin", "turn-remount-continuation"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-remount-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-remount-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);

  runtime.window.__codeyInstallMessageSelection(wrapper);
  interrupted.remove();
  continued.removed = false;
  runtime.window.__codeyInstallMessageSelection();

  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");
  assert.equal(messageSelectButton(continued).hidden, false);
  assert.deepEqual(JSON.parse(continued.dataset.codeyMessageIds), [
    "turn-remount-origin",
    "turn-remount-continuation",
  ]);
  messageSelectButton(continued).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await runtime.window.__codeyDeleteSelectedMessages();

  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-remount-origin", "turn-remount-continuation"],
  });
});

test("promotes a mounted continuation when removal mutation detaches its anchor", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-remove-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-remove-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  assert.equal(continued.dataset.codeyLogicalTurn, "continuation");

  wrapper.children.splice(wrapper.children.indexOf(interrupted), 1);
  interrupted.parentElement = null;
  interrupted.isConnected = false;
  interrupted.removed = true;
  runtime.emitMutations([{
    type: "childList",
    target: wrapper,
    addedNodes: [],
    removedNodes: [interrupted],
  }]);
  runtime.flushTimers();

  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");
  assert.equal(messageSelectButton(continued).hidden, false);
  assert.deepEqual(JSON.parse(continued.dataset.codeyMessageIds), [
    "turn-remove-origin",
    "turn-remove-continuation",
  ]);
});

test("restores selection when a logical group remounts through a new continuation node", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["turn-selected-origin", "turn-selected-continuation"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-selected-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-selected-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  for (const row of [interrupted, continued]) {
    const index = wrapper.children.indexOf(row);
    if (index >= 0) wrapper.children.splice(index, 1);
    row.parentElement = null;
    row.isConnected = false;
    row.removed = true;
  }
  const remounted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-selected-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(remounted);
  runtime.window.__codeyInstallMessageSelection(remounted);

  assert.equal(remounted.dataset.codeyLogicalTurn, "anchor");
  assert.equal(remounted.classList.contains("codey-message-selected"), true);
  assert.equal(messageSelectButton(remounted).getAttribute("aria-pressed"), "true");
  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-selected-origin", "turn-selected-continuation"],
  });
  assert.match(runtime.confirmations[0], /删除 1 轮对话/);
});

test("uses the selected group as topology after the ordinary logical cache is evicted", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["turn-evicted-origin", "turn-evicted-continuation"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-evicted-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-evicted-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  interrupted.removed = true;
  interrupted.isConnected = false;
  continued.removed = true;
  continued.isConnected = false;

  // Each filler consumes two topology keys. 2,048 groups displace the two
  // oldest target keys from the 4,096-key group-aware cache.
  for (let index = 0; index < 2_048; index += 1) {
    const filler = new TreeElement();
    filler.isConnected = true;
    filler.appendChild(attachReactTurn(new TreeElement({
      "data-turn-key": `turn-cache-filler-origin-${index}`,
    }), {
      items: [{ type: "userMessage" }],
      status: "interrupted",
    }));
    filler.appendChild(attachReactTurn(new TreeElement({
      "data-turn-key": `turn-cache-filler-continuation-${index}`,
    }), {
      items: [{ type: "commandExecution" }],
      status: "completed",
    }));
    runtime.window.__codeyInstallMessageSelection(filler);
  }

  const remountedOrigin = attachReactTurn(new TreeElement({
    "data-turn-key": "turn-evicted-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  });
  remountedOrigin.isConnected = true;
  runtime.appendExistingRow(remountedOrigin);
  runtime.window.__codeyInstallMessageSelection(remountedOrigin);

  assert.deepEqual(JSON.parse(remountedOrigin.dataset.codeyMessageIds), [
    "turn-evicted-origin",
    "turn-evicted-continuation",
  ]);
  assert.equal(remountedOrigin.classList.contains("codey-message-selected"), true);
  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-evicted-origin", "turn-evicted-continuation"],
  });
});

test("keeps the origin selected when a continuation row is reused for a new user turn", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 2,
          resolvedMessageIds: ["turn-reuse-origin", "turn-reuse-continuation"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-reuse-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const reused = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-reuse-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(reused);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  reused.setAttribute("data-turn-key", "turn-reuse-new-user");
  attachReactTurn(reused, {
    items: [{ type: "userMessage" }, { type: "final_answer" }],
    status: "completed",
    turnId: "turn-reuse-new-user",
  });
  runtime.window.__codeyInstallMessageSelection();

  assert.equal(interrupted.classList.contains("codey-message-selected"), true);
  assert.equal(reused.classList.contains("codey-message-selected"), false);
  assert.equal(reused.dataset.codeyLogicalTurn, "anchor");
  assert.deepEqual(JSON.parse(interrupted.dataset.codeyMessageIds), [
    "turn-reuse-origin",
    "turn-reuse-continuation",
  ]);

  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-reuse-origin", "turn-reuse-continuation"],
  });
  assert.deepEqual(runtime.getVisibleTurnIds(), ["turn-reuse-new-user"]);
});

test("keeps an explicitly mismatched continuation reference in a separate group", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 1,
          resolvedMessageIds: ["turn-explicit-continuation"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-explicit-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-explicit-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  assert.equal(continued.dataset.codeyLogicalTurn, "continuation");

  continued.__reactProps$test.children.props.entry.turn.resumedFromTurnId = "turn-other-origin";
  runtime.window.__codeyInstallMessageSelection();

  assert.equal(interrupted.dataset.codeyLogicalTurn, "anchor");
  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");
  assert.equal(messageSelectButton(continued).hidden, false);
  assert.deepEqual(JSON.parse(continued.dataset.codeyMessageIds), [
    "turn-explicit-continuation",
  ]);

  messageSelectButton(continued).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-explicit-continuation"],
  });
});

test("invalidates a cached group when a non-member turn appears between its rows", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-gap-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-gap-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  assert.equal(continued.dataset.codeyLogicalTurn, "continuation");

  const insertedUserTurn = attachReactTurn(new TreeElement({
    "data-turn-key": "turn-gap-new-user",
  }), {
    items: [{ type: "userMessage" }, { type: "final_answer" }],
    status: "completed",
  });
  insertedUserTurn.parentElement = wrapper;
  insertedUserTurn.isConnected = true;
  insertedUserTurn.removed = false;
  wrapper.children.splice(1, 0, insertedUserTurn);
  runtime.appendExistingRow(insertedUserTurn);
  runtime.window.__codeyInstallMessageSelection(wrapper);

  for (const row of [interrupted, insertedUserTurn, continued]) {
    assert.equal(row.dataset.codeyLogicalTurn, "anchor");
    assert.equal(messageSelectButton(row).hidden, false);
    assert.deepEqual(JSON.parse(row.dataset.codeyMessageIds), [
      row.dataset.codeyMessageId,
    ]);
  }
});

test("keeps only the old anchor selected when a cached continuation becomes a user turn", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 1,
          resolvedMessageIds: ["turn-split-origin"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-split-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-split-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  assert.equal(continued.classList.contains("codey-message-selected"), true);

  attachReactTurn(continued, {
    items: [{ type: "userMessage" }, { type: "final_answer" }],
    status: "completed",
  });
  runtime.window.__codeyInstallMessageSelection();

  assert.equal(interrupted.dataset.codeyLogicalTurn, "anchor");
  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");
  assert.equal(interrupted.classList.contains("codey-message-selected"), true);
  assert.equal(continued.classList.contains("codey-message-selected"), false);
  assert.equal(messageSelectButton(interrupted).getAttribute("aria-pressed"), "true");
  assert.equal(messageSelectButton(continued).getAttribute("aria-pressed"), "false");
  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-split-origin"],
  });
  assert.match(runtime.confirmations[0], /删除 1 轮对话/);
});

test("shrinks an offscreen selected group when its visible continuation becomes a user turn", async () => {
  const runtime = loadInjection({
    turnIds: [],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? {
          status: "ok",
          deleted: 1,
          resolvedMessageIds: ["turn-offscreen-origin"],
        }
        : { status: "ok" }
    ),
  });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interrupted = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-offscreen-origin",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const continued = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-offscreen-continuation",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));
  runtime.appendExistingRow(interrupted);
  runtime.appendExistingRow(continued);
  runtime.window.__codeyInstallMessageSelection(wrapper);
  messageSelectButton(interrupted).dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });

  wrapper.children.splice(wrapper.children.indexOf(interrupted), 1);
  interrupted.parentElement = null;
  interrupted.isConnected = false;
  interrupted.removed = true;
  attachReactTurn(continued, {
    items: [{ type: "userMessage" }, { type: "final_answer" }],
    status: "completed",
  });
  runtime.window.__codeyInstallMessageSelection();

  assert.equal(continued.dataset.codeyLogicalTurn, "anchor");
  assert.equal(continued.classList.contains("codey-message-selected"), false);
  await runtime.window.__codeyDeleteSelectedMessages();
  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-offscreen-origin"],
  });
  assert.match(runtime.confirmations[0], /删除 1 轮对话/);
});

test("clears selection when a virtualized row is reused for another stable turn", () => {
  const runtime = loadInjection({ turnIds: [] });
  const row = new TreeElement({ "data-turn-key": "turn-before-reuse" });
  row.isConnected = true;
  runtime.window.__codeyInstallMessageSelection(row);
  row.classList.add("codey-message-selected");

  row.setAttribute("data-turn-key", "turn-after-reuse");
  runtime.window.__codeyInstallMessageSelection(row);

  assert.equal(row.dataset.codeyMessageId, "turn-after-reuse");
  assert.equal(row.classList.contains("codey-message-selected"), false);
});

test("sends the normalized rollout turn id to the delete bridge", async () => {
  const uiTurnKey = "history-content:turn:019ff498-5f1c-7452-aac5-88e4eb99e657";
  const runtime = loadInjection({
    turnIds: [uiTurnKey],
    selectedTurnIds: [uiTurnKey],
    codexSessionController: createRecoveryController([]),
    bridgeHandler: async (path) => (
      path === "/session/delete-messages"
        ? { status: "ok", deleted: 1 }
        : { status: "ok" }
    ),
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  const deletion = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "session-1",
    messageIds: ["019ff498-5f1c-7452-aac5-88e4eb99e657"],
  });
  assert.deepEqual(runtime.getVisibleTurnIds(), []);
});

test("rescans a direct turn boundary without enumerating its subtree", () => {
  const runtime = loadInjection({ turnIds: ["turn-direct"] });
  const row = runtime.getTurnRow();
  row.querySelectorAllCalls.length = 0;

  runtime.window.__codeyInstallMessageSelection(row);

  assert.equal(row.querySelectorAllCalls.includes("[data-turn-key]"), false);
});

test("installs independent selection on canonical sibling turns inside a generic wrapper", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement({ "data-message-author-role": "assistant" });
  wrapper.isConnected = true;
  const first = wrapper.appendChild(new TreeElement({ "data-turn-key": "turn-first" }));
  const second = wrapper.appendChild(new TreeElement({ "data-turn-key": "turn-second" }));

  runtime.window.__codeyInstallMessageSelection(wrapper);

  assert.equal(wrapper.dataset.codeyMessageId, undefined);
  assert.equal(first.dataset.codeyMessageId, "turn-first");
  assert.equal(second.dataset.codeyMessageId, "turn-second");
});

test("keeps real user turns and post-completion background turns independent", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const interruptedUserTurn = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-interrupted-user",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const nextUserTurn = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-next-user",
  }), {
    items: [{ type: "userMessage" }, { type: "final_answer" }],
    status: "completed",
  }));
  const backgroundTurn = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-background",
  }), {
    items: [{ type: "commandExecution" }],
    status: "completed",
  }));

  runtime.window.__codeyInstallMessageSelection(wrapper);

  for (const row of [interruptedUserTurn, nextUserTurn, backgroundTurn]) {
    assert.equal(row.dataset.codeyLogicalTurn, "anchor");
    assert.equal(messageSelectButton(row).hidden, false);
  }
});

test("groups every adjacent userless segment whose predecessor was interrupted", () => {
  const runtime = loadInjection({ turnIds: [] });
  const wrapper = new TreeElement();
  wrapper.isConnected = true;
  const first = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-request",
  }), {
    items: [{ type: "userMessage" }],
    status: "interrupted",
  }));
  const second = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-resume-one",
  }), {
    items: [{ type: "commandExecution" }],
    status: "interrupted",
  }));
  const third = wrapper.appendChild(attachReactTurn(new TreeElement({
    "data-turn-key": "turn-resume-two",
  }), {
    items: [{ type: "final_answer" }],
    status: "completed",
  }));

  runtime.window.__codeyInstallMessageSelection(wrapper);

  assert.equal(first.dataset.codeyLogicalTurn, "anchor");
  assert.equal(second.dataset.codeyLogicalTurn, "continuation");
  assert.equal(third.dataset.codeyLogicalTurn, "continuation");
  assert.deepEqual(JSON.parse(first.dataset.codeyMessageIds), [
    "turn-request",
    "turn-resume-one",
    "turn-resume-two",
  ]);
});

test("does not install selection on nested canonical activity turns", () => {
  const runtime = loadInjection({ turnIds: [] });
  const outer = new TreeElement({ "data-turn-key": "turn-outer" });
  outer.isConnected = true;
  const activity = outer.appendChild(new TreeElement({ "data-turn-key": "turn-activity" }));

  runtime.window.__codeyInstallMessageSelection(outer);

  assert.equal(outer.dataset.codeyMessageId, "turn-outer");
  assert.equal(activity.dataset.codeyMessageId, undefined);
});

test("rescans every canonical sibling inside one newly added subtree", () => {
  const runtime = loadInjection({ turnIds: [] });
  const container = new TreeElement();
  container.isConnected = true;
  const wrapper = container.appendChild(new TreeElement());
  const first = wrapper.appendChild(new TreeElement({ "data-turn-key": "turn-new-first" }));
  const second = wrapper.appendChild(new TreeElement({ "data-turn-key": "turn-new-second" }));

  runtime.emitMutations([{
    type: "childList",
    target: container,
    addedNodes: [wrapper],
    removedNodes: [],
  }]);
  runtime.flushTimers();

  assert.equal(first.dataset.codeyMessageId, "turn-new-first");
  assert.equal(second.dataset.codeyMessageId, "turn-new-second");
});

test("installs selection on mixed Codex turn row shapes", () => {
  const runtime = loadInjection({
    turnIds: ["turn-keyed"],
  });
  const reactOnlyRow = new FakeElement({
    "data-testid": "conversation-turn",
  });
  reactOnlyRow.__reactFiber$test = {
    memoizedProps: {
      turn: { id: "history-content:turn:react-turn" },
    },
    return: null,
  };
  runtime.appendExistingRow(reactOnlyRow);

  runtime.window.__codeyInstallMessageSelection();

  assert.equal(reactOnlyRow.dataset.codeyMessageId, "react-turn");
});

test("extracts message ids from React turn state when DOM attributes omit ids", () => {
  const runtime = loadInjection();
  const row = new FakeElement({
    "data-testid": "conversation-turn",
  });
  row.__reactFiber$test = {
    memoizedProps: {
      children: {
        props: {
          message: {
            id: "history-content:turn:react-message",
          },
        },
      },
    },
    return: null,
  };

  assert.equal(runtime.window.__codeyGetMessageId(row), "react-message");
});

test("prefers React turn ids over response object ids", () => {
  const runtime = loadInjection();
  const row = new FakeElement({
    "data-testid": "conversation-turn",
  });
  row.__reactFiber$test = {
    memoizedProps: {
      response: { id: "resp-wrong-layer" },
      turn: { id: "history-content:turn:turn-right-layer" },
    },
    return: null,
  };

  assert.equal(runtime.window.__codeyGetMessageId(row), "turn-right-layer");
});

test("syncs Codex sidebar titles to the notification backend", async () => {
  const runtime = loadInjection({ sessionTitle: "修复飞书会话标题" });
  await new Promise((resolve) => setImmediate(resolve));

  const titleSync = runtime.bridgeCalls.find((call) => call.path === "/session/titles");
  assert.deepEqual(JSON.parse(JSON.stringify(titleSync?.payload)), {
    titles: [{ sessionId: "session-1", title: "修复飞书会话标题" }],
  });

  runtime.window.__codeySyncSidebarTitles();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    runtime.bridgeCalls.filter((call) => call.path === "/session/titles").length,
    1,
  );
});

test("bounds long-lived sidebar title cache entries", () => {
  const runtime = loadInjection();
  const rows = Array.from({ length: 2_049 }, (_, index) => new FakeElement({
    "data-app-action-sidebar-thread-id": `cache-session-${index}`,
    "data-app-action-sidebar-thread-title": `Cache title ${index}`,
  }));
  const root = {
    querySelectorAll(selector) {
      return selector ===
        "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]"
        ? rows
        : [];
    },
  };

  runtime.window.__codeySyncSidebarTitles(root);

  assert.equal(runtime.window.__codeyGetSessionTitle("cache-session-0"), "");
  assert.equal(
    runtime.window.__codeyGetSessionTitle("cache-session-2048"),
    "Cache title 2048",
  );
});

test("resolves a local project path from the current opaque project row id", () => {
  const runtime = loadInjection();
  const project = new FakeElement({
    "data-app-action-sidebar-project-id": "local-project-hash",
    "data-app-action-sidebar-project-row": "",
  });
  project.__reactFiber$test = {
    memoizedProps: {
      children: [{
        props: {
          group: {
            projectId: "local-project-hash",
            path: "/Users/test/workspace",
            projectKind: "local",
          },
        },
      }],
    },
    return: null,
  };

  assert.equal(
    runtime.window.__codeyProjectPathFromRow(project),
    "/Users/test/workspace",
  );
});

test("exports a session through ordered chunks and finalizes the transfer", async () => {
  const exported = Buffer.from("{\"format\":\"codey.session\",\"version\":1}");
  const chunkBytes = 11;
  const conversationId = "019f8339-ddc1-7652-8922-13e2b52d0d00";
  const written = [];
  const runtime = loadInjection({
    bridgeHandler: async (path, payload) => {
      if (path === "/session/export/start") {
        return {
          status: "ready",
          transferId: "export-transfer",
          filename: "session.codey-session.json",
          size: exported.length,
        };
      }
      if (path === "/session/export/chunk") {
        const bytes = exported.subarray(payload.offset, payload.offset + chunkBytes);
        const nextOffset = payload.offset + bytes.length;
        return {
          status: "ok",
          offset: payload.offset,
          nextOffset,
          data: bytes.toString("base64"),
          done: nextOffset === exported.length,
        };
      }
      if (path === "/session/export/finish") return { status: "ok" };
      return { status: "failed", message: `unexpected path: ${path}` };
    },
  });
  runtime.window.showSaveFilePicker = async () => ({
    createWritable: async () => ({
      abort: async () => {},
      close: async () => {},
      write: async (bytes) => written.push(Buffer.from(bytes)),
    }),
  });
  const thread = new FakeElement({
    "data-app-action-sidebar-thread-id": "local:client-new-thread:temporary-id",
  });
  thread.__reactFiber$test = {
    memoizedProps: {
      entry: { conversationId },
    },
    pendingProps: null,
    return: null,
  };
  const button = new FakeElement();

  await runtime.window.__codeyExportSession(thread, button);

  assert.equal(Buffer.concat(written).toString("utf8"), exported.toString("utf8"));
  assert.deepEqual(
    JSON.parse(JSON.stringify(
      runtime.bridgeCalls.find((call) => call.path === "/session/export/start")?.payload,
    )),
    { sessionId: conversationId },
  );
  assert.deepEqual(
    runtime.bridgeCalls
      .map((call) => call.path)
      .filter((path) => path.startsWith("/session/export/")),
    [
      "/session/export/start",
      "/session/export/chunk",
      "/session/export/chunk",
      "/session/export/chunk",
      "/session/export/chunk",
      "/session/export/finish",
    ],
  );
  assert.equal(button.disabled, false);
});

test("refreshes Codex recent sessions after importing instead of reloading", async () => {
  const signalCalls = [];
  const runtime = loadInjection({
    bridgeHandler: async (path, payload) => {
      if (path === "/session/import/start") {
        return {
          status: "ready",
          transferId: "transfer-1",
          chunkSize: 1024,
          maxBytes: 1024 * 1024,
        };
      }
      if (path === "/session/import/chunk") {
        return {
          status: "ok",
          nextOffset: payload.offset + Buffer.from(payload.data, "base64").length,
        };
      }
      if (path === "/session/import/finish") {
        return {
          status: "imported",
          sessionId: "imported-session",
          message: "会话数据已导入",
        };
      }
      return { status: "ok" };
    },
    codexSignalDispatcher: async (name, payload) => {
      signalCalls.push({ name, payload });
    },
  });
  const button = new FakeElement();

  await runtime.window.__codeyImportSessionFile(
    "/Users/test/workspace",
    { text: async () => "{\"format\":\"codey.session\"}" },
    button,
  );

  assert.deepEqual(JSON.parse(JSON.stringify(signalCalls)), [{
    name: "refresh-recent-conversations-for-host",
    payload: { hostId: "local" },
  }]);
  const chunkCall = runtime.bridgeCalls.find((call) => call.path === "/session/import/chunk");
  assert.equal(Buffer.from(chunkCall?.payload.data, "base64").toString("utf8"), "{\"format\":\"codey.session\"}");
  const finishCall = runtime.bridgeCalls.find((call) => call.path === "/session/import/finish");
  assert.deepEqual(JSON.parse(JSON.stringify(finishCall?.payload)), {
    transferId: "transfer-1",
    projectPath: "/Users/test/workspace",
  });
  assert.equal(runtime.getReloadCount(), 0);
  assert.equal(button.disabled, false);
});

test("imports from the tasks header using the project stored in the file", async () => {
  const runtime = loadInjection({
    bridgeHandler: async (path, payload) => {
      if (path === "/session/import/start") {
        return {
          status: "ready",
          transferId: "transfer-2",
          chunkSize: 1024,
          maxBytes: 1024 * 1024,
        };
      }
      if (path === "/session/import/chunk") {
        return {
          status: "ok",
          nextOffset: payload.offset + Buffer.from(payload.data, "base64").length,
        };
      }
      if (path === "/session/import/finish") {
        return {
          status: "imported",
          sessionId: "imported-session",
          projectPath: "/Users/test/task-project",
          message: "会话数据已导入",
        };
      }
      return { status: "ok" };
    },
    codexSignalDispatcher: async () => {},
  });
  const button = new FakeElement();

  await runtime.window.__codeyImportSessionFile(
    "",
    { text: async () => "{\"format\":\"codey.session\"}" },
    button,
  );

  const finishCall = runtime.bridgeCalls.find((call) => call.path === "/session/import/finish");
  assert.deepEqual(JSON.parse(JSON.stringify(finishCall?.payload)), {
    transferId: "transfer-2",
    projectPath: "",
  });
  assert.equal(button.disabled, false);
});
