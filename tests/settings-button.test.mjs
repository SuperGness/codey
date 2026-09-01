import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/renderer-inject.js", import.meta.url), "utf8");
const bridgeSource = readFileSync(new URL("../public/codey-bridge.js", import.meta.url), "utf8");

const runRenderer = (sandbox) => {
  const context = vm.createContext(sandbox);
  vm.runInContext(bridgeSource, context);
  vm.runInContext(source, context);
};

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", { visible = true, right = 100, width = right, height = 46, top = 0 } = {}) {
    super(tagName);
    this.right = right;
    this.width = width;
    this.height = height;
    this.top = top;
    this.visible = visible;
    this.rectReads = 0;
  }

  insertBefore(child, before) {
    child.remove();
    const index = this.children.indexOf(before);
    assert.notEqual(index, -1);
    child.parentElement = this;
    child.isConnected = true;
    this.children.splice(index, 0, child);
    return child;
  }

  closest() {
    return null;
  }

  getBoundingClientRect() {
    this.rectReads += 1;
    return this.visible
      ? {
          bottom: this.top + this.height,
          height: this.height,
          left: this.right - this.width,
          right: this.right,
          top: this.top,
          width: this.width,
        }
      : { bottom: 0, height: 0, left: 0, right: 0, top: 0, width: 0 };
  }

  getClientRects() {
    return this.visible ? [this.getBoundingClientRect()] : [];
  }

  querySelector() {
    return super.querySelector(...arguments);
  }

  querySelectorAll(selector) {
    return super.querySelectorAll(selector);
  }

  matches(selector) {
    return selector
      .split(",")
      .some((part) => super.matches(part.trim()));
  }

  closest(selector) {
    return super.closest(selector);
  }
}

test("moves the Codey button beside the visible header's trailing action region", () => {
  const hiddenHeader = new FakeElement("header", { visible: false });
  const visibleHeader = new FakeElement("header", { right: 1200 });
  const rightRegion = new FakeElement("div", { right: 1200, width: 70 });
  const actionRow = new FakeElement("div", { right: 1192, width: 62 });
  const controlWrapper = new FakeElement("span", { right: 1192, width: 28 });
  const nativeButton = new FakeElement("button", { right: 1192, width: 28 });
  const codeyButton = new FakeElement("button", { right: 200, width: 32 });
  codeyButton.id = "codey-settings-button";
  hiddenHeader.appendChild(codeyButton);
  visibleHeader.appendChild(rightRegion);
  rightRegion.appendChild(actionRow);
  actionRow.appendChild(controlWrapper);
  controlWrapper.appendChild(nativeButton);

  const placeholders = {
    "codey-injected-style": new FakeElement("style"),
    "codey-message-toolbar": new FakeElement(),
    "codey-settings-button": codeyButton,
  };
  const document = {
    body: new FakeElement("body"),
    documentElement: new FakeElement("html"),
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: (id) => placeholders[id] || null,
    querySelector: () => null,
    querySelectorAll: (selector) => (selector === "header" ? [hiddenHeader, visibleHeader] : []),
  };
  const window = {
    addEventListener() {},
    clearTimeout() {},
    dispatchEvent() {},
    getComputedStyle: (element) => ({
      display: element.visible ? "flex" : "none",
      visibility: element.visible ? "visible" : "hidden",
    }),
    localStorage: { getItem: () => null, key: () => null, length: 0, setItem() {} },
    setTimeout: () => 1,
  };
  window.window = window;

  runRenderer({
    console,
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      disconnect() {}
      observe() {}
    },
    URLSearchParams,
    window,
  });

  assert.equal(codeyButton.parentElement, visibleHeader);
  assert.equal(codeyButton.dataset.codeyHeaderActions, "true");
  assert.equal(hiddenHeader.children.includes(codeyButton), false);
  assert.deepEqual(visibleHeader.children, [codeyButton, rightRegion]);
});

