import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadPatchExpression(
  runtimeConfigOverrides = [],
  subagentGateActive = runtimeConfigOverrides.includes("features.hooks=true"),
  requireAppServerRuntimeOverrideValidation = false,
) {
  const template = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.js", import.meta.url),
    "utf8",
  ));
  assert.ok(template, "startup patch template should be readable by the regression test");
  return template
    .replaceAll(
      '"__CODEY_RUNTIME_CONFIG_OVERRIDES__"',
      JSON.stringify(runtimeConfigOverrides),
    )
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll(
      "__SUBAGENT_GATE_ACTIVE__",
      subagentGateActive ? "true" : "false",
    )
    .replaceAll(
      "__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__",
      requireAppServerRuntimeOverrideValidation ? "true" : "false",
    );
}

async function loadPatchInIsolatedContext(runtimeConfigOverrides, contextOverrides = {}) {
  const childProcess = process.getBuiltinModule("child_process");
  const originalSpawn = childProcess.spawn;
  const spawnCalls = [];
  childProcess.spawn = (...args) => {
    spawnCalls.push(args);
    return { pid: 4242 };
  };
  const context = {
    clearTimeout,
    console,
    process,
    Promise,
    setImmediate,
    setTimeout,
    ...contextOverrides,
  };
  context.globalThis = context;
  try {
    const result = vm.runInNewContext(
      await loadPatchExpression(
        runtimeConfigOverrides,
        runtimeConfigOverrides.includes("features.hooks=true"),
        true,
      ),
      context,
    );
    return {
      context,
      result,
      restore() {
        childProcess.spawn = originalSpawn;
      },
      spawnCalls,
    };
  } catch (error) {
    childProcess.spawn = originalSpawn;
    throw error;
  }
}

