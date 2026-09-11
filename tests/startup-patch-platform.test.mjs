import assert from "node:assert/strict";
import test from "node:test";

import { loadSpawnCodexSections } from "./helpers/startup-patch.mjs";

// Static contracts keep both platforms' spawn_codex wiring visible on any CI host.
test("Windows startup compatibility failure cleans the process before compatible restart", async () => {
  const { cleanup, windowsSpawn } = await loadSpawnCodexSections();
  const cleanupCall = windowsSpawn.indexOf(
    "stop_windows_spawned_codex(&mut spawned, app_dir).await",
  );
  const compatibleRestart = windowsSpawn.indexOf(
    "match spawn_windows_codex(app_dir, debug_port, &runtime_arguments, &[], false)",
  );

  assert.ok(cleanupCall >= 0);
  assert.ok(compatibleRestart > cleanupCall);
  assert.match(windowsSpawn, /fallback\.performance_status = "degraded"/);
  assert.match(
    windowsSpawn,
    /Codex 已启动，但部分启动设置未能应用/,
  );
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

test("Windows skips the Inspector when the Electron fuse is off and retries without a breakpoint", async () => {
  const { launcher, windowsSpawn } = await loadSpawnCodexSections();
  const fuseProbe = windowsSpawn.indexOf(
    "crate::electron_fuses::detect_electron_fuses(app_dir.to_path_buf()).await",
  );
  const loop = windowsSpawn.indexOf("loop {");
  const prepareRequire = windowsSpawn.indexOf("prepare_startup_require_launch(");
  const prepare = windowsSpawn.indexOf("prepare_cli_wrapper(");
  const reservePort = windowsSpawn.indexOf("reserve_loopback_port()");
  const noEntry = windowsSpawn.search(
    /if inspector_port\.is_none\(\) && wrapper\.is_none\(\)\s*&& require_patch\.is_none\(\)/,
  );
  const launch = windowsSpawn.indexOf("spawn_windows_codex(", noEntry);
  const budget = windowsSpawn.indexOf("let deadline =");
  const cleanup = windowsSpawn.indexOf("if let Err(cleanup_error) =");
  const packageGuard = windowsSpawn.indexOf("if !package_cleanup_succeeded {");
  const retry = windowsSpawn.indexOf("if should_retry_startup(&error, attempt) {");
  const requiredConfigGuard = windowsSpawn.indexOf("if !runtime_config_overrides.is_empty() {");

  // Store updates can change the executable between attempts; refresh before probing it.
  const refresh = windowsSpawn.indexOf("refresh_windows_packaged_app_dir(app_dir)");
  assert.ok(loop >= 0 && loop < refresh && refresh < fuseProbe && fuseProbe < prepareRequire);
  assert.ok(prepareRequire < prepare);
  assert.match(windowsSpawn, /let require_wanted = fuses\.node_options\.node_options_possible\(\);/);
  assert.match(windowsSpawn, /let use_require = require_patch\.is_some\(\);/);
  assert.match(
    windowsSpawn,
    /let use_inspector =\s*!use_require && inspect_fuse\.inspector_possible\(\) && !retry_without_inspector;/,
  );
  assert.match(windowsSpawn, /use_inspector \|\| use_require/);
  assert.ok(loop < prepare && prepare < reservePort && reservePort < noEntry && noEntry < launch);
  assert.match(windowsSpawn, /let inspector_port = if use_inspector \{/);
  assert.match(
    windowsSpawn,
    /use_require \|\| \(!use_inspector && constrained\)/,
  );
  assert.match(windowsSpawn, /startup_launch_arguments\(&runtime_arguments, inspector_port\)/);
  // Without any compatibility entry the decision is made before launching.
  assert.match(
    windowsSpawn.slice(noEntry, launch),
    /return launch_windows_codex_without_compatibility\(/,
  );
  // Each attempt gets its own readiness budget, taken after activation.
  assert.ok(budget > launch && budget < cleanup);
  assert.match(windowsSpawn, /STARTUP_CLI_READY_TIMEOUT/);
  assert.doesNotMatch(windowsSpawn, /STARTUP_COMPATIBILITY_TIMEOUT|startup_deadline\.get_or_insert/);
  assert.match(
    windowsSpawn,
    /StartupWaitContext \{\s*platform: "windows",\s*deadline,\s*renderer_debug_port: Some\(debug_port\),\s*spawned: Some\(&mut spawned\),/,
  );
  assert.ok(cleanup > launch && packageGuard > cleanup && retry > packageGuard);
  assert.ok(requiredConfigGuard > retry);
  assert.match(windowsSpawn.slice(cleanup, retry), /if let Err\(cleanup_error\)[\s\S]*?anyhow::bail!/);
  assert.match(windowsSpawn.slice(packageGuard, retry), /anyhow::bail!/);
  assert.match(
    windowsSpawn.slice(retry, requiredConfigGuard),
    /if should_retry_startup\(&error, attempt\) \{\s*retry_without_inspector = true;\s*continue;\s*\}/,
  );
  assert.match(windowsSpawn, /return Ok\(spawned\);/);

  // Missing entries never launch a constrained Codex.
  const noEntryLaunch = launcher.slice(
    launcher.indexOf("async fn launch_windows_codex_without_compatibility"),
    launcher.indexOf("async fn spawned_codex_alive"),
  );
  const overrideGuard = noEntryLaunch.indexOf("if !runtime_config_overrides.is_empty() {");
  const subagentGuard = noEntryLaunch.indexOf("if subagent_gate_active {");
  const degradedLaunch = noEntryLaunch.indexOf(
    "spawn_windows_codex(app_dir, debug_port, runtime_arguments, &[], false)",
  );
  assert.ok(overrideGuard >= 0 && overrideGuard < subagentGuard && subagentGuard < degradedLaunch);
  assert.match(noEntryLaunch, /performance_status = "degraded"/);
});

test("Startup waits end on process exit, marker confirmation or renderer evidence", async () => {
  const { launcher, startupPatch } = await loadSpawnCodexSections();

  assert.match(
    launcher,
    /tokio::select! \{\s*exited = exited => Err\(exited\.into\(\)\),\s*result = compatibility => result,\s*\}/,
  );
  assert.match(launcher, /error\.is::<crate::codex_startup_patch::StartupProcessExited>\(\)/);
  assert.match(launcher, /let marker = watch_cli_wrapper_marker\(&marker_path\);/);
  assert.match(launcher, /CLI_WRAPPER_MARKER_ENV\.to_string\(\)/);
  assert.match(launcher, /prune_cli_wrapper_markers\(/);
  assert.match(
    launcher,
    /Err\(patch_error\) if patch_error\.is::<InspectorUnavailable>\(\) => \{[\s\S]*?wrapper_ready\.as_mut\(\)\.await/,
  );
  assert.match(launcher, /渲染进程调试端口未就绪，主进程可能停在 --inspect-brk 断点/);

  // The wrapper retries a slow loopback connect but never a refused one, and
  // records its progress in the marker file as a second channel.
  assert.match(startupPatch, /fn connect_loopback_with_retry\(/);
  assert.match(
    startupPatch,
    /Err\(error\) if error\.kind\(\) == std::io::ErrorKind::ConnectionRefused => \{\s*return Err\(error\);/,
  );
  assert.match(startupPatch, /readiness\.mark\(CliWrapperMarkerStatus::Launching, None\)/);
  assert.match(startupPatch, /fn executed\(self\) \{\s*self\.mark_executed\(\);/);
  assert.match(startupPatch, /self\.mark\(CliWrapperMarkerStatus::Failed, Some\(failure\)\)/);
  // Inspector discovery stops as soon as the renderer port proves the main script runs.
  assert.match(
    startupPatch,
    /if loopback_port_accepts\(debug_port\)\.await \{[\s\S]*?return Err\(InspectorUnavailable \{/,
  );
});

test("Windows startup patch requires app-server runtime override validation", async () => {
  const { launcher, launcherPlatform, windowsSpawn } = await loadSpawnCodexSections();

  assert.match(
    launcher,
    /codex_startup_patch::install\(\s*inspector_port,\s*patch_options,\s*runtime_config_overrides,\s*!runtime_config_overrides\.is_empty\(\),\s*renderer_debug_port,\s*\)/,
  );
  assert.doesNotMatch(
    launcher,
    /codex_startup_patch::install\(\s*inspector_port,\s*patch_options,\s*runtime_config_overrides,\s*false,/,
  );
  assert.match(
    windowsSpawn,
    /if !runtime_config_overrides\.is_empty\(\) \{[\s\S]*?Codex 启动兼容方案未能确认 app-server 运行时覆盖/,
  );
  assert.match(windowsSpawn, /prepare_cli_wrapper\(/);
  assert.match(windowsSpawn, /install_startup_patch_with_cli_fallback\(/);
  assert.match(
    windowsSpawn,
    /match startup_result \{\s*Ok\(mode\) => \{\s*spawned\.startup_injection_mode = mode\.as_str\(\)\.to_string\(\);\s*spawned\.performance_status = "ready"/,
  );
  assert.match(windowsSpawn, /WindowsPackageDebugSession::finish/);
  assert.match(launcherPlatform, /WindowsPackageDebugSession::start\(app_dir, environment\)/);
  assert.match(launcherPlatform, /settings\.EnableDebugging\(/);
  assert.match(launcherPlatform, /settings\.DisableDebugging\(/);
  assert.match(launcherPlatform, /child_command\.envs\(environment/);
  const packageSetup = launcherPlatform.indexOf("match WindowsPackageDebugSession::start(app_dir, environment)");
  const activation = launcherPlatform.indexOf("codey_runtime_core::launcher::activate_packaged_app", packageSetup);
  assert.match(launcherPlatform.slice(packageSetup, activation), /if require_wrapper_environment \{\s*return Err\(error\)/);
});

test("macOS startup patch requires app-server runtime override validation", async () => {
  const { launcher: source, macosSpawn } = await loadSpawnCodexSections();
  const successStart = macosSpawn.indexOf("Ok(mode) =>");
  const failureStart = macosSpawn.indexOf("Err(error) =>", successStart);

  assert.match(macosSpawn, /install_startup_patch_with_cli_fallback\(/);
  assert.match(
    macosSpawn,
    /Ok\(mode\)[\s\S]*?startup_injection_mode = mode\.as_str\(\)\.to_string\(\)[\s\S]*?performance_status = "ready"/,
  );
  assert.ok(successStart >= 0);
  assert.ok(failureStart > successStart);
  assert.doesNotMatch(
    macosSpawn.slice(successStart, failureStart),
    /stop_macos_codex|reap_child_after_cleanup|degraded/,
  );
  assert.match(
    source,
    /"launcher\.startup_compatibility_mode"[\s\S]*?"main_process_inspector_unavailable"[\s\S]*?Ok\(StartupInjectionMode::CliWrapper\)/,
  );
  assert.match(
    macosSpawn,
    /let use_inspector = !use_require && inspect_fuse\.inspector_possible\(\);/,
  );
  assert.match(
    macosSpawn,
    /let pass_inspect_brk = use_inspector \|\| !inspect_fuse\.inspector_possible\(\);/,
  );
  assert.match(
    macosSpawn,
    /let wait_inspector_port = if use_inspector \{\s*inspector_port\s*\} else \{\s*None\s*\}/,
  );
  assert.match(
    macosSpawn,
    /install_startup_patch_with_cli_fallback\(\s*wait_inspector_port,/,
  );
});

test("Codex CLI wrapper environment does not leak into the real CLI", async () => {
  const { startupPatch } = await loadSpawnCodexSections();
  assert.match(
    startupPatch,
    /for name in \[\s*"CODEX_CLI_PATH",\s*CLI_WRAPPER_TARGET_ENV,/,
  );
  assert.match(
    startupPatch,
    /for name in \[[\s\S]*?STARTUP_PATCH_MARKER_ENV,[\s\S]*?"NODE_OPTIONS",/,
  );
});

test("NODE_OPTIONS require path is preferred when the fuse is on", async () => {
  const { launcher, macosSpawn, startupPatch, windowsSpawn } = await loadSpawnCodexSections();

  assert.match(startupPatch, /pub\(crate\) const STARTUP_PATCH_MARKER_ENV/);
  assert.match(startupPatch, /CODEY_STARTUP_PATCH_MARKER/);
  assert.match(startupPatch, /fn prepare_startup_require_in\(/);
  assert.match(startupPatch, /Ok\(format!\("--require=\{rendered\}"\)\)/);
  assert.match(windowsSpawn, /detect_electron_fuses\(app_dir\.to_path_buf\(\)\)\.await/);
  assert.match(windowsSpawn, /fuses\.node_options\.node_options_possible\(\)/);
  assert.match(windowsSpawn, /let use_require = require_patch\.is_some\(\);/);
  assert.match(
    windowsSpawn,
    /let use_inspector =\s*!use_require && inspect_fuse\.inspector_possible\(\) && !retry_without_inspector;/,
  );
  assert.match(
    windowsSpawn,
    /if inspector_port\.is_none\(\) && wrapper_handshake\.is_none\(\)\s*&& require_marker\.is_none\(\)/,
  );
  assert.match(macosSpawn, /detect_electron_fuses\(app_dir\.to_path_buf\(\)\)\.await/);
  assert.match(
    macosSpawn,
    /is_app_bundle && fuses\.node_options\.node_options_possible\(\)/,
  );
  assert.doesNotMatch(
    macosSpawn,
    /!inspect_fuse\.inspector_possible\(\)\s*&&\s*fuses\.node_options\.node_options_possible\(\)/,
  );
  assert.match(macosSpawn, /add_macos_cli_wrapper\(&mut command, &require\.environment\)/);
  assert.match(
    launcher,
    /"reason": "main_process_require_unavailable"/,
  );
  assert.match(
    launcher,
    /wait_for_require_patch_with_cli_fallback\(/,
  );
  assert.match(launcher, /Ok\(StartupInjectionMode::NodeRequire\)/);
  assert.match(launcher, /Ok\(StartupInjectionMode::CliWrapper\)/);
});
