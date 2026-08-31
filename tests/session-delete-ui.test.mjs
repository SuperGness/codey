import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", attributes = {}) {
    super(tagName, { attributes });
    this.innerHTML = "";
  }

  append(...children) {
    children.forEach((child) => this.appendChild(child));
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

  insertAdjacentElement(position, element) {
    assert.ok(position === "beforebegin" || position === "afterend");
    const siblings = this.parentElement.children;
    const index = siblings.indexOf(this);
    element.remove();
    element.parentElement = this.parentElement;
    siblings.splice(position === "beforebegin" ? index : index + 1, 0, element);
    return element;
  }

}

function loadInjection({
  bridge,
  sessionController,
  dispatcher = async () => {},
  now = () => Date.now(),
  tasksSectionHeading = "Tasks",
  tasksSectionLabel = "",
  tasksOptionsLabel = "任务侧边栏选项",
  newTaskLabel = "新建任务",
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
    "data-app-action-sidebar-section-heading": tasksSectionHeading,
  });
  const tasksTitleRow = new FakeElement("div");
  const tasksTitleLabel = new FakeElement("div");
  const tasksTitleLabelInner = new FakeElement("div");
  const tasksToggle = new FakeElement("button", {
    "data-app-action-sidebar-section-toggle": "",
  });
  tasksToggle.textContent = tasksSectionLabel;
  const tasksActionBar = new FakeElement("div");
  const tasksOptionsButton = new FakeElement("button", {
    "aria-label": tasksOptionsLabel,
    class: "native-tasks-header-action",
  });
  const newTaskButton = new FakeElement("button", {
    "aria-label": newTaskLabel,
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
  const reloadCalls = [];
  const timers = new Map();
  let nextTimerId = 0;
  const location = { pathname: "/", reload() { reloadCalls.push(true); }, search: "" };
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
      if (path === "/session/delete") return { status: "ok", deleted: true };
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
  if (sessionController) window.__codeyCodexSessionController = sessionController;
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
  class FakeDate extends Date {
    static now() {
      return now();
    }
  }

  vm.runInNewContext(source, {
    Blob,
    CustomEvent,
    Date: FakeDate,
    Error,
    HTMLElement: FakeElement,
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
    archiveButton,
    archiveTooltip,
    bridgeCalls,
    dispatcherCalls,
    reloadCalls,
    location,
    fireTimers(delay) {
      for (const [id, timer] of [...timers]) {
        if (timer.delay !== delay) continue;
        timers.delete(id);
        timer.callback();
      }
    },
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
    tasksImportButton,
    runtime.tasksOptionsButton,
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

  deleteButton.click();
  const popover = runtime.document.body.querySelector("[role=dialog]");
  assert.ok(popover);
  assert.match(popover.textContent + popover.children.map((child) => child.textContent).join(""), /待删除会话/);

  popover.querySelector("[data-codey-session-delete-confirm]").click();
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "pending");
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
    payload: { hostId: "local" },
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
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");
});

test("uses AppServerManager cache eviction and deletion notification on current Codex", async () => {
  const events = [];
  const runtime = loadInjection({
    bridge: async (path) => {
      events.push(`bridge:${path}`);
      if (path === "/session/delete") return { status: "ok", deleted: true };
      return { status: "ok" };
    },
    sessionController: {
      kind: "manager",
      async discardConversation(sessionId) {
        events.push(`manager:discard:${sessionId}`);
      },
      async notifyConversationDeleted(sessionId) {
        events.push(`manager:deleted:${sessionId}`);
      },
      async refreshRecentConversations() {
        events.push("manager:refresh");
      },
      async resumeConversation() {},
    },
  });
  events.length = 0;

  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body
    .querySelector("[data-codey-session-delete-confirm]")
    .click();
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(events, [
    "manager:discard:thread-1",
    "bridge:/session/delete",
    "manager:deleted:thread-1",
    "manager:refresh",
  ]);
});

test("cancels deletion when a virtualized row changes identity during confirmation", async () => {
  const runtime = loadInjection();
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.thread.setAttribute("data-app-action-sidebar-thread-id", "local:thread-2");
  runtime.thread.setAttribute("data-app-action-sidebar-thread-title", "另一条会话");
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.bridgeCalls.some(({ path }) => path === "/session/delete"), false);
  assert.equal(runtime.dispatcherCalls.some(({ signal }) => signal === "unsubscribe-thread-for-host"), false);
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), null);
  assert.match(runtime.document.getElementById("codey-runtime-toast")?.textContent, /重新确认/);
});

