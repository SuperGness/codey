import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const source = new URL("../src/remote/", import.meta.url);
const { groupThreads, projectForThread, modelOptions, relativeTime, messageUrl, imageUrl } = await loadTypeScriptModule(new URL("workspace-data.ts", source));
const projects = [
  { id: "codey", name: "Codey", cwd: "E:\\code\\codey", rootPaths: ["E:\\code\\codey", "E:/linked/worktree"] },
  { id: "nested", name: "Nested", cwd: "e:/code/codey/packages/app" },
  { id: "unix", name: "Unix", cwd: "/work/App" },
  { id: "empty", name: "Empty", cwd: "E:/empty" },
];
const thread = (id, cwd, updatedAt = 1) => ({ id, cwd, title: id, updatedAt });

test("project grouping normalizes Windows paths, supports secondary roots and prefers the deepest project", () => {
  for (const cwd of ["e:/CODE/codey/", "E:\\code\\codey\\src", "E:/linked/worktree/src", "\\\\?\\E:\\code\\codey", "//?/E:/code/codey/src"]) assert.equal(projectForThread(thread("t", cwd), projects)?.id, "codey");
  assert.equal(projectForThread(thread("t", "E:/code/codey/packages/app/src"), projects)?.id, "nested");
  for (const cwd of ["E:/code/codey-other", "/work/app", ""]) assert.equal(projectForThread(thread("t", cwd), projects), undefined);
  assert.equal(projectForThread(thread("t", "/work/App/subdir"), projects)?.id, "unix");
});

test("project grouping matches extended UNC paths on either side without merging sibling directories", () => {
  const shares = [{ id: "share", cwd: "\\\\server\\share\\project" }];
  assert.equal(projectForThread(thread("t", "\\\\?\\UNC\\SERVER\\share\\project\\src"), shares)?.id, "share");
  assert.equal(projectForThread(thread("t", "//server/share/project-other"), shares), undefined);
  assert.equal(projectForThread(thread("t", "E:/code/codey"), [{ id: "p", cwd: "\\\\?\\E:\\code\\codey" }])?.id, "p");
});

test("groups preserve project order and empty projects without duplicating recent threads", () => {
  const threads = [thread("a", "E:/code/codey", 2), thread("b", "E:/code/codey", 3), thread("c", "E:/other", 4)];
  const grouped = groupThreads(threads, projects);
  assert.deepEqual(grouped.groups.map(group => group.project.id), projects.map(project => project.id));
  assert.deepEqual(grouped.groups[0].threads.map(thread => thread.id), ["b", "a"]);
  assert.equal(grouped.groups[3].threads.length, 0);
  assert.deepEqual(grouped.recent.map(thread => thread.id), ["c"]);
  assert.equal(threads[0].id, "a");
  assert.deepEqual(groupThreads([], []), { groups: [], recent: [] });
});

test("model choices retain applied order, route identity and supported reasoning levels", () => {
  const result = modelOptions({ models: ["route/a", "route/b", "route/a"], model_metadata: [
    { model: "route/a", model_display_name: "Model A", route_name: "Route", supported_reasoning_efforts: ["low", "high", "high", "invalid"], default_reasoning_effort: "max" },
    { model: "route/b", display_name: "Model B", supported_reasoning_efforts: ["none", "medium"], default_reasoning_effort: "medium" },
  ] });
  assert.deepEqual(result.map(model => model.id), ["route/a", "route/b"]);
  assert.deepEqual(result[0], { id: "route/a", label: "Model A", route: "Route", efforts: ["low", "high"], defaultEffort: "low" });
  assert.equal(result[1].defaultEffort, "medium");
  assert.deepEqual(modelOptions({ models: ["disabled"], clear_models: true }), []);
  assert.deepEqual(modelOptions({}), []);
  assert.deepEqual(modelOptions({ models: ["unknown"] })[0].efforts, []);
});

