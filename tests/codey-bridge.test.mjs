import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(
  new URL("../public/codey-bridge.js", import.meta.url),
  "utf8",
);

function createRuntime({
  fetch = undefined,
  now = undefined,
  requestAnimationFrame = undefined,
  statsig = undefined,
  setTimeout = undefined,
} = {}) {
  const observers = [];
  class FakeHTMLElement {}
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      this.disconnects = 0;
      this.options = null;
      observers.push(this);
    }

    disconnect() {
      this.disconnects += 1;
    }

    observe(_target, options) {
      this.options = options;
    }
  }
  const document = {
    activeEventListeners: new Map(),
    addEventListener(name, listener) {
      this.activeEventListeners.set(name, listener);
    },
    documentElement: {},
    removeEventListener(name) {
      this.activeEventListeners.delete(name);
    },
  };
  const context = {
    Date,
    document,
    HTMLElement: FakeHTMLElement,
    MutationObserver: FakeMutationObserver,
    performance: now ? { now } : undefined,
    window: null,
  };
  const window = {
    __STATSIG__: statsig,
    fetch,
    requestAnimationFrame,
    setTimeout,
  };
  window.window = window;
  context.window = window;
  vm.runInNewContext(source, context);
  return {
    context,
    document,
    FakeHTMLElement,
    observers,
    source,
    window,
  };
}

function createLifecycleRuntime() {
  const animationFrames = new Map();
  let nextAnimationFrameId = 1;
  const runtime = createRuntime({
    requestAnimationFrame(callback) {
      const id = nextAnimationFrameId;
      nextAnimationFrameId += 1;
      animationFrames.set(id, callback);
      return id;
    },
  });
  runtime.window.cancelAnimationFrame = (id) => animationFrames.delete(id);
  runtime.document.documentElement = new runtime.FakeHTMLElement();
  runtime.document.documentElement.isConnected = true;
  runtime.document.documentElement.contains = () => false;
  runtime.document.documentElement.matches = () => false;
  runtime.document.documentElement.querySelector = () => null;

  const runNextFrame = () => {
    const [entry] = animationFrames.entries();
    assert.ok(entry, "expected a scheduled animation frame");
    const [id, callback] = entry;
    animationFrames.delete(id);
    callback();
  };
  const runAllFrames = (maximum = 100) => {
    let count = 0;
    while (animationFrames.size) {
      assert.ok(count < maximum, "animation frame processing did not settle");
      runNextFrame();
      count += 1;
    }
    return count;
  };
  return {
    ...runtime,
    animationFrames,
    runAllFrames,
    runNextFrame,
  };
}

function createMutationDispatchRuntime({ sourceTransform } = {}) {
  const animationFrames = new Map();
  const timeouts = new Map();
  let nextId = 1;
  let now = 0;
  const runtime = createRuntime({
    now: () => now,
    requestAnimationFrame(callback) {
      const id = nextId;
      nextId += 1;
      animationFrames.set(id, callback);
      return id;
    },
    setTimeout(callback) {
      const id = nextId;
      nextId += 1;
      timeouts.set(id, callback);
      return id;
    },
  });
  runtime.window.cancelAnimationFrame = (id) => animationFrames.delete(id);
  runtime.window.clearTimeout = (id) => timeouts.delete(id);
  runtime.document.documentElement = new runtime.FakeHTMLElement();
  runtime.document.documentElement.isConnected = true;
  runtime.document.documentElement.contains = () => false;
  runtime.document.documentElement.matches = () => false;
  runtime.document.documentElement.querySelector = () => null;

  const runNext = (callbacks) => {
    const [entry] = callbacks.entries();
    assert.ok(entry, "expected deferred mutation dispatch");
    const [id, callback] = entry;
    callbacks.delete(id);
    callback();
  };
  if (sourceTransform) {
    delete runtime.window.__codeyBridgeHelpersInstalled;
    delete runtime.window.__codeyMutationDispatcher;
    runtime.context.MutationObserver = class extends runtime.context.MutationObserver {
      constructor(callback) {
        super(callback);
        this.originalCallback = callback;
      }
    };
    vm.runInNewContext(sourceTransform(source), runtime.context);
  }
  return {
    ...runtime,
    advance(ms) {
      now += ms;
    },
    animationFrames,
    observe(mutations) {
      runtime.observers.at(-1).callback(mutations);
    },
    runAllAnimationFrames() {
      let count = 0;
      while (animationFrames.size || timeouts.size) {
        assert.ok(count < 100, "deferred mutation dispatch did not settle");
        if (animationFrames.size) runNext(animationFrames);
        else runNext(timeouts);
        count += 1;
      }
    },
    runNextAnimationFrame() {
      runNext(animationFrames);
    },
    runNextTimeout() {
      runNext(timeouts);
    },
    timeouts,
  };
}

