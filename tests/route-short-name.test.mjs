import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";
import { autoStubModule, collectElements, createModuleGraph, elementProps } from "./helpers/jsx-tree.mjs";

const shortNames = await loadTypeScriptModule(
  new URL("../src/routeShortNames.ts", import.meta.url),
);

test("third-party route short names are required and limited to four characters", () => {
  assert.equal(shortNames.MAX_ROUTE_SHORT_NAME_CHARACTERS, 4);
  assert.equal(shortNames.validateThirdPartyRouteShortName(""), "请输入短名称");
  assert.equal(shortNames.validateThirdPartyRouteShortName("   "), "请输入短名称");
  assert.equal(shortNames.validateThirdPartyRouteShortName("中转"), "");
  assert.equal(shortNames.validateThirdPartyRouteShortName("中转线路"), "");
  assert.equal(
    shortNames.validateThirdPartyRouteShortName("中转线路名"),
    "短名称最多 4 个字符",
  );
  assert.equal(
    shortNames.validateThirdPartyRouteShortName("官"),
    "短名称 官 仅供官方账号使用",
  );
  assert.equal(
    shortNames.validateThirdPartyRouteShortName(
      "中转",
      [{ id: "existing", authMode: "apiKey", officialAccount: false, shortName: "中转" }],
      "draft",
    ),
    "短名称 中转 已被其他线路使用",
  );
});

test("route labels use the first two name characters or a custom short name", () => {
  const officialDefault = {
    authMode: "officialAccount",
    officialAccount: true,
    name: "OpenAI 官方直登",
    shortName: "",
  };
  const officialCustom = {
    authMode: "officialAccount",
    officialAccount: true,
    name: "OpenAI 官方直登",
    shortName: "官1",
  };
  const relay = {
    authMode: "apiKey",
    officialAccount: false,
    name: "备用中转线路",
    shortName: "备",
  };

  assert.equal(shortNames.fallbackRouteShortName(" 备用中转 "), "备用");
  assert.equal(shortNames.fallbackRouteShortName("😀备用"), "😀备");
  assert.equal(shortNames.fallbackRouteShortName("主"), "主");
  assert.equal(shortNames.prefixedRouteName({ ...officialDefault, name: "Plus" }), "[Pl] Plus");
  assert.equal(shortNames.prefixedRouteName(officialCustom), "[官1] OpenAI 官方直登");
  assert.equal(shortNames.prefixedRouteName({ ...relay, shortName: "", name: "自建" }), "[自建] 自建");
  assert.equal(shortNames.prefixedRouteName({ ...relay, shortName: " 晚 ", name: "晚安" }), "[晚] 晚安");
  assert.equal(shortNames.prefixedRouteName({ ...relay, name: "" }), "备");
  assert.equal(shortNames.prefixedRouteName({ ...relay, name: "   " }), "备");
});

test("the native provider model options use the same fallback without a stored route", () => {
  const { exports } = createModuleGraph(new URL("../src/subagentModels.ts", import.meta.url));
  const modelState = {
    officialModels: [],
    officialModelIds: ["gpt-6-luna"],
    thirdPartyModels: ["gpt-6-luna"],
  };
  const config = { profiles: [], localRouterEnabled: false };
  for (const [name, official, expected] of [["Plus", true, "Pl"], ["😀备用", false, "😀备"]]) {
    const options = exports.buildSubagentModelOptions(config, modelState, true, {
      id: "native",
      name,
      official,
    });
    assert.equal(options[0].routePrefix, expected);
    assert.equal(options[0].value, "gpt-6-luna");
  }
});

test("official route short names are required and only conflict with third-party routes", () => {
  const profiles = [
    {
      id: "official",
      authMode: "officialAccount",
      officialAccount: true,
      name: "官方线路",
      shortName: "官",
    },
    {
      id: "relay",
      authMode: "apiKey",
      officialAccount: false,
      name: "中转线路",
      shortName: "中转",
    },
  ];

  assert.equal(shortNames.validateOfficialRouteShortName(""), "请输入短名称");
  assert.equal(shortNames.validateOfficialRouteShortName("   "), "请输入短名称");
  assert.equal(shortNames.validateOfficialRouteShortName("自"), "");
  assert.equal(shortNames.validateOfficialRouteShortName("官方线路"), "");
  assert.equal(
    shortNames.validateOfficialRouteShortName("官方线路名"),
    "短名称最多 4 个字符",
  );
  assert.equal(
    shortNames.validateOfficialRouteShortName("中转", profiles),
    "短名称「中转」已被其他线路使用",
  );
});

test("official settings allow an empty long name and reject an empty short name", () => {
  const { exports } = createModuleGraph(new URL("../src/officialRouteSettings.ts", import.meta.url));
  const draft = { routeName: "", routeShortName: "主", baseUrl: "", upstreamProxy: "", maxConcurrency: "0" };
  assert.deepEqual(exports.validateOfficialRouteSettings(draft, [], [], "account"), {
    routeName: "", shortName: "", baseUrl: "", upstreamProxy: "", maxConcurrency: "",
  });
  assert.equal(exports.validateOfficialRouteSettings({ ...draft, routeShortName: " " }, [], [], "account").shortName, "请输入短名称");
});

test("official concurrency accepts zero and unsigned integers without coercing invalid drafts", () => {
  const { exports } = createModuleGraph(new URL("../src/officialRouteSettings.ts", import.meta.url));
  for (const value of ["0", "1", "16", "4294967295", " 4 "]) {
    assert.equal(exports.validateOfficialRouteConcurrency(value), "", value);
  }
  for (const value of ["", " ", "-1", "1.5", "1e3", "0x10", "NaN", "Infinity", "4294967296", "99999999999999999999999"]) {
    assert.notEqual(exports.validateOfficialRouteConcurrency(value), "", value);
  }
});

