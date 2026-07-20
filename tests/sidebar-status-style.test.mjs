import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement {
  constructor() {
    this.attributes = new Map();
    this.dataset = {};
    this.parentElement = null;
    this.textContent = "";
    this.spinner = false;
    this.managedSpinner = false;
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  querySelector(selector) {
    if (selector === ".animate-spin") return this.spinner ? this.spinnerElement() : null;
    return null;
  }

  querySelectorAll(selector) {
    if (selector === ".animate-spin") return this.spinner ? [this.spinnerElement()] : [];
    return [];
  }

  spinnerElement() {
    const spinner = new FakeElement();
    spinner.closest = () => (this.managedSpinner ? new FakeElement() : null);
    return spinner;
  }

  getClientRects() {
    return [];
  }

  addEventListener() {}

  appendChild() {}
}

function loadInjection() {
  const placeholder = new FakeElement();
  const document = {
    body: new FakeElement(),
    documentElement: new FakeElement(),
    createElement: () => new FakeElement(),
    getElementById: () => placeholder,
    querySelector: () => null,
    querySelectorAll: () => [],
  };
  const window = {
    addEventListener: () => {},
    clearTimeout: () => {},
    dispatchEvent: () => true,
    localStorage: { length: 0, key: () => null, getItem: () => null, setItem: () => {} },
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
  return window;
}

function rowWithProps(props) {
  const row = new FakeElement();
  row.__reactFiber$test = { memoizedProps: props, pendingProps: null, return: null };
  return row;
}

test("maps native Codex task states to traffic-light states", () => {
  const runtime = loadInjection();

  assert.equal(
    runtime.__codeyThreadStatusFromRow(rowWithProps({ statusState: { type: "loading" } })),
    "running",
  );
  assert.equal(
    runtime.__codeyThreadStatusFromRow(rowWithProps({ statusState: { type: "error" } })),
    "error",
  );
  assert.equal(
    runtime.__codeyThreadStatusFromRow(rowWithProps({ statusPill: { label: "Needs input" } })),
    "waiting",
  );
  assert.equal(
    runtime.__codeyThreadStatusFromRow(rowWithProps({
      chips: [{ id: "awaiting-approval" }],
      statusState: { type: "idle" },
    })),
    "waiting",
  );
});

test("prefers authoritative Rust host lifecycle states over stale React state", () => {
  const runtime = loadInjection();
  const row = rowWithProps({ statusState: { type: "loading" } });
  row.attributes.set("data-app-action-sidebar-thread-id", "local:thread-1");
  runtime.__codeyHostThreadStatuses = { "thread-1": "idle" };
  runtime.__codeyHostThreadStatusesAuthoritative = true;

  assert.equal(runtime.__codeyThreadStatusFromRow(row), "");

  runtime.__codeyHostThreadStatuses["thread-1"] = "waiting";
  assert.equal(runtime.__codeyThreadStatusFromRow(row), "waiting");
});

test("maps a temporary client thread row to its canonical conversation id", () => {
  const runtime = loadInjection();
  const row = rowWithProps({ statusState: { type: "loading" } });
  row.attributes.set(
    "data-app-action-sidebar-thread-id",
    "local:client-new-thread:temporary-id",
  );
  row.__reactFiber$test.return = {
    memoizedProps: {
      entry: { conversationId: "019f7f5c-7ce6-7be2-a45f-4a583106cbb1" },
    },
    pendingProps: null,
    return: null,
  };
  runtime.__codeyHostThreadStatuses = {
    "019f7f5c-7ce6-7be2-a45f-4a583106cbb1": "running",
  };
  runtime.__codeyHostThreadStatusesAuthoritative = true;

  assert.equal(
    runtime.__codeyThreadSessionIdFromRow(row),
    "019f7f5c-7ce6-7be2-a45f-4a583106cbb1",
  );
  assert.equal(runtime.__codeyThreadStatusFromRow(row), "running");

  runtime.__codeyHostThreadStatuses["019f7f5c-7ce6-7be2-a45f-4a583106cbb1"] = "idle";
  assert.equal(runtime.__codeyThreadStatusFromRow(row), "");
});

test("an authoritative host snapshot clears sessions missing from the active map", () => {
  const runtime = loadInjection();
  const row = rowWithProps({ statusState: { type: "loading" } });
  row.attributes.set("data-app-action-sidebar-thread-id", "local:missing-thread");
  runtime.__codeyHostThreadStatuses = {};
  runtime.__codeyHostThreadStatusesAuthoritative = true;

  assert.equal(runtime.__codeyThreadStatusFromRow(row), "");
});

test("keeps a DOM spinner fallback for Codex renderer changes", () => {
  const runtime = loadInjection();
  const row = new FakeElement();
  row.spinner = true;

  assert.equal(runtime.__codeyThreadStatusFromRow(row), "running");
});

test("keeps explicit idle authoritative over a stale unmanaged spinner", () => {
  const runtime = loadInjection();
  const row = rowWithProps({ statusState: { type: "idle" } });
  row.spinner = true;

  assert.equal(runtime.__codeyThreadStatusFromRow(row), "");
});

test("keeps explicit idle authoritative after the status slot was managed", () => {
  const runtime = loadInjection();
  const row = rowWithProps({ statusState: { type: "idle" } });
  row.spinner = true;
  row.managedSpinner = true;

  assert.equal(runtime.__codeyThreadStatusFromRow(row), "");
});

test("injects blinking green and steady red/yellow status styles", () => {
  assert.match(source, /threadStatusAttribute = "data-codey-thread-traffic-status"/);
  assert.match(source, /threadStatusAttribute}\]::after \{[^}]*right: 10px;/);
  assert.match(source, /threadStatusAttribute}="running"\]::after.*animation: codey-thread-status-blink/s);
  assert.match(source, /threadStatusAttribute}="error"\]::after.*background: #ef4444/s);
  assert.match(source, /threadStatusAttribute}="waiting"\]::after.*background: #eab308/s);
  assert.match(source, /threadStatusAttribute}\].*group-hover:hidden.*visibility: hidden !important/s);
  assert.match(source, /data-codey-thread-status-indicator.*display: none !important/);
});
