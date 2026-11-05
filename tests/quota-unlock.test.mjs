import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../public/quota-unlock.js", import.meta.url), "utf8");

function createRuntime(initialValue = null, beforeInstall = null) {
  const values = new Map(initialValue === null ? [] : [["codeyQuotaUnlock", initialValue]]);
  const localStorage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: (key) => values.delete(key),
  };
  class WebSocket {
    send(data) { this.sent = data; }
    addEventListener() {}
  }
  const window = { localStorage, WebSocket };
  window.window = window;
  const context = vm.createContext({ window, localStorage, console });
  const nativeParse = vm.runInContext("JSON.parse", context);
  beforeInstall?.(window);
  vm.runInContext(source, context);
  return { context, window, localStorage, nativeParse, WebSocket, api: window.__codeyQuotaUnlock };
}

const usage = {
  rate_limit: { allowed: false, limit_reached: true,
    primary_window: { used_percent: 100, reset_at: 1 },
    secondary_window: { used_percent: 55, reset_at: 0 } },
  rateLimits: { primary: { usedPercent: 4, resetAt: 1 }, secondary: { usedPercent: 55, resetAt: null } },
  rateLimitsByLimitId: { codex: { primary: { usedPercent: 100, resetAt: 1 } } },
  rate_limit_reached_type: "weekly", rateLimitReachedType: "weekly",
  spend_control: { reached: true },
  credits: { has_credits: false, unlimited: false, balance: 0 },
  rate_limit_upsell: { plan: "pro" }, rate_limit_warning: "exhausted",
  sidebar_usage_warnings: ["exhausted"],
  blocked_features: [{ name: "send" }, { name: "tpp_send" }, { name: "other" }],
  limits_progress: [{ remaining: 0 }],
  additional_rate_limits: [{ primary_window: { used_percent: 99, reset_at: 1 } }],
  code_review_rate_limit: { allowed: false, limit_reached: true },
};

test("official REST, WebSocket and SSE usage payloads remain unchanged", () => {
  const { context, window, nativeParse, WebSocket } = createRuntime();
  assert.equal(vm.runInContext("JSON.parse", context), nativeParse);
  assert.equal(window.WebSocket, WebSocket);
  const socket = new window.WebSocket("ws://localhost");
  assert.equal(socket.send, WebSocket.prototype.send);
  assert.equal(socket.addEventListener, WebSocket.prototype.addEventListener);
  for (const payload of [usage, { id: 1, result: usage },
    { method: "account/rateLimits/updated", params: usage }, { type: "usage.snapshot", usage }]) {
    const serialized = JSON.stringify(payload);
    const parsed = vm.runInContext(`JSON.parse(${JSON.stringify(serialized)})`, context);
    assert.equal(JSON.stringify(parsed), serialized);
    socket.send(serialized);
    assert.equal(socket.sent, serialized);
  }
});

test("ordinary business data and JSON.parse revivers remain unchanged", () => {
  const { context } = createRuntime();
  const serialized = JSON.stringify({ name: "task", count: 2, nested: { credits: { balance: -20 } } });
  assert.equal(vm.runInContext(`JSON.stringify(JSON.parse(${JSON.stringify(serialized)}))`, context), serialized);
  assert.equal(vm.runInContext('JSON.parse(\'{"count":2}\', (key, value) => key === "count" ? value + 1 : value).count', context), 3);
});

test("only confirmed third-party models on the local host allow sending", () => {
  const { window, api } = createRuntime();
  const calls = [];
  window.__codeyModelWhitelistPatch = {
    isThirdPartyModel(context) { calls.push(context); return context.model === "third-party"; },
  };
  const context = { hostId: "local", model: "third-party" };
  assert.equal(api.allowsLocalSend(context), true);
  assert.equal(calls[0], context);
  assert.equal(api.allowsLocalSend({ hostId: "local", model: "official" }), false);
  const before = calls.length;
  for (const invalid of [undefined, null, {}, { model: "third-party" },
    { hostId: "remote", model: "third-party" }, { hostId: "local" },
    { hostId: "local", model: 42 }, { hostId: "local", model: "" }, { hostId: "local", model: "  " }]) {
    assert.equal(api.allowsLocalSend(invalid), false);
  }
  assert.equal(calls.length, before);
});

