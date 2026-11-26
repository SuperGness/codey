import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeMouseEvent {
  constructor(type, init = {}) {
    this.type = type;
    this.bubbles = Boolean(init.bubbles);
    this.cancelable = Boolean(init.cancelable);
    this.composed = Boolean(init.composed);
  }
}

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", attributes = {}) {
    super(tagName, { attributes });
    this.innerHTML = "";
    this.isContentEditable = attributes.contenteditable === "true";
  }

  setAttribute(name, value) {
    super.setAttribute(name, value);
    if (name === "contenteditable") {
      this.isContentEditable = String(value) === "true";
    }
  }

  async click() {
    const event = {
      composedPath: () => [this],
      preventDefault() {},
      stopImmediatePropagation() {},
      stopPropagation() {},
    };
    await Promise.all((this.listeners.get("click") || []).map((listener) => listener(event)));
  }

  getBoundingClientRect() {
    if (this.hasAttribute("data-app-action-sidebar-project-row")) {
      return { bottom: 32, height: 32, left: 0, right: 248, top: 0, width: 248 };
    }
    if (this.getAttribute("aria-label") === "项目操作") {
      return { bottom: 28, height: 24, left: 220, right: 244, top: 4, width: 24 };
    }
    return { bottom: 120, height: 24, left: 220, right: 244, top: 96, width: 24 };
  }

  getClientRects() {
    return [this.getBoundingClientRect()];
  }
}

// The sidebar row menu is a React component. Its props carry the official item
// builders, so the injected icon reaches them through the React fiber that
// React attaches to the row element.
function attachRowFiber(row, { menuItems, outerMenuItems }) {
  let fiber = outerMenuItems
    ? { memoizedProps: { getItems: () => outerMenuItems }, return: null }
    : { memoizedProps: { onContextMenu: () => {} }, return: null };
  if (menuItems) {
    fiber = { memoizedProps: { getItems: () => menuItems }, return: fiber };
  }
  row.__reactFiber$codeyTest = { memoizedProps: { className: "row" }, return: fiber };
}

