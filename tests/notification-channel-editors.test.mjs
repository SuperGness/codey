import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (relativePath) =>
  readFileSync(new URL(`../${relativePath}`, import.meta.url), "utf8");

const typesSource = read("src/notifications/types.ts");
const registrySource = read("src/notifications/channelRegistry.tsx");
const cardSource = read("src/notifications/NotificationChannelsCard.tsx");
const mockSource = read("src/dev/mockApi.ts");
const styleSource = read("src/styles.features.css");
const ntfyEditorSource = read("src/notifications/NtfyChannelEditor.tsx");
const wechatClawEditorSource = read("src/notifications/WechatClawChannelEditor.tsx");
const wecomEditorSource = read("src/notifications/WecomChannelEditor.tsx");
const webhookEditorSource = read("src/notifications/WebhookChannelEditor.tsx");

test("ntfy is a server plus topic notification channel", () => {
  assert.match(typesSource, /"ntfy"/);
  assert.match(registrySource, /ntfy:\s*\{[\s\S]*?Editor: NtfyChannelEditor/);
  assert.match(registrySource, /displayName: "ntfy"/);
  assert.match(registrySource, /iconClassName: "ntfy"/);
  assert.match(ntfyEditorSource, /https:\/\/ntfy\.sh/);
  assert.match(ntfyEditorSource, /服务器地址/);
  assert.match(ntfyEditorSource, /主题/);
  assert.match(ntfyEditorSource, /访问令牌/);
  assert.match(ntfyEditorSource, /公开主题可留空/);
  assert.match(ntfyEditorSource, /clearBotToken: false/);
  assert.doesNotMatch(ntfyEditorSource, /clearBotToken: true|clearUrl: true/);
  assert.match(styleSource, /\.notification-title span\.ntfy \{/);
  assert.doesNotMatch(cardSource, /ntfy/);
});

test("ntfy participates in the preview fixtures and runtime status count", () => {
  assert.match(
    mockSource,
    /kind: "ntfy" as const[\s\S]*?chatId: "preview-ntfy-topic"/,
  );
  assert.match(
    mockSource,
    /channel\.kind === "ntfy"[\s\S]*?channel\.urlConfigured && Boolean\(channel\.chatId\.trim\(\)\)/,
  );
  assert.match(
    mockSource,
    /channel\?\.kind === "ntfy"[\s\S]*?channel\.url\?\.trim\(\) && channel\.chatId\?\.trim\(\)/,
  );
});

test("WeChat ClawBot is a scan-bound, token-protected notification channel", () => {
  assert.match(typesSource, /"wechatClaw"/);
  assert.match(
    registrySource,
    /wechatClaw:\s*\{[\s\S]*?Editor: WechatClawChannelEditor/,
  );
  assert.match(registrySource, /displayName: "微信 ClawBot"/);
  assert.match(wechatClawEditorSource, /invoke<WechatClawLoginStartResult>\(\s*"start_wechat_claw_login"/);
  assert.match(wechatClawEditorSource, /"poll_wechat_claw_login"/);
  assert.match(wechatClawEditorSource, /window\.setTimeout\(\(\) => void poll\(\), 1_200\)/);
  assert.match(typesSource, /contextTokenConfigured: boolean/);
  assert.match(typesSource, /sessionStatus\?: NotificationChannelSessionStatus/);
  assert.match(wechatClawEditorSource, /status: "wait" \| "scanned" \| "activating"/);
  assert.match(wechatClawEditorSource, /contextTokenConfigured: true/);
  assert.match(wechatClawEditorSource, /sessionStatus: "active"/);
  assert.match(wechatClawEditorSource, /qrCodeImageUrl/);
  assert.doesNotMatch(wechatClawEditorSource, /解除已保存绑定|clearBotToken: true|clearContextToken: true/);
  assert.match(wechatClawEditorSource, /发送一条消息完成激活/);
  assert.match(wechatClawEditorSource, /登录已失效，请重新扫码后保存配置/);
  assert.match(cardSource, /label: "登录失效", variant: "warning"/);
  assert.match(
    registrySource,
    /channel\.contextToken\.trim\(\) \|\| channel\.contextTokenConfigured/,
  );
  assert.match(registrySource, /channel\.sessionStatus !== "expired"/);
  assert.match(
    mockSource,
    /channel\.sessionStatus !== "expired"[\s\S]*?channel\.urlConfigured/,
  );
  assert.doesNotMatch(wechatClawEditorSource, /type="password"/);
  assert.match(
    mockSource,
    /channel\.kind === "telegram" \|\| channel\.kind === "wechatClaw"/,
  );
});

test("enterprise wechat webhook shows its address without a leading icon", () => {
  assert.match(typesSource, /"feishu" \| "wecom" \| "telegram"/);
  assert.match(registrySource, /wecom:\s*\{[\s\S]*?Editor: WecomChannelEditor/);
  assert.match(registrySource, /displayName: "企业微信机器人"/);
  assert.match(webhookEditorSource, /type="text"/);
  assert.doesNotMatch(webhookEditorSource, /IconSend/);
  assert.doesNotMatch(webhookEditorSource, /清除已保存|clearUrl: true/);
  assert.match(wecomEditorSource, /qyapi\.weixin\.qq\.com\/cgi-bin\/webhook\/send\?key=/);
});
