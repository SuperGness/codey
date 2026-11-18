import assert from "node:assert/strict";
import test from "node:test";
import { autoStubModule, collectElements, createModuleGraph, elementProps, elementType, textContent } from "./helpers/jsx-tree.mjs";

const ui = autoStubModule("ui");
const settings = autoStubModule("model settings");
const picker = createModuleGraph(new URL("../src/AppDialogs.tsx", import.meta.url), {
  stubs: {
    "@tabler/icons-react": autoStubModule("icons"),
    "./components/ui": ui,
    "./components/ModelSettingsFields": settings,
  },
});
const modelFields = createModuleGraph(new URL("../src/components/ModelSettingsFields.tsx", import.meta.url), {
  stubs: {
    "@heroui/react": { cn: (...classes) => classes.filter(Boolean).join(" ") },
    "@tabler/icons-react": autoStubModule("icons"),
    "./ui": ui,
    "./ModelContextWindowCombobox": autoStubModule("context window"),
  },
});
const astra = "vendor/GPT-6-Astra";
const sol = "gpt-6.1-sol";
const other = "gpt-6-solar";
const thirdPartyModels = [astra, sol, other];
const modelState = {
  officialModels: [], officialModelIds: [], thirdPartyModels, upstreamModels: thirdPartyModels,
  manualThirdPartyModels: [], syncedUltrafastModels: [astra.toLowerCase()], defaultModel: astra,
};

function renderPicker(overrides = {}) {
  picker.reset();
  const tree = picker.exports.ModelPickerDialog({
    open: true, routeConfigReadOnly: false, officialOnly: false, customModelInput: "",
    modelState, thirdPartyModelOptions: thirdPartyModels,
    draftModelSet: new Set(), draftModelContexts: {}, draftReasoningEfforts: {},
    reasoningEffortAutoByModel: {}, manualThirdPartyModelKeys: new Set(),
    draftModelUltrafast: {}, ...overrides,
  });
  return Object.fromEntries(collectElements(tree, node => elementType(node) === settings.ModelSettingsFields)
    .map(node => [elementProps(node).model, elementProps(node)]));
}

function renderFields(props) {
  modelFields.reset();
  const tree = modelFields.exports.ModelSettingsFields(props);
  const control = collectElements(tree, node => elementType(node) === ui.Switch)[0];
  const reset = collectElements(tree, node => elementType(node) === "button" && textContent(node) === "恢复跟随目录")[0];
  const summary = collectElements(tree, node => elementType(node) === "summary")[0];
  return { control: elementProps(control), reset: elementProps(reset), summary: textContent(summary) };
}

test("third-party Astra and Sol switches show each model's synced or overridden value", () => {
  const changes = [];
  const fields = renderPicker({
    draftModelUltrafast: { [astra.toLowerCase()]: false, [sol]: true },
    onUpdateDraftModelUltrafast: (...args) => changes.push(args),
  });
  const astraSettings = renderFields(fields[astra]);
  assert.match(astraSettings.summary, /隐藏 Ultrafast/);
  assert.match(renderFields(fields[sol]).summary, /显示 Ultrafast/);
  assert.match(renderFields(renderPicker()[astra]).summary, /默认/);
  assert.equal(astraSettings.control.checked, false);
  assert.equal(astraSettings.control.disabled, false);
  astraSettings.reset.onClick();
  assert.equal(renderFields(fields[sol]).control.checked, true);
  renderFields(fields[sol]).control.onCheckedChange(false);
  assert.deepEqual(changes, [[astra, undefined], [sol, false]]);
  assert.equal(fields[other].ultrafast, undefined);
  assert.equal(renderFields(renderPicker()[astra]).control.checked, true);
  assert.equal(renderFields(renderPicker()[sol]).control.checked, false);
});

test("official model switches are read-only and follow the synced account catalog", () => {
  const fields = renderPicker({
    officialOnly: true,
    thirdPartyModelOptions: [],
    draftModelUltrafast: { [sol]: true },
    modelState: { ...modelState, officialModelIds: [astra, sol], thirdPartyModels: [] },
  });
  const supported = renderFields(fields[astra]);
  assert.equal(supported.control.checked, true);
  assert.equal(supported.control.disabled, true);
  assert.equal(supported.reset, undefined);
  assert.equal(renderFields(fields[sol]).control.checked, false);
});

test("saving the synchronized model picker sends explicit per-model values for its own route", async () => {
  const calls = [];
  const selection = createModuleGraph(new URL("../src/useModelSelection.ts", import.meta.url), {
    stubs: { "./api": { invoke: async (command, args) => {
      calls.push({ command, args });
      return {};
    } } },
  });
  const config = {
    localRouterEnabled: true, activeProfileId: "primary", declaredOfficialModelsByProvider: {},
    selectedModelsByProvider: {},
    profiles: [
      { id: "primary", name: "Primary", authMode: "apiKey" },
      { id: "secondary", name: "Secondary", authMode: "apiKey" },
    ],
    modelUltrafastByProvider: { primary: { [sol]: false }, secondary: { [sol]: true } },
  };
  const render = () => {
    selection.restart();
    return selection.exports.useModelSelection({
      config, currentProvider: null, runOperation: async (_name, operation) => operation(),
      applyConfigResult: () => true, setNotice() {},
    });
  };
  render().openModelPicker(modelState, "", "secondary");
  let draft = render();
  assert.deepEqual(draft.draftModelUltrafast, { [sol]: true });
  draft.updateDraftModelUltrafast(astra, false);
  draft.updateDraftModelUltrafast(other, true);
  const manualModel = "custom/GPT-6-Astra";
  draft.updateCustomModelInput(manualModel);
  render().addCustomModel();
  render().updateDraftModelUltrafast(manualModel, true);
  render().toggleDraftModel([manualModel.toLowerCase(), sol], false);
  render().updateCustomModelInput(manualModel);
  render().addCustomModel();
  assert.equal(render().draftModelUltrafast[manualModel.toLowerCase()], undefined);
  assert.equal(render().draftModelUltrafast[sol], true);
  await render().saveModelSelection();
  assert.equal(calls[0].command, "save_selected_models");
  assert.equal(calls[0].args.routeId, "secondary");
  assert.deepEqual(calls[0].args.modelUltrafast, { [astra]: false, [sol]: true });
});
