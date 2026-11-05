import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

import { createModuleGraph } from "./helpers/jsx-tree.mjs";

const { applyConfigCommandResult } = createModuleGraph(new URL("../src/configCommandResult.ts", import.meta.url)).exports;
const { reconcileConfigApply } = createModuleGraph(new URL("../src/configApply.ts", import.meta.url)).exports;
const baseConfig = () => ({ settingsRevision: 1, activeProfileId: "route", profiles: [{ id: "route", name: "Original" }], localRouterEnabled: true, miscModel: "old" });

function harness(initial = baseConfig()) {
  let cursor = 0;
  const cells = [];
  const react = {
    useState(initial) {
      const index = cursor++;
      cells[index] ??= { value: initial };
      return [cells[index].value, next => { cells[index].value = typeof next === "function" ? next(cells[index].value) : next; }];
    },
    useRef(initial) { return cells[cursor++] ??= { current: initial }; },
    useCallback: callback => callback,
    useMemo: callback => callback(),
  };
  const graph = createModuleGraph(new URL("../src/useDraftConfig.ts", import.meta.url), { stubs: { react } });
  const draft = () => { cursor = 0; return graph.exports.useDraftConfig(); };
  draft().setPersistedConfig(initial);
  const state = { status: { running: true }, model: { defaultModel: "current" }, provider: { id: "current" } };
  const calls = [];
  const apply = (result, submitted) => applyConfigCommandResult(result, {
    adoptConfig: config => submitted === undefined ? draft().adoptRouteConfig(config) : draft().adoptSavedConfig(config, submitted),
    acceptConfigApply: incoming => {
      calls.push("apply");
      if (!incoming) return incoming;
      state.status.configApply = reconcileConfigApply(state.status.configApply, incoming);
      return state.status.configApply;
    },
    setDirty: value => { calls.push("dirty"); draft().setDirty(value); },
    setModelState: value => { calls.push("model"); state.model = value; },
    setProviderStatus: value => { calls.push("provider"); state.provider = value; },
    setStatus: update => { calls.push("status"); state.status = update(state.status); },
  });
  return { draft, state, calls, apply };
}

for (const mode of ["route", "save"]) {
  test(`${mode} rejects stale config and all accompanying state`, () => {
    const initial = baseConfig();
    const h = harness({ ...initial, settingsRevision: 3 });
    h.draft().editConfig({ ...h.draft().config, miscModel: "edited" });
    const old = {
      config: { ...initial, settingsRevision: 2 },
      configApply: { revision: 2, state: "pending" },
      modelState: { defaultModel: "old" }, providerStatus: { id: "old" }, restartRequired: true,
    };
    assert.equal(h.apply(old, mode === "save" ? initial : undefined), false);
    assert.equal(h.draft().config.miscModel, "edited");
    assert.equal(h.draft().config.settingsRevision, 3);
    assert.equal(h.draft().dirty, true);
    assert.deepEqual(h.calls, []);
    assert.equal(h.state.model.defaultModel, "current");
    assert.equal(h.state.provider.id, "current");
    assert.deepEqual(h.state.status, { running: true });
  });
}

test("save response retains edits made after submitting the draft", () => {
  const h = harness();
  const submitted = { ...h.draft().config, miscModel: "submitted" };
  h.draft().editConfig(submitted);
  h.draft().editConfig({ ...submitted, miscModel: "edited while saving" });
  const incoming = { ...submitted, settingsRevision: 2, serverFlag: true };
  assert.equal(h.apply({ config: incoming }, submitted), true);
  assert.equal(h.draft().config.miscModel, "edited while saving");
  assert.equal(h.draft().config.serverFlag, true);
  assert.equal(h.draft().config.settingsRevision, 2);
  assert.equal(h.draft().persistedConfigRef.current, incoming);
  assert.equal(h.draft().dirty, true);
});

