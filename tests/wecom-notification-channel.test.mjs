import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const typesSource = readFileSync(
  new URL("../src/notifications/types.ts", import.meta.url),
  "utf8",
);
const registrySource = readFileSync(
  new URL("../src/notifications/channelRegistry.tsx", import.meta.url),
  "utf8",
);
const editorSource = readFileSync(
  new URL("../src/notifications/WecomChannelEditor.tsx", import.meta.url),
  "utf8",
);
const sharedEditorSource = readFileSync(
  new URL("../src/notifications/WebhookChannelEditor.tsx", import.meta.url),
  "utf8",
);

test("enterprise wechat webhook shows its address without a leading icon", () => {
  assert.match(typesSource, /"feishu" \| "wecom" \| "telegram"/);
  assert.match(registrySource, /wecom:\s*\{[\s\S]*?Editor: WecomChannelEditor/);
  assert.match(registrySource, /displayName: "企业微信机器人"/);
  assert.match(sharedEditorSource, /type="text"/);
  assert.doesNotMatch(sharedEditorSource, /IconSend/);
  assert.doesNotMatch(sharedEditorSource, /revealSecrets/);
  assert.match(sharedEditorSource, /clearUrl: true/);
  assert.match(editorSource, /qyapi\.weixin\.qq\.com\/cgi-bin\/webhook\/send\?key=/);
});
