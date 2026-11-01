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
  constructor(
    text = "",
    isControl = true,
    children = [],
    attributes = {},
  ) {
    super("div", { attributes });
    delete this.isConnected;
    this.textContent = text;
    this.children = children;
    this.isControl = isControl;
    children.forEach((child) => {
      child.parentElement = this;
    });
  }

  closest(selector) {
    const selectors = String(selector).split(",").map((part) => part.trim());
    let current = this;
    while (current) {
      if (selectors.some((candidate) => current.matches(candidate))) return current;
      current = current.parentElement;
    }
    return null;
  }

  matches(selector) {
    return super.matches(selector) || (
      this.isControl
        && String(selector).split(",").some((part) => {
          const candidate = part.trim();
          return candidate === "button" || candidate.endsWith("[role=button]");
        })
    );
  }

  querySelectorAll(selector) {
    const selectors = String(selector).split(",").map((part) => part.trim());
    const controls = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (selectors.some((candidate) => child.matches(candidate))) {
          controls.push(child);
        }
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
  const semanticMini = new FakeElement();
  semanticMini.__reactProps$test = {
    children: { props: { id: "codex.command.showMiniOverlay" } },
  };
  const navMini = new FakeElement();
  navMini.__reactProps$test = {
    children: { props: { id: "settings.nav.miniAndPets" } },
  };
  const localizedMini = new FakeElement("显示 Mini");
  const englishMini = new FakeElement("Show mini");
  const modelOption = new FakeElement("GPT-5.4-Mini");
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
    semanticMini,
    navMini,
    localizedMini,
    englishMini,
    modelOption,
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
  documentElement.isConnected = true;
  const document = {
    documentElement,
    querySelectorAll: () => {
      throw new Error("pet scans must not materialise the full control list");
    },
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: (name) => listeners.delete(name),
  };
  const window = {};
  window.window = window;
  const pendingTimers = new Map();
  controls.forEach((control) => {
    if (!control.parentElement && control.isControl) {
      control.parentElement = documentElement;
      documentElement.children.push(control);
    }
  });

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
    englishMini,
    get observerDisconnected() {
      return observerDisconnected;
    },
    listeners,
    localized,
    localizedMini,
    localizedSettingsMenu,
    modelOption,
    mutationCallback,
    navMini,
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
    runAllAnimationFrames: () => {
      let guard = 0;
      while (pendingAnimationFrames.size && guard < 100) {
        runPendingAnimationFrames();
        guard += 1;
      }
    },
    semantic,
    semanticMini,
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

test("pet slim mode blocks the renamed mini entry points without hiding models", () => {
  const runtime = loadShield(true);

  assert.equal(
    runtime.semanticMini.getAttribute("data-codey-pet-control-blocked"),
    "true",
  );
  assert.equal(
    runtime.navMini.getAttribute("data-codey-pet-control-blocked"),
    "true",
  );
  assert.equal(
    runtime.localizedMini.getAttribute("data-codey-pet-control-blocked"),
    "true",
  );
  assert.equal(
    runtime.englishMini.getAttribute("data-codey-pet-control-blocked"),
    "true",
  );
  assert.equal(
    runtime.modelOption.getAttribute("data-codey-pet-control-blocked"),
    null,
    "a model option must never match the mini entry point labels",
  );
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
  assert.equal(runtime.localizedMini.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(runtime.semanticMini.getAttribute("data-codey-pet-control-blocked"), null);
  assert.equal(
    runtime.window.__codeyMutationDispatcher.snapshot().subscriberCount,
    0,
  );
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

test("turn-external mutation batches coalesce into a single deferred sweep", () => {
  const runtime = loadShield(true);
  const flushesAfterLoad = runtime.scheduledFlushes;
  const dynamics = Array.from(
    { length: 12 },
    (_, index) => {
      const node = new FakeElement(
        "节点" + index,
        false,
        [],
        { "data-codey-critical-context": "" },
      );
      for (let childIndex = 0; childIndex < 70; childIndex += 1) {
        node.appendChild(new FakeElement("子节点" + childIndex, false));
      }
      return node;
    },
  );

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

test("streaming child insertions inside a mounted turn are skipped", () => {
  const runtime = loadShield(true);
  const turn = new FakeElement("", false, [], { "data-turn-key": "turn-1" });
  const stream = new FakeElement("普通流式节点");
  const before = runtime.scheduledFlushes;

  runtime.mutationCallback([{
    addedNodes: [stream],
    target: turn,
    type: "childList",
  }]);

  assert.equal(runtime.scheduledFlushes, before);
  assert.equal(runtime.pendingFlushCount, 0);
});

test("direct pet buttons inserted in a mounted turn remain immediately eligible", () => {
  const runtime = loadShield(true);
  const turn = new FakeElement("", false, [], { "data-turn-key": "turn-1" });
  const pet = new FakeElement("显示宠物");

  runtime.mutationCallback([{
    addedNodes: [pet],
    target: turn,
    type: "childList",
  }]);

  assert.equal(pet.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(runtime.pendingFlushCount, 0);
});

test("dialog and portal controls nested in a mounted turn remain eligible", () => {
  const runtime = loadShield(true);
  const turn = new FakeElement("", false, [], { "data-turn-key": "turn-1" });
  const dialog = new FakeElement("", false, [], { role: "dialog" });
  const dialogControl = new FakeElement("显示宠物");
  dialog.appendChild(dialogControl);
  const portal = new FakeElement("", false, [], { "data-radix-portal": "true" });
  const portalControl = new FakeElement("显示宠物");
  portal.appendChild(portalControl);
  turn.appendChild(dialog);
  turn.appendChild(portal);

  runtime.mutationCallback([
    { addedNodes: [dialogControl], target: dialog, type: "childList" },
    { addedNodes: [portal], target: turn, type: "childList" },
  ]);

  assert.equal(dialogControl.getAttribute("data-codey-pet-control-blocked"), "true");
  assert.equal(portalControl.getAttribute("data-codey-pet-control-blocked"), "true");
});

test("overlay, composer, and sidebar containers stay eligible inside a turn", () => {
  const runtime = loadShield(true);
  const turn = new FakeElement("", false, [], { "data-turn-key": "turn-1" });
  const contexts = [
    { role: "alertdialog" },
    { "aria-modal": "true" },
    { "data-testid": "modal" },
    { "data-testid": "composer" },
    { "data-testid": "sidebar" },
    { "data-radix-portal": "true" },
  ].map((attributes) => {
    const container = new FakeElement("", false, [], attributes);
    const control = new FakeElement("显示宠物");
    container.appendChild(control);
    turn.appendChild(container);
    return { container, control };
  });

  runtime.mutationCallback(contexts.map(({ container, control }) => ({
    addedNodes: [control],
    target: container,
    type: "childList",
  })));

  contexts.forEach(({ control }) => {
    assert.equal(control.getAttribute("data-codey-pet-control-blocked"), "true");
  });
});

test("deferred pet scans have a node budget and reach the trailing pet", () => {
  const runtime = loadShield(true);
  const menu = new FakeElement("", false);
  for (let index = 0; index < 320; index += 1) {
    menu.appendChild(new FakeElement("普通入口" + index, false));
  }
  const latePet = new FakeElement("显示宠物", true);
  menu.appendChild(latePet);
  const flushesBefore = runtime.scheduledFlushes;
  runtime.mutationCallback([{
    addedNodes: [menu],
    target: runtime.documentElement,
    type: "childList",
  }]);
  assert.equal(runtime.scheduledFlushes - flushesBefore, 1);
  runtime.runPendingAnimationFrames();
  assert.equal(runtime.scheduledFlushes - flushesBefore, 2);
  assert.equal(latePet.getAttribute("data-codey-pet-control-blocked"), null);

  runtime.runAllAnimationFrames();
  assert.equal(runtime.pendingFlushCount, 0);
  assert.equal(latePet.getAttribute("data-codey-pet-control-blocked"), "true");
});

test("pet shield cleanup disconnects the insertion observer", () => {
  const runtime = loadShield(true);

  runtime.window.__codeyPetControlShieldCleanup();

  assert.equal(runtime.observerDisconnected, true);
  assert.equal(runtime.listeners.size, 0);
});
