import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");
const resolverStart = source.indexOf("  const appServerManagerResolverFromModule =");
const resolverEnd = source.indexOf("  window.__codeyAppServerManagerResolverFromModule =", resolverStart);
assert.ok(resolverStart > 0 && resolverEnd > resolverStart);
const resolverSource = source.slice(resolverStart, resolverEnd);
const start = source.indexOf("  const remoteCreationInputsFactoryFromModule =");
const end = source.indexOf("  window.__codeyRemoteControl = remoteControl;", start);
assert.ok(start > 0 && end > start);
const remoteSource = source.slice(start, end + "  window.__codeyRemoteControl = remoteControl;".length);

function fixture({ compatible = true, ready = true, splitModules = true, managerAvailable = true, brokenAsset = false, nativeTier = false } = {}) {
  const calls = [];
  const imports = [];
  const overrides = new Map();
  const scope = {
    node: { familyBindings: new Map() },
    get: (key, params) => key === "rpc" ? { forHost: () => manager } : overrides.has(key) ? overrides.get(key).get(`local:${params.cwd ?? ""}`) : "native-client",
  };
  const addOverride = (values, family = { kind: "signal-family" }) => {
    const bindings = new Map(Object.entries(values));
    scope.node.familyBindings.set(family, bindings); overrides.set(family, bindings);
    return family;
  };
  class DynamicTools {}
  class Inputs {
    constructor(params) { this.params = params; }
    async readCreationInputs(...args) { calls.push(["inputs", this.params.requestClient, ...args]); return "native-inputs"; }
  }
  function factory(scope,hostId){return new Inputs({scope:scope,hostId:hostId,requestClient:scope.get("client"),dynamicTools:new DynamicTools()});}
  const manager = {
    async sendRequest(method, params) {
      calls.push([method, params]);
      return { config: { model: "route/model", model_reasoning_effort: "ultra", service_tier: "priority", privateKey: "fixture-secret" } };
    },
    async resumeConversation(args) { calls.push(["resume", args]); return { status: ready ? "ready" : "not-ready" }; },
    async startConversation(args, options) {
      calls.push(["create", args]);
      assert.equal(await options.readThreadCreationInputs(args, "canonical"), "native-inputs");
      return { status: "created", conversationId: "thread", firstTurn: { status: "accepted" } };
    },
  };
  async function readServiceTier(scope, host, model) {
    // Native function discovery uses these stable semantic markers.
    const marker = "Failed to read service tier for request";
    calls.push(["tier", host, model]);
    return scope && marker ? "default" : { service_tier: null };
  }
  function resolver(scope, hostId) {
    const rpc = scope.get("rpc");
    if (rpc == null) throw new Error("AppServerManager RPC is not connected");
    return rpc.forHost(hostId);
  }
  const initialUrl = "app://-/assets/app-initial-test.js";
  const sharedUrl = "app://-/assets/app-shared-test.js";
  const brokenUrl = "app://-/assets/app-initial-stale.js";
  const modules = {
    [initialUrl]: { ...(compatible ? { factory } : {}), ...(nativeTier ? { readServiceTier } : {}), ...(!splitModules && managerAvailable ? { resolver } : {}) },
    [sharedUrl]: splitModules && managerAvailable ? { resolver } : {},
  };
  const context = vm.createContext({
    window: { __codeyImportCodexAsset: async url => {
      imports.push(url);
      if (url === brokenUrl) throw new Error("stale asset");
      return modules[url];
    } },
    disposed: false, Date,
    getCodexSessionController: async () => ({ manager }),
    discoverCodexAppAssetUrls: async () => [...(brokenAsset ? [brokenUrl] : []), initialUrl, sharedUrl],
    appServerManagerFromReact: resolver => resolver(scope, "local"),
  });
  vm.runInContext(resolverSource, context);
  vm.runInContext(remoteSource, context);
  return { context, calls, imports, manager, Inputs, scope, addOverride, run: args => context.window.__codeyRemoteControl(args, Date.now() + 10000) };
}

test("remote subscription hydrates the native conversation before waiting for its owner", async () => {
  const { calls, run } = fixture();
  assert.equal((await run({ action: "resume", threadId: "thread" })).status, "ok");
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "resume");
  assert.equal(calls[0][1].conversationId, "thread");
  assert.equal(calls[0][1].model, null);
  assert.equal(calls[0][1].workspaceRoots.length, 0);
});

test("draft defaults read native project config without creating a thread or exposing credentials", async () => {
  const { calls, run } = fixture();
  const result = await run({ action: "defaults", cwd: "E:/code/codey" });
  assert.deepEqual(JSON.parse(JSON.stringify(result)), { model: "route/model", effort: "ultra", serviceTier: "priority" });
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "config/read");
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0][1])), { includeLayers: false, cwd: "E:/code/codey" });
  assert.ok(!JSON.stringify(result).includes("fixture-secret"));
});

test("draft defaults honor the desktop speed preference ahead of config", async () => {
  const { calls, run } = fixture({ nativeTier: true });
  assert.equal((await run({ action: "defaults", cwd: "E:/code/codey" })).serviceTier, "default");
  assert.deepEqual(calls[1], ["tier", "local", "route/model"]);
});

