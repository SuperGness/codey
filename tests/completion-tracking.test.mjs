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

  addEventListener() {}

  remove() {
    this.removed = true;
  }
}

function loadInjection({
  initialRunning = true,
  initialNow = 1_000_000,
  turnIds = ["turn-1"],
  sessionTitle = "排查飞书通知",
  stopButtonAttributes = { "aria-label": "停止" },
  bridgeHandler = null,
  codexSessionController = null,
  codexSignalDispatcher = null,
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
  const stopButton = new FakeElement(stopButtonAttributes);
  let running = initialRunning;
  let now = initialNow;
  let sessionId = "session-1";
  const bridgeCalls = [];
  const alerts = [];
  let reloadCount = 0;
  const timers = [];
  const toolbar = new FakeElement();
  const placeholder = new FakeElement();
  const documentElement = new FakeElement();
  const document = {
    documentElement,
    body: new FakeElement(),
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
      if (selector === "button[aria-label], button[title], button[data-testid]") {
        return running ? [stopButton] : [];
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
    confirm: () => true,
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
  window.window = window;
  const MutationObserver = class {
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
    getReloadCount: () => reloadCount,
    getTurnRow: (index = 0) => rows[index] || null,
    getVisibleTurnIds: () => rows
      .filter((row) => !row.removed)
      .map((row) => row.getAttribute("data-turn-key")),
    setRunning: (value) => {
      running = Boolean(value);
    },
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

const completedProbeResult = (overrides = {}) => ({
  status: "ok",
  sessionId: "session-1",
  turnId: "turn-1",
  sessionKnown: true,
  turnKnown: true,
  lifecycle: "idle",
  activityRevision: "100:1000",
  terminal: true,
  terminalKind: "completed",
  completedAt: 42,
  ...overrides,
});

const createRecoveryController = (events, overrides = {}) => ({
  kind: "manager",
  async discardConversation(sessionId) {
    events.push(`discard:${sessionId}`);
  },
  async notifyConversationDeleted() {},
  async refreshRecentConversations() {
    events.push("refresh");
  },
  async rehydrateRunningConversation(payload) {
    events.push(`rehydrate:${payload.conversationId}`);
    return true;
  },
  async resumeConversation(payload) {
    events.push(`resume:${payload.conversationId}`);
  },
  ...overrides,
});

test("waits for the stuck-running grace period before probing completion", async () => {
  const runtime = loadInjection({
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult({ lifecycle: "running", terminal: false, terminalKind: null })
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(29_999);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.equal(
    runtime.bridgeCalls.filter((call) => call.path === "/session/completion-state").length,
    0,
  );

  runtime.advanceTime(1);
  await runtime.window.__codeyProbeStuckTaskCompletion();
  assert.equal(
    runtime.bridgeCalls.filter((call) => call.path === "/session/completion-state").length,
    1,
  );
});

test("recognizes current Codex stop controls by title or test id", async () => {
  for (const stopButtonAttributes of [
    { title: "Stop response" },
    { "data-testid": "stop-generation-button" },
    { "data-testid": "response-stop-button" },
  ]) {
    const runtime = loadInjection({
      stopButtonAttributes,
      bridgeHandler: async () => completedProbeResult({
        lifecycle: "running",
        terminal: false,
        terminalKind: null,
      }),
    });
    runtime.advanceTime(30_000);
    await runtime.window.__codeyProbeStuckTaskCompletion();
    assert.equal(
      runtime.bridgeCalls.filter((call) => call.path === "/session/completion-state").length,
      1,
    );
  }
});

test("does not treat an unrelated voice stop control as task generation", async () => {
  const runtime = loadInjection({
    stopButtonAttributes: { "data-testid": "stop-voice-input" },
  });
  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.equal(
    runtime.bridgeCalls.filter((call) => call.path === "/session/completion-state").length,
    0,
  );
});

test("probes the outer conversation turn instead of a nested activity turn", async () => {
  const runtime = loadInjection({
    turnIds: ["turn-outer"],
    bridgeHandler: async () => ({
      status: "ok",
      sessionId: "session-1",
      turnId: "turn-outer",
      sessionKnown: true,
      turnKnown: true,
      lifecycle: "running",
      terminal: false,
      terminalKind: null,
    }),
  });
  const nested = new FakeElement({ "data-turn-key": "turn-nested" });
  nested.parentElement = runtime.getTurnRow();
  runtime.appendExistingRow(nested);

  runtime.advanceTime(30_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();

  const probe = runtime.bridgeCalls.find((call) => call.path === "/session/completion-state");
  assert.deepEqual(JSON.parse(JSON.stringify(probe?.payload)), {
    sessionId: "session-1",
    turnId: "turn-outer",
  });
  assert.deepEqual(JSON.parse(JSON.stringify(probe?.options)), { timeoutMs: 10_000 });
});

test("skips the Codex history tail placeholder when probing the current turn", async () => {
  const runtime = loadInjection({
    turnIds: [
      "history-content:turn:turn-real",
      "history-content:tail:0:local:temporary-tail",
    ],
    bridgeHandler: async () => ({
      status: "ok",
      sessionId: "session-1",
      turnId: "turn-real",
      sessionKnown: true,
      turnKnown: true,
      lifecycle: "running",
      terminal: false,
      terminalKind: null,
    }),
  });

  runtime.advanceTime(30_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();

  const probe = runtime.bridgeCalls.find((call) => call.path === "/session/completion-state");
  assert.deepEqual(JSON.parse(JSON.stringify(probe?.payload)), {
    sessionId: "session-1",
    turnId: "turn-real",
  });
});

test("probes the stable React turn hydrated into the current history tail row", async () => {
  const runtime = loadInjection({
    initialRunning: false,
    turnIds: [
      "history-content:turn:turn-previous",
      "history-content:tail:0:local:temporary-tail",
    ],
    bridgeHandler: async () => ({
      status: "ok",
      sessionId: "session-1",
      turnId: "turn-current",
      sessionKnown: true,
      turnKnown: true,
      lifecycle: "running",
      terminal: false,
      terminalKind: null,
    }),
  });
  runtime.getTurnRow(1).__reactProps$test = {
    children: { props: { entry: { turnId: "turn-current" } } },
  };
  runtime.setRunning(true);

  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  runtime.advanceTime(30_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();

  const probe = runtime.bridgeCalls.find((call) => call.path === "/session/completion-state");
  assert.deepEqual(JSON.parse(JSON.stringify(probe?.payload)), {
    sessionId: "session-1",
    turnId: "turn-current",
  });
});

test("does not recover while the authoritative lifecycle is running or waiting", async () => {
  const events = [];
  let lifecycle = "running";
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult({ lifecycle, terminal: false, terminalKind: null })
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  lifecycle = "waiting";
  runtime.advanceTime(15_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.deepEqual(events, []);
});

test("rehydrates a running task only after rollout activity advances while its rendered tail stays frozen", async () => {
  const events = [];
  let activityRevision = "100:1000";
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult({
          activityRevision,
          lifecycle: "running",
          terminal: false,
          terminalKind: null,
        })
        : { status: "ok" }
    ),
  });

  runtime.setTurnText(0, "页面停在这里");
  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);

  activityRevision = "200:2000";
  runtime.advanceTime(15_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  activityRevision = "300:3000";
  runtime.advanceTime(15_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  activityRevision = "400:4000";
  runtime.advanceTime(15_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);

  assert.deepEqual(events, ["rehydrate:session-1"]);
});

test("resets running stream detachment evidence whenever the current turn renders progress", async () => {
  const events = [];
  let activityRevision = "100:1000";
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async () => completedProbeResult({
      activityRevision,
      lifecycle: "running",
      terminal: false,
      terminalKind: null,
    }),
  });

  runtime.setTurnText(0, "第一段");
  runtime.advanceTime(30_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();
  activityRevision = "200:2000";
  runtime.advanceTime(15_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();

  runtime.setTurnText(0, "第一段\n页面已收到新内容");
  activityRevision = "300:3000";
  runtime.advanceTime(15_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();
  activityRevision = "400:4000";
  runtime.advanceTime(15_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();
  activityRevision = "500:5000";
  runtime.advanceTime(15_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.deepEqual(events, []);

  activityRevision = "600:6000";
  runtime.advanceTime(15_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  assert.deepEqual(events, ["rehydrate:session-1"]);
});

test("keeps a running task untouched when its rollout is not advancing", async () => {
  const events = [];
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async () => completedProbeResult({
      activityRevision: "100:1000",
      lifecycle: "running",
      terminal: false,
      terminalKind: null,
    }),
  });

  runtime.advanceTime(30_000);
  await runtime.window.__codeyProbeStuckTaskCompletion();
  for (let index = 0; index < 8; index += 1) {
    runtime.advanceTime(15_000);
    assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  }
  assert.deepEqual(events, []);
});

test("retries a completed task when native recovery resolves but the Stop state remains", async () => {
  const events = [];
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult()
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  assert.deepEqual(events, ["discard:session-1", "resume:session-1", "refresh"]);

  runtime.advanceTime(29_999);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  runtime.advanceTime(1);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  assert.deepEqual(events, [
    "discard:session-1", "resume:session-1", "refresh",
    "discard:session-1", "resume:session-1", "refresh",
  ]);

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  runtime.advanceTime(299_999);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  runtime.advanceTime(1);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  assert.equal(events.filter((event) => event === "refresh").length, 4);
});

test("clears completed-task retry history after the native Stop state disappears", async () => {
  const events = [];
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult()
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  runtime.setRunning(false);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);

  runtime.setRunning(true);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  assert.equal(events.filter((event) => event === "refresh").length, 2);
});

test("rejects mismatched completion confirmation", async () => {
  const events = [];
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult({ turnId: "turn-other" })
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.deepEqual(events, []);
});

test("cancels recovery when the visible task changes during confirmation", async () => {
  const events = [];
  let resolveCompletion;
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => {
      if (path !== "/session/completion-state") return { status: "ok" };
      return new Promise((resolve) => {
        resolveCompletion = resolve;
      });
    },
  });

  runtime.advanceTime(30_000);
  const probe = runtime.window.__codeyProbeStuckTaskCompletion();
  await Promise.resolve();
  runtime.setSessionId("session-2");
  resolveCompletion(completedProbeResult());

  assert.equal(await probe, false);
  assert.deepEqual(events, []);
});

test("cancels recovery when Codex clears its native running state", async () => {
  const events = [];
  let resolveCompletion;
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events),
    bridgeHandler: async (path) => {
      if (path !== "/session/completion-state") return { status: "ok" };
      return new Promise((resolve) => {
        resolveCompletion = resolve;
      });
    },
  });

  runtime.advanceTime(30_000);
  const probe = runtime.window.__codeyProbeStuckTaskCompletion();
  await Promise.resolve();
  runtime.setRunning(false);
  resolveCompletion(completedProbeResult());

  assert.equal(await probe, false);
  assert.deepEqual(events, []);
});

test("backs off after a native recovery failure without reloading", async () => {
  let discardCalls = 0;
  const runtime = loadInjection({
    codexSessionController: createRecoveryController([], {
      async discardConversation() {
        discardCalls += 1;
        throw new Error("controller failed");
      },
    }),
    bridgeHandler: async (path) => (
      path === "/session/completion-state"
        ? completedProbeResult()
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.equal(discardCalls, 1);
  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.equal(discardCalls, 1);
  runtime.advanceTime(31_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  assert.equal(discardCalls, 2);
  assert.equal(runtime.getReloadCount(), 0);
});

test("scopes native recovery cooldown to the failed task", async () => {
  const events = [];
  let failNextDiscard = true;
  const runtime = loadInjection({
    codexSessionController: createRecoveryController(events, {
      async discardConversation(sessionId) {
        if (failNextDiscard) {
          failNextDiscard = false;
          throw new Error("controller failed");
        }
        events.push(`discard:${sessionId}`);
      },
    }),
    bridgeHandler: async (path, payload) => (
      path === "/session/completion-state"
        ? completedProbeResult({ sessionId: payload.sessionId, turnId: payload.turnId })
        : { status: "ok" }
    ),
  });

  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);

  runtime.setSessionId("session-2");
  runtime.appendTurn("turn-2");
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), false);
  runtime.advanceTime(30_000);
  assert.equal(await runtime.window.__codeyProbeStuckTaskCompletion(), true);
  assert.deepEqual(events, ["discard:session-2", "resume:session-2", "refresh"]);
});

test("unloads Codex memory without discarding the active conversation", async () => {
  const dispatcherCalls = [];
  const events = [];
  const runtime = loadInjection({
    initialRunning: false,
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
    initialRunning: false,
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
    initialRunning: false,
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
    initialRunning: false,
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
    initialRunning: false,
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
    initialRunning: false,
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
    initialRunning: false,
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

test("sends the normalized rollout turn id to the delete bridge", async () => {
  const uiTurnKey = "history-content:turn:019ff498-5f1c-7452-aac5-88e4eb99e657";
  const runtime = loadInjection({
    initialRunning: false,
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
