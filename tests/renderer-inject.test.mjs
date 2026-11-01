import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/renderer-inject.js", import.meta.url), "utf8");
const bridgeSource = readFileSync(new URL("../public/codey-bridge.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {}

const createRuntime = () => {
  const timers = new Map();
  let nextTimer = 0;
  const subscribers = [];
  const body = new FakeElement("body", { connected: true });
  const documentElement = body.appendChild(new FakeElement("html"));
  const document = {
    body,
    visibilityState: "visible",
    documentElement,
    querySelector() {
      return null;
    },
    querySelectorAll(selector) {
      if (selector === "header") return [];
      if (selector === "nav") return [];
      return documentElement.querySelectorAll(selector);
    },
    createElement(tagName) {
      return new FakeElement(tagName);
    },
    getElementById(id) {
      return documentElement.querySelector(`#${id}`) || body.querySelector(`#${id}`);
    },
  };
  const window = {
    __codeyNativeModelSelectionOnly: false,
    __codeySessionToolsInjectLoaded: true,
    addEventListener() {},
    clearTimeout(id) {
      timers.delete(id);
    },
    dispatchEvent() {},
    getComputedStyle() {
      return { display: "flex", visibility: "visible" };
    },
    localStorage: {
      getItem() { return null; },
      key() { return null; },
      length: 0,
      setItem() {},
    },
    setTimeout(callback, delay) {
      nextTimer += 1;
      timers.set(nextTimer, { callback, delay });
      return nextTimer;
    },
  };
  window.window = window;
  const context = vm.createContext({
    console,
    document,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      disconnect() {}
      observe() {}
    },
    URLSearchParams,
    window,
  });
  vm.runInContext(bridgeSource, context);
  window.__codeyMutationDispatcher = {
    snapshot: () => ({ observerInstalled: true }),
    subscribe(handler, options) {
      subscribers.push({ handler, options });
      return () => {};
    },
  };
  vm.runInContext(source, context);
  timers.clear();
  const bootstrap = subscribers.find(({ options }) => options.attributes === true);
  assert.ok(bootstrap, "renderer bootstrap should subscribe to shared mutations");
  return { body, bootstrap, document, documentElement, timers };
};

const attributeMutation = (target) => ({
  target,
  type: "attributes",
  addedNodes: [],
  removedNodes: [],
});

const childListMutation = (target, addedNodes = [], removedNodes = []) => ({
  target,
  type: "childList",
  addedNodes,
  removedNodes,
});

test("streaming childList changes inside a mounted turn skip selector scans", () => {
  const runtime = createRuntime();
  const turn = runtime.body.appendChild(new FakeElement("div", {
    attributes: { "data-turn-key": "turn-1" },
  }));
  const content = turn.appendChild(new FakeElement("div"));
  const added = new FakeElement("span");
  let scans = 0;
  const scan = (selector) => {
    scans += 1;
    return null;
  };
  content.querySelector = scan;
  added.querySelector = scan;
  added.matches = (selector) => {
    scans += 1;
    return false;
  };

  runtime.bootstrap.handler([childListMutation(content, [added])]);

  assert.equal(scans, 0);
  assert.equal(runtime.timers.size, 0);
});

test("turn-external sidebar and header changes still schedule probes", () => {
  const runtime = createRuntime();
  const sidebarRoot = runtime.body.appendChild(new FakeElement("div"));
  const sidebarMarker = new FakeElement("div", {
    attributes: { "data-app-action-sidebar-scroll": "" },
  });
  sidebarRoot.querySelector = (selector) => (
    selector.includes("data-app-action-sidebar-scroll") ? sidebarMarker : null
  );
  runtime.bootstrap.handler([childListMutation(sidebarRoot, [sidebarMarker])]);
  assert.equal(runtime.timers.size, 1);
  assert.ok([...runtime.timers.values()].some(({ delay }) => delay === 60));

  runtime.timers.clear();
  const header = runtime.body.appendChild(new FakeElement("header"));
  const headerChild = header.appendChild(new FakeElement("button"));
  runtime.bootstrap.handler([childListMutation(header, [headerChild])]);
  assert.equal(runtime.timers.size, 1);
});

test("turn-external attribute changes still process header and sidebar updates", () => {
  const runtime = createRuntime();
  const header = runtime.body.appendChild(new FakeElement("header"));
  runtime.bootstrap.handler([attributeMutation(header)]);
  assert.equal(runtime.timers.size, 1);

  runtime.timers.clear();
  const sidebar = runtime.body.appendChild(new FakeElement("nav", {
    attributes: { "data-app-action-sidebar-scroll": "" },
  }));
  runtime.bootstrap.handler([attributeMutation(sidebar)]);
  assert.equal(runtime.timers.size, 1);
});

test("turn-external prompt-like wrappers still run the bootstrap probe", () => {
  const runtime = createRuntime();
  const wrapper = runtime.body.appendChild(new FakeElement("section"));
  const prompt = new FakeElement("div", {
    attributes: { "data-app-action-sidebar-thread-id": "thread-1" },
  });
  wrapper.querySelector = (selector) => (
    selector.includes("data-app-action-sidebar-thread-id") ? prompt : null
  );

  runtime.bootstrap.handler([childListMutation(runtime.body, [wrapper])]);

  assert.equal(prompt.getAttribute("data-app-action-sidebar-thread-id"), "thread-1");
  assert.equal(runtime.timers.size, 1);
});