test("renders weekly and optional five-hour usage above the sidebar account", async () => {
  const visibleHeader = new FakeElement("header", { right: 1200 });
  const sessionTitle = new FakeElement("div", { right: 700, width: 240 });
  sessionTitle.textContent = "当前会话";
  const rightRegion = new FakeElement("div", { right: 1200, width: 70 });
  const nativeButton = new FakeElement("button", { right: 1192, width: 28 });
  rightRegion.appendChild(nativeButton);
  visibleHeader.appendChild(sessionTitle);
  visibleHeader.appendChild(rightRegion);

  const sidebarRoot = new FakeElement("div", {
    right: 320,
    width: 320,
    height: 800,
  });
  const sidebarNavigation = new FakeElement("nav", {
    right: 320,
    width: 320,
    height: 720,
  });
  const sidebarScroll = new FakeElement("div", {
    right: 320,
    width: 320,
    height: 620,
  });
  sidebarScroll.setAttribute("data-app-action-sidebar-scroll", "");
  sidebarNavigation.appendChild(sidebarScroll);
  const sidebarFooterHost = new FakeElement("div", {
    right: 320,
    width: 320,
    height: 64,
    top: 736,
  });
  const nativeProfileFooter = new FakeElement("div", {
    right: 320,
    width: 320,
    height: 64,
    top: 736,
  });
  nativeProfileFooter.appendChild(new FakeElement("button", {
    right: 220,
    width: 200,
    height: 40,
    top: 748,
  }));
  sidebarFooterHost.appendChild(nativeProfileFooter);
  sidebarRoot.appendChild(sidebarNavigation);
  sidebarRoot.appendChild(sidebarFooterHost);

  const documentElement = new FakeElement("html", {
    right: 1200,
    width: 1200,
    height: 800,
  });
  documentElement.appendChild(sidebarRoot);
  const findById = (id) => {
    let result = null;
    const visit = (element) => {
      if (result) return;
      if (element.id === id) {
        result = element;
        return;
      }
      element.children.forEach(visit);
    };
    visit(documentElement);
    visit(document.body);
    visit(visibleHeader);
    return result;
  };
  const document = {
    body: new FakeElement("body"),
    documentElement,
    visibilityState: "visible",
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: findById,
    querySelector: (selector) => document.querySelectorAll(selector)[0] || null,
    querySelectorAll: (selector) => {
      if (selector === "header") return [visibleHeader];
      if (selector === "nav") return [sidebarNavigation];
      return documentElement.querySelectorAll(selector);
    },
  };
  const windowListeners = new Map();
  const addWindowListener = (type, handler) => {
    const handlers = windowListeners.get(type) || [];
    handlers.push(handler);
    windowListeners.set(type, handlers);
  };
  const removeWindowListener = (type, handler) => {
    const handlers = windowListeners.get(type) || [];
    windowListeners.set(type, handlers.filter((candidate) => candidate !== handler));
  };
  const dispatchWindowEvent = (event) => {
    for (const handler of [...(windowListeners.get(event.type) || [])]) {
      handler(event);
    }
  };
  const todayResetAt = new Date();
  todayResetAt.setHours(23, 45, 0, 0);
  const tomorrowResetAt = new Date(todayResetAt);
  tomorrowResetAt.setDate(todayResetAt.getDate() + 1);
  let accountUsageResult = {
    status: "ok",
    planType: "pro",
    fetchedAt: Math.floor(Date.now() / 1000),
    primary: {
      usedPercent: 15,
      windowMinutes: 300,
      resetsAt: Math.floor(todayResetAt.getTime() / 1000),
    },
    secondary: {
      usedPercent: 40,
      windowMinutes: 10080,
      resetsAt: Math.floor(tomorrowResetAt.getTime() / 1000),
    },
    credits: {
      hasCredits: true,
      unlimited: false,
      balance: "42",
    },
  };
  let accountUsageCalls = 0;
  let appServerUsageCalls = 0;
  const appServerUsageResult = {
    rateLimits: {
      limitId: "codex",
      primary: {
        usedPercent: 20,
        windowDurationMins: 10_080,
        resetsAt: Math.floor(tomorrowResetAt.getTime() / 1000),
      },
      credits: {
        hasCredits: true,
        unlimited: false,
        balance: "77",
      },
      planType: "plus",
    },
    rateLimitsByLimitId: {
      codex_bengalfox: {
        limitId: "codex_bengalfox",
        primary: {
          usedPercent: 35,
          windowDurationMins: 300,
          resetsAt: Math.floor(todayResetAt.getTime() / 1000),
        },
      },
    },
  };
  const scheduledDelays = [];
  const window = {
    __codeyReadAccountRateLimits: async () => {
      appServerUsageCalls += 1;
      return appServerUsageResult;
    },
    __codeySessionToolsInjectLoaded: true,
    __codexSessionDeleteBridge: async (path) => {
      if (path === "/account/usage") {
        accountUsageCalls += 1;
        return accountUsageResult;
      }
      if (path === "/backend/status") return { status: "ok", availableUpdate: null };
      if (path === "/backend/health") return { status: "ok" };
      throw new Error(`unexpected bridge path: ${path}`);
    },
    addEventListener: addWindowListener,
    alert() {},
    clearTimeout() {},
    dispatchEvent: dispatchWindowEvent,
    getComputedStyle: () => ({ display: "flex", visibility: "visible" }),
    innerHeight: 800,
    innerWidth: 1200,
    localStorage: { getItem: () => null, key: () => null, length: 0, setItem() {} },
    removeEventListener: removeWindowListener,
    setTimeout: (_callback, delay) => {
      scheduledDelays.push(delay);
      return scheduledDelays.length;
    },
  };
  window.window = window;

  runRenderer({
    console,
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      disconnect() {}
      observe() {}
    },
    URLSearchParams,
    window,
  });

  await window.__codeyRefreshAccountUsage();

  const usage = findById("codey-account-usage");
  const settingsButton = findById("codey-settings-button");
  const injectedStyle = findById("codey-core-injected-style");
  assert.ok(usage);
  assert.ok(settingsButton);
  assert.ok(injectedStyle);
  assert.match(injectedStyle.textContent, /#codey-account-usage \{[^}]*background: transparent;/);
  assert.doesNotMatch(injectedStyle.textContent, /#codey-account-usage \{[^}]*background:[^;}]*Canvas/);
  assert.match(injectedStyle.textContent, /\.codey-usage-meter \{ display: block;[^}]*height: 2px;[^}]*max-height: 2px;/);
  assert.match(injectedStyle.textContent, /data-tone="healthy"[^}]*background: #34c759;/);
  assert.match(injectedStyle.textContent, /data-tone="normal"[^}]*background: #0a84ff;/);
  assert.match(injectedStyle.textContent, /data-tone="warning"[^}]*background: #ffcc00;/);
  assert.match(injectedStyle.textContent, /data-tone="critical"[^}]*background: #ff453a;/);
  assert.match(injectedStyle.textContent, /#codey-account-usage:hover \.codey-usage-details/);
  assert.match(injectedStyle.textContent, /\.codey-usage-details \{[^}]*background: rgb\(34 34 34 \/ \.97\);/);
  assert.match(injectedStyle.textContent, /\.codey-usage-plan-tag \{[^}]*border:[^}]*background: color-mix\(in srgb, #0a84ff 13%, transparent\);/);
  assert.equal(usage.parentElement, sidebarFooterHost);
  assert.equal(usage.nextElementSibling, nativeProfileFooter);
  assert.deepEqual(sidebarFooterHost.children, [usage, nativeProfileFooter]);
  assert.equal(sidebarFooterHost.getAttribute("data-codey-usage-host"), "true");
  assert.equal(sessionTitle.parentElement, visibleHeader);
  assert.equal(visibleHeader.children[0], sessionTitle);
  assert.equal(visibleHeader.getAttribute("data-codey-usage-host"), null);
  assert.match(usage.innerHTML, /class="codey-usage-list"/);
  assert.match(usage.innerHTML, /周额度[\s\S]*?60%[\s\S]*?5 小时[\s\S]*?85%/);
  assert.match(usage.innerHTML, /data-window="weekly" data-tone="normal"/);
  assert.match(usage.innerHTML, /data-window="five-hour" data-tone="healthy"/);
  assert.match(usage.innerHTML, /今天 \d{2}:\d{2} 重置/);
  assert.match(usage.innerHTML, /明天 \d{2}:\d{2} 重置/);
  const summaryHtml = usage.innerHTML.split('class="codey-usage-details"')[0];
  assert.match(summaryHtml, /class="codey-usage-plan-tag">Pro 20x<\/span>[\s\S]*?周额度[\s\S]*?60%/);
  assert.doesNotMatch(summaryHtml, /余额|42/);
  assert.match(usage.innerHTML, /class="codey-usage-details" role="tooltip"/);
  assert.doesNotMatch(usage.innerHTML, /codey-usage-details-plan/);
  assert.match(usage.innerHTML, /5 小时额度[\s\S]*?剩余 85%[\s\S]*?已用 15%/);
  assert.match(usage.innerHTML, /周额度[\s\S]*?剩余 60%[\s\S]*?已用 40%/);
  assert.match(usage.innerHTML, /Credits 余额[\s\S]*?42/);
  assert.match(usage.innerHTML, /更新于 \d{2}:\d{2}/);
  assert.equal(usage.dataset.windowCount, "2");
  assert.equal(usage.dataset.plan, undefined);
  assert.equal(usage.getAttribute("tabindex"), "0");
  assert.equal(usage.getAttribute("aria-describedby"), "codey-account-usage-details");
  assert.match(usage.getAttribute("aria-label"), /周额度剩余 60%/);
  assert.match(usage.getAttribute("aria-label"), /5 小时额度剩余 85%/);

  accountUsageResult = {
    ...accountUsageResult,
    primary: { ...accountUsageResult.primary, usedPercent: 65 },
    secondary: { ...accountUsageResult.secondary, usedPercent: 85 },
  };
  await window.__codeyRefreshAccountUsage();
  assert.match(usage.innerHTML, /data-window="weekly" data-tone="critical"[\s\S]*?15%/);
  assert.match(usage.innerHTML, /data-window="five-hour" data-tone="warning"[\s\S]*?35%/);

  accountUsageResult = {
    ...accountUsageResult,
    primary: accountUsageResult.secondary,
    secondary: null,
  };
  await window.__codeyRefreshAccountUsage();
  assert.equal(usage.dataset.windowCount, "1");
  assert.match(usage.innerHTML, /周额度/);
  assert.doesNotMatch(usage.innerHTML, /5 小时/);

  accountUsageResult = { status: "error", message: "官方额度接口返回 401" };
  await window.__codeyRefreshAccountUsage();
  assert.equal(appServerUsageCalls, 1);
  assert.equal(usage.dataset.windowCount, "2");
  assert.match(usage.innerHTML, /周额度[\s\S]*?80%[\s\S]*?5 小时[\s\S]*?65%/);
  assert.match(usage.innerHTML, /Credits 余额[\s\S]*?77/);
  assert.match(usage.innerHTML, /class="codey-usage-plan-tag">Plus<\/span>/);

  accountUsageResult = { status: "unavailable", reason: "third_party" };
  const refreshSchedulesBeforeUnavailable = scheduledDelays.filter(
    (delay) => delay === 60_000,
  ).length;
  await window.__codeyRefreshAccountUsage();
  assert.equal(findById("codey-account-usage"), null);
  assert.equal(sidebarFooterHost.getAttribute("data-codey-usage-host"), null);
  assert.equal(visibleHeader.getAttribute("data-codey-usage-host"), null);
  assert.equal(
    scheduledDelays.filter((delay) => delay === 60_000).length,
    refreshSchedulesBeforeUnavailable,
  );

  accountUsageResult = {
    status: "ok",
    planType: "plus",
    primary: {
      usedPercent: 20,
      windowMinutes: 10080,
      resetsAt: Math.floor(tomorrowResetAt.getTime() / 1000),
    },
  };
  await window.__codeyRefreshAccountUsage();
  const remountedUsage = findById("codey-account-usage");
  assert.ok(remountedUsage);
  assert.equal(accountUsageCalls, 6);
  assert.equal(appServerUsageCalls, 1);
  assert.equal(remountedUsage.parentElement, sidebarFooterHost);
  assert.equal(remountedUsage.nextElementSibling, nativeProfileFooter);
  assert.match(remountedUsage.innerHTML, /周额度/);
  const remountedSummaryHtml = remountedUsage.innerHTML.split('class="codey-usage-details"')[0];
  assert.doesNotMatch(remountedSummaryHtml, /5 小时/);
  assert.match(remountedSummaryHtml, /class="codey-usage-plan-tag">Plus<\/span>/);
});

const createStartupUpdateFixture = (bridge) => {
  const visibleHeader = new FakeElement("header", { right: 1200 });
  const documentElement = new FakeElement("html");
  const elementsById = new Map();
  let nextTimerId = 1;
  const timers = [];
  const events = [];
  const alerts = [];
  const documentListeners = new Map();
  const activeTimers = () => timers.filter((timer) => !timer.cleared);
  const visibleButton = () =>
    elementsById.get("codey-settings-button") || null;
  const document = {
    body: new FakeElement("body"),
    documentElement,
    visibilityState: "visible",
    addEventListener(type, handler) {
      const handlers = documentListeners.get(type) || [];
      handlers.push(handler);
      documentListeners.set(type, handlers);
    },
    createElement: (tagName) => {
      const element = new FakeElement(tagName);
      let id = element.id;
      Object.defineProperty(element, "id", {
        configurable: true,
        get: () => id,
        set: (value) => {
          id = String(value);
          if (id) elementsById.set(id, element);
        },
      });
      const originalSetAttribute = element.setAttribute.bind(element);
      element.setAttribute = (name, value) => {
        originalSetAttribute(name, value);
        if (name === "id") elementsById.set(String(value), element);
      };
      return element;
    },
    getElementById: (id) => {
      const element = id === "codey-settings-button"
        ? visibleButton()
        : elementsById.get(id);
      return element?.isConnected ? element : null;
    },
    querySelector: () => null,
    querySelectorAll: (selector) =>
      selector === "header" ? [visibleHeader] : [],
  };
  const window = {
    __codexSessionDeleteBridge: async (path, payload, options) => {
      if (path === "/internal/codey/session-tools/load") {
        window.__codeySessionToolsInjectLoaded = true;
        return { status: "ok" };
      }
      return bridge(path, payload, options);
    },
    addEventListener() {},
    alert(message) {
      alerts.push(String(message));
    },
    clearTimeout(id) {
      const timer = timers.find((entry) => entry.id === id);
      if (timer) timer.cleared = true;
    },
    dispatchEvent(event) {
      events.push(event);
      return true;
    },
    getComputedStyle: () => ({ display: "flex", visibility: "visible" }),
    innerWidth: 1200,
    localStorage: { getItem: () => null, key: () => null, length: 0, setItem() {} },
    requestIdleCallback(callback, options = {}) {
      const timer = {
        id: nextTimerId,
        callback,
        delay: options.timeout ?? 0,
        cleared: false,
        idle: true,
      };
      nextTimerId += 1;
      timers.push(timer);
      return timer.id;
    },
    setTimeout(callback, delay) {
      const timer = { id: nextTimerId, callback, delay, cleared: false };
      nextTimerId += 1;
      timers.push(timer);
      return timer.id;
    },
  };
  window.window = window;

  runRenderer({
    console,
    CustomEvent: class {
      constructor(type, init = {}) {
        this.type = type;
        this.detail = init.detail;
      }
    },
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      disconnect() {}
      observe() {}
    },
    URLSearchParams,
    window,
  });

  return {
    activeTimers,
    alerts,
    document,
    dispatchDocumentEvent(type) {
      for (const handler of documentListeners.get(type) || []) {
        handler({ type });
      }
    },
    elementsById,
    events,
    timers,
    window,
  };
};

test("hydrates the passive update badge from startup backend state", async () => {
  const bridgeCalls = [];
  const fixture = createStartupUpdateFixture(async (path, payload) => {
    bridgeCalls.push({ path, payload });
    if (path === "/backend/status") {
      return {
        status: "ok",
        availableUpdate: {
          currentVersion: "0.3.9",
          latestVersion: "0.4.0",
          updateAvailable: true,
          selectedAsset: { fileName: "Codey-0.4.0.zip" },
        },
      };
    }
    if (path === "/backend/health") return { status: "ok" };
    throw new Error(`unexpected bridge path: ${path}`);
  });

  await new Promise((resolve) => setImmediate(resolve));

  const button = fixture.document.getElementById("codey-settings-button");
  assert.ok(button);
  assert.equal(button.getAttribute("data-codey-update-available"), "true");
  assert.equal(button.getAttribute("aria-label"), "打开 Codey 配置，有可用更新");
  assert.equal(fixture.window.__codeyUpdateAvailability.latestVersion, "0.4.0");
  const updateEvents = fixture.events.filter(
    (event) => event.type === "codey-update-availability-changed",
  );
  assert.equal(updateEvents.length, 1);
  assert.equal(fixture.document.getElementById("codey-update-check-status"), null);
  assert.equal(fixture.document.getElementById("codey-update-dialog"), null);
  assert.equal(
    fixture.activeTimers().some((timer) => timer.delay === 30 * 60 * 1000),
    false,
  );
  assert.deepEqual(
    bridgeCalls.map(({ path }) => path),
    ["/backend/status", "/backend/health"],
  );
  assert.equal(
    fixture.activeTimers().some((timer) => timer.delay === 30_000),
    true,
  );

  let unchangedAttributeWrites = 0;
  const originalSetAttribute = button.setAttribute.bind(button);
  button.setAttribute = (...args) => {
    unchangedAttributeWrites += 1;
    originalSetAttribute(...args);
  };
  await fixture.window.__codeyRefreshRuntimeHealth();
  assert.equal(unchangedAttributeWrites, 0);
});

test("falls back to a passive periodic check when backend update state hangs", async () => {
  const fixture = createStartupUpdateFixture(
    async () => new Promise(() => {}),
  );

  const timeoutTimer = fixture.activeTimers().find(
    (timer) => timer.delay === 10_000,
  );
  assert.ok(timeoutTimer);
  timeoutTimer.cleared = true;
  timeoutTimer.callback();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(fixture.document.getElementById("codey-update-check-status"), null);
  assert.equal(fixture.document.getElementById("codey-update-dialog"), null);
  assert.equal(fixture.window.__codeyUpdateAvailability, null);
  assert.equal(
    fixture.activeTimers().some((timer) => timer.delay === 30 * 60 * 1000),
    true,
  );
});

test("marks the Codey icon unavailable after consecutive hung health checks and recovers", async () => {
  let healthMode = "hang";
  const fixture = createStartupUpdateFixture(async (path) => {
    if (path === "/backend/status") {
      return { status: "ok", availableUpdate: null };
    }
    if (path === "/backend/health") {
      return healthMode === "healthy"
        ? { status: "ok" }
        : new Promise(() => {});
    }
    throw new Error(`unexpected bridge path: ${path}`);
  });

  const fireLatestHealthTimeout = () => {
    const timer = fixture.activeTimers()
      .filter((candidate) => candidate.delay === 3_250)
      .at(-1);
    assert.ok(timer, "health timeout should be armed");
    timer.cleared = true;
    timer.callback();
  };

  fireLatestHealthTimeout();
  await new Promise((resolve) => setImmediate(resolve));

  const button = fixture.document.getElementById("codey-settings-button");
  assert.ok(button);
  assert.equal(button.getAttribute("data-codey-runtime-state"), "checking");

  const retryTimer = fixture.activeTimers().find(
    (candidate) => candidate.delay === 1_000,
  );
  assert.ok(retryTimer, "first health failure should retry after one second");
  retryTimer.cleared = true;
  retryTimer.callback();
  fireLatestHealthTimeout();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(button.getAttribute("data-codey-runtime-state"), "unavailable");
  assert.match(button.getAttribute("aria-label"), /Codey 进程异常或连接中断/);
  assert.match(button.title, /Codey 后端未响应/);
  button.dispatchEvent({
    type: "click",
    preventDefault() {},
    stopPropagation() {},
  });
  assert.deepEqual(fixture.alerts, [
    "Codey 进程异常或已退出，当前配置面板无法连接。请退出 Codex 后重新启动 Codey。",
  ]);

  healthMode = "healthy";
  await fixture.window.__codeyRefreshRuntimeHealth();

  assert.equal(button.getAttribute("data-codey-runtime-state"), "healthy");
  assert.equal(button.getAttribute("aria-label"), "打开 Codey 配置");
  assert.equal(button.title, "打开 Codey 配置");
  assert.equal(fixture.window.__codeyRuntimeHealth.consecutiveFailures, 0);
});

test("pauses Codey health checks while the page is hidden and resumes immediately", async () => {
  let healthCalls = 0;
  const fixture = createStartupUpdateFixture(async (path) => {
    if (path === "/backend/status") return { status: "ok", availableUpdate: null };
    if (path === "/backend/health") {
      healthCalls += 1;
      return { status: "ok" };
    }
    throw new Error(`unexpected bridge path: ${path}`);
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(healthCalls, 1);

  fixture.document.visibilityState = "hidden";
  fixture.dispatchDocumentEvent("visibilitychange");
  assert.equal(
    fixture.activeTimers().some((timer) => timer.delay === 30_000),
    false,
  );
  await fixture.window.__codeyRefreshRuntimeHealth();
  assert.equal(healthCalls, 1);

  fixture.document.visibilityState = "visible";
  fixture.dispatchDocumentEvent("visibilitychange");
  const immediateTimer = fixture.activeTimers().find((timer) => timer.delay === 0);
  assert.ok(immediateTimer);
  immediateTimer.cleared = true;
  immediateTimer.callback();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(healthCalls, 2);
  assert.equal(
    fixture.activeTimers().some((timer) => timer.delay === 30_000),
    true,
  );
});

test("ignores sidebar nav and main content until top chrome is available", () => {
  const sidebarNav = new FakeElement("nav", { right: 84, width: 84, height: 720 });
  const main = new FakeElement("main", { right: 1200, width: 1200, height: 640, top: 80 });
  const mainContent = new FakeElement("div", { right: 1080, width: 960, height: 640, top: 80 });
  const staleButton = new FakeElement("button", { right: 60, width: 28 });
  staleButton.id = "codey-settings-button";
  sidebarNav.appendChild(staleButton);
  main.appendChild(mainContent);

  let topNav = null;
  const placeholders = {
    "codey-core-injected-style": new FakeElement("style"),
    "codey-settings-button": staleButton,
  };
  const document = {
    body: new FakeElement("body"),
    documentElement: new FakeElement("html", { right: 1200, width: 1200, height: 800 }),
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: (id) => placeholders[id] || null,
    querySelector: (selector) => (selector === "main" ? main : null),
    querySelectorAll: (selector) => {
      if (selector === "header") return [];
      if (selector === "nav") return topNav ? [sidebarNav, topNav] : [sidebarNav];
      return [];
    },
  };
  const window = {
    addEventListener() {},
    alert() {},
    clearTimeout() {},
    getComputedStyle: (element) => ({
      display: element.visible ? "flex" : "none",
      visibility: element.visible ? "visible" : "hidden",
    }),
    innerWidth: 1200,
    setTimeout: () => 1,
  };
  window.window = window;

  runRenderer({
    console,
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      observe() {}
    },
    URLSearchParams,
    window,
  });

  assert.equal(staleButton.parentElement, null);
  assert.equal(sidebarNav.children.includes(staleButton), false);
  assert.equal(mainContent.children.length, 0);

  topNav = new FakeElement("nav", { right: 1200, width: 96, height: 46 });
  window.__codeyRendererScan();

  assert.equal(staleButton.parentElement, topNav);
  assert.deepEqual(topNav.children, [staleButton]);
});

test("repeated scans fast-path an already mounted button without layout reads", () => {
  const visibleHeader = new FakeElement("header", { right: 1200 });
  const rightRegion = new FakeElement("div", { right: 1200, width: 70 });
  const nativeButton = new FakeElement("button", { right: 1192, width: 28 });
  const codeyButton = new FakeElement("button", { right: 1120, width: 28 });
  codeyButton.id = "codey-settings-button";
  codeyButton.dataset.codeyHeaderActions = "true";
  codeyButton.isConnected = true;
  visibleHeader.appendChild(codeyButton);
  visibleHeader.appendChild(rightRegion);
  rightRegion.appendChild(nativeButton);

  const placeholders = {
    "codey-core-injected-style": new FakeElement("style"),
    "codey-settings-button": codeyButton,
  };
  let headerQueries = 0;
  const document = {
    body: new FakeElement("body"),
    documentElement: new FakeElement("html"),
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: (id) => placeholders[id] || null,
    querySelector: () => null,
    querySelectorAll: (selector) => {
      if (selector === "header" || selector === "nav") headerQueries += 1;
      return selector === "header" ? [visibleHeader] : [];
    },
  };
  const window = {
    addEventListener() {},
    alert() {},
    clearTimeout() {},
    getComputedStyle: () => ({ display: "flex", visibility: "visible" }),
    setTimeout: () => 1,
  };
  window.window = window;
  let observerCallback = null;

  runRenderer({
    console,
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      constructor(callback) {
        observerCallback = callback;
      }

      observe() {}
    },
    URLSearchParams,
    window,
  });

  headerQueries = 0;
  for (const element of [visibleHeader, rightRegion, nativeButton, codeyButton]) {
    element.rectReads = 0;
  }
  for (let scan = 0; scan < 10; scan += 1) {
    window.__codeyRendererScan();
  }
  assert.equal(headerQueries, 0);
  assert.equal(visibleHeader.rectReads, 0);
  assert.equal(rightRegion.rectReads, 0);
  assert.equal(nativeButton.rectReads, 0);
  assert.equal(codeyButton.rectReads, 0);
  assert.deepEqual(visibleHeader.children, [codeyButton, rightRegion]);

  const newRightRegion = new FakeElement("div", { right: 1200, width: 50 });
  const newRightButton = new FakeElement("button", { right: 1200, width: 28 });
  newRightRegion.appendChild(newRightButton);
  visibleHeader.appendChild(newRightRegion);
  observerCallback([{
    type: "childList",
    target: visibleHeader,
    addedNodes: [newRightRegion],
    removedNodes: [],
  }]);
  window.__codeyRendererScan();

  assert.ok(headerQueries > 0);
  assert.equal(codeyButton.__codeyHeaderAnchor, newRightRegion);
  assert.equal(codeyButton.dataset.codeyHeaderActions, "true");
  assert.deepEqual(visibleHeader.children, [rightRegion, codeyButton, newRightRegion]);
});
