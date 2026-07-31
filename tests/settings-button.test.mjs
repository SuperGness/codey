import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/renderer-inject.js", import.meta.url), "utf8");

class FakeElement {
  constructor(tagName = "div", { visible = true, right = 100, width = right, height = 46, top = 0 } = {}) {
    this.tagName = tagName.toUpperCase();
    this.children = [];
    this.dataset = {};
    this.id = "";
    this.parentElement = null;
    this.right = right;
    this.width = width;
    this.height = height;
    this.top = top;
    this.style = {};
    this.textContent = "";
    this.innerHTML = "";
    this.title = "";
    this.attributes = new Map();
    this.visible = visible;
    this.isConnected = false;
    this.rectReads = 0;
  }

  addEventListener() {}

  get nextElementSibling() {
    if (!this.parentElement) return null;
    const index = this.parentElement.children.indexOf(this);
    return index >= 0 ? this.parentElement.children[index + 1] || null : null;
  }

  appendChild(child) {
    child.remove();
    child.parentElement = this;
    child.isConnected = true;
    this.children.push(child);
    return child;
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

  closest(selector) {
    if (selector.includes("aria-hidden") && this.getAttribute("aria-hidden") === "true") {
      return this;
    }
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

  matches() {
    return false;
  }

  querySelector() {
    return null;
  }

  querySelectorAll() {
    return [];
  }

  remove() {
    if (!this.parentElement) return;
    const index = this.parentElement.children.indexOf(this);
    if (index >= 0) this.parentElement.children.splice(index, 1);
    this.parentElement = null;
    this.isConnected = false;
  }

  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
    if (name === "id") this.id = String(value);
  }
}

function bootRenderer({ documentElement, placeholders = {}, extraWindow = {} }) {
  const document = {
    body: new FakeElement("body"),
    documentElement,
    createElement: (tagName) => {
      const element = new FakeElement(tagName);
      let id = element.id;
      Object.defineProperty(element, "id", {
        configurable: true,
        get: () => id,
        set: (value) => {
          id = String(value);
          if (id) placeholders[id] = element;
        },
      });
      const originalSetAttribute = element.setAttribute.bind(element);
      element.setAttribute = (name, value) => {
        originalSetAttribute(name, value);
        if (name === "id") placeholders[String(value)] = element;
      };
      return element;
    },
    getElementById: (id) => placeholders[id] || null,
    querySelector: () => null,
    querySelectorAll: () => [],
    addEventListener() {},
    removeEventListener() {},
  };
  const window = {
    addEventListener() {},
    alert() {},
    clearTimeout() {},
    dispatchEvent() {
      return true;
    },
    getComputedStyle: () => ({ display: "flex", visibility: "visible" }),
    innerWidth: 1200,
    localStorage: { getItem: () => null, key: () => null, length: 0, setItem() {} },
    setTimeout: () => 1,
    ...extraWindow,
  };
  window.window = window;

  vm.runInNewContext(source, {
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
      observe() {}
    },
    URLSearchParams,
    window,
  });

  return { document, window, placeholders };
}

test("mounts the Codey button on the window chrome, left of caption controls", () => {
  const documentElement = new FakeElement("html", { right: 1200, width: 1200, height: 800 });
  const sidebar = new FakeElement("nav", { right: 84, width: 84, height: 720 });
  const staleButton = new FakeElement("button", { right: 40, width: 28 });
  staleButton.id = "codey-settings-button";
  sidebar.appendChild(staleButton);

  const placeholders = {
    "codey-core-injected-style": new FakeElement("style"),
    "codey-settings-button": staleButton,
  };
  const { document } = bootRenderer({ documentElement, placeholders });

  assert.equal(staleButton.parentElement, documentElement);
  assert.equal(staleButton.dataset.codeyWindowChrome, "true");
  assert.equal(sidebar.children.includes(staleButton), false);
  assert.equal(document.documentElement.children.includes(staleButton), true);
  assert.match(source, /windowControlsReservePx = 138/);
  assert.match(source, /position: fixed !important/);
  assert.match(source, /titlebar-area-width/);
  assert.doesNotMatch(source, /findHeaderMount/);
  assert.doesNotMatch(source, /isTopChromeMountTarget/);
});

test("marks the Codey button when a silent update check finds a new version", async () => {
  const documentElement = new FakeElement("html", { right: 1200, width: 1200 });
  const placeholders = {};
  let nextTimerId = 1;
  const timers = [];
  const events = [];
  const activeTimers = () => timers.filter((timer) => !timer.cleared);

  const { window } = bootRenderer({
    documentElement,
    placeholders,
    extraWindow: {
      __codexSessionDeleteBridge: async (path) => {
        assert.equal(path, "/api/check_for_updates");
        return {
          currentVersion: "0.3.9",
          latestVersion: "0.4.0",
          updateAvailable: true,
          selectedAsset: { fileName: "Codey-0.4.0.zip" },
        };
      },
      clearTimeout(id) {
        const timer = timers.find((entry) => entry.id === id);
        if (timer) timer.cleared = true;
      },
      dispatchEvent(event) {
        events.push(event);
        return true;
      },
      setTimeout(callback, delay) {
        const timer = { id: nextTimerId, callback, delay, cleared: false };
        nextTimerId += 1;
        timers.push(timer);
        return timer.id;
      },
    },
  });

  const initialTimer = activeTimers().find((timer) => timer.delay === 0);
  assert.equal(initialTimer?.delay, 0);
  initialTimer.cleared = true;
  initialTimer.callback();
  await new Promise((resolve) => setImmediate(resolve));
  const button = placeholders["codey-settings-button"];
  assert.ok(button);
  assert.equal(button.parentElement, documentElement);
  assert.equal(button.getAttribute("data-codey-update-available"), "true");
  assert.equal(button.getAttribute("aria-label"), "打开 Codey 配置，有可用更新");
  assert.equal(window.__codeyUpdateAvailability.latestVersion, "0.4.0");
  assert.equal(events.length, 1);
  assert.equal(events[0].type, "codey-update-availability-changed");
  assert.equal(
    activeTimers().some((timer) => timer.delay === 30 * 60 * 1000),
    false,
  );
});

test("repeated scans fast-path an already mounted window-chrome button", () => {
  const documentElement = new FakeElement("html", { right: 1200, width: 1200 });
  const codeyButton = new FakeElement("button", { right: 1060, width: 28 });
  codeyButton.id = "codey-settings-button";
  codeyButton.dataset.codeyWindowChrome = "true";
  documentElement.appendChild(codeyButton);

  const placeholders = {
    "codey-core-injected-style": new FakeElement("style"),
    "codey-settings-button": codeyButton,
  };
  let createCount = 0;
  const document = {
    body: new FakeElement("body"),
    documentElement,
    createElement: (tagName) => {
      createCount += 1;
      return new FakeElement(tagName);
    },
    getElementById: (id) => placeholders[id] || null,
    querySelector: () => null,
    querySelectorAll: () => [],
    addEventListener() {},
    removeEventListener() {},
  };
  const window = {
    addEventListener() {},
    alert() {},
    clearTimeout() {},
    getComputedStyle: () => ({ display: "flex", visibility: "visible" }),
    setTimeout: () => 1,
  };
  window.window = window;

  vm.runInNewContext(source, {
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

  createCount = 0;
  for (let scan = 0; scan < 10; scan += 1) {
    window.__codeyRendererScan();
  }
  assert.equal(createCount, 0);
  assert.equal(codeyButton.parentElement, documentElement);
  assert.equal(codeyButton.dataset.codeyWindowChrome, "true");
});