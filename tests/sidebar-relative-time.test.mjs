import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {
  constructor(tagName = "div") {
    super(tagName);
    delete this.isConnected;
    this.className = "";
    this.title = "";
    this.attributeWrites = 0;
  }

  setAttribute(name, value) {
    this.attributeWrites += 1;
    super.setAttribute(name, value);
  }
}

function loadInjection({
  advanceTimeoutClock = false,
  assetModules = new Map(),
  bridgeHandler,
  entryScriptUrls = [],
  fetchHandler,
  now,
  rows = [],
  signalDispatcher,
} = {}) {
  const placeholder = new FakeElement();
  const intervalCallbacks = [];
  const canceledTimeouts = new Set();
  let nextTimeoutId = 1;
  let mutationCallback = null;
  let nowMs = Number.isFinite(now) ? now : Date.now();
  class FakeDate extends Date {
    constructor(...args) {
      super(...(args.length ? args : [nowMs]));
    }

    static now() {
      return nowMs;
    }
  }
  const document = {
    body: new FakeElement("body"),
    documentElement: new FakeElement("html"),
    scripts: entryScriptUrls.map((src) => ({ src })),
    visibilityState: "visible",
    addEventListener() {},
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: () => placeholder,
    querySelector: () => null,
    threadRowQueries: 0,
    querySelectorAll(selector) {
      if (selector !== "[data-app-action-sidebar-thread-row]") return [];
      this.threadRowQueries += 1;
      return rows;
    },
  };
  const window = {
    __codexSessionDeleteBridge: bridgeHandler,
    __codeyCodexSignalDispatcher: signalDispatcher,
    __codeyImportCodexAsset: async (url) => assetModules.get(url),
    addEventListener() {},
    clearTimeout(timeoutId) {
      canceledTimeouts.add(timeoutId);
    },
    dispatchEvent() {},
    localStorage: { length: 0, key: () => null, getItem: () => null, setItem() {} },
    requestIdleCallback(callback) {
      callback({ didTimeout: false, timeRemaining: () => 50 });
      return 1;
    },
    setInterval: (callback) => {
      intervalCallbacks.push(callback);
      return intervalCallbacks.length;
    },
    setTimeout: (callback, delayMs = 0) => {
      const timeoutId = nextTimeoutId;
      nextTimeoutId += 1;
      queueMicrotask(() => {
        if (canceledTimeouts.delete(timeoutId)) return;
        if (advanceTimeoutClock) nowMs += Math.max(0, Number(delayMs) || 0);
        callback();
      });
      return timeoutId;
    },
  };
  if (fetchHandler) window.fetch = fetchHandler;
  window.window = window;
  const context = {
    console,
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      constructor(callback) {
        mutationCallback = callback;
      }
      observe() {}
    },
    performance: { getEntriesByType: () => [] },
    URL,
    URLSearchParams,
    window,
  };
  if (Number.isFinite(now) || advanceTimeoutClock) context.Date = FakeDate;
  vm.runInNewContext(source, context);
  return {
    advanceTime: (milliseconds) => {
      nowMs += milliseconds;
    },
    document,
    notifyMutations: (mutations) => mutationCallback?.(mutations),
    runIntervals: () => intervalCallbacks.forEach((callback) => callback()),
    window,
  };
}

function timestampBridge(handler) {
  return async (path, payload) => {
    if (path !== "/session/timestamps") return { status: "ok" };
    return {
      status: "ok",
      timestamps: await handler(payload),
    };
  };
}

function sidebarThreadEntry({ running = false, sessionId = "" } = {}) {
  const list = new FakeElement();
  list.setAttribute("role", "list");
  const item = new FakeElement();
  item.setAttribute("role", "listitem");
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  if (sessionId) {
    row.setAttribute("data-app-action-sidebar-thread-id", sessionId);
    row.setAttribute("data-app-action-sidebar-thread-title", sessionId);
  }
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  const spinner = new FakeElement();
  spinner.className = "animate-spin rounded-full";
  if (running) nativeStatusRail.appendChild(spinner);
  content.appendChild(titleRegion);
  content.appendChild(nativeStatusRail);
  row.appendChild(content);
  item.appendChild(row);
  list.appendChild(item);
  return {
    content,
    item,
    list,
    nativeStatusRail,
    row,
    spinner,
    titleRegion,
  };
}

