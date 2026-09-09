import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire, Module } from "node:module";
import { fileURLToPath } from "node:url";
import test from "node:test";
import ts from "typescript";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

const root = new URL("../", import.meta.url);
const filename = fileURLToPath(new URL("src/components/antd/index.tsx", root));
const compiled = ts.transpileModule(readFileSync(filename, "utf8"), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, target: ts.ScriptTarget.ES2020 },
}).outputText;
const module = new Module(filename);
module.filename = filename;
module.paths = createRequire(filename).resolve.paths("react");
module._compile(`${compiled}\nmodule.exports.readDialogTitle = dialogTitle;`, filename);
const { Button, Input, PasswordInput, Checkbox, Switch, DialogHeader, DialogTitle, readDialogTitle } = module.exports;
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));

test("dialogs retain an accessible title when their header is nested in a layout", () => {
  const header = React.createElement(DialogHeader, null, React.createElement(DialogTitle, null, "通知渠道"));
  assert.equal(readDialogTitle([null, React.createElement("div", null, header)]), "通知渠道");
  assert.equal(readDialogTitle(React.createElement("div", null, "正文")), null);
});

test("Ant Design controls preserve native input values, labels and disabled states", () => {
  assert.match(render(Input, { value: 0, "aria-label": "数量", error: true }), /value="0"/);
  assert.match(render(Input, { error: true }), /ant-input-status-error/);
  assert.match(render(PasswordInput, { value: "secret", visibility: false }), /type="password"/);
  assert.match(render(PasswordInput, { value: "secret", visibility: true }), /type="text"/);
  assert.match(render(Checkbox, { checked: true, label: "启用" }), /checked=""/);
  assert.match(render(Checkbox, { checked: "indeterminate" }), /ant-checkbox-indeterminate/);
  assert.match(render(Switch, { checked: true, loading: true }), /disabled=""/);
  assert.match(render(Button, { variant: "destructive", type: "submit", children: "删除" }), /type="submit"/);
  assert.match(render(Button, { variant: "destructive", children: "删除" }), /ant-btn-color-dangerous/);
  assert.match(render(Button, { color: "primary", variant: "filled", children: "查看请求日志" }), /ant-btn-color-primary/);
  assert.match(render(Button, { color: "primary", variant: "filled", children: "查看请求日志" }), /ant-btn-variant-filled/);
});

test("frontend uses a single component library and keeps popups inside the overlay", () => {
  const retired = /@(?:mantine|douyinfe|arco-design)\/|\.mantine-|--mantine-|\.semi-|--semi-|arco-/i;
  const walk = (url) => readdirSync(url, { withFileTypes: true }).flatMap((entry) => entry.isDirectory() ? walk(new URL(`${entry.name}/`, url)) : [new URL(entry.name, url)]);
  for (const url of [...walk(new URL("src/", root)), new URL("package.json", root), new URL("pnpm-lock.yaml", root)]) {
    assert.doesNotMatch(readFileSync(url, "utf8"), retired, url.pathname);
  }
  const overlay = readFileSync(new URL("src/overlay.tsx", root), "utf8");
  assert.match(overlay, /<UiProvider container=\{modalContainer\} styleContainer=\{shadow\}/);
  const provider = readFileSync(new URL("src/UiProvider.tsx", root), "utf8");
  assert.match(provider, /<StyleProvider container=\{styleContainer\} layer>/);
  assert.match(provider, /getPopupContainer=\{container \? \(\) => container : undefined\}/);
});
