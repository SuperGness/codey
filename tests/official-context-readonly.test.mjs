import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("official routes expose no context editors and preserve context settings", () => {
  const ui = readFileSync(new URL("../src/ModelSection.tsx", import.meta.url), "utf8");
  const officialEditor = ui.split('<div className="official-route-editor">')[1].split('<div className="route-editor-form">')[0];
  assert.doesNotMatch(officialEditor, /ModelContextFields|label="1M"/);
  const backend = readFileSync(new URL("../backend/src/commands/models/defaults.rs", import.meta.url), "utf8");
  const saveOfficial = backend.split("pub async fn save_official_route_models(")[1].split("pub(crate) async fn")[0];
  assert.doesNotMatch(saveOfficial, /set_supports_1m_context_models\(|set_model_contexts\(/);
});
