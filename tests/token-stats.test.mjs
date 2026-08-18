import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(
  new URL("../public/token-stats.js", import.meta.url),
  "utf8",
);

function createEnvironment(options = {}) {
  const bridgeCalls = [];
  const observers = [];
  const windowListeners = new Map();
  const documentListeners = new Map();
  let clock = 0;
  let timerId = 0;

  const tokenStats = options.tokenStats ?? {
    status: "ok",
    reasonCode: "ok",
    durationMs: 2700,
    inputTokens: 40,
    outputTokens: 90,
    totalTokens: 130,
  };
  const config = { showTokenStatsCard: options.enabled ?? true };

  class Element {
    constructor(tagName = "div") {
      this.tagName = String(tagName).toUpperCase();
      this.children = [];
      this.parentElement = null;
      this.isConnected = false;
      this.textContent = "";
      this.dataset = {};
      this.style = {};
      this.attributes = new Map();
      this.listeners = new Map();
    }

    addEventListener(type, handler) {
      const handlers = this.listeners.get(type) || [];
      handlers.push(handler);
      this.listeners.set(type, handlers);
    }

    removeEventListener(type, handler) {
      const handlers = (this.listeners.get(type) || []).filter(
        (candidate) => candidate !== handler,
      );
      this.listeners.set(type, handlers);
    }

    appendChild(child) {
      child.remove();
      child.parentElement = this;
      child.isConnected = true;
      this.children.push(child);
      return child;
    }

    insertBefore(child, reference) {
      child.remove();
      const index = this.children.indexOf(reference);
      if (index < 0) return this.appendChild(child);
      child.parentElement = this;
      child.isConnected = true;
      this.children.splice(index, 0, child);
      return child;
    }

    remove() {
      if (!this.parentElement) return;
      const index = this.parentElement.children.indexOf(this);
      if (index >= 0) this.parentElement.children.splice(index, 1);
      this.parentElement = null;
      this.isConnected = false;
    }

    matches(selector) {
      return matchSelector(this, selector);
    }

    closest(selector) {
      let element = this;
      while (element) {
        if (matchSelector(element, selector)) return element;
        element = element.parentElement;
      }
      return null;
    }

    querySelectorAll() {
      return [];
    }

    getAttribute(name) {
      return this.attributes.has(name) ? this.attributes.get(name) : null;
    }

    setAttribute(name, value) {
      this.attributes.set(name, String(value));
    }

    contains(node) {
      if (node === this) return true;
      return this.children.some((child) => child.contains(node));
    }
  }

  function matchSelector(element, selector) {
    const value = String(selector).trim();
    if (value.startsWith("#")) {
      return element.getAttribute("id") === value.slice(1);
    }
    const attribute = value.match(/^\[([^=\]]+)(?:=['"]?([^'"\]]+)['"]?)?\]$/);
    if (attribute) {
      const actual = element.getAttribute(attribute[1]);
      return attribute[2] === undefined
        ? actual !== null
        : actual === attribute[2];
    }
    return element.tagName === value.toUpperCase();
  }

  class HTMLElement extends Element {}

  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      this.observed = false;
      this.target = null;
      this.options = null;
      this.disconnectCalls = 0;
      observers.push(this);
    }

    observe(target, options) {
      this.observed = true;
      this.target = target;
      this.options = options;
    }

    disconnect() {
      this.observed = false;
      this.disconnectCalls += 1;
    }
  }

  const documentElement = new HTMLElement("html");
  const body = new HTMLElement("body");
  documentElement.appendChild(body);

  const document = {
    documentElement,
    body,
    visibilityState: "visible",
    createElement: (tagName) => new HTMLElement(tagName),
    getElementById: () => null,
    querySelector: () => null,
    querySelectorAll: () => [],
    addEventListener(type, handler) {
      const handlers = documentListeners.get(type) || [];
      handlers.push(handler);
      documentListeners.set(type, handlers);
    },
  };

  const window = {
    document,
    performance: { now: () => clock },
    setTimeout: (callback) => {
      const id = ++timerId;
      callback();
      return id;
    },
    clearTimeout: () => {},
    addEventListener(type, handler) {
      const handlers = windowListeners.get(type) || [];
      handlers.push(handler);
      windowListeners.set(type, handlers);
    },
    removeEventListener(type, handler) {
      const handlers = (windowListeners.get(type) || []).filter(
        (candidate) => candidate !== handler,
      );
      windowListeners.set(type, handlers);
    },
  };

  const bridge = async (path, payload) => {
    bridgeCalls.push({ path, payload });
    if (path === "/settings/get") return config;
    if (path === "/token-stats") return tokenStats;
    return { status: "failed" };
  };
  if (options.bridgeReady !== false) {
    window.__codexSessionDeleteBridge = bridge;
  }

  const sandbox = {
    document,
    window,
    MutationObserver: FakeMutationObserver,
    Element,
    HTMLElement,
    performance: { now: () => clock },
    setTimeout: (callback) => {
      callback();
      return ++timerId;
    },
    clearTimeout: () => {},
  };
  const context = vm.createContext(sandbox);
  vm.runInContext(source, context);

  const dispatchWindow = (type, detail) => {
    for (const handler of windowListeners.get(type) || []) {
      handler.call(window, { type, detail, target: window });
    }
  };

  const settle = () => new Promise((resolve) => setTimeout(resolve, 10));

  return {
    body,
    bridgeCalls,
    context,
    dispatchWindow,
    documentListeners,
    observers,
    settle,
    snapshot: () => context.window.__codeyTokenStats.snapshot(),
    setClock: (value) => {
      clock = value;
    },
  };
}