function loadInjection({
  bridge,
  menuItems,
  outerMenuItems,
  mouseEvent = FakeMouseEvent,
  directArchiveButton = true,
  withPinButton = false,
} = {}) {
  const body = new FakeElement("body");
  const documentElement = new FakeElement("html");
  const thread = new FakeElement("div", {
    "data-app-action-sidebar-thread-active": "false",
    "data-app-action-sidebar-thread-id": "local:thread-1",
    "data-app-action-sidebar-thread-title": "待删除会话",
  });
  const actionBar = new FakeElement("div", { class: "flex" });
  const hoverCard = new FakeElement("div", { class: "contents" });
  const pinButton = new FakeElement("button", { "aria-label": "置顶聊天" });
  const archiveTooltip = new FakeElement("span");
  const archiveButton = new FakeElement("button", {
    "aria-label": "归档任务",
    class: "native-thread-action",
  });
  const project = new FakeElement("div", {
    "data-app-action-sidebar-project-id": "/Users/test/workspace",
    "data-app-action-sidebar-project-row": "",
  });
  const projectActionButton = new FakeElement("button", {
    "aria-label": "项目操作",
    class: "native-project-action",
  });
  const tasksSection = new FakeElement("section", {
    "data-app-action-sidebar-section": "",
    "data-app-action-sidebar-section-heading": "Tasks",
  });
  const tasksTitleRow = new FakeElement("div");
  const tasksTitleLabel = new FakeElement("div");
  const tasksTitleLabelInner = new FakeElement("div");
  const tasksToggle = new FakeElement("button", {
    "data-app-action-sidebar-section-toggle": "",
  });
  const tasksActionBar = new FakeElement("div");
  const tasksOptionsButton = new FakeElement("button", {
    "aria-label": "任务侧边栏选项",
    class: "native-tasks-header-action",
  });
  const newTaskButton = new FakeElement("button", {
    "aria-label": "新建任务",
    class: "native-tasks-header-action",
  });
  body.appendChild(thread);
  body.appendChild(project);
  body.appendChild(tasksSection);
  if (directArchiveButton) {
    thread.appendChild(hoverCard);
    hoverCard.appendChild(actionBar);
    if (withPinButton) actionBar.appendChild(pinButton);
    actionBar.appendChild(archiveButton);
  } else {
    thread.appendChild(actionBar);
    actionBar.appendChild(archiveTooltip);
    archiveTooltip.appendChild(archiveButton);
  }
  project.appendChild(projectActionButton);
  tasksSection.appendChild(tasksTitleRow);
  tasksTitleRow.append(tasksTitleLabel, tasksActionBar);
  tasksTitleLabel.appendChild(tasksTitleLabelInner);
  tasksTitleLabelInner.appendChild(tasksToggle);
  tasksActionBar.append(tasksOptionsButton, newTaskButton);

  if (menuItems || outerMenuItems) attachRowFiber(thread, { menuItems, outerMenuItems });
  const dispatched = [];
  thread.dispatchEvent = (event) => {
    dispatched.push(event);
    return true;
  };

  const placeholder = new FakeElement();
  const bridgeCalls = [];
  const timers = new Map();
  let nextTimerId = 0;
  let nowMs = 1_700_000_000_000;
  const location = { pathname: "/", reload() {}, search: "" };
  const documentListeners = new Map();
  const windowListeners = new Map();
  const addTrackedListener = (map, type, listener, options) => {
    const entries = map.get(type) || [];
    entries.push({
      capture: options === true || Boolean(options?.capture),
      listener,
    });
    map.set(type, entries);
  };
  const removeTrackedListener = (map, type, listener) => {
    const entries = map.get(type);
    if (!entries) return;
    if (!listener) {
      map.delete(type);
      return;
    }
    map.set(type, entries.filter((entry) => entry.listener !== listener));
  };
  const document = {
    activeElement: null,
    body,
    documentElement,
    addEventListener(type, listener, options) {
      addTrackedListener(documentListeners, type, listener, options);
    },
    createElement(tagName) {
      return new FakeElement(tagName);
    },
    getElementById(id) {
      if (["codey-injected-style", "codey-settings-button", "codey-message-toolbar"].includes(id)) {
        return placeholder;
      }
      return [...body.querySelectorAll("[id]"), ...documentElement.querySelectorAll("[id]")]
        .find((element) => element.id === id || element.getAttribute("id") === id) || null;
    },
    querySelector(selector) {
      return this.querySelectorAll(selector)[0] || null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-app-action-sidebar-thread-id][data-app-action-sidebar-thread-title]") {
        return body
          .querySelectorAll("[data-app-action-sidebar-thread-id]")
          .filter((element) => element.hasAttribute("data-app-action-sidebar-thread-title"));
      }
      if (selector === '[data-app-action-sidebar-thread-active="true"]') {
        return body.querySelectorAll("[data-app-action-sidebar-thread-id]")
          .filter((element) => element.getAttribute("data-app-action-sidebar-thread-active") === "true");
      }
      if (selector === "[data-app-action-sidebar-project-row][data-app-action-sidebar-project-id]") {
        return project.parentElement ? [project] : [];
      }
      if (selector === "[data-app-action-sidebar-section]") {
        return tasksSection.parentElement ? [tasksSection] : [];
      }
      if (selector === "button[aria-label]") {
        return body.querySelectorAll("button").filter((button) => button.hasAttribute("aria-label"));
      }
      if (selector === "button[aria-label], [role=button]") {
        return body.querySelectorAll("button, [role=button]")
          .filter((el) => el.hasAttribute("aria-label") || el.getAttribute("role") === "button");
      }
      if (selector === "button, [role=button], a") {
        return body.querySelectorAll("button, [role=button], a");
      }
      return body.querySelectorAll(selector);
    },
    removeEventListener(type, listener) {
      removeTrackedListener(documentListeners, type, listener);
    },
  };
  const window = {
    __codexSessionDeleteBridge: async (path, payload) => {
      bridgeCalls.push({ path, payload });
      if (bridge) return bridge(path, payload);
      return { status: "ok" };
    },
    addEventListener(type, listener, options) {
      addTrackedListener(windowListeners, type, listener, options);
    },
    clearTimeout(id) { timers.delete(id); },
    dispatchEvent() {},
    innerHeight: 800,
    innerWidth: 1200,
    getComputedStyle(element) {
      return { display: element === actionBar ? "flex" : "inline", position: "static" };
    },
    localStorage: {
      getItem: () => null,
      key: () => null,
      length: 0,
      setItem: () => {},
    },
    removeEventListener(type, listener) {
      removeTrackedListener(windowListeners, type, listener);
    },
    setTimeout(callback, delay = 0) {
      const id = ++nextTimerId;
      if (delay > 1000) timers.set(id, { callback, delay });
      else callback();
      return id;
    },
  };
  window.window = window;
  const MutationObserver = class {
    observe() {}
  };
  class CustomEvent {
    constructor(type, init) {
      this.type = type;
      this.detail = init?.detail;
    }
  }
  class ControlledDate extends Date {
    static now() {
      return nowMs;
    }
  }

  vm.runInNewContext(source, {
    Blob,
    CustomEvent,
    Date: ControlledDate,
    Error,
    HTMLElement: FakeElement,
    MouseEvent: mouseEvent ?? undefined,
    MutationObserver,
    URL,
    URLSearchParams,
    console,
    document,
    location,
    window,
  });
  const dispatchWindowEvent = (type, init = {}) => {
    let defaultPrevented = false;
    let propagationStopped = false;
    let immediateStopped = false;
    const event = {
      type,
      ...init,
      get defaultPrevented() {
        return defaultPrevented;
      },
      preventDefault() {
        defaultPrevented = true;
      },
      stopPropagation() {
        propagationStopped = true;
      },
      stopImmediatePropagation() {
        propagationStopped = true;
        immediateStopped = true;
      },
    };
    for (const { listener } of windowListeners.get(type) || []) {
      if (immediateStopped) break;
      listener(event);
    }
    return {
      defaultPrevented,
      immediateStopped,
      propagationStopped,
    };
  };
  const dispatchDocumentEvent = (type, init = {}) => {
    for (const { listener } of documentListeners.get(type) || []) {
      listener({ type, ...init });
    }
  };
  return {
    actionBar,
    advanceNow(ms) {
      nowMs += ms;
    },
    archiveButton,
    archiveTooltip,
    hoverCard,
    pinButton,
    bridgeCalls,
    dispatched,
    dispatchDocumentEvent,
    dispatchWindowEvent,
    document,
    documentElement,
    project,
    tasksSection,
    thread,
    window,
  };
}

