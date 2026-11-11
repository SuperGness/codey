import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { validatePluginConfigText } = await loadTypeScriptModule(new URL("../src/codeyPlugins.ts", import.meta.url));
const {
  isEditableConfigArray,
  parsePluginConfigDocument: parse,
  validatePluginConfigValue: validate,
  serializePluginConfigDocument: serialize,
} = await loadTypeScriptModule(new URL("../src/pluginConfigDocument.ts", import.meta.url));
const flatten = document => {
  const result = new Map();
  const visit = entry => { result.set(JSON.stringify(entry.path), entry); entry.children.forEach(visit); };
  document.entries.forEach(visit);
  return result;
};
const entryAt = (document, ...path) => flatten(document).get(JSON.stringify(path));
const edits = (...pairs) => new Map(pairs.map(([path, value]) => [JSON.stringify(path), value]));

test("unchanged source preserves CRLF, escaped text, numeric spelling and comments exactly", () => {
  const content = '{\r\n  "_comments": {"x":"不可改", "missing":"保留"},\r\n  "x" : "\\u4e2d\\n\\/", "n":900719925474099312345, "e":1.000e+02\r\n}\r\n';
  const document = parse(content);
  assert.equal(entryAt(document, "x").valueText, "中\n/");
  assert.equal(entryAt(document, "n").valueText, "900719925474099312345");
  assert.equal(entryAt(document, "e").valueText, "1.000e+02");
  assert.equal(serialize(document, new Map()), content);
  assert.equal(serialize(document, edits([["x"], "中\n/"], [["n"], "900719925474099312345"])), content);
  assert.equal(entryAt(document, "_comments"), undefined);
});

test("several replacements use original positions and preserve every other source byte", () => {
  const content = '{ "_comments":{"a":"说明"}, "a":"a", "list":[false, 20, null], "untouched":1e2 }';
  const updated = serialize(parse(content), edits([["a"], '多行\n"\\😀'], [["list", 0], "true"], [["list", 1], "-0.05e+2"], [["list", 2], '"新值"']));
  assert.equal(updated, '{ "_comments":{"a":"说明"}, "a":"多行\\n\\\"\\\\😀", "list":[true, -0.05e+2, "新值"], "untouched":1e2 }');
  assert.equal(validatePluginConfigText(updated), undefined);
  assert.equal(parse(updated).entries[1].children[0].key, "[0]");
});

test("comments follow nearest local field then ancestor paths across arrays", () => {
  const document = parse(JSON.stringify({
    _comments: { stateConfigs: "列表说明", "stateConfigs.model": "祖先模型说明", "stateConfigs.deep.value": "深层说明", missing: "未匹配" },
    stateConfigs: [{ model: "gpt6", deep: { value: 1 } }, { _comments: { model: "本项说明" }, model: "5.5" }],
  }));
  assert.equal(entryAt(document, "stateConfigs").comment, "列表说明");
  assert.equal(entryAt(document, "stateConfigs", 0).comment, undefined);
  assert.equal(entryAt(document, "stateConfigs", 0, "model").comment, "祖先模型说明");
  assert.equal(entryAt(document, "stateConfigs", 1, "model").comment, "本项说明");
  assert.equal(entryAt(document, "stateConfigs", 0, "deep", "value").comment, "深层说明");
  assert.ok([...flatten(document).values()].every(entry => !entry.path.includes("_comments")));
});

test("literal dotted names take precedence and arbitrary names cannot pollute prototypes", () => {
  const content = '{"_comments":{"a.b":"字面字段", "__proto__":"普通键", "constructor":"普通键二", "":"空键"},"a.b":1,"a":{"b":2},"__proto__":false,"constructor":"x","":null}';
  const document = parse(content);
  assert.equal(entryAt(document, "a.b").comment, "字面字段");
  assert.equal(entryAt(document, "a", "b").comment, undefined);
  assert.notEqual(entryAt(document, "a.b").id, entryAt(document, "a", "b").id);
  assert.equal(entryAt(document, "__proto__").comment, "普通键");
  assert.equal(entryAt(document, "constructor").comment, "普通键二");
  assert.equal(entryAt(document, "").comment, "空键");
  assert.equal(Object.prototype.polluted, undefined);
  const updated = serialize(document, edits([["__proto__"], "true"], [[""], "42"]));
  assert.equal(JSON.parse(updated).__proto__, true);
  assert.equal(JSON.parse(updated)[""], 42);
});

