import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

import {
  autoStubModule,
  collectElements,
  createModuleGraph,
  elementProps,
  textContent,
} from "./helpers/jsx-tree.mjs";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
import { readSource } from "./helpers/read-source.mjs";

const root = new URL("../", import.meta.url);

test("model order helpers keep the saved order and move one model onto another", async () => {
  const { orderModelIdsBy, moveModelId } = await loadTypeScriptModule(
    new URL("src/modelIds.ts", root),
  );
  assert.deepEqual(orderModelIdsBy(["a", "b", "c", "d"], ["C", "a"]), ["c", "a", "b", "d"]);
  assert.deepEqual(orderModelIdsBy(["a", "b"], []), ["a", "b"]);
  assert.deepEqual(moveModelId(["a", "b", "c"], "a", "c"), ["b", "c", "a"]);
  assert.deepEqual(moveModelId(["a", "b", "c"], "C", "a"), ["c", "a", "b"]);
  assert.equal(moveModelId(["a", "b"], "a", "a"), null);
  assert.equal(moveModelId(["a", "b"], "x", "a"), null);
});

test("official picker draft starts in the order saved on the route", async () => {
  const ids = await loadTypeScriptModule(new URL("src/modelIds.ts", root));
  const reasoningEfforts = await loadTypeScriptModule(
    new URL("src/modelReasoningEfforts.ts", root),
  );
  const state = [];
  let cursor = 0;
  const react = {
    useCallback: (callback) => callback,
    useMemo: (factory) => factory(),
    useRef: (initial) => ({ current: initial }),
    useState(initial) {
      const index = cursor++;
      if (!(index in state)) state[index] = initial;
      return [
        state[index],
        (value) => {
          state[index] = typeof value === "function" ? value(state[index]) : value;
        },
      ];
    },
  };
  const source = await readFile(new URL("src/useModelSelection.ts", root), "utf8");
  const exports = {};
  new Function(
    "require",
    "exports",
    ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
    }).outputText,
  )((name) => {
    if (name === "react") return react;
    if (name === "./modelIds") return ids;
    if (name === "./modelReasoningEfforts") return reasoningEfforts;
    if (name === "./modelRoutes") {
      return { routeProviderId: (profile) => profile.sourceProviderId || profile.id };
    }
    if (name === "./subagentModels") return { buildSubagentModelOptions: () => [] };
    return {};
  }, exports);
  const config = {
    activeProfileId: "official",
    profiles: [{ id: "official", authMode: "officialAccount", sourceProviderId: "openai" }],
    selectedModelsByProvider: { openai: ["gpt-5.6-luna", "gpt-6-astra"] },
  };
  const render = () => {
    cursor = 0;
    return exports.useModelSelection({ config, currentProvider: null });
  };
  const officialModels = ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-luna"].map((slug) => ({
    slug,
    supported: true,
  }));
  render().openModelPicker(
    {
      officialModels,
      officialModelIds: officialModels.map((model) => model.slug),
      upstreamModels: [],
      thirdPartyModels: [],
      manualThirdPartyModels: [],
    },
    "",
    "official",
  );
  assert.deepEqual([...render().draftModelSet], ["gpt-5.6-luna", "gpt-6-astra", "gpt-5.6-sol"]);
});

// 线路卡片按生产路径渲染：每个模型芯片带独立排序手柄，拖放与方向键都只在同一线路内换位。
const ui = autoStubModule("ui");
const icons = autoStubModule("icon");
const heroui = autoStubModule("heroui");
const modelSection = createModuleGraph(new URL("../src/ModelSection.tsx", import.meta.url), {
  stubs: {
    "@heroui/react": heroui,
    "@tabler/icons-react": icons,
    "./api": { invoke: async () => ({}) },
    "./components/ModelCombobox": autoStubModule("combobox"),
    "./components/ui": ui,
    "./OfficialAccountsPanel": autoStubModule("official-accounts"),
    "./overlayTheme": { readHostTheme: () => "light" },
    "./SettingsPageHeader": autoStubModule("settings-header"),
  },
});

