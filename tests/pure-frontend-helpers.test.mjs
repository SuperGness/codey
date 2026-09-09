import assert from "node:assert/strict";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);
const [urlValidation, formatters, appUtils, runtimeStatusSnapshot] =
  await Promise.all([
    loadTypeScriptModule(new URL("src/urlValidation.ts", root)),
    loadTypeScriptModule(new URL("src/formatters.ts", root)),
    loadTypeScriptModule(new URL("src/appUtils.ts", root)),
    loadTypeScriptModule(new URL("src/runtimeStatusSnapshot.ts", root)),
  ]);

test("outbound API URL validation rejects non-http schemes, credentials and blanks", () => {
  const { validateOutboundApiUrl } = urlValidation;
  assert.equal(validateOutboundApiUrl("https://api.example.com/v1"), "");
  assert.equal(validateOutboundApiUrl("http://127.0.0.1:8080/v1"), "");
  assert.match(validateOutboundApiUrl("   "), /请输入/);
  assert.match(validateOutboundApiUrl("ftp://example.com"), /HTTP\(S\)/);
  assert.match(validateOutboundApiUrl("https://user:pw@example.com"), /用户名或密码/);
  assert.match(validateOutboundApiUrl("not a url"), /格式无效/);
  assert.match(validateOutboundApiUrl("", "服务地址"), /服务地址/);
});

test("formatBytes picks a unit and one decimal below ten", () => {
  const { formatBytes } = formatters;
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(-5), "0 B");
  assert.equal(formatBytes(Number.NaN), "0 B");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(10 * 1024 * 1024), "10 MB");
  assert.equal(formatBytes(3 * 1024 ** 4), "3.0 TB");
  assert.equal(formatBytes(4096 * 1024 ** 4), "4096 TB");
});

test("formatTimestamp preserves local Chinese date formatting and invalid-value behavior", () => {
  const { formatTimestamp } = formatters;
  const reference = new Intl.DateTimeFormat("zh-CN", {
    year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
  });
  for (const value of [0, -1, Date.UTC(2024, 1, 29), Date.UTC(2026, 8, 8, 23, 59, 59)]) {
    assert.equal(formatTimestamp(value), reference.format(new Date(value)));
  }
  for (const value of [NaN, Infinity, -Infinity]) assert.equal(formatTimestamp(value), "—");
  assert.throws(() => formatTimestamp(8.64e15 + 1), RangeError);
});

test("errorText unwraps Error messages and stringifies everything else", () => {
  const { errorText } = appUtils;
  assert.equal(errorText(new Error("boom")), "boom");
  assert.equal(errorText("plain"), "plain");
  assert.equal(errorText(42), "42");
});

test("reconcileRuntimeStatus keeps referential identity for unchanged nested sections", () => {
  const { reconcileRuntimeStatus } = runtimeStatusSnapshot;
  const current = {
    status: "ok",
    maintenance: { scanned: 1, items: [1, 2] },
    injectionScripts: [{ id: "a", state: "ready" }],
  };
  assert.equal(reconcileRuntimeStatus(current, structuredClone(current)), current);
  const next = {
    ...structuredClone(current),
    status: "degraded",
    injectionScripts: [{ id: "a", state: "error" }],
  };
  const reconciled = reconcileRuntimeStatus(current, next);
  assert.notEqual(reconciled, current);
  assert.equal(reconciled.status, "degraded");
  assert.equal(reconciled.maintenance, current.maintenance, "equal nested object is reused");
  assert.notEqual(reconciled.injectionScripts, current.injectionScripts);
  assert.deepEqual(reconciled.injectionScripts, [{ id: "a", state: "error" }]);
});
