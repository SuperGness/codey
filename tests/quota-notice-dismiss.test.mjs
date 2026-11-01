import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const root = new URL("../", import.meta.url);
const source = await readFile(
  new URL("public/quota-notice-dismiss.js", root),
  "utf8",
);

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", text = "") {
    super(tagName, { connected: true });
    this.textContent = text;
    this.clicks = 0;
  }

  get textContent() {
    if (this.children.length === 0) return this._textContent || "";
    return [
      this._textContent || "",
      ...this.children.map((child) => child.textContent || ""),
    ].join(" ");
  }

  set textContent(value) {
    this._textContent = String(value);
  }

  click() {
    this.dispatchEvent({ type: "click" });
  }
}

const createElement = (tagName, text = "") => new FakeElement(tagName, text);

function findById(rootElement, id) {
  if (rootElement.id === id) return rootElement;
  for (const child of rootElement.children) {
    const match = findById(child, id);
    if (match) return match;
  }
  return null;
}

function findAll(rootElement, predicate) {
  const matches = predicate(rootElement) ? [rootElement] : [];
  for (const child of rootElement.children) {
    matches.push(...findAll(child, predicate));
  }
  return matches;
}

function createBusyTree({ depth = 2, width = 6 } = {}) {
  const top = createElement("div");
  let level = [top];
  for (let currentDepth = 0; currentDepth < depth; currentDepth += 1) {
    const next = [];
    for (const parent of level) {
      for (let index = 0; index < width; index += 1) {
        next.push(parent.appendChild(createElement("div", "unrelated content")));
      }
    }
    level = next;
  }
  return top;
}

function createWideTree(width = 600) {
  const top = createElement("div");
  for (let index = 0; index < width; index += 1) {
    top.appendChild(createElement("div", "unrelated content"));
  }
  return { top };
}

function createRuntime({ beforeInstall } = {}) {
  const html = createElement("html");
  const body = html.appendChild(createElement("body"));
  let mutationCallback = null;
  let subscriptions = 0;
  let unsubscriptions = 0;
  const window = {
    clearTimeout() {},
    getComputedStyle: () => ({ position: "static" }),
  };
  const document = {
    body,
    documentElement: html,
    createElement,
    getElementById: (id) => findById(html, id),
    querySelectorAll: () => findAll(html, () => true),
  };
  window.window = window;
  window.__codeyMutationDispatcher = {
    subscribe(callback) {
      subscriptions += 1;
      mutationCallback = callback;
      return () => {
        unsubscriptions += 1;
      };
    },
  };
  const timers = [];
  window.setTimeout = (callback) => {
    timers.push(callback);
    return timers.length;
  };
  window.clearTimeout = (timer) => {
    timers[timer - 1] = null;
  };
  const flushNextTimer = () => {
    const index = timers.findIndex(Boolean);
    if (index < 0) return;
    const callback = timers[index];
    timers[index] = null;
    callback();
  };
  const flushTimers = () => {
    while (timers.some(Boolean)) {
      for (let index = 0; index < timers.length; index += 1) {
        const callback = timers[index];
        timers[index] = null;
        callback?.();
      }
    }
  };
  const sandbox = {
    document,
    Element: FakeElement,
    window,
  };
  beforeInstall?.({ body, document, window });
  vm.runInNewContext(source, sandbox);
  return {
    body,
    document,
    flushNextTimer,
    flushTimers,
    window,
    emit(mutations, { flush = true } = {}) {
      mutationCallback?.(mutations);
      if (flush) flushTimers();
    },
    get subscriptions() {
      return subscriptions;
    },
    get unsubscriptions() {
      return unsubscriptions;
    },
  };
}

function appendNotice(body, {
  title = "Codex 和工作使用额度已用完",
  bodyText = "你的速率限制将于 17:31 重置。立即升级，或使用一次速率限制重置机会。",
  resetLabel = "重置使用量",
} = {}) {
  const notice = body.appendChild(createElement("section"));
  notice.appendChild(createElement("h2", title));
  notice.appendChild(createElement("p", bodyText));
  const actions = notice.appendChild(createElement("div"));
  const reset = actions.appendChild(createElement("button", resetLabel));
  const upgrade = actions.appendChild(createElement("button", "升级"));
  return { actions, notice, reset, upgrade };
}