test("mutation dispatcher unions subscriptions and tears down only when empty", () => {
  const runtime = createRuntime();
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  const unsubscribePet = dispatcher.subscribe(
    (mutations) => calls.push(["pet", ...mutations]),
    {
      attributes: true,
      attributeOldValue: true,
      attributeFilter: ["aria-label", "role", "title"],
      childList: true,
    },
  );
  const unsubscribeSecurity = dispatcher.subscribe(
    (mutations) => calls.push(["security", ...mutations]),
    { childList: true },
  );
  const unsubscribeAssets = dispatcher.subscribe(
    (mutations) => calls.push(["assets", ...mutations]),
    {
      attributes: true,
      attributeFilter: ["aria-label", "role", "title", "src"],
      childList: true,
    },
  );

  const activeObserver = runtime.observers.at(-1);
  assert.deepEqual(
    [...activeObserver.options.attributeFilter],
    ["aria-label", "role", "title", "src"],
  );
  assert.equal(activeObserver.options.attributes, true);
  assert.equal(activeObserver.options.attributeOldValue, true);
  assert.equal(activeObserver.options.childList, true);
  assert.equal(activeObserver.options.subtree, true);
  assert.equal(dispatcher.snapshot().observerInstalled, true);
  assert.equal(dispatcher.snapshot().subscriberCount, 3);

  activeObserver.callback(["mutation"]);
  assert.deepEqual(calls, [
    ["pet", "mutation"],
    ["security", "mutation"],
    ["assets", "mutation"],
  ]);

  // Records are routed by each subscriber's own options: the childList-only
  // subscriber never sees attribute records, attribute subscribers only see
  // attributes in their filter, and subscribers with nothing to see are skipped.
  calls.length = 0;
  const classFlip = { type: "attributes", attributeName: "class" };
  const srcFlip = { type: "attributes", attributeName: "src" };
  const added = { type: "childList" };
  activeObserver.callback([classFlip, srcFlip, added]);
  assert.deepEqual(calls, [
    ["pet", added],
    ["security", added],
    ["assets", srcFlip, added],
  ]);
  calls.length = 0;
  activeObserver.callback([classFlip, added, srcFlip]);
  assert.deepEqual(calls, [
    ["pet", added],
    ["security", added],
    ["assets", added, srcFlip],
  ]);
  calls.length = 0;
  activeObserver.callback([classFlip]);
  assert.deepEqual(calls, []);

  calls.length = 0;
  const firstUnknown = { type: "characterData" };
  const secondUnknown = { type: "records" };
  activeObserver.callback([srcFlip, firstUnknown, classFlip, secondUnknown, added]);
  assert.deepEqual(calls, [
    ["pet", firstUnknown, secondUnknown, added],
    ["security", firstUnknown, secondUnknown, added],
    ["assets", srcFlip, firstUnknown, secondUnknown, added],
  ]);

  unsubscribeAssets();
  unsubscribeAssets();
  assert.deepEqual(
    [...runtime.observers.at(-1).options.attributeFilter],
    ["aria-label", "role", "title"],
  );
  assert.equal(dispatcher.snapshot().subscriberCount, 2);

  unsubscribePet();
  assert.equal(dispatcher.snapshot().subscriberCount, 1);
  assert.equal(runtime.observers.at(-1).options.childList, true);
  assert.equal(runtime.observers.at(-1).options.attributes, false);

  const finalObserver = runtime.observers.at(-1);
  unsubscribeSecurity();
  assert.equal(finalObserver.disconnects, 1);
  assert.equal(dispatcher.snapshot().observerInstalled, false);
  assert.equal(dispatcher.snapshot().subscriberCount, 0);
});

