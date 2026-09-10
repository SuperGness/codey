import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const template = readFileSync(
  new URL("../public/pet-control-shield.js", import.meta.url),
  "utf8",
);
const bridgeTemplate = readFileSync(
  new URL("../public/codey-bridge.js", import.meta.url),
  "utf8",
);

class FakeElement extends FakeElementCore {
  constructor(text = "", isControl = true, children = []) {
    super();
    delete this.isConnected;
    this.textContent = text;
    this.children = children;
    this.isControl = isControl;
    children.forEach((child) => {
      child.parentElement = this;
    });
  }

  closest() {
    return this.isControl ? this : null;
  }

  contains() {
    return false;
  }

  matches() {
    return this.isControl;
  }

  querySelectorAll() {
    const controls = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (child.isControl) controls.push(child);
        visit(child);
      }
    };
    visit(this);
    return controls;
  }
}

function loadShield(enabled) {
  const semantic = new FakeElement();
  semantic.__reactProps$test = {
    children: { props: { id: "settings.personalization.pets.openPet" } },
  };
  const settingsMenu = new FakeElement("外观设置");
  settingsMenu.__reactProps$test = {
    children: { props: { id: "settings.appearance.pets.title" } },
  };
  const nestedSettingsMenu = new FakeElement("设置分区");
  nestedSettingsMenu.__reactProps$test = {
    children: { props: { id: "settings.nav.pets.title" } },
  };
  const localizedSettingsMenu = new FakeElement("宠物");
  const localized = new FakeElement("唤醒宠物");
  const sharedAvatarControl = new FakeElement();
  sharedAvatarControl.__reactProps$test = {
    children: { props: { id: "openAvatarOverlay" } },
  };
  const unrelated = new FakeElement("打开设置");
  const controls = [
    semantic,
    settingsMenu,
    nestedSettingsMenu,
    localizedSettingsMenu,
    localized,
    sharedAvatarControl,
    unrelated,
  ];
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
  const documentElement = new FakeElement("", false);
  const document = {
    documentElement,
    querySelectorAll: () => controls,
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const window = {};
  window.window = window;
  const pendingTimers = new Map();
  const pendingAnimationFrames = new Map();
  let nextTimerId = 1;
  let nextAnimationFrameId = 1;
  let scheduledFlushes = 0;
  window.setTimeout = (callback) => {
    const id = nextTimerId;
    nextTimerId += 1;
    scheduledFlushes += 1;
    pendingTimers.set(id, callback);
    return id;
  };
  window.clearTimeout = (id) => {
    pendingTimers.delete(id);
  };
  window.requestAnimationFrame = (callback) => {
    const id = nextAnimationFrameId;
    nextAnimationFrameId += 1;
    scheduledFlushes += 1;
    pendingAnimationFrames.set(id, callback);
    return id;
  };
  window.cancelAnimationFrame = (id) => {
    pendingAnimationFrames.delete(id);
  };
  const runPendingAnimationFrames = () => {
    const callbacks = [...pendingAnimationFrames.values()];
    pendingAnimationFrames.clear();
    callbacks.forEach((callback) => callback());
  };
  const runPendingTimers = () => {
    const callbacks = [...pendingTimers.values()];
    pendingTimers.clear();
    callbacks.forEach((callback) => callback());
  };
  const sandbox = {
    document,
    Element: FakeElement,
    HTMLElement: FakeElement,
    MutationObserver: FakeMutationObserver,
    WeakMap,
    window,
  };
  vm.runInNewContext(bridgeTemplate, sandbox);
  vm.runInNewContext(
    template.replace("__CODEY_SLIM_PET__", enabled ? "true" : "false"),
    sandbox,
  );
  return {
    documentElement,
    get observerDisconnected() {
      return observerDisconnected;
    },
    listeners,
    localized,
    localizedSettingsMenu,
    mutationCallback,
    nestedSettingsMenu,
    observerOptions,
    get pendingTimerCount() {
      return pendingTimers.size;
    },
    get pendingAnimationFrameCount() {
      return pendingAnimationFrames.size;
    },
    get pendingFlushCount() {
      return pendingTimers.size + pendingAnimationFrames.size;
    },
    runPendingAnimationFrames,
    runPendingTimers,
    get scheduledFlushes() {
      return scheduledFlushes;
    },
    semantic,
    sharedAvatarControl,
    settingsMenu,
    unrelated,
    window,
  };
}

test("pet slim mode blocks semantic and localized native pet controls", () => {
  const runtime = loadShield(true);

  assert.equal(runtime.semantic.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(runtime.settingsMenu.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(
    runtime.nestedSettingsMenu.getAttribute("data-codey-pet-control-blocked"),
    "true",
  );
  assert.equal(
    runtime.localizedSettingsMenu.getAttribute("data-codey-pet-control-blocked"),
    "true",
  );
  assert.equal(runtime.localized.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(runtime.semantic.disabled, true);
  assert.equal(runtime.semantic.style.display, "none:important");
  assert.equal(
    runtime.sharedAvatarControl.getAttribute("data-codey-pet-control-blocked"),
    null,
  );
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

test("pet slim mode stops the current settings menu before lazy pet resources load", () => {
  const runtime = loadShield(true);
  let petResourceLoads = 0;
  let stopped = false;
  runtime.settingsMenu.activate = () => {
    petResourceLoads += 1;
  };

  runtime.listeners.get("click")({
    target: runtime.settingsMenu,
    preventDefault: () => {},
    stopPropagation: () => { stopped = true; },
    stopImmediatePropagation: () => { stopped = true; },
  });
  if (!stopped) runtime.settingsMenu.activate();

  assert.equal(stopped, true);
  assert.equal(petResourceLoads, 0);
});

test("disabling pet slim mode restores native pet controls", () => {
  const runtime = loadShield(false);

  assert.equal(runtime.window.__codeyPetControlShield.enabled, false);
  assert.equal(runtime.semantic.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(runtime.settingsMenu.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(runtime.localized.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(runtime.mutationCallback, null);
  assert.equal(runtime.window.__codeyBlockNativePetControls(), 0);
});

test("pet slim mode blocks inserted menu controls before a deferred flush", () => {
  const runtime = loadShield(true);
  const dynamic = new FakeElement("显示宠物");
  const menu = new FakeElement("", false, [dynamic]);

  runtime.mutationCallback([{
    addedNodes: [menu],
    target: runtime.documentElement,
    type: "childList",
  }]);

  assert.equal(dynamic.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(dynamic.getAttribute("aria-hidden"), "true");
  assert.equal(dynamic.getAttribute("inert"), "");
  assert.equal(dynamic.style.display, "none:important");
  assert.equal(dynamic.disabled, true);
  assert.equal(runtime.pendingFlushCount, 0);
  assert.equal(runtime.observerOptions.attributes, true);
  assert.deepEqual([...runtime.observerOptions.attributeFilter], ["aria-label", "role", "title"]);
  assert.equal(runtime.observerOptions.childList, true);
  assert.equal(runtime.observerOptions.subtree, true);
});

test("streaming mutation batches coalesce into a single deferred sweep", () => {
  const runtime = loadShield(true);
  const flushesAfterLoad = runtime.scheduledFlushes;
  const dynamics = Array.from({ length: 12 }, (_, index) => new FakeElement(`节点${index}`));

  dynamics.forEach((node) => {
    runtime.mutationCallback([{
      addedNodes: [node],
      target: runtime.documentElement,
      type: "childList",
    }]);
  });

  assert.equal(
    runtime.scheduledFlushes - flushesAfterLoad,
    1,
    "a sustained mutation stream must not schedule one flush per batch",
  );
  assert.equal(runtime.pendingFlushCount, 1);
  assert.equal(runtime.pendingTimerCount, 0);
  assert.equal(runtime.pendingAnimationFrameCount, 1);

  runtime.runPendingAnimationFrames();
  assert.equal(runtime.pendingFlushCount, 0);
});

test("pet control verdicts are re-evaluated when React repurposes the element", () => {
  const runtime = loadShield(true);
  // Plain label so the cheap text heuristic cannot short-circuit the fiber walk.
  const control = new FakeElement("打开设置");
  let fiberProps = { children: { props: { id: "codex.command.somethingElse" } } };
  Object.defineProperty(control, "__reactProps$test", {
    configurable: true,
    enumerable: true,
    get() {
      return fiberProps;
    },
  });
  // React attaches a fiber key alongside the props key; the verdict reads both.
  Object.defineProperty(control, "__reactFiber$test", {
    configurable: true,
    enumerable: true,
    get() {
      return { memoizedProps: fiberProps };
    },
  });

  const evaluate = () => runtime.window.__codeyPetControlShield.isPetControl(control);
  assert.equal(evaluate(), false);

  // A stale verdict here would leave a live pet entry point reachable, so this
  // shield must never cache across a React update.
  fiberProps = { children: { props: { id: "codex.command.openPetOverlay" } } };
  assert.equal(evaluate(), true, "a repurposed control must produce a fresh verdict");

  fiberProps = { children: { props: { id: "codex.command.somethingElse" } } };
  assert.equal(evaluate(), false, "verdicts must also drop back when React swaps props again");
});

test("pet control text changes are picked up without a React update", () => {
  const runtime = loadShield(true);
  const control = new FakeElement("打开设置");

  assert.equal(runtime.window.__codeyPetControlShield.isPetControl(control), false);

  // The label heuristic must stay uncached: textContent changes with no React
  // prop update at all.
  control.textContent = "唤醒宠物";
  assert.equal(runtime.window.__codeyPetControlShield.isPetControl(control), true);
});

test("pet shield cleanup disconnects the insertion observer", () => {
  const runtime = loadShield(true);

  runtime.window.__codeyPetControlShieldCleanup();

  assert.equal(runtime.observerDisconnected, true);
  assert.equal(runtime.listeners.size, 0);
});