test("subagent route options preserve a blank long name without changing model IDs", () => {
  const { exports } = createModuleGraph(new URL("../src/subagentModels.ts", import.meta.url));
  const profile = { id: "relay", authMode: "apiKey", name: "", shortName: "主" };
  const state = { officialModels: [], officialModelIds: [], thirdPartyModels: ["shared"] };
  for (const localRouterEnabled of [true, false]) {
    const options = exports.buildSubagentModelOptions({
      profiles: [profile], localRouterEnabled, activeProfileId: "relay",
      selectedModelsByProvider: { relay: ["shared"] }, declaredOfficialModelsByProvider: {},
    }, state, false, { id: "relay", name: "", official: false });
    assert.equal(options[0].routeName, "");
    assert.equal(options[0].routePrefix, "主");
    assert.equal(options[0].value, localRouterEnabled ? "relay/shared" : "shared");
  }
});

test("subagent model groups display only the short name when the long name is blank", () => {
  const heroui = autoStubModule("heroui");
  const graph = createModuleGraph(new URL("../src/components/ModelCombobox.tsx", import.meta.url), {
    stubs: {
      "@heroui/react": new Proxy(heroui, { get: (target, key) => key === "useFilter" ? () => ({ contains: () => true }) : target[key] }),
      "@tabler/icons-react": autoStubModule("icons"),
    },
  });
  const tree = graph.exports.ModelCombobox({
    "aria-label": "模型", value: "relay/shared", onChange: () => {},
    options: [
      { value: "relay/shared", modelId: "shared", label: "shared", routeId: "relay", routeName: "", routePrefix: "主" },
      { value: "backup/shared", modelId: "shared", label: "shared", routeId: "backup", routeName: "备用线路", routePrefix: "备" },
    ],
  });
  const list = collectElements(tree, (element) => Array.isArray(elementProps(element).items))[0];
  assert.deepEqual(elementProps(list).items.map((group) => group.label), ["主", "[备] 备用线路"]);
  assert.equal(elementProps(list).items[0].options[0].id, "relay/shared");
});

test("preview saving blank official long names survives derivation and missing short names fail without mutations", async () => {
  const sourceUrl = new URL("../src/dev/mockApi.ts", import.meta.url);
  const source = (await readFile(sourceUrl, "utf8")).replace("import.meta.env.DEV", "true");
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  }).outputText;
  const previousWindow = globalThis.window;
  const previewWindow = { location: { search: "" } };
  globalThis.window = previewWindow;
  try {
    new Function("require", "exports", "window", "console", "setTimeout", compiled)(
      (specifier) => createModuleGraph(new URL(`${specifier}.ts`, sourceUrl)).exports,
      {}, previewWindow, { log: () => {} }, (callback) => callback(),
    );
    const invoke = previewWindow.__codeyInvokeApi;
    const loaded = await invoke("load_codey_config", {});
    const route = loaded.config.profiles.find((profile) => profile.officialAccountId === "acct_preview_1");
    const model = loaded.modelState.officialModelIds[0];
    const saved = await invoke("save_official_route_models", {
      routeId: route.id, models: [model], accountId: "acct_preview_1",
      routeName: " ", routeShortName: "主官", baseUrl: "", upstreamProxy: "", maxConcurrency: 3,
    });
    assert.equal(saved.status, "ok");
    assert.equal(saved.accounts.find((account) => account.id === "acct_preview_1").routeName, "");
    assert.equal(saved.config.profiles.find((profile) => profile.officialAccountId === "acct_preview_1").name, "");
    assert.equal(saved.accounts.find((account) => account.id === "acct_preview_1").maxConcurrency, 3);
    assert.equal(saved.config.profiles.find((profile) => profile.id === route.id).maxConcurrency, 3);
    const modelSynced = await invoke("save_official_route_models", { routeId: route.id, models: [model] });
    assert.equal(modelSynced.config.profiles.find((profile) => profile.id === route.id).maxConcurrency, 3);
    for (const maxConcurrency of [-1, 1.5, 4294967296]) {
      const invalid = await invoke("save_official_route_models", {
        routeId: route.id, models: [model], accountId: "acct_preview_1",
        routeName: "不应保存", routeShortName: "主官", maxConcurrency,
      });
      assert.equal(invalid.status, "failed");
      assert.equal(saved.accounts.find((account) => account.id === "acct_preview_1").maxConcurrency, 3);
      assert.equal(saved.accounts.find((account) => account.id === "acct_preview_1").routeName, "");
    }
    const rejected = await invoke("save_official_route_models", {
      routeId: route.id, models: [model], accountId: "acct_preview_1",
      routeName: "不应保存", routeShortName: " ", baseUrl: "", upstreamProxy: "",
    });
    assert.equal(rejected.status, "failed");
    assert.equal(rejected.message, "请输入短名称");
    assert.equal(saved.accounts.find((account) => account.id === "acct_preview_1").routeName, "");
    const profiles = saved.config.profiles.map((profile) => profile.authMode === "apiKey" && profile.id === saved.config.profiles.find((candidate) => candidate.authMode === "apiKey").id
      ? { ...profile, name: "", shortName: "自" }
      : profile);
    const configSaved = await invoke("save_codey_config", { config: { ...saved.config, profiles } });
    assert.equal(configSaved.config.profiles.find((profile) => profile.shortName === "自").name, "");
    const invalidProfiles = profiles.map((profile) => profile.shortName === "自" ? { ...profile, shortName: "" } : profile);
    assert.equal((await invoke("save_codey_config", { config: { ...saved.config, profiles: invalidProfiles } })).message, "请输入短名称");
  } finally {
    globalThis.window = previousWindow;
  }
});