test("attribute filters use set membership and preserve record order within a linear dispatch", () => {
  class CountingSet extends Set {
    static checks = 0;

    has(value) {
      CountingSet.checks += 1;
      return super.has(value);
    }
  }
  const runtimeSource = source.replace(
    "? new Set(options.attributeFilter.map(String))",
    "? new CountingSet(options.attributeFilter.map(String))",
  );
  const observers = [];
  class FakeHTMLElement {}
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      observers.push(this);
    }

    disconnect() {}

    observe(_target, options) {
      this.options = options;
    }
  }
  const runtime = createRuntime();
  const context = {
    CountingSet,
    document: runtime.document,
    HTMLElement: FakeHTMLElement,
    MutationObserver: FakeMutationObserver,
    window: runtime.window,
  };
  delete runtime.window.__codeyBridgeHelpersInstalled;
  delete runtime.window.__codeyMutationDispatcher;
  vm.runInNewContext(runtimeSource, context);
  const dispatcher = context.window.__codeyMutationDispatcher;
  const subscriberOrder = [];
  dispatcher.subscribe(() => subscriberOrder.push("first"), {
    attributes: true,
    attributeFilter: ["first", "second"],
  });
  dispatcher.subscribe(() => subscriberOrder.push("second"), {
    attributes: true,
    attributeFilter: ["second"],
  });
  const records = Array.from({ length: 40 }, (_, index) => ({
    id: index,
    type: index % 3 === 0 ? "childList" : "attributes",
    attributeName: index % 2 === 0 ? "second" : "first",
  }));
  const deliveries = [];
  dispatcher.subscribe((mutations) => {
    subscriberOrder.push("observed");
    deliveries.push(mutations.map(({ id }) => id));
  }, {
    attributes: true,
    childList: true,
    attributeFilter: ["first", "second"],
  });

  observers.at(-1).callback(records);

  assert.deepEqual(subscriberOrder, ["first", "second", "observed"]);
  assert.deepEqual(deliveries, [records.map(({ id }) => id)]);
  const attributeRecordCount = records.filter(({ type }) => type === "attributes").length;
  assert.equal(
    CountingSet.checks,
    attributeRecordCount * 3,
    "attribute filters should use three set lookups per record, independent of filter size",
  );
});

test("mutation dispatcher defers later subscribers and preserves batch order across frames", () => {
  const runtime = createMutationDispatchRuntime({
    sourceTransform: (bridgeSource) => bridgeSource
      .replace(
        "const maxMutationSubscribersPerFrame = 16;",
        "const maxMutationSubscribersPerFrame = 1;",
      )
      .replace("const mutationDispatchBudgetMs = 8;", "const mutationDispatchBudgetMs = Infinity;"),
  });
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  const calls = [];
  runtime.advance(20);
  dispatcher.subscribe((records) => calls.push(["first", ...records]), {
    attributes: true,
    childList: true,
  });
  dispatcher.subscribe((records) => calls.push(["second", ...records]), {
    attributes: true,
    childList: true,
  });
  dispatcher.subscribe((records) => calls.push(["third", ...records]), {
    attributes: true,
    childList: true,
  });
  const firstBatch = [{ id: 1, type: "childList" }];
  const secondBatch = [{ id: 2, type: "childList" }];
  runtime.observe(firstBatch);
  runtime.advance(20);
  runtime.observe(secondBatch);
  assert.deepEqual(calls, [
    ["first", firstBatch[0]],
  ]);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, [
    ["first", firstBatch[0]],
    ["second", firstBatch[0]],
  ]);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, [
    ["first", firstBatch[0]],
    ["second", firstBatch[0]],
    ["third", firstBatch[0]],
  ]);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, [
    ["first", firstBatch[0]],
    ["second", firstBatch[0]],
    ["third", firstBatch[0]],
    ["first", secondBatch[0]],
  ]);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, [
    ["first", firstBatch[0]],
    ["second", firstBatch[0]],
    ["third", firstBatch[0]],
    ["first", secondBatch[0]],
    ["second", secondBatch[0]],
  ]);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, [
    ["first", firstBatch[0]],
    ["second", firstBatch[0]],
    ["third", firstBatch[0]],
    ["first", secondBatch[0]],
    ["second", secondBatch[0]],
    ["third", secondBatch[0]],
  ]);
  assert.equal(runtime.animationFrames.size, 0);
});

