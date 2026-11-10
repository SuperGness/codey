import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
import { readSource } from "./helpers/read-source.mjs";

const { loadRequestLogModels } = await loadTypeScriptModule(new URL("../src/requestLogModels.ts", import.meta.url));
const { requestLogModelLabels: labelsFor, displayRequestLogModelName: display, requestLogRowModelNames: rowNames } = await loadTypeScriptModule(new URL("../src/requestLogModelLabels.ts", import.meta.url));
const catalog = (changes = {}) => ({
  profiles: [], selectedModelsByProvider: {}, upstreamModelsByProvider: {}, declaredOfficialModelsByProvider: {},
  modelAliasHistory: {}, ...changes,
});
const source = await readFile(new URL("../src/RequestLogDialog.tsx", import.meta.url), "utf8");
const tree = ts.createSourceFile("RequestLogDialog.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const effects = [];
const visit = (node) => {
  if (ts.isCallExpression(node) && node.expression.getText(tree) === "useEffect") {
    const text = node.getText(tree);
    if (text.includes("loadRequestLogModels") || text.includes('"query_route_request_log_stats"')) effects.push(text);
  }
  ts.forEachChild(node, visit);
};
visit(tree);
assert.equal(effects.length, 2);

test("model and statistics effects load all candidates with only one statistics request", async () => {
  const calls = [];
  let candidates;
  let statistics;
  const context = {
    opened: true, validRange: true, fromUnixMs: 1, toUnixMs: 2000,
    provider: "provider-a", officialAccount: "account-a", refreshRevision: 0,
    modelsCatalogNeeded: true, model: "all",
    filters: { fromUnixMs: 1, toUnixMs: 2000 }, groupBy: "model",
    modelsTask: { current: Promise.resolve() }, statsTask: { current: Promise.resolve() },
    useEffect: (callback) => callback(),
    optionalFilter: (value) => value !== "all",
    loadRequestLogModels,
    setUsedModels: (value) => { candidates = value; },
    setStats: (value) => { statistics = value; },
    setStatsLoading: () => {}, setStatsError: () => {},
    setError: (error) => assert.fail(error), errorText: String,
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === "query_route_request_log_stats") return { queryable: true, total: 123, groups: [] };
      assert.equal(command, "query_route_request_log_models");
      assert.equal(args.provider, "provider-a");
      assert.equal(args.officialAccountId, "account-a");
      return args.afterModel
        ? { queryable: true, models: ["model-z"], nextCursor: null }
        : { queryable: true, models: ["model-a"], nextCursor: "model-a" };
    },
  };
  const compiled = ts.transpileModule(effects.join(";\n"), {}).outputText;
  new Function(...Object.keys(context), compiled)(...Object.values(context));
  await Promise.all([context.modelsTask.current, context.statsTask.current]);
  assert.equal(calls.filter(({ command }) => command === "query_route_request_log_stats").length, 1);
  assert.equal(calls.filter(({ command }) => command === "query_route_request_log_models").length, 2);
  assert.deepEqual(candidates, ["model-a", "model-z"]);
  assert.equal(statistics.total, 123);
});

test("model catalog stays idle until the model filter is opened", async () => {
  const calls = [];
  const context = {
    opened: true, validRange: true, fromUnixMs: 1, toUnixMs: 2000,
    provider: "all", officialAccount: "all", refreshRevision: 0,
    modelsCatalogNeeded: false, model: "all",
    filters: { fromUnixMs: 1, toUnixMs: 2000 }, groupBy: "model",
    modelsTask: { current: Promise.resolve() }, statsTask: { current: Promise.resolve() },
    useEffect: (callback) => callback(),
    optionalFilter: (value) => value !== "all",
    loadRequestLogModels,
    setUsedModels: () => assert.fail("should not load models"),
    setStats: () => {},
    setStatsLoading: () => {}, setStatsError: () => {},
    setError: (error) => assert.fail(error), errorText: String,
    invoke: async (command) => {
      calls.push(command);
      if (command === "query_route_request_log_stats") return { queryable: true, total: 1, groups: [] };
      assert.fail(`unexpected command ${command}`);
    },
  };
  const compiled = ts.transpileModule(effects.join(";\n"), {}).outputText;
  new Function(...Object.keys(context), compiled)(...Object.values(context));
  await Promise.all([context.modelsTask.current, context.statsTask.current]);
  assert.deepEqual(calls, ["query_route_request_log_stats"]);
});

test("model candidates include low-frequency models beyond the statistics and page limits", async () => {
  const models = Array.from({ length: 251 }, (_, index) => `model-${String(index).padStart(3, "0")}`);
  const calls = [];
  const loaded = await loadRequestLogModels(async (cursor) => {
    calls.push(cursor);
    const start = cursor ? models.indexOf(cursor) + 1 : 0;
    const page = models.slice(start, start + 200);
    return { queryable: true, models: page, nextCursor: start + 200 < models.length ? page.at(-1) : null };
  }, () => true);
  assert.deepEqual(loaded, models);
  assert.deepEqual(calls, [undefined, "model-199"]);
});

test("closing or superseding a model query discards results and stops pagination", async () => {
  let active = true;
  let resolvePage;
  let calls = 0;
  const loading = loadRequestLogModels(() => {
    calls += 1;
    return new Promise((resolve) => { resolvePage = resolve; });
  }, () => active);
  active = false;
  resolvePage({ queryable: true, models: ["stale"], nextCursor: "stale" });
  assert.equal(await loading, null);
  assert.equal(calls, 1);
});

test("model query refuses non-advancing pagination instead of looping", async () => {
  await assert.rejects(loadRequestLogModels(async () => ({ queryable: true, models: ["same"], nextCursor: "same" }), () => true), /游标未前进/);
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
