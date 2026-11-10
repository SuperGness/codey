import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { flushTimers } from "./helpers/flush.mjs";
import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(
  new URL("../public/conversation-stats-inject.js", import.meta.url),
  "utf8",
);

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", { visible = true } = {}) {
    super(tagName);
    this.visible = visible;
    this.querySelectorCalls = [];
  }

  querySelector(selector) {
    this.querySelectorCalls.push(selector);
    return super.querySelector(selector);
  }

  matches(selector) {
    if (selector === ":popover-open") {
      if (typeof this.showPopover !== "function") throw new SyntaxError("Unsupported popover selector");
      return this.popoverOpen === true;
    }
    const match = selector.match(/^(\w+)?\.([\w-]+)$/);
    if (match) return (!match[1] || this.tagName === match[1].toUpperCase())
      && String(this.className || "").split(/\s+/).includes(match[2]);
    return super.matches(selector);
  }

  focus() {
    if (this.ownerDocument) this.ownerDocument.activeElement = this;
  }

  getClientRects() {
    return this.visible ? [{ bottom: 10, left: 0, right: 10, top: 0 }] : [];
  }

  getBoundingClientRect() {
    return this.visible
      ? { bottom: 160, height: 60, left: 0, right: 600, top: 100, width: 600 }
      : { bottom: 0, height: 0, left: 0, right: 0, top: 0, width: 0 };
  }
}

const createEnvironment = (options = {}) => {
  const documentElement = new FakeElement("html");
  const body = new FakeElement("body");
  const scope = new FakeElement("div");
  const anchor = new FakeElement("div", { visible: options.anchorVisible !== false });
  anchor.setAttribute(
    "data-above-composer-conversation-id",
    options.conversationId ?? "conversation-1",
  );
  const textarea = new FakeElement("textarea", {
    visible: options.inputVisible !== false,
  });
  documentElement.appendChild(body);
  body.appendChild(scope);
  if (options.anchor !== false) {
    scope.appendChild(anchor);
    scope.appendChild(textarea);
  }

  const findById = (root, id) => {
    if (root.id === id || root.getAttribute?.("id") === id) return root;
    for (const child of root.children || []) {
      const found = findById(child, id);
      if (found) return found;
    }
    return null;
  };

  let latestObserver = null;
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      this.observed = false;
      latestObserver = this;
    }

    observe(_target, options) {
      this.observed = true;
      this.options = options;
    }

    disconnect() {
      this.observed = false;
    }
  }

  const document = {
    body,
    documentElement,
    visibilityState: "visible",
    activeElement: body,
    createElement: (tagName) => Object.assign(new FakeElement(tagName), { ownerDocument: document }, options.popover ? {
      showPopover() { this.popoverOpen = true; },
      hidePopover() { this.popoverOpen = false; },
    } : {}),
    getElementById: (id) => findById(documentElement, id),
    querySelector: (selector) => documentElement.querySelector(selector),
    querySelectorAll: (selector) => documentElement.querySelectorAll(selector),
    listeners: new Map(),
    addEventListener: FakeElementCore.prototype.addEventListener,
    removeEventListener: FakeElementCore.prototype.removeEventListener,
    dispatchEvent: FakeElementCore.prototype.dispatchEvent,
  };
  for (const element of [documentElement, body, scope, anchor, textarea]) element.ownerDocument = document;

  const location = {
    pathname: options.pathname ?? "/",
    search: options.search ?? "",
  };

  const statsBySession = new Map(Object.entries(options.statsBySession ?? {}));
  const calls = [];
  const bridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/session/stats") {
      if (options.request) return options.request(payload.sessionId);
      return {
        status: "ok",
        stats: statsBySession.get(payload.sessionId) ?? null,
      };
    }
    return { status: "ok" };
  };

  const animationFrames = new Map();
  let nextAnimationFrame = 1;
  const window = {
    __codexSessionDeleteBridge: options.bridgeReady === false ? undefined : bridge,
    innerHeight: 800,
    innerWidth: 1000,
    location,
    listeners: new Map(),
    addEventListener: FakeElementCore.prototype.addEventListener,
    removeEventListener: FakeElementCore.prototype.removeEventListener,
    dispatchEvent: FakeElementCore.prototype.dispatchEvent,
    requestAnimationFrame: (callback) => {
      const id = nextAnimationFrame++;
      animationFrames.set(id, callback);
      return id;
    },
    cancelAnimationFrame: (id) => animationFrames.delete(id),
  };

  const testSetTimeout = (callback, delay, ...args) => {
    options.onTimer?.(delay, () => callback(...args));
    const timer = setTimeout(callback, delay, ...args);
    timer.unref?.();
    return timer;
  };
  const sandbox = {
    document,
    window,
    location,
    URLSearchParams,
    MutationObserver: FakeMutationObserver,
    HTMLElement: FakeElement,
    setTimeout: testSetTimeout,
    clearTimeout,
  };
  const context = vm.createContext(sandbox);
  vm.runInContext(source, context);
  const observer = latestObserver;

  return {
    anchor,
    textarea,
    document,
    scope,
    calls,
    statsBySession,
    bar: () => findById(documentElement, "codey-conversation-stats"),
    snapshot: () => context.window.__codeyConversationStats?.snapshot(),
    refresh: () => context.window.__codeyConversationStats.refresh(),
    setStats: (sessionId, value) => statsBySession.set(sessionId, value),
    setConversation: (id) => {
      anchor.setAttribute("data-above-composer-conversation-id", id);
    },
    emitMutation: (mutations = [{ type: "childList", target: documentElement }]) => {
      if (observer?.observed) observer.callback(mutations.filter(mutation => mutation.type !== "attributes"
        || observer.options?.attributeFilter?.includes(mutation.attributeName)));
    },
    emitAnchorMutation: () => {
      observer?.callback?.([
        { type: "attributes", target: anchor, attributeName: "data-above-composer-conversation-id" },
      ]);
    },
    rerun: () => vm.runInContext(source, context),
    dispose: () => context.window.__codeyConversationStats.dispose(),
    markPreviousVersion: () => { context.window.__codeyConversationStats.version = 3; },
    animationFrames,
    runAnimationFrames: () => {
      const callbacks = [...animationFrames.values()];
      animationFrames.clear();
      callbacks.forEach((callback) => callback(0));
    },
    window,
  };
};

const LAST_TURN = {
  id: "turn-1", status: "completed", stepCount: 2,
  modelDurationMs: 10000, toolDurationMs: 3000, averageTtftMs: 500,
  averageOutputTokensPerSecond: 187.52, speedIsEstimate: false,
  inFlight: 0, speedState: null, coverage: "runtime",
};
const STATS = {
  lastTurn: LAST_TURN,
  turnCount: 4,
  usageSource: "runtime",
  usageScope: "runtime",
  requestsScope: "runtime",
  model: "gpt-5",
  requests: 2,
  inFlight: 0,
  lastStatus: "succeeded",
  outputTokensPerSecond: 999,
  averageOutputTokensPerSecond: 187.52,
  averageSpeedIsEstimate: false,
  averageSpeedSource: "runtime",
  averageSpeedScope: "runtime",
  averageSpeedRequestCount: 2,
  inputTokens: 128_400,
  outputTokens: 3_100,
  cachedInputTokens: 120_000,
  cacheWriteInputTokens: 2_000,
  totalTokens: 131_500,
  updatedAtUnixMs: 1_760_000_000_000,
};