test("mutation dispatcher uses a zero-delay timer when rAF is unavailable", () => {
  const runtime = createMutationDispatchRuntime({
    sourceTransform: (bridgeSource) => bridgeSource
      .replace(
        "const maxMutationSubscribersPerFrame = 16;",
        "const maxMutationSubscribersPerFrame = 1;",
      )
      .replace("const mutationDispatchBudgetMs = 8;", "const mutationDispatchBudgetMs = Infinity;"),
  });
  runtime.window.requestAnimationFrame = undefined;
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  dispatcher.subscribe(() => calls.push("first"), { childList: true });
  dispatcher.subscribe(() => calls.push("second"), { childList: true });
  runtime.observe([{ id: 1, type: "childList" }]);
  assert.equal(runtime.timeouts.size, 1);
  runtime.advance(20);
  runtime.runNextTimeout();
  assert.deepEqual(calls, ["first", "second"]);
});

test("mutation dispatcher stops dispatching when the synchronous time budget is exhausted", () => {
  const runtime = createMutationDispatchRuntime();
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  dispatcher.subscribe(() => {
    calls.push("first");
    runtime.advance(9);
  }, { childList: true });
  dispatcher.subscribe(() => calls.push("second"), { childList: true });
  runtime.observe([{ id: 1, type: "childList" }]);

  assert.deepEqual(calls, ["first"]);
  assert.equal(runtime.animationFrames.size, 1);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, ["first", "second"]);
});

test("mutation dispatcher skips removed subscribers in a deferred batch", () => {
  const runtime = createMutationDispatchRuntime({
    sourceTransform: (bridgeSource) => bridgeSource
      .replace(
        "const maxMutationSubscribersPerFrame = 16;",
        "const maxMutationSubscribersPerFrame = 1;",
      )
      .replace("const mutationDispatchBudgetMs = 8;", "const mutationDispatchBudgetMs = Infinity;"),
  });
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  dispatcher.subscribe(() => calls.push("first"), { childList: true });
  const unsubscribeSecond = dispatcher.subscribe(
    (records) => calls.push(["second", ...records]),
    { childList: true },
  );
  const records = [{ id: 1, type: "childList" }];
  runtime.observe(records);
  unsubscribeSecond();
  runtime.advance(20);
  runtime.runNextAnimationFrame();

  assert.deepEqual(calls, ["first"]);
  assert.equal(dispatcher.snapshot().subscriberCount, 1);
});

test("removing a queued direct subscriber skips its callback", () => {
  const runtime = createMutationDispatchRuntime({
    sourceTransform: (bridgeSource) => bridgeSource
      .replace(
        "const maxMutationSubscribersPerFrame = 16;",
        "const maxMutationSubscribersPerFrame = 1;",
      )
      .replace("const mutationDispatchBudgetMs = 8;", "const mutationDispatchBudgetMs = Infinity;"),
  });
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  const records = [{ id: 1, type: "childList" }];
  dispatcher.subscribe(() => calls.push("first"), { childList: true });
  const unsubscribeDirect = dispatcher.subscribe(
    () => calls.push("direct"),
    {
      attributes: true,
      requiresSynchronousDelivery: () => true,
    },
  );
  dispatcher.subscribe(() => calls.push("last"), { childList: true });
  runtime.observe(records);
  unsubscribeDirect();
  runtime.runAllAnimationFrames();

  assert.deepEqual(calls, ["first", "last"]);
  assert.equal(dispatcher.snapshot().subscriberCount, 2);
});

