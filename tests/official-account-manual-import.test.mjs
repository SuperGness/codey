import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const panel = await readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8");
const api = await readFile(new URL("../src/api.ts", import.meta.url), "utf8");
const mockApi = await readFile(new URL("../src/dev/mockApi.ts", import.meta.url), "utf8");
const store = await readFile(new URL("../backend/src/official_accounts.rs", import.meta.url), "utf8");
const commands = await readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8");
const accountCommands = await readFile(
  new URL("../backend/src/commands/official_accounts.rs", import.meta.url),
  "utf8",
);

test("添加官方账号可以手动粘贴 Refresh Token 或 OAuth JSON", () => {
  assert.match(panel, /type="file"/);
  assert.match(panel, /accept="\.json,application\/json"/);
  assert.match(panel, /const content = await file\.text\(\)/);
  assert.match(panel, /const request = \+\+manualFileReadSequenceRef\.current/);
  assert.match(panel, /setManualInput\(content\)/);
  assert.match(panel, /invoke<OfficialAccountsResult>\("import_official_account_credential", \{ credential \}\)/);
  assert.match(panel, /MANUAL_CREDENTIAL_MAX_BYTES = 64 \* 1024/);
  assert.match(api, /"import_official_account_credential"/);
  assert.match(commands, /"import_official_account_credential"/);
  assert.match(accountCommands, /import_official_account_credential/);
  assert.match(mockApi, /command === "import_official_account_credential"/);
  assert.match(store, /pub async fn official_account_from_manual_input/);
  assert.match(store, /ManualOfficialCredential::Record/);
  assert.match(store, /exchange_refresh_token/);
  // 没有 access token 时才换票；已有 access token 的 JSON 直接保存。
  const parser = store.slice(
    store.indexOf("fn credential_from_oauth_json"),
    store.indexOf("fn oauth_json_source"),
  );
  assert.match(parser, /let Some\(access_token\) = access_token else \{[\s\S]*credential_from_refresh_token/);
  assert.ok(parser.indexOf("credential_from_refresh_token") < parser.indexOf("ManualOfficialCredential::Record"));
});

test("官方账号可以导出服务端命名的 JSON 账号文件", () => {
  assert.match(api, /"export_official_account_credential"/);
  assert.match(commands, /"export_official_account_credential"/);
  assert.match(accountCommands, /account_id: String/);
  assert.match(mockApi, /command === "export_official_account_credential"/);
  assert.match(panel, /invoke<OfficialAccountCredentialExport>/);
  assert.match(panel, /downloadResource\(result\.fileName, result\.content/);
  assert.match(panel, /请妥善保管，不要分享给他人/);
});
