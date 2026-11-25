import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

const root = new URL("../", import.meta.url);
// HeroUI 只提供 ESM 入口：把控件层编译成 ESM，并把裸模块名解析为绝对路径后以 data: URL 载入。
const compiled = ts.transpileModule(readFileSync(new URL("src/components/ui/index.tsx", root), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
}).outputText.replace(/from "([^"./][^"]*)"/g, (_match, specifier) => `from "${import.meta.resolve(specifier)}"`);
const { Badge, Button, Input, PasswordInput, NumberInput, Checkbox, Switch, TextArea, Label, Select } = await import(
  `data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`
);
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));

test("HeroUI controls preserve native input values, labels and disabled states", () => {
  assert.match(render(Input, { value: 0, readOnly: true, "aria-label": "数量", error: true }), /value="0"/);
  assert.match(render(Input, { error: true }), /aria-invalid="true"/);
  assert.match(render(Input, { leftSection: "L", value: "x", readOnly: true }), /data-slot="input-group-prefix"/);
  assert.match(render(PasswordInput, { value: "secret", readOnly: true, visibility: false }), /type="password"/);
  assert.match(render(PasswordInput, { value: "secret", readOnly: true, visibility: true }), /type="text"/);
  assert.match(render(NumberInput, { value: 4, "aria-label": "会话错误重试次数" }), /value="4"/);
  assert.match(render(NumberInput, { value: 4, "aria-label": "会话错误重试次数" }), /data-slot="number-field"/);
  assert.match(render(NumberInput, { value: 4, disabled: true, "aria-label": "会话错误重试次数" }), /data-disabled="true"/);
  assert.match(render(TextArea, { value: "优化指令", readOnly: true, "aria-label": "优化指令", error: true }), /优化指令/);
  assert.match(render(TextArea, { error: true }), /aria-invalid="true"/);
  assert.match(render(TextArea, { disabled: true }), /data-disabled="true"/);
  assert.match(render(Label, { children: "优化指令", htmlFor: "instruction-id" }), /for="instruction-id"/);
  assert.match(render(Checkbox, { checked: true, label: "启用" }), /checked=""/);
  assert.match(render(Checkbox, { checked: "indeterminate" }), /data-indeterminate="true"/);
  assert.match(render(Switch, { checked: true, loading: true }), /disabled=""/);
  assert.match(render(Button, { variant: "destructive", type: "submit", children: "删除" }), /type="submit"/);
  assert.match(render(Button, { variant: "destructive", children: "删除" }), /button--danger/);
  assert.match(render(Button, { color: "primary", variant: "filled", children: "查看请求日志" }), /button--secondary/);
  assert.match(render(Button, { loading: true, children: "刷新" }), /disabled=""[\s\S]*spinner/);
  assert.match(render(Button, { variant: "link", color: "danger", children: "删除" }), /text-danger/);
  assert.match(render(Badge, { variant: "success", children: "运行中" }), /chip--success/);
  assert.match(render(Select, {
    "aria-label": "协议",
    value: "http",
    optionList: [{ label: "HTTP", value: "http" }],
  }), /HTTP/);
  assert.match(render(Select, {
    "aria-label": "可筛选协议",
    filter: true,
    value: "http",
    optionList: [{ label: "HTTP", value: "http" }],
  }), /role="combobox"/);
});