test("startup patch disables Codex analytics and trims diagnostic polling", async () => {
  const Module = process.getBuiltinModule("module");
  const childProcess = process.getBuiltinModule("child_process");
  const workerThreads = process.getBuiltinModule("worker_threads");
  const originalLoad = Module._load;
  const originalJsExtension = Module._extensions[".js"];
  const originalSpawn = childProcess.spawn;
  const NativeWorker = workerThreads.Worker;
  const spawnCalls = [];
  const ipcHandlers = new Map();
  const rendererStatusResponses = [];
  const rendererEvent = {
    sender: {
      send(channel, message) {
        rendererStatusResponses.push({ channel, message });
      },
    },
  };
  const fakeIpcMain = new EventEmitter();
  fakeIpcMain.handle = (channel, handler) => {
    ipcHandlers.set(channel, handler);
  };
  const fakeElectron = {
    BrowserWindow: class BrowserWindow {},
    ipcMain: fakeIpcMain,
  };
  Module._load = function testElectronLoader(request) {
    if (request === "electron") return fakeElectron;
    return Reflect.apply(originalLoad, this, arguments);
  };
  childProcess.spawn = (...args) => {
    spawnCalls.push(args);
    return { pid: 42 };
  };

  try {
    const runtimeConfigOverrides = [
      "features.hooks=true",
      'model_provider="codey_global"',
      'model_providers.codey_global.base_url="http://127.0.0.1:61818/v1"',
      'developer_instructions="Codey route"',
      'mcp_servers.codey_fastctx.command="C:\\\\Program Files\\\\Codey\\\\codey-fastctx.exe"',
      'agents.default.config_file="D:\\\\Codey\\\\runtime\\\\default.toml"',
      'hooks.state."C:\\\\Users\\\\Kim\\\\.codex\\\\hooks.json:pre_tool_use:1:0".trusted_hash="sha256:test"',
      `hooks.PreToolUse=[{ hooks = [{ type = "command", command = "'C:\\\\Program Files\\\\Codey\\\\codey.exe' --codey-subagent-gate-hook" }] }]`,
    ];
    const nativeRuntimeConfigOverrides = runtimeConfigOverrides;
    const expression = await loadPatchExpression(runtimeConfigOverrides);
    assert.equal((0, eval)(expression), "codey-startup-patch-installed-v37");

    const patchedElectron = Module._load("electron");
    const passthroughGitHandler = () => "git-handler";
    const passthroughMessageHandler = () => "message-handler";
    patchedElectron.ipcMain.handle(
      "codex_desktop:worker:git:from-view",
      passthroughGitHandler,
    );
    patchedElectron.ipcMain.handle(
      "codex_desktop:message-from-view",
      passthroughMessageHandler,
    );
    assert.notEqual(
      ipcHandlers.get("codex_desktop:worker:git:from-view"),
      passthroughGitHandler,
    );
    assert.notEqual(
      ipcHandlers.get("codex_desktop:message-from-view"),
      passthroughMessageHandler,
    );
    const startupGitGuardStatus = ipcHandlers.get(
      "codex_desktop:message-from-view",
    )(rendererEvent, {
      type: "codey-git-request-guard-status",
      requestId: "git-status-1",
    });
    assert.equal(startupGitGuardStatus.status, "ok");
    assert.equal(startupGitGuardStatus.guard.gitHandlerPatched, true);
    assert.equal(startupGitGuardStatus.guard.statusHandlerPatched, true);
    assert.equal(startupGitGuardStatus.guard.ipcHandlersWrapped, 2);
    const startupWmiSamplerStatus = ipcHandlers.get(
      "codex_desktop:message-from-view",
    )(rendererEvent, {
      type: "codey-windows-wmi-sampler-status",
      requestId: "wmi-status-1",
    });
    assert.equal(startupWmiSamplerStatus.status, "ok");
    assert.equal(startupWmiSamplerStatus.sampler.version, 4);
    assert.equal(
      startupWmiSamplerStatus.sampler.workerWrapperPatched,
      true,
    );
    assert.equal(startupWmiSamplerStatus.sampler.blocked, 0);
    assert.equal(
      startupWmiSamplerStatus.sampler.esmExportsSynchronized,
      true,
    );
    assert.deepEqual(
      rendererStatusResponses.map(({ channel, message }) => ({
        channel,
        requestId: message.requestId,
        type: message.type,
      })),
      [
        {
          channel: "codex_desktop:message-for-view",
          requestId: "git-status-1",
          type: "codey-git-request-guard-status-response",
        },
        {
          channel: "codex_desktop:message-for-view",
          requestId: "wmi-status-1",
          type: "codey-windows-wmi-sampler-status-response",
        },
      ],
    );

    const renamedMessageChannel = "codex_desktop:messages:v2";
    const renamedMessageHandler = () => "renamed-message-handler";
    fakeElectron.ipcMain.handle(renamedMessageChannel, renamedMessageHandler);
    const guardedRenamedMessageHandler =
      ipcHandlers.get(renamedMessageChannel);
    assert.notEqual(guardedRenamedMessageHandler, renamedMessageHandler);
    const renamedChannelStatus = guardedRenamedMessageHandler(null, {
      type: "codey-git-request-guard-status",
    });
    assert.equal(renamedChannelStatus.status, "ok");
    assert.equal(
      renamedChannelStatus.guard.lastWrappedChannel,
      renamedMessageChannel,
    );
    assert.equal(renamedChannelStatus.guard.ipcHandlersWrapped, 3);

    const eventGitChannel = "codex_desktop:worker:git:event";
    let eventGitCalls = 0;
    const eventGitHandler = () => {
      eventGitCalls += 1;
    };
    patchedElectron.ipcMain.on(eventGitChannel, eventGitHandler);
    assert.notEqual(
      patchedElectron.ipcMain.rawListeners(eventGitChannel)[0],
      eventGitHandler,
    );
    patchedElectron.ipcMain.emit(eventGitChannel, null, { type: "noop" });
    patchedElectron.ipcMain.removeListener(eventGitChannel, eventGitHandler);
    patchedElectron.ipcMain.emit(eventGitChannel, null, { type: "noop" });
    assert.equal(eventGitCalls, 1);
    assert.equal(
      globalThis.__CODEY_MAIN_GIT_REQUEST_GUARD__.snapshot().ipcHandlersWrapped,
      4,
    );
    let onceGitCalls = 0;
    patchedElectron.ipcMain.once(eventGitChannel, () => {
      onceGitCalls += 1;
    });
    patchedElectron.ipcMain.emit(eventGitChannel, null, { type: "noop" });
    patchedElectron.ipcMain.emit(eventGitChannel, null, { type: "noop" });
    assert.equal(onceGitCalls, 1);

    let gitGuardTime = 10_000;
    let nextGitGuardTimer = 0;
    const gitGuardTimers = new Map();
    const mainGitGuard =
      globalThis.__CODEY_CREATE_MAIN_GIT_REQUEST_GUARD__({
        enabled: true,
        clock: () => gitGuardTime,
        scheduleTimeout(callback, delay) {
          const id = ++nextGitGuardTimer;
          gitGuardTimers.set(id, {
            callback,
            dueAt: gitGuardTime + delay,
          });
          return id;
        },
        cancelTimeout(id) {
          gitGuardTimers.delete(id);
        },
        limits: {
          perKeyIntervalMs: 20,
          tokenRefillMs: 10,
        },
      });
    const handledGitRequests = [];
    const nativeGitHandler = (_event, message) => {
      handledGitRequests.push(message);
      return { accepted: message.request?.id };
    };
    const guardedGitHandler = mainGitGuard.wrapGitHandler(nativeGitHandler);
    const gitRequest = (id) => ({
      type: "worker-request",
      workerId: "git",
      request: {
        id,
        method: "status-summary",
        params: { cwd: "C:\\repo" },
      },
    });
    const firstGitRequest = guardedGitHandler(null, gitRequest("status-1"));
    const secondGitRequest = guardedGitHandler(null, gitRequest("status-2"));
    assert.equal(handledGitRequests.length, 1);
    assert.equal(mainGitGuard.snapshot().queued, 1);
    await firstGitRequest;
    gitGuardTime += 20;
    for (const [id, timer] of [...gitGuardTimers]) {
      if (timer.dueAt > gitGuardTime) continue;
      gitGuardTimers.delete(id);
      timer.callback();
      await Promise.resolve();
    }
    assert.deepEqual(await secondGitRequest, { accepted: "status-2" });
    assert.equal(handledGitRequests.length, 2);
    const guardedStatusHandler = mainGitGuard.wrapStatusHandler(() => {
      throw new Error("Codey status messages must not reach Codex");
    });
    const mainGitGuardStatus = guardedStatusHandler(null, {
      type: "codey-git-request-guard-status",
    });
    assert.equal(mainGitGuardStatus.status, "ok");
    assert.equal(mainGitGuardStatus.guard.gitHandlerPatched, true);
    assert.equal(mainGitGuardStatus.guard.statusHandlerPatched, true);
    assert.equal(mainGitGuardStatus.guard.strategy, "main-process-ipc");

    const desktopMcpConfig =
      'mcp_servers.codex_app={ command = "/opt/codex-app-mcp", args = ["server.mjs"] }';
    const directArgs = [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "--analytics-default-enabled",
      "-c",
      desktopMcpConfig,
    ];
    childProcess.spawn("/Applications/ChatGPT.app/Contents/Resources/codex", directArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
      "-c",
      desktopMcpConfig,
    ]);
    const fastctxRuntimeConfig = nativeRuntimeConfigOverrides.find((config) =>
      config.startsWith("mcp_servers.codey_fastctx.command="),
    );
    assert.ok(fastctxRuntimeConfig);
    assert.ok(
      spawnCalls.at(-1)[1].indexOf(fastctxRuntimeConfig) <
        spawnCalls.at(-1)[1].indexOf(desktopMcpConfig),
    );
    assert.equal(
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_ACTIVE,
      "1",
    );
    const subagentGateRuntimeId =
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_RUNTIME_ID;
    assert.match(subagentGateRuntimeId, /^[A-Za-z0-9-]+$/);
    const alreadyPatchedDirectArgs = spawnCalls.at(-1)[1];
    childProcess.spawn("codex", alreadyPatchedDirectArgs);
    assert.equal(spawnCalls.at(-1)[1], alreadyPatchedDirectArgs);
    assert.equal(
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_ACTIVE,
      "1",
    );
    const secondSubagentGateRuntimeId =
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_RUNTIME_ID;
    assert.notEqual(
      secondSubagentGateRuntimeId,
      subagentGateRuntimeId,
    );

    const wrappedAppServerArgs = [
      "/opt/codey/codex.js",
      "-c",
      'model_provider="stale_provider"',
      "--config",
      'model_providers.codey_global.base_url="https://stale.example/v1"',
      "app-server",
      "--analytics-default-enabled",
    ];
    childProcess.spawn(process.execPath, wrappedAppServerArgs);
    const patchedWrappedAppServerArgs = spawnCalls.at(-1)[1];
    assert.deepEqual(patchedWrappedAppServerArgs, [
      "/opt/codey/codex.js",
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    assert.equal(
      patchedWrappedAppServerArgs.filter(
        (argument) => argument === 'model_provider="codey_global"',
      ).length,
      1,
    );
    assert.equal(
      patchedWrappedAppServerArgs.some((argument) =>
        String(argument).includes("stale_provider") ||
        String(argument).includes("stale.example")
      ),
      false,
    );
    assert.equal(
      spawnCalls.at(-1)[2].env.CODEY_SUBAGENT_GATE_ACTIVE,
      "1",
    );

    const configuredArgs = [
      "-c",
      "analytics.enabled=true",
      "app-server",
      "--analytics-default-enabled",
    ];
    childProcess.spawn("codex", configuredArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);

    const argsWithoutLegacyAnalyticsFlag = ["app-server"];
    childProcess.spawn("codex", argsWithoutLegacyAnalyticsFlag);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);

    const unrelatedArgs = ["--version"];
    childProcess.spawn("git", unrelatedArgs);
    assert.equal(spawnCalls.at(-1)[1], unrelatedArgs);

    const unrelatedShell = "echo 'app-server --analytics-default-enabled'";
    childProcess.spawn("bash", ["-lc", unrelatedShell]);
    assert.equal(spawnCalls.at(-1)[1].at(-1), unrelatedShell);

    const runtimeManagedAppServerArgs = ["app-server", "--analytics-default-enabled"];
    childProcess.spawn("node", runtimeManagedAppServerArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...nativeRuntimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);

    const spawnOptions = { cwd: "/tmp" };
    childProcess.spawn("git", spawnOptions);
    assert.equal(spawnCalls.at(-1).length, 2);
    assert.equal(spawnCalls.at(-1)[1], spawnOptions);
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.appServerAnalyticsPatchCount,
      5,
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
    assert.equal(
      await globalThis.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__(),
      "codey-app-server-runtime-overrides-verified",
    );

    const desktopAnalyticsFixture = [
      "let u={},g={get(){return Promise.resolve({})}},",
      "d={analyticsEnabled:u!=null&&u.analytics?.enabled!==!1};",
      "p.postMessage({type:`worker-analytics-enabled-update`,",
      "enabled:e.analytics?.enabled!==!1});",
      "T=new Transport({analyticsEnabled:g.get().then(",
      "e=>e.analytics?.enabled!==!1)}),",
      "E=new Reporter({source:`codex-desktop`,transport:T});",
    ].join("");
    const patchedDesktopAnalytics =
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__(
        desktopAnalyticsFixture,
      );
    assert.equal(
      patchedDesktopAnalytics.match(/analyticsEnabled:!1/g)?.length,
      2,
    );
    assert.match(
      patchedDesktopAnalytics,
      /worker-analytics-enabled-update`,enabled:!1/,
    );
    assert.doesNotMatch(
      patchedDesktopAnalytics,
      /analytics\?\.enabled!==!1/,
    );

    const doubleQuotedDesktopAnalyticsFixture =
      desktopAnalyticsFixture.replaceAll("`", '"');
    const patchedDoubleQuotedDesktopAnalytics =
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__(
        doubleQuotedDesktopAnalyticsFixture,
      );
    assert.equal(
      patchedDoubleQuotedDesktopAnalytics.match(/analyticsEnabled:!1/g)?.length,
      2,
    );
    assert.match(
      patchedDoubleQuotedDesktopAnalytics,
      /worker-analytics-enabled-update",enabled:!1/,
    );
    assert.doesNotMatch(
      patchedDoubleQuotedDesktopAnalytics,
      /analytics\?\.enabled!==!1/,
    );

    const desktopAnalyticsWithoutReporterFixture =
      desktopAnalyticsFixture.replace(
        "E=new Reporter({source:`codex-desktop`,transport:T});",
        "",
      );
    const patchedDesktopAnalyticsWithoutReporter =
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__(
        desktopAnalyticsWithoutReporterFixture,
      );
    assert.equal(
      patchedDesktopAnalyticsWithoutReporter.match(/analyticsEnabled:!1/g)
        ?.length,
      2,
    );
    assert.doesNotMatch(
      patchedDesktopAnalyticsWithoutReporter,
      /analytics\?\.enabled!==!1/,
    );

    const incompatibleDesktopAnalyticsFixture =
      "const analyticsEnabledFromNewBundleShape = true;";
    const degradedDesktopAnalytics =
      globalThis.__CODEY_APPLY_OPTIONAL_MAIN_BUNDLE_PATCH__(
        "desktopCesAnalytics",
        globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__,
        incompatibleDesktopAnalyticsFixture,
      );
    assert.equal(
      degradedDesktopAnalytics,
      incompatibleDesktopAnalyticsFixture,
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.disableDesktopCesAnalytics,
      false,
    );
    assert.deepEqual(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.optionalMainBundlePatchFailures,
      [{
        name: "desktopCesAnalytics",
        message: "Codey desktop analytics matches 0/0/0",
      }],
    );

    globalThis.__CODEY_APPLY_OPTIONAL_MAIN_BUNDLE_PATCH__(
      "desktopCesAnalytics",
      globalThis.__CODEY_PATCH_CODEX_MAIN_DESKTOP_ANALYTICS__,
      desktopAnalyticsFixture,
    );
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.disableDesktopCesAnalytics,
      true,
    );
    assert.deepEqual(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.optionalMainBundlePatchFailures,
      [],
    );

    const fixture = [
      "let Oe={},",
      "ke=()=>{Oe.reconcileExternalPluginState(`focus`)};",
      "l.app.on(`browser-window-focus`,ke);",
      "P.add(()=>{l.app.off(`browser-window-focus`,ke)});",
    ].join("");
    const patchedFixture =
      globalThis.__CODEY_PATCH_CODEX_MAIN_FOCUS_RECONCILE__(fixture);
    assert.match(
      patchedFixture,
      /ke=globalThis\.__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__/,
    );
    assert.match(patchedFixture, /ke\.cancel\?\.\(\)/);

    const reconciles = [];
    const throttled =
      globalThis.__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__(
        (value) => reconciles.push(value),
        20,
      );
    throttled("leading");
    throttled("middle");
    throttled("trailing");
    assert.deepEqual(reconciles, ["leading"]);
    await new Promise((resolve) => setTimeout(resolve, 35));
    assert.deepEqual(reconciles, ["leading", "trailing"]);
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__
        .externalPluginFocusReconcileSuppressedCount,
      2,
    );

    const cancelledReconciles = [];
    const cancelled =
      globalThis.__CODEY_THROTTLE_EXTERNAL_PLUGIN_FOCUS_RECONCILE__(
        (value) => cancelledReconciles.push(value),
        20,
      );
    cancelled("leading");
    cancelled("trailing");
    cancelled.cancel();
    await new Promise((resolve) => setTimeout(resolve, 35));
    assert.deepEqual(cancelledReconciles, ["leading"]);

    const heartbeatFixture = [
      "class Sampler{constructor(){",
      "this.appStateHeartbeat=setInterval(()=>{",
      "this.requestAppStateSnapshot(`heartbeat`)",
      "},gX),this.appStateHeartbeat.unref()",
      "}dispose(){clearInterval(this.appStateHeartbeat)}",
      "requestAppStateSnapshot(e){",
      "send({type:`electron-app-state-snapshot-request`,reason:e})",
      "}}",
    ].join("");
    const patchedHeartbeat =
      globalThis.__CODEY_PATCH_CODEX_MAIN_APP_STATE_HEARTBEAT__(
        heartbeatFixture,
      );
    assert.match(patchedHeartbeat, /this\.appStateHeartbeat=null/);
    assert.doesNotMatch(patchedHeartbeat, /appStateHeartbeat=setInterval/);
    assert.match(
      patchedHeartbeat,
      /requestAppStateSnapshot\(e\).*electron-app-state-snapshot-request/,
    );
  } finally {
    childProcess.spawn = originalSpawn;
    workerThreads.Worker = NativeWorker;
    Module.syncBuiltinESMExports?.();
    Module._load = originalLoad;
    Module._extensions[".js"] = originalJsExtension;
  }
});

