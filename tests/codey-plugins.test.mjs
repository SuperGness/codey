import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { parseCodeyPluginsResult, validatePluginConfigText, pluginConfigBusinessValuesEqual } = await loadTypeScriptModule(
  new URL("../src/codeyPlugins.ts", import.meta.url),
);

test("plugin list validation distinguishes valid empty lists from malformed state", () => {
  const empty = { plugins: [], platform: "linux", arch: "x86_64" };
  assert.equal(parseCodeyPluginsResult(empty), empty);
  const plugin = { id: "demo", name: "Demo", version: "1", status: "stopped", enabled: false, configPath: "/demo/config.json", capabilities: [] };
  assert.equal(parseCodeyPluginsResult({ ...empty, plugins: [plugin] }).plugins[0], plugin);
  for (const value of [null, {}, { plugins: [] }, { ...empty, plugins: [null] }, { ...empty, plugins: [{ ...plugin, enabled: "false" }] }, { ...empty, plugins: [{ ...plugin, capabilities: null }] }, { ...empty, plugins: [plugin, plugin] }]) assert.throws(() => parseCodeyPluginsResult(value));
});

test("file validation checks JSON object and UTF-8 byte limit", () => {
  assert.equal(validatePluginConfigText('{ "unknown": [1, true] }\n'), undefined);
  for (const text of ["", "{", "null", "[]", "1"]) assert.ok(validatePluginConfigText(text));
  assert.ok(validatePluginConfigText(JSON.stringify({ text: "中".repeat(400000) })));
  assert.equal(validatePluginConfigText('{"x":"' + "a".repeat(1024 * 1024 - 8) + '"}'), undefined);
});

test("comments support nested objects and arrays without interpreting description contents", () => {
  const config = {
    _comments: { "": "", _comments: '{"_comments":null} // example', "任意键名[]": "说明" },
    rules: [{ _comments: { state: "模型对应的 state" }, nested: [[{ _comments: {} }]] }],
    _comment: null, _commentsExtra: [1], __comments: false, text: '{"_comments":false}',
  };
  assert.equal(validatePluginConfigText(JSON.stringify(config)), undefined);
  for (const invalid of [null, [], "说明", 1, false]) {
    const result = validatePluginConfigText(JSON.stringify({ rules: [{ _comments: invalid }] }));
    assert.ok(result.includes('$["rules"][0]["_comments"]'), result);
    assert.match(result, /必须是对象/);
  }
  for (const invalid of [null, [], {}, 1, true]) {
    const result = validatePluginConfigText(JSON.stringify({ rules: [{ _comments: { state: invalid } }] }));
    assert.ok(result.includes('$["rules"][0]["_comments"]["state"]'), result);
    assert.match(result, /必须是字符串/);
  }
  assert.match(validatePluginConfigText('{"_comments":{"x":{"secret":"private-value"}}}'), /必须是字符串/);
  assert.ok(!validatePluginConfigText('{"_comments":{"x":{"secret":"private-value"}}}').includes("private-value"));
  assert.match(validatePluginConfigText('{ // comment\n "x": 1 }'), /JSON 格式无效/);
});

test("deep configuration validation avoids recursive traversal", () => {
  const nested = '{"rules":' + '['.repeat(15000) + '{"_comments":{"x":"说明"}}' + ']'.repeat(15000) + '}';
  assert.equal(validatePluginConfigText(nested), undefined);
  assert.equal(pluginConfigBusinessValuesEqual(nested, nested.replace("说明", "新说明")), true);
});

test("plugin package import uses a preview confirmation dialog", async () => {
  const { readFile } = await import("node:fs/promises");
  const [section, dialog] = await Promise.all([
    readFile(new URL("../src/CodeyPluginsSection.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/PluginImportDialog.tsx", import.meta.url), "utf8"),
  ]);
  assert.match(section, /PluginImportDialog/);
  assert.match(section, /select_codey_plugin_package/);
  assert.match(section, /install_codey_plugin/);
  assert.doesNotMatch(section, /本地路径导入/);
  assert.doesNotMatch(section, /收起路径导入/);
  assert.doesNotMatch(section, /填写本地路径导入/);
  assert.doesNotMatch(section, /platform !== "linux"/);
  assert.match(dialog, /选择文件/);
  assert.match(dialog, /确认导入/);
  assert.match(dialog, /确认升级/);
  assert.match(dialog, /检查安装包/);
});

test("plugin cards can open the plugin directory in a file manager", async () => {
  const { readFile } = await import("node:fs/promises");
  const [section, api] = await Promise.all([
    readFile(new URL("../src/CodeyPluginsSection.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/api.ts", import.meta.url), "utf8"),
  ]);
  assert.match(section, /IconFolderOpen/);
  assert.match(section, /open_codey_plugin_directory/);
  assert.match(section, /在文件管理器中打开插件目录/);
  assert.match(section, /pluginId: plugin\.id/);
  assert.doesNotMatch(section, /open_codey_plugin_directory[\s\S]{0,200}pluginDir/);
  assert.match(api, /"open_codey_plugin_directory"/);
});

test("business comparison ignores only annotations and object key order", () => {
  const active = '{"value":1,"rules":[{"model":"gpt6","_comments":{"model":"说明"}}]}';
  const commentsOnly = '{"_comments":{"value":"参数"},"rules":[{"_comments":{"model":"更新说明"},"model":"gpt6"}],"value":1}';
  assert.equal(pluginConfigBusinessValuesEqual(active, commentsOnly), true);
  const changed = commentsOnly.replace('"value":1', '"value":2');
  assert.equal(pluginConfigBusinessValuesEqual(active, changed), false);
  assert.equal(pluginConfigBusinessValuesEqual(active, changed.replace("更新说明", "再次修改说明")), false);
  assert.equal(pluginConfigBusinessValuesEqual(active, commentsOnly.replace('"value":1', '"value":"1"')), false);
  assert.equal(pluginConfigBusinessValuesEqual('{"_comment":1}', '{"_comment":2}'), false);
  assert.equal(pluginConfigBusinessValuesEqual('{"x":"_comments"}', '{"x":"changed"}'), false);
  assert.equal(pluginConfigBusinessValuesEqual('{"x":[1,2]}', '{"x":[2,1]}'), false);
  assert.equal(pluginConfigBusinessValuesEqual('{"x":null}', '{"x":{}}'), false);
  assert.equal(pluginConfigBusinessValuesEqual('{"x":[]}', '{"x":{}}'), false);
});
