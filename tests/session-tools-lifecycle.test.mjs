import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {
  constructor(tag = "div", attributes = {}) {
    super(tag, { attributes, connected: true });
    this.className = "";
    const classes = new Set();
    this.classList = {
      add: (value) => classes.add(value),
      contains: (value) => classes.has(value),
      toggle(value, enabled) {
        if (enabled) classes.add(value);
        else classes.delete(value);
      },
    };
  }

  getBoundingClientRect() {
    return { top: 100, bottom: 124, left: 100, right: 124, width: 24, height: 24 };
  }

  getClientRects() { return [this.getBoundingClientRect()]; }
}

function fixture({ idle = true, bridge, controller, pathname = "/" } = {}) {
  let sequence = 0;
  let queries = 0;
  const timeouts = new Map();
  const intervals = new Map();
  const idleCallbacks = new Map();
  const observers = [];
  const calls = [];
  const document = new FakeElement("document");
  document.body = new FakeElement("body");
  document.documentElement = new FakeElement("html");
  document.appendChild(document.documentElement);
  document.documentElement.appendChild(document.body);
  document.visibilityState = "visible";
  document.scripts = [];
  document.createElement = (tag) => new FakeElement(tag);
  const placeholder = new FakeElement();
  document.getElementById = (id) => {
    if (["codey-injected-style", "codey-settings-button", "codey-message-toolbar"].includes(id)) {
      return placeholder;
    }
    return document.querySelector(`#${id}`);
  };
  document.querySelectorAll = (selector) => {
    queries += 1;
    return FakeElementCore.prototype.querySelectorAll.call(document, selector);
  };
  const window = new FakeElement("window");
  Object.assign(window, {
    __codeyCodexSessionController: controller,
    __codexSessionDeleteBridge(path, payload) {
      calls.push({ path, payload });
      return bridge?.(path, payload) ?? Promise.resolve({ status: "ok", timestamps: {} });
    },
    clearTimeout: (id) => timeouts.delete(id),
    clearInterval: (id) => intervals.delete(id),
    cancelIdleCallback: (id) => idleCallbacks.delete(id),
    setTimeout(callback, delay = 0) {
      const id = ++sequence;
      timeouts.set(id, { callback, delay });
      return id;
    },
    setInterval(callback) {
      const id = ++sequence;
      intervals.set(id, callback);
      return id;
    },
    localStorage: { length: 0, key() {}, getItem() {}, setItem() {} },
  });
  if (idle) window.requestIdleCallback = (callback) => {
    const id = ++sequence;
    idleCallbacks.set(id, callback);
    return id;
  };
  const context = vm.createContext({
    console, document, window, URL, URLSearchParams,
    HTMLElement: FakeElement, Element: FakeElement,
    Node: { TEXT_NODE: 3 },
    location: { pathname, search: "" },
    MutationObserver: class {
      constructor(callback) {
        this.callback = callback;
        this.connected = false;
        observers.push(this);
      }
      observe() { this.connected = true; }
      disconnect() { this.connected = false; }
    },
  });
  return {
    document, window, timeouts, intervals, idleCallbacks, observers, calls,
    queries: () => queries,
    load: () => vm.runInContext(source, context),
    runTimeout(delay) {
      const entry = [...timeouts].find(([, timer]) => timer.delay === delay);
      assert.ok(entry, `missing timeout with delay ${delay}`);
      timeouts.delete(entry[0]);
      return entry[1].callback();
    },
  };
}

function thread(document, sessionId, running = false) {
  const item = new FakeElement("div", { role: "listitem" });
  const row = new FakeElement("div", {
    "data-app-action-sidebar-thread-row": "",
    "data-app-action-sidebar-thread-id": sessionId,
    "data-app-action-sidebar-thread-title": sessionId,
  });
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const title = new FakeElement();
  title.className = "flex min-w-0 flex-1 items-center gap-2";
  const rail = new FakeElement();
  rail.className = "ml-[3px] flex items-center justify-end gap-1";
  const spinner = new FakeElement();
  spinner.className = "animate-spin rounded-full";
  if (running) rail.appendChild(spinner);
  content.append(title, rail);
  row.appendChild(content);
  item.appendChild(row);
  document.body.appendChild(item);
  return { row, spinner };
}