const metric = (bar, name) => name === "status" ? bar.dataset.status : bar.querySelector(`.codey-cs-${name}`).textContent;
const field = (bar, name) => bar.__codeyCs.fields[name].textContent;
const summary = (env) => env.bar().querySelector("button.codey-cs-summary");
const panel = (env) => env.document.getElementById("codey-conversation-stats-activity");
const click = (element) => {
  const event = { type: "click", defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
  element.dispatchEvent(event);
  return event;
};
const assertExpanded = (env, expanded) => {
  assert.equal(summary(env).getAttribute("data-codey-expanded"), String(expanded));
  // Codex 的 :has([aria-expanded="true"]) 规则会让该属性每次变化都整页重算样式。
  assert.equal(summary(env).getAttribute("aria-expanded"), null);
  assert.equal(panel(env).hidden, !expanded);
};

const observeRenderWork = (env) => {
  const counts = { composerScans: 0, textWrites: 0 };
  const querySelectorAll = env.document.querySelectorAll;
  env.document.querySelectorAll = (selector) => {
    if (selector === "[data-above-composer-conversation-id]") counts.composerScans += 1;
    return querySelectorAll(selector);
  };
  const refs = env.bar().__codeyCs;
  for (const element of [refs.speed, refs["cache-rate"], refs.total, refs.activity,
    ...Object.values(refs.fields), ...Object.values(refs.notes)]) {
    let value = element.textContent;
    Object.defineProperty(element, "textContent", {
      configurable: true,
      get: () => value,
      set: (next) => { counts.textWrites += 1; value = next; },
    });
  }
  return counts;
};

test("updates an existing single text node in place instead of replacing it", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    const element = env.bar().__codeyCs.speed;
    const node = { nodeType: 3, nextSibling: null, nodeValue: element.textContent };
    let replacements = 0;
    Object.defineProperties(element, {
      firstChild: { configurable: true, get: () => node },
      textContent: { configurable: true, get: () => node.nodeValue, set: () => { replacements += 1; } },
    });
    env.setStats("conversation-1", { ...STATS, lastTurn: { ...LAST_TURN, averageOutputTokensPerSecond: 50 } });
    env.refresh();
    await flushTimers(30);
    assert.equal(node.nodeValue, "50.0 Token/s");
    assert.equal(replacements, 0);
  } finally {
    env.dispose();
  }
});

test("refreshes with one composer scan per async phase and only writes changed text", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    const counts = observeRenderWork(env);
    const initialCalls = env.calls.length;
    env.refresh();
    await flushTimers(30);
    assert.deepEqual(counts, { composerScans: 2, textWrites: 0 });
    assert.equal(env.calls.length, initialCalls + 1);
    env.setStats("conversation-1", { ...STATS, totalTokens: 500, lastTurn: { ...LAST_TURN, averageOutputTokensPerSecond: 50 } });
    env.refresh();
    await flushTimers(30);
    // 速度（摘要与详情）和总量（摘要与详情）各写一次。
    assert.deepEqual(counts, { composerScans: 4, textWrites: 4 });
    assert.equal(metric(env.bar(), "total"), "总计 500 Tokens");
    assert.equal(field(env.bar(), "totalTokens"), "500 Tokens");
    assert.equal(metric(env.bar(), "speed"), "50.0 Token/s");
    assert.equal(field(env.bar(), "speed"), "50.0 Token/s");
  } finally {
    env.dispose();
  }
});

test("unchanged refreshes with an open panel reduce geometry reads and repeated attribute writes", async t => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    click(summary(env));
    const counts = observeRenderWork(env);
    const geometryReads = { button: 0, panel: 0 };
    let attributeWrites = 0;
    for (const [kind, element] of [["button", summary(env)], ["panel", panel(env)]]) {
      const readRect = element.getBoundingClientRect.bind(element);
      element.getBoundingClientRect = () => { geometryReads[kind] += 1; return readRect(); };
    }
    for (const element of Object.values(env.bar().__codeyCs.buttons)) {
      const setAttribute = element.setAttribute.bind(element);
      element.setAttribute = (...args) => { attributeWrites += 1; setAttribute(...args); };
    }
    for (let index = 0; index < 10; index += 1) {
      env.refresh();
      await flushTimers(15);
    }
    t.diagnostic(`10 unchanged refreshes: ${counts.composerScans} composer scans, ${geometryReads.button} button geometry reads, ${geometryReads.panel} panel geometry reads, ${counts.textWrites} text writes, ${attributeWrites} label writes`);
    assert.deepEqual(counts, { composerScans: 20, textWrites: 0 });
    // 轮询期间不切换读取中状态：每次刷新只在响应后渲染一次，且不改写标签。
    assert.deepEqual(geometryReads, { button: 10, panel: 0 });
    assert.equal(attributeWrites, 0);
    env.emitAnchorMutation();
    await flushTimers(160);
    assert.equal(attributeWrites, 0, "a DOM rescan with the same data must not rewrite labels");
    assert.deepEqual(geometryReads, { button: 11, panel: 0 });
  } finally {
    env.dispose();
  }
});

test("repositions an unchanged panel when its entry moves without resize or scroll", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    panel(env).getBoundingClientRect = () => ({ width: 300, height: 200 });
    click(summary(env));
    const original = summary(env).getBoundingClientRect();
    const originalLeft = Number.parseFloat(panel(env).style.left);
    summary(env).getBoundingClientRect = () => ({ ...original, left: original.left + 200 });
    env.refresh();
    await flushTimers(30);
    assert.equal(Number.parseFloat(panel(env).style.left), originalLeft + 200);
    assertExpanded(env, true);
  } finally {
    env.dispose();
  }
});

test("repositions an open panel after its visible content changes", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    click(summary(env));
    panel(env).getBoundingClientRect = () => ({ width: 300, height: 200 });
    env.setStats("conversation-1", { ...STATS, lastTurn: {
      ...LAST_TURN, modelDurationMs: 121_000, averageOutputTokensPerSecond: 42,
    } });
    env.refresh();
    await flushTimers(30);
    assert.equal(field(env.bar(), "speed"), "42.0 Token/s");
    assert.equal(panel(env).style.left, "150px");
    assertExpanded(env, true);
    panel(env).getBoundingClientRect = () => ({ width: 400, height: 200 });
    env.setStats("conversation-1", { ...STATS, lastTurn: {
      ...LAST_TURN, stepCount: 3, averageOutputTokensPerSecond: 42,
    } });
    env.refresh();
    await flushTimers(30);
    assert.equal(summary(env).querySelector(".codey-cs-activity").textContent, "本轮 3 步");
    assert.equal(panel(env).style.left, "100px");
  } finally {
    env.dispose();
  }
});

test("keeps an open panel theme current without rewriting unchanged style properties", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    let color = "#111";
    env.window.getComputedStyle = () => ({ getPropertyValue: name => name === "--text-primary" ? color : "", colorScheme: "dark" });
    click(summary(env));
    const overlay = env.bar().__codeyCs.overlay;
    let writes = 0;
    const setProperty = overlay.style.setProperty;
    overlay.style.setProperty = (...args) => { writes += 1; setProperty(...args); };
    env.refresh();
    await flushTimers(30);
    assert.equal(writes, 0);
    color = "#eee";
    env.refresh();
    await flushTimers(30);
    assert.equal(writes, 1);
    assert.equal(overlay.style.getPropertyValue("--text-primary"), "#eee");
  } finally {
    env.dispose();
  }
});

test("mounts promptly when a textarea itself is inserted beside the composer marker", async () => {
  const env = createEnvironment({ inputVisible: false, statsBySession: { "conversation-1": STATS } });
  try {
    env.textarea.remove();
    await flushTimers(30);
    assert.equal(env.bar(), null);
    env.textarea.visible = true;
    env.scope.appendChild(env.textarea);
    env.emitMutation([{ type: "childList", target: env.scope, addedNodes: [env.textarea] }]);
    await flushTimers(160);
    assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
    assert.equal(env.calls.length, 1);
  } finally {
    env.dispose();
  }
});

test("does not keep scheduling polling timers while the page is hidden", async () => {
  const timers = [];
  let resolve;
  const env = createEnvironment({ onTimer: delay => timers.push(delay),
    request: () => new Promise(done => { resolve = done; }) });
  try {
    await flushTimers(30);
    env.document.visibilityState = "hidden";
    env.document.dispatchEvent({ type: "visibilitychange" });
    const scheduledBefore = timers.length;
    resolve({ status: "ok", stats: STATS });
    await flushTimers(30);
    env.refresh();
    await flushTimers(30);
    assert.equal(timers.length, scheduledBefore);
    env.document.visibilityState = "visible";
    env.document.dispatchEvent({ type: "visibilitychange" });
    await flushTimers(30);
    assert.equal(env.calls.length, 2);
  } finally {
    env.dispose();
  }
});

