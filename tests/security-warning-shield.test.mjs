import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const root = new URL("../", import.meta.url);
const source = await readFile(new URL("public/security-warning-shield.js", root), "utf8");

class FakeElement {
  constructor(tagName = "div", text = "") {
    this.attributes = new Map();
    this.children = [];
    this.disabled = false;
    this.isConnected = true;
    this.parentElement = null;
    this.style = {
      setProperty: (name, value, priority) => {
        this.style[name] = `${value}:${priority}`;
      },
    };
    this.tagName = tagName.toUpperCase();
    this.textContent = text;
    this.clicks = 0;
  }

  appendChild(child) {
    child.parentElement = this;
    this.children.push(child);
    return child;
  }

  click() {
    this.clicks += 1;
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
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

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
}

function createRuntime(config) {
  const html = new FakeElement("html");
  const body = html.appendChild(new FakeElement("body"));
  const listeners = new Map();
  const document = {
    body,
    documentElement: html,
    querySelectorAll: (...args) => html.querySelectorAll(...args),
  };
  const window = {
    __codexSessionDeleteBridge: async () => config,
    addEventListener: (name, listener) => listeners.set(name, listener),
    setTimeout: (callback) => {
      callback();
      return 1;
    },
  };
  window.window = window;
  vm.runInNewContext(source, {
    document,
    Element: FakeElement,
    MutationObserver: class {
      observe() {}
    },
    window,
  });
  return { body, listeners, window };
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

test("full-access warning shield is opt-in and persisted by Codey settings", async () => {
  const [sectionsSource, configSource, commandSource, cdpSource] = await Promise.all([
    readFile(new URL("src/AppSections.tsx", root), "utf8"),
    readFile(new URL("backend/src/config.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("backend/src/cdp.rs", root), "utf8"),
  ]);

  assert.match(configSource, /pub hide_full_access_warning: bool/);
  assert.match(configSource, /hide_full_access_warning: false/);
  assert.match(commandSource, /config\.hide_full_access_warning = config_input\.hide_full_access_warning/);
  assert.match(sectionsSource, /checked=\{config\.hideFullAccessWarning\}/);
  assert.match(sectionsSource, /aria-label="屏蔽完全访问安全提示"/);
  assert.match(cdpSource, /public\/security-warning-shield\.js/);
});

test("disabled shield preserves the native full-access warning", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: false });
  const { button, warning } = appendEnglishWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.window.__codeySecurityWarningShield.enabled, false);
  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
  assert.equal(button.clicks, 0);
  assert.equal(warning.style.display, undefined);
});

test("enabled shield dismisses a verified full-access warning once", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const { button, warning } = appendEnglishWarning(runtime.body);
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(button.clicks, 1);
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

test("unrelated session controls are never clicked", async () => {
  const runtime = createRuntime({ hideFullAccessWarning: true });
  const panel = runtime.body.appendChild(new FakeElement(
    "section",
    "Session preferences without your permission",
  ));
  const button = panel.appendChild(new FakeElement("button", "Hide from this session"));
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(runtime.window.__codeySecurityWarningShield.dismissWarnings(), 0);
  assert.equal(button.clicks, 0);
});