const deleteMenuItem = (onSelect) => ({ id: "delete-thread", onSelect });

for (const withPinButton of [false, true]) {
  test(`keeps injected icons inside the native action rail without tooltip wrappers (pin: ${withPinButton})`, () => {
    const runtime = loadInjection({ directArchiveButton: true, withPinButton });
    const exportButton = runtime.thread.querySelector("[data-codey-session-export]");
    const deleteButton = runtime.thread.querySelector("[data-codey-session-delete]");
    assert.deepEqual(runtime.actionBar.children, [
      ...(withPinButton ? [runtime.pinButton] : []),
      exportButton,
      runtime.archiveButton,
      deleteButton,
    ]);
    assert.deepEqual(runtime.hoverCard.children, [runtime.actionBar]);
    assert.equal(runtime.actionBar.getAttribute("data-codey-session-action-row"), "true");
    assert.equal(runtime.hoverCard.hasAttribute("data-codey-session-action-row"), false);
  });
}

test("injects the sidebar delete icon and opens the official permanent delete entry", async () => {
  const calls = [];
  const runtime = loadInjection({
    menuItems: [
      { id: "rename-thread", onSelect: () => calls.push("rename") },
      { id: "archive-thread", onSelect: () => calls.push("archive") },
      deleteMenuItem(() => calls.push("delete")),
    ],
  });
  const exportButton = runtime.thread.querySelector("[data-codey-session-export]");
  const deleteButton = runtime.thread.querySelector("[data-codey-session-delete]");

  assert.ok(deleteButton);
  assert.deepEqual(runtime.actionBar.children, [
    exportButton,
    runtime.archiveButton,
    deleteButton,
  ]);
  assert.equal(deleteButton.getAttribute("aria-label"), "永久删除");
  assert.equal(deleteButton.getAttribute("title"), null);
  assert.equal(deleteButton.getAttribute("class"), "native-thread-action");
  assert.equal(deleteButton.getAttribute("aria-haspopup"), null);
  assert.equal(deleteButton.getAttribute("aria-expanded"), null);
  assert.equal(runtime.thread.querySelectorAll("[data-codey-session-delete]").length, 1);

  await deleteButton.click();

  assert.deepEqual(calls, ["delete"]);
  assert.equal(runtime.document.body.querySelector("[role=dialog]"), null);
  assert.equal(
    runtime.bridgeCalls.some(({ path }) => path === "/session/delete"),
    false,
  );
});

