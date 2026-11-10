import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement extends FakeElementCore {
  constructor(tag = "div", attributes = {}) {
    super(tag, { attributes, connected: true });
    this.className = "";
    const classes = new Set();
    this.classList = {
      add: (value) => classes.add(value),
      contains: (value) => classes.has(value),
      remove: (value) => classes.delete(value),
      toggle: (value, enabled) => {
        if (enabled === undefined) enabled = !classes.has(value);
        if (enabled) classes.add(value);
        else classes.delete(value);
        return enabled;
      },
    };
  }

  get previousElementSibling() {
    if (!this.parentElement) return null;
    const index = this.parentElement.children.indexOf(this);
    return index > 0 ? this.parentElement.children[index - 1] : null;
  }

  getBoundingClientRect() {
    return { top: 100, bottom: 124, left: 100, right: 124, width: 24, height: 24 };
  }

  getClientRects() {
    return [this.getBoundingClientRect()];
  }
}

function sourceWithBudget(name, value) {
  const pattern = new RegExp(`const ${name} = [^;]+;`);
  assert.match(source, pattern);
  return source.replace(pattern, `const ${name} = ${value};`);
}

function withSessionMutationTestHooks(runtimeSource) {
  const marker = "window.__codeySessionToolsInstall = {";
  assert.equal(runtimeSource.split(marker).length - 1, 1);
  return runtimeSource.replace(marker, `${marker}
    __codeyTestHooks: {
      deferredMutationIdentityIndex,
      enqueueMutationBatchTask,
      removeDeferredMutationTask,
    },`);
}

function fixture({ replacements = {}, sourceTransform = null } = {}) {
  let sequence = 0;
  const timeouts = new Map();
  const intervals = new Map();
  const idleCallbacks = new Map();
  const animationFrames = new Map();
  const microtasks = [];
  const observers = [];
  const document = new FakeElement("document");
  document.body = new FakeElement("body");
  document.documentElement = new FakeElement("html");
  document.appendChild(document.documentElement);
  document.documentElement.appendChild(document.body);
  document.visibilityState = "visible";
  document.scripts = [];
  document.createElement = (tag) => new FakeElement(tag);
  const placeholders = new Map();
  document.getElementById = (id) => {
    if (!placeholders.has(id)) placeholders.set(id, new FakeElement("div", { id }));
    return placeholders.get(id);
  };
  document.querySelectorAll = (selector) => (
    FakeElementCore.prototype.querySelectorAll.call(document, selector)
  );

  const window = new FakeElement("window");
  Object.assign(window, {
    addEventListener() {},
    cancelAnimationFrame: (id) => animationFrames.delete(id),
    cancelIdleCallback: (id) => idleCallbacks.delete(id),
    clearInterval: (id) => intervals.delete(id),
    clearTimeout: (id) => timeouts.delete(id),
    getComputedStyle: () => ({ display: "block", visibility: "visible" }),
    localStorage: { length: 0, key() {}, getItem() {}, setItem() {} },
    queueMicrotask(callback) {
      microtasks.push(callback);
    },
    removeEventListener() {},
    requestAnimationFrame: (callback) => {
      const id = ++sequence;
      animationFrames.set(id, callback);
      return id;
    },
    requestIdleCallback(callback) {
      const id = ++sequence;
      idleCallbacks.set(id, callback);
      return id;
    },
    setInterval(callback) {
      const id = ++sequence;
      intervals.set(id, callback);
      return id;
    },
    setTimeout(callback, delay = 0) {
      const id = ++sequence;
      timeouts.set(id, { callback, delay });
      return id;
    },
  });
  window.window = window;

  let runtimeSource = sourceTransform ? sourceTransform(source) : source;
  for (const [name, value] of Object.entries(replacements)) {
    runtimeSource = runtimeSource.replace(
      new RegExp(`const ${name} = [^;]+;`),
      `const ${name} = ${value};`,
    );
  }
  const context = vm.createContext({
    atob: (value) => Buffer.from(value, "base64").toString("binary"),
    btoa: (value) => Buffer.from(value, "binary").toString("base64"),
    console,
    document,
    Element: FakeElement,
    HTMLElement: FakeElement,
    location: { pathname: "/", search: "" },
    MutationObserver: class {
      constructor(callback) {
        this.callback = callback;
        this.connected = false;
        this.options = null;
        observers.push(this);
      }

      observe(_target, options) {
        this.connected = true;
        this.options = options;
      }

      disconnect() {
        this.connected = false;
      }
    },
    Node: { TEXT_NODE: 3 },
    performance: { now: () => 1_000 },
    TextEncoder,
    URL,
    URLSearchParams,
    window,
  });

  return {
    animationFrames,
    document,
    emit: (mutations) => observers[0]?.callback(mutations),
    load: () => vm.runInContext(runtimeSource, context),
    microtasks,
    observers,
    runTimeout(delay) {
      const entry = [...timeouts].find(([, timer]) => timer.delay === delay);
      assert.ok(entry, `missing timeout with delay ${delay}`);
      timeouts.delete(entry[0]);
      return entry[1].callback();
    },
    runAnimationFrame() {
      const entry = animationFrames.entries().next().value;
      assert.ok(entry, "missing animation frame");
      animationFrames.delete(entry[0]);
      return entry[1]();
    },
    timeouts,
    window,
  };
}