test("restores a composer promptly when its hidden attribute is removed", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    env.scope.setAttribute("hidden", "");
    env.emitMutation([{ type: "attributes", target: env.scope, attributeName: "hidden" }]);
    await flushTimers(160);
    assert.equal(env.bar().hidden, true);
    env.scope.removeAttribute("hidden");
    env.emitMutation([{ type: "attributes", target: env.scope, attributeName: "hidden" }]);
    await flushTimers(160);
    assert.equal(env.bar().hidden, false);
    assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  } finally {
    env.dispose();
  }
});

test("ignores streaming child churn and visibility flips inside conversation turns", async () => {
  const delays = [];
  const env = createEnvironment({
    onTimer: (delay) => delays.push(delay),
    statsBySession: { "conversation-1": STATS },
  });
  try {
    await flushTimers(30);
    const turn = env.document.createElement("div");
    turn.setAttribute("data-turn-key", "turn-1");
    env.scope.appendChild(turn);
    const streamedNode = turn.appendChild(env.document.createElement("span"));
    streamedNode.querySelectorCalls.length = 0;
    delays.length = 0;

    env.emitMutation([{
      type: "childList",
      target: turn,
      addedNodes: [streamedNode],
    }]);
    turn.setAttribute("aria-hidden", "true");
    env.emitMutation([{
      type: "attributes",
      target: turn,
      attributeName: "aria-hidden",
    }]);
    await flushTimers(160);

    assert.deepEqual(streamedNode.querySelectorCalls, []);
    assert.equal(delays.includes(120), false);
  } finally {
    env.dispose();
  }
});

for (const attribute of ["data-session-id", "data-thread-id", "data-app-action-sidebar-thread-active"]) {
  test(`refreshes fallback session context on ${attribute} changes`, async () => {
    const env = createEnvironment({ anchor: false, statsBySession: {
      "conversation-1": STATS, "conversation-2": { ...STATS, totalTokens: 42 },
    } });
    try {
      const marker = env.document.createElement("div");
      if (attribute === "data-app-action-sidebar-thread-active") {
        marker.setAttribute("data-app-action-sidebar-thread-id", "conversation-1");
        marker.setAttribute(attribute, "true");
      } else marker.setAttribute(attribute, "conversation-1");
      env.scope.appendChild(marker);
      await flushTimers(30);
      assert.equal(env.snapshot().sessionId, "conversation-1");
      if (attribute === "data-app-action-sidebar-thread-active") {
        marker.setAttribute(attribute, "false");
        const active = env.document.createElement("div");
        active.setAttribute("data-app-action-sidebar-thread-id", "conversation-2");
        active.setAttribute(attribute, "true");
        env.scope.appendChild(active);
      } else marker.setAttribute(attribute, "conversation-2");
      env.emitMutation([{ type: "attributes", target: marker, attributeName: attribute }]);
      await flushTimers(160);
      assert.equal(env.snapshot().sessionId, "conversation-2");
      assert.equal(env.snapshot().stats.totalTokens, 42);
    } finally {
      env.dispose();
    }
  });
}

test("shows precise totals in the tooltip and avoids a rounded 1000k boundary", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS, totalTokens: 999_999, cachedInputTokens: null } } });
  try {
    await flushTimers(30);
    assert.equal(metric(env.bar(), "total"), "总计 1.0M Tokens");
    assert.equal(env.bar().__codeyCs.total.title, "本次运行 Token 总量 999,999 Tokens，包含缓存读取。");
    assert.equal(field(env.bar(), "totalTokens"), "999,999 Tokens");
    assert.equal(env.bar().__codeyCs.notes.activity.textContent, "");
  } finally {
    env.dispose();
  }
});

test("rejects fractional or imprecise token and step counts", async () => {
  const env = createEnvironment();
  try {
    for (const value of [1.5, "42", Number.MAX_SAFE_INTEGER + 1]) {
      env.setStats("conversation-1", { ...STATS, turnCount: value, totalTokens: value,
        inputTokens: value, cachedInputTokens: value, outputTokens: value,
        lastTurn: { ...LAST_TURN, stepCount: value } });
      env.refresh();
      await flushTimers(30);
      assert.equal(metric(env.bar(), "total"), "—");
      assert.equal(metric(env.bar(), "activity"), "本轮 — 步");
      assert.equal(field(env.bar(), "outputTokens"), "—");
      assert.equal(metric(env.bar(), "cache-rate"), "—");
    }
  } finally {
    env.dispose();
  }
});

test("keeps the bar persistently mounted across a replacement composer after an in-flight stats response", async () => {
  let resolve;
  const env = createEnvironment({ request: () => new Promise(done => { resolve = done; }) });
  try {
    await flushTimers(30);
    const originalBar = env.bar();
    assert.equal(originalBar.parentElement, env.document.body);
    let bodyAppends = 0;
    const appendChild = env.document.body.appendChild.bind(env.document.body);
    env.document.body.appendChild = (child) => {
      if (child === originalBar) bodyAppends += 1;
      return appendChild(child);
    };
    env.scope.remove();
    const replacement = env.document.createElement("div");
    replacement.getBoundingClientRect = () => ({ bottom: 780, height: 80, left: 240, right: 940, top: 700, width: 700 });
    const anchor = env.document.createElement("div");
    anchor.setAttribute("data-above-composer-conversation-id", "conversation-1");
    replacement.append(anchor, env.document.createElement("textarea"));
    env.document.body.appendChild(replacement);
    // 不发 MutationObserver 通知，确保完成异步读取时主动重新定位且无需重插节点。
    resolve({ status: "ok", stats: STATS });
    await flushTimers(30);
    assert.equal(env.bar(), originalBar);
    assert.equal(env.bar().parentElement, env.document.body);
    assert.equal(bodyAppends, 0, "stats bar must remain fixed without re-inserting into the DOM");
    assert.equal(env.bar().style.getPropertyValue("left"), "240px");
    assert.equal(env.bar().style.getPropertyValue("width"), "700px");
    assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  } finally {
    env.dispose();
  }
});

test("discards a response after navigation without a mutation notification", async () => {
  const pending = [];
  const env = createEnvironment({ request: sessionId => new Promise(resolve => pending.push({ sessionId, resolve })) });
  try {
    await flushTimers(30);
    env.setConversation("conversation-2");
    pending[0].resolve({ status: "ok", stats: STATS });
    await flushTimers(30);
    assert.equal(metric(env.bar(), "total"), "—");
    assert.equal(pending[1].sessionId, "conversation-2");
    pending[1].resolve({ status: "ok", stats: { ...STATS, totalTokens: 700 } });
    await flushTimers(30);
    assert.equal(field(env.bar(), "totalTokens"), "700 Tokens");
  } finally {
    env.dispose();
  }
});

