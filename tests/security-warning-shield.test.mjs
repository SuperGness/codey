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
  }

  click() {
    this.clicks += 1;
  }

  matches(selector) {
    return selector.includes("button") && this.tagName === "BUTTON";
  }

  querySelectorAll() {
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

function createRuntime(config) {
  const html = new FakeElement("html");
  const body = html.appendChild(new FakeElement("body"));
  const listeners = new Map();
  const statusEvents = [];
  let mutationCallback = null;
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
      callback();
      return 1;
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
  const button = warning.appendChild(new FakeElement("button", "继续"));
  return { button, warning };
}

test("full-access warning shield is opt-in and persisted by Codey settings", async () => {
  const [sectionsSource, configSource, commandSource, cdpSource] = await Promise.all([
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readFile(new URL("backend/src/config.rs", root), "utf8"),
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
  const { button, warning } = appendUltraConfirmation(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(button.clicks, 1);
  assert.equal(warning.style.display, "none:important");
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