test("mutation dispatcher keeps unknown records fail-open and delivery objects intact", () => {
  const runtime = createMutationDispatchRuntime({
    sourceTransform: (bridgeSource) => bridgeSource
      .replace(
        "const maxMutationSubscribersPerFrame = 16;",
        "const maxMutationSubscribersPerFrame = 1;",
      )
      .replace("const mutationDispatchBudgetMs = 8;", "const mutationDispatchBudgetMs = Infinity;"),
  });
  const calls = [];
  const unknown = { type: "characterData" };
  const anonymous = {};
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  dispatcher.subscribe((records) => calls.push(["first", ...records]), {
    childList: true,
  });
  dispatcher.subscribe((records) => calls.push(["second", ...records]), {
    attributes: true,
  });
  runtime.observe([unknown, anonymous]);
  runtime.advance(20);
  runtime.runNextAnimationFrame();

  assert.deepEqual(calls, [
    ["first", unknown, anonymous],
    ["second", unknown, anonymous],
  ]);
  assert.equal(calls[0][1], unknown);
  assert.equal(calls[1][1], unknown);
});

test("a queued direct-control batch is promoted without reordering earlier batches", () => {
  const runtime = createMutationDispatchRuntime({
    sourceTransform: (bridgeSource) => bridgeSource
      .replace(
        "const maxMutationSubscribersPerFrame = 16;",
        "const maxMutationSubscribersPerFrame = 1;",
      )
      .replace("const mutationDispatchBudgetMs = 8;", "const mutationDispatchBudgetMs = Infinity;"),
  });
  const directControl = new runtime.FakeHTMLElement();
  directControl.isConnected = true;
  directControl.contains = () => false;
  directControl.closest = () => null;
  directControl.parentElement = null;
  directControl.matches = (selector) => selector === "button";
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  dispatcher.subscribe((records) => calls.push(["first", ...records]), {
    childList: true,
  });
  dispatcher.subscribe((records) => calls.push(["second", ...records]), {
    childList: true,
  });
  runtime.observe([{ id: 1, type: "childList" }]);
  dispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root) => {
      calls.push(["direct", root]);
      return 1;
    },
    eventSelector: "button",
    isControl: (root) => root === directControl,
    probeNodeLimit: 1,
  });
  runtime.advance(20);
  runtime.observe([{
    addedNodes: [directControl],
    target: runtime.document.documentElement,
    type: "childList",
  }]);

  assert.deepEqual(
    calls.filter(([name]) => name === "direct"),
    [["direct", directControl]],
  );
  assert.deepEqual(calls.at(-2), [
    "second",
    {
      addedNodes: [directControl],
      target: runtime.document.documentElement,
      type: "childList",
    },
  ]);
  assert.deepEqual(calls.at(-1), ["direct", directControl]);
  assert.equal(runtime.animationFrames.size, 1);
  runtime.runNextAnimationFrame();
  assert.deepEqual(
    calls.filter(([name]) => name === "direct"),
    [["direct", directControl]],
  );
});

test("direct control mutations remain synchronous when subscriber delivery is deferred", () => {
  const runtime = createMutationDispatchRuntime();
  const directControl = new runtime.FakeHTMLElement();
  directControl.isConnected = true;
  directControl.contains = () => false;
  directControl.closest = () => null;
  directControl.parentElement = null;
  directControl.matches = (selector) => selector === "button";
  const lifecycles = [];
  runtime.window.__codeyMutationDispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root) => {
      lifecycles.push(root);
      return 0;
    },
    eventSelector: "button",
    isControl: (root) => root === directControl,
    probeNodeLimit: 1,
  });
  runtime.observe([{
    addedNodes: [directControl],
    target: runtime.document.documentElement,
    type: "childList",
  }]);

  const directLifecycleIndex = lifecycles.indexOf(directControl);
  assert.notEqual(directLifecycleIndex, -1);
  assert.equal(lifecycles.at(-1), directControl);
});

