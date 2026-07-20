import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/fast-mode-fix.js", import.meta.url), "utf8");

function loadShim() {
  const removed = [];
  const storage = new Map([["codey-fast-mode-enabled", "true"]]);
  let previousCleanupCalls = 0;
  const legacyButton = { remove: () => removed.push("button") };
  const legacyStyle = { remove: () => removed.push("style") };
  const document = {
    querySelectorAll: (selector) => (
      selector === '[data-codey-fast-mode-toggle="true"]' ? [legacyButton] : []
    ),
    getElementById: (id) => (id === "codey-fast-mode-style" ? legacyStyle : null),
  };
  const window = {
    __codeyFastModeFixInstalled: "5",
    __codeyFastModeFixCleanup: () => {
      previousCleanupCalls += 1;
    },
    __codeyGetFastMode: () => true,
    __codeySetFastMode: () => {},
    __codeyPatchFastModeRequest: () => {},
    __codeyFastModeTestHooks: {},
    __codeyFastModeLastRequest: {},
    __codeyFastModeDispatcherStatus: {},
    localStorage: {
      removeItem: (key) => storage.delete(key),
    },
  };
  window.window = window;
  vm.runInNewContext(source, { document, window });
  return { previousCleanupCalls, removed, storage, window };
}

test("native shim removes the legacy control and request interceptor", () => {
  const result = loadShim();

  assert.equal(result.previousCleanupCalls, 1);
  assert.deepEqual(result.removed, ["button", "style"]);
  assert.equal(result.storage.has("codey-fast-mode-enabled"), false);
  assert.equal(result.window.__codeyFastModeFixInstalled, "native-1");
  assert.equal(typeof result.window.__codeyFastModeFixCleanup, "function");
  assert.equal("__codeyGetFastMode" in result.window, false);
  assert.equal("__codeySetFastMode" in result.window, false);
  assert.equal("__codeyPatchFastModeRequest" in result.window, false);
  assert.equal("__codeyFastModeDispatcherStatus" in result.window, false);
});

test("native shim is idempotent", () => {
  const result = loadShim();
  const cleanup = result.window.__codeyFastModeFixCleanup;

  vm.runInNewContext(source, {
    document: {
      querySelectorAll: () => {
        throw new Error("idempotent load should not rescan");
      },
      getElementById: () => {
        throw new Error("idempotent load should not rescan");
      },
    },
    window: result.window,
  });

  assert.equal(result.window.__codeyFastModeFixCleanup, cleanup);
});
