import assert from "node:assert/strict";
import { loadTypeScriptModule } from "../../tests/helpers/load-typescript-module.mjs";

const { formatTimestamp } = await loadTypeScriptModule(new URL("../../src/formatters.ts", import.meta.url));
function previousFormatTimestamp(value) {
  if (!Number.isFinite(value)) return "—";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
  }).format(new Date(value));
}

const values = Array.from({ length: 100 }, (_, index) => Date.UTC(2026, 8, 8, 0, 0, index));
for (const value of values) assert.equal(formatTimestamp(value), previousFormatTimestamp(value));
const functions = [previousFormatTimestamp, formatTimestamp];
const samples = [[], []];
let checksum = 0;
for (let group = 0; group < 9; group++) {
  for (const index of group % 2 ? [1, 0] : [0, 1]) {
    const start = performance.now();
    for (let round = 0; round < 100; round++) {
      for (const value of values) checksum += functions[index](value).length;
    }
    samples[index].push((performance.now() - start) / 100);
  }
}
const median = samples.map((values) => [...values].sort((a, b) => a - b)[4]);
console.log(JSON.stringify({ unit: "ms per 100 rows", samples, median, speedup: median[0] / median[1], checksum }, null, 2));
