import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/renderer-inject.js", import.meta.url), "utf8");
const bridgeSource = readFileSync(new URL("../public/codey-bridge.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {}

const createRuntime = ({ styleSheets = [] } = {}) => {
  const timers = new Map();
  let nextTimer = 0;
  const subscribers = [];
  const mutationObservers = [];
  const body = new FakeElement("body", { connected: true });
  const documentElement = body.appendChild(new FakeElement("html"));
  const document = {
    body,
    styleSheets,
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
      constructor(handler) {
        this.handler = handler;
        this.options = null;
        this.target = null;
        mutationObservers.push(this);
      }
      disconnect() {}
      observe(target, options) {
        this.target = target;
        this.options = options;
      }
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
  assert.ok(
    bootstrap.options.attributeFilter.includes("data-state"),
    "renderer bootstrap should include data-state in attributeFilter",
  );
  return { body, bootstrap, document, documentElement, timers, window };
};

const attributeMutation = (target, attributeName) => ({
  attributeName,
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

test("hover data-state transitions stay no-ops while open menus sync data-codey-open-within", () => {
  const runtime = createRuntime();
  const rowGroup = runtime.documentElement.appendChild(new FakeElement("div", {
    attributes: { class: "group group/project-source" },
  }));
  const buttonContainer = rowGroup.appendChild(new FakeElement("div", {
    attributes: { class: "has-[button[data-state=open]]:opacity-100" },
  }));
  const tooltipTrigger = rowGroup.appendChild(new FakeElement("span", {
    attributes: { "data-state": "closed" },
  }));
  const menuButton = buttonContainer.appendChild(new FakeElement("button", {
    attributes: { "data-state": "closed" },
  }));

  tooltipTrigger.setAttribute("data-state", "delayed-open");
  runtime.bootstrap.handler([attributeMutation(tooltipTrigger, "data-state")]);
  assert.equal(rowGroup.hasAttribute("data-codey-open-within"), false);
  assert.equal(buttonContainer.hasAttribute("data-codey-open-within"), false);
  assert.equal(runtime.timers.size, 0);

  tooltipTrigger.setAttribute("data-state", "closed");
  runtime.bootstrap.handler([attributeMutation(tooltipTrigger, "data-state")]);
  assert.equal(rowGroup.hasAttribute("data-codey-open-within"), false);
  assert.equal(runtime.timers.size, 0);

  menuButton.setAttribute("data-state", "open");
  runtime.bootstrap.handler([attributeMutation(menuButton, "data-state")]);
  assert.equal(rowGroup.getAttribute("data-codey-open-within"), "");
  assert.equal(buttonContainer.getAttribute("data-codey-open-within"), "");

  menuButton.setAttribute("data-state", "closed");
  runtime.bootstrap.handler([attributeMutation(menuButton, "data-state")]);
  assert.equal(rowGroup.hasAttribute("data-codey-open-within"), false);
  assert.equal(buttonContainer.hasAttribute("data-codey-open-within"), false);

  const insertedWrapper = new FakeElement("div");
  const nestedButtonHost = insertedWrapper.appendChild(new FakeElement("div", {
    attributes: { class: "has-[button[data-state=open]]:opacity-100" },
  }));
  const nestedOpenButton = nestedButtonHost.appendChild(new FakeElement("button", {
    attributes: { "data-state": "open" },
  }));
  rowGroup.appendChild(insertedWrapper);
  runtime.bootstrap.handler([childListMutation(rowGroup, [insertedWrapper], [])]);
  assert.equal(rowGroup.getAttribute("data-codey-open-within"), "");
  assert.equal(nestedButtonHost.getAttribute("data-codey-open-within"), "");
  assert.equal(nestedOpenButton.getAttribute("data-state"), "open");

  insertedWrapper.remove();
  runtime.bootstrap.handler([childListMutation(rowGroup, [], [insertedWrapper])]);
  assert.equal(rowGroup.hasAttribute("data-codey-open-within"), false);
  assert.equal(nestedButtonHost.hasAttribute("data-codey-open-within"), false);
});

test("runtime stylesheet fallback rewrites unpatched link stylesheets and skips pre-patched ones", () => {
  const head = new FakeElement("head", { connected: true });
  const unpatchedLink = head.appendChild(new FakeElement("link", {
    attributes: {
      rel: "stylesheet",
      href: "app://-/assets/async/e77f816560c6.css",
    },
  }));
  const prePatchedLink = head.appendChild(new FakeElement("link", {
    attributes: {
      rel: "stylesheet",
      href: "app://-/assets/async/166572aa20c2.css",
    },
  }));
  const unpatchedSheet = {
    ownerNode: unpatchedLink,
    disabled: false,
    cssRules: [
      {
        selectorText: ".group-has-\\[\\[data-state\\=open\\]\\]\\:opacity-100:is(:where(.group):has([data-state=\"open\"]) *)",
        cssText: ".group-has-\\[\\[data-state\\=open\\]\\]\\:opacity-100:is(:where(.group):has([data-state=\"open\"]) *) { opacity: 1; }",
      },
      {
        selectorText: ".group-has-\\[\\[aria-expanded\\=true\\]\\,\\[data-state\\=open\\]\\]\\:opacity-100:is(:where(.group):has([aria-expanded=\"true\"], [data-state=\"open\"]) *)",
        cssText: ".group-has-\\[\\[aria-expanded\\=true\\]\\,\\[data-state\\=open\\]\\]\\:opacity-100:is(:where(.group):has([aria-expanded=\"true\"], [data-state=\"open\"]) *) { opacity: 1; }",
      },
    ],
  };
  const prePatchedSheet = {
    ownerNode: prePatchedLink,
    disabled: false,
    cssRules: [
      {
        selectorText: ":where(.card-ineZLT)[data-codey-open-within]",
        cssText: ":where(.card-ineZLT)[data-codey-open-within] { opacity: 1 !important; }",
      },
      {
        selectorText: ":root",
        cssText: ":root { --codey-css-open-state-has-patched: 1; }",
      },
    ],
  };

  createRuntime({ styleSheets: [unpatchedSheet, prePatchedSheet] });

  assert.equal(unpatchedSheet.disabled, true);
  assert.equal(unpatchedLink.parentElement, null);
  assert.equal(prePatchedSheet.disabled, false);
  assert.equal(prePatchedLink.getAttribute("data-codey-open-state-patched"), "true");
  const replacementStyle = head.children.find((child) => child.tagName === "STYLE");
  assert.ok(replacementStyle, "unpatched <link> should be replaced with a <style>");
  assert.equal(replacementStyle.getAttribute("data-href"), "app://-/assets/async/e77f816560c6.css");
  assert.doesNotMatch(replacementStyle.textContent, /:has\([^)]*data-state/);
  assert.match(replacementStyle.textContent, /:where\(\.group\)\[data-codey-open-within\]/);
  assert.match(
    replacementStyle.textContent,
    /:where\(\.group\):is\(:has\(\[aria-expanded=true\]\),\[data-codey-open-within\]\)/,
  );
});