function modelSectionProps(overrides = {}) {
  const reorderCalls = [];
  const props = {
    config: {
      activeProfileId: "route-a",
      autoCheckCodeyUpdates: true,
      codexAppPath: "",
      declaredOfficialModelsByProvider: {},
      defaultModel: "route-a/k3",
      disableTraceLogWrites: false,
      fastContextTools: false,
      gpuLaunchMode: "off",
      hideFullAccessWarning: false,
      initialRouteImportCompleted: true,
      localRouterEnabled: true,
      manualThirdPartyModelsByProvider: {},
      miscModel: "",
      modelContextByProvider: {},
      modelReasoningEffortsByProvider: {},
      profiles: [
        {
          apiKey: "",
          apiKeyConfigured: true,
          authMode: "apiKey",
          baseUrl: "https://a.example.com/v1",
          clearApiKey: false,
          enabled: true,
          id: "route-a",
          modelRequestHeaders: {},
          name: "线路 A",
          officialAccount: false,
          shortName: "",
          supportsNativeWebSearch: false,
          supportsRemoteCompaction: false,
          supportsWebsockets: false,
          upstreamProtocol: "openaiResponses",
          upstreamProxy: "",
        },
      ],
      promptOptimization: {},
      protectCrashpadPending: false,
      routeRequestLog: { enabled: false },
      selectedModelsByProvider: { "route-a": ["k3", "kimi-for-coding", "k3-256k"] },
      settingsRevision: 1,
      showAccountUsageInHeader: false,
      slimCodexPet: false,
      streamMaxRetries: 3,
      subagentModel: "",
      subagentOptimization: false,
      subagentReasoningEffort: "",
      subagentRoles: {},
      upstreamModelsByProvider: { "route-a": ["k3", "kimi-for-coding", "k3-256k"] },
      userScripts: [],
      webhook: { channels: [] },
    },
    busy: null,
    canSyncCurrentProvider: true,
    currentProvider: null,
    dirty: false,
    isBusy: false,
    modelState: {
      defaultModel: "k3",
      officialModelIds: [],
      officialModels: [],
      thirdPartyModels: ["k3", "kimi-for-coding", "k3-256k"],
      manualThirdPartyModels: [],
      upstreamModels: ["k3", "kimi-for-coding", "k3-256k"],
    },
    officialAccountAvailable: true,
    onConfigChange: () => {},
    onDeleteRoute: () => {},
    onFetchRouteModels: () => {},
    onNotice: () => {},
    onOfficialAccountsChanged: () => {},
    onOpenUsageAnalysis: () => {},
    onReorderRoute: async () => {},
    onReorderRouteModels: async (routeId, models) => {
      reorderCalls.push([routeId, models]);
    },
    onRequestConfirmation: () => {},
    onSaveRoute: async () => true,
    onSetDefaultModel: () => {},
    onSetRouteEnabled: async () => true,
    onToggleLocalRouter: () => {},
    onToggleRouteRequestLog: () => {},
    popupContainer: null,
    showAccountUsageInHeader: false,
    subagentModelOptions: [],
    ...overrides,
  };
  return { props, reorderCalls };
}

const modelHandles = (tree) =>
  collectElements(tree, (element) =>
    String(elementProps(element)["aria-label"] ?? "").startsWith("调整模型 "));
const modelItems = (tree) =>
  collectElements(tree, (element) =>
    String(elementProps(element).className ?? "").startsWith("model-tag-item"));
const routeHandles = (tree) =>
  collectElements(tree, (element) =>
    String(elementProps(element)["aria-label"] ?? "").startsWith("调整线路 "));

test("每个模型芯片带排序手柄，方向键在同一线路内换位并提交完整顺序", () => {
  const { props, reorderCalls } = modelSectionProps();
  modelSection.reset();
  const tree = modelSection.exports.ModelSection(props);
  assert.match(textContent(tree), /拖动线路或模型左侧手柄调整顺序/);
  assert.equal(routeHandles(tree).length, 1, "线路排序手柄保持不变");
  const handles = modelHandles(tree);
  assert.deepEqual(
    handles.map((handle) => elementProps(handle)["aria-label"]),
    ["调整模型 k3 的顺序", "调整模型 kimi-for-coding 的顺序", "调整模型 k3-256k 的顺序"],
  );
  assert.ok(handles.every((handle) => elementProps(handle).draggable === true));
  assert.ok(handles.every((handle) => elementProps(handle).disabled === false));

  elementProps(handles[2]).onKeyDown({ key: "ArrowLeft", preventDefault() {} });
  assert.deepEqual(reorderCalls, [["route-a", ["k3", "k3-256k", "kimi-for-coding"]]]);
  elementProps(handles[0]).onKeyDown({ key: "ArrowLeft", preventDefault() {} });
  elementProps(handles[0]).onKeyDown({ key: "Enter", preventDefault() {} });
  assert.equal(reorderCalls.length, 1, "首个模型不能再往前，非方向键不触发");
  elementProps(handles[0]).onKeyDown({ key: "ArrowRight", preventDefault() {} });
  assert.deepEqual(reorderCalls[1], ["route-a", ["kimi-for-coding", "k3", "k3-256k"]]);
});

