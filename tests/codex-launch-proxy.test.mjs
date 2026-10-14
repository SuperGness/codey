import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const root = new URL("../", import.meta.url);

test("codex launch proxy validator matches the launch-time constraints", async () => {
  const { validateCodexLaunchProxy } = await loadTypeScriptModule(
    new URL("../src/urlValidation.ts", import.meta.url),
  );

  for (const accepted of [
    "",
    "http://127.0.0.1:7890",
    "https://proxy.example.com:8443",
    "socks5://127.0.0.1:1080",
    "socks5h://proxy.example.com:1080",
    "http://[::1]:7890",
  ]) {
    assert.equal(validateCodexLaunchProxy(accepted), "", accepted);
  }

  for (const rejected of [
    // Chromium 命令行代理不支持认证。
    "http://user:pass@proxy.example.com:8080",
    "socks5://user@127.0.0.1:1080",
    // 只允许协议、地址和端口。
    "http://127.0.0.1:7890/v1",
    "socks5://127.0.0.1:1080/v1",
    "http://127.0.0.1:7890?x=1",
    "http://127.0.0.1:7890#frag",
    // SOCKS 必须显式端口。
    "socks5://127.0.0.1",
    "socks5h://proxy.example.com",
    // 协议与格式。
    "ftp://proxy.example.com:21",
    "127.0.0.1:7890",
    "not a url",
  ]) {
    assert.notEqual(validateCodexLaunchProxy(rejected), "", rejected);
  }
});

test("launch proxy input lives in the Codex feature policy card", async () => {
  const [cardSource, typesSource, mockSource] = await Promise.all([
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readFile(new URL("src/App.types.ts", root), "utf8"),
    readFile(new URL("src/dev/mockApi.ts", root), "utf8"),
  ]);

  assert.match(typesSource, /codexLaunchProxy: string/);
  assert.match(mockSource, /codexLaunchProxy: ""/);
  // 启动代理是与「Codex 功能策略」平级的独立区块，排在它前面。
  assert.match(
    cardSource,
    /secondary-section launch-proxy-section" aria-labelledby="launch-proxy-title"[\s\S]*?Codex 启动代理[\s\S]*?aria-labelledby="runtime-title"/,
  );
  assert.match(cardSource, /codexLaunchProxy: event\.target\.value/);
  assert.match(cardSource, /aria-invalid=\{Boolean\(launchProxyError\)\}/);
  assert.match(cardSource, /validateCodexLaunchProxy\(launchProxy\)/);
  assert.match(cardSource, /重启 Codex 后生效/);
});

test("launch proxy reaches Chromium arguments, spawn environment and restart detection", async () => {
  const [configSource, commandsSource, processSource, proxySource, appServerSource] =
    await Promise.all([
      readFile(new URL("backend/src/config.rs", root), "utf8"),
      readFile(new URL("backend/src/commands.rs", root), "utf8"),
      readFile(new URL("backend/src/launcher/process.rs", root), "utf8"),
      readFile(new URL("backend/src/launch_proxy.rs", root), "utf8"),
      readFile(new URL("backend/src/appserver_call.rs", root), "utf8"),
    ]);

  assert.match(configSource, /pub codex_launch_proxy: String/);
  assert.match(configSource, /fn validate_codex_launch_proxy/);
  assert.match(commandsSource, /validate_codex_launch_proxy\(codex_launch_proxy\)/);
  assert.match(
    commandsSource,
    /applied\.codex_launch_proxy != current\.codex_launch_proxy/,
  );
  assert.match(processSource, /proxy\.chromium_argument\(\)/);
  assert.match(proxySource, /--proxy-server=/);
  assert.match(proxySource, /"socks5"[\s\S]{0,40}else/);
  assert.match(proxySource, /"HTTP_PROXY"/);
  assert.match(proxySource, /"NO_PROXY"/);
  assert.match(
    processSource,
    /local_router_proxy_bypass_environment[\s\S]*launch_proxy_environment/,
  );
  assert.match(
    appServerSource,
    /shared_app_server_environment\(\)/,
  );
});