test("keeps the confirmed title when the same conversation is renamed", async () => {
  const runtime = loadInjection();
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.thread.setAttribute("data-app-action-sidebar-thread-title", "更新后的标题");
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));

  const deletion = runtime.bridgeCalls.find(({ path }) => path === "/session/delete");
  assert.equal(deletion?.payload.sessionId, "thread-1");
  assert.equal(deletion?.payload.title, "待删除会话");
});

test("leaves the active conversation before eviction and hard deletion", async () => {
  const events = [];
  const runtime = loadInjection({
    bridge: async (path) => {
      if (path === "/session/delete") {
        events.push("delete");
        return { status: "ok", deleted: true };
      }
      return { status: "ok" };
    },
    dispatcher: async (signal) => {
      if (signal === "unsubscribe-thread-for-host") {
        assert.equal(runtime.thread.getAttribute("data-app-action-sidebar-thread-active"), "false");
        assert.equal(runtime.location.pathname, "/");
        events.push("discard");
      }
    },
  });
  runtime.thread.setAttribute("data-app-action-sidebar-thread-active", "true");
  runtime.location.pathname = "/c/thread-1";
  runtime.newTaskButton.addEventListener("click", () => {
    events.push("navigate");
    runtime.thread.setAttribute("data-app-action-sidebar-thread-active", "false");
    runtime.location.pathname = "/";
  });
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(events, ["navigate", "discard", "delete"]);
  assert.deepEqual(runtime.reloadCalls, []);
});

test("does not evict or delete if navigation leaves the current page on the target", async () => {
  const runtime = loadInjection();
  runtime.thread.setAttribute("data-app-action-sidebar-thread-active", "true");
  runtime.location.pathname = "/c/thread-1";
  // A sidebar update alone does not prove that React finished navigation.
  runtime.newTaskButton.addEventListener("click", () => {
    runtime.thread.setAttribute("data-app-action-sidebar-thread-active", "false");
    runtime.document.body.appendChild(new FakeElement("div", {
      "data-app-action-sidebar-thread-active": "true",
      "data-app-action-sidebar-thread-id": "local:thread-2",
      "data-app-action-sidebar-thread-title": "新选中的会话",
    }));
  });
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.bridgeCalls.some(({ path }) => path === "/session/delete"), false);
  assert.equal(runtime.dispatcherCalls.some(({ signal }) => signal === "unsubscribe-thread-for-host"), false);
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), null);
  assert.match(runtime.document.getElementById("codey-runtime-toast")?.textContent, /未执行删除/);
  assert.deepEqual(runtime.reloadCalls, []);
});

test("aborts hard deletion when native unsubscribe rejects or times out", async (t) => {
  for (const outcome of ["reject", "timeout"]) {
    await t.test(outcome, async () => {
      const runtime = loadInjection({
        dispatcher: async (signal) => {
          if (signal !== "unsubscribe-thread-for-host") return;
          if (outcome === "reject") throw new Error("app-server unavailable");
          return new Promise(() => {});
        },
      });
      runtime.thread.querySelector("[data-codey-session-delete]").click();
      runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
      await new Promise((resolve) => setImmediate(resolve));
      if (outcome === "timeout") {
        runtime.fireTimers(5_000);
        await new Promise((resolve) => setImmediate(resolve));
      }

      assert.equal(runtime.bridgeCalls.some(({ path }) => path === "/session/delete"), false);
      assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), null);
      assert.match(runtime.document.getElementById("codey-runtime-toast")?.textContent, /未执行删除/);
      assert.deepEqual(runtime.reloadCalls, []);
    });
  }
});

