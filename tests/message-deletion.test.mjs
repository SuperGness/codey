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

async function managerFixture() {
  const errors = [];
  const context = vm.createContext({ URL, console: { error(...args) { errors.push(args); } } });
  vm.runInContext(await loadStartupPatchTemplate(), context);
  const patch = (source) => context.__codeyRendererPatchEntry("app://-/assets/app-initial-manager.js", source) ?? source;
  return { context, patch, errors };
}

test("native getConversation registers live local managers preserving result and receiver", async () => {
  const { context, patch } = await managerFixture();
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
  const { context, patch } = await managerFixture();
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
  const { patch } = await managerFixture();
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

const methodsStart = injectionSource.indexOf("  const messageDeletionMethods = {");
const methodsEnd = injectionSource.indexOf("  const managerSessionController", methodsStart);
const declaredStart = injectionSource.indexOf("  const declaredMethod =");
const declaredEnd = injectionSource.indexOf("\n  //", declaredStart);
const capabilityStart = injectionSource.indexOf("  const capabilityProbes =");
const capabilityEnd = injectionSource.indexOf("  const sessionControllerCanReconcileCompletedConversation", capabilityStart);
assert.ok(methodsStart >= 0 && methodsEnd > methodsStart);
assert.ok(declaredStart >= 0 && declaredEnd > declaredStart);
assert.ok(capabilityStart >= 0 && capabilityEnd > capabilityStart);
const methods = Function(`const disposed=false, window={};
  ${injectionSource.slice(declaredStart, declaredEnd)}
  ${injectionSource.slice(capabilityStart, capabilityEnd)}
  ${injectionSource.slice(methodsStart, methodsEnd)}; return messageDeletionMethods;`)();

function stateFixture({ canonical = false } = {}) {
  const first = { turnId: "turn-1", status: "completed", items: [] };
  const second = { turnId: "turn-2", status: "completed", items: [] };
  const conversation = {
    id: "session-1",
    title: "保留标题",
    turns: [first, second],
    requests: [],
    resumeState: "resumed",
    threadRuntimeStatus: { type: "idle" },
    turnsPagination: { olderCursor: "cursor-1", hasLoadedOldest: false },
    latestModel: "model-1",
  };
  if (canonical) {
    conversation.turnHistory = {
      kind: "canonical",
      history: {
        generation: 4,
        isComplete: false,
        entitiesByKey: { "turn:turn-1": first, "turn:turn-2": second },
        islands: [{
          id: "tail:4",
          entries: [
            { key: "turn:turn-1", value: "turn:turn-1" },
            { key: "turn:turn-2", value: "turn:turn-2" },
          ],
          olderBoundary: { status: "available", handle: { cursor: "cursor-1" } },
          newerBoundary: { status: "exhausted" },
        }],
      },
    };
  }
  const requests = [];
  const manager = {
    ...methods,
    getHostId() { return "local"; },
    role: { role: "owner" },
    getConversation(sessionId) {
      assert.equal(sessionId, conversation.id);
      return conversation;
    },
    getStreamRole() { return this.role; },
    async sendRequest(method, params) { requests.push({ method, params }); },
    updateConversationState(sessionId, update) {
      assert.equal(sessionId, conversation.id);
      update(conversation);
    },
    inactiveThreadUnsubscriber: {
      clearConversationStreamOwnership(sessionId) {
        assert.equal(sessionId, conversation.id);
        manager.role = null;
      },
    },
    discardConversationFromCache() { assert.fail("must retain mounted conversation"); },
    resumeConversation() { assert.fail("must not reload the conversation"); },
  };
  return { conversation, manager, requests, first, second };
}

test("preparing deletion releases only the subscription and preserves the rendered history", async () => {
  const { manager, conversation, requests } = stateFixture({ canonical: true });
  const before = structuredClone(conversation);
  await manager.prepare(conversation.id);
  assert.deepEqual(conversation, before);
  assert.deepEqual(requests, [{ method: "thread/unsubscribe", params: { threadId: conversation.id } }]);
});

test("in-place deletion retains unselected turn objects and conversation metadata", () => {
  const { manager, conversation, second } = stateFixture();
  const pagination = conversation.turnsPagination;
  manager.finish(conversation.id, ["turn-1"]);
  assert.deepEqual(conversation.turns, [second]);
  assert.equal(conversation.turns[0], second);
  assert.equal(conversation.turnsPagination, pagination);
  assert.equal(conversation.title, "保留标题");
  assert.equal(conversation.latestModel, "model-1");
  assert.equal(conversation.resumeState, "needs_resume");
  assert.equal(manager.role, null);
});

test("canonical deletion invalidates pending pages while retaining pagination boundaries", () => {
  const { manager, conversation, second } = stateFixture({ canonical: true });
  const history = conversation.turnHistory.history;
  const island = history.islands[0];
  const olderBoundary = island.olderBoundary;
  const retainedEntry = island.entries[1];
  manager.finish(conversation.id, ["turn-1"]);
  assert.equal(history.generation, 5);
  assert.equal(history.islands[0], island);
  assert.equal(island.olderBoundary, olderBoundary);
  assert.equal(island.entries[0], retainedEntry);
  assert.deepEqual(Object.keys(history.entitiesByKey), ["turn:turn-2"]);
  assert.equal(history.entitiesByKey["turn:turn-2"], second);
  assert.deepEqual(conversation.turns, [second]);
});

test("canonical deletion matches unresolved tail selectors without deleting adjacent records", () => {
  const { manager, conversation } = stateFixture({ canonical: true });
  const history = conversation.turnHistory.history;
  const unresolvedTurn = { turnId: null, items: [] };
  history.entitiesByKey["tail:4:local:0"] = unresolvedTurn;
  conversation.turns.push(unresolvedTurn);
  history.islands[0].entries.push({ key: "tail:4:local:0", value: "tail:4:local:0" });
  manager.finish(conversation.id, ["history-content:tail:4:local:0"]);
  assert.deepEqual(Object.keys(history.entitiesByKey), ["turn:turn-1", "turn:turn-2"]);
  assert.equal(history.islands[0].entries.length, 2);
  assert.equal(conversation.turns.length, 2);
});

test("deleting all loaded turns retains history boundaries and permits later continuation", () => {
  const { manager, conversation } = stateFixture({ canonical: true });
  const island = conversation.turnHistory.history.islands[0];
  manager.finish(conversation.id, ["turn-1", "turn-2"]);
  assert.deepEqual(conversation.turns, []);
  assert.deepEqual(conversation.turnHistory.history.entitiesByKey, {});
  assert.deepEqual(island.entries, []);
  assert.equal(conversation.turnHistory.history.islands[0], island);
  assert.equal(conversation.resumeState, "needs_resume");
});

test("failed persistence releases ownership without removing or rebuilding history", () => {
  const { manager, conversation } = stateFixture({ canonical: true });
  const turns = conversation.turns;
  const before = structuredClone(conversation.turnHistory);
  manager.finish(conversation.id, []);
  assert.equal(conversation.turns, turns);
  assert.deepEqual(conversation.turnHistory, before);
  assert.equal(manager.role, null);
});

test("preparing deletion rejects active tasks followers and unsupported history before unsubscribing", async () => {
  for (const configure of [
    ({ conversation }) => { conversation.threadRuntimeStatus = { type: "active" }; },
    ({ conversation }) => { conversation.turns[0].status = "inProgress"; },
    ({ conversation }) => { conversation.requests.push({ id: "pending" }); },
    ({ manager }) => { manager.role = { role: "follower" }; },
    ({ conversation }) => { conversation.turnHistory = { kind: "unknown" }; },
  ]) {
    const runtime = stateFixture();
    configure(runtime);
    const before = structuredClone(runtime.conversation);
    await assert.rejects(runtime.manager.prepare(runtime.conversation.id));
    assert.deepEqual(runtime.requests, []);
    assert.deepEqual(runtime.conversation, before);
  }
});

test("a manager disposed after preparation cannot change the conversation", async () => {
  const { manager, conversation, requests } = stateFixture();
  await manager.prepare(conversation.id);
  assert.equal(requests.length, 1);
  manager.disposed = true;
  const before = structuredClone(conversation);
  await assert.rejects(manager.prepare(conversation.id), { code: "codey_capability_unavailable" });
  assert.throws(() => manager.finish(conversation.id, ["turn-1"]), { code: "codey_capability_unavailable" });
  assert.equal(requests.length, 1);
  assert.deepEqual(conversation, before);
  assert.deepEqual(manager.role, { role: "owner" });
});

const resolverStart = injectionSource.indexOf("  const appServerManagerRegistryResolverFromSource =");
const resolverEnd = injectionSource.indexOf("  window.__codeyAppServerManagerRegistryResolverFromSource", resolverStart);
assert.ok(resolverStart >= 0 && resolverEnd > resolverStart);
const resolveRegistry = Function(`${injectionSource.slice(resolverStart, resolverEnd)}; return appServerManagerRegistryResolverFromSource;`)();
const registryFixture = "class Registry {getForHostId(e){return this.scope.get(hosts).includes(e)?this.scope.get(managers,e):null}};export {hosts as h,managers as m};";

test("unexported registry resolves existing local state through renamed module exports", () => {
  const hosts = {}, managers = {}, manager = {};
  const lookup = resolveRegistry(registryFixture, { h: hosts, m: managers });
  const calls = [];
  const scope = { get(key, host) {
    calls.push([key, host]);
    if (key === hosts) return ["local"];
    assert.equal(key, managers);
    assert.equal(host, "local");
    return manager;
  } };
  assert.equal(lookup(scope, "local"), manager);
  assert.deepEqual(calls, [[hosts, undefined], [managers, "local"]]);
  calls.length = 0;
  assert.equal(lookup(scope, "remote"), null);
  assert.deepEqual(calls, [[hosts, undefined]], "unregistered hosts must not create a manager");
});

test("lookup accepts whitespace and shorthand exports without evaluating bundle code", () => {
  const hosts = {}, managers = {};
  const text = "throw Error('must not execute'); class R { getForHostId( id ) { return this.scope.get( hosts ).includes( id ) ? this.scope.get( managers, id ) : null; } } export { hosts, managers };";
  const lookup = resolveRegistry(text, { hosts, managers });
  assert.equal(typeof lookup, "function");
  assert.equal(lookup({ get() { return null; } }, "local"), null);
});

test("missing exports and ambiguous registry bindings fail without calling a scope", () => {
  assert.equal(resolveRegistry(registryFixture, { h: {} }), null);
  assert.equal(resolveRegistry(registryFixture.replace("export", "const"), { h: {}, m: {} }), null);
  assert.equal(resolveRegistry(registryFixture.replace(".includes(e)", ".has(e)"), { h: {}, m: {} }), null);
  const different = "class Other {getForHostId(e){return this.scope.get(otherHosts).includes(e)?this.scope.get(otherManagers,e):null}}";
  assert.equal(resolveRegistry(different + registryFixture, { h: {}, m: {} }), null);
  const same = {};
  assert.equal(resolveRegistry(registryFixture, { h: same, m: same }), null);
});
