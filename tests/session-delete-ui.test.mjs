import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement {
  constructor(tagName = "div", attributes = {}) {
    this.tagName = tagName.toUpperCase();
    this.attributes = new Map(Object.entries(attributes));
    this.children = [];
    this.dataset = {};
    this.disabled = false;
    this.listeners = new Map();
    this.parentElement = null;
    this.innerHTML = "";
    this.style = {
      setProperty: (name, value) => {
        this.style[name] = value;
      },
    };
    this.textContent = "";
  }

  append(...children) {
    children.forEach((child) => this.appendChild(child));
  }

  appendChild(child) {
    child.remove();
    child.parentElement = this;
    this.children.push(child);
    return child;
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) || [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  click() {
    const event = {
      composedPath: () => [this],
      preventDefault() {},
      stopImmediatePropagation() {},
      stopPropagation() {},
    };
    for (const listener of this.listeners.get("click") || []) listener(event);
  }

  dispatch(type) {
    for (const listener of this.listeners.get(type) || []) listener({ type });
  }

  focus() {}

  getAttribute(name) {
    if (name === "id" && this.id) return this.id;
    return this.attributes.get(name) ?? null;
  }

  getBoundingClientRect() {
    if (this.hasAttribute("data-codey-session-delete")) {
      return { bottom: 124, height: 24, left: 230, right: 254, top: 100, width: 24 };
    }
    if (this.hasAttribute("data-app-action-sidebar-project-row")) {
      return { bottom: 32, height: 32, left: 0, right: 248, top: 0, width: 248 };
    }
    if (this.getAttribute("aria-label") === "项目操作") {
      return { bottom: 28, height: 24, left: 220, right: 244, top: 4, width: 24 };
    }
    return { bottom: 0, height: 110, left: 0, right: 248, top: 0, width: 248 };
  }

  getClientRects() {
    return [this.getBoundingClientRect()];
  }

  hasAttribute(name) {
    if (name === "id" && this.id) return true;
    return this.attributes.has(name);
  }

  insertAdjacentElement(position, element) {
    assert.ok(position === "beforebegin" || position === "afterend");
    const siblings = this.parentElement.children;
    const index = siblings.indexOf(this);
    element.remove();
    element.parentElement = this.parentElement;
    siblings.splice(position === "beforebegin" ? index : index + 1, 0, element);
    return element;
  }

  matches(selector) {
    const tag = selector.match(/^[a-z]+/i)?.[0];
    if (tag && this.tagName !== tag.toUpperCase()) return false;
    const attributes = [...selector.matchAll(/\[([^\]=\]]+)(?:=(?:"([^"]*)"|'([^']*)'|([^\]]+)))?\]/g)];
    if (attributes.length) {
      return attributes.every((match) => {
        if (!this.hasAttribute(match[1])) return false;
        const expected = match[2] ?? match[3] ?? match[4];
        return expected === undefined || this.getAttribute(match[1]) === expected;
      });
    }
    if (tag) return true;
    return false;
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] || null;
  }

  querySelectorAll(selector) {
    const selectors = selector.split(",").map((item) => item.trim());
    const matches = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (selectors.some((candidate) => child.matches(candidate))) matches.push(child);
        visit(child);
      }
    };
    visit(this);
    return matches;
  }

  remove() {
    if (!this.parentElement) return;
    const siblings = this.parentElement.children;
    const index = siblings.indexOf(this);
    if (index >= 0) siblings.splice(index, 1);
    this.parentElement = null;
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
}