test("reads the delete entry from the nearest row menu, including submenus", async () => {
  const calls = [];
  const runtime = loadInjection({
    menuItems: [
      { id: "archive-thread", onSelect: () => calls.push("archive") },
      {
        id: "move-thread-to-project",
        submenu: [
          { id: "remove-thread-from-project", onSelect: () => calls.push("remove") },
          deleteMenuItem(() => calls.push("inner-delete")),
        ],
      },
    ],
    outerMenuItems: [deleteMenuItem(() => calls.push("outer-delete"))],
  });

  await runtime.thread.querySelector("[data-codey-session-delete]").click();

  assert.deepEqual(calls, ["inner-delete"]);
});

test("awaits the official menu and ignores repeated clicks while it loads", async () => {
  const calls = [];
  let resolveItems;
  const menuItems = new Promise((resolve) => { resolveItems = resolve; });
  const runtime = loadInjection({ menuItems });
  const button = runtime.thread.querySelector("[data-codey-session-delete]");

  const firstClick = button.click();
  assert.equal(button.disabled, true);
  await button.click();
  resolveItems([deleteMenuItem(() => calls.push("delete"))]);
  await firstClick;

  assert.deepEqual(calls, ["delete"]);
  assert.equal(button.disabled, false);
  assert.equal(runtime.dispatched.length, 0);
  assert.equal(runtime.bridgeCalls.some(({ path }) => path === "/session/delete"), false);
});

test("handles an asynchronous menu failure and allows retrying", async () => {
  const runtime = loadInjection({ menuItems: Promise.reject(new Error("menu unavailable")) });
  const button = runtime.thread.querySelector("[data-codey-session-delete]");

  await button.click();

  assert.equal(button.disabled, false);
  assert.equal(runtime.dispatched.length, 0);
  assert.equal(runtime.documentElement.querySelector("#codey-runtime-toast").dataset.tone, "error");
  let selected = false;
  attachRowFiber(runtime.thread, { menuItems: [deleteMenuItem(() => { selected = true; })] });
  await button.click();
  assert.equal(selected, true);
});