function drainDeferredMutations(runtime, afterEach = null) {
  let count = 0;
  while (runtime.animationFrames.size || runtime.microtasks.length) {
    const nextMicrotask = runtime.microtasks.shift();
    if (nextMicrotask) nextMicrotask();
    if (runtime.animationFrames.size) runtime.runAnimationFrame();
    if (!nextMicrotask && !runtime.animationFrames.size) break;
    afterEach?.();
    count += 1;
    assert.ok(count < 10_000, "deferred mutation flush did not settle");
  }
}

function mutation(target, { addedNodes = [], removedNodes = [] } = {}) {
  return { type: "childList", target, addedNodes, removedNodes };
}

function attributeMutation(target, attributeName = "data-testid") {
  return { type: "attributes", target, attributeName, oldValue: null };
}

function orderedTarget(label, events) {
  const target = new FakeElement("div");
  const closest = target.closest.bind(target);
  target.closest = (selector) => {
    if (selector === "[data-turn-key]") events.push(label);
    return closest(selector);
  };
  return target;
}

test("incremental scans retain overflow beyond the root budget", () => {
  const runtime = fixture({
    replacements: { maxIncrementalScanRootsPerFlush: 2 },
  });
  runtime.load();
  const roots = Array.from({ length: 5 }, (_, index) => new FakeElement("nav", {
    "data-app-action-sidebar-thread-id": `thread-${index}`,
  }));
  runtime.document.body.append(...roots);
  runtime.emit(roots.map((root) => mutation(runtime.document.body, { addedNodes: [root] })));

  runtime.runTimeout(60);
  let stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.scannedRoots, 2);
  assert.equal(stats.pending, 3);

  while (stats.pending) {
    runtime.runTimeout(60);
    stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  }
  stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.scannedRoots, roots.length);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("a zero time budget scans one root and leaves the rest queued", () => {
  const runtime = fixture({
    replacements: { maxIncrementalScanDurationMs: 0 },
  });
  runtime.load();
  const roots = Array.from({ length: 3 }, (_, index) => new FakeElement("nav", {
    "data-app-action-sidebar-thread-id": `timed-${index}`,
  }));
  runtime.document.body.append(...roots);
  runtime.emit(roots.map((root) => mutation(runtime.document.body, { addedNodes: [root] })));

  runtime.runTimeout(60);
  let stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.scannedRoots, 1);
  assert.equal(stats.pending, roots.length - 1);

  while (stats.pending) {
    runtime.runTimeout(60);
    stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  }
  assert.equal(stats.scannedRoots, roots.length);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("a mutation larger than the node budget is split without losing deferred nodes", () => {
  const runtime = fixture();
  runtime.load();
  const nodes = Array.from({ length: 1_000 }, () => new FakeElement());
  runtime.emit([mutation(runtime.document.body, { addedNodes: nodes })]);

  let stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.nodes, nodes.length);
  assert.equal(stats.pendingNodes, nodes.length - 256);
  assert.equal(runtime.microtasks.length, 1);
  assert.equal(runtime.animationFrames.size, 0);

  drainDeferredMutations(runtime);
  stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.pending, 0);
  assert.equal(stats.pendingNodes, 0);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("deferred identities keep new work and evict stale work one task at a time", () => {
  const runtime = fixture({
    replacements: {
      maxDeferredMutationTasks: 3,
      maxDeferredMutationNodes: 3,
      maxIncrementalNodesPerBatch: 1,
    },
  });
  runtime.load();
  const targets = ["same-old", "same-middle", "same-new", "other-a", "other-b"]
    .map((turnKey) => {
      const target = new FakeElement("div", { "data-turn-key": turnKey });
      target.turnClosestCalls = 0;
      const closest = target.closest.bind(target);
      target.closest = (selector) => {
        if (selector === "[data-turn-key]") target.turnClosestCalls += 1;
        return closest(selector);
      };
      return target;
    });
  runtime.emit(targets.map((target) => mutation(target, {
    addedNodes: [new FakeElement("button")],
  })));

  drainDeferredMutations(runtime);
  assert.deepEqual(targets.map((target) => target.turnClosestCalls), [1, 0, 1, 1, 1]);
  const stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.pending, 0);
  assert.equal(stats.pendingNodes, 0);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("deferred overflow settles without Array.prototype.sort", () => {
  const runtime = fixture({
    replacements: {
      maxDeferredMutationBatches: 1,
      maxIncrementalNodesPerBatch: 2,
    },
  });
  runtime.load();
  let sortCalls = 0;
  const originalSort = Array.prototype.sort;
  Array.prototype.sort = function sort(...args) {
    sortCalls += 1;
    return originalSort.apply(this, args);
  };
  try {
    const nodes = Array.from({ length: 10 }, () => new FakeElement("span"));
    runtime.emit([mutation(runtime.document.body, { addedNodes: nodes })]);
    const stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
    assert.ok(stats.pendingNodes > 0);
  } finally {
    Array.prototype.sort = originalSort;
  }

  assert.equal(sortCalls, 0);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("removing the newest identity task restores the next newest indexed task", () => {
  const runtime = fixture({ sourceTransform: withSessionMutationTestHooks });
  runtime.load();
  const hooks = runtime.window.__codeySessionToolsInstall.__codeyTestHooks;
  const tasks = ["old", "middle", "new"].map((suffix, index) => {
    const target = new FakeElement("div", { "data-turn-key": "shared" });
    return {
      addedNodes: [new FakeElement("button")],
      critical: false,
      mutation: mutation(target, {
        addedNodes: [new FakeElement("button")],
      }),
      removedNodes: [],
      sequence: index + 1,
    };
  });
  for (const task of tasks) hooks.enqueueMutationBatchTask(task, "turn:shared");

  assert.equal(hooks.deferredMutationIdentityIndex.get("turn:shared"), tasks[2]);
  assert.equal(hooks.removeDeferredMutationTask(tasks[2]), true);
  assert.equal(hooks.deferredMutationIdentityIndex.get("turn:shared"), tasks[1]);
  assert.equal(hooks.removeDeferredMutationTask(tasks[1]), true);
  assert.equal(hooks.deferredMutationIdentityIndex.get("turn:shared"), tasks[0]);
  assert.equal(hooks.removeDeferredMutationTask(tasks[0]), true);
  assert.equal(hooks.deferredMutationIdentityIndex.has("turn:shared"), false);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("dispose clears overflow records and invalidates the queued flush", () => {
  const runtime = fixture();
  runtime.load();
  const events = [];
  const targets = Array.from({ length: 620 }, (_, index) => (
    orderedTarget(String(index), events)
  ));
  runtime.emit(targets.map((target) => attributeMutation(target)));

  let stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.ok(stats.pending > 0);
  assert.equal(events.length, 512);
  const queuedFlush = runtime.microtasks.shift();
  assert.equal(typeof queuedFlush, "function");
  runtime.window.__codeySessionToolsInstall.dispose();
  stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.pending, 0);
  assert.equal(stats.pendingNodes, 0);
  queuedFlush();
  assert.equal(events.length, 512);
  stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.pending, 0);
  assert.equal(stats.pendingNodes, 0);
});

test("large added and removed subtrees reuse containment results per selector", () => {
  const runtime = fixture();
  runtime.load();
  const addedRoot = new FakeElement();
  const addedNested = addedRoot.appendChild(new FakeElement());
  addedNested.appendChild(new FakeElement("button"));
  let addedRootQueries = 0;
  let addedNestedQueries = 0;
  const rootQuerySelector = addedRoot.querySelector.bind(addedRoot);
  const nestedQuerySelector = addedNested.querySelector.bind(addedNested);
  addedRoot.querySelector = (selector) => {
    if (String(selector).includes("button")) addedRootQueries += 1;
    return rootQuerySelector(selector);
  };
  addedNested.querySelector = (selector) => {
    if (String(selector).includes("button")) addedNestedQueries += 1;
    return nestedQuerySelector(selector);
  };

  const removedRoot = new FakeElement();
  const removedTurn = removedRoot.appendChild(new FakeElement("div", {
    "data-turn-key": "removed-turn",
  }));
  let removedRootQueries = 0;
  let removedTurnQueries = 0;
  const removedRootQuerySelector = removedRoot.querySelector.bind(removedRoot);
  const removedTurnQuerySelector = removedTurn.querySelector.bind(removedTurn);
  removedRoot.querySelector = (selector) => {
    if (String(selector).includes("data-turn-key")) removedRootQueries += 1;
    return removedRootQuerySelector(selector);
  };
  removedTurn.querySelector = (selector) => {
    if (String(selector).includes("data-turn-key")) removedTurnQueries += 1;
    return removedTurnQuerySelector(selector);
  };

  runtime.emit([
    mutation(runtime.document.body, { addedNodes: [addedRoot] }),
    mutation(runtime.document.body, { addedNodes: [addedNested] }),
    mutation(runtime.document.body, { removedNodes: [removedRoot] }),
    mutation(runtime.document.body, { removedNodes: [removedTurn] }),
  ]);

  assert.equal(addedRootQueries + addedNestedQueries + removedRootQueries + removedTurnQueries, 3);
  assert.equal(addedNestedQueries, 0);
  assert.equal(removedRootQueries, 2);
  assert.equal(removedTurnQueries, 0);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("mutation pre-processing caps DOM probes before records are deferred", () => {
  const runtime = fixture();
  runtime.load();
  let probes = 0;
  const records = Array.from({ length: 620 }, () => {
    const node = new FakeElement("span");
    node.querySelector = (selector) => {
      if (!String(selector).includes("button")) probes += 1;
      return null;
    };
    return mutation(runtime.document.body, { addedNodes: [node] });
  });

  runtime.emit(records);

  const stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(probes, 16);
  assert.ok(stats.pending >= records.length - 512);
  assert.ok(stats.pendingNodes > 0);
  let lastProbeCount = probes;
  let maxDeferredProbes = 0;
  drainDeferredMutations(runtime, () => {
    maxDeferredProbes = Math.max(maxDeferredProbes, probes - lastProbeCount);
    lastProbeCount = probes;
  });
  assert.ok(maxDeferredProbes <= 16, `deferred probes=${maxDeferredProbes}`);
  assert.equal(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pending,
    0,
  );
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("mutation pre-processing does not materialize an oversized node list", () => {
  const runtime = fixture();
  runtime.load();
  let iterated = 0;
  const addedNodes = { length: 1_000 };
  for (let index = 0; index < addedNodes.length; index += 1) {
    const node = new FakeElement("span");
    node.querySelector = () => null;
    addedNodes[index] = node;
  }
  Object.defineProperty(addedNodes, Symbol.iterator, {
    value: function* iterate() {
      for (let index = 0; index < addedNodes.length; index += 1) {
        iterated += 1;
        yield addedNodes[index];
      }
    },
  });

  runtime.emit([mutation(runtime.document.body, { addedNodes })]);

  assert.ok(iterated <= 16, "unexpected full node iteration: " + iterated);
  assert.ok(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pendingNodes > 0,
  );
  drainDeferredMutations(runtime);
  assert.equal(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pendingNodes,
    0,
  );
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("deferred mutations preserve order across node slicing and record overflow", () => {
  const runtime = fixture();
  runtime.load();
  const labels = Array.from({ length: 620 }, (_, index) => String(index));
  const events = [];
  const targets = labels.map((label) => orderedTarget(label, events));
  const largeNodes = Array.from({ length: 300 }, () => new FakeElement("span"));
  const records = [
    mutation(targets[0], { addedNodes: largeNodes }),
    ...targets.slice(1).map((target) => attributeMutation(target)),
  ];

  runtime.emit(records);
  assert.deepEqual(events, ["0"], "later records ran before the first sliced tail");
  drainDeferredMutations(runtime);

  assert.equal(events.filter((label) => label === "0").length, 2);
  assert.ok(
    events.lastIndexOf("0") < events.indexOf("1"),
    "an earlier sliced record completed after a later record",
  );
  const firstSeen = [];
  const seen = new Set();
  for (const label of events) {
    if (seen.has(label)) continue;
    seen.add(label);
    firstSeen.push(label);
  }
  assert.deepEqual(firstSeen, labels);
  assert.equal(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pending,
    0,
  );
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("a split mutation keeps added and removed nodes in their original order", () => {
  const runtime = fixture();
  runtime.load();
  const observed = [];
  const addedNodes = Array.from({ length: 200 }, (_, index) => {
    const node = new FakeElement("span");
    node.matches = () => {
      observed.push(`added-${index}`);
      return false;
    };
    node.querySelector = () => {
      observed.push(`added-${index}`);
      return null;
    };
    return node;
  });
  const removedNodes = Array.from({ length: 150 }, (_, index) => {
    const node = new FakeElement("span");
    node.matches = () => {
      observed.push(`removed-${index}`);
      return false;
    };
    node.querySelector = () => {
      observed.push(`removed-${index}`);
      return null;
    };
    return node;
  });

  runtime.emit([mutation(runtime.document.body, { addedNodes, removedNodes })]);
  drainDeferredMutations(runtime);

  const firstSeen = [];
  const seen = new Set();
  for (const label of observed) {
    if (seen.has(label)) continue;
    seen.add(label);
    firstSeen.push(label);
  }
  assert.deepEqual(firstSeen, [
    ...addedNodes.map((_, index) => `added-${index}`),
    ...removedNodes.map((_, index) => `removed-${index}`),
  ]);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("critical records do not reorder earlier ordinary records", () => {
  const runtime = fixture();
  runtime.load();
  const events = [];
  const ordinaryTarget = orderedTarget("ordinary", events);
  const laterTarget = orderedTarget("later-critical", events);
  const criticalNode = new FakeElement("div", { "data-turn-key": "later-turn" });

  runtime.emit([
    mutation(ordinaryTarget, { addedNodes: [new FakeElement("span")] }),
    mutation(laterTarget, { addedNodes: [criticalNode] }),
  ]);

  assert.deepEqual(events, ["ordinary", "later-critical"]);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("attributes after an exhausted node budget stay in the deferred queue", () => {
  const runtime = fixture({ replacements: { maxIncrementalNodesPerBatch: 2 } });
  runtime.load();
  const events = [];
  const firstTarget = orderedTarget("first", events);
  const attributeTarget = orderedTarget("attribute", events);
  const nodes = Array.from({ length: 4 }, () => new FakeElement("span"));

  runtime.emit([
    mutation(firstTarget, { addedNodes: nodes }),
    attributeMutation(attributeTarget),
  ]);

  assert.deepEqual(events, ["first"]);
  const stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.ok(stats.pending > 0);
  drainDeferredMutations(runtime);
  assert.deepEqual(events, ["first", "first", "attribute"]);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("deferred overflow flushes one batch per frame and keeps record order", () => {
  const runtime = fixture({ replacements: { maxIncrementalNodesPerBatch: 2 } });
  runtime.load();
  const events = [];
  const targets = ["first", "second", "third"].map((label) => (
    orderedTarget(label, events)
  ));
  const nodes = Array.from({ length: 6 }, () => new FakeElement("span"));

  runtime.emit([
    mutation(targets[0], { addedNodes: nodes.slice(0, 4) }),
    attributeMutation(targets[1]),
    attributeMutation(targets[2]),
  ]);

  assert.deepEqual(events, ["first"]);
  assert.equal(runtime.animationFrames.size, 0);
  assert.equal(runtime.microtasks.length, 1);
  runtime.microtasks.shift()();
  assert.deepEqual(events, ["first", "first"]);
  assert.equal(runtime.animationFrames.size, 1);
  assert.ok(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pending > 0,
  );

  runtime.runAnimationFrame();
  assert.deepEqual(events, ["first", "first", "second"]);
  assert.equal(runtime.animationFrames.size, 1);

  runtime.runAnimationFrame();
  assert.deepEqual(events, ["first", "first", "second", "third"]);
  assert.equal(runtime.animationFrames.size, 0);
  assert.equal(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pending,
    0,
  );
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("deferred overflow uses zero-delay tasks when animation frames are unavailable", () => {
  const runtime = fixture({ replacements: { maxIncrementalNodesPerBatch: 2 } });
  runtime.load();
  runtime.window.requestAnimationFrame = undefined;
  const events = [];
  const targets = ["first", "second", "third"].map((label) => (
    orderedTarget(label, events)
  ));
  const nodes = Array.from({ length: 4 }, () => new FakeElement("span"));

  runtime.emit([
    mutation(targets[0], { addedNodes: nodes }),
    attributeMutation(targets[1]),
    attributeMutation(targets[2]),
  ]);
  assert.deepEqual(events, ["first"]);
  assert.equal(runtime.microtasks.length, 1);

  runtime.microtasks.shift()();
  assert.deepEqual(events, ["first", "first"]);
  assert.equal(runtime.animationFrames.size, 0);

  runtime.runTimeout(0);
  assert.deepEqual(events, ["first", "first", "second"]);
  runtime.runTimeout(0);
  assert.deepEqual(events, ["first", "first", "second", "third"]);
  assert.equal(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pending,
    0,
  );
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("the mutation observer watches identity attributes and excludes display-only attributes", () => {
  const runtime = fixture();
  runtime.load();
  const filter = runtime.observers[0].options.attributeFilter;
  for (const attribute of [
    "class",
    "data-turn-key",
    "data-message-id",
    "data-message-author-role",
    "data-testid",
    "data-session-id",
    "data-conversation-id",
    "data-thread-id",
    "data-app-action-sidebar-thread-id",
  ]) {
    assert.equal(filter.includes(attribute), true, `missing ${attribute}`);
  }
  for (const attribute of ["style", "aria-label", "aria-hidden", "disabled", "hidden"]) {
    assert.equal(filter.includes(attribute), false, `unexpected ${attribute}`);
  }
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("telemetry cumulatively counts immediate and deferred flush work", () => {
  const runtime = fixture({
    replacements: { maxIncrementalNodesPerBatch: 2 },
  });
  runtime.load();
  runtime.emit([mutation(runtime.document.body)]);
  const first = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(first.mutations, 1);
  assert.equal(first.calls, 1);
  assert.equal(first.items, 1);

  runtime.emit([mutation(runtime.document.body, {
    addedNodes: Array.from({ length: 5 }, () => new FakeElement("button")),
  })]);
  let stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.mutations, 2);
  assert.equal(stats.nodes, 5);
  assert.equal(stats.skipped, 1);
  assert.ok(stats.pendingNodes > 0);
  assert.equal(stats.calls, 1);
  assert.equal(stats.items, 1);

  drainDeferredMutations(runtime);
  stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.mutations, 2);
  assert.equal(stats.nodes, 5);
  assert.equal(stats.calls, 1);
  assert.equal(stats.items, 1);
  assert.ok(stats.totalMs >= first.totalMs);
  assert.ok(stats.maxMs >= first.maxMs);
  assert.ok(stats.timing.handleMs >= 0);
  assert.ok(stats.timing.flushMs >= 0);
  assert.ok(stats.timing.scanMs >= 0);
  assert.equal(stats.pending, 0);
  assert.equal(stats.pendingNodes, 0);
  runtime.window.__codeySessionToolsInstall.dispose();
});

test("dispose clears deferred task and node telemetry", () => {
  const runtime = fixture();
  runtime.load();
  runtime.emit([mutation(runtime.document.body, {
    addedNodes: Array.from({ length: 600 }, () => new FakeElement()),
  })]);
  let stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.ok(stats.pending > 0);
  assert.ok(stats.pendingNodes > 0);

  runtime.window.__codeySessionToolsInstall.dispose();
  stats = runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"];
  assert.equal(stats.pending, 0);
  assert.equal(stats.pendingNodes, 0);
});

test("session identity attribute changes refresh message selection without full-body sidebar scans", () => {
  const runtime = fixture();
  runtime.load();
  const composer = runtime.document.body.appendChild(new FakeElement("div", {
    "data-above-composer-conversation-id": "session-a",
  }));
  const turn = runtime.document.body.appendChild(new FakeElement("div", {
    "data-turn-key": "turn-1",
  }));
  let bodySidebarQueries = 0;
  const originalBodyQuerySelectorAll = runtime.document.body.querySelectorAll.bind(
    runtime.document.body,
  );
  runtime.document.body.querySelectorAll = (selector) => {
    if (String(selector).includes("data-app-action-sidebar")) {
      bodySidebarQueries += 1;
    }
    return originalBodyQuerySelectorAll(selector);
  };

  composer.setAttribute("data-above-composer-conversation-id", "session-b");
  runtime.emit([
    attributeMutation(composer, "data-above-composer-conversation-id"),
  ]);
  runtime.runTimeout(60);

  assert.equal(bodySidebarQueries, 0);
  assert.equal(turn.dataset.codeyMessageId, "turn-1");
  assert.equal(
    runtime.window.__codeyObserverStats["codey-inject.sessionToolMutations"].pending,
    0,
  );
  runtime.window.__codeySessionToolsInstall.dispose();
});
