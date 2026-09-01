import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [modelSectionSource, typeSource] = await Promise.all([
  readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/App.types.ts", import.meta.url), "utf8"),
]);

test("third-party native Web Search is an explicit Responses-only capability", () => {
  assert.match(typeSource, /supportsNativeWebSearch\?: boolean/);
  assert.match(modelSectionSource, /supportsNativeWebSearch: false/);
  assert.match(
    modelSectionSource,
    /upstreamProtocol === "openaiResponses"[\s\S]*?supportsNativeWebSearch:[\s\S]*?: false/,
  );
  assert.match(
    modelSectionSource,
    /checked=\{Boolean\(routeDraft\.supportsNativeWebSearch\)\}/,
  );
  assert.match(modelSectionSource, /aria-label="原生网页搜索"/);
});