test("reports a failed official delete action without opening the context menu", async () => {
  const runtime = loadInjection({
    menuItems: [deleteMenuItem(async () => { throw new Error("dialog unavailable"); })],
  });
  const button = runtime.thread.querySelector("[data-codey-session-delete]");

  await button.click();

  assert.equal(button.disabled, false);
  assert.equal(runtime.dispatched.length, 0);
  assert.match(runtime.documentElement.querySelector("#codey-runtime-toast").textContent, /请重试/);
});

test("reports a missing delete entry without opening the official context menu", async () => {
  const runtime = loadInjection({
    menuItems: [{ id: "archive-thread", onSelect: () => {} }],
  });

  await runtime.thread.querySelector("[data-codey-session-delete]").click();

  assert.equal(runtime.dispatched.length, 0);
  assert.equal(runtime.documentElement.querySelector("#codey-runtime-toast").dataset.tone, "error");
  assert.equal(
    runtime.bridgeCalls.some(({ path }) => path === "/session/delete"),
    false,
  );
});

test("reports a missing official entry instead of deleting on its own", async () => {
  const runtime = loadInjection({ mouseEvent: null });

  await runtime.thread.querySelector("[data-codey-session-delete]").click();

  const toast = runtime.documentElement.querySelector("#codey-runtime-toast");
  assert.ok(toast);
  assert.match(toast.textContent, /永久删除/);
  assert.equal(toast.dataset.tone, "error");
  assert.equal(runtime.dispatched.length, 0);
  assert.equal(
    runtime.bridgeCalls.some(({ path }) => path === "/session/delete"),
    false,
  );
});

test("mounting multiple sidebar thread rows reuses wrapper placement without per-row style recalculations", () => {
  const runtime = loadInjection({ directArchiveButton: true });
  let styleReads = 0;
  runtime.window.getComputedStyle = () => {
    styleReads += 1;
    return { display: "flex", position: "static" };
  };

  const rows = Array.from({ length: 12 }, (_, index) => {
    const row = new FakeElement("div", {
      "data-app-action-sidebar-thread-active": index === 0 ? "true" : "false",
      "data-app-action-sidebar-thread-id": `local:batch-thread-${index}`,
      "data-app-action-sidebar-thread-title": `会话 ${index}`,
    });
    const hoverCard = new FakeElement("div", { class: "contents" });
    const actionBar = new FakeElement("div", { class: "flex" });
    const archiveButton = new FakeElement("button", {
      "aria-label": "归档任务",
      class: "native-thread-action",
    });
    row.appendChild(hoverCard);
    hoverCard.appendChild(actionBar);
    actionBar.appendChild(archiveButton);
    runtime.document.body.appendChild(row);
    runtime.window.__codeyInstallSessionDeleteButtons(row);
    return { actionBar, archiveButton, row };
  });

  assert.equal(styleReads, 0);
  for (const { actionBar, archiveButton, row } of rows) {
    const deleteButton = row.querySelector("[data-codey-session-delete]");
    assert.ok(deleteButton);
    assert.deepEqual(actionBar.children, [archiveButton, deleteButton]);
    assert.equal(actionBar.getAttribute("data-codey-session-action-row"), "true");
  }
});

function appendBrowserWebview(body, conversationId, tabId = "tab-1") {
  const container = new FakeElement("div", {
    "data-browser-sidebar-webview": `${conversationId}:${tabId}`,
  });
  const webview = new FakeElement("webview", {
    "data-browser-sidebar-browser-tab-id": tabId,
    "data-browser-sidebar-conversation-id": conversationId,
    src: "https://localhost:3000",
  });
  webview.src = "https://localhost:3000";
  webview.mutedCalls = [];
  webview.stopped = 0;
  webview.loadedUrls = [];
  webview.setAudioMuted = (muted) => webview.mutedCalls.push(muted);
  webview.stop = () => { webview.stopped += 1; };
  webview.loadURL = (url) => webview.loadedUrls.push(url);
  const overlay = new FakeElement("div", {
    "data-browser-sidebar-cursor-overlay-host": `${conversationId}:${tabId}`,
  });
  container.append(webview, overlay);
  body.appendChild(container);
  return { container, overlay, webview };
}