function sidebarProjectList({ projectId = "project-1", showAll = false } = {}) {
  const projectItem = new FakeElement();
  projectItem.setAttribute("role", "listitem");
  const projectRow = new FakeElement();
  projectRow.setAttribute("data-app-action-sidebar-project-row", "");
  projectRow.setAttribute("data-app-action-sidebar-project-id", projectId);
  const toggle = new FakeElement("button");
  toggle.setAttribute("aria-expanded", "true");
  projectRow.appendChild(toggle);
  const projectList = new FakeElement();
  projectList.setAttribute("data-app-action-sidebar-project-list-id", projectId);
  projectList.setAttribute("data-app-action-sidebar-project-show-all", String(showAll));
  const list = new FakeElement();
  list.setAttribute("role", "list");
  projectList.appendChild(list);
  projectItem.appendChild(projectRow);
  projectItem.appendChild(projectList);
  return { list, projectItem, projectList, projectRow, toggle };
}

test("formats compact relative times for the sidebar", () => {
  const { window } = loadInjection();
  const now = Date.UTC(2026, 6, 21, 12);
  const format = window.__codeyFormatRelativeThreadTime;

  assert.equal(format(now - 59_000, now), "刚刚");
  assert.equal(format(now - 3 * 60_000, now), "3 分");
  assert.equal(format(now - 3 * 60 * 60_000, now), "3 小时");
  assert.equal(format(now - 2 * 24 * 60 * 60_000, now), "2 天");
  assert.equal(format(now - 14 * 24 * 60 * 60_000, now), "2 周");
  assert.equal(format(now - 45 * 24 * 60 * 60_000, now), "1 月");
  assert.equal(format(now - 360 * 24 * 60 * 60_000, now), "12 月");
  assert.equal(format(now - 400 * 24 * 60 * 60_000, now), "1 年");
});

test("normalizes Codex timestamp payload variants to milliseconds", () => {
  const { window } = loadInjection();
  const timestampFrom = window.__codeyThreadTimestampMsFromPayload;

  assert.equal(timestampFrom({ recency_at_ms: 222_333, updated_at_ms: 123_456 }), 222_333);
  assert.equal(timestampFrom({ recency_at: 123, updated_at_ms: 456_789 }), 123_000);
  assert.equal(timestampFrom({ createdAtMs: 456_789 }), 456_789);
  assert.equal(timestampFrom({ updatedAt: 123, createdAt: 45 }), 123_000);
  assert.equal(
    timestampFrom({ id: "019f948c-dba4-73c0-83e3-804e6ad6a5be" }),
    1_784_903_687_076,
  );
  assert.equal(timestampFrom({ updated_at: 123 }), 123_000);
});

test("renders an accessible time element in the thread row content", () => {
  const { window } = loadInjection();
  const row = new FakeElement();
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  const nativeActionSpacer = new FakeElement();
  nativeActionSpacer.className = "shrink-0";
  content.appendChild(titleRegion);
  content.appendChild(nativeStatusRail);
  content.appendChild(nativeActionSpacer);
  row.appendChild(content);
  const timestamp = Date.now() - 2 * 24 * 60 * 60_000;

  window.__codeyUpdateThreadUpdatedAt(row, timestamp);

  const label = content.querySelector("[data-codey-thread-updated-at]");
  assert.ok(label);
  assert.equal(label.textContent, "2 天");
  assert.equal(label.getAttribute("data-codey-thread-updated-at-ms"), String(timestamp));
  assert.match(label.getAttribute("datetime"), /^\d{4}-\d{2}-\d{2}T/);
  assert.match(label.getAttribute("aria-label"), /^最后消息：2 天/);
  assert.match(label.title, /^最后消息：/);
  assert.deepEqual(
    content.children,
    [titleRegion, label, nativeStatusRail, nativeActionSpacer],
  );

  const attributeWrites = label.attributeWrites;
  window.__codeyUpdateThreadUpdatedAt(row, timestamp);
  assert.equal(label.attributeWrites, attributeWrites);

  window.__codeyUpdateThreadUpdatedAt(row, 0);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);
});

test("hides thread time while the native status rail is occupied", () => {
  const { window } = loadInjection();
  const row = new FakeElement();
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  const runningStatus = new FakeElement();
  runningStatus.className = "animate-spin rounded-full";
  const nativeActionSpacer = new FakeElement();
  nativeActionSpacer.className = "shrink-0";
  content.appendChild(titleRegion);
  nativeStatusRail.appendChild(runningStatus);
  content.appendChild(nativeStatusRail);
  content.appendChild(nativeActionSpacer);
  row.appendChild(content);
  const timestamp = Date.now() - 5 * 60_000;

  window.__codeyUpdateThreadUpdatedAt(row, timestamp);

  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);

  runningStatus.remove();
  window.__codeyUpdateThreadUpdatedAt(row, timestamp);

  const label = content.querySelector("[data-codey-thread-updated-at]");
  assert.ok(label);
  assert.equal(label.textContent, "5 分");
  assert.deepEqual(
    content.children,
    [titleRegion, label, nativeStatusRail, nativeActionSpacer],
  );
});