test("startup patch fails closed when app-server runtime override injection is never observed", async () => {
  const runtimeConfigOverrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.name="Codey Local Router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:61818/v1"',
    'model_providers.codey_router.http_headers={ x-codey-router-token = "codey-router-secret-token-1234" }',
  ];
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides, {
    setTimeout(callback) {
      queueMicrotask(callback);
      return { unref() {} };
    },
    clearTimeout() {},
  });

  try {
    assert.match(
      await loadPatchExpression(runtimeConfigOverrides, false, true),
      /appServerRuntimeOverrideTimeoutMs = 20_000/,
    );
    assert.equal(runtime.result, "codey-startup-patch-installed-v37");
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.observed,
      false,
    );
    await assert.rejects(
      runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__(),
      (error) => {
        assert.match(
          error.message,
          /当前 Codex 版本的 app-server 启动参数结构与 Codey 不兼容/,
        );
        assert.match(error.message, /model_providers\.codey_router\.http_headers/);
        assert.doesNotMatch(error.message, /secret-token-1234/);
        return true;
      },
    );
  } finally {
    runtime.restore();
  }
});

test("startup patch keeps Codey MCP servers in the app-server config layer", async () => {
  const childProcess = process.getBuiltinModule("child_process");
  const runtimeConfigOverrides = [
    'mcp_servers.codey_fastctx.command="/opt/codey-fastctx"',
    'mcp_servers.codey_fastctx.args=["--codey-fastctx-mcp"]',
    'mcp_servers.codey_subagent_control.command="/opt/codey"',
  ];
  const desktopMcpConfig =
    'mcp_servers.codex_app={ command = "/opt/codex-app-mcp", args = ["server.mjs"] }';
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides);

  try {
    childProcess.spawn("codex", [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "-c",
      desktopMcpConfig,
    ]);
    const rewritten = Array.from(runtime.spawnCalls.at(-1)[1]);
    const appServerIndex = rewritten.indexOf("app-server");
    const desktopMcpIndex = rewritten.indexOf(desktopMcpConfig);

    assert.ok(appServerIndex >= 0);
    assert.ok(desktopMcpIndex > appServerIndex);
    for (const config of runtimeConfigOverrides) {
      const configIndex = rewritten.indexOf(config);
      assert.ok(
        configIndex > appServerIndex,
        `${config} must follow app-server`,
      );
      assert.ok(
        configIndex < desktopMcpIndex,
        `${config} must share the desktop MCP layer`,
      );
    }
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
  } finally {
    runtime.restore();
  }
});