test("mounts cached stats when the composer appears after the initial response", async () => {
  const env = createEnvironment({ anchor: false, pathname: "/c/conversation-1", statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    assert.equal(env.bar(), null);
    assert.equal(env.calls.length, 1);
    env.scope.append(env.anchor, env.textarea);
    env.emitAnchorMutation();
    await flushTimers(160);
    assert.equal(env.bar().parentElement, env.document.body);
    assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
    assert.equal(env.calls.length, 1, "mounting must not refetch the same session");
  } finally {
    env.dispose();
  }
});

test("mounts the stats bar under the composer and renders speed/cache/total", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);

  const bar = env.bar();
  assert.ok(bar, "stats bar should exist");
  assert.equal(bar.parentElement, env.document.body);
  assert.equal(env.scope.contains(bar), false);
  assert.equal(bar.hidden, false);
  assert.equal(metric(bar, "speed"), "188 Token/s");
  assert.equal(metric(bar, "activity"), "本轮 2 步");
  assert.match(bar.__codeyCs.activity.title, /最近一轮的模型生成步数/);
  assert.equal(metric(bar, "cache-rate"), "93%");
  assert.equal(metric(bar, "total"), "总计 132k Tokens");
  assert.equal(field(bar, "inputTokens"), "128,400 Tokens");
  assert.equal(bar.__codeyCs.total.title, "本次运行 Token 总量 131,500 Tokens，包含缓存读取。");
  assert.match(bar.querySelector(".codey-cs-tokens-summary").getAttribute("aria-label"), /本次运行 总量 132k Tokens，缓存命中 93%/);
  assert.equal(field(bar, "outputTokens"), "3,100 Tokens");
  assert.equal(summary(env).querySelectorAll(".codey-cs-metric").length, 2);
  for (const selector of [".codey-cs-meta", ".codey-cs-status", '[data-field="model"]']) {
    assert.equal(summary(env).querySelector(selector), null);
  }
  assert.equal(field(bar, "totalTokens"), "131,500 Tokens");
  assert.equal(field(bar, "cachedInputTokens"), "120,000 Tokens");
  assert.match(summary(env).getAttribute("aria-label"), /查看最近一轮详情/);
  assert.match(summary(env).getAttribute("aria-label"), /本轮 2 步/);
  assert.match(summary(env).getAttribute("aria-label"), /会话共 4 轮/);
  assert.equal(
    env.calls.filter((call) => call.path === "/session/stats").at(-1).payload.sessionId,
    "conversation-1",
  );
});

test("keeps the bar visible while the current conversation has no stats", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  assert.equal(env.bar()?.hidden, false);
  assert.equal(metric(env.bar(), "status"), "暂无统计");
  assert.equal(metric(env.bar(), "total"), "—");
});

test("shows unknown values when upstream omits speed and cache", async () => {
  const env = createEnvironment({
    statsBySession: {
      "conversation-1": {
        ...STATS,
        lastTurn: { ...LAST_TURN, averageOutputTokensPerSecond: null },
        cachedInputTokens: null,
        inputTokens: null,
      },
    },
  });
  await flushTimers(30);
  const bar = env.bar();
  assert.equal(bar.hidden, false);
  assert.equal(metric(bar, "speed"), "—");
  assert.equal(metric(bar, "cache-rate"), "—");
  assert.equal(field(bar, "cachedInputTokens"), "—");
  assert.equal(metric(bar, "total"), "总计 132k Tokens");
  assert.equal(field(bar, "inputTokens"), "—");
});

test("switches stats context when the conversation changes", async () => {
  const env = createEnvironment({
    statsBySession: {
      "conversation-1": STATS,
      "conversation-2": { ...STATS, totalTokens: 900, model: "claude-sonnet" },
    },
  });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");

  env.setConversation("conversation-2");
  env.emitAnchorMutation();
  await flushTimers(200);

  const bar = env.bar();
  assert.equal(field(bar, "totalTokens"), "900 Tokens");
  assert.equal(env.calls.at(-1).payload.sessionId, "conversation-2");
});

test("clears stale numbers immediately on a session switch before refetch", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  assert.equal(env.bar().hidden, false);

  env.setConversation("conversation-3");
  env.emitAnchorMutation();
  await flushTimers(160);

  const bar = env.bar();
  assert.equal(bar.hidden, false);
  assert.equal(metric(bar, "total"), "—", "previous session totals must be cleared");
  assert.equal(metric(bar, "activity"), "本轮 — 步");
  assert.equal(metric(bar, "status"), "暂无统计");
  assert.match(summary(env).getAttribute("aria-label"), /查看最近一轮详情/);
  assert.doesNotMatch(summary(env).getAttribute("aria-label"), /132k/);
});

test("remounts the bar after Codex rebuilds the composer DOM", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  const bar = env.bar();
  assert.ok(bar);
  bar.remove();
  env.emitMutation([
    { type: "childList", target: env.document.body, removedNodes: [bar], addedNodes: [] },
  ]);
  await flushTimers(200);
  const remounted = env.bar();
  assert.ok(remounted, "bar should be re-created after removal");
  assert.equal(remounted.parentElement, env.document.body);
  assert.equal(remounted.hidden, false);
});

test("marks in-flight requests as running", async () => {
  const env = createEnvironment({
    statsBySession: { "conversation-1": { ...STATS, lastTurn: { ...LAST_TURN, status: "running", inFlight: 1 } } },
  });
  await flushTimers(30);
  const bar = env.bar();
  assert.equal(bar.hidden, false);
  assert.equal(bar.dataset.running, "true");
});

test("does not fetch in a hidden page and refreshes when visibility returns", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS, inFlight: 1 } } });
  await flushTimers(30);
  const calls = env.calls.length;
  env.document.visibilityState = "hidden";
  env.refresh();
  await flushTimers(30);
  assert.equal(env.calls.length, calls);
  env.document.visibilityState = "visible";
  env.document.dispatchEvent({ type: "visibilitychange" });
  await flushTimers(30);
  assert.equal(env.calls.length, calls + 1);
});

test("shows the turn state independently of old request state", async () => {
  const env = createEnvironment();
  for (const [status, label] of [
    ["aborted", "本轮已中止"],
    ["running", "本轮进行中"],
    ["completed", "运行期统计"],
  ]) {
    env.setStats("conversation-1", { ...STATS, lastStatus: "failed", lastTurn: { ...LAST_TURN, status } });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "status"), label);
    assert.ok(summary(env).getAttribute("aria-label").includes(label));
  }
});

test("ignores hidden composer anchors and stays empty without a session", async () => {
  const env = createEnvironment({ inputVisible: false });
  await flushTimers(30);
  assert.equal(env.snapshot().sessionId, "");
});

test("does not double-install on repeated injection", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  // 同一上下文再次执行整个脚本：installed 标记应阻止重复挂载。
  env.rerun();
  await flushTimers(30);
  const count = env.document.body.children.filter(
    (child) => child.id === "codey-conversation-stats",
  ).length;
  assert.equal(count, 1);
  assert.equal(env.snapshot().installed !== false, true);
});

test("keeps a waiting bar without a session and does not request stats", async () => {
  const env = createEnvironment({
    conversationId: "", pathname: "/c/previous-conversation",
    statsBySession: { "previous-conversation": STATS },
  });
  await flushTimers(30);
  assert.equal(env.bar().hidden, false);
  assert.equal(metric(env.bar(), "status"), "等待会话");
  assert.equal(metric(env.bar(), "total"), "—");
  assert.equal(env.calls.length, 0);
  click(summary(env));
  assertExpanded(env, true);
});

test("preserves reported zero tokens and zero cache hit rate", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": {
    requests: 1, totalTokens: 0, inputTokens: 100, outputTokens: 0,
    cachedInputTokens: 0, cacheWriteInputTokens: 0, inFlight: 0,
  } } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "0 Tokens");
  assert.equal(metric(env.bar(), "total"), "总计 0 Tokens");
  assert.equal(field(env.bar(), "inputTokens"), "100 Tokens");
  assert.equal(field(env.bar(), "outputTokens"), "0 Tokens");
  assert.equal(metric(env.bar(), "cache-rate"), "0%");
  assert.equal(metric(env.bar(), "status"), "运行期统计");
});

for (const stats of [{}, { requests: 0 }]) {
  test(`treats ${JSON.stringify(stats)} as an empty session`, async () => {
    const env = createEnvironment({ statsBySession: { "conversation-1": stats } });
    await flushTimers(30);
    assert.equal(env.bar().hidden, false);
    assert.equal(metric(env.bar(), "status"), "暂无统计");
    assert.equal(metric(env.bar(), "total"), "—");
  });
}

test("pins each independent panel and switches between both groups", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  assert.equal(summary(env).getAttribute("aria-controls"), panel(env).id);
  click(summary(env));
  assertExpanded(env, true);
  const tokens = env.bar().querySelector(".codey-cs-tokens-summary");
  click(tokens);
  assertExpanded(env, false);
  assert.equal(tokens.getAttribute("data-codey-expanded"), "true");
  click(tokens);
  assert.equal(tokens.getAttribute("data-codey-expanded"), "false");
});

