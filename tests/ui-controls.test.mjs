import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import ts from "typescript";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

const root = new URL("../", import.meta.url);
// HeroUI 只提供 ESM 入口：把控件层编译成 ESM，并把裸模块名解析为绝对路径后以 data: URL 载入。
const compiled = ts.transpileModule(readFileSync(new URL("src/components/ui/index.tsx", root), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
}).outputText.replace(/from "([^"./][^"]*)"/g, (_match, specifier) => `from "${import.meta.resolve(specifier)}"`);
const { Badge, Button, Input, PasswordInput, Checkbox, Switch } = await import(
  `data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`
);
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));

test("HeroUI controls preserve native input values, labels and disabled states", () => {
  assert.match(render(Input, { value: 0, readOnly: true, "aria-label": "数量", error: true }), /value="0"/);
  assert.match(render(Input, { error: true }), /aria-invalid="true"/);
  assert.match(render(Input, { leftSection: "L", value: "x", readOnly: true }), /data-slot="input-group-prefix"/);
  assert.match(render(PasswordInput, { value: "secret", readOnly: true, visibility: false }), /type="password"/);
  assert.match(render(PasswordInput, { value: "secret", readOnly: true, visibility: true }), /type="text"/);
  assert.match(render(Checkbox, { checked: true, label: "启用" }), /checked=""/);
  assert.match(render(Checkbox, { checked: "indeterminate" }), /data-indeterminate="true"/);
  assert.match(render(Switch, { checked: true, loading: true }), /disabled=""/);
  assert.match(render(Button, { variant: "destructive", type: "submit", children: "删除" }), /type="submit"/);
  assert.match(render(Button, { variant: "destructive", children: "删除" }), /button--danger/);
  assert.match(render(Button, { color: "primary", variant: "filled", children: "查看请求日志" }), /button--secondary/);
  assert.match(render(Button, { loading: true, children: "刷新" }), /disabled=""[\s\S]*spinner/);
  assert.match(render(Button, { variant: "link", color: "danger", children: "删除" }), /text-danger/);
  assert.match(render(Badge, { variant: "success", children: "运行中" }), /chip--success/);
});

test("frontend uses a single component library and keeps popups inside the overlay", () => {
  const retired = /@(?:mantine|douyinfe|arco-design|ant-design)\/|\bantd\b|\.ant-|\.mantine-|--mantine-|\.semi-|--semi-|arco-/i;
  const walk = (url) => readdirSync(url, { withFileTypes: true }).flatMap((entry) => entry.isDirectory() ? walk(new URL(`${entry.name}/`, url)) : [new URL(entry.name, url)]);
  for (const url of [...walk(new URL("src/", root)), new URL("package.json", root), new URL("pnpm-lock.yaml", root)]) {
    assert.doesNotMatch(readFileSync(url, "utf8"), retired, url.pathname);
  }
  const overlay = readFileSync(new URL("src/overlay.tsx", root), "utf8");
  assert.match(overlay, /enableShadowDOM\(\)/);
  assert.match(overlay, /rootElement\.dataset\.theme = "light"/);
  assert.match(overlay, /modalContainer\.dataset\.theme = "light"/);
  assert.match(overlay, /<UiProvider container=\{modalContainer\}>/);
  const provider = readFileSync(new URL("src/UiProvider.tsx", root), "utf8");
  assert.match(provider, /<UNSAFE_PortalProvider getContainer=\{getContainer\}>/);
  assert.match(provider, /<ToastProvider placement="top"/);
  assert.match(provider, /<I18nProvider locale="zh-CN">/);
  const styles = readFileSync(new URL("src/tailwind.css", root), "utf8");
  // HeroUI 样式按组件导入，避免把从未渲染的组件表内联进 overlay；
  // 主题、工具类与变体三个入口必须保留，否则语义色与 focus-ring 失效。
  assert.doesNotMatch(styles, /@import "@heroui\/styles";/);
  assert.match(styles, /@import "@heroui\/styles\/themes\/default" layer\(theme\);/);
  assert.match(styles, /@import "@heroui\/styles\/utilities";/);
  assert.match(styles, /@import "@heroui\/styles\/variants";/);
  for (const component of ["modal", "table", "combo-box", "toast", "checkbox", "switch", "tooltip", "popover"]) {
    assert.match(
      styles,
      new RegExp(`@import "@heroui\\/styles\\/components\\/${component}\\.css" layer\\(components\\);`),
      component,
    );
  }
});