test("destroys and removes residual browser <webview> containers on single and batch session deletion", async () => {
  const runtime = loadInjection({
    menuItems: [
      deleteMenuItem(() => {
        // Direct delete removes the thread row without mounting a dialog.
        runtime.thread.remove();
      }),
    ],
  });
  const targetBrowser = appendBrowserWebview(runtime.document.body, "thread-1", "tab-a");
  const survivorBrowser = appendBrowserWebview(runtime.document.body, "thread-keep", "tab-b");

  await runtime.thread.querySelector("[data-codey-session-delete]").click();
  await Promise.resolve();

  assert.deepEqual(targetBrowser.webview.mutedCalls, [true]);
  assert.equal(targetBrowser.webview.stopped, 1);
  assert.equal(targetBrowser.webview.src, "about:blank");
  assert.equal(targetBrowser.webview.parentElement, null);
  assert.equal(targetBrowser.container.parentElement, null);
  assert.equal(survivorBrowser.container.parentElement, runtime.document.body);
  assert.equal(survivorBrowser.webview.stopped, 0);

  // Batch DeleteThreadDialog confirmation cleans up all deleted threads while sparing cancelled ones.
  const batchA = appendBrowserWebview(runtime.document.body, "batch-1", "tab-1");
  const batchB = appendBrowserWebview(runtime.document.body, "batch-2", "tab-2");
  const dialog = new FakeElement("div", { role: "dialog" });
  dialog.__reactFiber$test = {
    memoizedProps: {},
    return: {
      memoizedProps: {
        threads: [
          { hostId: "local", threadId: "local:batch-1" },
          { hostId: "local", threadId: "batch-2" },
        ],
      },
      type: function DeleteThreadDialog() {},
    },
  };
  const form = dialog.appendChild(new FakeElement("form"));
  const cancelBtn = form.appendChild(new FakeElement("button", { type: "button" }));
  cancelBtn.textContent = "Cancel";
  const confirmBtn = form.appendChild(new FakeElement("button", { type: "submit" }));
  confirmBtn.textContent = "Delete";
  runtime.document.body.appendChild(dialog);

  // Clicking Cancel closes the dialog without touching any webviews.
  runtime.dispatchDocumentEvent("click", { target: cancelBtn });
  dialog.remove();
  await Promise.resolve();
  assert.equal(batchA.container.parentElement, runtime.document.body);
  assert.equal(batchB.container.parentElement, runtime.document.body);

  // Submitting the DeleteThreadDialog form destroys all batch threads' webviews once the dialog closes.
  runtime.document.body.appendChild(dialog);
  runtime.dispatchDocumentEvent("submit", { target: form });
  dialog.remove();
  await Promise.resolve();

  assert.equal(batchA.container.parentElement, null);
  assert.equal(batchB.container.parentElement, null);
  assert.equal(batchA.webview.stopped, 1);
  assert.equal(batchB.webview.stopped, 1);
  assert.equal(survivorBrowser.container.parentElement, runtime.document.body);
});

