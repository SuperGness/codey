import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement {
  constructor(tagName = "div", { visible = true, right = 100, width = right } = {}) {
    this.tagName = tagName.toUpperCase();
    this.children = [];
    this.dataset = {};
    this.id = "";
    this.parentElement = null;
    this.right = right;
    this.width = width;
    this.style = {};
    this.textContent = "";
    this.visible = visible;
  }

  addEventListener() {}

  appendChild(child) {
    child.remove();
    child.parentElement = this;
    this.children.push(child);
    return child;
  }

  insertBefore(child, before) {
    child.remove();
    const index = this.children.indexOf(before);
    assert.notEqual(index, -1);
    child.parentElement = this;
    this.children.splice(index, 0, child);
    return child;
  }

  closest() {
    return null;
  }

  getBoundingClientRect() {
    return this.visible
      ? { bottom: 46, height: 46, left: this.right - this.width, right: this.right, top: 0, width: this.width }
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

  remove() {
    if (!this.parentElement) return;
    const index = this.parentElement.children.indexOf(this);
    if (index >= 0) this.parentElement.children.splice(index, 1);
    this.parentElement = null;
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
