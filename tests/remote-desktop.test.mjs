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

function fixture({ compatible = true, ready = true, splitModules = true, managerAvailable = true, brokenAsset = false } = {}) {
  const calls = [];
  const imports = [];
  const scope = { get: key => key === "rpc" ? { forHost: () => manager } : "native-client" };
  class DynamicTools {}
  class Inputs {
    constructor(params) { this.params = params; }
    async readCreationInputs(...args) { calls.push(["inputs", this.params.requestClient, ...args]); return "native-inputs"; }
  }
  function factory(scope,hostId){return new Inputs({scope:scope,hostId:hostId,requestClient:scope.get("client"),dynamicTools:new DynamicTools()});}
  const manager = {
    async resumeConversation(args) { calls.push(["resume", args]); return { status: ready ? "ready" : "not-ready" }; },
    async startConversation(args, options) {
      calls.push(["create", args]);
      assert.equal(await options.readThreadCreationInputs(args, "canonical"), "native-inputs");
      return { status: "created", conversationId: "thread", firstTurn: { status: "accepted" } };
    },
  };
  function resolver(scope, hostId) {
    const rpc = scope.get("rpc");
    if (rpc == null) throw new Error("AppServerManager RPC is not connected");
    return rpc.forHost(hostId);
  }
  const initialUrl = "app://-/assets/app-initial-test.js";
  const sharedUrl = "app://-/assets/app-shared-test.js";
  const brokenUrl = "app://-/assets/app-initial-stale.js";
  const modules = {
    [initialUrl]: { ...(compatible ? { factory } : {}), ...(!splitModules && managerAvailable ? { resolver } : {}) },
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
  return { context, calls, imports, manager, Inputs, run: args => context.window.__codeyRemoteControl(args, Date.now() + 10000) };
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
