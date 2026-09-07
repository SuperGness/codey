import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

async function moduleUrl(name) {
  const source = await readFile(new URL(`../src/${name}.ts`, import.meta.url), "utf8");
  let code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2020 } }).outputText;
  for (const dependency of [...code.matchAll(/from "\.\/(\w+)"/g)]) {
    code = code.replace(dependency[0], `from "${await moduleUrl(dependency[1])}"`);
  }
  return `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
}

test("disabled routes do not contribute subagent options while legacy routes stay enabled", async () => {
  const { buildSubagentModelOptions } = await import(await moduleUrl("subagentModels"));
  const config = { localRouterEnabled: true, profiles: [{ id: "enabled", name: "Enabled", authMode: "apiKey" }, { id: "disabled", name: "Disabled", authMode: "apiKey", enabled: false }], selectedModelsByProvider: { enabled: ["model"], disabled: ["model"] }, declaredOfficialModelsByProvider: {} };
  const options = buildSubagentModelOptions(config, { officialModels: [], officialModelIds: [] }, true);
  assert.deepEqual(options.map((option) => option.value), ["enabled/model"]);
});

test("preview configuration persists and prunes model context declarations", async () => {
  const source = await readFile(new URL("../src/dev/mockApi.ts", import.meta.url), "utf8");
  assert.match(source, /supports1MContextByProvider: \{\}/);
  assert.match(source, /id: "primary",\s+enabled: true/);
  assert.match(source, /args.supports1MContextModels/);
  assert.match(source, /available1MModels/);
  assert.match(source, /线路已禁用，不能同步模型/);
});
