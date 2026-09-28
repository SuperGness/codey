import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const panel = await readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8");
const models = await readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8");
const layout = await readFile(new URL("../src/SettingsLayout.tsx", import.meta.url), "utf8");
const app = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");
const pages = await readFile(new URL("../src/SettingsPages.tsx", import.meta.url), "utf8");

test("设置页只在首次打开对应菜单后挂载模块", () => {
  assert.match(layout, /const \[visitedPages, setVisitedPages\]/);
  assert.match(layout, /visitedPages\.has\(page\.id\) && \(/);
});

test("线路与模型菜单把激活状态透传到官方账号面板", () => {
  assert.match(app, /models: \(active\) => \(/);
  assert.match(app, /<ModelSection\s+active=\{active\}/);
  assert.match(pages, /models: \(active: boolean\) => \(/);
  assert.match(pages, /<ModelSection\s+active=\{active\}/);
  const section = models.match(/<OfficialAccountsPanel[\s\S]*?\/>/)?.[0];
  assert.ok(section, "应在 ModelSection 内渲染官方账号面板");
  assert.match(section, /active=\{active\}/);
  assert.match(models, /active: boolean;/);
});

test("官方额度只在菜单打开时获取，关闭后停止", () => {
  assert.match(panel, /active: boolean;/);
  const effect = panel.match(
    /useEffect\(\(\) => \{\s*if \(!active\) return;\s*const list = accountsRef\.current;/,
  );
  assert.ok(effect, "额度查询应在菜单未打开时直接返回");
  assert.match(panel, /\}, \[active, accountListKey, refreshUsage\]\);/);
  // 账号失效等其它入口仍保留原行为。
  assert.match(panel, /await refreshUsage\(account\.id, false\)/);
  assert.match(panel, /onClick=\{\(\) => void refreshUsage\(account\.id, true\)\}/);
});