test("direct controls stay synchronous after an earlier subscriber exhausts the budget", () => {
  const runtime = createMutationDispatchRuntime();
  const directControl = new runtime.FakeHTMLElement();
  directControl.isConnected = true;
  directControl.contains = () => false;
  directControl.closest = () => null;
  directControl.parentElement = null;
  directControl.matches = (selector) => selector === "button";
  const calls = [];
  const dispatcher = runtime.window.__codeyMutationDispatcher;
  dispatcher.subscribe(() => {
    calls.push("expensive");
    runtime.advance(9);
  }, { childList: true });
  dispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root) => {
      calls.push("direct");
      assert.equal(root, directControl);
      return 1;
    },
    eventSelector: "button",
    isControl: (root) => root === directControl,
    probeNodeLimit: 1,
  });
  dispatcher.subscribe(() => calls.push("tail"), { childList: true });

  runtime.observe([{
    addedNodes: [directControl],
    target: runtime.document.documentElement,
    type: "childList",
  }]);

  assert.deepEqual(calls, ["expensive", "direct"]);
  assert.equal(runtime.animationFrames.size, 1);
  runtime.runNextAnimationFrame();
  assert.deepEqual(calls, ["expensive", "direct", "tail"]);
});

test("shared control lookup includes a matching root and its descendants", () => {
  const runtime = createRuntime();
  const child = {};
  const root = new runtime.FakeHTMLElement();
  root.matches = (selector) => selector === "button";
  root.querySelectorAll = (selector) => selector === "button" ? [child] : [];

  const controls = runtime.window.__codeyMutationDispatcher.controlsWithin(root, "button");
  assert.equal(controls.length, 2);
  assert.equal(controls[0], root);
  assert.equal(controls[1], child);
});

test("shared control descriptor normalizes accessible labels and text", () => {
  const runtime = createRuntime();
  const control = {
    getAttribute(name) {
      return name === "aria-label" ? "  Open " : name === "title" ? "Settings" : null;
    },
    textContent: " now\nplease ",
  };

  assert.equal(
    runtime.window.__codeyMutationDispatcher.controlDescriptor(control),
    "Open Settings now please",
  );
});

test("shield lifecycle limits roots per frame and resumes in the next frame", () => {
  const runtime = createLifecycleRuntime();
  const scanned = [];
  runtime.window.__codeyMutationDispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root) => {
      scanned.push(root);
      return 0;
    },
    eventSelector: "button",
    frameRootLimit: 1,
    isControl: () => false,
    pendingRootLimit: 4,
  });
  const roots = Array.from({ length: 3 }, () => {
    const root = new runtime.FakeHTMLElement();
    root.isConnected = true;
    root.contains = () => false;
    root.matches = () => false;
    root.querySelector = () => null;
    return root;
  });

  runtime.observers.at(-1).callback(roots.map((root) => ({
    addedNodes: [root],
    target: {},
    type: "childList",
  })));
  assert.equal(runtime.animationFrames.size, 1);

  runtime.runNextFrame();
  assert.deepEqual(scanned, [roots[0]]);
  assert.equal(runtime.animationFrames.size, 1);

  runtime.runNextFrame();
  assert.deepEqual(scanned, roots.slice(0, 2));
  assert.equal(runtime.animationFrames.size, 1);

  runtime.runNextFrame();
  assert.deepEqual(scanned, roots);
  assert.equal(runtime.animationFrames.size, 0);
});