test("startup patch resolves app-server runtime override validation after the matching spawn", async () => {
  const childProcess = process.getBuiltinModule("child_process");
  const runtimeConfigOverrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.name="Codey Local Router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:61818/v1"',
  ];
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides);

  try {
    const pending =
      runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__();
    childProcess.spawn("codex", ["app-server"]);
    assert.equal(
      await pending,
      "codey-app-server-runtime-overrides-verified",
    );
    assert.deepEqual(Array.from(runtime.spawnCalls.at(-1)[1]), [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...runtimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
  } finally {
    runtime.restore();
  }
});

test("startup patch tolerates duplicate Codex analytics flags while injecting runtime overrides", async () => {
  const childProcess = process.getBuiltinModule("child_process");
  const runtimeConfigOverrides = [
    'model_provider="codey_router"',
    'model_providers.codey_router.name="Codey Local Router"',
    'model_providers.codey_router.base_url="http://127.0.0.1:61818/v1"',
  ];
  const runtime = await loadPatchInIsolatedContext(runtimeConfigOverrides);

  try {
    const pending =
      runtime.context.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__();
    childProcess.spawn("codex", [
      "app-server",
      "--analytics-default-enabled",
      "--analytics-default-enabled",
    ]);
    assert.equal(
      await pending,
      "codey-app-server-runtime-overrides-verified",
    );
    assert.deepEqual(Array.from(runtime.spawnCalls.at(-1)[1]), [
      "app-server",
      "-c",
      "analytics.enabled=false",
      ...runtimeConfigOverrides.flatMap((config) => ["-c", config]),
    ]);
    assert.equal(
      runtime.context.__CODEY_CODEX_STARTUP_PATCH__.appServerRuntimeOverrides.complete,
      true,
    );
  } finally {
    runtime.restore();
  }
});