test("draft defaults inherit the desktop in-memory selection when startup config overrides persisted model changes", async () => {
  const { addOverride, calls, run } = fixture({ nativeTier: true });
  addOverride({
    "local:E:/code/codey": { model: "route/selected", reasoningEffort: "max", profile: null },
    "local:": { model: "route/global", reasoningEffort: "high", profile: "work" },
  });
  addOverride({ "local:E:/code/codey": { model: "unrelated", reasoningEffort: "low", profile: null, extra: true } });
  addOverride({ "local:E:/code/codey": { model: "unmounted", reasoningEffort: "low", profile: null } }, { kind: "readable-family" });
  assert.deepEqual(JSON.parse(JSON.stringify(await run({ action: "defaults", cwd: "E:/code/codey" }))), { model: "route/selected", effort: "max", serviceTier: "default" });
  assert.deepEqual(calls[1], ["tier", "local", "route/selected"]);
  assert.equal((await run({ action: "defaults", cwd: "E:/other" })).model, "route/global");
});

test("native default discovery reads parent scopes, skips missing values and rejects ambiguous selections", async () => {
  const { scope, addOverride, run } = fixture();
  addOverride({ "local:": null });
  addOverride({ "local:": { model: "ignored", reasoningEffort: "low" } });
  assert.equal((await run({ action: "defaults" })).model, "route/model");
  addOverride({ "local:": { model: "route/parent", reasoningEffort: "medium", profile: null } });
  scope.chain = new Map([["parent", scope.node]]); scope.node = { familyBindings: new Map() };
  assert.equal((await run({ action: "defaults" })).model, "route/parent");
  addOverride({ "local:": { model: "route/conflict", reasoningEffort: "low", profile: null } });
  await assert.rejects(run({ action: "defaults" }), /不明确/);
});

test("missing, failed and late default reads never create a thread", async () => {
  const { calls, manager, context, run } = fixture();
  manager.sendRequest = undefined;
  await assert.rejects(run({ action: "defaults" }), /默认设置/);
  manager.sendRequest = async () => ({ config: null });
  await assert.rejects(run({ action: "defaults" }), /格式不兼容/);
  manager.sendRequest = async () => { throw new Error("offline"); };
  await assert.rejects(run({ action: "defaults" }), /offline/);
  manager.sendRequest = async () => ({ config: { model: null, model_reasoning_effort: null } });
  assert.deepEqual(JSON.parse(JSON.stringify(await run({ action: "defaults" }))), { serviceTier: "default" });
  manager.sendRequest = async () => { context.disposed = true; return { config: {} }; };
  await assert.rejects(run({ action: "defaults" }), /过期/);
  assert.equal(calls.length, 0);
});

test("first submission uses native creation inputs, tools and automatic title generation", async () => {
  const { calls, imports, run } = fixture();
  const params = { input: [{ type: "text", text: "开始任务" }], cwd: "E:/code/codey" };
  const result = await run({ action: "create", params });
  assert.equal(result.id, "thread"); assert.equal(result.firstTurn, "accepted");
  assert.equal(calls[0][1], params);
  assert.equal(calls[0][1].initialTitle, undefined);
  assert.deepEqual(calls[1], ["inputs", "native-client", params, "canonical"]);
  assert.ok(imports.some(url => url.includes("app-shared-")), "the manager resolver can be exported separately from the creation reader");
});

test("creation remains compatible with colocated exports and skips stale assets", async () => {
  for (const options of [{ splitModules: false }, { brokenAsset: true }]) {
    const { calls, run } = fixture(options);
    assert.equal((await run({ action: "create", params: {} })).id, "thread");
    assert.equal(calls.filter(call => call[0] === "create").length, 1);
  }
});

test("unsupported native interfaces and unloaded conversations fail without an external URL", async () => {
  const unsupported = fixture({ compatible: false });
  await assert.rejects(unsupported.run({ action: "create", params: {} }), /暂不支持/);
  assert.equal(unsupported.calls.length, 0);
  const missingManager = fixture({ managerAvailable: false });
  await assert.rejects(missingManager.run({ action: "create", params: {} }), /暂不支持/);
  assert.equal(missingManager.calls.length, 0);
  await assert.rejects(fixture({ ready: false }).run({ action: "resume", threadId: "thread" }), /尚未就绪/);
});

test("expired and disposed requests cannot submit after asynchronous discovery", async () => {
  const { context, calls, run } = fixture();
  context.window.__codeyImportCodexAsset = async () => { context.disposed = true; return {}; };
  await assert.rejects(run({ action: "create", params: {} }));
  context.disposed = false;
  await assert.rejects(context.window.__codeyRemoteControl({ action: "resume", threadId: "thread" }, Date.now() - 1), /过期/);
  context.disposed = true;
  await assert.rejects(run({ action: "resume", threadId: "thread" }), /过期/);
  assert.equal(calls.length, 0);
});

test("uncertain creation is never automatically retried", async () => {
  const { calls, manager, run } = fixture();
  manager.startConversation = async () => { calls.push(["create"]); return { status: "outcome-unknown" }; };
  await assert.rejects(run({ action: "create", params: {} }), /勿重复发送/);
  assert.equal(calls.length, 1);
});

test("a late native settings read cannot continue creating a conversation after disposal", async () => {
  const { context, Inputs, run } = fixture();
  Inputs.prototype.readCreationInputs = async () => { context.disposed = true; return "native-inputs"; };
  await assert.rejects(run({ action: "create", params: {} }), /过期/);
});