test("message URLs never turn filesystem paths, executable protocols or SVG data into browser requests", () => {
  for (const url of ["javascript:alert(1)", "data:text/html,hello", "file:///C:/secret.txt", "C:\\secret.txt", "/remote/logout", "//example.test/image"]) {
    assert.equal(messageUrl(url), ""); assert.equal(imageUrl(url), "");
  }
  assert.equal(messageUrl("https://example.test/docs"), "https://example.test/docs");
  assert.equal(messageUrl("mailto:hello@example.test"), "mailto:hello@example.test");
  assert.equal(imageUrl("data:image/png;base64,YQ=="), "data:image/png;base64,YQ==");
  assert.equal(imageUrl("data:image/svg+xml;base64,YQ=="), "");
});

test("relative timestamps handle recent, future and absent values", () => {
  const now = 2_000_000_000;
  assert.equal(relativeTime(now + 1000, now), "刚刚");
  assert.equal(relativeTime(now - 180_000, now), "3 分");
  assert.equal(relativeTime(now - 7_200_000, now), "2 小时");
  assert.equal(relativeTime(now - 172_800_000, now), "2 天");
  assert.equal(relativeTime(0, now), ""); assert.equal(relativeTime(NaN, now), "");
});

function compile(name) {
  const compiled = ts.transpileModule(readFileSync(new URL(name, source), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.ESNext, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
  }).outputText.replace(/from "([^"]+)"/g, (_match, specifier) => `from "${specifier.startsWith("./") ? compile(`${specifier.slice(2)}.ts`) : import.meta.resolve(specifier)}"`);
  return `data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`;
}
const { RichText, ConversationTurn } = await import(compile("messages.tsx"));
const { Composer } = await import(compile("composer.tsx"));
const render = (component, props) => renderToStaticMarkup(React.createElement(component, props));

test("chat renders GFM, code highlighting and links while escaping untrusted markup", () => {
  const html = render(RichText, { text: "**完成**\n\n- 第一项\n- [x] 第二项\n\n| 项目 | 状态 |\n| --- | --- |\n| codey | 完成 |\n\n```js\nconst value = 1;\n```\n\n[文档](https://example.test) [文件](/private/file.ts)\n\n<script>alert(1)</script>\n\n[恶意](javascript:alert)" });
  for (const pattern of [/<strong>完成<\/strong>/, /<ul\b/, /type="checkbox"/, /<table>/, /hljs-keyword/, /aria-label="复制代码"/, /rel="noopener noreferrer"/]) assert.match(html, pattern);
  for (const pattern of [/<script/, /href="javascript:/, /href="\/private/]) assert.doesNotMatch(html, pattern);
});

test("conversation retains user messages and folds tool output with a readable duration", () => {
  const html = render(ConversationTurn, { turn: { id: "turn", status: "completed", startedAt: 1000, completedAt: 66000, messages: [
    { role: "user", kind: "userMessage", text: "查看项目" },
    { role: "activity", kind: "commandExecution", text: "pnpm check" },
    { role: "assistant", kind: "agentMessage", text: "检查**通过**" },
  ] } });
  assert.match(html, /aria-label="你"/); assert.match(html, /用时 1 分 5 秒/);
  assert.match(html, /<details class="remote-work-log">/); assert.doesNotMatch(html, /<details[^>]+open/);
  assert.match(html, /检查<strong>通过<\/strong>/);
});

test("composer exposes native selectors and does not offer unsupported effort values", () => {
  const html = render(Composer, { draft: "", onDraft() {}, view: { model: "route/model", effort: "high", permissionMode: "auto", turns: [] }, models: [{ id: "route/model", label: "My model", route: "My route", efforts: ["low", "high"], defaultEffort: "low" }], connected: true, busy: false, uncertain: false, onSend() {}, onStop() {}, onSettings() {}, catalogError: "", onReloadModels() {} });
  assert.match(html, /aria-label="权限"/); assert.match(html, /aria-label="模型"/); assert.match(html, /aria-label="思考程度"/);
  assert.match(html, /<optgroup label="My route">/); assert.match(html, /value="high" selected=""/);
  assert.doesNotMatch(html, /value="ultra"/); assert.match(html, /aria-label="发送消息" disabled=""/);
});
