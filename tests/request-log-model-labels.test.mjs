import assert from "node:assert/strict";
import test from "node:test";
import ts from "typescript";
import { readSource } from "./helpers/read-source.mjs";

const source = await readSource("src/requestLogModelLabels.ts");
const ids = await readSource("src/modelIds.ts");
const compiled = ts.transpileModule(`${ids}\n${source.replace(/^import .*;\n/gm, "")}`.replaceAll("export ", ""), {
  compilerOptions: { target: ts.ScriptTarget.ES2022 },
}).outputText;
const { requestLogModelLabels: labelsFor, displayRequestLogModelName: display, requestLogRowModelNames: rowNames } = new Function(`${compiled}; return { requestLogModelLabels, displayRequestLogModelName, requestLogRowModelNames };`)();
const catalog = (changes = {}) => ({
  profiles: [], selectedModelsByProvider: {}, upstreamModelsByProvider: {}, declaredOfficialModelsByProvider: {},
  modelAliasHistory: {}, ...changes,
});

test("deleted routes retain trusted historical model labels, including legacy image records", () => {
  const labels = labelsFor(catalog({ modelAliasHistory: { "removed/gpt-image": "gpt-image" } }));
  assert.equal(display(" REMOVED/gpt-image ", labels), "gpt-image");
  assert.deepEqual(rowNames({ requestedModel: "removed/gpt-image", model: "removed/gpt-image" }, labels), {
    primaryModel: "gpt-image", sentDisplay: "gpt-image", returnedDisplay: "",
  });
  assert.equal(rowNames({ requestedModel: "removed/gpt-image" }, labels).primaryModel, "gpt-image");
});

test("complete current aliases outrank raw ids, and raw ids outrank historical aliases", () => {
  const labels = labelsFor(catalog({
    profiles: [{ id: "route" }],
    upstreamModelsByProvider: { route: ["org/model", "route/org/model"], other: ["old/org/model"] },
    modelAliasHistory: { "route/org/model": "wrong", "old/org/model": "wrong" },
  }));
  assert.equal(display("route/org/model", labels), "org/model");
  assert.equal(display("old/org/model", labels), "old/org/model");
  assert.equal(display("route/unknown", labels), "route/unknown");
  assert.equal(display("unknown/org/model", labels), "unknown/org/model");
});

test("resolved and returned slash-containing model ids are never decoded again", () => {
  const labels = labelsFor(catalog({
    profiles: [{ id: "route" }], upstreamModelsByProvider: { route: ["model"] },
    modelAliasHistory: { "old/model": "route/model", "route/model": "model" },
  }));
  assert.deepEqual(rowNames({ requestedModel: "old/model", model: "route/model", upstreamResponseModel: "route/model" }, labels), {
    primaryModel: "route/model", sentDisplay: "route/model", returnedDisplay: "route/model",
  });
  assert.equal(rowNames({ requestedModel: "", model: "route/model" }, labels).primaryModel, "route/model");
});

test("nested historical selectors resolve once and encoded current route ids are matched exactly", () => {
  const labels = labelsFor(catalog({
    profiles: [{ id: "local", sourceProviderId: "route /~" }],
    selectedModelsByProvider: { "route /~": ["org/model"] },
    modelAliasHistory: { "outer/inner/model": "inner/model", "inner/model": "model" },
  }));
  assert.equal(display("outer/inner/model", labels), "inner/model");
  assert.equal(display("ROUTE%20%2f%7e/org/model", labels), "org/model");
});

test("log controls retain raw filter values and diagnosis retains the requested selector", async () => {
  const viewer = await readSource("src/RequestLogDialog.tsx");
  assert.match(viewer, /label: displayRequestLogModelName\(value, modelLabels\), value/);
  assert.match(viewer, /optionalFilter\(model\) \? \{ model \}/);
  assert.match(viewer, /selectedItem\.requestedModel/);
});
