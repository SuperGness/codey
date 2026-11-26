import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { readSource } from "./helpers/read-source.mjs";

const source = await readSource("public/plugin-marketplace-fix.js");

function setup({ lateBridge = false } = {}) {
  const calls = [];
  const localCalls = [];
  const timers = new Map();
  let timerId = 0;
  let interceptor;
  const response = { plugins: [{ id: "remote", name: "remote", hidden: true }] };
  const nativePromise = Promise.resolve(response);
  const electronBridge = {
    sendMessageFromView(...args) {
      calls.push(args);
      return nativePromise;
    },
  };
  const window = {
    __codeyCall(...args) {
      localCalls.push(args);
      return Promise.resolve({ plugins: [{ id: "local", name: "local", marketplace: "local" }] });
    },
    __codeySharedRuntime: {
      registerFetchInterceptor(_name, callback) { interceptor = callback; },
    },
    clearTimeout(id) { timers.delete(id); },
    setTimeout(callback, delay) {
      timers.set(++timerId, { callback, delay });
      return timerId;
    },
    dispatchEvent() {},
  };
  if (!lateBridge) window.electronBridge = electronBridge;
  const sandbox = { window, CustomEvent: class {}, Headers, Response };
  vm.runInNewContext(source, sandbox);
  return { window, calls, localCalls, timers, nativePromise, response, electronBridge, interceptor, sandbox };
}

test("chat text, paths, legacy envelopes and arbitrary objects never activate plugin IPC patches", async () => {
  const fixture = setup();
  const { window, calls, localCalls, nativePromise, response } = fixture;
  const messages = [
    { channel: "thread-update", payload: { text: "install-plugin and list-plugins", includeHidden: false } },
    { channel: "list-plugins", options: { includeHidden: false, includeRemote: false } },
    { method: "list-plugins", options: { includeHidden: false, includeRemote: false } },
    { path: "/api/plugins/list", options: { includeHidden: false, includeRemote: false } },
    { type: "invoke", payload: { request: { method: "plugin/list" } } },
    { method: "turn/start", params: { input: "plugin marketplace", includeRemote: false } },
    { method: "turn/start", payload: { method: "plugin/list" } },
    { type: "mcp-request", request: { method: "tools/call", params: { name: "plugin/install" } } },
    { description: "plugin", plugin: { includeHidden: false } },
    { payload: { text: "marketplace", file: "/workspace/plugins/example.md" } },
    { metadata: { method: "plugin/list", path: "/plugins/list" } },
    { method: "get-plugin-documentation" },
    { method: "plugin-helper/list" },
  ];
  for (const message of messages) {
    const result = window.electronBridge.sendMessageFromView(message);
    assert.equal(result, nativePromise, JSON.stringify(message));
    assert.equal(calls.at(-1)[0], message);
    assert.equal(await result, response);
  }
  const payload = { method: "plugin/list" };
  assert.equal(window.electronBridge.sendMessageFromView("turn/start", payload), nativePromise);
  assert.equal(calls.at(-1)[1], payload);
  assert.equal(localCalls.length, 0);
  assert.equal(response.plugins[0].hidden, true);
});

test("26.1120.50957 plugin RPC commands and mcp-request envelopes retain normalization and local lists", async () => {
  for (const message of [
    { method: "plugin/list", params: { cwds: ["/workspace"] } },
    { method: "plugin/installed", params: {} },
    { type: "mcp-request", request: { id: "req-1", method: "plugin/list", params: {} } },
    { type: "mcp-request", request: { id: "req-2", method: "plugin/installed", params: {} } },
  ]) {
    const { window, localCalls, calls } = setup();
    const response = await window.electronBridge.sendMessageFromView(message);
    assert.equal(localCalls.length, 1, JSON.stringify(message));
    assert.equal(localCalls[0][0], "/plugins/list");
    assert.equal(calls[0][0], message);
    assert.equal(response.plugins[0].hidden, false);
    assert.equal(response.plugins.some((plugin) => plugin.id === "local"), true);
  }
  const { window, localCalls } = setup();
  await window.electronBridge.sendMessageFromView("mcp-request", { method: "plugin/list", params: {} });
  await window.electronBridge.sendMessageFromView("mcp-request", { request: { method: "plugin/installed" } });
  assert.equal(localCalls.length, 2);
});

