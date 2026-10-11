import assert from "node:assert/strict";
import test from "node:test";

import { autoStubModule, collectElements, createModuleGraph, elementProps } from "./helpers/jsx-tree.mjs";

const model = "gpt-5.6-sol";
const policy = { contextWindowTokens: 128_000, autoCompactTokenLimit: 100_000, reserveOutputTokens: 16_000 };
const modelState = {
  officialModels: [{ slug: model, supported: true }],
  officialModelIds: [model],
  thirdPartyModels: [],
  manualThirdPartyModels: [],
  upstreamModels: [],
  defaultModel: model,
};

function officialPicker(localRouterEnabled = true) {
  const calls = [];
  const config = {
    activeProfileId: "official",
    localRouterEnabled,
    profiles: [{ id: "official", sourceProviderId: "account-provider", authMode: "officialAccount" }],
    selectedModelsByProvider: { "account-provider": [model] },
    modelContextByProvider: { "account-provider": { [model]: policy } },
  };
  const graph = createModuleGraph(new URL("../src/useModelSelection.ts", import.meta.url), {
    stubs: {
      "./api": { invoke: async (command, args) => {
        calls.push({ command, args });
        return { config, modelState, restartRequired: true };
      } },
      "./subagentModels": { buildSubagentModelOptions: () => [] },
    },
  });
  const options = {
    config, currentProvider: null, officialAccountAvailable: true,
    runOperation: async (_name, action) => action(),
    setPersistedConfig: () => {}, setStatus: () => {}, setNotice: () => {},
  };
  const render = () => {
    graph.restart();
    return graph.exports.useModelSelection(options);
  };
  render().openModelPicker(modelState, "", "official");
  return { render, calls, config };
}

test("官方模型选择器提交预算，并过滤不属于该线路的模型", async () => {
  const { render, calls, config } = officialPicker();
  assert.deepEqual(render().draftModelContexts, { [model]: policy });
  const updated = { ...policy, autoCompactTokenLimit: 90_000 };
  render().updateDraftModelContext(model.toUpperCase(), updated);
  render().updateDraftModelContext("unknown-model", policy);
  await render().saveModelSelection();
  assert.deepEqual(calls, [{
    command: "save_official_route_models",
    args: { routeId: "official", models: [model], modelContexts: { [model.toUpperCase()]: updated } },
  }]);
  assert.deepEqual(config.modelContextByProvider["account-provider"], { [model]: policy });
});

test("官方模型恢复默认提交空预算，原生模式省略预算变更", async () => {
  const writable = officialPicker();
  writable.render().updateDraftModelContext(model, undefined);
  await writable.render().saveModelSelection();
  assert.deepEqual(writable.calls[0].args.modelContexts, {});
  const native = officialPicker(false);
  await native.render().saveModelSelection();
  assert.equal(Object.hasOwn(native.calls[0].args, "modelContexts"), false);
  assert.equal(Object.hasOwn(native.calls[0].args, "reasoningEfforts"), false);
});

test("上下文输入以 K 为单位，非法输入不能改变预算", () => {
  const calls = [];
  const graph = createModuleGraph(new URL("../src/components/ModelContextWindowCombobox.tsx", import.meta.url), {
    stubs: { "@heroui/react": autoStubModule("heroui") },
  });
  const tree = graph.exports.ModelContextWindowCombobox({
    value: 128_000, onChange: (value) => calls.push(value),
  });
  const props = elementProps(tree);
  const inputProps = elementProps(collectElements(tree, (node) =>
    elementProps(node)?.placeholder === "256",
  )[0]);
  assert.equal(inputProps.inputMode, "decimal");
  for (const value of ["-128", "128.0005", "abc", "128abc", "12,34", "1e3", "10001"]) {
    props.onInputChange(value);
  }
  assert.deepEqual(calls, []);
  props.onInputChange("256");
  props.onInputChange("128.5");
  props.onInputChange(" 1,000 ");
  props.onInputChange("");
  props.onSelectionChange("1000000");
  assert.deepEqual(calls, [256_000, 128_500, 1_000_000, undefined, 1_000_000]);
});

test("阈值和输出预留要求明确窗口，不能隐式创建固定窗口", () => {
  const calls = [];
  const graph = createModuleGraph(new URL("../src/components/ModelSettingsFields.tsx", import.meta.url), {
    stubs: {
      "@heroui/react": autoStubModule("heroui"),
      "@tabler/icons-react": autoStubModule("icons"),
      "./ui": autoStubModule("ui"),
    },
  });
  const render = (currentPolicy) => graph.exports.ModelSettingsFields({
    model, disabled: false, policy: currentPolicy, onChange: (value) => calls.push(value),
  });
  const input = (tree, label) => elementProps(collectElements(tree, (node) =>
    elementProps(node)?.ariaLabel === `${model} ${label} K Token`,
  )[0]);
  for (const label of ["压缩阈值", "输出预留"]) {
    const field = input(render(undefined), label);
    assert.equal(field.disabled, true);
    field.onChange(16_000);
  }
  assert.deepEqual(calls, []);
  const explicit = { contextWindowTokens: 272_000 };
  const threshold = input(render(explicit), "压缩阈值");
  assert.equal(threshold.disabled, false);
  threshold.onChange(220_000);
  input(render(explicit), "输出预留").onChange(500);
  assert.deepEqual(calls, [
    { ...explicit, autoCompactTokenLimit: 220_000 },
    { ...explicit, reserveOutputTokens: 500 },
  ]);
  const window = elementProps(collectElements(render(undefined), (node) =>
    elementProps(node)?.ariaLabel === `${model} 窗口 K Token`,
  )[0]);
  assert.equal(window.placeholder, "跟随模型目录");
  window.onChange(1_000_000);
  assert.deepEqual(calls.at(-1), { contextWindowTokens: 1_000_000 });
  window.onChange(undefined);
  assert.equal(calls.at(-1), undefined);
  assert.equal(input(render(undefined), "压缩阈值").disabled, true);
});

test("阈值与输出预留输入以 K 为单位并校验范围", () => {
  const graph = createModuleGraph(new URL("../src/components/ModelSettingsFields.tsx", import.meta.url), {
    stubs: {
      "@heroui/react": autoStubModule("heroui"),
      "@tabler/icons-react": autoStubModule("icons"),
      "./ui": autoStubModule("ui"),
    },
  });
  const calls = [];
  const input = elementProps(graph.exports.TokenKInput({
    ariaLabel: "压缩阈值 K Token", disabled: false, maxTokens: 10_000_000,
    minTokens: 1, placeholder: "自动", value: 16_000,
    onChange: (tokens) => calls.push(tokens),
  }));
  assert.equal(input.inputMode, "decimal");
  input.onChange({ target: { value: "220" } });
  input.onChange({ target: { value: "0.5" } });
  input.onChange({ target: { value: "" } });
  assert.deepEqual(calls, [220_000, 500, undefined]);
  for (const value of ["abc", "12.3456", "-3", "10001"]) {
    input.onChange({ target: { value } });
  }
  assert.deepEqual(calls, [220_000, 500, undefined]);
});
