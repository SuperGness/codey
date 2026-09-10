import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { flushMicrotasks } from "./helpers/flush.mjs";
import vm from "node:vm";
import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/renderer-inject.js", import.meta.url), "utf8");
const flush = flushMicrotasks;

test("subagent headers await native thread metadata and discard stale responses", async () => {
  const header = new FakeElementCore("div", { connected: true });
  const select = (id) => {
    header.__reactFiber$test = { return: {
      memoizedProps: { backAriaLabel: "返回子代理列表", onBack() {}, seed: id },
      return: { memoizedProps: { conversationId: id } },
    } };
  };
  select("child");
  let now = 2000;
  const pending = [];
  const manager = {
    readThread(id, options) {
      assert.equal(options.includeTurns, false);
      return new Promise((resolve) => pending.push({ id, resolve }));
    },
  };
  const document = {
    documentElement: new FakeElementCore("html"),
    querySelectorAll: (selector) => selector === ".h-12.shrink-0.border-b" && header.isConnected ? [header] : [],
    createElement: (tag) => new FakeElementCore(tag),
  };
  const window = {
    __codeyRendererCoreLoaded: true,
    __codeySessionToolsInjectLoaded: true,
    __codeyLoadCodexSessionController: async () => ({ manager }),
    clearTimeout() {},
    setTimeout: () => 1,
  };
  vm.runInNewContext(source, {
    window, document, HTMLElement: FakeElementCore, Date: { now: () => now },
    MutationObserver: class { observe() {} disconnect() {} },
  });
  await flush();
  assert.equal(pending[0].id, "child");
  assert.equal(header.children[0].title, "模型：待获取 · 推理强度：待获取");
  pending.shift().resolve({ thread: { model: "openai/gpt-5.6-luna", reasoningEffort: "xhigh" } });
  await flush();
  const label = header.children[0];
  assert.equal(label.title, "模型：openai/gpt-5.6-luna · 推理强度：xhigh");
  assert.equal(label.children[0].textContent, "gpt-5.6-luna");
  assert.equal(label.children[1].textContent, "· xhigh");
  assert.equal(label.getAttribute("aria-label"), label.title);
  window.__codeySyncSubagentHeaders();
  assert.equal(pending.length, 0, "unchanged scans do not flood native RPC");

  now += 2000;
  window.__codeySyncSubagentHeaders();
  const stale = pending.shift();
  select("other-child");
  window.__codeySyncSubagentHeaders();
  await flush();
  stale.resolve({ thread: { model: "stale-model", reasoningEffort: "high" } });
  pending.shift().resolve({ thread: { model: "gpt-6-astra", reasoningEffort: "none" } });
  await flush();
  assert.equal(header.children.length, 1);
  assert.equal(header.children[0].title, "模型：gpt-6-astra · 推理强度：none");
  header.isConnected = false;
  window.__codeySyncSubagentHeaders();
  assert.equal(header.children.length, 0);
  window.__codeySubagentHeaderCleanup();
});