test("shield lifecycle keeps the newest roots when the pending set overflows", () => {
  const runtime = createLifecycleRuntime();
  const scanned = [];
  runtime.window.__codeyMutationDispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root) => {
      scanned.push(Number(root.id.slice(4)));
      return 0;
    },
    eventSelector: "button",
    frameRootLimit: 100,
    isControl: () => false,
    pendingRootLimit: 64,
  });
  const roots = Array.from({ length: 70 }, (_, index) => {
    const root = new runtime.FakeHTMLElement();
    root.id = "root" + index;
    root.isConnected = true;
    root.contains = () => false;
    root.matches = () => false;
    root.querySelector = () => null;
    return root;
  });

  runtime.observers.at(-1).callback(roots.map((root) => ({
    addedNodes: [root],
    target: {},
    type: "childList",
  })));
  runtime.runAllFrames();

  assert.deepEqual(scanned, roots.slice(6).map((_, index) => index + 6));
});

test("shield lifecycle bounds synchronous critical probes and defers overflow", () => {
  const runtime = createLifecycleRuntime();
  const criticalContextSelector = "[data-critical]";
  const syncProbes = [];
  const syncDirectControls = [];
  const deferredRoots = [];
  let inMutationCallback = false;
  runtime.window.__codeyMutationDispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root, options) => {
      if (inMutationCallback) {
        if (root.matches(criticalContextSelector)) {
          assert.equal(options.maxNodes, 1);
          syncProbes.push(root);
        } else {
          syncDirectControls.push(root);
        }
      } else {
        deferredRoots.push(root);
      }
      return 0;
    },
    criticalContextSelector,
    eventSelector: "button",
    frameRootLimit: 2,
    isControl: (root) => root.isDirectControl === true,
    nodeLimitPerRoot: 3,
    pendingRootLimit: 32,
    probeNodeLimit: 1,
  });
  const criticalRoots = Array.from({ length: 12 }, (_, index) => {
    const root = new runtime.FakeHTMLElement();
    root.id = "critical-" + index;
    root.isConnected = true;
    root.contains = () => false;
    root.closest = () => null;
    root.matches = (selector) => selector === criticalContextSelector;
    return root;
  });
  const directControl = new runtime.FakeHTMLElement();
  directControl.isConnected = true;
  directControl.isDirectControl = true;
  directControl.contains = () => false;
  directControl.closest = () => null;
  directControl.matches = (selector) => selector === "button";

  inMutationCallback = true;
  runtime.observers.at(-1).callback(
    [...criticalRoots, directControl].map((root) => ({
      addedNodes: [root],
      target: {},
      type: "childList",
    })),
  );
  inMutationCallback = false;

  assert.deepEqual(syncProbes, criticalRoots.slice(0, 8));
  assert.ok(syncProbes.length <= 8, "synchronous probes must stay bounded");
  assert.deepEqual(syncDirectControls, [directControl]);
  assert.equal(runtime.animationFrames.size, 1);

  runtime.runAllFrames();
  assert.deepEqual(deferredRoots, criticalRoots);
});

test("shield lifecycle resumes a root after its per-frame node budget", () => {
  const runtime = createLifecycleRuntime();
  const calls = [];
  runtime.window.__codeyMutationDispatcher.createShieldLifecycle({
    attributeFilter: [],
    block: (root, options) => {
      calls.push(options);
      if (options.startCursor) {
        options.onProgress?.({
          complete: true,
          nextIndex: 4,
          nodesVisited: 1,
        });
      } else {
        options.onProgress?.({
          complete: false,
          nextCursor: root,
          nodesVisited: 2,
        });
      }
      return 0;
    },
    eventSelector: "button",
    frameRootLimit: 1,
    isControl: () => false,
    nodeLimitPerRoot: 2,
    pendingRootLimit: 4,
  });
  const root = new runtime.FakeHTMLElement();
  root.isConnected = true;
  root.contains = () => false;
  root.matches = () => false;
  root.querySelector = () => null;

  runtime.observers.at(-1).callback([{
    addedNodes: [root],
    target: {},
    type: "childList",
  }]);
  runtime.runAllFrames();

  assert.deepEqual(
    calls.map(({ maxNodes, startCursor }) => ({ maxNodes, startCursor })),
    [
      { maxNodes: 2, startCursor: null },
      { maxNodes: 2, startCursor: root },
    ],
  );
});

