import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const proxyEnvironmentKeys = [
  "CODEY_WORKFLOW_PROXY_ENABLED",
  "CODEY_WORKFLOW_PROXY_EXECUTABLE",
  "CODEY_WORKFLOW_PROXY_CONTROL_ADDR",
  "CODEY_WORKFLOW_PROXY_TOKEN",
  "CODEY_WORKFLOW_PROXY_BYPASS",
];

async function loadStartupPatchExpression() {
  const template = await readFile(
    new URL("../backend/src/codex_startup_patch.js", import.meta.url),
    "utf8",
  );
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__FAST_CODEX_STARTUP__", "false")
    .replaceAll("__SUBAGENT_GATE_ACTIVE__", "false")
    .replaceAll('"__CODEY_RUNTIME_CONFIG_OVERRIDES__"', "[]")
    .replaceAll('"__CODEY_ERROR_LOGGER_EXECUTABLE__"', '""');
}

test("workflow proxy is opt-in and rewrites only exact codex app-server spawns", async () => {
  const Module = process.getBuiltinModule("module");
  const nativeLoad = Module._load;
  const nativeJsExtension = Module._extensions[".js"];
  const nativeGetBuiltinModule = process.getBuiltinModule;
  const originalEnvironment = new Map(
    proxyEnvironmentKeys.map((key) => [key, process.env[key]]),
  );
  const nativeConsoleError = console.error;
  const consoleErrors = [];
  const nativeSpawns = [];
  const fakeChildProcess = {
    spawn(command, args, ...rest) {
      const child = { command, args, rest, passedThrough: true };
      nativeSpawns.push(child);
      return child;
    },
    spawnSync() {
      return { status: 0, stderr: "" };
    },
  };
  class FakeBrowserWindow {}
  const fakeElectron = { BrowserWindow: FakeBrowserWindow };
  Module._load = function testStartupPatchLoader(request) {
    if (request === "electron" || request === "node:electron") {
      return fakeElectron;
    }
    if (request === "child_process" || request === "node:child_process") {
      return fakeChildProcess;
    }
    return Reflect.apply(nativeLoad, this, arguments);
  };
  process.getBuiltinModule = function testBuiltinModuleLoader(request) {
    if (request === "child_process" || request === "node:child_process") {
      return fakeChildProcess;
    }
    return Reflect.apply(nativeGetBuiltinModule, this, arguments);
  };
  console.error = (...values) => { consoleErrors.push(values); };
  for (const key of proxyEnvironmentKeys) delete process.env[key];

  try {
    assert.equal(
      (0, eval)(await loadStartupPatchExpression()),
      "codey-startup-patch-installed-v22",
    );
    const childProcess = Module._load("node:child_process", undefined, false);

    const disabled = childProcess.spawn("/opt/codex", ["app-server"]);
    assert.equal(disabled.command, "/opt/codex");
    assert.deepEqual(disabled.args, [
      "-c",
      "analytics.enabled=false",
      "app-server",
    ]);
    assert.equal(
      globalThis.__CODEY_REWRITE_CODEX_APP_SERVER_PROXY_SPAWN__(
        "/opt/codex",
        disabled.args,
      ),
      null,
    );

    const capabilityToken = "T".repeat(64);
    process.env.CODEY_WORKFLOW_PROXY_ENABLED = "1";
    process.env.CODEY_WORKFLOW_PROXY_EXECUTABLE = process.execPath;
    process.env.CODEY_WORKFLOW_PROXY_CONTROL_ADDR = "127.0.0.1:43127";
    process.env.CODEY_WORKFLOW_PROXY_TOKEN = capabilityToken;

    const proxied = childProcess.spawn("/opt/codex", [
      "app-server",
      "--listen",
      "stdio",
    ]);
    assert.equal(proxied.command, process.execPath);
    assert.deepEqual(proxied.args, [
      "--codey-app-server-proxy",
      "--codex-executable",
      "/opt/codex",
      "--",
      "-c",
      "analytics.enabled=false",
      "app-server",
      "--listen",
      "stdio",
    ]);
    assert.doesNotMatch(
      JSON.stringify({ command: proxied.command, args: proxied.args }),
      new RegExp(capabilityToken),
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.appServerWorkflowProxySpawnCount,
      1,
    );

    const nodeSpawn = childProcess.spawn("/usr/bin/node", ["app-server"]);
    assert.equal(nodeSpawn.command, "/usr/bin/node");
    assert.deepEqual(nodeSpawn.args, ["app-server"]);

    const lookalikeSubcommand = childProcess.spawn("/opt/codex", [
      "exec",
      "app-server",
    ]);
    assert.equal(lookalikeSubcommand.command, "/opt/codex");
    assert.ok(lookalikeSubcommand.args.includes("exec"));

    const duplicateSubcommand = childProcess.spawn("/opt/codex", [
      "app-server",
      "app-server",
    ]);
    assert.equal(duplicateSubcommand.command, "/opt/codex");
    assert.deepEqual(duplicateSubcommand.args, ["app-server", "app-server"]);

    process.env.CODEY_WORKFLOW_PROXY_BYPASS = "1";
    const bypassed = childProcess.spawn("/opt/codex", ["app-server"]);
    assert.equal(bypassed.command, "/opt/codex");
    assert.ok(!bypassed.args.includes("--codey-app-server-proxy"));
    delete process.env.CODEY_WORKFLOW_PROXY_BYPASS;

    delete process.env.CODEY_WORKFLOW_PROXY_TOKEN;
    const missingControlCapability = childProcess.spawn("/opt/codex", [
      "app-server",
    ]);
    assert.equal(missingControlCapability.command, "/opt/codex");

    process.env.CODEY_WORKFLOW_PROXY_TOKEN = capabilityToken;
    process.env.CODEY_WORKFLOW_PROXY_CONTROL_ADDR = "0.0.0.0:43127";
    const nonLoopbackControl = childProcess.spawn("/opt/codex", ["app-server"]);
    assert.equal(nonLoopbackControl.command, "/opt/codex");

    assert.ok(nativeSpawns.length >= 8);
    assert.doesNotMatch(JSON.stringify(consoleErrors), new RegExp(capabilityToken));
    assert.doesNotMatch(
      JSON.stringify(globalThis.__CODEY_CODEX_STARTUP_PATCH__),
      new RegExp(capabilityToken),
    );
  } finally {
    Module._load = nativeLoad;
    Module._extensions[".js"] = nativeJsExtension;
    process.getBuiltinModule = nativeGetBuiltinModule;
    console.error = nativeConsoleError;
    for (const [key, value] of originalEnvironment) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  }
});