const findCloseButton = (notice) =>
  notice.querySelector('[data-codey-quota-notice-close="true"]');

test("scans a quota notice that exists before the script is installed", () => {
  let notice = null;
  const runtime = createRuntime({
    beforeInstall({ body }) {
      ({ notice } = appendNotice(body));
    },
  });

  runtime.flushTimers();
  assert.ok(findCloseButton(notice));
});

test("adds an accessible close button to an existing quota notice", () => {
  const runtime = createRuntime();
  const { notice } = appendNotice(runtime.body);
  runtime.emit([{ addedNodes: [notice], target: runtime.body, type: "childList" }]);

  const close = findCloseButton(notice);
  assert.ok(close);
  assert.equal(close.tagName, "BUTTON");
  assert.equal(close.getAttribute("type"), "button");
  assert.equal(close.getAttribute("aria-label"), "关闭使用额度提示");
  assert.equal(close.getAttribute("title"), "关闭使用额度提示");
  assert.equal(close.textContent, "\u00d7");
  assert.equal(notice.getAttribute("data-codey-quota-notice"), "true");
  assert.equal(runtime.window.__codeyQuotaNoticeDismiss.snapshot().observerInstalled, true);
});

test("native click hides the notice and repeated scans do not inject it again", () => {
  const runtime = createRuntime();
  const { notice } = appendNotice(runtime.body);
  runtime.window.__codeyQuotaNoticeDismiss.scan(runtime.body);
  const close = findCloseButton(notice);

  close.click();
  assert.equal(notice.style.display, "none:important");
  assert.equal(notice.getAttribute("aria-hidden"), "true");
  assert.equal(notice.getAttribute("data-codey-quota-notice-dismissed"), "true");

  const churn = notice.appendChild(createElement("span", "later render"));
  runtime.emit([{ addedNodes: [churn], target: notice, type: "childList" }]);
  runtime.window.__codeyQuotaNoticeDismiss.scan(notice);
  assert.equal(findCloseButton(notice), null);
  assert.equal(notice.style.display, "none:important");
});

test("ordinary quota panels and unrelated upgrade controls remain untouched", () => {
  const runtime = createRuntime();
  const panel = runtime.body.appendChild(createElement("section"));
  panel.appendChild(createElement("h2", "本月使用额度"));
  panel.appendChild(createElement("p", "剩余 20%，距离额度重置还有 3 天。"));
  const actions = panel.appendChild(createElement("div"));
  const reset = actions.appendChild(createElement("button", "重置使用量"));
  const upgrade = actions.appendChild(createElement("button", "升级"));

  runtime.window.__codeyQuotaNoticeDismiss.scan(panel);
  assert.equal(findCloseButton(panel), null);
  assert.equal(runtime.window.__codeyQuotaNoticeDismiss.dismiss(panel), false);
  assert.equal(reset.clicks, 0);
  assert.equal(upgrade.clicks, 0);
});

test("a surrounding page container is not decorated or hidden", () => {
  const runtime = createRuntime();
  const page = runtime.body.appendChild(createElement("main"));
  page.appendChild(createElement("p", "普通会话内容"));
  const { notice } = appendNotice(page);

  runtime.window.__codeyQuotaNoticeDismiss.scan(page);
  assert.ok(findCloseButton(notice));
  assert.equal(findCloseButton(page), findCloseButton(notice));
  assert.equal(page.getAttribute("data-codey-quota-notice"), null);

  runtime.window.__codeyQuotaNoticeDismiss.dismiss(page);
  assert.equal(page.style.display, undefined);
  assert.equal(notice.style.display, "none:important");
});

test("handles dynamic insertion and text that arrives after the container", () => {
  const runtime = createRuntime();
  const notice = runtime.body.appendChild(createElement("section"));
  const title = notice.appendChild(createElement("h2", ""));
  const bodyText = notice.appendChild(createElement("p", ""));
  const reset = notice.appendChild(createElement("button", ""));
  runtime.emit([{ addedNodes: [notice], target: runtime.body, type: "childList" }]);
  assert.equal(findCloseButton(notice), null);

  title.textContent = "  Codex   和工作使用额度已用完  ";
  bodyText.textContent = "你的速率限制将于 17:31 重置。";
  reset.textContent = "重置使用量";
  runtime.emit([
    { addedNodes: [{ nodeType: 3, textContent: title.textContent }], target: title, type: "childList" },
    { addedNodes: [{ nodeType: 3, textContent: bodyText.textContent }], target: bodyText, type: "childList" },
    { addedNodes: [{ nodeType: 3, textContent: reset.textContent }], target: reset, type: "childList" },
  ]);

  assert.ok(findCloseButton(notice));
});

