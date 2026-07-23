import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const SUPPORTED_MODELS = [
  "gpt-5.6-sol",
  "gpt-5.6-luna",
  "gpt-5.5",
  "gpt-5.4",
  "gpt-5.4-mini",
  "gpt-5.3-codex-spark",
];

async function loadModelWhitelistPatch() {
  const source = await readFile(
    new URL(
      "../vendor/CodexPlusPlus/assets/inject/renderer-inject.js",
      import.meta.url,
    ),
    "utf8",
  );
  const start = source.indexOf("function uniqueValues");
  const end = source.indexOf("function statsigClients");
  assert.notEqual(start, -1, "model whitelist helpers should be present");
  assert.notEqual(end, -1, "model whitelist helper boundary should be present");

  return Function(`
    ${source.slice(start, end)}
    return {
      codexPlusModelDescriptor,
      codexPlusModelNames,
      patchModelArray,
      patchModelContainer,
      patchStatsigModelDynamicConfig,
      setCatalog(value) {
        codexModelCatalog = value;
      },
    };
  `)();
}

test("model whitelist keeps supported official models before configured models", async () => {
  const patch = await loadModelWhitelistPatch();
  patch.setCatalog({
    status: "ok",
    default_model: "gpt-5.6-terra",
    model: "gpt-5.2",
    model_provider: "codey_global",
    provider_name: "Codey",
    models: [
      "gpt-5.6-sol",
      "gpt-5.6-luna",
      "gpt-5.5",
      "gpt-5.4",
      "gpt-5.4-mini",
      "gpt-5.3-codex-spark",
      "provider-fast-coder",
    ],
  });

  const models = [
    { model: "gpt-5.2", hidden: false, isDefault: true },
    {
      model: "gpt-5.5",
      hidden: false,
      isDefault: false,
      serviceTiers: [{ id: "priority", name: "Fast", iconKind: "legacy-bolt" }],
      additionalSpeedTiers: ["fast"],
    },
    { model: "gpt-5.6-sol", hidden: false, isDefault: false },
  ];
  assert.equal(patch.patchModelArray(models), true);
  assert.deepEqual(
    models.map((model) => model.model),
    [...SUPPORTED_MODELS, "provider-fast-coder"],
  );
  assert.equal(models.some((model) => model.model === "gpt-5.2"), false);
  assert.deepEqual(
    models.filter((model) => model.isDefault).map((model) => model.model),
    ["gpt-5.6-sol"],
  );
  assert.ok(models.every((model) =>
    model.serviceTiers.some((tier) => tier.id === "priority")
    && model.additionalSpeedTiers.includes("fast")));
  assert.ok(models.every((model) =>
    model.serviceTiers.every((tier) => !Object.hasOwn(tier, "iconKind"))));
  assert.deepEqual(patch.codexPlusModelNames(), [...SUPPORTED_MODELS, "provider-fast-coder"]);
});

test("model whitelist replaces every availability container and default", async () => {
  const patch = await loadModelWhitelistPatch();
  const allowed = [
    "gpt-5.6-sol",
    "gpt-5.3-codex-spark",
    "provider-fast-coder",
  ];
  patch.setCatalog({
    status: "ok",
    default_model: "gpt-5.6-sol",
    model: "gpt-5.6-sol",
    models: allowed,
  });

  const container = {
    models: [
      { model: "gpt-5.2", hidden: false, isDefault: true },
      { model: "gpt-5.6-sol", hidden: false, isDefault: false },
    ],
    availableModels: new Set(["gpt-5.2", "gpt-5.6-sol"]),
    available_models: ["gpt-5.2"],
    defaultModel: "gpt-5.2",
  };
  assert.equal(patch.patchModelContainer(container), true);
  assert.deepEqual(container.models.map((model) => model.model), allowed);
  assert.deepEqual([...container.availableModels], allowed);
  assert.deepEqual(container.available_models, allowed);
  assert.equal(container.defaultModel, "gpt-5.6-sol");
  assert.equal(container.model, "gpt-5.6-sol");

  const statsig = {
    value: {
      available_models: ["gpt-5.2", "gpt-5.6-sol"],
      default_model: "gpt-5.2",
    },
  };
  patch.patchStatsigModelDynamicConfig(statsig);
  assert.deepEqual(statsig.value.available_models, allowed);
  assert.equal(statsig.value.default_model, "gpt-5.6-sol");
});
