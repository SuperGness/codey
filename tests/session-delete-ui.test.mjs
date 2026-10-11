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
} = {}) {
  const body = new FakeElement("body");
  const documentElement = new FakeElement("html");
  const thread = new FakeElement("div", {
    "data-app-action-sidebar-thread-active": "false",
    "data-app-action-sidebar-thread-id": "local:thread-1",
    "data-app-action-sidebar-thread-title": "待删除会话",
  });
  const actionBar = new FakeElement("div");
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
  thread.appendChild(actionBar);
  actionBar.appendChild(archiveTooltip);
  archiveTooltip.appendChild(archiveButton);
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
  const location = { pathname: "/", reload() {}, search: "" };
  const documentListeners = new Map();
  const document = {
    body,
    documentElement,
    addEventListener(type, listener) {
      documentListeners.set(type, listener);
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
      if (selector === "button, [role=button], a") {
        return body.querySelectorAll("button, [role=button], a");
      }
      return [];
    },
    removeEventListener(type) {
      documentListeners.delete(type);
    },
  };
  const window = {
    __codexSessionDeleteBridge: async (path, payload) => {
      bridgeCalls.push({ path, payload });
      if (bridge) return bridge(path, payload);
      return { status: "ok" };
    },
    addEventListener() {},
    clearTimeout(id) { timers.delete(id); },
    dispatchEvent() {},
    innerHeight: 800,
    innerWidth: 1200,
    localStorage: {
      getItem: () => null,
      key: () => null,
      length: 0,
      setItem: () => {},
    },
    removeEventListener() {},
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

  vm.runInNewContext(source, {
    Blob,
    CustomEvent,
    Date,
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
  return {
    actionBar,
    archiveTooltip,
    bridgeCalls,
    dispatched,
    document,
    documentElement,
    project,
    tasksSection,
    thread,
    window,
  };
}

const deleteMenuItem = (onSelect) => ({ id: "delete-thread", onSelect });

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
    runtime.archiveTooltip,
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
