import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const root = new URL("../", import.meta.url);
const [bridgeSource, source] = await Promise.all([
  readFile(new URL("public/codey-bridge.js", root), "utf8"),
  readFile(new URL("public/security-warning-shield.js", root), "utf8"),
]);

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", text = "") {
    super(tagName, { connected: true });
    this.textContent = text;
    this.clicks = 0;
    this.manualQueryCount = 0;
    this.querySelectorAllCalls = 0;
  }

  click() {
    this.clicks += 1;
  }

  matches(selector) {
    return (
      (selector.includes("button") && this.tagName === "BUTTON")
      || super.matches(selector)
    );
  }

  querySelectorAll() {
    this.querySelectorAllCalls += 1;
    this.manualQueryCount += 1;
    const matches = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (child.tagName === "BUTTON" || child.getAttribute("role") === "button") {
          matches.push(child);
        }
        visit(child);
      }
    };
    visit(this);
    return matches;
  }
}

function createRuntime(config, { deferTimers = false } = {}) {
  const html = new FakeElement("html");
  const body = html.appendChild(new FakeElement("body"));
  const listeners = new Map();
  const statusEvents = [];
  let mutationCallback = null;
  const timers = [];
  const document = {
    body,
    documentElement: html,
    querySelectorAll: (...args) => html.querySelectorAll(...args),
  };
  const window = {
    __codexSessionDeleteBridge: async () => config,
    __codeyInjectionStatus: {
      "security-warning-shield": { status: "executed", detail: null, error: null },
    },
    addEventListener: (name, listener) => listeners.set(name, listener),
    CustomEvent: class {
      constructor(type, init = {}) {
        this.type = type;
        this.detail = init.detail;
      }
    },
    dispatchEvent: (event) => {
      statusEvents.push(event);
      return true;
    },
    setTimeout: (callback) => {
      if (!deferTimers) {
        callback();
        return timers.length + 1;
      }
      timers.push(callback);
      return timers.length;
    },
  };
  window.window = window;
  const sandbox = {
    document,
    Element: FakeElement,
    MutationObserver: class {
      constructor(callback) {
        mutationCallback = callback;
      }

      observe() {}

      disconnect() {}
    },
    window,
  };
  vm.runInNewContext(bridgeSource, sandbox);
  vm.runInNewContext(source, sandbox);
  return {
    body,
    listeners,
    statusEvents,
    get mutationCallback() {
      return mutationCallback;
    },
    flushNextTimer() {
      timers.shift()?.();
    },
    flushTimers() {
      while (timers.length) {
        const callback = timers.shift();
        callback();
        while (timers.length) timers.shift()();
      }
    },
    window,
  };
}

function appendEnglishWarning(body) {
  const warning = body.appendChild(new FakeElement(
    "section",
    "Full access is on ChatGPT can run commands without your permission. Prompt injection.",
  ));
  const button = warning.appendChild(new FakeElement("button", "Hide from this session"));
  return { button, warning };
}

function appendPersistentEnglishWarning(body) {
  const warning = body.appendChild(new FakeElement(
    "section",
    "Full access is on ChatGPT can edit any file and run commands with internet access without your approval. This increases the risk of data loss, exposed information, and unexpected changes.",
  ));
  const button = warning.appendChild(new FakeElement("button", "Don’t show again"));
  return { button, warning };
}

function appendCurrentSessionWarning(body) {
  const warning = body.appendChild(new FakeElement(
    "section",
    "Full access is on ChatGPT can edit any file and run commands with internet access without your approval. This increases the risk of data loss, exposed information, and unexpected changes.",
  ));
  const button = warning.appendChild(new FakeElement("button", ""));
  button.setAttribute("aria-label", "Dismiss Full access warning for this session");
  return { button, warning };
}

function appendCurrentChineseWarning(body) {
  const warning = body.appendChild(new FakeElement(
    "aside",
    "完整访问权限已开启 ChatGPT 可以在未经你批准的情况下编辑任何文件，并通过互联网访问权限运行命令。这会增加数据丢失、信息暴露和意外更改的风险。了解更多关于风险升高的信息。",
  ));
  warning.setAttribute("role", "status");
  const button = warning.appendChild(new FakeElement("button", "不再显示"));
  return { button, warning };
}

function appendUltraConfirmation(body) {
  const warning = body.appendChild(new FakeElement(
    "section",
    "是否启用 Ultra 搭配完整访问权限? 开启 Ultra 和完整访问权限后，Codex 可运行命令、联网检索并编辑文件。",
  ));
  const fullAccessButton = warning.appendChild(new FakeElement("button", "使用完整访问权限"));
  const continueButton = warning.appendChild(new FakeElement("button", "继续"));
  return { continueButton, fullAccessButton, warning };
}

