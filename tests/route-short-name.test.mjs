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

test("route and model labels use the first two name characters or a custom short name", () => {
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

  assert.equal(shortNames.prefixedRouteModelName(officialDefault, "gpt-5.6-sol"), "[Op] gpt-5.6-sol");
  assert.equal(shortNames.prefixedRouteModelName(officialCustom, "gpt-5.6-sol"), "[官1] gpt-5.6-sol");
  assert.equal(shortNames.prefixedRouteModelName(relay, "claude-opus"), "[备] claude-opus");
  assert.equal(shortNames.fallbackRouteShortName(" 备用中转 "), "备用");
  assert.equal(shortNames.fallbackRouteShortName("😀备用"), "😀备");
  assert.equal(shortNames.fallbackRouteShortName("主"), "主");
  assert.equal(shortNames.prefixedRouteName({ ...officialDefault, name: "Plus" }), "[Pl] Plus");
  assert.equal(shortNames.prefixedRouteName({ ...relay, shortName: "", name: "自建" }), "[自建] 自建");
  assert.equal(shortNames.prefixedRouteName({ ...relay, shortName: " 晚 ", name: "晚安" }), "[晚] 晚安");
  assert.equal(shortNames.prefixedRouteName({ ...relay, name: "" }), "备");
  assert.equal(shortNames.prefixedRouteName({ ...relay, name: "   " }), "备");
});