test("centers the details immediately above the trigger and clamps the viewport", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  summary(env).getBoundingClientRect = () => ({ left: 200, top: 700, width: 400, height: 28 });
  panel(env).getBoundingClientRect = () => ({ width: 420, height: 300 });
  env.textarea.getBoundingClientRect = () => ({ left: 100, top: 520, width: 600, height: 140 });
  click(summary(env));
  assert.equal(panel(env).style.left, "190px");
  assert.equal(panel(env).style.top, "392px");
  click(summary(env));
  summary(env).getBoundingClientRect = () => ({ left: 0, top: 700, width: 200, height: 28 });
  click(summary(env));
  assert.equal(panel(env).style.left, "12px");
});

test("coalesces scroll and resize repositioning into one measurement per frame", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    let entry = { left: 200, top: 700, width: 400, height: 28 };
    let measurements = 0;
    summary(env).getBoundingClientRect = () => { measurements += 1; return entry; };
    panel(env).getBoundingClientRect = () => ({ width: 420, height: 300 });
    click(summary(env));
    assert.equal(panel(env).style.left, "190px");

    measurements = 0;
    entry = { ...entry, left: 300 };
    for (let index = 0; index < 5; index += 1) {
      env.document.dispatchEvent({ type: "scroll", target: env.document });
      env.window.dispatchEvent({ type: "resize" });
    }
    assert.equal(measurements, 0, "滚动和缩放事件里不得同步测量布局");
    assert.equal(env.animationFrames.size, 1);
    env.runAnimationFrames();
    assert.equal(measurements, 1);
    assert.equal(panel(env).style.left, "290px");

    env.document.dispatchEvent({ type: "scroll", target: panel(env) });
    assert.equal(env.animationFrames.size, 0, "浮层内部滚动不移动入口");

    env.document.dispatchEvent({ type: "scroll", target: env.document });
    click(summary(env));
    assertExpanded(env, false);
    assert.equal(env.animationFrames.size, 0, "收起时取消排队的定位");
    env.document.dispatchEvent({ type: "scroll", target: env.document });
    assert.equal(env.animationFrames.size, 0, "收起后滚动不再排队");
  } finally {
    env.dispose();
  }
});

test("measures the panel after lifting a stale height limit", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    summary(env).getBoundingClientRect = () => ({ left: 200, top: 300, width: 400, height: 28 });
    // 浮层实际高度受 max-height 限制：内容 500px，上限多少就渲染多高。
    panel(env).getBoundingClientRect = () => {
      const limit = Number.parseFloat(panel(env).style.maxHeight);
      return { width: 420, height: Number.isFinite(limit) ? Math.min(500, limit) : 500 };
    };
    click(summary(env));
    assert.equal(panel(env).style.top, "12px");

    summary(env).getBoundingClientRect = () => ({ left: 200, top: 700, width: 400, height: 28 });
    env.window.dispatchEvent({ type: "resize" });
    env.runAnimationFrames();
    assert.equal(panel(env).style.maxHeight, "680px");
    assert.equal(panel(env).style.top, "192px", "应按新上限下的高度贴近入口");
  } finally {
    env.dispose();
  }
});

test("Escape restores focus only when focus was inside the stats bar", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  for (const inside of [true, false]) {
    click(summary(env));
    const target = inside ? summary(env) : env.textarea;
    target.focus();
    let prevented = false;
    let stopped = false;
    env.document.dispatchEvent({
      type: "keydown", key: "Escape",
      preventDefault: () => { prevented = true; },
      stopPropagation: () => { stopped = true; },
    });
    assertExpanded(env, false);
    assert.equal(env.document.activeElement, inside ? summary(env) : env.textarea);
    assert.equal(prevented, inside);
    assert.equal(stopped, inside);
  }
});

test("pointerdown inside details keeps them open, outside closes them", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  click(summary(env));
  assert.equal(panel(env).parentElement.parentElement, env.document.body);
  assert.equal(env.bar().contains(panel(env)), false, "details must escape the clipping composer");
  env.document.dispatchEvent({ type: "pointerdown", target: panel(env).querySelector("dd") });
  assertExpanded(env, true);
  env.textarea.focus();
  env.document.dispatchEvent({ type: "pointerdown", target: env.textarea });
  assertExpanded(env, false);
  assert.equal(env.document.activeElement, env.textarea);
});

test("Escape from the independent panel restores its trigger focus", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  click(summary(env));
  panel(env).focus();
  env.document.dispatchEvent({ type: "keydown", key: "Escape", preventDefault() {}, stopPropagation() {} });
  assertExpanded(env, false);
  assert.equal(env.document.activeElement, summary(env));
});

test("native popovers switch and close together with their triggers", async () => {
  const env = createEnvironment({ popover: true });
  await flushTimers(30);
  // 关联只供无障碍树推导展开状态；原生点击切换必须被阻止，否则会与 setExpanded 互相抵消。
  assert.equal(summary(env).popoverTargetElement, panel(env));
  assert.equal(click(summary(env)).defaultPrevented, true);
  assert.equal(panel(env).popoverOpen, true);
  const tokens = env.bar().__codeyCs.panels.tokens;
  click(env.bar().__codeyCs.buttons.tokens);
  assert.equal(panel(env).popoverOpen, false);
  assert.equal(tokens.popoverOpen, true);
  env.document.dispatchEvent({ type: "pointerdown", target: env.textarea });
  assert.equal(tokens.popoverOpen, false);
});

test("reuses one detached bar and overlay and closes panels on remount", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  const originalBar = env.bar();
  const overlay = panel(env).parentElement;
  click(summary(env));
  originalBar.remove();
  env.emitMutation([{ type: "childList", target: env.scope, removedNodes: [originalBar] }]);
  await flushTimers(160);
  assert.equal(env.bar(), originalBar);
  assert.equal(panel(env).parentElement, overlay);
  assert.equal(env.document.querySelectorAll("#codey-conversation-stats-overlay").length, 1);
  assertExpanded(env, false);
  overlay.remove();
  env.emitMutation([{ type: "childList", target: env.document.body, removedNodes: [overlay] }]);
  await flushTimers(160);
  assert.equal(overlay.parentElement, env.document.body);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
});

test("dispose removes portal, listeners and pending response without resurrecting UI", async () => {
  let resolve;
  const env = createEnvironment({ request: () => new Promise(done => { resolve = done; }) });
  await flushTimers(30);
  env.dispose();
  assert.equal(env.bar(), null);
  assert.equal(env.document.getElementById("codey-conversation-stats-overlay"), null);
  assert.equal(env.document.getElementById("codey-conversation-stats-styles"), null);
  assert.ok([...env.document.listeners.values(), ...env.window.listeners.values()].every(list => list.length === 0));
  resolve({ status: "ok", stats: STATS });
  await flushTimers(30);
  assert.equal(env.bar(), null);
  assert.equal(env.calls.length, 1);
});

test("version replacement disposes previous ownership before installing one new portal", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  const originalBar = env.bar();
  env.markPreviousVersion();
  env.rerun();
  await flushTimers(30);
  assert.notEqual(env.bar(), originalBar);
  assert.equal(env.document.querySelectorAll("#codey-conversation-stats-overlay").length, 1);
  assert.equal(env.document.listeners.get("pointerdown").length, 1);
  click(summary(env));
  assertExpanded(env, true);
});

test("hiding the composer closes its open details", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  click(summary(env));
  env.textarea.visible = false;
  env.textarea.setAttribute("hidden", "");
  env.emitMutation([{ type: "attributes", target: env.textarea, attributeName: "hidden" }]);
  await flushTimers(160);
  assert.equal(env.bar().hidden, true);
  assertExpanded(env, false);
});

test("keeps current totals on failed refresh and recovers after retry", async () => {
  let response = { status: "ok", stats: STATS };
  const env = createEnvironment({ request: () => {
    if (response instanceof Error) throw response;
    return response;
  } });
  await flushTimers(30);
  for (const failure of [{ status: "failed" }, new Error("offline")]) {
    response = failure;
    env.refresh();
    await flushTimers(30);
    assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
    assert.equal(metric(env.bar(), "status"), "统计更新失败");
    assert.equal(env.bar().hidden, false);
  }
  response = { status: "ok", stats: { ...STATS, totalTokens: 200 } };
  env.refresh();
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "200 Tokens");
  assert.equal(metric(env.bar(), "status"), "运行期统计");
});

