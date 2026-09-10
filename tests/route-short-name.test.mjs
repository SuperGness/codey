import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const shortNames = await loadTypeScriptModule(
  new URL("../src/routeShortNames.ts", import.meta.url),
);

test("third-party route short names are required and limited to two characters", () => {
  assert.equal(shortNames.MAX_ROUTE_SHORT_NAME_CHARACTERS, 2);
  assert.equal(shortNames.validateThirdPartyRouteShortName(""), "请输入短名称");
  assert.equal(shortNames.validateThirdPartyRouteShortName("中转"), "");
  assert.equal(
    shortNames.validateThirdPartyRouteShortName("中转线"),
    "短名称最多 2 个字符",
  );
  assert.equal(
    shortNames.validateThirdPartyRouteShortName("官"),
    "“官”仅供官方账号使用",
  );
  assert.equal(
    shortNames.validateThirdPartyRouteShortName(
      "中转",
      [{ id: "existing", authMode: "apiKey", officialAccount: false, shortName: "中转" }],
      "draft",
    ),
    "短名称“中转”已被其他线路使用",
  );
});

test("model labels use the fixed official prefix or a route short name", () => {
  const official = {
    authMode: "officialAccount",
    officialAccount: true,
    name: "OpenAI 官方直登",
    shortName: "ignored",
  };
  const relay = {
    authMode: "apiKey",
    officialAccount: false,
    name: "备用中转线路",
    shortName: "备",
  };

  assert.equal(shortNames.prefixedRouteModelName(official, "gpt-5.6-sol"), "[官] gpt-5.6-sol");
  assert.equal(shortNames.prefixedRouteModelName(relay, "claude-opus"), "[备] claude-opus");
  assert.equal(shortNames.fallbackRouteShortName(" 备用中转 "), "备用");
});

test("the third-party route editor exposes the short-name field and validation hint", async () => {
  const source = await readFile(
    new URL("../src/ModelSection.tsx", import.meta.url),
    "utf8",
  );

  assert.match(source, /id="route-short-name-input"/);
  assert.match(source, /maxLength=\{MAX_ROUTE_SHORT_NAME_CHARACTERS\}/);
  assert.match(source, /最多 2 个字符且不可重复，模型名称前会显示为 \[短名称\]/);
  assert.match(
    source,
    /validateThirdPartyRouteShortName\(route\.shortName, profiles, route\.id\)/,
  );
});