test("拖动模型手柄到同线路另一芯片上会高亮目标并按落点重排", () => {
  const { props, reorderCalls } = modelSectionProps();
  modelSection.reset();
  let tree = modelSection.exports.ModelSection(props);
  const dataTransfer = { setData() {}, effectAllowed: "", dropEffect: "" };
  elementProps(modelHandles(tree)[0]).onDragStart({ dataTransfer });
  assert.equal(dataTransfer.effectAllowed, "move");

  modelSection.restart();
  tree = modelSection.exports.ModelSection(props);
  let items = modelItems(tree);
  assert.equal(items.length, 3);
  let prevented = false;
  elementProps(items[2]).onDragOver({ preventDefault: () => { prevented = true; }, dataTransfer });
  assert.ok(prevented, "同线路的其他芯片接受拖放");
  assert.equal(dataTransfer.dropEffect, "move");
  prevented = false;
  elementProps(items[0]).onDragOver({ preventDefault: () => { prevented = true; }, dataTransfer });
  assert.ok(!prevented, "被拖动的芯片自身不是落点");

  modelSection.restart();
  tree = modelSection.exports.ModelSection(props);
  items = modelItems(tree);
  assert.match(elementProps(items[2]).className, /is-drop-target/);
  assert.doesNotMatch(elementProps(items[1]).className, /is-drop-target/);
  elementProps(items[2]).onDrop({ preventDefault() {} });
  assert.deepEqual(reorderCalls, [["route-a", ["kimi-for-coding", "k3-256k", "k3"]]]);

  modelSection.restart();
  tree = modelSection.exports.ModelSection(props);
  assert.ok(modelItems(tree).every((item) => !/is-drop-target/.test(elementProps(item).className)));
});

test("有未保存改动或操作进行中时模型手柄禁用，本地路由关闭时不渲染手柄", () => {
  for (const overrides of [{ dirty: true }, { isBusy: true }]) {
    const { props, reorderCalls } = modelSectionProps(overrides);
    modelSection.reset();
    const tree = modelSection.exports.ModelSection(props);
    const handles = modelHandles(tree);
    assert.equal(handles.length, 3);
    assert.ok(handles.every((handle) => elementProps(handle).disabled === true));
    assert.ok(handles.every((handle) => elementProps(handle).draggable === false));
    const items = modelItems(tree);
    let prevented = false;
    elementProps(items[1]).onDragOver({ preventDefault: () => { prevented = true; }, dataTransfer: {} });
    assert.ok(!prevented);
    assert.equal(reorderCalls.length, 0);
  }
  const { props } = modelSectionProps();
  props.config = { ...props.config, localRouterEnabled: false };
  modelSection.reset();
  assert.equal(modelHandles(modelSection.exports.ModelSection(props)).length, 0);
});

test("控制台通过带版本号的排序命令保存模型顺序", async () => {
  const [app, api] = await Promise.all([readSource("src/App.tsx"), readSource("src/api.ts")]);
  assert.match(app, /onReorderRouteModels=\{handleReorderRouteModels\}/);
  assert.match(
    app,
    /"reorder_route_models",\s*\{\s*routeId,\s*models,\s*expectedRevision: config\.settingsRevision,\s*\}/,
  );
  assert.match(app, /if \(!config \|\| dirty \|\| isBusy \|\| !config\.localRouterEnabled\) return;\s*const route = config\.profiles\.find\(\(profile\) => profile\.id === routeId\);/);
  assert.match(api, /"reorder_route_models",/);
});
