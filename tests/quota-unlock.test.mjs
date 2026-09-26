import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../public/quota-unlock.js", import.meta.url), "utf8");

function createRuntime(initialValue = null) {
  const values = new Map(initialValue === null ? [] : [["codeyQuotaUnlock", initialValue]]);
  const localStorage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: (key) => values.delete(key),
  };
  const window = { localStorage };
  window.window = window;
  const context = { window, localStorage, console };
  vm.runInNewContext(source, context);
  return { context, window, localStorage };
}

test("quota unlock sanitizes rate-limit payloads before Codex reads them", () => {
  const { context, window } = createRuntime();
  const value = vm.runInNewContext(
    `JSON.parse(${JSON.stringify(JSON.stringify({
      rate_limit: { allowed: false, limit_reached: true },
      primary_window: { used_percent: 100, reset_at: 1 },
      spend_control: { reached: true },
      credits: { has_credits: false, unlimited: false, balance: 0 },
      blocked_features: [{ name: "send" }, { name: "other" }],
    }))})`,
    context,
  );

  assert.equal(value.rate_limit.allowed, true);
  assert.equal(value.rate_limit.limit_reached, false);
  assert.equal(value.primary_window.used_percent, 3);
  assert.ok(value.primary_window.reset_at > Math.floor(Date.now() / 1000));
  assert.equal(value.spend_control.reached, false);
  assert.equal(value.credits.has_credits, true);
  assert.equal(value.credits.unlimited, true);
  assert.equal(value.credits.balance, 1_000_000);
  assert.equal(JSON.stringify(value.blocked_features), JSON.stringify([{ name: "other" }]));
  assert.equal(window.__codeyQuotaUnlock.status().sanitized, 1);
});

test("quota unlock can be disabled before the document is loaded", () => {
  const { context, window } = createRuntime("off");
  const value = vm.runInNewContext(
    `JSON.parse(${JSON.stringify(JSON.stringify({
      rate_limit: { allowed: false, limit_reached: true },
    }))})`,
    context,
  );

  assert.equal(value.rate_limit.allowed, false);
  assert.equal(value.rate_limit.limit_reached, true);
  assert.equal(window.__codeyQuotaUnlock.status().enabled, false);
});