const settle = () => new Promise((resolve) => setImmediate(resolve));

for (const idle of [true, false]) {
  test(`dispose cancels the initial ${idle ? "idle callback" : "timeout"} and stale callbacks stay inert`, () => {
    const runtime = fixture({ idle });
    runtime.load();
    const initial = idle ? [...runtime.idleCallbacks.values()][0] : [...runtime.timeouts.values()][0].callback;
    const observer = runtime.observers[0];
    const install = runtime.window.__codeySessionToolsInstall;
    install.dispose();
    install.dispose();
    const queries = runtime.queries();
    initial();
    observer.callback([]);
    assert.equal(runtime.queries(), queries);
    assert.equal(runtime.timeouts.size + runtime.idleCallbacks.size + runtime.intervals.size, 0);
    assert.equal(observer.connected, false);
    runtime.load();
    assert.notEqual(runtime.window.__codeySessionToolsInstall, install);
    install.dispose();
    assert.equal(runtime.window.__codeySessionToolsInjectLoaded, true);
  });
}

test("failed installation releases registered resources and can be loaded again", () => {
  const runtime = fixture();
  const original = runtime.window.setInterval;
  let count = 0;
  runtime.window.setInterval = (callback) => {
    if (++count === 2) throw new Error("injected interval failure");
    runtime.document.dispatchEvent({ type: "pointerdown" });
    return original(callback);
  };
  assert.throws(runtime.load, /injected interval failure/);
  assert.equal(runtime.timeouts.size + runtime.intervals.size, 0);
  assert.equal(runtime.observers[0].connected, false);
  assert.equal([...runtime.document.listeners.values()].flat().length, 0);
  assert.equal([...runtime.window.listeners.values()].flat().length, 0);
  runtime.window.setInterval = original;
  runtime.load();
  assert.equal(runtime.window.__codeySessionToolsInjectLoaded, true);
  assert.equal(runtime.intervals.size, 2);
});

test("dispose cancels scan, watcher, sidebar tooltip and running recheck timers", () => {
  const runtime = fixture();
  const { row, spinner } = thread(runtime.document, "thread-1", true);
  runtime.load();
  runtime.window.__codeyInstallThreadUpdatedTimes(row);
  spinner.remove();
  runtime.window.__codeyInstallThreadUpdatedTimes(row);
  runtime.document.dispatchEvent({ type: "pointerdown" });
  runtime.observers[0].callback([{ type: "attributes", attributeName: "aria-label", target: row }]);

  const archive = new FakeElement("button", { "aria-label": "归档任务" });
  row.appendChild(archive);
  runtime.window.__codeyInstallSessionDeleteButtons(row);
  const button = row.querySelector("[data-codey-session-delete]");
  assert.ok(button);
  button.dispatchEvent({ type: "mouseenter" });
  const delays = [...runtime.timeouts.values()].map(({ delay }) => delay);
  for (const delay of [40, 60, 400, 2_000, 30_000]) assert.ok(delays.includes(delay), `missing ${delay} timer`);
  const callbacks = [...runtime.timeouts.values()].map(({ callback }) => callback);
  runtime.window.__codeySessionToolsInstall.dispose();
  const queries = runtime.queries();
  callbacks.forEach((callback) => callback());
  button.dispatchEvent({ type: "mouseenter" });
  assert.equal(runtime.timeouts.size, 0);
  assert.equal(runtime.queries(), queries);
});

