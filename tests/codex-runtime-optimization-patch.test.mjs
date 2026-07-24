import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

async function loadPatchExpression() {
  const source = normalizeLineEndings(await readFile(
    new URL("../backend/src/codex_startup_patch.rs", import.meta.url),
    "utf8",
  ));
  const template = source.match(
    /const STARTUP_PATCH_TEMPLATE: &str = r#"\n([\s\S]*?)\n"#;/,
  )?.[1];
  assert.ok(template, "startup patch template should be readable by the regression test");
  return template
    .replaceAll("__DISABLE_PET__", "false")
    .replaceAll("__DISABLE_VOICE__", "false");
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
  childProcess.spawn = (...args) => {
    spawnCalls.push(args);
    return { pid: 42 };
  };

  try {
    const expression = await loadPatchExpression();
    assert.equal((0, eval)(expression), "codey-startup-patch-installed-v10");

    const directArgs = [
      "-c",
      "features.code_mode_host=true",
      "app-server",
      "--analytics-default-enabled",
    ];
    childProcess.spawn("/Applications/ChatGPT.app/Contents/Resources/codex", directArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "-c",
      "features.code_mode_host=true",
      "-c",
      "analytics.enabled=false",
      "app-server",
    ]);

    const configuredArgs = [
      "-c",
      "analytics.enabled=true",
      "app-server",
      "--analytics-default-enabled",
    ];
    childProcess.spawn("codex", configuredArgs);
    assert.deepEqual(spawnCalls.at(-1)[1], [
      "-c",
      "analytics.enabled=false",
      "app-server",
    ]);

    const shellCommand = [
      "source /etc/profile;",
      "exec /usr/bin/codex -c features.code_mode_host=true",
      "app-server --analytics-default-enabled",
    ].join(" ");
    childProcess.spawn("wsl.exe", [
      "-d",
      "Ubuntu",
      "--",
      "/usr/bin/bash",
      "-lc",
      shellCommand,
    ]);
    const patchedShellCommand = spawnCalls.at(-1)[1].at(-1);
    assert.doesNotMatch(patchedShellCommand, /--analytics-default-enabled/);
    assert.match(
      patchedShellCommand,
      /-c features\.code_mode_host=true -c analytics\.enabled=false app-server/,
    );

    const configuredShellCommand = [
      "source /etc/profile;",
      "exec /usr/bin/codex --config=analytics.enabled=custom",
      "app-server --analytics-default-enabled",
    ].join(" ");
    childProcess.spawn("wsl.exe", [
      "-d",
      "Ubuntu",
      "--",
      "/usr/bin/bash",
      "-lc",
      configuredShellCommand,
    ]);
    const patchedConfiguredShellCommand = spawnCalls.at(-1)[1].at(-1);
    assert.match(
      patchedConfiguredShellCommand,
      /--config=analytics\.enabled=false app-server/,
    );
    assert.equal(
      patchedConfiguredShellCommand.match(/analytics\.enabled=false/g)?.length,
      1,
    );

    const unrelatedArgs = ["--version"];
    childProcess.spawn("git", unrelatedArgs);
    assert.equal(spawnCalls.at(-1)[1], unrelatedArgs);

    const unrelatedShell = "echo 'app-server --analytics-default-enabled'";
    childProcess.spawn("bash", ["-lc", unrelatedShell]);
    assert.equal(spawnCalls.at(-1)[1].at(-1), unrelatedShell);

    const unrelatedWslShell =
      "source /etc/profile; exec /usr/bin/echo 'app-server --analytics-default-enabled'";
    childProcess.spawn("wsl.exe", [
      "-d",
      "Ubuntu",
      "--",
      "/usr/bin/bash",
      "-lc",
      unrelatedWslShell,
    ]);
    assert.equal(spawnCalls.at(-1)[1].at(-1), unrelatedWslShell);

    const unrelatedCodexTokens = ["app-server", "--analytics-default-enabled"];
    childProcess.spawn("node", unrelatedCodexTokens);
    assert.equal(spawnCalls.at(-1)[1], unrelatedCodexTokens);

    const spawnOptions = { cwd: "/tmp" };
    childProcess.spawn("git", spawnOptions);
    assert.equal(spawnCalls.at(-1).length, 2);
    assert.equal(spawnCalls.at(-1)[1], spawnOptions);
    assert.equal(
      globalThis.__CODEY_CODEX_STARTUP_PATCH__.appServerAnalyticsPatchCount,
      4,
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
    Module._load = originalLoad;
    Module._extensions[".js"] = originalJsExtension;
  }
});
