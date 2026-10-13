import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const panel = await readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8");
const styles = await readFile(new URL("../src/styles.models.css", import.meta.url), "utf8");
const types = await readFile(new URL("../src/App.types.ts", import.meta.url), "utf8");
const mockApi = await readFile(new URL("../src/dev/mockApi.ts", import.meta.url), "utf8");
const store = await readFile(new URL("../backend/src/official_accounts.rs", import.meta.url), "utf8");
const commands = await readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8");
const provider = await readFile(new URL("../backend/src/codex_provider.rs", import.meta.url), "utf8");
const accountCommands = await readFile(
  new URL("../backend/src/commands/official_accounts.rs", import.meta.url),
  "utf8",
);

test("失效账号卡片使用红色背景并显示失效标识", () => {
  assert.match(types, /invalid\?: boolean;/);
  assert.match(types, /invalidReason\?: string;/);
  assert.match(styles, /\.official-account-item\.is-invalid \{[\s\S]*?background: rgba\(255, 59, 48/);
  assert.match(panel, /account\.invalid \? " is-invalid" : ""/);
  assert.match(panel, /<Badge variant="destructive" title=\{account\.invalidReason\}>/);
  assert.match(panel, /账号已失效/);
});

test("仅有效的非默认账号卡片提供切换交互，后端也拒绝失效账号切换", () => {
  assert.match(panel, /const switchable = !account\.isDefault && !account\.invalid;/);
  assert.match(panel, /\.\.\.\(switchable\s*\? \{[\s\S]*?role: "button",[\s\S]*?askSetDefault\(account\);[\s\S]*?onKeyDown:[\s\S]*?askSetDefault\(account\);[\s\S]*?: \{\}\)/);
  const setDefault = accountCommands.slice(
    accountCommands.indexOf("pub(super) async fn set_default_official_account"),
  );
  assert.match(setDefault, /if record\.invalid\(\) \{[\s\S]{0,240}无法设为默认/);
});

test("额度查询确认失效后重读列表，且不再请求官方接口", () => {
  assert.match(panel, /snapshot\.reason === "official_account_invalid"/);
  assert.match(commands, /record\.invalid_reason\(\)[\s\S]{0,240}official_account_invalid/);
  // 只有 OAuth 明确的 invalid_grant 才标记失效，其他错误只记日志。
  assert.match(store, /official_account_invalid_from_body/);
  assert.match(store, /invalid_grant/);
});

test("源码约束：失效账号跳过两类令牌刷新，前端轮询包含失效分支", () => {
  const refreshTokens = accountCommands.match(/async fn refresh_official_account_tokens\([\s\S]*?\n\}/)?.[0];
  assert.ok(refreshTokens, "应存在账号令牌刷新入口");
  // 非默认且有效的账号才轮换 Codex 凭据，失效账号也跳过独立的直连凭据刷新。
  assert.match(
    refreshTokens,
    /if !is_default \{[\s\S]*?if !record\.invalid\(\) \{\s*let expected = record\.clone\(\);\s*let proxy = [^;]+;\s*match refresh_if_stale_cached/,
  );
  assert.match(refreshTokens, /if !record\.invalid\(\) && record\.siwc\.is_some\(\) \{\s*match refresh_siwc_if_stale/);
  const refresh = store.match(/pub async fn refresh_if_stale_cached\([\s\S]*?\n\}/)?.[0];
  assert.ok(refresh, "应存在生产使用的令牌刷新函数");
  assert.match(
    refresh,
    /if record\.invalid\(\) \{\s*return Ok\(false\);\s*\}\s*if !needs_token_refresh\(record\)/,
  );
  assert.ok(refresh.indexOf("if record.invalid()") < refresh.indexOf(".post(OAUTH_TOKEN_URL)"));
  // 前端轮询跳过已标记失效的账号，只在失效状态变化时重算线路。
  assert.match(panel, /if \(account\.invalid\) \{[\s\S]{0,200}invalidUsageSnapshot\(account\)/);
  assert.match(panel, /snapshot\.reason === "official_account_invalid" && !known\?\.invalid/);
  assert.match(panel, /const recovered = Boolean\(known\?\.invalid\) && snapshot\.status === "ok"/);
  assert.match(panel, /if \(becameInvalid \|\| recovered\) \{/);
  assert.match(panel, /account\.invalid \? 1 : 0/);
});

test("预览数据包含失效账号，便于界面检查", () => {
  assert.match(mockApi, /invalid: true, invalidReason:/);
  assert.match(mockApi, /reason: "official_account_invalid"/);
  assert.match(mockApi, /status: "failed", message: "该账号已失效，无法设为默认；请重新添加账号"/);
});

test("失效账号的线路随失效标记一起移除", () => {
  // 派生线路时跳过失效账号，编号仍按全部账号的添加顺序计算。
  assert.match(provider, /records\.retain\(\|record\| !record\.invalid\(\)\)/);
  assert.match(provider, /has_stored_accounts/);
  // 账号还在但凭据全部失效时清掉线路，不再报成需要重新登录。
  assert.match(commands, /stored_accounts_invalid/);
  // 标记失效后立刻重算线路，并且前端能取回最新的配置与模型状态。
  assert.match(commands, /refresh_official_routes_after_invalid_account/);
  assert.match(commands, /"refresh_official_account_routes"(?:\s*\|[\s\S]{0,800})?\s*=>/);
  const refreshRoutes = accountCommands.match(
    /async fn refresh_official_route_after_account_change\([\s\S]*?\n\}/,
  )?.[0];
  assert.ok(refreshRoutes, "应存在账号变更后的线路刷新函数");
  assert.match(refreshRoutes, /prepare_routes_for_current_launch\(state\)\.await/);
  assert.match(refreshRoutes, /hot_reload_runtime_models\(state\)\.await/);
  assert.match(refreshRoutes, /"config": redacted_config\(&config\)/);
  assert.match(refreshRoutes, /"modelState": model_state/);
  assert.match(
    panel,
    /invoke<OfficialAccountsResult>\("refresh_official_account_routes"\)/,
  );
  // 预览派生同样跳过失效账号，界面检查时线路列表与后端一致。
  assert.match(mockApi, /\.filter\(\(account\) => !account\.invalid\)/);
});