test("read and mutation commands patch results while only mutations trigger a refresh", async () => {
  const { window, localCalls } = setup();
  const read = await window.electronBridge.sendMessageFromView({
    type: "mcp-request",
    request: { method: "plugin/read", params: { pluginName: "remote" } },
  });
  assert.equal(read.plugins[0].hidden, false);
  assert.equal(localCalls.length, 0);
  for (const message of [
    { method: "plugin/install", params: { pluginName: "remote" } },
    { method: "plugin/uninstall", params: { pluginId: "remote" } },
    { type: "mcp-request", request: { method: "plugin/install", params: { pluginName: "remote" } } },
    { type: "mcp-request", request: { method: "plugin/uninstall", params: { pluginId: "remote" } } },
  ]) {
    await window.electronBridge.sendMessageFromView(message);
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(localCalls.length, 4);
});

test("late bridge retries and reinjection preserve narrow request matching", async () => {
  const { window, timers, electronBridge, nativePromise, calls, sandbox } = setup({ lateBridge: true });
  assert.equal(timers.size, 1);
  const retry = [...timers.values()][0];
  assert.equal(retry.delay, 50);
  window.electronBridge = electronBridge;
  retry.callback();
  const wrapped = window.electronBridge.sendMessageFromView;
  vm.runInNewContext(source, sandbox);
  assert.equal(window.electronBridge.sendMessageFromView, wrapped);
  assert.equal(window.electronBridge.sendMessageFromView({ method: "turn/start", prompt: "plugin" }), nativePromise);
  const req = { type: "mcp-request", request: { method: "plugin/list", params: {} } };
  const result = await window.electronBridge.sendMessageFromView(req);
  assert.equal(calls[1][0], req);
  assert.equal(result.plugins[0].hidden, false);
});

test("fetch matching ignores hostnames, query strings, fragments and chat payload content", async () => {
  const { interceptor, localCalls } = setup();
  const nativePromise = Promise.resolve({
    headers: { get() { throw new Error("unrelated response must remain untouched"); } },
  });
  for (const [url, body] of [
    ["https://plugins.example/chat", { prompt: "plugin" }],
    ["//plugins/chat", { prompt: "marketplace" }],
    ["https://example.com/chat?redirect=/ps/plugins/list#marketplace", { prompt: "plugin/list" }],
    ["https://example.com/plugin-docs", { method: "turn/start", payload: { method: "plugin/list" } }],
    ["https://example.com/rpc", { method: "tools/call", params: { method: "plugin/list" } }],
    ["https://example.com/api/list-plugins", { description: "legacy route" }],
  ]) {
    const result = interceptor(() => nativePromise, url, { body: JSON.stringify(body) });
    assert.equal(result, nativePromise, url);
  }
  assert.equal(localCalls.length, 0);
});

test("plugin fetch routes patch JSON across /ps/plugins/* and /marketplace/list", async () => {
  const { interceptor, localCalls } = setup();
  const native = () => Promise.resolve(new Response(JSON.stringify({ plugins: [{ id: "remote", hidden: true }] }), {
    headers: { "content-type": "application/json", "content-length": "1" },
  }));
  for (const url of [
    "https://chatgpt.com/backend-api/wham/ps/plugins/list",
    "/backend-api/ps/plugins/installed",
    "/ps/plugins/search?query=figma",
    "/ps/plugins/workspace/created",
    "/api/marketplace/list",
  ]) {
    const result = await interceptor(native, url);
    assert.equal(result.headers.has("content-length"), false);
    const data = await result.json();
    assert.equal(data.plugins[0].hidden, false);
    assert.equal(data.plugins.some((plugin) => plugin.id === "local"), true);
  }
  assert.equal(localCalls.length, 5);
});

test("local plugins are merged only into plugin lists, not other nested arrays", async () => {
  const { window } = setup();
  window.__codeyLocalPlugins = [{ id: "local@codey", name: "local", marketplaceName: "codey" }];
  const patched = window.__codeyPatchPluginResponse({
    marketplaces: [{
      name: "openai-curated",
      plugins: [{ id: "a@openai-curated", name: "a", interface: { capabilities: ["Read"], screenshots: [] } }],
    }],
  });
  assert.equal(patched.marketplaces.length, 1);
  const [plugin, local] = patched.marketplaces[0].plugins;
  assert.equal(JSON.stringify(plugin.interface.capabilities), "[\"Read\"]");
  assert.equal(plugin.interface.screenshots.length, 0);
  assert.equal(local.id, "local@codey");
  const flat = window.__codeyPatchPluginResponse([{ id: "remote", name: "remote", hidden: true }]);
  assert.equal(flat.some((entry) => entry.id === "local@codey"), true);
});