test("switched official routes send only when their provider is bypassed", () => {
  const { window, api } = createRuntime();
  const calls = [];
  window.__codeyModelWhitelistPatch = {
    isThirdPartyModel: () => false,
    officialRouteProviderId(context) {
      calls.push(context);
      return context.model === "official-b" ? "codey-official-account-b" : "";
    },
  };

  const context = { hostId: "local", model: "official-b" };
  assert.equal(api.allowsLocalSend(context), false);
  assert.equal(api.setOfficialSendBypass(["codey-official-account-b"]), 1);
  assert.equal(api.allowsLocalSend(context), true);
  assert.equal(api.allowsLocalSend({ hostId: "local", model: "official-a" }), false);
  assert.equal(api.setOfficialSendBypass([]), 0);
  assert.equal(api.allowsLocalSend(context), false);

  // 第三方判断优先，不依赖 bypass 名单。
  window.__codeyModelWhitelistPatch = {
    isThirdPartyModel: () => true,
    officialRouteProviderId: () => "",
  };
  assert.equal(api.allowsLocalSend({ hostId: "local", model: "third-party" }), true);
});

test("install picks up the bypass list the model patch already received", () => {
  const { window, api } = createRuntime(null, (w) => {
    w.__codeyModelWhitelistPatch = {
      isThirdPartyModel: () => false,
      officialRouteProviderId: () => "codey-official-account-b",
      officialSendBypassProviderIds: () => [" codey-official-account-b ", ""],
    };
  });
  assert.equal(api.status().officialSendBypass, 1);
  assert.equal(api.allowsLocalSend({ hostId: "local", model: "official-b" }), true);
  api.setOfficialSendBypass([]);
  assert.equal(api.allowsLocalSend({ hostId: "local", model: "official-b" }), false);
});

test("bypass entries normalize input and never apply to other contexts", () => {
  const { window, api } = createRuntime();
  window.__codeyModelWhitelistPatch = {
    isThirdPartyModel: () => false,
    officialRouteProviderId: () => "codey-official-account-b",
  };
  assert.equal(
    api.setOfficialSendBypass([" codey-official-account-b ", "", null, 42]),
    1,
  );
  for (const invalid of [undefined, null, {}, { model: "official-b" },
    { hostId: "remote", model: "official-b" }, { hostId: "local" },
    { hostId: "local", model: "" }, { hostId: "local", model: "  " }]) {
    assert.equal(api.allowsLocalSend(invalid), false);
  }
  assert.equal(api.status().officialSendBypass, 1);
});

test("missing, failing and unconfirmed helpers preserve restrictions", () => {
  const { window, api } = createRuntime();
  const context = { hostId: "local", model: "third-party" };
  assert.equal(api.allowsLocalSend(context), false);
  for (const helper of [{}, { isThirdPartyModel: true },
    { isThirdPartyModel() { throw new Error("unavailable"); } },
    ...[false, undefined, null, 1, "true"].map(value => ({ isThirdPartyModel: () => value }))]) {
    window.__codeyModelWhitelistPatch = helper;
    assert.equal(api.allowsLocalSend(context), false);
  }
});

test("enable and disable take immediate effect and persist the local option", () => {
  const { window, api, localStorage } = createRuntime("off");
  window.__codeyModelWhitelistPatch = { isThirdPartyModel: () => true };
  const context = { hostId: "local", model: "third-party" };
  assert.equal(api.status().version, "2.1.0");
  assert.ok(Number.isFinite(Date.parse(api.status().installedAt)));
  assert.equal(api.status().enabled, false);
  assert.equal(api.allowsLocalSend(context), false);
  api.enable();
  assert.equal(localStorage.getItem("codeyQuotaUnlock"), null);
  assert.equal(api.status().enabled, true);
  assert.equal(api.allowsLocalSend(context), true);
  api.disable();
  assert.equal(localStorage.getItem("codeyQuotaUnlock"), "off");
  assert.equal(api.status().enabled, false);
  assert.equal(api.allowsLocalSend(context), false);
});

test("storage failures do not prevent immediate enable or disable", () => {
  const { api, localStorage } = createRuntime();
  localStorage.setItem = localStorage.removeItem = () => { throw new Error("denied"); };
  api.disable();
  assert.equal(api.status().enabled, false);
  api.enable();
  assert.equal(api.status().enabled, true);
});