test("shared helpers deduplicate Statsig clients and inspect React values", () => {
  const first = {};
  const second = {};
  const runtime = createRuntime({
    statsig: {
      firstInstance: first,
      instance: () => first,
      instances: { first, second },
    },
  });
  const control = new runtime.FakeHTMLElement();
  control.__reactProps$test = {
    children: { props: { id: "settings.personalization.pets" } },
  };
  const shared = runtime.window.__codeySharedRuntime;

  assert.deepEqual([...shared.statsigClients()], [first, second]);
  assert.equal(shared.reactInternals(control).length, 1);
  assert.equal(
    shared.objectGraphIncludes(
      shared.reactInternals(control)[0],
      (value) => value === "settings.personalization.pets",
      { ignoredKeys: new Set(["return"]), maxDepth: 7 },
    ),
    true,
  );
  assert.equal(
    shared.reactInternalGraphIncludes(
      control,
      (value) => value === "settings.personalization.pets",
    ),
    true,
  );
});

test("shared React graph walking preserves direct, ancestor, and container semantics", () => {
  const runtime = createRuntime();
  const shared = runtime.window.__codeySharedRuntime;
  const control = new runtime.FakeHTMLElement();
  control.__reactFiber$direct = {
    memoizedProps: { children: { commandId: "composer.openCommandMenu" } },
    return: null,
  };
  control.__reactContainer$root = { current: {} };
  assert.deepEqual(
    [...shared.reactInternalKeys(control, { includeContainer: true })],
    ["__reactFiber$direct", "__reactContainer$root"],
  );
  assert.equal(
    shared.reactInternalGraphIncludes(
      control,
      (value) => value === "composer.openCommandMenu",
    ),
    true,
  );

  const ancestorControl = new runtime.FakeHTMLElement();
  ancestorControl.__reactFiber$ancestor = {
    memoizedProps: {},
    return: {
      memoizedProps: { commandId: "settings.general.appearance" },
      return: null,
    },
  };
  assert.equal(
    shared.reactInternalGraphIncludes(
      ancestorControl,
      (value) => value === "settings.general.appearance",
      {
        ancestorDepth: 8,
        ancestorIgnoredKeys:
          new Set(["return", "child", "sibling", "stateNode", "_owner", "children"]),
      },
    ),
    true,
  );

  ancestorControl.__reactFiber$ancestor.return.memoizedProps = {
    children: { commandId: "settings.general.appearance" },
  };
  assert.equal(
    shared.reactInternalGraphIncludes(
      ancestorControl,
      (value) => value === "settings.general.appearance",
      {
        ancestorDepth: 8,
        ancestorIgnoredKeys:
          new Set(["return", "child", "sibling", "stateNode", "_owner", "children"]),
      },
    ),
    false,
  );
});

test("fetch interceptors share one stable wrapper and unregister independently", async () => {
  const nativeCalls = [];
  const nativeFetch = async (input) => {
    nativeCalls.push(input);
    return input;
  };
  const runtime = createRuntime({ fetch: nativeFetch });
  const shared = runtime.window.__codeySharedRuntime;
  const order = [];
  const unregisterInner = shared.registerFetchInterceptor(
    "inner",
    (next, input) => {
      order.push("inner");
      return next(`${input}:inner`);
    },
    10,
  );
  const wrapper = runtime.window.fetch;
  const unregisterOuter = shared.registerFetchInterceptor(
    "outer",
    (next, input) => {
      order.push("outer");
      return next(`${input}:outer`);
    },
    20,
  );

  assert.equal(runtime.window.fetch, wrapper);
  assert.equal(await runtime.window.fetch("request"), "request:outer:inner");
  assert.deepEqual(order, ["outer", "inner"]);
  assert.deepEqual(nativeCalls, ["request:outer:inner"]);
  assert.equal(shared.fetchSnapshot().interceptorCount, 2);

  unregisterOuter();
  assert.equal(runtime.window.fetch, wrapper);
  unregisterInner();
  assert.equal(runtime.window.fetch, nativeFetch);
  assert.equal(shared.fetchSnapshot().installed, false);
});
