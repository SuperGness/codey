import assert from "node:assert/strict";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

const assetUrl = "app://-/assets/async/418532.084e06d418.js";
const patchScript = await loadStartupPatchTemplate();
function patch(source) {
  const logs = [];
  const install = runInNewContext(`${patchScript};__codeyRendererPatchEntry`, {
    URL, __codeyPatchLog: (record) => logs.push(record),
  });
  const result = install(assetUrl, source);
  assert.ok(result);
  assert.equal(install(assetUrl, result), null);
  assert.deepEqual(logs, []);
  return result;
}

// 保留这版 Codex 的原生标记查询、滚动计算及其调用形式。
const navigationLabel = 'const label="thread.userMessageNavigation.ariaLabel";';
const navigate = 'function ex(e,t){var n,a;(0,u.I)(e,(n=e,null==(a=t)||null==n?null:n.querySelector(`[data-thread-user-message-navigation-item-id="${escapeId(a)}"]`)))}';
const navigation = navigationLabel + navigate;

test("navigation keeps its export and expression context and reads the current import only on a call", async () => {
  for (const definition of [
    `export ${navigate}`,
    `export const ex=${navigate}`,
    `export const ex=[null,${navigate}][1]`,
    `export const ex=(()=>{return ${navigate}})()`,
  ]) {
    const source = patch(`${navigationLabel}${definition};
      const escapeId = id => id;
      let reads = 0;
      var u = { get I() { reads++; return (rail, marker) => rail.aligned = marker; } };
      export const readCount = () => reads;`);
    const { ex, readCount } = await import(`data:text/javascript,${encodeURIComponent(source)}`);
    assert.equal(readCount(), 0);
    const rail = { ownerDocument: {}, contains: () => true, querySelector: () => "marker" };
    ex(rail, "first");
    ex(rail, "next");
    assert.equal(rail.aligned, "marker");
    assert.equal(readCount(), 2);
  }
});

test("message navigation measures after layout and only scrolls the current clipped marker", () => {
  const markers = { first: { isConnected: true }, next: { isConnected: true } };
  const document = { activeElement: null };
  let scrubbing = false;
  const rail = {
    ownerDocument: document,
    contains: (item) => Object.values(markers).includes(item),
    querySelector(selector) {
      if (selector === '[data-scrub-target="true"]') return scrubbing ? markers.next : null;
      return markers[selector.match(/id="(\w+)"/)[1]] ?? null;
    },
  };
  const observers = [];
  class Visibility {
    constructor(deliver, options) {
      assert.equal(options.root, rail);
      this.deliver = deliver;
      observers.push(this);
    }
    observe(marker) { this.marker = marker; }
    disconnect() { this.disconnected = true; }
    notify(...positions) {
      this.deliver(positions.map(([top, bottom]) => ({
        rootBounds: { top: 0, bottom: 60, height: 60 },
        boundingClientRect: { top, bottom, height: bottom - top },
      })));
    }
  }
  const scrolled = [];
  const align = runInNewContext(`${patch(navigation)};ex`, {
    IntersectionObserver: Visibility, escapeId: (id) => id,
    u: { I: (container, marker) => { assert.equal(container, rail); scrolled.push(marker); } },
  });

  align(rail, "first");
  align(rail, "first");
  assert.equal(observers.length, 1);
  assert.equal(observers[0].marker, markers.first);
  assert.deepEqual(scrolled, []);
  align(rail, "next");
  assert.equal(observers[0].disconnected, true);
  observers[0].notify([-4, 12]);
  observers[1].notify([-4, 12], [1, 59]);
  assert.deepEqual(scrolled, []);
  align(rail, "next");
  observers.at(-1).notify([48, 64]);
  assert.deepEqual(scrolled, [markers.next]);

  align(rail, "first");
  scrubbing = true;
  observers.at(-1).notify([-3, 15]);
  scrubbing = false;
  align(rail, "first");
  markers.first.isConnected = false;
  observers.at(-1).notify([-3, 15]);
  assert.deepEqual(scrolled, [markers.next]);
  align(rail, "next");
  const waiting = observers.at(-1);
  document.activeElement = markers.next;
  align(rail, "next");
  assert.equal(waiting.disconnected, true);
  waiting.notify([-3, 15]);
  assert.deepEqual(scrolled, [markers.next, markers.next]);

  document.activeElement = null;
  align(rail, "next");
  observers.at(-1).notify([0, 0]);
  assert.deepEqual(scrolled, [markers.next, markers.next, markers.next]);
});