test("blocks CJK IME Enter submit and Escape turn-interrupt during composition and 60ms post-compositionend window", () => {
  const runtime = loadInjection();
  const composer = new FakeElement("div", {
    class: "ProseMirror",
    contenteditable: "true",
    role: "textbox",
  });
  runtime.document.body.appendChild(composer);

  // 1. Active composition (isComposing: true) blocks Enter and Escape in composer.
  runtime.dispatchWindowEvent("compositionstart", { target: composer });
  const composingEnter = runtime.dispatchWindowEvent("keydown", {
    isComposing: true,
    key: "Enter",
    keyCode: 13,
    target: composer,
  });
  assert.equal(composingEnter.defaultPrevented, true);
  assert.equal(composingEnter.immediateStopped, true);

  const composingEscape = runtime.dispatchWindowEvent("keydown", {
    isComposing: true,
    key: "Escape",
    keyCode: 27,
    target: composer,
  });
  assert.equal(composingEscape.defaultPrevented, true);
  assert.equal(composingEscape.immediateStopped, true);

  // 2. Chromium/macOS keyCode 229 with isComposing: false is also guarded.
  const key229Enter = runtime.dispatchWindowEvent("keydown", {
    code: "Enter",
    isComposing: false,
    key: "Process",
    keyCode: 229,
    target: composer,
  });
  assert.equal(key229Enter.defaultPrevented, true);
  assert.equal(key229Enter.immediateStopped, true);

  // 3. Within 60ms after compositionend, trailing Enter is blocked once; subsequent Enter passes through.
  runtime.dispatchWindowEvent("compositionend", { target: composer });
  runtime.advanceNow(35);
  const postEndEnter = runtime.dispatchWindowEvent("keydown", {
    isComposing: false,
    key: "Enter",
    keyCode: 13,
    target: composer,
  });
  assert.equal(postEndEnter.defaultPrevented, true);
  assert.equal(postEndEnter.immediateStopped, true);

  const secondNormalEnter = runtime.dispatchWindowEvent("keydown", {
    isComposing: false,
    key: "Enter",
    keyCode: 13,
    target: composer,
  });
  assert.equal(secondNormalEnter.defaultPrevented, false);
  assert.equal(secondNormalEnter.propagationStopped, false);

  // 4. After >60ms post-compositionend, normal Escape and Enter pass through untouched.
  runtime.dispatchWindowEvent("compositionstart", { target: composer });
  runtime.dispatchWindowEvent("compositionend", { target: composer });
  runtime.advanceNow(85);
  const lateEscape = runtime.dispatchWindowEvent("keydown", {
    isComposing: false,
    key: "Escape",
    keyCode: 27,
    target: composer,
  });
  assert.equal(lateEscape.defaultPrevented, false);
  assert.equal(lateEscape.propagationStopped, false);
});

test("prevents Enter on focused dictation Cancel button from discarding voice and triggers Transcribe and send when ready", () => {
  const runtime = loadInjection();
  const dictationBar = runtime.document.body.appendChild(new FakeElement("div"));
  const cancelButton = dictationBar.appendChild(new FakeElement("button", {
    "aria-label": "Cancel",
  }));
  let cancelClicks = 0;
  cancelButton.addEventListener("click", () => { cancelClicks += 1; });

  const submitButton = dictationBar.appendChild(new FakeElement("button", {
    "aria-busy": "true",
    "aria-label": "Transcribe and send",
  }));
  let submitClicks = 0;
  submitButton.addEventListener("click", () => { submitClicks += 1; });
  runtime.document.activeElement = cancelButton;

  // While transcribing (aria-busy="true" and Cancel autoFocused), Enter must NOT cancel or double-submit.
  const busyEnter = runtime.dispatchWindowEvent("keydown", {
    key: "Enter",
    keyCode: 13,
    target: cancelButton,
  });
  assert.equal(busyEnter.defaultPrevented, true);
  assert.equal(busyEnter.immediateStopped, true);
  assert.equal(cancelClicks, 0);
  assert.equal(submitClicks, 0);

  // Once ready (not busy, enabled), pressing Enter on Cancel or composer submits instead of cancelling.
  submitButton.setAttribute("aria-busy", "false");
  submitButton.setAttribute("aria-label", "转录并发送");
  const readyEnter = runtime.dispatchWindowEvent("keydown", {
    key: "Enter",
    keyCode: 13,
    target: cancelButton,
  });
  assert.equal(readyEnter.defaultPrevented, true);
  assert.equal(readyEnter.immediateStopped, true);
  assert.equal(cancelClicks, 0);
  assert.equal(submitClicks, 1);
});
