import assert from "node:assert/strict";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { parseHeadersText, headersTextFromMap } = await loadTypeScriptModule(
  new URL("../src/requestHeaders.ts", import.meta.url),
);
const parse = (value) => parseHeadersText(JSON.stringify(value));

test("headers normalize names and preserve values and deletion markers", () => {
  assert.deepEqual(parse({ "X-Test": " a\tb ", "X-Delete": null, Empty: "", "X-Unicode": "中文" }), {
    "x-test": " a\tb ", "x-delete": "", empty: "", "x-unicode": "中文",
  });
  assert.deepEqual(JSON.parse(headersTextFromMap({ empty: "", value: "ok" })), { empty: null, value: "ok" });
  assert.equal(headersTextFromMap(undefined), "{}");
  assert.deepEqual(parseHeadersText('{"x-test":"first","x-test":"last"}'), { "x-test": "last" });
  assert.equal(Object.hasOwn(parse({ ["__proto__"]: "safe" }), "__proto__"), true);
});

test("invalid headers produce specific errors without exposing values", () => {
  for (const value of [null, [], true, 1, "text"]) assert.throws(() => parse(value), /JSON 对象/);
  assert.throws(() => parseHeadersText('{"authorization":"sensitive-token",'), (error) => {
    assert.doesNotMatch(error.message, /sensitive-token/);
    return /JSON 对象/.test(error.message);
  });
  assert.throws(() => parse({ "X-Test": "one", "x-test": "two" }), /x-test 重复/);
  for (const name of ["", "x test", "x:test", "中文", "x\r\n"]) assert.throws(() => parse({ [name]: "value" }), /名称/);
  for (const value of [false, 0, [], {}]) assert.throws(() => parse({ "x-test": value }), /字符串或 null/);
  for (const control of ["\0", "\x08", "\n", "\r", "\x1f", "\x7f"]) {
    assert.throws(() => parse({ authorization: `sensitive${control}token` }), (error) => {
      assert.doesNotMatch(error.message, /sensitive|token/);
      return /控制字符/.test(error.message);
    });
  }
  for (const value of [" ", "\t", " \t ", "\u00a0"]) assert.throws(() => parse({ "x-test": value }), /请用 null/);
});

test("header limits count UTF-8 bytes and include names in the total", () => {
  assert.equal(Object.keys(parse(Object.fromEntries(Array.from({ length: 128 }, (_, i) => [`x-${i}`, ""])))).length, 128);
  assert.throws(() => parse(Object.fromEntries(Array.from({ length: 129 }, (_, i) => [`x-${i}`, ""]))), /128/);
  assert.equal(parse({ x: "é".repeat(4096) }).x.length, 4096);
  assert.throws(() => parse({ x: "é".repeat(4097) }), /8 KiB/);
  const total = Object.fromEntries(["a", "b", "c", "d"].map((name) => [name, "a".repeat(8191)]));
  assert.equal(Object.keys(parse(total)).length, 4);
  assert.throws(() => parse({ ...total, e: "" }), /32 KiB/);
});