test("save acknowledgement clears dirty and later runtime states cannot restore it", () => {
  const h = harness();
  const submitted = { ...h.draft().config, miscModel: "submitted" };
  h.draft().editConfig(submitted);
  const incoming = { ...submitted, settingsRevision: 2 };
  assert.equal(h.apply({ config: incoming }, submitted), true);
  assert.equal(h.draft().dirty, false);

  for (const state of ["pending", "restart_required", "failed"]) {
    assert.equal(h.apply({
      config: incoming,
      configApply: { revision: 2, state },
      restartRequired: true,
    }, submitted), true);
    assert.equal(h.draft().dirty, false);
  }
});

test("editing a saved value back to its baseline clears dirty", () => {
  const h = harness();
  const initial = h.draft().config;
  h.draft().editConfig({ ...initial, miscModel: "changed" });
  assert.equal(h.draft().dirty, true);
  h.draft().editConfig({ ...initial, settingsRevision: 99 });
  assert.equal(h.draft().dirty, false);
});

test("route responses merge saved routes while retaining unrelated draft edits", () => {
  const h = harness();
  const initial = h.draft().config;
  h.draft().editConfig({ ...initial, miscModel: "draft" });
  const incoming = { ...initial, settingsRevision: 2, profiles: [{ id: "route", name: "Updated" }] };
  assert.equal(h.apply({ config: incoming, modelState: { defaultModel: "new" } }), true);
  assert.equal(h.draft().config.miscModel, "draft");
  assert.equal(h.draft().config.profiles[0].name, "Updated");
  assert.equal(h.draft().dirty, true);
  assert.equal(h.state.model.defaultModel, "new");
});

for (const incomingApply of [undefined, { revision: 2, state: "pending" }]) {
  test(`completed application survives a delayed response ${incomingApply ? "with pending status" : "with legacy restart flag"}`, () => {
    const h = harness();
    const complete = { revision: 2, state: "applied" };
    h.state.status = { running: true, configApply: complete, restartRequired: false };
    const result = { config: { ...baseConfig(), settingsRevision: 2 }, configApply: incomingApply, restartRequired: true };
    assert.equal(h.apply(result), true);
    assert.equal(h.state.status.configApply, complete);
    assert.equal(h.state.status.restartRequired, false);
    if (incomingApply) assert.equal(result.configApply, complete);
    assert.equal(h.draft().dirty, false);
  });
}

test("legacy command results still update restart state when no newer application exists", () => {
  const h = harness();
  assert.equal(h.apply({ config: { ...baseConfig(), settingsRevision: 2 }, restartRequired: true }), true);
  assert.equal(h.state.status.restartRequired, true);
});

test("App does not broadcast rejected save or route results", async () => {
  const source = ts.createSourceFile("App.tsx", readFileSync(new URL("../src/App.tsx", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const functions = new Map();
  const visit = node => {
    if (ts.isFunctionDeclaration(node) && node.name) functions.set(node.name.text, node.getText(source));
    ts.forEachChild(node, visit);
  };
  visit(source);
  const effects = [];
  const result = { config: baseConfig(), modelState: { defaultModel: "old" } };
  const dependencies = {
    config: baseConfig(),
    invoke: async () => result,
    applyCommandResult: () => false,
    adoptSavedConfig: () => { throw new Error("the shared boundary owns config adoption"); },
    adoptRouteConfig: () => { throw new Error("the shared boundary owns config adoption"); },
    setFastContextToolsStatus: value => effects.push(value),
    UNKNOWN_FAST_CONTEXT_TOOLS_STATUS: {},
    window: { dispatchEvent: event => effects.push(event) },
    refreshStatus: async () => effects.push("refresh"),
  };
  const load = name => {
    const compiled = ts.transpileModule(functions.get(name), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
    return new Function(...Object.keys(dependencies), `${compiled}; return ${name};`)(...Object.values(dependencies));
  };
  assert.equal(await load("persist")(baseConfig()), result);
  assert.equal(load("applyRouteResult")(result), false);
  assert.deepEqual(effects, []);
});
