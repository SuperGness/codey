import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";
import {
  autoStubModule,
  collectElements,
  createModuleGraph,
  elementProps,
  elementType,
} from "./helpers/jsx-tree.mjs";

const [promptSource, apiSource, backendSource] = await Promise.all([
  readSource("src/PromptOptimizationCard.tsx"),
  readSource("src/api.ts"),
  readSource("backend/src/commands.rs"),
]);

// 组件库/图标/HeroUI 按名字生成桩，真实执行组件函数后断言元素树：
// API Key 输入必须以 PasswordInput 渲染，且带上 autoComplete="new-password"。
const ui = autoStubModule("ui");
const icons = autoStubModule("icon");
const heroui = autoStubModule("heroui");

const promptCard = await createModuleGraph(
  new URL("../src/PromptOptimizationCard.tsx", import.meta.url),
  {
    stubs: {
      "@heroui/react": heroui,
      "@tabler/icons-react": icons,
      "./api": { invoke: async () => ({}) },
      "./appUtils": { errorText: String, withTimeout: (promise) => promise },
      "./components/ManualModelCombobox": autoStubModule("manual-combobox"),
      "./components/ModelCombobox": autoStubModule("combobox"),
      "./components/ui": ui,
      "./SettingsPageHeader": autoStubModule("settings-header"),
      "./urlValidation": { validateOutboundApiUrl: () => "" },
    },
  },
);

const modelSection = createModuleGraph(
  new URL("../src/ModelSection.tsx", import.meta.url),
  {
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
  },
);

const passwordInputs = (tree) =>
  collectElements(tree, (element) => elementType(element) === ui.PasswordInput);
const routeKeyInputs = (tree) =>
  collectElements(tree, (element) => elementProps(element).id === "route-key-input");

function renderPromptCard(optimizationOverrides = {}) {
  promptCard.reset();
  return promptCard.exports.PromptOptimizationCard({
    config: {
      localRouterEnabled: true,
      settingsRevision: 1,
      promptOptimization: {
        enabled: true,
        mode: "manual",
        model: "gpt-5",
        instruction: "",
        upstreamProtocol: "openaiResponses",
        baseUrl: "https://api.example.com/v1",
        apiKey: "",
        apiKeyConfigured: false,
        clearApiKey: false,
        ...optimizationOverrides,
      },
    },
    isBusy: false,
    subagentModelOptions: [],
    onConfigChange: () => {},
    onNotice: () => {},
  });
}

// 线路编辑器依赖真实 config/modelState 结构：给出最小可用的一整套，让组件按生产路径渲染。
function modelSectionProps() {
  return {
    config: {
      activeProfileId: "route-a",
      autoCheckCodeyUpdates: true,
      codexAppPath: "",
      declaredOfficialModelsByProvider: {},
      defaultModel: "",
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
          apiKeyConfigured: false,
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
      selectedModelsByProvider: {},
      settingsRevision: 1,
      showAccountUsageInHeader: false,
      slimCodexPet: false,
      streamMaxRetries: 3,
      subagentModel: "",
      subagentModelOptions: [],
      subagentOptimization: false,
      subagentReasoningEffort: "",
      subagentRoles: {},
      upstreamModelsByProvider: {},
      userScripts: [],
      webhook: { channels: [] },
    },
    busy: null,
    canSyncCurrentProvider: false,
    currentProvider: null,
    dirty: false,
    isBusy: false,
    modelState: {
      defaultModel: "",
      officialModelIds: [],
      officialModels: [],
      thirdPartyModels: [],
    },
    officialAccountAvailable: true,
    onConfigChange: () => {},
    onDeleteRoute: () => {},
    onFetchRouteModels: () => {},
    onNotice: () => {},
    onOfficialAccountsChanged: () => {},
    onOpenUsageAnalysis: () => {},
    onReorderRoute: async () => {},
    onReorderRouteModels: async () => {},
    onRequestConfirmation: () => {},
    onSaveRoute: async () => true,
    onSetDefaultModel: () => {},
    onSetRouteEnabled: async () => true,
    onToggleLocalRouter: () => {},
    onToggleRouteRequestLog: () => {},
    popupContainer: null,
    showAccountUsageInHeader: false,
    subagentModelOptions: [],
  };
}

test("提示词优化的 API Key 渲染为带 autoComplete=new-password 的 PasswordInput", () => {
  const tree = renderPromptCard();
  const inputs = passwordInputs(tree);
  assert.equal(inputs.length, 1, "API Key 必须使用 PasswordInput");
  const [input] = inputs;
  assert.equal(elementProps(input).autoComplete, "new-password");
  assert.equal(elementProps(input).type, undefined, "可见性不由组件硬编码");
  assert.equal(typeof elementProps(input).onVisibilityChange, "function");
  assert.match(elementProps(input).id, /-api-key$/);
  assert.equal(
    collectElements(tree, (element) => elementProps(element).type === "password")
      .length,
    0,
    "不得退回普通 Input 的 password 类型",
  );
});

test("已保存的 Key 用占位符提示可替换，不提供查看明文入口", () => {
  const tree = renderPromptCard({ apiKeyConfigured: true });
  assert.equal(
    elementProps(passwordInputs(tree)[0]).placeholder,
    "已保存（输入新 Key 可替换）",
  );
  assert.doesNotMatch(promptSource, /Key 已保存；点击眼睛可查看/);
});

test("配置接口不提供明文回读或清空 Key 的后端命令", () => {
  assert.doesNotMatch(apiSource, /reveal_(?:route|prompt_optimization)_api_key/);
  assert.doesNotMatch(backendSource, /profile\.api_key\.clear\(\)/);
  assert.doesNotMatch(backendSource, /prompt_optimization\.api_key\.clear\(\)/);
});

test("线路 Key 输入在打开编辑弹窗后渲染为带 autoComplete=new-password 的 PasswordInput", () => {
  // 线路编辑器只在弹窗打开后渲染，这里用真实交互打开它，而不是匹配源码。
  modelSection.reset();
  const props = modelSectionProps();
  const initial = modelSection.exports.ModelSection(props);
  assert.equal(routeKeyInputs(initial).length, 0, "弹窗未打开时不应渲染线路 Key 输入");

  elementProps(
    collectElements(
      initial,
      (element) => elementProps(element)["aria-label"] === "编辑线路 线路 A",
    )[0],
  ).onClick();

  const rerender = () => {
    modelSection.restart();
    const tree = modelSection.exports.ModelSection(props);
    const [input] = routeKeyInputs(tree);
    return { input, tree, visibility: elementProps(input).visibility };
  };
  const { input, tree } = rerender();
  assert.ok(input, "编辑线路后必须渲染 Key 输入");
  assert.equal(elementType(input), ui.PasswordInput, "线路 Key 必须使用 PasswordInput");
  assert.equal(elementProps(input).autoComplete, "new-password");
  assert.equal(elementProps(input).type, undefined, "可见性不由组件硬编码");
  assert.equal(typeof elementProps(input).onVisibilityChange, "function");
  assert.equal(elementProps(input).value, "");
  assert.equal(
    collectElements(tree, (element) => elementProps(element).type === "password")
      .length,
    0,
    "不得退回普通 Input 的 password 类型",
  );

  // 可见性切换必须真的接回组件状态。
  elementProps(input).onVisibilityChange();
  assert.equal(rerender().visibility, true);
});