test("values keep original types with scalar-only conversion from null", () => {
  const document = parse('{"n":1,"s":"a","b":false,"nil":null,"a":[],"o":{}}');
  const numeric = entryAt(document, "n");
  for (const text of ["0", "-0", "1.25", "-1e-9", "2E+3"]) assert.equal(validate(numeric, text), undefined);
  for (const text of ["", " ", " 1", "1 ", "+1", "01", ".5", "1.", "Infinity", "NaN", "1e999", "false", "{}"])
    assert.ok(validate(numeric, text), text);
  assert.equal(validate(entryAt(document, "s"), "arbitrary\ntext"), undefined);
  for (const text of ["0", "null", "TRUE", " false"]) assert.ok(validate(entryAt(document, "b"), text));
  for (const text of ['"text"', "-2", "true", "false", "null"]) assert.equal(validate(entryAt(document, "nil"), text), undefined);
  for (const text of ["{}", "[]", "invalid", "1e999"]) assert.ok(validate(entryAt(document, "nil"), text));
  assert.equal(validate(entryAt(document, "a"), "[]"), undefined);
  assert.ok(validate(entryAt(document, "o"), "{}"));
});

test("saving rejects invalid types, structures, annotations and nonexistent entries", () => {
  const document = parse('{"_comments":{"n":"说明"},"n":1,"a":[],"o":{}}');
  for (const [path, value] of [[["n"], "false"], [["a"], "{}"], [["o"], "{}"], [["unknown"], "1"], [["_comments", "n"], "改说明"]])
    assert.throws(() => serialize(document, edits([path, value])));
  entryAt(document, "n").kind = "string";
  assert.throws(() => serialize(document, edits([["n"], "bad"])), /有限数字/);
});

test("invalid documents report format, annotations, duplicates and nesting limits", () => {
  for (const content of ["null", "[]", "{", '{"x":1,}', '{"x":1 //注释\n}']) assert.throws(() => parse(content));
  for (const content of ['{"_comments":[]}', '{"a":[{"_comments":{"x":1}}]}']) assert.throws(() => parse(content), /配置注释/);
  for (const content of ['{"a":1,"a":2}', '{"_comments":{"a":"1","a":"2"}}', '{"a":1,"\\u0061":2}']) assert.throws(() => parse(content), /重复/);
  assert.throws(() => parse('{"x":' + '['.repeat(130) + '0' + ']'.repeat(130) + '}'), /嵌套/);
  assert.throws(() => parse('{"x":1e999}'), /有限值/);
});

test("final serialized size is checked using UTF-8 bytes", () => {
  const document = parse('{"text":""}');
  assert.throws(() => serialize(document, edits([["text"], "中".repeat(400000)])), /1 MiB/);
});

test("whole value arrays can add, remove and clear items without changing other source tokens", () => {
  const content = '{\r\n "_comments":{"lengths":"长度说明"}, "lengths" : [292, 300], "empty": [], "n":1e2\r\n}\r\n';
  const document = parse(content);
  const entry = entryAt(document, "lengths");
  assert.equal(isEditableConfigArray(entry), true);
  assert.equal(entry.valueText, "[292, 300]");
  assert.equal(entry.children.length, 2);
  for (const value of ['[292, 300, 400]', '[300]', '[]', '["text",true,null,12,[false,[]]]']) {
    assert.equal(validate(entry, value), undefined);
    assert.equal(serialize(document, edits([["lengths"], value])), content.replace('[292, 300]', value));
  }
  const value = '[900719925474099312345, 1.000e+02, -0]';
  assert.equal(serialize(document, edits([["empty"], value])), content.replace('"empty": []', `"empty": ${value}`));
  assert.equal(serialize(document, edits([["lengths"], entry.valueText])), content);
});

test("value arrays reject invalid JSON, objects, annotations, overflow and non-arrays", () => {
  const document = parse('{"values":[1]}');
  for (const value of ['[', '[1,]', '[NaN]', '[1e999]', '1', 'null', '"[]"', '{}', '[{}]', '[[{"_comments":{"x":"说明"}}]]', '[1],"injected":2']) {
    assert.ok(validate(entryAt(document, "values"), value), value);
    assert.throws(() => serialize(document, edits([["values"], value])), undefined, value);
  }
});

test("nested value arrays retain token precision while object arrays cannot be replaced", () => {
  const content = '{"values":[[9007199254740993],[]],"rules":[{"_comments":{"x":"说明"},"x":1}],"mixed":[0,[{}]]}';
  const document = parse(content);
  assert.equal(isEditableConfigArray(entryAt(document, "values")), true);
  assert.equal(entryAt(document, "values").valueText, '[[9007199254740993],[]]');
  assert.equal(isEditableConfigArray(entryAt(document, "rules")), false);
  assert.equal(isEditableConfigArray(entryAt(document, "mixed")), false);
  entryAt(document, "rules").children = [];
  assert.throws(() => serialize(document, edits([["rules"], '[]'])), /包含对象/);
  assert.equal(serialize(document, edits([["rules", 0, "x"], '2'])), content.replace('"x":1', '"x":2'));
});

test("overlapping parent array and child edits are rejected in either order", () => {
  const document = parse('{"values":[[1],2],"other":false}');
  for (const pairs of [
    [[["values"], '[3]'], [["values", 0, 0], '4']],
    [[["values", 0], '[3,4]'], [["values"], '[]']],
  ]) {
    assert.throws(() => serialize(document, edits(...pairs)), /同时修改/);
    assert.throws(() => serialize(document, edits(...pairs.toReversed())), /同时修改/);
  }
});