function createBusyTree({ depth = 5, width = 6 } = {}) {
  const root = new FakeElement("section");
  let level = [root];
  for (let currentDepth = 0; currentDepth < depth; currentDepth += 1) {
    const next = [];
    for (const parent of level) {
      for (let index = 0; index < width; index += 1) {
        next.push(parent.appendChild(new FakeElement("div", "streamed output")));
      }
    }
    level = next;
  }
  return root;
}

function createWideTree(width = 600) {
  const root = new FakeElement("section");
  for (let index = 0; index < width; index += 1) {
    root.appendChild(new FakeElement("div", "streamed output"));
  }
  return { root };
}

test("full-access warning shield is opt-in and persisted by Codey settings", async () => {
  const [sectionsSource, configSource, commandSource, cdpSource] = await Promise.all([
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readFile(new URL("backend/src/config/types.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("backend/src/cdp.rs", root), "utf8"),
  ]);

  assert.match(configSource, /pub hide_full_access_warning: bool/);
  assert.match(configSource, /hide_full_access_warning: false/);
  assert.match(commandSource, /config\.hide_full_access_warning = config_input\.hide_full_access_warning/);
  assert.match(sectionsSource, /checked=\{config\.hideFullAccessWarning\}/);
  assert.match(sectionsSource, /aria-label="屏蔽完全访问安全提示"/);
  assert.match(cdpSource, /dist-overlay\/inject\/security-warning-shield\.js/);
});

test("disabled shield preserves the native full-access warning", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: false });
  const { button, warning } = appendEnglishWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.window.__codeySecurityWarningShield.enabled, false);
  assert.equal(
    runtime.window.__codeyInjectionStatus["security-warning-shield"].status,
    "inactive",
  );
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
  assert.equal(button.clicks, 0);
  assert.equal(warning.style.display, undefined);
});

test("enabled shield dismisses a verified full-access warning once", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const { button, warning } = appendEnglishWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(button.clicks, 1);
  assert.equal(
    runtime.window.__codeyInjectionStatus["security-warning-shield"].status,
    "effective",
  );
  assert.equal(warning.style.display, "none:important");
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
  assert.equal(button.clicks, 1);
});

test("enabled shield dismisses the persistent full-access warning", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const { button, warning } = appendPersistentEnglishWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
});

test("enabled shield dismisses the current icon-only session warning", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const { button, warning } = appendCurrentSessionWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(button.textContent, "");
  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
});

test("enabled shield dismisses the current Chinese full-access callout", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const { button, warning } = appendCurrentChineseWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(warning.getAttribute("role"), "status");
  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
});

test("enabled shield continues past the Ultra full-access confirmation", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const { continueButton, fullAccessButton, warning } = appendUltraConfirmation(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(fullAccessButton.clicks, 1);
  assert.equal(continueButton.clicks, 0);
  assert.equal(warning.isConnected, false);
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
});

test("a warning label that renders after insertion is still dismissed", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const warning = runtime.body.appendChild(new FakeElement(
    "section",
    "Full access is on ChatGPT can run commands without your permission. Prompt injection.",
  ));
  // The button exists before its label does, which is what React does when the
  // action text streams in a tick later.
  const button = warning.appendChild(new FakeElement("button", ""));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(button.clicks, 0);

  button.textContent = "Hide from this session";
  const label = new FakeElement("span", "Hide from this session");
  // A text-only or child insertion inside the existing button must still be
  // scanned; only the inserted node's own subtree is not enough.
  runtime.mutationCallback([{ addedNodes: [label], target: button, type: "childList" }]);

  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
});

test("ignores streamed turn updates and still handles a late warning outside the turn", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const turn = runtime.body.appendChild(new FakeElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const warning = turn.appendChild(new FakeElement("section"));
  const button = warning.appendChild(
    new FakeElement("button", "Hide from this session"),
  );
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(button.clicks, 0);
  const before = runtime.window.__codeySecurityWarningShield.snapshot();

  runtime.mutationCallback([
    {
      addedNodes: [button],
      target: warning,
      type: "childList",
    },
    {
      addedNodes: [new FakeElement("span", "streamed output")],
      target: button,
      type: "childList",
    },
  ]);

  const after = runtime.window.__codeySecurityWarningShield.snapshot();
  assert.equal(after.pendingRoots, before.pendingRoots);
  assert.equal(after.scanCount, before.scanCount);
  assert.equal(after.scannedElements, before.scannedElements);
  assert.equal(button.clicks, 0);

  const later = appendEnglishWarning(runtime.body);
  runtime.mutationCallback([{
    addedNodes: [later.warning],
    target: runtime.body,
    type: "childList",
  }]);
  runtime.flushNextTimer();
  assert.equal(later.button.clicks, 1);
});

