import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement {
  constructor(attributes = {}) {
    this.attributes = new Map(Object.entries(attributes));
    this.dataset = {};
    this.disabled = false;
    this.parentElement = null;
    this.textContent = "";
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }

  hasAttribute(name) {
    return this.attributes.has(name);
  }

  querySelector(selector) {
    if (selector === "[data-codey-message-select]") return {};
    if (selector === "[data-local-conversation-final-assistant]") return {};
    return null;
  }

  querySelectorAll() {
    if (this.getAttribute("data-terminal-error") === "true") {
      return [new FakeElement({ "data-status": "failed" })];
    }
    return [];
  }

  closest() {
    return null;
  }

  getClientRects() {
    return [1];
  }

  appendChild() {}

  addEventListener() {}

  remove() {}
}

function loadInjection({
  initialRunning = true,
  turnIds = ["turn-1"],
  sessionTitle = "排查飞书通知",
  bridgeHandler = null,
  codexSignalDispatcher = null,
} = {}) {
  const rows = turnIds.map((turnId) => new FakeElement({ "data-turn-key": turnId }));
  const sidebarThread = new FakeElement({
    "data-app-action-sidebar-thread-id": "local:session-1",
    "data-app-action-sidebar-thread-title": sessionTitle,
  });
  const stopButton = new FakeElement({ "aria-label": "停止" });
  let running = initialRunning;
  const bridgeCalls = [];
  let reloadCount = 0;
  const timers = [];
  const toolbar = new FakeElement();
  const placeholder = new FakeElement();
  const documentElement = new FakeElement();
  const document = {
    documentElement,
    body: new FakeElement(),
    getElementById(id) {
      if (id === "codey-injected-style" || id === "codey-settings-button") return placeholder;
      if (id === "codey-message-toolbar") return toolbar;
      return null;
    },
    querySelector(selector) {
      if (selector === "[data-session-id]") {
        return new FakeElement({ "data-session-id": "session-1" });
      }
      return null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-turn-key]") return rows;
      if (selector === "button[aria-label]") return running ? [stopButton] : [];
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
    __codexSessionDeleteBridge: async (path, payload) => {
      bridgeCalls.push({ path, payload });
      if (bridgeHandler) return bridgeHandler(path, payload);
      return { status: "ok" };
    },
    __codeyCodexSignalDispatcher: codexSignalDispatcher,
    addEventListener: () => {},
    clearTimeout: () => {},
    dispatchEvent: () => true,
    getComputedStyle: () => ({ display: "block", visibility: "visible" }),
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
  vm.runInNewContext(source, {
    console,
    CustomEvent: class {
      constructor(type, options = {}) {
        this.type = type;
        this.detail = options.detail;
      }
    },
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
    URLSearchParams,
    window,
  });
  return {
    appendTurn: (turnId) => {
      rows.push(new FakeElement({ "data-turn-key": turnId }));
    },
    bridgeCalls,
    getReloadCount: () => reloadCount,
    window,
  };
}

test("unloads Codex memory before reapplying a permanent message deletion", async () => {
  const dispatcherCalls = [];
  const runtime = loadInjection({
    initialRunning: false,
    codexSignalDispatcher: async (signal, payload) => {
      dispatcherCalls.push({ signal, payload });
    },
    bridgeHandler: async (path) => path === "/session/delete-messages"
      ? { status: "ok", deleted: 0 }
      : { status: "ok" },
  });

  await runtime.window.__codeyReloadConversationAfterHardDelete(
    "local:session-1",
    ["turn-deleted"],
  );

  assert.deepEqual(JSON.parse(JSON.stringify(dispatcherCalls)), [{
    signal: "discard-conversation-from-cache",
    payload: { hostId: "local", conversationId: "session-1" },
  }, {
    signal: "refresh-recent-conversations-for-host",
    payload: { hostId: "local", sortKey: "updated_at" },
  }]);
  const cleanup = runtime.bridgeCalls.find(
    (call) => call.path === "/session/delete-messages",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(cleanup?.payload)), {
    sessionId: "session-1",
    messageIds: ["turn-deleted"],
  });
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

test("refreshes Codex recent sessions after importing instead of reloading", async () => {
  const signalCalls = [];
  const runtime = loadInjection({
    bridgeHandler: async (path) => {
      if (path === "/session/import") {
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
    payload: { hostId: "local", sortKey: "updated_at" },
  }]);
  const importCall = runtime.bridgeCalls.find((call) => call.path === "/session/import");
  assert.deepEqual(JSON.parse(JSON.stringify(importCall?.payload)), {
    projectPath: "/Users/test/workspace",
    data: "{\"format\":\"codey.session\"}",
  });
  assert.equal(runtime.getReloadCount(), 0);
  assert.equal(button.disabled, false);
});

test("imports from the tasks header using the project stored in the file", async () => {
  const runtime = loadInjection({
    bridgeHandler: async (path) => {
      if (path === "/session/import") {
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

  const importCall = runtime.bridgeCalls.find((call) => call.path === "/session/import");
  assert.deepEqual(JSON.parse(JSON.stringify(importCall?.payload)), {
    projectPath: "",
    data: "{\"format\":\"codey.session\"}",
  });
  assert.equal(button.disabled, false);
});