test("does not delete a conversation reopened while native unsubscribe was pending", async () => {
  let finishUnsubscribe;
  const runtime = loadInjection({
    dispatcher: async (signal) => {
      if (signal !== "unsubscribe-thread-for-host") return;
      return new Promise((resolve) => { finishUnsubscribe = resolve; });
    },
  });
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));
  runtime.location.pathname = "/c/thread-1";
  finishUnsubscribe();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.bridgeCalls.some(({ path }) => path === "/session/delete"), false);
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), null);
  assert.match(runtime.document.getElementById("codey-runtime-toast")?.textContent, /已重新打开/);
  assert.deepEqual(runtime.reloadCalls, []);
});

test("does not evict the active page if the user returns during a backend deletion", async () => {
  let finishDelete;
  const runtime = loadInjection({
    bridge: async (path) => {
      if (path !== "/session/delete") return { status: "ok" };
      return new Promise((resolve) => { finishDelete = resolve; });
    },
  });
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));
  runtime.location.pathname = "/c/thread-1";
  finishDelete({ status: "ok", deleted: true });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");
  assert.equal(runtime.dispatcherCalls.some(({ signal }) => signal === "handle-app-server-notification-for-host"), false);
  assert.match(runtime.document.getElementById("codey-runtime-toast")?.textContent, /列表同步暂未完成/);
  assert.deepEqual(runtime.reloadCalls, []);
});

test("does not reload other conversations when post-delete synchronization fails", async () => {
  const runtime = loadInjection({
    dispatcher: async (signal) => {
      if (signal === "unsubscribe-thread-for-host") return;
      throw new Error("renderer synchronization unavailable");
    },
  });
  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body.querySelector("[data-codey-session-delete-confirm]").click();
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.bridgeCalls.filter(({ path }) => path === "/session/delete").length, 1);
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");
  assert.deepEqual(runtime.reloadCalls, []);
  assert.match(runtime.document.getElementById("codey-runtime-toast")?.textContent, /列表同步暂未完成/);
});

test("keeps permanently deleted virtualized sidebar rows hidden for the renderer lifetime", async () => {
  let nowMs = 1_000;
  const runtime = loadInjection({ now: () => nowMs });

  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body
    .querySelector("[data-codey-session-delete-confirm]")
    .click();
  await new Promise((resolve) => setImmediate(resolve));

  nowMs += 24 * 60 * 60 * 1000;
  runtime.thread.removeAttribute("data-codey-session-delete-state");

  assert.equal(runtime.window.__codeyPruneDeletedSidebarSessions(runtime.thread), true);
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");
});

test("restores an optimistically hidden sidebar session when deletion fails", async () => {
  let finishDelete;
  const runtime = loadInjection({
    bridge: async (path) => {
      if (path !== "/session/delete") return { status: "ok" };
      return new Promise((resolve) => {
        finishDelete = resolve;
      });
    },
    dispatcher: async () => {},
  });

  runtime.thread.querySelector("[data-codey-session-delete]").click();
  runtime.document.body
    .querySelector("[data-codey-session-delete-confirm]")
    .click();

  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "pending");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(typeof finishDelete, "function");

  finishDelete({ status: "failed", deleted: false, message: "database is locked" });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), null);
  assert.equal(runtime.thread.parentElement, runtime.document.body);
  assert.equal(
    runtime.document.getElementById("codey-runtime-toast")?.textContent,
    "删除失败：database is locked",
  );
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
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");
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
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");

  runtime.document.body.appendChild(runtime.thread);
  assert.equal(runtime.window.__codeyPruneDeletedSidebarSessions(runtime.thread), true);
  assert.equal(runtime.thread.parentElement, runtime.document.body);
  assert.equal(runtime.thread.getAttribute("data-codey-session-delete-state"), "deleted");
});
