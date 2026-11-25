import assert from "node:assert/strict";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

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
    // 凭据交给本地认证中继，不交给 Chromium 命令行。
    "http://user:pass@proxy.example.com:8080",
    "socks5://user@127.0.0.1:1080",
  ]) {
    assert.equal(validateCodexLaunchProxy(accepted), "", accepted);
  }

  for (const rejected of [
    // 只允许协议、地址、端口和认证信息。
    "http://127.0.0.1:7890/v1",
    "socks5://127.0.0.1:1080/v1",
    "http://127.0.0.1:7890?x=1",
    "http://127.0.0.1:7890#frag",
    // SOCKS 必须显式端口。
    "socks5://127.0.0.1",
    "socks5h://proxy.example.com",
    // RFC 1929 的用户名和密码各限 255 字节。
    `socks5://${"u".repeat(256)}@127.0.0.1:1080`,
    // 协议与格式。
    "ftp://proxy.example.com:21",
    "127.0.0.1:7890",
    "not a url",
  ]) {
    assert.notEqual(validateCodexLaunchProxy(rejected), "", rejected);
  }
});
