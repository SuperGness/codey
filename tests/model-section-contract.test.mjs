import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";

const [modelSectionSource, typeSource] = await Promise.all([
  readSource("src/ModelSection.tsx"),
  readSource("src/App.types.ts"),
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

test("official routes display WS while third-party routes remain explicit opt-in", () => {
  assert.match(modelSectionSource, /\(isOfficial \|\| profile\.supportsWebsockets\) &&/);
  assert.match(modelSectionSource, /routeDraft\.upstreamProtocol === "openaiResponses" && \(/);
  assert.match(modelSectionSource, /aria-label="WebSocket"/);
  assert.match(
    modelSectionSource,
    /checked=\{Boolean\(routeDraft\.supportsWebsockets\)\}[\s\S]*?disabled=\{isBusy\}/,
  );
  assert.doesNotMatch(
    modelSectionSource,
    /仅在线路明确支持 Responses WebSocket 时开启；连接失败会自动回退 HTTP\/SSE。/,
  );
});

test("official routes expose no context editors and preserve context settings", async () => {
  const officialEditor = modelSectionSource
    .split('<div className="official-route-editor">')[1]
    .split('<div className="route-editor-form">')[0];
  assert.doesNotMatch(officialEditor, /ModelContextFields|label="1M"/);
  const backend = await readSource("backend/src/commands/models/defaults.rs");
  const saveOfficial = backend
    .split("pub async fn save_official_route_models(")[1]
    .split("pub(crate) async fn")[0];
  assert.doesNotMatch(saveOfficial, /set_supports_1m_context_models\(|set_model_contexts\(/);
});