test("keeps warning dialogs inside a conversation turn eligible", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const turn = runtime.body.appendChild(new FakeElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const dialog = turn.appendChild(new FakeElement("section"));
  dialog.setAttribute("role", "dialog");
  dialog.textContent = "Full access is on without your permission";
  const button = dialog.appendChild(
    new FakeElement("button", "Hide from this session"),
  );

  runtime.mutationCallback([{
    addedNodes: [button],
    target: dialog,
    type: "childList",
  }]);
  runtime.flushNextTimer();

  assert.equal(button.clicks, 1);
  assert.equal(dialog.style.display, "none:important");
});

test("keeps a warning portal nested in a conversation turn eligible", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const turn = runtime.body.appendChild(new FakeElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const host = turn.appendChild(new FakeElement("div"));
  const portal = host.appendChild(new FakeElement("section"));
  portal.setAttribute("data-radix-portal", "true");
  portal.textContent = "Full access is on without your permission";
  portal.appendChild(new FakeElement("button", "Hide from this session"));

  runtime.mutationCallback([{
    addedNodes: [portal],
    target: host,
    type: "childList",
  }]);
  runtime.flushNextTimer();

  assert.equal(portal.querySelector("button").clicks, 1);
});

test("dismisses a mounted turn portal when its action button arrives late", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const turn = runtime.body.appendChild(new FakeElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const portal = turn.appendChild(new FakeElement("div"));
  portal.setAttribute("data-radix-portal", "true");
  const title = portal.appendChild(new FakeElement(
    "h2",
    "Full access is on without your permission",
  ));
  portal.textContent = title.textContent;
  runtime.mutationCallback([{
    addedNodes: [title],
    target: portal,
    type: "childList",
  }]);
  runtime.flushNextTimer();

  const button = portal.appendChild(
    new FakeElement("button", "Hide from this session"),
  );
  runtime.mutationCallback([{
    addedNodes: [button],
    target: portal,
    type: "childList",
  }]);
  runtime.flushTimers();

  assert.equal(button.clicks, 1);
  assert.equal(portal.style.display, "none:important");
});

test("dismisses a mounted turn portal when its warning text arrives late", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const turn = runtime.body.appendChild(new FakeElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const portal = turn.appendChild(new FakeElement("div"));
  portal.setAttribute("data-radix-portal", "true");
  const button = portal.appendChild(
    new FakeElement("button", "Hide from this session"),
  );
  runtime.mutationCallback([{
    addedNodes: [button],
    target: portal,
    type: "childList",
  }]);
  runtime.flushNextTimer();
  assert.equal(button.clicks, 0);

  portal.textContent = "Full access is on without your permission";
  const risk = portal.appendChild(new FakeElement("p", "Prompt injection"));
  runtime.mutationCallback([{
    addedNodes: [risk],
    target: portal,
    type: "childList",
  }]);
  runtime.flushNextTimer();

  assert.equal(button.clicks, 1);
  assert.equal(portal.style.display, "none:important");
});

test("keeps unrelated turn portal child insertions ignored", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const turn = runtime.body.appendChild(new FakeElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const portal = turn.appendChild(new FakeElement("div"));
  portal.setAttribute("data-radix-portal", "true");
  const before = runtime.window.__codeySecurityWarningShield.snapshot();

  runtime.mutationCallback([{
    addedNodes: [new FakeElement("span", "streamed output")],
    target: portal,
    type: "childList",
  }]);

  const after = runtime.window.__codeySecurityWarningShield.snapshot();
  assert.equal(after.pendingRoots, before.pendingRoots);
  assert.equal(after.scanCount, before.scanCount);
  assert.equal(after.scannedElements, before.scannedElements);
});

test("unrelated session controls are never clicked", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const panel = runtime.body.appendChild(new FakeElement(
    "section",
    "Session preferences without your permission",
  ));
  const button = panel.appendChild(new FakeElement("button", "Hide from this session"));
  const iconButton = panel.appendChild(new FakeElement("button", ""));
  iconButton.setAttribute("aria-label", "Dismiss Full access warning for this session");
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
  assert.equal(button.clicks, 0);
  assert.equal(iconButton.clicks, 0);
});

test("defers wrapper subtree scans until the next short mutation batch", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  const streamedRoot = runtime.body.appendChild(new FakeElement("section"));
  streamedRoot.appendChild(new FakeElement("span", "streamed output"));
  streamedRoot.querySelectorAllCalls = 0;

  runtime.mutationCallback([{
    addedNodes: [streamedRoot],
    target: runtime.body,
    type: "childList",
  }]);

  assert.equal(streamedRoot.querySelectorAllCalls, 0);
  runtime.flushTimers();
  assert.equal(streamedRoot.manualQueryCount, 0);
});

