import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { canRepairMainProcessInjection, isMainProcessInjectionConfirmed } = await loadTypeScriptModule(
  new URL("../src/runtimeStatusPresentation.ts", import.meta.url),
);
const failed = { running: true, clientPlatform: "windows", maintenance: { startupInjectionMode: "cli" } };

test("main process repair is available only for confirmed Windows fallback", () => {
  assert.equal(canRepairMainProcessInjection(failed), true);
  for (const mode of ["node_options", "inspector", "", undefined, "unknown"]) {
    assert.equal(canRepairMainProcessInjection({ ...failed, maintenance: { startupInjectionMode: mode } }), false, String(mode));
  }
  for (const patch of [{ running: false }, { clientPlatform: "macos" }, { clientPlatform: undefined }, { restartInProgress: true }, { maintenance: undefined }]) {
    assert.equal(canRepairMainProcessInjection({ ...failed, ...patch }), false);
  }
});

test("repair completion requires a running main-process injection marker", () => {
  for (const mode of ["node_options", "inspector"]) {
    assert.equal(isMainProcessInjectionConfirmed({ running: true, maintenance: { startupInjectionMode: mode } }), true);
    assert.equal(isMainProcessInjectionConfirmed({ running: false, maintenance: { startupInjectionMode: mode } }), false);
  }
  assert.equal(isMainProcessInjectionConfirmed(failed), false);
  assert.equal(isMainProcessInjectionConfirmed({ running: true }), false);
});
