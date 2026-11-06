import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { formatUsageUtcMinute, formatUsageShortDate, formatCompactUsageNumber } = await loadTypeScriptModule(new URL("../src/usageFormatters.ts", import.meta.url));

test("用量时间始终使用 UTC，跨年时显示年份并保留午夜格式", () => {
  const end = Date.UTC(2025, 0, 1);
  assert.equal(formatUsageUtcMinute(end - 60_000), "2024/12/31 23:59");
  assert.equal(formatUsageUtcMinute(end), "2025/01/01 00:00");
  assert.equal(formatUsageShortDate(end, true), "2025/01/01 00:00");
  assert.equal(formatUsageShortDate(end, false), "01/01 00:00");
});

test("图表刻度保留未知、已知零和紧凑数字的显示规则", () => {
  assert.equal(formatCompactUsageNumber(null), "—");
  assert.equal(formatCompactUsageNumber(0), "0");
  assert.equal(formatCompactUsageNumber(1234), "1.2K");
  assert.equal(formatCompactUsageNumber(1234567), "1.2M");
});