function loadInjection({ bridge, dispatcher } = {}) {
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

  const placeholder = new FakeElement();
  const bridgeCalls = [];
  const dispatcherCalls = [];
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
      if (path === "/session/delete") return { status: "ok", deleted: true };
      return { status: "ok" };
    },
    addEventListener() {},
    clearTimeout() {},
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
      if (delay > 1000) return 1;
      callback();
      return 1;
    },
  };
  if (dispatcher) {
    window.__codeyCodexSignalDispatcher = async (signal, payload) => {
      dispatcherCalls.push({ signal, payload });
      return dispatcher(signal, payload);
    };
  }
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
    Error,
    HTMLElement: FakeElement,
    MutationObserver,
    URL,
    URLSearchParams,
    console,
    document,
    location: { pathname: "/", reload() {}, search: "" },
    window,
  });
  return {
    actionBar,
    archiveButton,
    archiveTooltip,
    bridgeCalls,
    dispatcherCalls,
    document,
    project,
    projectActionButton,
    tasksActionBar,
    tasksOptionsButton,
    tasksSection,
    newTaskButton,
    thread,
    window,
  };
}

test("matches native sidebar actions and deletes after popover confirmation", async () => {
  const events = [];
  const runtime = loadInjection({
    bridge: async (path) => {
      events.push(`bridge:${path}`);
      if (path === "/session/delete") return { status: "ok", deleted: true };
      return { status: "ok" };
    },
    dispatcher: async (signal) => {
      events.push(`signal:${signal}`);
      if (signal === "refresh-recent-conversations-for-host") {
        return new Promise(() => {});
      }
    },
  });
  events.length = 0;
  const exportButton = runtime.thread.querySelector("[data-codey-session-export]");
  const sessionImportButton = runtime.thread.querySelector("[data-codey-session-import]");
  const tasksImportButton = runtime.tasksSection.querySelector("[data-codey-tasks-import]");
  const deleteButton = runtime.thread.querySelector("[data-codey-session-delete]");
  const importButton = runtime.project.querySelector("[data-codey-project-import]");

  assert.ok(exportButton);
  assert.equal(sessionImportButton, null);
  assert.ok(tasksImportButton);
  assert.ok(deleteButton);
  assert.ok(importButton);
  assert.deepEqual(runtime.actionBar.children, [
    exportButton,
    runtime.archiveTooltip,
    deleteButton,
  ]);
  assert.deepEqual(runtime.tasksActionBar.children, [
    runtime.tasksOptionsButton,
    tasksImportButton,
    runtime.newTaskButton,
  ]);
  assert.deepEqual(runtime.archiveTooltip.children, [runtime.archiveButton]);
  assert.equal(exportButton.getAttribute("aria-label"), "导出会话数据");
  assert.equal(tasksImportButton.getAttribute("aria-label"), "导入会话数据");
  assert.equal(deleteButton.getAttribute("aria-label"), "删除会话");
  assert.equal(importButton.getAttribute("aria-label"), "导入会话数据到此项目");
  assert.equal(exportButton.getAttribute("title"), null);
  assert.equal(tasksImportButton.getAttribute("title"), null);
  assert.equal(deleteButton.getAttribute("title"), null);
  assert.equal(importButton.getAttribute("title"), null);
  assert.equal(exportButton.getAttribute("class"), "native-thread-action");
  assert.equal(tasksImportButton.getAttribute("class"), "native-tasks-header-action");
  assert.equal(deleteButton.getAttribute("class"), "native-thread-action");
  assert.equal(importButton.getAttribute("class"), "native-project-action");
  assert.equal(importButton.style.right, "32px");
  assert.match(exportButton.innerHTML, /<svg[\s>]/);
  assert.match(tasksImportButton.innerHTML, /<svg[\s>]/);
  assert.match(deleteButton.innerHTML, /<svg[\s>]/);
  assert.match(importButton.innerHTML, /<svg[\s>]/);
  assert.doesNotMatch(exportButton.innerHTML, /⇩/);
  assert.doesNotMatch(importButton.innerHTML, /⇧/);

  exportButton.dispatch("mouseenter");
  const tooltip = runtime.document.body.querySelector("[role=tooltip]");
  assert.ok(tooltip);
  assert.match(
    tooltip.getAttribute("class"),
    /border-token-border bg-token-dropdown-background text-token-foreground/,
  );
  assert.equal(tooltip.children[0].children[0].textContent, "导出会话数据");
  exportButton.dispatch("mouseleave");
  assert.equal(runtime.document.body.querySelector("[role=tooltip]"), null);

  deleteButton.click();
  const popover = runtime.document.body.querySelector("[role=dialog]");
  assert.ok(popover);
  assert.match(popover.textContent + popover.children.map((child) => child.textContent).join(""), /待删除会话/);

  popover.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));

  const deletion = runtime.bridgeCalls.find((call) => call.path === "/session/delete");
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "thread-1",
    title: "待删除会话",
  });
  assert.deepEqual(JSON.parse(JSON.stringify(runtime.dispatcherCalls)), [{
    signal: "unsubscribe-thread-for-host",
    payload: {
      hostId: "local",
      threadId: "thread-1",
    },
  }, {
    signal: "handle-app-server-notification-for-host",
    payload: {
      hostId: "local",
      notification: {
        method: "thread/deleted",
        params: { threadId: "thread-1" },
      },
    },
  }, {
    signal: "refresh-recent-conversations-for-host",
    payload: { hostId: "local", sortKey: "updated_at" },
  }]);
  assert.deepEqual(events, [
    "signal:unsubscribe-thread-for-host",
    "bridge:/session/delete",
    "signal:handle-app-server-notification-for-host",
    "signal:refresh-recent-conversations-for-host",
  ]);
  assert.equal(
    runtime.document.getElementById("codey-runtime-toast")?.textContent,
    "已删除会话“待删除会话”",
  );
  assert.equal(runtime.thread.parentElement, runtime.document.body);
});

