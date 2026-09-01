import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
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
    return [1];
  }

  appendChild() {}

  addEventListener(...args) {
    return FakeElementCore.prototype.addEventListener.apply(this, args);
  }

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
  turnIds = ["turn-1"],
  sessionTitle = "排查飞书通知",
  bridgeHandler = null,
  codexSessionController = null,
  codexSignalDispatcher = null,
  discoveredAppServerManager = null,
  selectedTurnIds = [],
} = {}) {
  const rows = turnIds.map((turnId) => new FakeElement({ "data-turn-key": turnId }));
  rows.forEach((row) => {
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
  let sessionId = "session-1";
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
  if (discoveredAppServerManager) {
    const scope = {
      query: null,
      get() {},
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
      if (selector === "[data-session-id]") {
        return new FakeElement({ "data-session-id": sessionId });
      }
      return null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-turn-key]") {
        return rows.filter((row) => !row.removed && row.hasAttribute("data-turn-key"));
      }
      if (selector === "[data-turn-key], [data-message-author-role], [data-testid=conversation-turn], [data-message-id]") {
        return rows.filter((row) => !row.removed && row.matches(selector));
      }
      if (selector === "[data-codey-message-id]") {
        return rows.filter((row) => !row.removed && row.dataset.codeyMessageId);
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

const createRecoveryController = (events, overrides = {}) => ({
  kind: "manager",
  async discardConversation() {},
  async notifyConversationDeleted() {},
  async refreshRecentConversations() {},
  async reconcileCompletedConversation(payload) {
    events.push({ payload, type: "reconcile" });
    return true;
  },
  async resumeConversation() {},
  ...overrides,
});

const flushMicrotasks = async () => {
  await new Promise((resolve) => setImmediate(resolve));
};

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

test("rejects a reconciliation result when the visible session changes in flight", async () => {
  const events = [];
  let deferReconcile = false;
  let resolveReconcile;
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events, {
      async reconcileCompletedConversation(payload) {
        events.push({ payload, type: "reconcile" });
        if (!deferReconcile) return false;
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
  resolveReconcile(true);

  assert.equal(await reconciliation, false);
  assert.deepEqual(
    events.map((event) => event.payload.conversationId),
    ["session-1", "session-1"],
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

test("unloads Codex memory without discarding the active conversation", async () => {
  const dispatcherCalls = [];
  const events = [];
  const runtime = loadInjection({
    codexSignalDispatcher: async (signal, payload) => {
      dispatcherCalls.push({ signal, payload });
      events.push(`signal:${signal}`);
    },
    bridgeHandler: async (path) => {
      events.push(`bridge:${path}`);
      return path === "/session/delete-messages"
        ? { status: "ok", deleted: 0 }
        : { status: "ok" };
    },
  });
  events.length = 0;

  await runtime.window.__codeyReloadConversationAfterHardDelete(
    "local:session-1",
    ["turn-deleted"],
  );

  assert.deepEqual(JSON.parse(JSON.stringify(dispatcherCalls)), [{
    signal: "unsubscribe-thread-for-host",
    payload: {
      hostId: "local",
      threadId: "session-1",
    },
  }, {
    signal: "maybe-resume-conversation",
    payload: {
      hostId: "local",
      conversationId: "session-1",
      model: null,
      serviceTier: null,
      reasoningEffort: null,
      workspaceRoots: [],
      collaborationMode: null,
    },
  }, {
    signal: "refresh-recent-conversations-for-host",
    payload: { hostId: "local" },
  }]);
  assert.equal(
    dispatcherCalls.some(({ signal }) => signal === "discard-conversation-from-cache"),
    false,
  );
  assert.deepEqual(events, [
    "signal:unsubscribe-thread-for-host",
    "bridge:/session/delete-messages",
    "signal:maybe-resume-conversation",
    "signal:refresh-recent-conversations-for-host",
  ]);
  const cleanup = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(cleanup?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-deleted"],
  });
});

test("uses the current AppServerManager flow to evict, clean, resume, and refresh", async () => {
  const events = [];
  const managerCalls = [];
  const runtime = loadInjection({
    codexSessionController: {
      kind: "manager",
      async discardConversation(sessionId) {
        managerCalls.push({ method: "discardConversation", sessionId });
        events.push("manager:discard");
      },
      async notifyConversationDeleted(sessionId) {
        managerCalls.push({ method: "notifyConversationDeleted", sessionId });
      },
      async refreshRecentConversations() {
        managerCalls.push({ method: "refreshRecentConversations" });
        events.push("manager:refresh");
      },
      async resumeConversation(payload) {
        managerCalls.push({ method: "resumeConversation", payload });
        events.push("manager:resume");
      },
    },
    bridgeHandler: async (path) => {
      events.push(`bridge:${path}`);
      return path === "/session/delete-messages"
        ? { status: "ok", deleted: 0 }
        : { status: "ok" };
    },
  });
  events.length = 0;

  await runtime.window.__codeyReloadConversationAfterHardDelete(
    "local:session-1",
    ["turn-deleted"],
  );

  assert.deepEqual(events, [
    "manager:discard",
    "bridge:/session/delete-messages",
    "manager:resume",
    "manager:refresh",
  ]);
  assert.deepEqual(JSON.parse(JSON.stringify(managerCalls)), [{
    method: "discardConversation",
    sessionId: "session-1",
  }, {
    method: "resumeConversation",
    payload: {
      collaborationMode: null,
      conversationId: "session-1",
      model: null,
      reasoningEffort: null,
      serviceTier: null,
      showThreadGoalResumeConfirmation: false,
      workspaceRoots: [],
    },
  }, {
    method: "refreshRecentConversations",
  }]);
});

test("removes a hard-deleted turn and rejects a stale React rerender", async () => {
  let deleteCalls = 0;
  const runtime = loadInjection({
    turnIds: ["turn-1", "turn-2"],
    selectedTurnIds: ["turn-1"],
    codexSignalDispatcher: async () => {},
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

test("reuses the resolved turn id when cleaning up a deleted tail turn", async () => {
  const tailKey = "history-content:tail:0:local:temporary-id";
  let deleteCalls = 0;
  const runtime = loadInjection({
    turnIds: [tailKey],
    selectedTurnIds: [tailKey],
    codexSignalDispatcher: async () => {},
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
  assert.equal(deletions.length, 2);
  assert.deepEqual(JSON.parse(JSON.stringify(deletions[0].payload.messageIds)), [tailKey]);
  assert.deepEqual(JSON.parse(JSON.stringify(deletions[1].payload.messageIds)), [
    "stable-last-turn",
  ]);
  assert.deepEqual(runtime.getVisibleTurnIds(), []);
  assert.deepEqual(runtime.alerts, []);
});

test("keeps a turn visible when no persisted turn was deleted", async () => {
  let deleteCalls = 0;
  let dispatcherCalls = 0;
  const runtime = loadInjection({
    turnIds: ["failed-turn"],
    selectedTurnIds: ["failed-turn"],
    codexSignalDispatcher: async () => {
      dispatcherCalls += 1;
    },
    bridgeHandler: async (path) => {
      if (path !== "/session/delete-messages") return { status: "ok" };
      deleteCalls += 1;
      return { status: "ok", deleted: 0 };
    },
  });

  await runtime.window.__codeyDeleteSelectedMessages();

  assert.equal(deleteCalls, 1);
  assert.equal(dispatcherCalls, 0);
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
    codexSignalDispatcher: async () => {},
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
