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
    return null;
  }

  querySelectorAll(selector) {
    if (selector !== "button, [role=button], a[href]") return [];
    const controls = [];
    const visit = (element) => {
      for (const child of element.children) {
        if (child.tagName === "BUTTON") controls.push(child);
        visit(child);
      }
    };
    visit(this);
    return controls;
  }

  matches(selector) {
    return selector
      .split(",")
      .some((part) => part.trim().toUpperCase() === this.tagName);
  }

  remove() {
    if (!this.parentElement) return;
    const index = this.parentElement.children.indexOf(this);
    if (index >= 0) this.parentElement.children.splice(index, 1);
    this.parentElement = null;
    this.isConnected = false;
  }

  setAttribute() {}
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

  assert.equal(codeyButton.parentElement, visibleHeader);
  assert.equal(codeyButton.dataset.codeyHeaderActions, "true");
  assert.equal(hiddenHeader.children.includes(codeyButton), false);
  assert.deepEqual(visibleHeader.children, [codeyButton, rightRegion]);
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

  vm.runInNewContext(source, {
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
