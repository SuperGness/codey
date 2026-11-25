import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { FakeElementCore } from "./helpers/fake-element.mjs";

const injectionSource = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");

function fakeRpcStub(handlers) {
  const proto = Object.create(Function.prototype, {
    dup: { value() { return this; } },
    onRpcBroken: { value() {} },
  });
  const target = Object.setPrototypeOf(function stub() {}, proto);
  return new Proxy(target, {
    get: (stubTarget, key) => (Object.hasOwn(handlers, key) ? handlers[key] : Reflect.get(stubTarget, key)),
  });
}

function hostedDeletionFixture(scope) {
  const body = new FakeElementCore();
  body.__reactFiber$hostedManager = { memoizedState: { scope }, return: null };
  const document = {
    body,
    documentElement: new FakeElementCore(),
    scripts: [],
    getElementById: () => new FakeElementCore(),
    querySelector: () => null,
    querySelectorAll: () => [],
  };
  const window = {
    setTimeout: () => 1,
    clearTimeout() {},
    get fetch() {
      throw new Error("hosted discovery must not fetch renderer assets");
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
  return { window };
}

for (const inherited of [false, true]) {
  test(`hosted message deletion discovers a mounted ${inherited ? "parent" : "local"} signal without getStreamRole`, async () => {
    const events = [];
    const rpc = fakeRpcStub({
      getThreadExecutionState(id) {
        events.push(["getThreadExecutionState", id]);
        return { inProgress: false, activeTurnId: null };
      },
      getStreamRole() {
        assert.fail("Codex 26.1120 removed getStreamRole");
      },
      unsubscribeInactiveConversation(id) {
        events.push(["unsubscribeInactiveConversation", id]);
      },
      sendRequest(method, params) {
        events.push(["sendRequest", method, params.threadId]);
        return { status: "unsubscribed" };
      },
      getConversationCwd(id) {
        events.push(["getConversationCwd", id]);
        return "/repo";
      },
      discardConversationFromCache(id) {
        events.push(["discardConversationFromCache", id]);
      },
      resumeConversation(params) {
        events.push(["resumeConversation", params.conversationId, ...params.workspaceRoots]);
        return { status: "ready" };
      },
      refreshRecentConversations() {
        events.push(["refreshRecentConversations"]);
      },
    });
    const family = { kind: "signal-family" };
    const node = {
      familyBindings: new Map([
        [{ kind: "signal-family" }, new Map()],
        [{ kind: "signal-family" }, new Map([["remote", {}]])],
        [family, new Map([["local", {}]])],
      ]),
    };
    const reads = [];
    const scope = {
      query: null,
      node: inherited ? { familyBindings: new Map() } : node,
      chain: new Map(inherited ? [["parent", node]] : []),
      get(candidate, hostId) {
        reads.push([candidate, hostId]);
        assert.equal(candidate, family, "discovery must not initialize an unused signal");
        assert.equal(hostId, "local", "discovery must not initialize a remote host");
        return { status: "ready", client: { hostId: "local", rpc } };
      },
      set() { assert.fail("discovery must not mutate signals"); },
      watch() { assert.fail("discovery must not register watchers"); },
      when() {},
    };
    const { window } = hostedDeletionFixture(scope);

    const controller = await window.__codeyLoadCodexSessionController({ feature: "deleteMessages" });
    assert.equal(controller.reloadsConversationAfterDeletion, true);
    assert.equal(window.__codeyPageCapabilities.deleteMessages.status, "available");
    assert.deepEqual(reads, [[family, "local"]]);
    assert.deepEqual(events, []);

    await controller.prepareMessageDeletion("session-1");
    await controller.finishMessageDeletion("session-1", ["turn-1"]);
    assert.deepEqual(events, [
      ["getThreadExecutionState", "session-1"],
      ["unsubscribeInactiveConversation", "session-1"],
      ["sendRequest", "thread/unsubscribe", "session-1"],
      ["getConversationCwd", "session-1"],
      ["discardConversationFromCache", "session-1"],
      ["resumeConversation", "session-1", "/repo"],
      ["refreshRecentConversations"],
    ]);

    events.length = 0;
    await controller.finishMessageDeletion("session-1", []);
    assert.deepEqual(events, [
      ["getConversationCwd", "session-1"],
      ["resumeConversation", "session-1", "/repo"],
    ]);
  });
}

test("hosted deletion discovery rejects ambiguous or remote connections", async () => {
  const familyA = { kind: "signal-family" };
  const familyB = { kind: "signal-family" };
  const rpc = fakeRpcStub({ getThreadExecutionState: () => ({ inProgress: false, activeTurnId: null }) });
  for (const entries of [
    [[familyA, { status: "ready", client: { hostId: "remote", rpc } }]],
    [
      [familyA, { status: "ready", client: { hostId: "local", rpc } }],
      [familyB, { status: "ready", client: { hostId: "local", rpc } }],
    ],
  ]) {
    const map = new Map(entries);
    const scope = {
      query: null,
      node: { familyBindings: new Map(entries.map(([f]) => [f, new Map([["local", {}]])])) },
      chain: new Map(),
      get(family) { return map.get(family); },
      set() {},
      watch() {},
      when() {},
    };
    const { window } = hostedDeletionFixture(scope);
    await assert.rejects(
      window.__codeyLoadCodexSessionController({ feature: "deleteMessages" }),
      { code: "codey_capability_unavailable" },
    );
  }
});
