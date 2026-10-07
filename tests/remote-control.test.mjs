import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { remoteRequest, RemoteError, requestId, pairingCode, rememberSubmission, readSubmission, forgetSubmission, clearSubmissions } = await loadTypeScriptModule(new URL("../src/remote/transport.ts", import.meta.url));

test("remote transport includes same-origin cookies and never retries mutations", async t => {
  const calls = [];
  t.mock.method(globalThis, "fetch", async (path, options) => {
    calls.push({ path, options });
    return new Response(JSON.stringify({ status: "failed", message: "授权已撤销" }), { status: 401 });
  });
  await assert.rejects(remoteRequest("/remote/action", { action: "send" }), error => error instanceof RemoteError && error.status === 401 && error.message === "授权已撤销");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.credentials, "same-origin");
  assert.equal(calls[0].options.method, "POST");
  assert.equal(calls[0].options.headers["Content-Type"], "application/json");
  assert.equal(calls[0].options.body, '{"action":"send"}');
});

test("remote transport handles success, protocol failures and dropped connections", async t => {
  const fetch = t.mock.method(globalThis, "fetch", async () => new Response(JSON.stringify({ id: "thread" })));
  assert.deepEqual(await remoteRequest("/remote/session"), { id: "thread" });
  assert.equal(fetch.mock.calls[0].arguments[1].method, "GET");
  fetch.mock.mockImplementation(async () => new Response('{"status":"failed","message":"expired"}'));
  await assert.rejects(remoteRequest("/remote/action", {}), /expired/);
  fetch.mock.mockImplementation(async () => { throw new TypeError("network disconnected"); });
  await assert.rejects(remoteRequest("/remote/action", {}), /network disconnected/);
  assert.equal(fetch.mock.callCount(), 3);
});

test("pairing accepts only the exact token format and random IDs work without randomUUID", () => {
  const code = "ab".repeat(32);
  assert.equal(pairingCode(`#pair=${code}`), code);
  for (const hash of ["", "#pair=short", `#pair=${code}z`, `#pair=${"g".repeat(64)}`]) assert.equal(pairingCode(hash), "");
  const ids = new Set(Array.from({ length: 100 }, requestId));
  assert.equal(ids.size, 100);
  for (const id of ids) assert.match(id, /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/);
});

test("an unconfirmed submission retains its ID across reloads and explicit verification clears it", () => {
  const storage = new Map();
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: {
    getItem: key => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, value),
    removeItem: key => storage.delete(key),
    key: index => Array.from(storage.keys())[index] ?? null,
    get length() { return storage.size; },
  } });
  try {
    const args = { action: "send", text: "test" };
    const first = rememberSubmission("thread", args);
    assert.equal(rememberSubmission("thread", args).id, first.id);
    assert.deepEqual(readSubmission("thread"), first);
    // Simulate a pending record restored by a fresh browser module.
    storage.set("codey-remote-pending:restored", JSON.stringify(first));
    assert.equal(rememberSubmission("restored", args).id, first.id);
    forgetSubmission("thread");
    assert.equal(readSubmission("thread"), null);
    assert.notEqual(rememberSubmission("thread", args).id, first.id);
    assert.notEqual(rememberSubmission("thread", { action: "send", text: "changed" }).id, first.id);
    storage.set("unrelated", "retained");
    clearSubmissions();
    assert.equal(readSubmission("thread"), null);
    assert.equal(readSubmission("restored"), null);
    assert.deepEqual(Array.from(storage.entries()), [["unrelated", "retained"]]);
  } finally {
    if (descriptor) Object.defineProperty(globalThis, "sessionStorage", descriptor);
    else delete globalThis.sessionStorage;
  }
});