test("deduplicates target and added-subtree scans within one mutation batch", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  const panel = runtime.body.appendChild(new FakeElement("section"));
  const button = panel.appendChild(new FakeElement("button", ""));
  button.setAttribute("aria-label", "Dismiss Full access warning for this session");
  panel.appendChild(new FakeElement("span", "Full access is on without your permission"));
  panel.querySelectorAllCalls = 0;
  button.querySelectorAllCalls = 0;

  runtime.mutationCallback([
    {
      addedNodes: [panel],
      target: runtime.body,
      type: "childList",
    },
    {
      addedNodes: [button],
      target: panel,
      type: "childList",
    },
  ]);

  assert.ok(
    button.querySelectorAllCalls + panel.querySelectorAllCalls <= 1,
    "one mutation batch should scan the shared subtree once",
  );
  runtime.flushTimers();
  assert.equal(button.clicks, 0);
});

test("a wrapper and its button mutations share one batch scan", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  const panel = runtime.body.appendChild(new FakeElement(
    "section",
    "Full access is on ChatGPT can run without your permission. Prompt injection.",
  ));
  const button = panel.appendChild(new FakeElement("button", "Hide from this session"));
  await new Promise((resolve) => setImmediate(resolve));
  const before = runtime.window.__codeySecurityWarningShield.snapshot();

  runtime.mutationCallback([
    {
      addedNodes: [button],
      target: panel,
      type: "childList",
    },
    {
      addedNodes: [panel],
      target: runtime.body,
      type: "childList",
    },
  ]);

  const after = runtime.window.__codeySecurityWarningShield.snapshot();
  assert.equal(after.scanCount - before.scanCount, 0);
  runtime.flushNextTimer();
  const scanned = runtime.window.__codeySecurityWarningShield.snapshot();
  assert.equal(scanned.scanCount - before.scanCount, 1);
  assert.equal(button.clicks, 1);
});

test("shares the element budget across deferred roots and retains overflow", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const targetRoot = runtime.body.appendChild(new FakeElement("section"));
  const addedRoot = createBusyTree();
  runtime.mutationCallback([{
    addedNodes: [addedRoot],
    target: targetRoot,
    type: "childList",
  }]);

  const before = runtime.window.__codeySecurityWarningShield.snapshot().scannedElements;
  runtime.flushNextTimer();
  const snapshot = runtime.window.__codeySecurityWarningShield.snapshot();

  assert.equal(snapshot.batchElementLimit, 160);
  assert.equal(snapshot.scannedElements - before, 160);
  assert.ok(snapshot.pendingRoots > 0);
});

test("bounds element and child expansion for a wide subtree across batches", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  runtime.flushNextTimer();
  const { root: wideRoot } = createWideTree();
  runtime.body.appendChild(wideRoot);
  const { button, warning } = appendEnglishWarning(wideRoot);
  runtime.mutationCallback([{
    addedNodes: [wideRoot],
    target: runtime.body,
    type: "childList",
  }]);

  let previous = runtime.window.__codeySecurityWarningShield.snapshot();
  let scannedBatches = 0;
  while (button.clicks === 0 && scannedBatches < 8) {
    runtime.flushNextTimer();
    const current = runtime.window.__codeySecurityWarningShield.snapshot();
    assert.ok(current.scannedElements - previous.scannedElements <= 160);
    assert.ok(current.childExpansions - previous.childExpansions <= 160);
    assert.ok(current.scannedElements - previous.scannedElements > 0);
    previous = current;
    scannedBatches += 1;
  }
  assert.ok(scannedBatches > 1);
  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
});

test("keeps the newest warning root when the pending set is saturated", async () => {
  const runtime = createRuntime(
    { hideFullAccessWarning: true },
    { deferTimers: true },
  );
  await new Promise((resolve) => setImmediate(resolve));
  for (let index = 0; index < 40; index += 1) {
    const root = runtime.body.appendChild(new FakeElement("section"));
    runtime.mutationCallback([{
      addedNodes: [root],
      target: runtime.body,
      type: "childList",
    }]);
  }
  const { button, warning } = appendEnglishWarning(runtime.body);
  runtime.mutationCallback([{
    addedNodes: [warning],
    target: runtime.body,
    type: "childList",
  }]);

  runtime.flushTimers();

  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
});

test("config changes publish the shield's current injection status", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: false });
  await new Promise((resolve) => setImmediate(resolve));
  const configListener = runtime.listeners.get("codey:config-changed");
  configListener({ detail: { config: { hideFullAccessWarning: true } } });

  assert.equal(
    runtime.window.__codeyInjectionStatus["security-warning-shield"].status,
    "effective",
  );
  assert.deepEqual(
    { ...runtime.statusEvents.at(-1).detail },
    { id: "security-warning-shield", status: "effective" },
  );
});
