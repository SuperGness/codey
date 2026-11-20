import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { FakeElementCore } from "./helpers/fake-element.mjs";
import { flushMicrotasks } from "./helpers/flush.mjs";

const source = readFileSync(new URL("../public/plugin-status-inject.js", import.meta.url), "utf8");
const at = Date.parse("2026-11-20T12:00:00+08:00");
const panels = [
  { id: "balance", generation: "v1", name: "<b>中转站余额</b>" },
  { id: "service", generation: "v1", name: "服务状态" },
];
const reading = (value) => ({ state: "ready", value, observedAt: at });
const row = (id, reading, label = id) => ({ id, label, reading });
const reply = (...rows) => ({ status: "ok", panel: { rows } });
const inClass = (node, name) => node.className?.split(" ").includes(name)
  ? node : node.children.map((child) => inClass(child, name)).find(Boolean);

function fixture(respond, connected = true) {
  let active;
  class Element extends FakeElementCore {
    replaceChildren(...children) {
      for (const old of [...this.children]) old.remove();
      this.append(...children);
    }
    focus() { active = this; }
  }
  const html = new Element("html", { connected: true });
  const head = new Element("head");
  const body = new Element("body");
  html.append(head, body);
  const anchor = new Element("button");
  anchor.id = "codey-settings-button";
  anchor.setAttribute("aria-label", "打开 Codey 配置");
  body.append(anchor);
  const document = new Element("document");
  Object.assign(document, {
    head, body, createElement: (tag) => new Element(tag),
    getElementById: (id) => html.querySelector("#" + id),
  });
  const window = new Element("window");
  Object.assign(window, { innerWidth: 600, innerHeight: 430 });
  vm.runInNewContext(source, { window, document });
  const calls = [];
  let settings = 0;
  const ui = window.__codeyCreatePluginStatusPanels({
    openSettings: () => { settings++; },
    connected: () => connected,
    callBridge: async (path, input) => {
      calls.push([path, input.id, input.generation]);
      return respond(path, input);
    },
  });
  const panel = () => document.getElementById("codey-plugin-status");
  return { anchor, ui, body, calls, document, panel, active: () => active, settings: () => settings };
}

test("the original Codey entry opens settings when no panels are enabled or the bridge is offline", async () => {
  const f = fixture(() => ({ status: "ok", panels: [] }));
  await f.ui.open(f.anchor);
  assert.equal(f.settings(), 1);
  assert.deepEqual(f.calls, [["/plugin-status/list", undefined, undefined]]);
  assert.equal(f.panel(), null);
  assert.equal(f.body.querySelectorAll("button").length, 1);

  const offline = fixture(() => assert.fail("no offline request"), false);
  await offline.ui.open(offline.anchor);
  assert.equal(offline.settings(), 1);
});

test("one entry shows distinct cards and literal text, with controls and stale zero preserved after a failed refresh", async () => {
  let failed = false;
  const f = fixture((path, input) => path.endsWith("/list") ? { status: "ok", panels }
    : input.id === "balance" ? failed ? { status: "failed", code: "timeout" } : reply(
      row("zero", reading("0.00")),
      row("unknown", { state: "unavailable", message: "待配置" }),
      row("cached", { state: "stale", message: "查询超时", value: "4.20", observedAt: at }),
    ) : reply(row("service", reading("就绪"))));
  await f.ui.open(f.anchor);
  await flushMicrotasks();
  const panel = f.panel();
  const [balance, service] = inClass(panel, "ps-list").children;
  assert.equal(f.body.querySelectorAll("#codey-settings-button").length, 1);
  assert.equal(f.active(), panel);
  assert.equal(balance.children[0].textContent, "<b>中转站余额</b>");
  assert.equal(balance.querySelector("b"), null);
  const [, zero, unavailable, stale] = balance.children;
  assert.equal(inClass(zero, "ps-value").textContent, "0.00");
  assert.equal(inClass(unavailable, "ps-value").textContent, "—");
  assert.equal(inClass(stale, "ps-value").textContent, "4.20");
  assert.match(inClass(stale, "ps-warning").textContent, /^查询超时 · 上次/);
  assert.equal(inClass(service, "ps-value").textContent, "就绪");

  failed = true;
  inClass(panel, "ps-refresh").click();
  await flushMicrotasks();
  assert.match(balance.children[1].textContent, /查询超时 · 以下为上次读数/);
  assert.equal(inClass(balance, "ps-value").textContent, "0.00");
  inClass(panel, "ps-footer").children[1].click();
  assert.equal(f.settings(), 1);
  assert.equal(f.panel(), null);
  assert.equal(f.anchor.getAttribute("aria-expanded"), null);
});

test("refresh removes disabled cards and discards readings from a prior activation", async () => {
  let current = panels;
  const f = fixture((path, input) => path.endsWith("/list") ? { status: "ok", panels: current }
    : input.generation === "v1" ? reply(row("old", reading("旧值"))) : { status: "failed", code: "busy" });
  await f.ui.open(f.anchor);
  await flushMicrotasks();
  const panel = f.panel();
  current = [{ ...panels[0], generation: "v2" }];
  inClass(panel, "ps-refresh").click();
  await flushMicrotasks();
  assert.equal(inClass(panel, "ps-list").children.length, 1);
  assert.equal(inClass(panel, "ps-value"), undefined);
  assert.match(inClass(panel, "ps-warning").textContent, /正在查询/);
  current = [];
  inClass(panel, "ps-refresh").click();
  await flushMicrotasks();
  assert.equal(f.panel(), null);
  assert.equal(f.active(), f.anchor);
});

test("Escape closes the popover and late updates cannot affect a new one", async () => {
  let finish;
  let first = true;
  const f = fixture((path) => path.endsWith("/list") ? { status: "ok", panels: [panels[0]] }
    : first ? new Promise((resolve) => { finish = resolve; }) : reply(row("new", reading("新值"))));
  await f.ui.open(f.anchor);
  f.document.dispatchEvent({ type: "keydown", key: "Escape", preventDefault() {}, stopPropagation() {} });
  assert.equal(f.panel(), null);
  assert.equal(f.active(), f.anchor);
  first = false;
  await f.ui.open(f.anchor);
  finish(reply(row("old", reading("旧值"))));
  await flushMicrotasks();
  assert.equal(inClass(f.panel(), "ps-value").textContent, "新值");
});