test("discards the original A response after switching A to B to A", async () => {
  const pending = [];
  const env = createEnvironment({ request: (sessionId) => new Promise((resolve) => {
    pending.push({ sessionId, resolve });
  }) });
  await flushTimers(30);
  assert.equal(pending[0].sessionId, "conversation-1");
  click(summary(env));
  env.setConversation("conversation-2");
  env.emitAnchorMutation();
  await flushTimers(160);
  assertExpanded(env, false);
  env.setConversation("conversation-1");
  env.emitAnchorMutation();
  await flushTimers(160);
  pending[0].resolve({ status: "ok", stats: STATS });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "—", "old A request must not repopulate A after a round trip");
  assert.equal(pending.length, 2, "current A must fetch again immediately after the stale request settles");
  assert.equal(pending[1].sessionId, "conversation-1");
  pending[1].resolve({ status: "ok", stats: { ...STATS, totalTokens: 700 } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "700 Tokens");
});

test("restores historical totals without inventing historical request information", async () => {
  const history = { ...STATS, usageSource: "rollout", usageScope: "session", requestsScope: null, requests: null, inFlight: null, cacheWriteInputTokens: null };
  const env = createEnvironment({ statsBySession: { "conversation-1": history } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  assert.equal(metric(env.bar(), "status"), "历史统计");
  assert.match(env.bar().querySelector(".codey-cs-tokens-summary").getAttribute("aria-label"), /会话累计 总量 132k Tokens/);
  assert.equal(env.bar().dataset.running, "false");
});

test("historical tokens and current runtime request details keep separate scopes", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS, usageSource: "rollout", usageScope: "session" } } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  assert.equal(metric(env.bar(), "speed"), "188 Token/s");
  env.setStats("conversation-1", { ...STATS, usageSource: "rollout", usageScope: "session", lastTurn: { ...LAST_TURN, averageOutputTokensPerSecond: null } });
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "—");
});

test("session totals own the turn count while the last turn owns speed, steps and timing", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": {
    ...STATS, turnCount: 99, stepCount: 500, modelDurationMs: 900000,
    toolDurationMs: 800000, averageTtftMs: 9999, averageOutputTokensPerSecond: 999,
    outputTokensPerSecond: 777,
    lastTurn: { ...LAST_TURN, stepCount: 3, averageOutputTokensPerSecond: 20 },
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "activity"), "本轮 3 步");
  assert.equal(field(env.bar(), "modelDurationMs"), "10秒");
  assert.equal(field(env.bar(), "toolDurationMs"), "3秒");
  assert.equal(field(env.bar(), "averageTtftMs"), "0.5秒");
  assert.equal(metric(env.bar(), "speed"), "20.0 Token/s");
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  assert.equal(field(env.bar(), "outputTokens"), "3,100 Tokens");
  assert.equal(panel(env).querySelectorAll("dt")[0].textContent, "平均生成速度");
  assert.equal(env.bar().__codeyCs.notes.activity.textContent, "");
});

test("uses only the reported session turn count and preserves zero independently of last turn availability", async () => {
  const env = createEnvironment();
  try {
    for (const turnCount of [undefined, null, NaN, Infinity, -1, 1.5, "4", 0, 4]) {
      env.setStats("conversation-1", { ...STATS, turnCount, requests: 900,
        lastTurn: { ...LAST_TURN, id: "turn-50" } });
      env.refresh();
      await flushTimers(30);
      const expected = turnCount === 0 || turnCount === 4 ? String(turnCount) : "—";
      assert.equal(env.bar().__codeyCs.headingTotal.textContent, `会话 ${expected} 轮`);
      assert.equal(metric(env.bar(), "activity"), "本轮 2 步");
      assert.equal(metric(env.bar(), "speed"), "188 Token/s");
    }
    env.setStats("conversation-1", { turnCount: 0 });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "activity"), "本轮 — 步");
    assert.equal(metric(env.bar(), "speed"), "—");
    assert.notEqual(env.bar().dataset.codeyState, "empty");
  } finally {
    env.dispose();
  }
});

test("does not reuse session or request statistics without an identified turn", async () => {
  const env = createEnvironment();
  for (const lastTurn of [undefined, null, {}, { ...LAST_TURN, id: "" }, { ...LAST_TURN, id: " " }]) {
    env.setStats("conversation-1", { ...STATS, lastTurn, modelDurationMs: 500, toolDurationMs: 10, stepCount: 9 });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "activity"), "本轮 — 步");
    for (const name of ["modelDurationMs", "toolDurationMs", "averageTtftMs", "speed"]) assert.equal(field(env.bar(), name), "—");
    assert.match(env.bar().__codeyCs.notes.activity.textContent, /暂无可识别的最近一轮/);
  }
});

test("keeps the same turn average through multiple steps and tool intervals without recreating panels", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  click(summary(env));
  const originalPanel = panel(env);
  for (const inFlight of [1, 0, 1]) {
    env.setStats("conversation-1", { ...STATS, outputTokensPerSecond: null,
      lastTurn: { ...LAST_TURN, status: "running", stepCount: 3, inFlight, speedState: "waiting" } });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "speed"), "188 Token/s");
    assert.equal(field(env.bar(), "speed"), "188 Token/s");
    assert.equal(env.bar().dataset.running, "true");
    assert.equal(panel(env), originalPanel);
    assertExpanded(env, true);
  }
});

test("a new user turn clears all prior turn values without clearing session tokens", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  click(summary(env));
  env.setStats("conversation-1", { ...STATS, turnCount: 5, lastTurn: { id: "turn-2", status: "running", stepCount: 0,
    averageOutputTokensPerSecond: null, speedState: "waiting", coverage: "runtime" } });
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "等待输出");
  assert.equal(metric(env.bar(), "activity"), "本轮 0 步");
  for (const name of ["modelDurationMs", "toolDurationMs", "averageTtftMs"]) assert.equal(field(env.bar(), name), "—");
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  assertExpanded(env, true);
});

test("distinguishes waiting, streaming, estimated average and completed average for one turn", async () => {
  const env = createEnvironment();
  for (const [changes, expected] of [
    [{ status: "running", speedState: "waiting", averageOutputTokensPerSecond: null }, "等待输出"],
    [{ status: "running", speedState: "streaming", averageOutputTokensPerSecond: null }, "生成中"],
    [{ status: "running", speedIsEstimate: true, averageOutputTokensPerSecond: 62.35 }, "≈62.4 Token/s"],
    [{ status: "running", speedIsEstimate: false, averageOutputTokensPerSecond: 65 }, "65.0 Token/s"],
    [{ status: "completed", averageOutputTokensPerSecond: 71.2 }, "71.2 Token/s"],
    [{ status: "completed", averageOutputTokensPerSecond: null }, "—"],
  ]) {
    env.setStats("conversation-1", { ...STATS, lastTurn: { ...LAST_TURN, ...changes } });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "speed"), expected);
    assert.equal(field(env.bar(), "speed"), expected);
    assert.match(summary(env).getAttribute("aria-label"), /平均生成速度/);
    assert.equal(env.bar().__codeyCs.speed.title.includes("包含生成中的 Token 估算"), changes.speedIsEstimate === true);
    assert.equal(env.bar().__codeyCs.notes.activity.textContent, "");
  }
});

test("uses the backend weighted turn average and preserves valid zero values", async () => {
  // 两步各输出 100 token，用时 1 秒和 9 秒，轮平均为 20。
  const env = createEnvironment();
  for (const averageOutputTokensPerSecond of [200 / 10, 0]) {
    env.setStats("conversation-1", { ...STATS, lastTurn: { ...LAST_TURN,
      averageOutputTokensPerSecond, stepCount: 0, modelDurationMs: 0, toolDurationMs: 0, averageTtftMs: 0 } });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "speed"), `${averageOutputTokensPerSecond.toFixed(1)} Token/s`);
    assert.equal(metric(env.bar(), "activity"), "本轮 0 步");
    for (const name of ["modelDurationMs", "toolDurationMs", "averageTtftMs"]) assert.equal(field(env.bar(), name), "0秒");
  }
});