test("hides thread time from Codex React loading and unread status state", () => {
  const { window } = loadInjection();
  const row = new FakeElement();
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  content.appendChild(titleRegion);
  content.appendChild(nativeStatusRail);
  row.appendChild(content);
  const timestamp = Date.now() - 6 * 60_000;
  const statusFiber = {
    memoizedProps: { statusState: { type: "loading", unread: false } },
    return: null,
  };
  row.__reactFiber$test = { memoizedProps: {}, return: statusFiber };

  window.__codeyUpdateThreadUpdatedAt(row, timestamp);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);

  statusFiber.memoizedProps.statusState = { type: "streaming", unread: false };
  window.__codeyUpdateThreadUpdatedAt(row, timestamp);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);

  statusFiber.memoizedProps.statusState = { type: undefined, unread: true };
  window.__codeyUpdateThreadUpdatedAt(row, timestamp);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);

  statusFiber.memoizedProps.statusState = { type: undefined, unread: false };
  window.__codeyUpdateThreadUpdatedAt(row, timestamp);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]")?.textContent, "6 分");
});

test("visually prioritizes running threads without changing official DOM order", async () => {
  const olderIdle = sidebarThreadEntry({ sessionId: "thread-older" });
  const firstRunning = sidebarThreadEntry({
    running: true,
    sessionId: "thread-running-1",
  });
  const unread = sidebarThreadEntry({ sessionId: "thread-unread" });
  const secondRunning = sidebarThreadEntry({
    running: true,
    sessionId: "thread-running-2",
  });
  unread.row.__reactFiber$test = {
    memoizedProps: {},
    return: {
      memoizedProps: { statusState: { type: undefined, unread: true } },
      return: null,
    },
  };
  const list = olderIdle.list;
  list.appendChild(firstRunning.item);
  list.appendChild(unread.item);
  list.appendChild(secondRunning.item);
  loadInjection({
    rows: [olderIdle.row, firstRunning.row, unread.row, secondRunning.row],
    signalDispatcher: async (signal, request) => {
      assert.equal(signal, "send-cli-request-for-host");
      assert.equal(request.method, "thread/list");
      return {
        data: [
          { id: "thread-older", recencyAt: 60 },
          { id: "thread-running-1", recencyAt: 120 },
          { id: "thread-unread", recencyAt: 180 },
          { id: "thread-running-2", recencyAt: 240 },
        ],
        nextCursor: null,
      };
    },
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(olderIdle.item.getAttribute("data-codey-thread-running"), null);
  assert.equal(firstRunning.item.getAttribute("data-codey-thread-running"), "true");
  assert.equal(unread.item.getAttribute("data-codey-thread-running"), null);
  assert.equal(secondRunning.item.getAttribute("data-codey-thread-running"), "true");
  assert.deepEqual(
    list.children,
    [olderIdle.item, firstRunning.item, unread.item, secondRunning.item],
  );
});

test("keeps three running threads prioritized through a transient status gap", async () => {
  const firstRunning = sidebarThreadEntry({
    running: true,
    sessionId: "client-new-thread:running-1",
  });
  const secondRunning = sidebarThreadEntry({
    running: true,
    sessionId: "client-new-thread:running-2",
  });
  const thirdRunning = sidebarThreadEntry({
    running: true,
    sessionId: "client-new-thread:running-3",
  });
  const list = firstRunning.list;
  list.appendChild(secondRunning.item);
  list.appendChild(thirdRunning.item);
  const { window } = loadInjection({
    rows: [firstRunning.row, secondRunning.row, thirdRunning.row],
  });

  assert.deepEqual(
    [firstRunning.item, secondRunning.item, thirdRunning.item].map((item) => (
      item.getAttribute("data-codey-thread-running")
    )),
    ["true", "true", "true"],
  );

  secondRunning.spinner.remove();
  window.__codeyInstallThreadUpdatedTimes(secondRunning.row);

  assert.equal(secondRunning.item.getAttribute("data-codey-thread-running"), "true");
  assert.deepEqual(list.children, [firstRunning.item, secondRunning.item, thirdRunning.item]);

  secondRunning.nativeStatusRail.appendChild(secondRunning.spinner);
  window.__codeyInstallThreadUpdatedTimes(secondRunning.row);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(secondRunning.item.getAttribute("data-codey-thread-running"), "true");
});

test("carries running priority across a transient React row replacement", async () => {
  const sessionId = "client-new-thread:replaced-running";
  const running = sidebarThreadEntry({ running: true, sessionId });
  const rows = [running.row];
  const { window } = loadInjection({ rows });
  const replacement = sidebarThreadEntry({ sessionId });
  const list = running.list;
  running.item.remove();
  list.appendChild(replacement.item);
  rows[0] = replacement.row;

  window.__codeyInstallThreadUpdatedTimes(replacement.row);

  assert.equal(replacement.item.getAttribute("data-codey-thread-running"), "true");

  replacement.nativeStatusRail.appendChild(replacement.spinner);
  window.__codeyInstallThreadUpdatedTimes(replacement.row);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(replacement.item.getAttribute("data-codey-thread-running"), "true");
});

test("keeps running priority while a placeholder thread receives its canonical id", async () => {
  const running = sidebarThreadEntry({
    running: true,
    sessionId: "client-new-thread:pending-id",
  });
  const { window } = loadInjection({
    rows: [running.row],
    signalDispatcher: async () => ({
      data: [{ id: "thread-canonical", recencyAt: Date.now() / 1_000 }],
      nextCursor: null,
    }),
  });

  running.spinner.remove();
  running.row.setAttribute("data-app-action-sidebar-thread-id", "thread-canonical");
  window.__codeyInstallThreadUpdatedTimes(running.row);

  assert.equal(running.item.getAttribute("data-codey-thread-running"), "true");

  running.nativeStatusRail.appendChild(running.spinner);
  window.__codeyInstallThreadUpdatedTimes(running.row);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(running.item.getAttribute("data-codey-thread-running"), "true");
});

test("reveals a paginated running thread when its project is expanded", () => {
  const sessionId = "client-new-thread:hidden-running";
  const project = sidebarProjectList({ projectId: "project-running" });
  const running = sidebarThreadEntry({ running: true, sessionId });
  project.list.appendChild(running.item);
  project.projectList.__reactFiber$test = {
    memoizedProps: {},
    return: {
      memoizedProps: {
        threadKeys: [
          ...Array.from({ length: 5 }, (_, index) => `local:client-new-thread:idle-${index}`),
          `local:${sessionId}`,
        ],
      },
      return: null,
    },
  };
  const { window } = loadInjection({ rows: [running.row] });
  const replacement = sidebarThreadEntry({ running: true, sessionId });
  running.item.remove();
  Array.from({ length: 5 }, (_, index) => sidebarThreadEntry({
    sessionId: `client-new-thread:idle-${index}`,
  })).forEach((entry) => project.list.appendChild(entry.item));
  const footer = new FakeElement();
  footer.setAttribute("role", "listitem");
  const showAllButton = new FakeElement("button");
  showAllButton.textContent = "展开显示";
  let clicks = 0;
  showAllButton.click = () => {
    clicks += 1;
    project.projectList.setAttribute("data-app-action-sidebar-project-show-all", "true");
    footer.remove();
    project.list.appendChild(replacement.item);
  };
  footer.appendChild(showAllButton);
  project.list.appendChild(footer);

  project.toggle.setAttribute("aria-expanded", "false");
  window.__codeyRecoverHiddenRunningThreads(project.projectRow);
  assert.equal(clicks, 0);

  project.toggle.setAttribute("aria-expanded", "true");
  window.__codeyRecoverHiddenRunningThreads(project.projectRow);

  assert.equal(clicks, 1);
  assert.equal(
    project.projectList.getAttribute("data-app-action-sidebar-project-show-all"),
    "true",
  );
  window.__codeyInstallThreadUpdatedTimes(project.projectList);
  assert.equal(replacement.item.getAttribute("data-codey-thread-running"), "true");
});

test("does not expand a project that has no hidden running thread", () => {
  const runningProject = sidebarProjectList({ projectId: "project-running" });
  const running = sidebarThreadEntry({
    running: true,
    sessionId: "client-new-thread:running-elsewhere",
  });
  runningProject.list.appendChild(running.item);
  const { window } = loadInjection({ rows: [running.row] });
  const idleProject = sidebarProjectList({ projectId: "project-idle" });
  idleProject.projectList.__reactFiber$test = {
    memoizedProps: {},
    return: {
      memoizedProps: { threadKeys: ["local:client-new-thread:idle-hidden"] },
      return: null,
    },
  };
  const footer = new FakeElement();
  footer.setAttribute("role", "listitem");
  const showAllButton = new FakeElement("button");
  showAllButton.textContent = "展开显示";
  let clicks = 0;
  showAllButton.click = () => {
    clicks += 1;
  };
  footer.appendChild(showAllButton);
  idleProject.list.appendChild(footer);

  window.__codeyRecoverHiddenRunningThreads(idleProject.projectList);

  assert.equal(clicks, 0);
});

test("refreshes the official timestamp when a running thread completes", async () => {
  const running = sidebarThreadEntry({
    running: true,
    sessionId: "thread-running",
  });
  let dispatcherCalls = 0;
  const firstTimestamp = Date.now() - 60 * 60_000;
  const completedTimestamp = Date.now() - 2 * 60_000;
  const { window } = loadInjection({
    advanceTimeoutClock: true,
    rows: [running.row],
    bridgeHandler: timestampBridge(async () => {
      dispatcherCalls += 1;
      return {
        "thread-running": dispatcherCalls === 1 ? firstTimestamp : completedTimestamp,
      };
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(dispatcherCalls, 1);
  assert.equal(running.content.querySelector("[data-codey-thread-updated-at]"), null);

  running.spinner.remove();
  window.__codeyInstallThreadUpdatedTimes(running.row);

  assert.equal(dispatcherCalls, 1);
  assert.equal(running.item.getAttribute("data-codey-thread-running"), "true");

  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(dispatcherCalls, 2);
  assert.equal(
    running.content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "2 分",
  );
  assert.equal(running.item.getAttribute("data-codey-thread-running"), null);
});

test("coalesces sidebar mutations before recomputing React status", async () => {
  const entry = sidebarThreadEntry({ sessionId: "thread-1" });
  let fiberReads = 0;
  let dispatcherCalls = 0;
  Object.defineProperty(entry.row, "__reactFiber$test", {
    configurable: true,
    enumerable: true,
    get() {
      fiberReads += 1;
      return { memoizedProps: {}, return: null };
    },
  });
  const { notifyMutations } = loadInjection({
    rows: [entry.row],
    bridgeHandler: timestampBridge(async () => {
      dispatcherCalls += 1;
      return { "thread-1": 60_000 };
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(dispatcherCalls, 1);
  fiberReads = 0;

  entry.nativeStatusRail.appendChild(entry.spinner);
  notifyMutations(Array.from({ length: 100 }, () => ({
    type: "childList",
    target: entry.nativeStatusRail,
    addedNodes: [entry.spinner],
    removedNodes: [],
  })));

  assert.equal(fiberReads, 0);

  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(dispatcherCalls, 1);
  assert.equal(entry.item.getAttribute("data-codey-thread-running"), "true");
  assert.ok(
    fiberReads <= 2,
    `100 mutations should converge in one scan, observed ${fiberReads} React fiber reads`,
  );
});

test("ignores cosmetic descendant class churn but tracks status class changes", async () => {
  const entry = sidebarThreadEntry({ sessionId: "thread-class-change" });
  let fiberReads = 0;
  Object.defineProperty(entry.row, "__reactFiber$test", {
    configurable: true,
    enumerable: true,
    get() {
      fiberReads += 1;
      return { memoizedProps: {}, return: null };
    },
  });
  const { notifyMutations } = loadInjection({
    rows: [entry.row],
    signalDispatcher: async () => ({
      data: [{ id: "thread-class-change", updatedAt: 60 }],
      nextCursor: null,
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));
  fiberReads = 0;

  entry.titleRegion.className = "flex min-w-0 flex-1 items-center gap-2 selected";
  notifyMutations([{
    type: "attributes",
    target: entry.titleRegion,
    attributeName: "class",
    oldValue: "flex min-w-0 flex-1 items-center gap-2",
  }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(fiberReads, 0, "cosmetic descendant class changes should not rescan the row");

  entry.nativeStatusRail.appendChild(entry.spinner);
  notifyMutations([{
    type: "attributes",
    target: entry.spinner,
    attributeName: "class",
    oldValue: "rounded-full",
  }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(fiberReads > 0, "status class changes must still refresh the row");
  assert.equal(entry.item.getAttribute("data-codey-thread-running"), "true");

  const readsAfterStatusAppeared = fiberReads;
  const persistentSpinner = new FakeElement();
  persistentSpinner.className = "animate-pulse";
  entry.nativeStatusRail.appendChild(persistentSpinner);
  entry.spinner.className = "rounded-full";
  notifyMutations([{
    type: "attributes",
    target: entry.spinner,
    attributeName: "class",
    oldValue: "animate-spin rounded-full",
  }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(
    fiberReads > readsAfterStatusAppeared,
    "removing a status class must refresh the row through MutationRecord.oldValue",
  );
  assert.equal(entry.item.getAttribute("data-codey-thread-running"), "true");
});

test("keeps an existing thread time when a native completion marker appears", async () => {
  const timestamp = Date.now() - 2 * 60_000;
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "local:thread-1");
  row.setAttribute("data-app-action-sidebar-thread-title", "增加文本清洗 key 配置");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  const nativeActionSpacer = new FakeElement();
  nativeActionSpacer.className = "shrink-0";
  content.appendChild(titleRegion);
  content.appendChild(nativeStatusRail);
  content.appendChild(nativeActionSpacer);
  row.appendChild(content);

  const { window } = loadInjection({
    advanceTimeoutClock: true,
    rows: [row],
    bridgeHandler: timestampBridge(async () => ({ "thread-1": timestamp })),
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(content.querySelector("[data-codey-thread-updated-at]")?.textContent, "2 分");

  const completedStatus = new FakeElement();
  completedStatus.className = "rounded-full bg-blue-500";
  completedStatus.setAttribute("aria-label", "Completed");
  nativeStatusRail.appendChild(completedStatus);
  window.__codeyInstallThreadUpdatedTimes(row);

  assert.equal(content.querySelector("[data-codey-thread-updated-at]")?.textContent, "2 分");

  const runningStatus = new FakeElement();
  runningStatus.className = "animate-spin rounded-full";
  nativeStatusRail.appendChild(runningStatus);
  window.__codeyInstallThreadUpdatedTimes(row);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);

  runningStatus.remove();
  window.__codeyInstallThreadUpdatedTimes(row);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]")?.textContent, "2 分");
});

test("does not treat trailing action icons as native thread status", () => {
  const { window } = loadInjection();
  const row = new FakeElement();
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  const nativeActionSpacer = new FakeElement();
  nativeActionSpacer.className = "shrink-0";
  const actionButton = new FakeElement("button");
  const actionIcon = new FakeElement("svg");
  actionButton.appendChild(actionIcon);
  nativeActionSpacer.appendChild(actionButton);
  content.appendChild(titleRegion);
  content.appendChild(nativeStatusRail);
  content.appendChild(nativeActionSpacer);
  row.appendChild(content);
  const timestamp = Date.now() - 9 * 60_000;

  window.__codeyUpdateThreadUpdatedAt(row, timestamp);

  assert.equal(content.querySelector("[data-codey-thread-updated-at]")?.textContent, "9 分");
});

test("moves a previously appended time before the native trailing rail", () => {
  const { window } = loadInjection();
  const row = new FakeElement();
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center text-sm leading-4";
  const titleRegion = new FakeElement();
  titleRegion.className = "flex min-w-0 flex-1 items-center gap-2";
  const nativeStatusRail = new FakeElement();
  nativeStatusRail.className = "ml-[3px] flex items-center justify-end gap-1";
  const nativeActionSpacer = new FakeElement();
  nativeActionSpacer.className = "shrink-0";
  const misplacedLabel = new FakeElement("time");
  misplacedLabel.setAttribute("data-codey-thread-updated-at", "true");
  const duplicateLabel = new FakeElement("time");
  duplicateLabel.setAttribute("data-codey-thread-updated-at", "true");
  content.appendChild(titleRegion);
  content.appendChild(nativeStatusRail);
  content.appendChild(nativeActionSpacer);
  content.appendChild(misplacedLabel);
  content.appendChild(duplicateLabel);
  row.appendChild(content);

  window.__codeyUpdateThreadUpdatedAt(row, Date.now() - 12 * 60_000);

  assert.deepEqual(
    content.children,
    [titleRegion, misplacedLabel, nativeStatusRail, nativeActionSpacer],
  );
  assert.equal(
    content.querySelectorAll("[data-codey-thread-updated-at]").length,
    1,
  );
  assert.equal(duplicateLabel.parentElement, null);
});

test("loads visible thread timestamps through the Codey bridge", async () => {
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "local:thread-1");
  row.setAttribute("data-app-action-sidebar-thread-title", "发布计划");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  const calls = [];
  const timestamp = Date.now() - 3 * 60 * 60_000;

  const { document } = loadInjection({
    rows: [row],
    bridgeHandler: timestampBridge(async (payload) => {
      calls.push(payload);
      return { "thread-1": timestamp };
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(calls.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0])), {
    sessionIds: ["thread-1"],
  });
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "3 小时",
  );
  assert.equal(document.threadRowQueries, 1);
});

test("reads a remote task timestamp from the official React row without a bridge request", async () => {
  const now = Date.UTC(2026, 7, 10, 12);
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-host-id", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "remote:task-1");
  row.setAttribute("data-app-action-sidebar-thread-kind", "remote");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  row.__reactFiber$test = {
    memoizedProps: {
      task: {
        created_at: (now - 60 * 60_000) / 1_000,
        id: "task-1",
        updated_at: (now - 6 * 60_000) / 1_000,
      },
    },
    pendingProps: null,
    return: null,
  };
  let dispatcherCalls = 0;

  loadInjection({
    now,
    rows: [row],
    bridgeHandler: async () => {
      dispatcherCalls += 1;
      throw new Error("remote tasks must not use the local timestamp bridge");
    },
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(dispatcherCalls, 0);
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "6 分",
  );
});

test("refreshes bridge metadata on the visible one-minute tick", async () => {
  const now = Date.UTC(2026, 7, 10, 12);
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "thread-refresh");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  let timestamp = now - 10 * 60_000;
  let listCalls = 0;
  const fixture = loadInjection({
    now,
    rows: [row],
    bridgeHandler: timestampBridge(async () => {
      listCalls += 1;
      return { "thread-refresh": timestamp };
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(listCalls, 1);
  const fullScanQueries = fixture.document.threadRowQueries;

  fixture.advanceTime(60_000);
  timestamp = now + 60_000 - 2 * 60_000;
  fixture.runIntervals();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(listCalls, 2);
  assert.equal(
    fixture.document.threadRowQueries,
    fullScanQueries,
    "the minute tick should revisit tracked rows without scanning the document",
  );
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "2 分",
  );
});

test("tracks virtualized sidebar rows discovered after the initial scan", async () => {
  const now = Date.UTC(2026, 7, 10, 12);
  const first = sidebarThreadEntry({ sessionId: "thread-first" });
  const second = sidebarThreadEntry({ sessionId: "thread-second" });
  const rows = [first.row];
  const timestamps = new Map([
    ["thread-first", now - 5 * 60_000],
    ["thread-second", now - 8 * 60_000],
  ]);
  let listCalls = 0;
  const fixture = loadInjection({
    now,
    rows,
    bridgeHandler: timestampBridge(async ({ sessionIds }) => {
      listCalls += 1;
      return Object.fromEntries(sessionIds.map((id) => [id, timestamps.get(id)]));
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(listCalls, 1);
  const fullScanQueries = fixture.document.threadRowQueries;

  rows.push(second.row);
  fixture.notifyMutations([{
    type: "childList",
    target: second.list,
    addedNodes: [second.row],
    removedNodes: [],
  }]);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(listCalls, 2);
  assert.equal(
    second.content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "8 分",
  );
  assert.equal(fixture.document.threadRowQueries, fullScanQueries);

  fixture.advanceTime(60_000);
  timestamps.set("thread-second", now - 60_000);
  fixture.runIntervals();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(listCalls, 3);
  assert.equal(
    second.content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "2 分",
  );
  assert.equal(fixture.document.threadRowQueries, fullScanQueries);
});

for (const { name, visible, batches, minutes } of [
  {
    name: "loads forty visible timestamps in one bridge batch",
    visible: 40,
    batches: [40],
    minutes: 9,
  },
  {
    name: "continues timestamp work after the first 200 visible refs",
    visible: 201,
    batches: [200, 1],
    minutes: 11,
  },
]) {
  test(name, async () => {
    const now = Date.UTC(2026, 7, 10, 12);
    const entries = Array.from({ length: visible }, (_, index) => {
      const row = new FakeElement();
      row.setAttribute("data-app-action-sidebar-thread-row", "");
      row.setAttribute("data-app-action-sidebar-thread-id", `thread-${index}`);
      const content = new FakeElement();
      content.className = "flex h-full w-full items-center";
      row.appendChild(content);
      return { content, row };
    });
    const requestSizes = [];

    loadInjection({
      now,
      rows: entries.map(({ row }) => row),
      bridgeHandler: timestampBridge(async ({ sessionIds }) => {
        requestSizes.push(sessionIds.length);
        return Object.fromEntries(sessionIds.map((sessionId) => (
          [sessionId, now - minutes * 60_000]
        )));
      }),
    });
    await new Promise((resolve) => setImmediate(resolve));

    assert.deepEqual(requestSizes, batches);
    entries.forEach(({ content }) => {
      assert.equal(
        content.querySelector("[data-codey-thread-updated-at]")?.textContent,
        `${minutes} 分`,
      );
    });
  });
}

test("does not tight-loop retry a failed timestamp bridge request", async () => {
  const now = Date.UTC(2026, 7, 10, 12);
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "thread-read-retry");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  let bridgeCalls = 0;
  let dispatcherCalls = 0;

  const fixture = loadInjection({
    now,
    rows: [row],
    bridgeHandler: timestampBridge(async () => {
      bridgeCalls += 1;
      if (bridgeCalls === 1) throw new Error("temporary bridge failure");
      return { "thread-read-retry": now - 13 * 60_000 };
    }),
    signalDispatcher: async () => {
      dispatcherCalls += 1;
      throw new Error("timestamp reads must not discover Codex internals");
    },
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(bridgeCalls, 1);
  assert.equal(dispatcherCalls, 0);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);

  fixture.advanceTime(60_000);
  fixture.runIntervals();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(bridgeCalls, 2);
  assert.equal(dispatcherCalls, 0);
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "14 分",
  );
});

test("keeps the cached time when a forced bridge refresh fails", async () => {
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "thread-retry");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  const timestamp = Date.now() - 7 * 60_000;
  let calls = 0;

  const { window } = loadInjection({
    rows: [row],
    bridgeHandler: timestampBridge(async () => {
      calls += 1;
      if (calls > 1) throw new Error("temporary failure");
      return { "thread-retry": timestamp };
    }),
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(calls, 1);
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "7 分",
  );

  window.__codeyInstallThreadUpdatedTimes(row, true);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(calls, 2);
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "7 分",
  );
});

test("clears a cached timestamp when bridge metadata no longer has one", async () => {
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "thread-cleared");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  const timestamp = Date.now() - 8 * 60_000;
  let includeTimestamp = true;

  const { window } = loadInjection({
    rows: [row],
    bridgeHandler: timestampBridge(async () => (
      includeTimestamp ? { "thread-cleared": timestamp } : {}
    )),
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "8 分",
  );

  includeTimestamp = false;
  window.__codeyInstallThreadUpdatedTimes(row, true);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);
});

test("accepts only a unique direct app-server request wrapper", () => {
  const { window } = loadInjection();
  const direct = function request(action, payload, options) {
    return options == null
      ? signalClient.sendRequest(action, payload)
      : signalClient.sendRequest(action, payload, options);
  };
  const instanceMethod = function request(action, payload, options) {
    return this.requestClient.sendRequest(action, payload, options);
  };
  const secondDirect = function request(action, payload) {
    return otherSignalClient.sendRequest(action, payload);
  };

  assert.equal(
    window.__codeySignalDispatcherFromModule({ arbitrary: direct }, false),
    direct,
  );
  assert.equal(
    window.__codeySignalDispatcherFromModule({ O: instanceMethod }, true),
    null,
  );
  assert.equal(
    window.__codeySignalDispatcherFromModule({
      arbitrary: direct,
      another: secondDirect,
    }, false),
    null,
  );
  assert.equal(
    window.__codeySignalDispatcherFromModule({
      O: direct,
      another: secondDirect,
    }, false),
    null,
  );
});

test("discovers the current app-initial asset and resolves AppServerManager from React scope", async () => {
  const entryUrl = "app://-/assets/index-BZNttYfb.js";
  const appInitialUrl = "app://-/assets/app-initial-BCLYDefw.js";
  const appServerRequests = [];
  const manager = {
    discardConversationFromCache() {},
    handleThreadDeletion() {},
    refreshRecentConversations() {},
    resumeConversation() {},
    sendRequest(...args) {
      appServerRequests.push(args);
      return { rateLimits: { limitId: "codex" } };
    },
  };
  const AppServerManagerRpc = Symbol("AppServerManagerRpc");
  const managerRpc = { forHost: (hostId) => (hostId === "local" ? manager : null) };
  const scope = {
    get: () => managerRpc,
    query: {},
    set() {},
    watch() {},
    when() {},
  };
  const row = new FakeElement();
  row.__reactFiber$codeyTest = {
    memoizedState: { current: scope },
    return: null,
  };
  const resolver = function appServerManagerForHost(runtimeScope, hostId) {
    const rpc = runtimeScope.get(AppServerManagerRpc);
    if (rpc == null) throw new Error("AppServerManager RPC is not connected");
    return rpc.forHost(hostId);
  };
  const { window } = loadInjection({
    assetModules: new Map([[appInitialUrl, { arbitraryExport: resolver }]]),
    entryScriptUrls: [entryUrl],
    fetchHandler: async (url) => ({
      ok: url === entryUrl,
      text: async () => 'import "./app-initial-BCLYDefw.js";',
    }),
    rows: [row],
  });

  const controller = await window.__codeyLoadCodexSessionController();

  assert.equal(controller.kind, "manager");
  assert.equal(controller.manager, manager);
  assert.deepEqual(
    await window.__codeyReadAccountRateLimits(),
    { rateLimits: { limitId: "codex" } },
  );
  assert.deepEqual(appServerRequests, [["account/rateLimits/read"]]);
});

test("accepts only a unique semantic AppServerManager resolver", () => {
  const { window } = loadInjection();
  const direct = function appServerManagerForHost(scope, hostId) {
    const rpc = scope.get(AppServerManagerRpc);
    if (rpc == null) throw new Error("AppServerManager RPC is not connected");
    return rpc.forHost(hostId);
  };
  const second = function anotherAppServerManagerForHost(scope, hostId) {
    const rpc = scope.get(AnotherAppServerManagerRpc);
    if (rpc == null) throw new Error("AppServerManager RPC is not connected");
    return rpc.forHost(hostId);
  };

  assert.equal(
    window.__codeyAppServerManagerResolverFromModule({ arbitrary: direct }),
    direct,
  );
  assert.equal(
    window.__codeyAppServerManagerResolverFromModule({ direct, second }),
    null,
  );
});

test("timestamp metadata uses only the bounded bridge route", () => {
  assert.match(source, /threadTimestampBridgePath = "\/session\/timestamps"/);
  assert.match(source, /callBridge\(threadTimestampBridgePath, \{ sessionIds \}\)/);
  assert.doesNotMatch(source, /method: "thread\/(?:list|read)"/);
  assert.doesNotMatch(source, /fetch\(url\)/);
});