test("retained action buttons use the current tooltip installation after reload", () => {
  const runtime = fixture();
  const { row } = thread(runtime.document, "thread-1");
  row.appendChild(new FakeElement("button", { "aria-label": "归档任务" }));
  runtime.load();
  runtime.window.__codeyInstallSessionDeleteButtons(row);
  const button = row.querySelector("[data-codey-session-delete]");
  const oldToast = runtime.window.__codeyShowRuntimeToast;
  runtime.window.__codeySessionToolsInstall.dispose();
  runtime.load();
  runtime.window.__codeyInstallSessionDeleteButtons(row);
  assert.equal(row.querySelector("[data-codey-session-delete]"), button);
  button.dispatchEvent({ type: "mouseenter" });
  runtime.runTimeout(400);
  assert.equal(button.getAttribute("aria-describedby"), "codey-sidebar-action-tooltip");
  button.dispatchEvent({ type: "mouseleave" });
  assert.equal(button.getAttribute("aria-describedby"), null);
  assert.equal(runtime.document.getElementById("codey-sidebar-action-tooltip"), null);
  // Previously started user operations can still report their final result.
  oldToast("operation finished");
  assert.equal(runtime.document.getElementById("codey-runtime-toast").textContent, "operation finished");
});

test("a listener registration failure releases the listeners already installed", () => {
  const runtime = fixture();
  const register = runtime.document.addEventListener.bind(runtime.document);
  runtime.document.addEventListener = (type, ...args) => {
    if (type === "pointerdown") throw new Error("injected listener failure");
    register(type, ...args);
  };
  assert.throws(runtime.load, /injected listener failure/);
  assert.equal([...runtime.document.listeners.values()].flat().length, 0);
  assert.equal(runtime.observers[0].connected, false);
});

test("a dispatcher inspection failure unsubscribes the installed mutation handler", () => {
  const runtime = fixture();
  let subscriptions = 0;
  runtime.window.__codeyMutationDispatcher = {
    subscribe() {
      subscriptions += 1;
      return () => { subscriptions -= 1; };
    },
    snapshot() { throw new Error("injected dispatcher failure"); },
  };
  assert.throws(runtime.load, /injected dispatcher failure/);
  assert.equal(subscriptions, 0);
});

test("an old timestamp response neither updates rows nor schedules the queued batch after reload", async () => {
  let complete;
  const runtime = fixture({ bridge: (path) => path === "/session/timestamps"
    ? new Promise((resolve) => { complete = resolve; }) : Promise.resolve({ status: "ok" }) });
  const first = thread(runtime.document, "thread-1");
  runtime.load();
  runtime.window.__codeyInstallThreadUpdatedTimes(first.row);
  runtime.runTimeout(40);
  const second = thread(runtime.document, "thread-2");
  runtime.window.__codeyInstallThreadUpdatedTimes(second.row);
  runtime.window.__codeySessionToolsInstall.dispose();
  runtime.load();
  complete({ status: "ok", timestamps: { "thread-1": Date.now() } });
  await settle();
  assert.equal(first.row.querySelector("[data-codey-thread-updated-at]"), null);
  assert.equal(runtime.timeouts.size, 0);
  assert.equal(runtime.calls.filter(({ path }) => path === "/session/timestamps").length, 1);
});

test("disposal preserves native wait settlement and prevents a subsequent reconciliation", async () => {
  let reconcileCalls = 0;
  const controller = {
    kind: "manager",
    discardConversation() {},
    notifyConversationDeleted() {},
    resumeConversation() {},
    refreshRecentConversations() {},
    reconcileCompletedConversation() {
      reconcileCalls += 1;
      return new Promise(() => {});
    },
  };
  const runtime = fixture({ controller });
  runtime.load();
  const active = new FakeElement("div", { "data-session-id": "thread-1" });
  runtime.document.body.appendChild(active);
  const result = runtime.window.__codeyReconcileStaleCompletedTask();
  await settle();
  assert.equal(reconcileCalls, 1);
  runtime.window.__codeySessionToolsInstall.dispose();
  active.setAttribute("data-session-id", "thread-2");
  runtime.runTimeout(5_000);
  assert.equal(await result, false);
  assert.equal(reconcileCalls, 1);
  assert.equal(runtime.timeouts.size, 0);
});