test("rejects invalid last turn values and keeps absent values unknown", async () => {
  const env = createEnvironment();
  for (const value of [undefined, null, NaN, Infinity, -5]) {
    env.setStats("conversation-1", { ...STATS, lastTurn: { ...LAST_TURN,
      averageOutputTokensPerSecond: value, stepCount: value, modelDurationMs: value, toolDurationMs: value, averageTtftMs: value } });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "activity"), "本轮 — 步");
    for (const name of ["speed", "modelDurationMs", "toolDurationMs", "averageTtftMs"]) assert.equal(field(env.bar(), name), "—");
  }
});

test("explains limited coverage of historical and partial turn records", async () => {
  const env = createEnvironment();
  for (const [coverage, note] of [
    ["recorded", "仅包含可恢复且有有效计时的记录"],
    ["recorded_and_runtime", "合并可恢复记录与本次运行统计，缺失记录不计入"],
    ["partial", "本轮记录不完整，仅统计可恢复部分"],
    ["runtime", "仅包含本次运行可统计的步骤"],
    ["unknown", "本轮统计范围未知"],
  ]) {
    env.setStats("conversation-1", { ...STATS, requestsScope: null, requests: null,
      lastTurn: { ...LAST_TURN, coverage } });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "speed"), "188 Token/s");
    assert.ok(env.bar().__codeyCs.turnCoverage.title.includes(note));
    assert.equal(env.bar().__codeyCs.turnCoverage.dataset.tone, coverage === "partial" ? "warn" : "");
    assert.equal(env.bar().__codeyCs.notes.activity.textContent, "");
  }
});

test("hides a frozen live turn speed after failure and resumes without changing its panel", async () => {
  const live = { ...STATS, lastTurn: { ...LAST_TURN, status: "running", speedIsEstimate: true } };
  let response = { status: "ok", stats: live };
  const env = createEnvironment({ request: () => response });
  await flushTimers(30);
  click(summary(env));
  const originalPanel = panel(env);
  assert.equal(metric(env.bar(), "speed"), "≈188 Token/s");
  response = { status: "failed" };
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "更新暂停");
  assert.equal(field(env.bar(), "speed"), "更新暂停");
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  response = { status: "ok", stats: { ...live, lastTurn: { ...live.lastTurn, averageOutputTokensPerSecond: 54 } } };
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "≈54.0 Token/s");
  assert.equal(panel(env), originalPanel);
  assertExpanded(env, true);
});

test("clears live turn speed and waiting state on conversation changes", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": {
    ...STATS, lastTurn: { ...LAST_TURN, status: "running", speedIsEstimate: true },
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "≈188 Token/s");
  env.setConversation("conversation-2");
  env.emitAnchorMutation();
  await flushTimers(160);
  assert.equal(metric(env.bar(), "speed"), "—");
  assert.equal(env.bar().dataset.running, "false");
});

test("partial historical usage keeps absent values unknown and preserves explicit zero", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": {
    usageSource: "rollout", usageScope: "session", requestsScope: null,
    totalTokens: null, inputTokens: 0, outputTokens: null, cachedInputTokens: null,
  } } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "outputTokens"), "—");
  for (const name of ["total", "cache-rate", "speed"]) assert.equal(metric(env.bar(), name), "—");
  assert.equal(metric(env.bar(), "status"), "历史统计");
});

test("normalizes local conversation IDs before reading historical stats", async () => {
  const env = createEnvironment({ conversationId: " local:conversation-1 ", statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  assert.equal(env.calls[0].payload.sessionId, "conversation-1");
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
});

test("preserves the same session values while refreshing and marks failed values stale", async () => {
  let resolveRefresh;
  let first = true;
  const env = createEnvironment({ request: () => {
    if (first) { first = false; return { status: "ok", stats: STATS }; }
    return new Promise((resolve) => { resolveRefresh = resolve; });
  } });
  await flushTimers(30);
  env.refresh();
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  assert.equal(metric(env.bar(), "status"), "运行期统计", "an in-flight refresh keeps the current status");
  resolveRefresh({ status: "failed" });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 Tokens");
  assert.match(env.bar().__codeyCs.notes.activity.textContent, /可能已过期/);
});

test("shows reference activity rows and computes uncached input without double counting", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS,
    lastTurn: { ...LAST_TURN, stepCount: 209, modelDurationMs: 1286000, toolDurationMs: 573000,
      averageTtftMs: 2800, coverage: "recorded" },
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "activity"), "本轮 209 步");
  assert.equal(field(env.bar(), "modelDurationMs"), "21分 26秒");
  assert.equal(field(env.bar(), "toolDurationMs"), "9分 33秒");
  assert.equal(field(env.bar(), "averageTtftMs"), "2.8秒");
  assert.equal(field(env.bar(), "uncachedInputTokens"), "8,400 Tokens");
  assert.equal(env.bar().querySelectorAll(".codey-cs-summary").length, 2);
});

test("formats long durations by hours and keeps model wait, tool accumulation and coverage distinct", async () => {
  const env = createEnvironment();
  try {
    for (const status of ["completed", "running"]) {
      env.setStats("conversation-1", { ...STATS,
        lastTurn: { ...LAST_TURN, status, modelDurationMs: 15_543_000, toolDurationMs: 14_567_000,
          averageTtftMs: 4_800, coverage: "recorded_and_runtime" },
      });
      env.refresh();
      await flushTimers(30);
      assert.equal(field(env.bar(), "modelDurationMs"), "4小时 19分 3秒");
      assert.equal(field(env.bar(), "toolDurationMs"), "4小时 2分 47秒");
      assert.equal(env.bar().__codeyCs.turnCoverage.textContent, "会话记录 + 本次运行");
      assert.equal(env.bar().__codeyCs.notes.activity.textContent, "");
    }
  } finally {
    env.dispose();
  }
});

test("renders merged timing of one turn without accumulating again or restoring another session response", async () => {
  // 后端已合并本轮历史与运行期计时，UI 只负责展示。
  const merged = { ...STATS, usageSource: "rollout", usageScope: "session",
    lastTurn: { ...LAST_TURN, modelDurationMs: 120500, averageTtftMs: 750,
      averageOutputTokensPerSecond: 42, coverage: "recorded_and_runtime" },
  };
  const runtime = { ...STATS, lastTurn: { ...LAST_TURN, id: "another-turn", modelDurationMs: 500, averageTtftMs: 200,
    coverage: "runtime" } };
  let pendingRefresh;
  let deferNext = false;
  const env = createEnvironment({ request: sessionId => {
    if (deferNext) {
      deferNext = false;
      return new Promise(resolve => { pendingRefresh = resolve; });
    }
    return { status: "ok", stats: sessionId === "conversation-1" ? merged : runtime };
  } });
  const assertMerged = () => {
    assert.equal(field(env.bar(), "modelDurationMs"), "2分");
    assert.equal(field(env.bar(), "averageTtftMs"), "0.8秒");
    assert.equal(field(env.bar(), "speed"), "42.0 Token/s");
    assert.equal(metric(env.bar(), "speed"), "42.0 Token/s");
    assert.match(env.bar().__codeyCs.turnCoverage.title, /合并可恢复记录与本次运行统计/);
    assert.doesNotMatch(env.bar().__codeyCs.turnCoverage.title, /仅包含本次运行可统计的步骤/);
  };
  try {
    await flushTimers(30);
    click(summary(env));
    assertMerged();
    env.refresh();
    await flushTimers(30);
    assertMerged();
    assertExpanded(env, true);

    env.setConversation("conversation-2");
    env.emitAnchorMutation();
    await flushTimers(160);
    assert.equal(field(env.bar(), "modelDurationMs"), "0.5秒");
    assert.match(env.bar().__codeyCs.turnCoverage.title, /仅包含本次运行可统计的步骤/);

    deferNext = true;
    env.refresh();
    await flushTimers(30);
    env.setConversation("conversation-1");
    env.emitAnchorMutation();
    await flushTimers(160);
    assert.equal(field(env.bar(), "modelDurationMs"), "—");
    pendingRefresh({ status: "ok", stats: runtime });
    await flushTimers(30);
    assertMerged();
    assert.equal(env.calls.at(-1).payload.sessionId, "conversation-1");
  } finally {
    env.dispose();
  }
});

