import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";
import {
  autoStubModule,
  collectElements,
  createModuleGraph,
  elementProps,
  elementType,
} from "./helpers/jsx-tree.mjs";

const [
  cardSource,
  policySource,
  stylesSource,
  typesSource,
  registrySource,
  mockSource,
  ntfyEditorSource,
  wechatClawEditorSource,
  wecomEditorSource,
  webhookEditorSource,
  appSource,
  updateSource,
  dialogSource,
] = await Promise.all([
  readSource("src/notifications/NotificationChannelsCard.tsx"),
  readSource("src/FeaturePolicyCard.tsx"),
  readSource("src/styles.features.css"),
  readSource("src/notifications/types.ts"),
  readSource("src/notifications/channelRegistry.tsx"),
  readSource("src/dev/mockApi.ts"),
  readSource("src/notifications/NtfyChannelEditor.tsx"),
  readSource("src/notifications/WechatClawChannelEditor.tsx"),
  readSource("src/notifications/WecomChannelEditor.tsx"),
  readSource("src/notifications/WebhookChannelEditor.tsx"),
  readSource("src/App.tsx"),
  readSource("src/useAppUpdates.ts"),
  readSource("src/notifications/NotificationChannelDialog.tsx"),
]);

const ui = autoStubModule("ui");
const icons = autoStubModule("icon");
const graph = createModuleGraph(
  new URL("../src/notifications/NotificationChannelsCard.tsx", import.meta.url),
  {
    autoStub: true,
    stubs: {
      "@tabler/icons-react": icons,
      "../components/ui": ui,
      "./FeishuChannelEditor": autoStubModule("feishu-editor"),
      "./NotificationChannelDialog": autoStubModule("channel-dialog"),
      "./NtfyChannelEditor": autoStubModule("ntfy-editor"),
      "./TelegramChannelEditor": autoStubModule("telegram-editor"),
      "./WechatClawChannelEditor": autoStubModule("wechat-claw-editor"),
      "./WecomChannelEditor": autoStubModule("wecom-editor"),
    },
  },
);

function channel(overrides = {}) {
  return {
    botToken: "",
    botTokenConfigured: false,
    chatId: "",
    contextToken: "",
    contextTokenConfigured: false,
    enabled: true,
    id: "channel-1",
    kind: "ntfy",
    url: "https://ntfy.example.com",
    urlConfigured: true,
    ...overrides,
  };
}

function renderCard(channels) {
  graph.reset();
  return graph.exports.NotificationChannelsCard({
    config: { webhook: { channels } },
    container: null,
    isBusy: false,
    onAddChannel: async () => true,
    onChannelChange: async () => true,
    onRequestRemoveChannel: () => {},
    popupContainer: null,
  });
}

const cardClasses = (tree) =>
  collectElements(
    tree,
    (element) =>
      typeof elementProps(element).className === "string" &&
      elementProps(element).className.split(" ").includes("notification-card"),
  ).map((element) => elementProps(element).className);

test("渠道列表保留 ul/li 语义与 aria-label", () => {
  const tree = renderCard([channel(), channel({ id: "channel-2", kind: "telegram" })]);
  const lists = collectElements(tree, (element) => elementType(element) === "ul");
  assert.equal(lists.length, 1, "渠道列表必须是真正的列表元素");
  assert.equal(elementProps(lists[0])["aria-label"], "已配置通知渠道");
  const items = collectElements(lists[0], (element) => elementType(element) === "li");
  assert.equal(items.length, 2, "每个渠道占一个列表项");
  assert.equal(cardClasses(tree).length, 2);
  assert.equal(elementProps(lists[0]).className, "notification-channel-list");

  const empty = renderCard([]);
  const emptyLists = collectElements(empty, (element) => elementType(element) === "ul");
  const emptyItems = collectElements(emptyLists[0], (element) => elementType(element) === "li");
  assert.equal(emptyItems.length, 1, "空态也必须放在列表项里");
  assert.match(elementProps(emptyItems[0]).className, /notification-empty-card/);
});

test("启用、停用与登录失效分别带上可区分的状态类", () => {
  const tree = renderCard([
    channel({ id: "enabled", kind: "ntfy" }),
    channel({ enabled: false, id: "disabled", kind: "ntfy" }),
    channel({ id: "expired", kind: "wechatClaw", sessionStatus: "expired" }),
  ]);
  assert.deepEqual(cardClasses(tree), [
    "notification-card codey-card active",
    "notification-card codey-card inactive",
    "notification-card codey-card expired",
  ]);
});

test("样式表定义三条状态规则与列表选择器，且不残留已废弃的网格类", () => {
  for (const state of ["active", "inactive", "expired"])
    assert.match(
      stylesSource,
      new RegExp(`\\.notification-card\\.${state}\\s*\\{`),
      state,
    );
  assert.match(stylesSource, /\.notification-channel-list\s*\{/);
  assert.match(stylesSource, /\.notification-channel-list > li\s*\{/);
  assert.match(stylesSource, /\.notification-channel-list > li > \.notification-card\s*\{/);
  assert.doesNotMatch(stylesSource, /\.notification-section-grid\s*\{/);
  assert.doesNotMatch(cardSource, /notification-section-grid/);
});

test("通知区域的 aria-labelledby 指向真实存在的标题", () => {
  const tree = renderCard([channel()]);
  assert.equal(
    collectElements(tree, (element) => elementProps(element).id === "notification-title")
      .length,
    1,
    "子组件必须渲染出被引用的标题",
  );
  assert.match(policySource, /aria-labelledby="notification-title"/);
  assert.doesNotMatch(policySource, /notification-section-title/);
});

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
  assert.match(stylesSource, /\.notification-title span\.ntfy \{/);
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

test("notification dialog waits for the channel configuration to persist", () => {
  assert.match(dialogSource, /onSave: \(channel: NotificationChannel\) => Promise<boolean>/);
  assert.match(dialogSource, /if \(await onSave\(draft\)\) onOpenChange\(false\)/);
  assert.match(appSource, /async function persistNotificationChannels/);
  assert.match(appSource, /await persist\(\{[\s\S]*?webhook:[\s\S]*?channels/);
});

test("installing an update saves pending settings first", () => {
  assert.match(appSource, /beforeInstall: async \(\) => \{[\s\S]*?await persist\(config\)/);
  assert.match(updateSource, /await beforeInstall\(\);[\s\S]*?install_downloaded_update/);
});