const scrollController = [
  'function restore(r,mode,prev,saved,fallback,anchor,footer,store,tracer,k,readBack){',
  'let t=null!=prev&&(!prev.active||"top"===prev.scrollOrigin),n=null==saved?fallback:Math.max(0,r.scrollHeight-r.clientHeight-saved);',
  '"top"===mode?n=Math.max(0,r.scrollHeight-r.clientHeight):t&&null!=anchor.current&&(n=anchor.current+(footer()?.getHeightPx()??0)),store(n),(0,tracer.S)("threadScroll.restore",()=>(0,k.it)(r,n)),readBack((0,k.IA)(r))}',
  'function resize(view,previous,mode,pinned,follow,k,scroll){let s=previous!==view.clientHeight;if(previous=view.clientHeight,s&&0===previous)return "zero";',
  'let p=(0,k.IA)(view);if("top"===mode&&pinned.current[mode])return void scroll(Math.max(0,view.scrollHeight-view.clientHeight),"instant");',
  'if("bottom"===mode&&!follow)return [s,previous,p];return p}const fields={previousViewportHeightPx:"threadScroll.initialRead"};',
].join("");

test("scroll restoration selects the native distance before reading dimensions and reads back after scrolling", () => {
  const patched = patch(scrollController);
  const run = (source, scenario) => {
    const restore = runInNewContext(`${source};restore`);
    const { mode, previous, saved, anchor } = scenario;
    let reads = 0;
    let height = 180;
    const view = { get scrollHeight() { reads++; return height; }, clientHeight: 100, scrollTop: 0 };
    const trace = [];
    const k = {
      it(el, distance) { el.scrollTop = Math.round(Math.max(0, el.scrollHeight - el.clientHeight - distance)); },
      IA(el) { return Math.max(0, el.scrollHeight - el.clientHeight - el.scrollTop); },
    };
    restore(view, mode, previous, saved, 33, { current: anchor }, () => ({ getHeightPx: () => 8 }),
      (n) => { trace.push(n); height += 40; }, { S: (_label, fn) => fn() }, k, (n) => trace.push(n));
    return { trace, scrollTop: view.scrollTop, reads };
  };
  for (const scenario of [
    { mode: "top", previous: null, saved: 17, anchor: null, savedReads: 1 },
    { mode: "bottom", previous: { active: false }, saved: 17, anchor: 0, savedReads: 1 },
    { mode: "bottom", previous: null, saved: 17.5, anchor: null, savedReads: 0 },
    { mode: "bottom", previous: null, saved: null, anchor: null, savedReads: 0 },
  ]) {
    const before = run(scrollController, scenario);
    const after = run(patched, scenario);
    assert.deepEqual(after, { ...before, reads: before.reads - scenario.savedReads });
  }
});

test("resize reads viewport height once per comparison and preserves pinned scrolling and zero-height handling", () => {
  const resize = runInNewContext(`${patch(scrollController)};resize`);
  let height = 80;
  let reads = 0;
  let distanceReads = 0;
  const view = { scrollHeight: 200, get clientHeight() { reads++; return height; } };
  const k = { IA() { distanceReads++; return 41; } };
  const scrolls = [];
  const scrolled = (...args) => scrolls.push(args);
  const pinned = { current: { top: true } };
  assert.deepEqual(Array.from(resize(view, 80, "bottom", pinned, false, k, scrolled)), [false, 80, 41]);
  assert.equal(reads, 1);
  height = 90;
  resize(view, 80, "top", pinned, false, k, scrolled);
  assert.deepEqual(scrolls, [[110, "instant"]]);
  assert.equal(distanceReads, 1);
  height = 0;
  assert.equal(resize(view, 90, "bottom", pinned, false, k, scrolled), "zero");
  assert.equal(distanceReads, 1);
});
