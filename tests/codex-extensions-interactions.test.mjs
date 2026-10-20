import assert from "node:assert/strict";
import test from "node:test";
import { autoStubModule, collectElements, createModuleGraph, elementProps, elementType, textContent } from "./helpers/jsx-tree.mjs";

const ui = autoStubModule("ui");
const stubs = { "../../components/ui": ui, "@tabler/icons-react": autoStubModule("icons") };
const matching = (tree, predicate) => collectElements(tree, predicate);
const button = (tree, text) => matching(tree, (node) => elementType(node) === ui.Button && textContent(node) === text)[0];
const source = { name: "demo", description: "演示技能", sourcePath: "/tmp/skill", sourceKind: "directory", sourceRevision: "source-1", fileCount: 201, totalBytes: 2010, files: Array.from({ length: 100 }, (_, i) => ({ path: `file-${i}`, size: 10, executable: false })) };

test("列表只有目标服务显示检查中，其他操作仍由全局锁保护", () => {
  const graph = createModuleGraph(new URL("../src/features/codex-extensions/ExtensionList.tsx", import.meta.url), { stubs });
  const entries = ["first", "second"].map((id) => ({ id, name: id, sourcePath: "/config", enabled: true, readOnly: false }));
  const tree = graph.exports.ExtensionList({ kind: "mcp", entries, busy: true, busyAction: "test_mcp", busyResourceId: "first", onAction() {} });
  const cards = matching(tree, (node) => elementType(node) === "article");
  assert.match(textContent(cards[0]), /检查中/);
  assert.doesNotMatch(textContent(cards[1]), /检查中/);
  assert.equal(elementProps(button(cards[1], "测试连接")).disabled, true);
});

test("安装预览文件列表有界，来源可在预览期间编辑且最终操作含明确确认", () => {
  const graph = createModuleGraph(new URL("../src/features/codex-extensions/ExtensionEditor.tsx", import.meta.url), { stubs });
  const draft = { kind: "install", content: "/tmp/skill", isNew: true, installPreview: source };
  const tree = graph.exports.ExtensionEditor({ draft, busy: false });
  assert.equal(matching(tree, (node) => elementType(node) === "li").length, 100);
  assert.match(textContent(tree), /仅显示前 100 项/);
  assert.ok(button(tree, "确认安装（默认禁用）"));
  graph.restart();
  const waiting = graph.exports.ExtensionEditor({ draft: { ...draft, installPreview: undefined }, busy: true, previewing: true });
  const input = matching(waiting, (node) => elementProps(node)["aria-label"] === "Skill 来源绝对路径")[0];
  assert.equal(elementProps(input).disabled, false);
  assert.equal(elementProps(button(waiting, "正在预览…")).disabled, true);
});

function pageHarness() {
  const pending = [];
  const mutations = [];
  const controller = {
    inventory: { revision: "r1", mcps: [], skills: [], warnings: [] }, scope: { kind: "user" },
    busy: false, loading: false, checks: {}, clearError() {},
    run(action, success, isCurrent) { return new Promise((resolve) => pending.push({ action, isCurrent, finish(result) { success(result); resolve("ok"); } })); },
    async mutate(action) { mutations.push(action); return "ok"; },
  };
  const editor = autoStubModule("editor");
  const graph = createModuleGraph(new URL("../src/features/codex-extensions/CodexExtensionsPage.tsx", import.meta.url), {
    autoStub: true,
    stubs: { ...stubs, "./useExtensionsController": { useExtensionsController: () => controller }, "./ExtensionEditor": editor },
  });
  // 此交互需要按 React 语义跨渲染保留 ref，默认树桩只创建临时 ref。
  graph.react.useRef = (initial) => graph.react.useState(() => ({ current: initial }))[0];
  const render = () => { graph.restart(); return graph.exports.CodexExtensionsPage({ kind: "skill", request() {} }); };
  const draftEditor = () => elementProps(matching(render(), (node) => elementType(node) === editor.ExtensionEditor)[0]);
  const header = matching(render(), (node) => Boolean(elementProps(node).actions))[0];
  elementProps(button(elementProps(header).actions, "导入目录 / ZIP")).onClick();
  const initial = draftEditor();
  initial.onChange({ ...initial.draft, content: "/tmp/skill" });
  return { pending, mutations, draftEditor };
}

test("安装首次提交只预览，再次明确提交才携来源版本安装", async () => {
  const h = pageHarness();
  h.draftEditor().onSave();
  assert.equal(h.pending[0].action.action, "preview_skill_install");
  assert.equal(h.mutations.length, 0);
  h.pending[0].finish(source);
  await Promise.resolve();
  assert.equal(h.draftEditor().draft.installPreview.sourceRevision, "source-1");
  h.draftEditor().onSave();
  assert.deepEqual(h.mutations[0], { action: "install_skill", sourcePath: "/tmp/skill", sourceRevision: "source-1", revision: "r1" });
});

test("预览期间更改路径丢弃迟到结果，预览后更改路径撤销确认", async () => {
  const h = pageHarness();
  h.draftEditor().onSave();
  const waiting = h.draftEditor();
  assert.equal(h.pending[0].isCurrent(), true);
  waiting.onChange({ ...waiting.draft, content: "/tmp/other" });
  assert.equal(h.pending[0].isCurrent(), false);
  h.pending[0].finish(source);
  await Promise.resolve();
  assert.equal(h.draftEditor().draft.installPreview, undefined);
  h.draftEditor().onSave();
  h.pending[1].finish({ ...source, sourcePath: "/tmp/other" });
  await Promise.resolve();
  const ready = h.draftEditor();
  assert.ok(ready.draft.installPreview);
  ready.onChange({ ...ready.draft, content: "/tmp/third" });
  assert.equal(h.draftEditor().draft.installPreview, undefined);
  assert.equal(h.mutations.length, 0);
});