test("ignores streamed turn updates and still handles a late notice outside the turn", () => {
  const runtime = createRuntime();
  runtime.flushTimers();
  const turn = runtime.body.appendChild(createElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const notice = turn.appendChild(createElement("section"));
  notice.appendChild(createElement("h2", "Codex 和工作使用额度已用完"));
  notice.appendChild(createElement(
    "p",
    "你的速率限制将于 17:31 重置。",
  ));
  const actions = notice.appendChild(createElement("div"));
  const reset = actions.appendChild(createElement("button", "重置使用量"));
  const before = runtime.window.__codeyQuotaNoticeDismiss.snapshot();

  runtime.emit([
    {
      addedNodes: [reset],
      target: actions,
      type: "childList",
    },
    {
      addedNodes: [{ nodeType: 3, textContent: "更多流式内容" }],
      target: notice,
      type: "childList",
    },
  ]);

  const after = runtime.window.__codeyQuotaNoticeDismiss.snapshot();
  assert.equal(after.pendingRoots, before.pendingRoots);
  assert.equal(after.scanCount, before.scanCount);
  assert.equal(after.scannedElements, before.scannedElements);
  assert.equal(findCloseButton(notice), null);

  const later = appendNotice(runtime.body);
  runtime.emit([{
    addedNodes: [later.notice],
    target: runtime.body,
    type: "childList",
  }]);
  assert.ok(findCloseButton(later.notice));
});

test("keeps warning dialogs inside a conversation turn eligible", () => {
  const runtime = createRuntime();
  runtime.flushTimers();
  const turn = runtime.body.appendChild(createElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const dialog = turn.appendChild(createElement("section"));
  dialog.setAttribute("role", "dialog");
  dialog.appendChild(createElement("h2", "Codex 和工作使用额度已用完"));
  dialog.appendChild(createElement("p", "你的速率限制将于 17:31 重置。"));
  const reset = dialog.appendChild(createElement("button", "重置使用量"));

  runtime.emit([{
    addedNodes: [{ nodeType: 3, textContent: reset.textContent }],
    target: reset,
    type: "childList",
  }]);

  assert.ok(findCloseButton(dialog));
});

test("keeps a warning portal mounted inside a conversation turn eligible", () => {
  const runtime = createRuntime();
  runtime.flushTimers();
  const turn = runtime.body.appendChild(createElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const host = turn.appendChild(createElement("div"));
  const portal = createElement("section");
  portal.setAttribute("data-radix-portal", "true");
  portal.appendChild(createElement("h2", "Codex 和工作使用额度已用完"));
  portal.appendChild(createElement("p", "你的速率限制将于 17:31 重置。"));
  host.appendChild(portal);

  runtime.emit([{
    addedNodes: [portal],
    target: host,
    type: "childList",
  }]);

  assert.ok(findCloseButton(portal));
});

test("adds a close button when a portal reset control arrives after its title", () => {
  const runtime = createRuntime();
  runtime.flushTimers();
  const turn = runtime.body.appendChild(createElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const portal = turn.appendChild(createElement("div"));
  portal.setAttribute("data-radix-portal", "true");
  portal.appendChild(createElement("h2", "Codex 和工作使用额度已用完"));
  const support = portal.appendChild(createElement("p", ""));
  runtime.emit([{
    addedNodes: [portal],
    target: turn,
    type: "childList",
  }]);
  assert.equal(findCloseButton(portal), null);

  support.textContent = "你的速率限制将于 17:31 重置。";
  const reset = portal.appendChild(createElement("button", "重置使用量"));
  runtime.emit([{ addedNodes: [reset], target: portal, type: "childList" }]);

  assert.ok(findCloseButton(portal));
});

test("adds a close button when a portal title arrives after its reset control", () => {
  const runtime = createRuntime();
  runtime.flushTimers();
  const turn = runtime.body.appendChild(createElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const portal = turn.appendChild(createElement("div"));
  portal.setAttribute("data-radix-portal", "true");
  const actions = portal.appendChild(createElement("div"));
  const reset = actions.appendChild(createElement("button", "重置使用量"));
  portal.appendChild(createElement("p", "你的速率限制将于 17:31 重置。"));
  runtime.emit([{
    addedNodes: [portal],
    target: turn,
    type: "childList",
  }]);
  assert.equal(findCloseButton(portal), null);

  const title = createElement("h2", "");
  portal.insertBefore(title, portal.firstChild || null);
  runtime.emit([{ addedNodes: [title], target: portal, type: "childList" }]);
  assert.equal(findCloseButton(portal), null);

  title.textContent = "Codex 和工作使用额度已用完";
  runtime.emit([{
    addedNodes: [{ nodeType: 3, textContent: title.textContent }],
    target: title,
    type: "childList",
  }]);

  assert.ok(findCloseButton(portal));
  assert.equal(reset.textContent, "重置使用量");
});

test("keeps an unrelated insertion in a turn's portal wrapper ignored", () => {
  const runtime = createRuntime();
  runtime.flushTimers();
  const turn = runtime.body.appendChild(createElement("section"));
  turn.setAttribute("data-turn-key", "turn-1");
  const portal = turn.appendChild(createElement("div"));
  portal.setAttribute("data-radix-portal", "true");
  const before = runtime.window.__codeyQuotaNoticeDismiss.snapshot();

  runtime.emit([{
    addedNodes: [createElement("span", "streamed output")],
    target: portal,
    type: "childList",
  }]);

  const after = runtime.window.__codeyQuotaNoticeDismiss.snapshot();
  assert.equal(after.pendingRoots, before.pendingRoots);
  assert.equal(after.scanCount, before.scanCount);
  assert.equal(after.scannedElements, before.scannedElements);
});

test("a new notice can be handled after the dismissed DOM instance is removed", () => {
  const runtime = createRuntime();
  const first = appendNotice(runtime.body, { title: "Codex and Work limit exhausted" });
  runtime.emit([{ addedNodes: [first.notice], target: runtime.body, type: "childList" }]);
  runtime.window.__codeyQuotaNoticeDismiss.scan(first.notice);
  findCloseButton(first.notice).click();
  first.notice.remove();
  runtime.emit([{ removedNodes: [first.notice], target: runtime.body, type: "childList" }]);

  const second = appendNotice(runtime.body, {
    title: "Codex and Work limits reached",
    bodyText: "Your rate limit resets at 17:31. Upgrade now or reset usage.",
    resetLabel: "Reset usage",
  });
  runtime.emit([{ addedNodes: [second.notice], target: runtime.body, type: "childList" }]);

  assert.ok(findCloseButton(second.notice));
  assert.equal(first.notice.style.display, "none:important");
  assert.equal(second.notice.style.display, undefined);
});

test("pending roots are bounded and still include a notice in the same batch", () => {
  const runtime = createRuntime();
  const addedNodes = Array.from(
    { length: 20 },
    (_, index) => runtime.body.appendChild(createElement("div", `change ${index}`)),
  );
  const { notice } = appendNotice(runtime.body);
  addedNodes.push(notice);
  runtime.emit([{
    addedNodes,
    target: runtime.body,
    type: "childList",
  }]);

  assert.ok(findCloseButton(notice));
});

test("repeated script evaluation is idempotent and dispose releases the subscription", () => {
  const runtime = createRuntime();
  vm.runInNewContext(source, {
    document: runtime.document,
    Element: FakeElement,
    window: runtime.window,
  });
  assert.equal(runtime.subscriptions, 1);

  runtime.window.__codeyQuotaNoticeDismiss.dispose();
  const afterDispose = runtime.subscriptions;
  vm.runInNewContext(source, {
    document: runtime.document,
    Element: FakeElement,
    window: runtime.window,
  });
  assert.equal(runtime.unsubscriptions, 1);
  assert.equal(runtime.subscriptions, afterDispose);
});

test("pending-root overflow keeps recent roots without a full-document rescan", () => {
  const runtime = createRuntime();
  for (let index = 0; index < 30; index += 1) {
    const node = runtime.body.appendChild(createElement("button", `unrelated ${index}`));
    runtime.emit([{ addedNodes: [node], target: runtime.body, type: "childList" }]);
  }

  assert.ok(runtime.window.__codeyQuotaNoticeDismiss.snapshot().pendingRoots <= 24);
  assert.doesNotMatch(source, /pendingRoots\.add\(document\.documentElement\)/);
});

test("multiple roots share one element budget and retain skipped roots", () => {
  const runtime = createRuntime();
  const roots = Array.from(
    { length: 24 },
    () => runtime.body.appendChild(createBusyTree()),
  );
  const before = runtime.window.__codeyQuotaNoticeDismiss.snapshot().scannedElements;
  runtime.emit([{
    addedNodes: roots,
    target: runtime.body,
    type: "childList",
  }]);

  const snapshot = runtime.window.__codeyQuotaNoticeDismiss.snapshot();
  assert.equal(snapshot.batchElementLimit, 160);
  assert.equal(snapshot.scannedElements - before, 160);
  assert.ok(snapshot.pendingRoots > 0);

  const originallyPending = snapshot.pendingRoots;
  const { notice } = appendNotice(roots.at(-1));
  for (let scan = 0; scan < 10; scan += 1) {
    const beforeScan = runtime.window.__codeyQuotaNoticeDismiss.snapshot().scannedElements;
    runtime.emit([{
      addedNodes: [],
      target: roots.at(-1),
      type: "attributes",
    }]);
    const afterScan = runtime.window.__codeyQuotaNoticeDismiss.snapshot();
    assert.ok(afterScan.scannedElements - beforeScan <= 160);
    if (!afterScan.pendingRoots) break;
  }

  assert.ok(originallyPending > 0);
  assert.ok(findCloseButton(notice));
  assert.equal(runtime.window.__codeyQuotaNoticeDismiss.snapshot().pendingRoots, 0);
});

test("bounds element and child expansion for a wide subtree across batches", () => {
  const runtime = createRuntime();
  const { top: wideRoot } = createWideTree();
  runtime.body.appendChild(wideRoot);
  const { notice } = appendNotice(wideRoot);
  let previous = runtime.window.__codeyQuotaNoticeDismiss.snapshot();
  let scannedBatches = 0;
  while (!findCloseButton(notice) && scannedBatches < 8) {
    if (scannedBatches === 0) {
      runtime.emit([{
        addedNodes: [wideRoot],
        target: runtime.body,
        type: "childList",
      }], { flush: false });
    } else {
      runtime.emit([{
        addedNodes: [],
        target: wideRoot,
        type: "attributes",
      }], { flush: false });
    }
    runtime.flushNextTimer();
    const current = runtime.window.__codeyQuotaNoticeDismiss.snapshot();
    assert.ok(current.scannedElements - previous.scannedElements <= 160);
    assert.ok(current.childExpansions - previous.childExpansions <= 160);
    assert.ok(current.scannedElements - previous.scannedElements > 0);
    previous = current;
    scannedBatches += 1;
  }
  assert.ok(scannedBatches > 1);
  assert.ok(findCloseButton(notice));
});

test("CDP preparation registers the script after bridge and status dependencies", async () => {
  const cdpSource = await readFile(new URL("backend/src/cdp.rs", root), "utf8");
  assert.match(
    cdpSource,
    /dist-overlay\/inject\/quota-notice-dismiss\.js/,
  );
  assert.match(cdpSource, /"quota-notice-dismiss"/);
  assert.match(
    cdpSource,
    /__codeyQuotaNoticeDismissInstalled === true[\s\S]*__codeyQuotaNoticeDismiss\?\.scan[\s\S]*__codeyQuotaNoticeDismiss\?\.dismiss/,
  );
  assert.ok(cdpSource.indexOf('"bridge-helpers"') < cdpSource.indexOf('"quota-notice-dismiss"'));
  assert.ok(cdpSource.indexOf('"security-warning-shield"') < cdpSource.indexOf('"quota-notice-dismiss"'));
});
