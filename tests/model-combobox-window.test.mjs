import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { modelComboboxLayout, visibleModelGroups, MODEL_OPTION_HEIGHT, MODEL_GROUP_HEIGHT, MODEL_LIST_HEIGHT } = await loadTypeScriptModule(new URL("../src/modelComboboxWindow.ts", import.meta.url));
const option = (routeId, modelId) => ({ routeId, modelId, value: `${routeId}/${modelId}`, routeName: routeId, providerId: routeId });

test("virtual model layout preserves route grouping, order and keyboard offsets", () => {
  const options = [option("a", "first"), option("b", "second"), option("a", "third")];
  const layout = modelComboboxLayout(options);
  assert.deepEqual(layout.options, [options[0], options[2], options[1]]);
  assert.deepEqual(layout.groups.map(({ startIndex }) => startIndex), [0, 2]);
  assert.deepEqual(layout.offsets, [MODEL_GROUP_HEIGHT, MODEL_GROUP_HEIGHT + MODEL_OPTION_HEIGHT, 2 * MODEL_GROUP_HEIGHT + 2 * MODEL_OPTION_HEIGHT]);
  assert.equal(layout.height, 2 * MODEL_GROUP_HEIGHT + 3 * MODEL_OPTION_HEIGHT);
  assert.deepEqual(visibleModelGroups([], 0), []);
});

test("10000 models render a bounded window while every option remains reachable", () => {
  const layout = modelComboboxLayout(Array.from({ length: 10000 }, (_, index) => option(`route-${Math.floor(index / 1000)}`, `model-${index}`)));
  for (const index of [0, 7, 999, 1000, 5678, 9999]) {
    const scrollTop = Math.max(0, Math.min(layout.height - MODEL_LIST_HEIGHT, layout.offsets[index]));
    const window = visibleModelGroups(layout.groups, scrollTop);
    const options = window.flatMap(({ group, start, end }) => group.options.slice(start, end));
    assert.ok(options.includes(layout.options[index]), `model ${index} must be mounted after keyboard navigation`);
    assert.ok(options.length <= 16, `mounted ${options.length} options`);
  }
  const filtered = modelComboboxLayout([layout.options[9999]]);
  assert.equal(visibleModelGroups(filtered.groups, 0)[0].end, 1);
});