test("deletes a newly created sidebar session by its canonical conversation id", async () => {
  const runtime = loadInjection();
  const conversationId = "019f8339-ddc1-7652-8922-13e2b52d0d00";
  runtime.thread.setAttribute(
    "data-app-action-sidebar-thread-id",
    "local:client-new-thread:temporary-id",
  );
  runtime.thread.__reactFiber$test = {
    memoizedProps: {
      entry: { conversationId },
    },
    pendingProps: null,
    return: null,
  };

  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body
    .querySelector("[data-codey-session-delete-confirm]")
    .click();
  await new Promise((resolve) => setImmediate(resolve));

  const deletion = runtime.bridgeCalls.find((call) => call.path === "/session/delete");
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: conversationId,
    title: "待删除会话",
  });
  assert.equal(runtime.thread.parentElement, runtime.document.body);
});

test("treats an already missing local thread as deleted without detaching virtualized rows", async () => {
  const runtime = loadInjection({
    bridge: async (path) => {
      if (path === "/session/delete") {
        return { status: "failed", message: "Thread not found in local storage" };
      }
      return { status: "ok" };
    },
    dispatcher: async () => {},
  });

  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body
    .querySelector("[data-codey-session-delete-confirm]")
    .click();
  await new Promise((resolve) => setImmediate(resolve));

  const deletion = runtime.bridgeCalls.find((call) => call.path === "/session/delete");
  assert.deepEqual(JSON.parse(JSON.stringify(deletion?.payload)), {
    sessionId: "thread-1",
    title: "待删除会话",
  });
  assert.deepEqual(
    JSON.parse(JSON.stringify(runtime.dispatcherCalls.map(({ signal }) => signal))),
    [
      "unsubscribe-thread-for-host",
      "handle-app-server-notification-for-host",
      "refresh-recent-conversations-for-host",
    ],
  );
  assert.equal(runtime.thread.parentElement, runtime.document.body);

  runtime.document.body.appendChild(runtime.thread);
  assert.equal(runtime.window.__codeyPruneDeletedSidebarSessions(runtime.thread), true);
  assert.equal(runtime.thread.parentElement, runtime.document.body);
});
