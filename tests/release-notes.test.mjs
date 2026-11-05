import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

const source = await readFile(new URL("../src/releaseNotes.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const exports = {};
new Function("exports", compiled)(exports);
const { parseReleaseNotes, groupReleaseNotes, releaseVersionLabel } = exports;

test("跨版本标题与旧分类同时保留，不把版本标题当作条目", () => {
  const groups = parseReleaseNotes("## 1.2.7\n\n**新增功能**\n- 新功能\n\n## 1.2.6\n**问题修复**\n- 修复启动");
  assert.deepEqual(groups.map(group => group.title), ["1.2.7", "新增功能", "1.2.6", "问题修复"]);
  assert.deepEqual(groups.map(group => group.lines), [[], ["新功能"], [], ["修复启动"]]);
  assert.deepEqual(groups.map(group => group.level), [2, 3, 2, 3]);
});

test("支持六级标题、Tab 和合法的尾部井号，保留正文中的井号", () => {
  for (let level = 1; level <= 6; level++) {
    assert.deepEqual(parseReleaseNotes(`${"#".repeat(level)}\t标题 ###\r\n- C#`), [
      { title: "标题", level, lines: ["C#"] },
    ]);
  }
  assert.equal(parseReleaseNotes("## C#")[0].title, "C#");
  assert.equal(parseReleaseNotes("## **新增功能**")[0].title, "新增功能");
  assert.deepEqual(parseReleaseNotes("##无空格\n#tag\n####### 七级"), [
    { title: "", level: 0, lines: ["##无空格", "#tag", "####### 七级"] },
  ]);
});

test("保留旧日志和无标题条目的展示，忽略空内容", () => {
  assert.deepEqual(parseReleaseNotes("说明\n- 修复\n* 优化\n• 新功能"), [
    { title: "", level: 0, lines: ["说明", "修复", "优化", "新功能"] },
  ]);
  assert.deepEqual(parseReleaseNotes("**新增功能**\n- 支持切换"), [
    { title: "新增功能", level: 1, lines: ["支持切换"] },
  ]);
  assert.deepEqual(parseReleaseNotes("## ###\n\n"), []);
  assert.deepEqual(parseReleaseNotes(), []);
});

test("按版本分段，分类归入所属版本，版本下的无标题条目保留", () => {
  const sections = groupReleaseNotes(parseReleaseNotes(
    "## 1.5.1\n\n### 新增功能\n- 导入\n\n**问题修复**\n- 修复\n\n## 1.3.9\n修复统计",
  ));
  assert.deepEqual(sections, [
    { version: "1.5.1", categories: [{ title: "新增功能", lines: ["导入"] }, { title: "问题修复", lines: ["修复"] }] },
    { version: "1.3.9", categories: [{ title: "", lines: ["修复统计"] }] },
  ]);
  assert.deepEqual(groupReleaseNotes(parseReleaseNotes("**体验优化**\n- 更快\n说明")), [
    { version: "", categories: [{ title: "体验优化", lines: ["更快", "说明"] }] },
  ]);
  assert.deepEqual(groupReleaseNotes([]), []);
  assert.equal(releaseVersionLabel("1.5.1"), "v1.5.1");
  assert.equal(releaseVersionLabel("v1.5.1"), "v1.5.1");
});
