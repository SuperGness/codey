import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

class FakeElement {
  constructor(tagName = "div") {
    this.attributes = new Map();
    this.children = [];
    this.className = "";
    this.parentElement = null;
    this.textContent = "";
    this.title = "";
    this.attributeWrites = 0;
  }

  appendChild(child) {
    child.parentElement = this;
    this.children.push(child);
    return child;
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  hasAttribute(name) {
    return this.attributes.has(name);
  }

  matches(selector) {
    const attribute = selector.match(/^\[([^\]]+)\]$/)?.[1];
    return attribute ? this.hasAttribute(attribute) : false;
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] || null;
  }

  querySelectorAll(selector) {
    const selectors = selector.split(",").map((candidate) => candidate.trim());
    const matches = [];
    const visit = (node) => {
      node.children.forEach((child) => {
        if (selectors.some((candidate) => child.matches(candidate))) matches.push(child);
        visit(child);
      });
    };
    visit(this);
    return matches;
  }

  remove() {
    if (!this.parentElement) return;
    this.parentElement.children = this.parentElement.children.filter((child) => child !== this);
    this.parentElement = null;
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  setAttribute(name, value) {
    this.attributeWrites += 1;
    this.attributes.set(name, String(value));
  }
}

function loadInjection({ rows = [], bridgeHandler } = {}) {
  const placeholder = new FakeElement();
  const document = {
    body: new FakeElement("body"),
    documentElement: new FakeElement("html"),
    visibilityState: "visible",
    addEventListener() {},
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: () => placeholder,
    querySelector: () => null,
    threadRowQueries: 0,
    querySelectorAll(selector) {
      if (selector !== "[data-app-action-sidebar-thread-row]") return [];
      this.threadRowQueries += 1;
      return rows;
    },
  };
  const window = {
    __codexSessionDeleteBridge: bridgeHandler,
    addEventListener() {},
    clearTimeout() {},
    dispatchEvent() {},
    localStorage: { length: 0, key: () => null, getItem: () => null, setItem() {} },
    setInterval: () => 1,
    setTimeout: (callback) => {
      queueMicrotask(callback);
      return 1;
    },
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
  return { document, window };
}

test("formats compact relative times for the sidebar", () => {
  const { window } = loadInjection();
  const now = Date.UTC(2026, 6, 21, 12);
  const format = window.__codeyFormatRelativeThreadTime;

  assert.equal(format(now - 59_000, now), "刚刚");
  assert.equal(format(now - 3 * 60_000, now), "3m");
  assert.equal(format(now - 3 * 60 * 60_000, now), "3h");
  assert.equal(format(now - 2 * 24 * 60 * 60_000, now), "2d");
  assert.equal(format(now - 45 * 24 * 60 * 60_000, now), "1mo");
  assert.equal(format(now - 360 * 24 * 60 * 60_000, now), "12mo");
  assert.equal(format(now - 400 * 24 * 60 * 60_000, now), "1y");
});

test("normalizes Codex timestamp payload variants to milliseconds", () => {
  const { window } = loadInjection();
  const timestampFrom = window.__codeyThreadTimestampMsFromPayload;

  assert.equal(timestampFrom({ updated_at_ms: 123_456 }), 123_456);
  assert.equal(timestampFrom({ updated_at: 123 }), 123_000);
  assert.equal(timestampFrom({ createdAtMs: 456_789 }), 456_789);
});

test("renders an accessible time element in the thread row content", () => {
  const { window } = loadInjection();
  const row = new FakeElement();
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  const timestamp = Date.now() - 2 * 24 * 60 * 60_000;

  window.__codeyUpdateThreadUpdatedAt(row, timestamp);

  const label = content.querySelector("[data-codey-thread-updated-at]");
  assert.ok(label);
  assert.equal(label.textContent, "2d");
  assert.equal(label.getAttribute("data-codey-thread-updated-at-ms"), String(timestamp));
  assert.match(label.getAttribute("datetime"), /^\d{4}-\d{2}-\d{2}T/);
  assert.match(label.getAttribute("aria-label"), /^最后消息：2d/);
  assert.match(label.title, /^最后消息：/);

  const attributeWrites = label.attributeWrites;
  window.__codeyUpdateThreadUpdatedAt(row, timestamp);
  assert.equal(label.attributeWrites, attributeWrites);

  window.__codeyUpdateThreadUpdatedAt(row, 0);
  assert.equal(content.querySelector("[data-codey-thread-updated-at]"), null);
});

test("batches visible thread timestamps through the bridge and renders the result", async () => {
  const row = new FakeElement();
  row.setAttribute("data-app-action-sidebar-thread-row", "");
  row.setAttribute("data-app-action-sidebar-thread-id", "local:thread-1");
  row.setAttribute("data-app-action-sidebar-thread-title", "发布计划");
  const content = new FakeElement();
  content.className = "flex h-full w-full items-center";
  row.appendChild(content);
  const calls = [];
  const timestamp = Date.now() - 3 * 60 * 60_000;

  const { document } = loadInjection({
    rows: [row],
    bridgeHandler: async (path, payload) => {
      calls.push({ path, payload });
      return {
        status: "ok",
        sort_keys: [{ session_id: "thread-1", updated_at_ms: timestamp }],
      };
    },
  });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(calls.length, 1);
  assert.equal(calls[0].path, "/thread-sort-keys");
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0].payload)), {
    sessions: [{ session_id: "thread-1", title: "发布计划" }],
  });
  assert.equal(
    content.querySelector("[data-codey-thread-updated-at]")?.textContent,
    "3h",
  );
  assert.equal(document.threadRowQueries, 1);
});

test("injects time styles that yield to native statuses and sidebar actions", () => {
  assert.match(source, /threadUpdatedAtAttribute = "data-codey-thread-updated-at"/);
  assert.match(source, /font-variant-numeric: tabular-nums/);
  assert.match(
    source,
    /sidebar-thread-row\]:has\(\[data-hover-card-open-immediately\]\[class\*="group-hover:hidden"\]\) \[\$\{threadUpdatedAtAttribute\}\] \{ display: none; \}/,
  );
  assert.match(source, /sidebar-thread-row\]:hover \[\$\{threadUpdatedAtAttribute\}\].*opacity: 0/s);
});
