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
  const document = {
    querySelectorAll: () => controls,
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const window = {};
  window.window = window;
  vm.runInNewContext(
    template.replace("__CODEY_SLIM_PET__", enabled ? "true" : "false"),
    { document, Element: FakeElement, HTMLElement: FakeElement, window },
  );
  return { listeners, localized, semantic, unrelated, window };
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
  assert.equal(runtime.window.__codeyBlockNativePetControls(), 0);
});
