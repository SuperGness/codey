import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("../src/remoteCompactionStatus.ts", import.meta.url), "utf8");
const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { remoteCompactionStatusText, getRouteCompactionStatus } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);

test("saving remote compaction reports gateway handling without claiming activation", () => {
  const local = remoteCompactionStatusText({ running: true, remoteCompaction: {
    configured: true, active: false, restartRequired: true, blockingRoutes: [],
  } });
  assert.equal(local.title, "当前由 Codey 网关生成压缩摘要");
  assert.match(local.detail, /已保存配置声明了原生远程压缩.*压缩方式随所选线路自动生效/);
  assert.doesNotMatch(local.detail, /重启|新建任务/);
  const remote = remoteCompactionStatusText({ running: true, remoteCompaction: {
    configured: false, active: true, restartRequired: true, blockingRoutes: [],
  } });
  assert.equal(remote.title, "当前已启用远程压缩");
  assert.match(remote.detail, /已保存配置未声明原生远程压缩，由网关生成摘要.*压缩方式随所选线路自动生效/);
  assert.doesNotMatch(remote.detail, /重启|新建任务/);
});

test("missing status, stopped Codex and unmanaged compression remain distinct", () => {
  assert.equal(remoteCompactionStatusText({ running: true }).title, "压缩方式待确认");
  const remoteCompaction = { configured: true, active: null, restartRequired: false, blockingRoutes: [] };
  assert.equal(remoteCompactionStatusText({ running: false, remoteCompaction }).title, "Codex 未运行");
  assert.equal(remoteCompactionStatusText({ running: true, remoteCompaction }).title, "压缩由 Codex 自行管理");
  assert.equal(remoteCompactionStatusText(undefined).title, "压缩方式待确认");
});

test("mixed routes advertise compaction that follows the selected route automatically", () => {
  const status = { running: true, remoteCompaction: {
    configured: true, active: true, configuredMode: "mixed", activeMode: "mixed",
    restartRequired: true, blockingRoutes: [{ routeName: "Chat", reason: "上游协议不支持原生压缩" }],
  } };
  const text = remoteCompactionStatusText(status);
  assert.equal(text.title, "当前按线路选择压缩方式");
  assert.match(text.detail, /原生支持的线路透传上游压缩.*其余线路由网关生成摘要/);
  assert.match(text.detail, /压缩方式随所选线路自动生效/);
  assert.doesNotMatch(text.detail, /重启|新建任务/);
});

test("getRouteCompactionStatus correctly identifies official, remote, and local routes", () => {
  const officialRoute = {
    id: "route-official",
    name: "plus",
    authMode: "officialAccount",
    officialAccount: true,
  };
  const offStatus = getRouteCompactionStatus(officialRoute, { running: true });
  assert.equal(offStatus.mode, "remote");
  assert.equal(offStatus.badgeText, "远程压缩");
  assert.equal(offStatus.badgeVariant, "success");
  assert.match(offStatus.tooltip, /官方账号原生支持远程压缩/);

  const unavailableOfficial = getRouteCompactionStatus(officialRoute, { running: true }, false);
  assert.equal(unavailableOfficial.mode, "local");
  assert.equal(unavailableOfficial.badgeText, "本地压缩");
  assert.equal(unavailableOfficial.badgeVariant, "secondary");
  assert.equal(unavailableOfficial.reason, "官方账号未就绪");

  const customRemoteRoute = {
    id: "route-custom-remote",
    name: "自建-开启",
    authMode: "apiKey",
    upstreamProtocol: "openaiResponses",
    supportsRemoteCompaction: true,
    remoteCompactionProtocol: "compactEndpoint",
  };
  const customRemoteStatus = getRouteCompactionStatus(customRemoteRoute, { running: true });
  assert.equal(customRemoteStatus.mode, "remote");
  assert.equal(customRemoteStatus.badgeText, "远程压缩");
  assert.equal(customRemoteStatus.badgeVariant, "success");
  assert.match(customRemoteStatus.tooltip, /独立压缩接口/);

  const customDisabledRoute = {
    id: "route-custom-disabled",
    name: "自建-未开启",
    authMode: "apiKey",
    upstreamProtocol: "openaiResponses",
    supportsRemoteCompaction: false,
  };
  const customDisabledStatus = getRouteCompactionStatus(customDisabledRoute, { running: true });
  assert.equal(customDisabledStatus.mode, "remote");
  assert.equal(customDisabledStatus.badgeText, "网关压缩");
  assert.equal(customDisabledStatus.badgeVariant, "success");
  assert.match(customDisabledStatus.tooltip, /网关生成等价摘要/);
  assert.equal(customDisabledStatus.reason, "未开启远程压缩");

  const chatRoute = {
    id: "route-chat",
    name: "obu-gpt",
    authMode: "apiKey",
    upstreamProtocol: "openaiChatCompletions",
    supportsRemoteCompaction: true,
  };
  const chatStatus = getRouteCompactionStatus(chatRoute, { running: true });
  assert.equal(chatStatus.mode, "remote");
  assert.equal(chatStatus.badgeText, "网关压缩");
  assert.equal(chatStatus.badgeVariant, "success");
  assert.match(chatStatus.tooltip, /网关生成等价摘要/);
  assert.equal(chatStatus.reason, "上游协议不支持原生压缩");

  const runtimeBlockedRoute = {
    id: "route-wa",
    name: "wa",
    authMode: "apiKey",
    upstreamProtocol: "openaiResponses",
    supportsRemoteCompaction: false,
  };
  const runtimeStatus = {
    running: true,
    remoteCompaction: {
      configured: true,
      active: true,
      restartRequired: true,
      blockingRoutes: [{ routeId: "route-wa", routeName: "wa", reason: "未开启远程压缩" }],
    },
  };
  const waStatus = getRouteCompactionStatus(runtimeBlockedRoute, runtimeStatus);
  assert.equal(waStatus.mode, "remote");
  assert.equal(waStatus.badgeText, "网关压缩");
  assert.match(waStatus.tooltip, /网关生成等价摘要/);
  assert.equal(waStatus.reason, "未开启远程压缩");

  const restartRouteStatus = getRouteCompactionStatus(customRemoteRoute, runtimeStatus);
  assert.equal(restartRouteStatus.mode, "remote");
  assert.match(restartRouteStatus.tooltip, /压缩方式随所选线路自动生效/);
  assert.doesNotMatch(restartRouteStatus.tooltip, /重启|新建任务/);
  const officialChangedStatus = getRouteCompactionStatus(officialRoute, runtimeStatus);
  assert.match(officialChangedStatus.tooltip, /压缩方式随所选线路自动生效/);
  assert.doesNotMatch(officialChangedStatus.tooltip, /重启|新建任务/);
});
