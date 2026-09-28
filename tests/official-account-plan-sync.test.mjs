import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const panel = await readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8");
const quota = await readFile(new URL("../src/quotaEstimate.ts", import.meta.url), "utf8");
const store = await readFile(new URL("../backend/src/official_accounts.rs", import.meta.url), "utf8");
const commands = await readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8");

test("账号记录里的套餐随登录信息刷新，而不是只在添加账号时解析一次", () => {
  // 解析逻辑集中在一处，登录、令牌刷新与 Codex 同步都走它。
  assert.match(store, /pub\(crate\) fn plan_type_from_auth\(auth: &Value\)/);
  assert.match(store, /let plan_type = plan_type_from_auth\(&auth\);/);
  const applyTokenResponse = store.match(
    /fn apply_token_response\([\s\S]*?\n\}/,
  )?.[0];
  assert.ok(applyTokenResponse, "应存在令牌响应应用函数");
  assert.match(applyTokenResponse, /record\.set_plan_type\(plan_type\.as_deref\(\)\)/);
  const syncDefault = store.match(/pub fn sync_default_from_codex_home\([\s\S]*?\n    \}/)?.[0];
  assert.ok(syncDefault, "应存在默认账号同步函数");
  assert.match(syncDefault, /let plan_type = plan_type_from_auth\(&auth\);/);
  assert.match(syncDefault, /record\.set_plan_type\(plan_type\.as_deref\(\)\)/);
});

test("额度接口返回的实时套餐回写到账号记录", () => {
  assert.match(store, /pub fn set_plan_type\(&mut self, plan_type: Option<&str>\) -> bool/);
  assert.match(store, /pub fn update_plan_type\(&self, id: &str, plan_type: Option<&str>\)/);
  // 凭据条件更新时套餐一起写入，否则刷新后的新套餐会被磁盘旧值盖回去。
  assert.match(store, /current\.plan_type = updated\.plan_type\.clone\(\);/);
  const query = commands.slice(
    commands.indexOf("async fn query_stored_official_account_usage"),
  );
  assert.match(query, /snapshot\.get\("planType"\)/);
  assert.match(query, /update_plan_type\(&plan_id, Some\(&plan_type\)\)/);
  // 只有自己打开的线路菜单会写回套餐：页头的定时读取不碰账号记录。
  assert.match(query, /if write_back_plan\s*&& snapshot\.get\("status"\)/);
  const entry = commands.slice(
    commands.indexOf("async fn query_official_account_usage"),
    commands.indexOf("async fn header_official_account_id"),
  );
  assert.match(entry, /query_stored_official_account_usage\(state, force_refresh, account_id, true\)/);
  assert.match(entry, /query_stored_official_account_usage\(state, force_refresh, account_id, false\)/);
});

test("账号卡片优先显示额度接口的当前套餐", () => {
  assert.match(quota, /planType\?: string;/);
  assert.match(panel, /const snapshotPlan = usages\[account\.id\]\?\.status === "ok"/);
  assert.match(panel, /const planType = snapshotPlan \|\| account\.planType;/);
  assert.match(panel, /const plan = formatPlan\(planType\);/);
  assert.match(panel, /planTagClass\(planType\)/);
  assert.match(panel, /planType\?\.toLowerCase\(\) === "pro"/);
  assert.doesNotMatch(panel, /planTagClass\(account\.planType\)/);
});
