import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

async function loadWindowsStartupSource() {
  const [launcher, launcherPlatform] = await Promise.all([
    readFile(new URL("../backend/src/launcher/process.rs", import.meta.url), "utf8"),
    readFile(
      new URL("../backend/src/launcher/platform.rs", import.meta.url),
      "utf8",
    ),
  ]).then((sources) => sources.map((source) => source.replace(/\r\n/g, "\n")));
  const windowsSpawn = launcher.slice(
    launcher.indexOf("#[cfg(windows)]\n    {", launcher.indexOf("async fn spawn_codex")),
    launcher.indexOf("#[cfg(target_os = \"macos\")]", launcher.indexOf("async fn spawn_codex")),
  );
  const cleanup = launcherPlatform.slice(
    launcherPlatform.indexOf("async fn stop_windows_spawned_codex"),
    launcherPlatform.indexOf(
      "#[cfg(target_os = \"macos\")]\npub(super) fn build_fresh_macos_open_command",
    ),
  );
  return { cleanup, launcher, launcherPlatform, windowsSpawn };
}

test("Windows startup compatibility failure cleans the process before compatible restart", async () => {
  const { cleanup, windowsSpawn } = await loadWindowsStartupSource();
  const cleanupCall = windowsSpawn.indexOf(
    "stop_windows_spawned_codex(&mut spawned, app_dir).await",
  );
  const compatibleRestart = windowsSpawn.indexOf(
    "match spawn_windows_codex(app_dir, debug_port, &runtime_arguments, &[]).await",
  );

  assert.ok(cleanupCall >= 0);
  assert.ok(compatibleRestart > cleanupCall);
  assert.match(windowsSpawn, /fallback\.performance_status = "degraded"/);
  assert.match(
    windowsSpawn,
    /启动增强未能安装，已采用基础启动方式/,
  );
  assert.doesNotMatch(windowsSpawn, /宠物精简启动补丁未能确认生效/);
  assert.doesNotMatch(windowsSpawn, /petSlimRequested/);
  assert.match(cleanup, /-> Result<\(\)>/);
  assert.match(
    cleanup,
    /terminate_windows_codex_processes_with_timeout\(\s*app_dir,\s*process_id,\s*WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT,\s*\)\s*\.await/,
  );
  assert.doesNotMatch(
    cleanup,
    /terminate_windows_codex_processes\(app_dir, process_id\)\.await/,
  );
});

test("Windows startup patch requires app-server runtime override validation", async () => {
  const { launcher, launcherPlatform, windowsSpawn } = await loadWindowsStartupSource();

  assert.match(
    launcher,
    /codex_startup_patch::install\(\s*inspector_port,\s*patch_options,\s*runtime_config_overrides,\s*!runtime_config_overrides\.is_empty\(\),\s*\)/,
  );
  assert.doesNotMatch(
    launcher,
    /codex_startup_patch::install\(\s*inspector_port,\s*patch_options,\s*runtime_config_overrides,\s*false,\s*\)/,
  );
  assert.match(
    windowsSpawn,
    /if !runtime_config_overrides\.is_empty\(\) \{[\s\S]*?Codex 启动兼容方案未能确认 app-server 运行时覆盖/,
  );
  assert.match(windowsSpawn, /prepare_cli_wrapper\(/);
  assert.match(windowsSpawn, /install_startup_patch_with_cli_fallback\(/);
  assert.match(windowsSpawn, /WindowsPackageDebugSession::finish/);
  assert.match(launcherPlatform, /WindowsPackageDebugSession::start\(app_dir, environment\)/);
  assert.match(launcherPlatform, /settings\.EnableDebugging\(/);
  assert.match(launcherPlatform, /settings\.DisableDebugging\(/);
  assert.match(launcherPlatform, /child_command\.envs\(environment/);
});
