import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const template = readFileSync(
  new URL("../public/pet-control-shield.js", import.meta.url),
  "utf8",
);

class FakeElement {
  constructor(text = "") {
    this.textContent = text;
    this.attributes = new Map();
    this.disabled = false;
    this.style = {
      setProperty: (name, value, priority) => {
        this.style[name] = `${value}:${priority}`;
      },
    };
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }

  closest() {
    return this;
  }

  matches() {
    return true;
  }
}

function loadShield(enabled) {
  const semantic = new FakeElement();
  semantic.__reactProps$test = {
    children: { props: { id: "settings.personalization.pets.openPet" } },
  };
  const localized = new FakeElement("唤醒宠物");
  const unrelated = new FakeElement("打开设置");
  const controls = [semantic, localized, unrelated];
  const listeners = new Map();
  let mutationCallback = null;
  let observerOptions = null;
  let observerDisconnected = false;
  class FakeMutationObserver {
    constructor(callback) {
      mutationCallback = callback;
    }

    observe(_target, options) {
      observerOptions = options;
    }

    disconnect() {
      observerDisconnected = true;
    }
  }
  const documentElement = new FakeElement();
  const document = {
    documentElement,
    querySelectorAll: () => controls,
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const window = {};
  window.window = window;
  vm.runInNewContext(
    template.replace("__CODEY_SLIM_PET__", enabled ? "true" : "false"),
    {
      document,
      Element: FakeElement,
      HTMLElement: FakeElement,
      MutationObserver: FakeMutationObserver,
      window,
    },
  );
  return {
    documentElement,
    get observerDisconnected() {
      return observerDisconnected;
    },
    listeners,
    localized,
    mutationCallback,
    observerOptions,
    semantic,
    unrelated,
    window,
  };
}

test("pet slim mode blocks semantic and localized native pet controls", () => {
  const runtime = loadShield(true);

  assert.equal(runtime.semantic.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(runtime.localized.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(runtime.semantic.disabled, true);
  assert.equal(runtime.semantic.style.display, "none:important");
  assert.equal(runtime.unrelated.getAttribute("data-codey-pet-control-blocked"), null);

  let prevented = false;
  let stopped = false;
  runtime.listeners.get("click")({
    target: runtime.semantic,
    preventDefault: () => { prevented = true; },
    stopPropagation: () => { stopped = true; },
    stopImmediatePropagation: () => {},
  });
  assert.equal(prevented, true);
  assert.equal(stopped, true);
});

test("disabling pet slim mode restores native pet controls", () => {
  const runtime = loadShield(false);

  assert.equal(runtime.window.__codeyPetControlShield.enabled, false);
  assert.equal(runtime.semantic.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(runtime.localized.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(runtime.mutationCallback, null);
  assert.equal(runtime.window.__codeyBlockNativePetControls(), 0);
});

test("pet slim mode blocks controls in the insertion observer callback", () => {
  const runtime = loadShield(true);
  const dynamic = new FakeElement("显示宠物");

  runtime.mutationCallback([{
    addedNodes: [dynamic],
    target: runtime.documentElement,
    type: "childList",
  }]);

  assert.equal(dynamic.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(dynamic.getAttribute("aria-hidden"), "true");
  assert.equal(dynamic.getAttribute("inert"), "");
  assert.equal(dynamic.style.display, "none:important");
  assert.equal(dynamic.disabled, true);
  assert.equal(runtime.observerOptions.attributes, true);
  assert.deepEqual([...runtime.observerOptions.attributeFilter], ["aria-label", "role", "title"]);
  assert.equal(runtime.observerOptions.childList, true);
  assert.equal(runtime.observerOptions.subtree, true);
});

test("pet shield cleanup disconnects the insertion observer", () => {
  const runtime = loadShield(true);

  runtime.window.__codeyPetControlShieldCleanup();

  assert.equal(runtime.observerDisconnected, true);
  assert.equal(runtime.listeners.size, 0);
});