test("the third-party route editor exposes the route-name and short-name fields with maxLength", async () => {
  const source = await readFile(
    new URL("../src/ModelSection.tsx", import.meta.url),
    "utf8",
  );

  assert.match(source, /id="route-name-input"/);
  assert.match(source, /id="route-short-name-input"/);
  assert.doesNotMatch(source, /短名称（可选）|留空使用线路名称前两个字符/);
  assert.match(source, /线路名（可选）/);
  assert.ok(source.indexOf('id="route-short-name-input"') < source.indexOf('id="route-name-input"'));
  assert.ok(source.indexOf('id="official-route-short-name-input"') < source.indexOf('id="official-route-name-input"'));
  assert.match(source, /\{prefixedRouteName\(profile\)\}/);
  assert.match(source, /maxLength=\{MAX_ROUTE_NAME_CHARACTERS\}/);
  assert.match(source, /maxLength=\{MAX_ROUTE_SHORT_NAME_CHARACTERS\}/);
  assert.doesNotMatch(source, /最多 2 个字符且不可重复，模型名称前会显示为 \[短名称\]/);
  assert.match(
    source,
    /validateThirdPartyRouteShortName\(route\.shortName, profiles, route\.id\)/,
  );
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

test("official route settings are edited on the route card instead of the account row", async () => {
  const [modelSection, panel, app] = await Promise.all([
    readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  ]);

  assert.match(modelSection, /id="official-route-name-input"/);
  assert.match(modelSection, /id="official-route-short-name-input"/);
  assert.match(modelSection, /id="official-route-base-url-input"/);
  assert.match(modelSection, /留空使用官方默认网关/);
  assert.match(modelSection, /maxLength=\{MAX_ROUTE_NAME_CHARACTERS\}/);
  assert.match(modelSection, /maxLength=\{MAX_ROUTE_SHORT_NAME_CHARACTERS\}/);
  assert.match(modelSection, /validateOfficialRouteSettings\(/);
  assert.match(modelSection, /官方账号登录 · \$\{email\}/);
  // 编辑按钮对所有线路渲染，只有第三方线路才有删除按钮。
  const editButton = modelSection.indexOf("aria-label={`编辑线路 ${profile.name}`}");
  const deleteGuard = modelSection.indexOf("{!isOfficial && (");
  assert.ok(editButton > 0 && deleteGuard > editButton);
  // 编辑只改线路名、短名称、网关和代理，模型列举交给同步入口。
  assert.match(modelSection, /openRouteDialog\(profile, isOfficial \? "settings" : null\)/);
  assert.match(modelSection, /const syncModels = \(\) => onFetchRouteModels\(profile\)/);
  assert.match(modelSection, /\{officialDialogScope !== "models" && \(/);
  assert.match(modelSection, /\{officialDialogScope !== "settings" && \(/);

  assert.match(panel, /aria-label=\{`移除官方账号 \$\{label\}`\}/);
  assert.doesNotMatch(panel, /IconPencil|validateOutboundProxyUrl/);

  assert.match(app, /accountId: routeSettings.accountId/);
  assert.match(app, /routeName: routeSettings.routeName/);
  assert.match(app, /routeShortName: routeSettings.routeShortName/);
  assert.match(app, /baseUrl: routeSettings.baseUrl/);
});

test("official accounts derive missing route settings and keep explicit blank long names", async () => {
  const [modelSection, mock] = await Promise.all([
    readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/dev/mockApi.ts", import.meta.url), "utf8"),
  ]);

  assert.match(mock, /const previewOfficialRouteName = \(index: number\) => `官方账号\$\{index\}`/);
  assert.match(mock, /account\.routeName !== undefined/);
  assert.match(mock, /shortName: account\.routeShortName \|\| fallbackRouteShortName/);
  assert.doesNotMatch(mock, /previewOfficialRouteShortName/);
  assert.match(mock, /previewEnsureGeneratedRouteSettings\(\);/);
  assert.doesNotMatch(mock, /previewOfficialDerivedRouteName/);

  assert.doesNotMatch(modelSection, /留空则按账号添加顺序使用默认线路名，例如「官方账号1」/);
  assert.doesNotMatch(modelSection, /留空则按账号添加顺序使用默认短名称，例如「官1」/);
  assert.doesNotMatch(modelSection, /最多 2 个字符且不可重复，模型名称前会显示为 \[短名称\]/);
});

test("official settings allow an empty long name and reject an empty short name", () => {
  const { exports } = createModuleGraph(new URL("../src/officialRouteSettings.ts", import.meta.url));
  const draft = { routeName: "", routeShortName: "主", baseUrl: "", upstreamProxy: "" };
  assert.deepEqual(exports.validateOfficialRouteSettings(draft, [], [], "account"), {
    routeName: "", shortName: "", baseUrl: "", upstreamProxy: "",
  });
  assert.equal(exports.validateOfficialRouteSettings({ ...draft, routeShortName: " " }, [], [], "account").shortName, "请输入短名称");
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
    const saved = await invoke("save_official_account_route_settings", {
      accountId: "acct_preview_1", routeName: " ", routeShortName: "主官", baseUrl: "", upstreamProxy: "",
    });
    assert.equal(saved.status, "ok");
    assert.equal(saved.accounts.find((account) => account.id === "acct_preview_1").routeName, "");
    assert.equal(saved.config.profiles.find((profile) => profile.officialAccountId === "acct_preview_1").name, "");
    const rejected = await invoke("save_official_account_route_settings", {
      accountId: "acct_preview_1", routeName: "不应保存", routeShortName: " ", baseUrl: "", upstreamProxy: "",
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

test("the route-name limit is shared by the renderer and the official account command", async () => {
  const [settings, backendConfig, officialAccounts] = await Promise.all([
    readFile(new URL("../src/officialRouteSettings.ts", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/config.rs", import.meta.url), "utf8"),
    readFile(
      new URL("../backend/src/commands/official_accounts.rs", import.meta.url),
      "utf8",
    ),
  ]);

  assert.match(settings, /export const MAX_ROUTE_NAME_CHARACTERS = 15;/);
  assert.match(backendConfig, /pub const MAX_ROUTE_NAME_CHARS: usize = 15;/);
  // 后端保存线路时使用同一个上限，直接调用接口也写不进界面存不下的名称。
  assert.match(
    officialAccounts,
    /if route_name\.chars\(\)\.count\(\) > MAX_ROUTE_NAME_CHARS \{/,
  );
  assert.doesNotMatch(officialAccounts, /MAX_OFFICIAL_ROUTE_NAME_CHARS/);
});
