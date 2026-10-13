import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = {
  helper: await readFile(new URL("../src/officialAccountsRequests.ts", import.meta.url), "utf8"),
  panel: await readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8"),
  models: await readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8"),
  logs: await readFile(new URL("../src/RequestLogDialog.tsx", import.meta.url), "utf8"),
  app: await readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
};

test("官方账号列表共用短缓存，变更结果回写缓存", () => {
  assert.match(source.helper, /list_official_accounts/);
  assert.match(source.helper, /const LIST_TTL_MS = 2_000/);
  assert.match(source.helper, /export function rememberOfficialAccounts/);
  assert.match(source.helper, /if \(!force && cache.promise\) return cache.promise/);
});

test("控制台各入口走共享读取，不再各自直接 invoke 列表", () => {
  assert.match(source.panel, /listOfficialAccounts\(\{ force \}\)/);
  assert.match(source.panel, /rememberOfficialAccounts\(result\)/);
  assert.match(source.models, /await listOfficialAccounts\(\)/);
  assert.match(source.logs, /listOfficialAccounts\(\)/);
  assert.match(source.app, /rememberOfficialAccounts\(result\)/);
  assert.doesNotMatch(source.models, /invoke<OfficialAccountsResult>\("list_official_accounts"\)/);
  assert.doesNotMatch(source.logs, /invoke<OfficialAccountsResult>\("list_official_accounts"\)/);
});
