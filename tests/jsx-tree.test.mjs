import assert from "node:assert/strict";
import test from "node:test";
import { createModuleGraph, elementType, textContent } from "./helpers/jsx-tree.mjs";

function headerGraph() {
  return createModuleGraph(new URL("../src/SettingsPageHeader.tsx", import.meta.url), {
    stubs: {
      "react-dom": {
        createPortal() {
          assert.fail("未提供目标时不应创建 Portal");
        },
      },
    },
  });
}

test("React 树替身保留各个 Context 的默认值及其引用", () => {
  const { react, reset } = headerGraph();
  const defaults = [
    undefined, null, false, 0, "default", { value: "default" },
    () => assert.fail("Context 默认函数不应作为初始化函数执行"),
  ];
  const contexts = defaults.map((value) => react.createContext(value));
  reset();
  contexts.forEach((context, index) => {
    assert.strictEqual(react.useContext(context), defaults[index]);
  });
});

test("标题组件在没有 Provider 时按 undefined 默认值渲染标题", () => {
  const graph = headerGraph();
  const tree = graph.exports.SettingsPageHeader({ id: "settings-title", title: "设置" });
  assert.equal(elementType(tree), "header");
  assert.equal(textContent(tree), "设置");
});