test("preview remains open during pointer travel and click pins it", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  summary(env).dispatchEvent({ type: "pointerenter" });
  assertExpanded(env, true);
  summary(env).dispatchEvent({ type: "pointerleave" });
  panel(env).dispatchEvent({ type: "pointerenter" });
  await flushTimers(380);
  assertExpanded(env, true);
  panel(env).dispatchEvent({ type: "pointerleave" });
  await flushTimers(380);
  assertExpanded(env, false);
  summary(env).dispatchEvent({ type: "focus" });
  click(summary(env));
  summary(env).dispatchEvent({ type: "pointerleave" });
  await flushTimers(380);
  assertExpanded(env, true);
});

test("does not fabricate missing activity or subtract inconsistent cache", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS,
    lastTurn: { ...LAST_TURN, stepCount: null, toolDurationMs: 0, modelDurationMs: -1,
      averageTtftMs: null }, cachedInputTokens: 999999,
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "activity"), "本轮 — 步");
  assert.equal(field(env.bar(), "toolDurationMs"), "0秒");
  for (const key of ["modelDurationMs", "averageTtftMs", "uncachedInputTokens", "cacheRate"]) assert.equal(field(env.bar(), key), "—");
});

test("explains turn boundaries and accumulated tool timing", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS,
    lastTurn: { ...LAST_TURN, coverage: "recorded" },
  } } });
  await flushTimers(30);
  const note = env.bar().__codeyCs.notes.activity.textContent;
  assert.equal(env.bar().__codeyCs.activity.textContent, "本轮 2 步");
  assert.equal(env.bar().__codeyCs.headingTotal.textContent, "会话 4 轮");
  assert.equal(note, "");
  assert.match(env.bar().__codeyCs.speed.title, /按有效步骤的生成 Token 总量与生成耗时计算/);
  assert.match(env.bar().__codeyCs.turnCoverage.title, /仅包含可恢复且有有效计时的记录/);
});

test("renders token composition, cache ring and panel subtitles without rewriting unchanged styles", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    const refs = env.bar().__codeyCs;
    const grow = key => refs.segments[key].style.getPropertyValue("flex-grow");
    const activityGrow = key => refs.activitySegments[key].style.getPropertyValue("flex-grow");
    assert.equal(refs.composition.hidden, false);
    assert.deepEqual(["cachedInputTokens", "uncachedInputTokens", "outputTokens"].map(grow), ["913", "64", "24"]);
    assert.equal(refs.activityComposition.hidden, false);
    assert.deepEqual(["modelDurationMs", "toolDurationMs"].map(activityGrow), ["769", "231"]);
    assert.equal(refs.rows.speed.getAttribute("data-tier"), "primary");
    assert.equal(refs.rows.averageTtftMs.getAttribute("data-tier"), "primary");
    assert.equal(refs.rows.modelDurationMs.getAttribute("data-tier"), "breakdown");
    assert.equal(refs.rows.inputTokens.getAttribute("data-tier"), "primary");
    assert.equal(refs.rows.outputTokens.getAttribute("data-tier"), "primary");
    assert.equal(refs.rows.cachedInputTokens.getAttribute("data-tier"), "breakdown");
    assert.equal(refs.rows.cacheRate.getAttribute("data-tier"), "breakdown");
    assert.equal(refs.ring.style.getPropertyValue("--cs-rate"), "93");
    assert.equal(refs.rows.cacheWriteInputTokens.hidden, false);
    assert.equal(field(env.bar(), "cacheWriteInputTokens"), "2,000 Tokens");
    assert.equal(refs.subtitles.activity.textContent, "2 步 · 已完成");
    assert.equal(refs.subtitles.tokens.textContent, "本次运行");
    assert.equal(refs.notes.tokens.textContent, "");

    let styleWrites = 0;
    for (const element of [refs.ring, ...Object.values(refs.segments), ...Object.values(refs.activitySegments)]) {
      const setProperty = element.style.setProperty;
      element.style.setProperty = (...args) => { styleWrites += 1; setProperty(...args); };
    }
    env.refresh();
    await flushTimers(30);
    assert.equal(styleWrites, 0, "unchanged stats must not rewrite composition styles");

    env.setStats("conversation-1", { ...STATS, cachedInputTokens: null, cacheWriteInputTokens: null,
      lastTurn: { ...LAST_TURN, status: "running" } });
    env.refresh();
    await flushTimers(30);
    assert.equal(refs.composition.hidden, true);
    assert.equal(refs.ring.style.getPropertyValue("--cs-rate"), "0");
    assert.equal(refs.rows.cacheWriteInputTokens.hidden, true);
    assert.equal(refs.subtitles.activity.textContent, "2 步 · 进行中");
  } finally {
    env.dispose();
  }
});

const pollTimers = () => {
  const timers = [];
  return {
    timers,
    onTimer: (delay, run) => { if (delay >= 1_000) timers.push({ delay, run }); },
    // 直接触发最近一次排定的轮询，避免测试真实等待数秒。
    async fireLatest() {
      const timer = timers.at(-1);
      timers.length = 0;
      timer.run();
      await flushTimers(30);
      return timers.at(-1)?.delay;
    },
  };
};

test("backs off idle polling while results stay unchanged and resets on change or activity", async () => {
  const polls = pollTimers();
  const env = createEnvironment({ onTimer: polls.onTimer, statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    assert.equal(polls.timers.at(-1).delay, 3_000);
    assert.equal(await polls.fireLatest(), 6_000);
    assert.equal(await polls.fireLatest(), 12_000);
    assert.equal(await polls.fireLatest(), 12_000, "idle backoff is capped");
    env.setStats("conversation-1", { ...STATS, totalTokens: 140_000 });
    assert.equal(await polls.fireLatest(), 3_000, "changed stats reset the backoff");
    env.setStats("conversation-1", { ...STATS, lastTurn: { ...LAST_TURN, status: "running" } });
    assert.equal(await polls.fireLatest(), 1_100);
    assert.equal(await polls.fireLatest(), 1_100, "a running turn never backs off");
  } finally {
    env.dispose();
  }
});

test("polls soon after the composer submits and keeps fast polling while the turn starts", async () => {
  const delays = [];
  const env = createEnvironment({ onTimer: delay => delays.push(delay), statsBySession: { "conversation-1": STATS } });
  try {
    await flushTimers(30);
    const fetches = () => env.calls.filter(call => call.path === "/session/stats").length;
    const before = fetches();
    env.document.dispatchEvent({ type: "keydown", key: "Enter", shiftKey: true, target: env.textarea });
    env.document.dispatchEvent({ type: "keydown", key: "Enter", isComposing: true, target: env.textarea });
    env.document.dispatchEvent({ type: "keydown", key: "Enter", target: env.document.body });
    assert.equal(delays.includes(300), false, "newlines, IME input and keys outside the composer do not poll");

    delays.length = 0;
    env.document.dispatchEvent({ type: "keydown", key: "Enter", target: env.textarea });
    assert.deepEqual(delays, [300]);
    await flushTimers(330);
    assert.equal(fetches(), before + 1);
    assert.equal(delays.at(-1), 1_100, "the submit watch window polls at the active rate");

    delays.length = 0;
    const send = env.scope.appendChild(env.document.createElement("button"));
    env.document.dispatchEvent({ type: "click", target: send });
    assert.deepEqual(delays, [300]);
    delays.length = 0;
    env.document.dispatchEvent({ type: "click", target: env.bar().querySelector(".codey-cs-tokens-summary") });
    assert.equal(delays.includes(300), false, "the stats buttons themselves do not count as a submit");
  } finally {
    env.dispose();
  }
});
