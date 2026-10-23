import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";
import { loadStartupPatchTemplate } from "./helpers/startup-patch.mjs";

const injectionSource = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

const managerSource = `class Manager {
  constructor(hostId="local") {
    this.hostId=hostId;
    this.disposed=false;
    this.value={turns:[]};
    const owner=this;
    this.threadStore={getConversation(id){owner.lastStoreThis=this;owner.lastId=id;return owner.value}};
    this.inactiveThreadUnsubscriber={clearConversationStreamOwnership(){}};
  }
  getHostId(){return this.hostId}
  getConversation(e){return this.threadStore.getConversation(e)}
  getStreamRole(){return {role:"owner"}}
  sendRequest(){}
  updateConversationState(){}
  clear(){this.inactiveThreadUnsubscriber.clearConversationStreamOwnership()}
};globalThis.Manager=Manager;`;

async function fixture() {
  const errors = [];
  const context = vm.createContext({ URL, console: { error(...args) { errors.push(args); } } });
  vm.runInContext(await loadStartupPatchTemplate(), context);
  const patch = (source) => context.__codeyRendererPatchEntry("app://-/assets/app-initial-manager.js", source) ?? source;
  return { context, patch, errors };
}

test("native getConversation registers live local managers preserving result and receiver", async () => {
  const { context, patch } = await fixture();
  const patched = patch(managerSource);
  assert.notEqual(patched, managerSource);
  assert.equal(patch(patched), patched, "registration patch is idempotent");
  vm.runInContext(patched, context);
  const first = new context.Manager();
  assert.equal(first.getConversation("thread-1"), first.value);
  assert.equal(first.lastId, "thread-1");
  assert.equal(first.lastStoreThis, first.threadStore);
  assert.equal(context.__codeyAppServerManagers.get("local"), first);
  const second = new context.Manager();
  assert.equal(second.getConversation("thread-2"), second.value);
  assert.equal(context.__codeyAppServerManagers.get("local"), second);
  first.disposed = true;
  assert.equal(first.getConversation("thread-1"), first.value);
  assert.equal(context.__codeyAppServerManagers.get("local"), second);
  const remote = new context.Manager("remote");
  assert.equal(remote.getConversation("remote-thread"), remote.value);
  assert.equal(context.__codeyAppServerManagers.size, 1);
  assert.equal(context.__codeyAppServerManagers.get("local"), second);
});

test("registration failures leave the native conversation read intact", async () => {
  const { context, patch } = await fixture();
  vm.runInContext(patch(managerSource), context);
  const manager = new context.Manager();
  manager.getHostId = () => { throw new Error("disposed host"); };
  assert.equal(manager.getConversation("thread-1"), manager.value);
  assert.equal(context.__codeyAppServerManagers, undefined);
  context.__codeyAppServerManagers = {};
  manager.getHostId = () => "local";
  assert.equal(manager.getConversation("thread-2"), manager.value);
});

test("nonmatching and ambiguous manager sources remain unchanged", async () => {
  const { patch } = await fixture();
  const unmatched = managerSource.replace("return this.threadStore.getConversation(e)", "return this.threadStore.lookup(e)");
  assert.equal(patch(unmatched), unmatched);
  const ambiguous = `${managerSource}\nclass Other {getConversation(e){return this.threadStore.getConversation(e)}}`;
  assert.equal(patch(ambiguous), ambiguous);
  const unrelated = "class Store {getConversation(e){return this.threadStore.getConversation(e)}}";
  assert.equal(patch(unrelated), unrelated);
});

function cachedDeletionFixture(scope) {
  const body = new FakeElementCore();
  body.__reactFiber$cachedManager = { memoizedState: { scope }, return: null };
  const document = {
    body,
    documentElement: new FakeElementCore(),
    scripts: [{ src: "app://-/assets/app-initial-private-registry.js" }],
    getElementById: () => new FakeElementCore(),
    querySelector: () => null,
    querySelectorAll: () => [],
  };
  const resourceReads = [];
  const window = {
    setTimeout: () => 1,
    clearTimeout() {},
    __codeyImportCodexAsset: async () => {
      resourceReads.push("import");
      return {};
    },
    get fetch() {
      resourceReads.push("fetch");
      throw new Error("asset fetch is unavailable");
    },
  };
  vm.runInNewContext(injectionSource, {
    document,
    HTMLElement: FakeElementCore,
    location: { pathname: "/", search: "" },
    MutationObserver: class { observe() {} },
    URL,
    URLSearchParams,
    window,
  });
  return { window, resourceReads };
}

for (const inherited of [false, true]) {
  test(`message deletion uses a mounted ${inherited ? "parent" : "local"} signal cache without registry exports or fetch`, async () => {
    const events = [];
    const retainedTurn = { turnId: "turn-2", status: "completed" };
    const conversation = {
      turns: [{ turnId: "turn-1", status: "completed" }, retainedTurn],
      resumeState: "resumed",
    };
    const manager = {
      requestClient: {
        hostId: "local",
        enqueueRequest() { events.push("enqueueRequest"); },
        createRequest() { events.push("createRequest"); },
        onResult() { events.push("onResult"); },
      },
      getHostId() { return "local"; },
      getConversation(id) { assert.equal(id, "session-1"); return conversation; },
      getStreamRole() { return { role: "owner" }; },
      async sendRequest(method, params) {
        assert.equal(method, "thread/unsubscribe");
        assert.equal(params.threadId, "session-1");
        events.push("unsubscribe");
      },
      updateConversationState(id, update) {
        assert.equal(id, "session-1");
        update(conversation);
        events.push("update");
      },
      inactiveThreadUnsubscriber: {
        clearConversationStreamOwnership(id) {
          assert.equal(id, "session-1");
          events.push("clear");
        },
      },
    };
    const family = { kind: "signal-family" };
    const node = { familyBindings: new Map([
      [{ kind: "signal-family" }, new Map()],
      [{ kind: "signal-family" }, new Map([["remote", {}]])],
      [family, new Map([["local", {}]])],
    ]) };
    const reads = [];
    const scope = {
      query: null,
      node: inherited ? { familyBindings: new Map() } : node,
      chain: new Map(inherited ? [["parent", node]] : []),
      get(candidate, hostId) {
        reads.push([candidate, hostId]);
        assert.equal(candidate, family, "discovery must not initialize an unused signal");
        assert.equal(hostId, "local", "discovery must not initialize a remote manager");
        return manager;
      },
      set() { events.push("set"); },
      watch() { events.push("watch"); },
      when() { events.push("when"); },
    };
    const { window, resourceReads } = cachedDeletionFixture(scope);

    const controller = await window.__codeyLoadCodexSessionController({ feature: "deleteMessages" });
    assert.equal(controller.manager, manager);
    assert.equal((await window.__codeyLoadCodexSessionController({ feature: "deleteMessages" })).manager, manager);
    assert.equal(window.__codeyPageCapabilities.deleteMessages.status, "available");
    assert.deepEqual(reads, [[family, "local"]], "subsequent calls reuse the verified manager");
    assert.deepEqual(resourceReads, [], "cached discovery must work without importing a registry or reading fetch");
    assert.deepEqual(events, [], "discovery must not send probes, initialize signals, or mutate state");

    await controller.prepareMessageDeletion("session-1");
    await controller.finishMessageDeletion("session-1", ["turn-1"]);
    assert.deepEqual(events, ["unsubscribe", "update", "clear"]);
    assert.deepEqual(conversation.turns, [retainedTurn]);
    assert.equal(conversation.resumeState, "needs_resume");
  });
}