test("token stats script installs and reports an enabled snapshot", async () => {
  const env = createEnvironment();
  await env.settle();
  const snapshot = env.snapshot();
  assert.equal(snapshot.installed, true);
  assert.equal(snapshot.enabled, true);
  assert.equal(snapshot.observedTurns, 0);
});

test("observes turn/start requests and renders a footer below the turn row", async () => {
  const env = createEnvironment();
  await env.settle();

  env.setClock(100);
  env.dispatchWindow("codex-message-from-view", {
    type: "mcp-request",
    request: {
      id: 41,
      method: "turn/start",
      params: { threadId: "thread-1", model: "gpt-x" },
    },
  });

  const row = new env.context.HTMLElement("div");
  row.setAttribute("data-turn-key", "history-content:turn:t1");
  row.textContent = "user question";
  env.body.appendChild(row);

  const documentObserver = env.observers[0];
  assert.ok(documentObserver, "document observer should be installed");
  documentObserver.callback([{ type: "childList", addedNodes: [row] }]);

  // The assistant reply streams into the row after the turn starts.
  env.setClock(500);
  row.textContent = "user questionassistant reply";
  const rowObserver = env.observers[1];
  assert.ok(rowObserver, "row observer should be installed on the turn row");
  rowObserver.callback([{ type: "characterData" }]);

  await env.settle();

  const snapshot = env.snapshot();
  assert.equal(snapshot.observedTurns, 1);
  assert.equal(snapshot.observedMessages, 1);

  const footer = row.children[0];
  assert.ok(footer, "footer should be appended below the turn row");
  assert.match(footer.textContent, /400ms 首字/);
  assert.match(footer.textContent, /📥 输入: 40 tokens/);
  assert.match(footer.textContent, /📤 输出: 90 tokens/);
  assert.match(footer.textContent, /🪙 合计: 130 tokens/);
  assert.match(footer.textContent, /2\.7s 总耗时/);

  const tokenStatsCalls = env.bridgeCalls.filter(
    (call) => call.path === "/token-stats",
  );
  assert.equal(tokenStatsCalls.length, 1);
  assert.equal(tokenStatsCalls[0].payload.turnId, "t1");
  assert.equal(tokenStatsCalls[0].payload.sessionId, "thread-1");
});

test("renders a partial footer with dashes when token stats are unavailable", async () => {
  const env = createEnvironment({
    tokenStats: { status: "unavailable", reasonCode: "not-found" },
  });
  await env.settle();

  env.setClock(100);
  env.dispatchWindow("codex-message-from-view", {
    type: "mcp-request",
    request: {
      id: 42,
      method: "turn/start",
      params: { threadId: "thread-2" },
    },
  });

  const row = new env.context.HTMLElement("div");
  row.setAttribute("data-turn-key", "history-content:turn:t2");
  row.textContent = "hi";
  env.body.appendChild(row);
  env.observers[0].callback([{ type: "childList", addedNodes: [row] }]);

  env.setClock(300);
  row.textContent = "hiassistant reply";
  env.observers[1].callback([{ type: "childList", addedNodes: [] }]);
  await env.settle();

  const footer = row.children[0];
  assert.ok(footer, "footer should still render with timing only");
  assert.match(footer.textContent, /200ms 首字/);
  assert.match(footer.textContent, /📥 输入: — tokens/);
});

test("renders a separate subagent line without polluting the main tokens", async () => {
  const env = createEnvironment({
    tokenStats: {
      status: "ok",
      reasonCode: "ok",
      durationMs: 2700,
      inputTokens: 20,
      outputTokens: 30,
      totalTokens: 50,
      subagentStats: { inputTokens: 15, outputTokens: 25, totalTokens: 40, count: 2 },
    },
  });
  await env.settle();

  env.setClock(100);
  env.dispatchWindow("codex-message-from-view", {
    type: "mcp-request",
    request: { id: 43, method: "turn/start", params: { threadId: "thread-3" } },
  });

  const row = new env.context.HTMLElement("div");
  row.setAttribute("data-turn-key", "history-content:turn:t3");
  row.textContent = "user";
  env.body.appendChild(row);
  env.observers[0].callback([{ type: "childList", addedNodes: [row] }]);

  env.setClock(500);
  row.textContent = "userassistant";
  env.observers[1].callback([{ type: "characterData" }]);
  await env.settle();

  const footer = row.children[0];
  assert.ok(footer, "footer should render");
  // Main line keeps only the parent-agent tokens.
  assert.match(footer.textContent, /📥 输入: 20 tokens/);
  assert.match(footer.textContent, /📤 输出: 30 tokens/);
  assert.match(footer.textContent, /🪙 合计: 50 tokens/);

  // Subagent totals are their own line.
  const subLine = footer.children.find(
    (child) => child.className === "codey-token-stats-sub",
  );
  assert.ok(subLine, "subagent line should be present");
  assert.match(subLine.textContent, /子代理/);
  assert.match(subLine.textContent, /📥 输入: 15/);
  assert.match(subLine.textContent, /📤 输出: 25/);
  assert.match(subLine.textContent, /🪙 合计: 40/);
  assert.match(subLine.textContent, /2 轮/);
});
