import assert from "node:assert/strict";
import test from "node:test";

import { loadSpawnCodexSections } from "./helpers/startup-patch.mjs";
import { readSource } from "./helpers/read-source.mjs";

// Static contracts keep both platforms' spawn_codex wiring visible on any CI host.
test("Windows startup compatibility failure cleans the process before compatible restart", async () => {
  const { cleanup, windowsSpawn } = await loadSpawnCodexSections();
  const cleanupCall = windowsSpawn.indexOf(
    "stop_windows_spawned_codex(&mut spawned, app_dir).await",
  );
  const compatibleRestart = windowsSpawn.indexOf(
    "match spawn_windows_codex(",
    cleanupCall,
  );

  assert.ok(cleanupCall >= 0);
  assert.ok(compatibleRestart > cleanupCall);
  assert.match(
    windowsSpawn.slice(compatibleRestart, compatibleRestart + 500),
    /&runtime_arguments,\s*launch_proxy_environment\.as_deref\(\)\.unwrap_or\(&\[\]\),\s*options\.launch_proxy\.is_some\(\)/,
  );
  const singleInstanceCleanup = windowsSpawn.indexOf("if single_instance_exit {", cleanupCall);
  assert.ok(singleInstanceCleanup > cleanupCall && singleInstanceCleanup < compatibleRestart);
  assert.match(
    windowsSpawn.slice(singleInstanceCleanup, compatibleRestart),
    /Err\(sweep_error\) => anyhow::bail!\(/,
  );
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

test("the CLI wrapper is the only main-process compatibility entry", async () => {
  const { launcher, macosSpawn, windowsSpawn } = await loadSpawnCodexSections();

  // Electron fuses disabled NODE_OPTIONS and --inspect; neither may appear in
  // any launch path, and no fuse probe decides startup wiring anymore.
  for (const section of [windowsSpawn, macosSpawn]) {
    const code = section.replaceAll(/\/\/[^\n]*/g, "");
    assert.doesNotMatch(code, /electron_fuses|NODE_OPTIONS|--inspect|StartupRequire|inspector/i);
    assert.match(section, /prepare_cli_wrapper\(/);
    assert.match(section, /confirm_cli_wrapper_startup\(/);
    assert.match(section, /STARTUP_CLI_READY_TIMEOUT/);
  }
  assert.doesNotMatch(launcher, /StartupInjectionMode|inspector_argument|require_marker/);

  // Windows: a wrapper that cannot even be prepared never launches a
  // constrained Codex; it degrades through the dedicated no-entry path.
  const loop = windowsSpawn.indexOf("loop {");
  const prepare = windowsSpawn.indexOf("prepare_cli_wrapper(");
  const noEntry = windowsSpawn.indexOf("launch_windows_codex_without_compatibility(");
  const refresh = windowsSpawn.indexOf("refresh_windows_packaged_app_dir(app_dir)");
  const validate = windowsSpawn.indexOf("validate_codex_app_dir(app_dir)");
  const launch = windowsSpawn.indexOf("spawn_windows_codex(", noEntry);
  const budget = windowsSpawn.indexOf("let deadline =");
  const cleanup = windowsSpawn.indexOf("if let Err(cleanup_error) =");
  const retry = windowsSpawn.indexOf("if should_retry_startup(&error, attempt) {");
  const requiredConfigGuard = windowsSpawn.indexOf("if !runtime_config_overrides.is_empty() {");

  assert.ok(loop >= 0 && loop < refresh && refresh < validate && validate < prepare);
  assert.ok(prepare < noEntry && noEntry < launch);
  assert.match(windowsSpawn, /wrapper_environment_applied\.then\(\|\| wrapper\.into_handshake\(\)\)/);
  // Each attempt gets its own readiness budget, taken after activation.
  assert.ok(budget > launch && budget < cleanup);
  assert.ok(cleanup > launch && retry > cleanup);
  assert.ok(requiredConfigGuard > retry);
  assert.match(windowsSpawn.slice(cleanup, retry), /if let Err\(cleanup_error\)[\s\S]*?anyhow::bail!/);
  // A wrapper handshake that never applied falls back to a degraded run.
  assert.match(windowsSpawn, /let Some\(wrapper_handshake\) = wrapper_handshake else/);
  assert.match(windowsSpawn, /spawned\.startup_injection_mode = "cli"\.to_string\(\)/);

  // Exit code 0 may indicate a different installation owns the lock. The
  // selected-installation guard runs after cleanup and before the retry.
  const singleInstanceSweep = windowsSpawn.indexOf(
    "stop_running_windows_codex_instances(app_dir).await",
  );
  assert.ok(singleInstanceSweep > cleanup && singleInstanceSweep < retry);
  assert.match(windowsSpawn, /exited\.exit_code == Some\(0\)/);
  assert.match(windowsSpawn, /return Ok\(spawned\);/);

  // Missing entries never launch a constrained Codex.
  const noEntryLaunch = launcher.slice(
    launcher.indexOf("async fn launch_windows_codex_without_compatibility"),
    launcher.indexOf("async fn spawned_codex_alive"),
  );
  const overrideGuard = noEntryLaunch.indexOf("if !runtime_config_overrides.is_empty() {");
  const compatibilityGuard = noEntryLaunch.indexOf("if compatibility_required {");
  const degradedLaunch = noEntryLaunch.indexOf("spawn_windows_codex(");
  assert.ok(overrideGuard >= 0 && overrideGuard < compatibilityGuard && compatibilityGuard < degradedLaunch);
  assert.match(
    windowsSpawn.slice(noEntry, launch),
    /subagent_gate_active \|\| misc_model\.is_some\(\)/,
  );
  assert.match(
    noEntryLaunch.slice(degradedLaunch, degradedLaunch + 400),
    /runtime_arguments,\s*launch_proxy_environment\.unwrap_or\(&\[\]\),\s*launch_proxy_environment\.is_some\(\)/,
  );
  assert.match(noEntryLaunch, /performance_status = "degraded"/);

  // macOS: the wrapper environment rides the `open --env` channel for .app
  // bundles and the spawn environment otherwise.
  const macosPrepare = macosSpawn.indexOf("prepare_cli_wrapper(");
  const bundleCommand = macosSpawn.indexOf("build_fresh_macos_open_command(");
  const applyEnvironment = macosSpawn.indexOf("add_macos_launch_environment(&mut command, &wrapper.environment)");
  const confirm = macosSpawn.indexOf("confirm_cli_wrapper_startup(");
  assert.ok(macosPrepare >= 0 && bundleCommand > macosPrepare && applyEnvironment > bundleCommand && confirm > applyEnvironment);
  assert.match(macosSpawn, /spawned\.startup_injection_mode = "cli"\.to_string\(\)/);
  assert.match(macosSpawn, /spawned\.performance_status = "ready"\.to_string\(\)/);
});

test("Startup waits end on process exit, marker confirmation or handshake", async () => {
  const { launcher, startupPatch } = await loadSpawnCodexSections();

  assert.match(
    launcher,
    /tokio::select! \{\s*exited = exited => Err\(exited\.into\(\)\),\s*result = ready => result,\s*\}/,
  );
  assert.match(launcher, /error\.is::<crate::codex_startup_patch::StartupProcessExited>\(\)/);
  assert.match(launcher, /let marker = watch_cli_wrapper_marker\(&marker_path\);/);
  assert.match(launcher, /CLI_WRAPPER_MARKER_ENV\.to_string\(\)/);
  assert.match(launcher, /prune_cli_wrapper_markers\(/);

  // The wrapper retries a slow loopback connect but never a refused one, and
  // records its progress in the marker file as a second channel.
  assert.match(startupPatch, /fn connect_loopback_with_retry\(/);
  assert.match(
    startupPatch,
    /Err\(error\) if error\.kind\(\) == std::io::ErrorKind::ConnectionRefused => \{\s*return Err\(error\);/,
  );
  assert.match(startupPatch, /readiness\.mark\(CliWrapperMarkerStatus::Launching, None\)/);
  assert.match(
    startupPatch,
    /fn executed\(self\) \{\s*self\.finish_with\(None, connect_cli_wrapper_handshake\);/,
  );
  assert.match(
    startupPatch,
    /let marker_written = match failure \{\s*Some\(failure\) => self\.mark\(CliWrapperMarkerStatus::Failed, Some\(failure\)\),\s*None => self\.mark_executed\(\),\s*\};/,
  );
  assert.match(
    startupPatch,
    /fn mark_executed\(&self\) -> bool \{\s*self\.mark\(CliWrapperMarkerStatus::Executed, None\)/,
  );
});

test("macOS startup patch requires app-server runtime override validation", async () => {
  const { macosSpawn } = await loadSpawnCodexSections();
  const successStart = macosSpawn.indexOf("Ok(()) =>");
  const failureStart = macosSpawn.indexOf("Err(error) =>", successStart);

  assert.match(macosSpawn, /confirm_cli_wrapper_startup\(/);
  assert.ok(successStart >= 0);
  assert.ok(failureStart > successStart);
  assert.doesNotMatch(
    macosSpawn.slice(successStart, failureStart),
    /terminate_unix_codex_processes|reap_child_after_cleanup|degraded/,
  );
  // On failure the spawned tree is swept before the launch reports an error;
  // the remote-debugging port argument marks our Electron instance under `open`.
  assert.match(
    macosSpawn.slice(failureStart),
    /let launch_marker = format!\("--remote-debugging-port=\{debug_port\}"\)/,
  );
  assert.match(
    macosSpawn.slice(failureStart),
    /terminate_unix_codex_processes\(\s*app_dir,\s*spawned\.process_id,\s*spawned\.process_group_id,\s*Some\(launch_marker\.as_str\(\)\),?\s*\)/,
  );
});

test("Codex CLI wrapper environment does not leak into the real CLI", async () => {
  const { startupPatch } = await loadSpawnCodexSections();
  assert.match(
    startupPatch,
    /for name in \[\s*"CODEX_CLI_PATH",\s*CLI_WRAPPER_TARGET_ENV,\s*CLI_WRAPPER_SOURCE_ENV,/,
  );
  assert.match(
    startupPatch,
    /for name in \[[\s\S]*?CLI_WRAPPER_MARKER_ENV,[\s\S]*?"NODE_OPTIONS",/,
  );
});

test("CLI relay provenance records the discovered source before Windows runtime staging", async () => {
  const { launcher, startupPatch } = await loadSpawnCodexSections();
  assert.match(startupPatch, /const CLI_WRAPPER_SOURCE_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_SOURCE"/);
  assert.match(launcher, /windows_cli_wrapper_target_from_source\(&app_dir, &source\)/);
  assert.match(launcher, /CLI_WRAPPER_SOURCE_ENV\.to_string\(\),\s*source\.to_string_lossy\(\)\.to_string\(\)/);
});

test("Store environment uses supervised native activation with scoped cleanup", async () => {
  const { launcherPlatform } = await loadSpawnCodexSections();
  const source = await readSource("backend/src/launcher/windows_packaged.rs");
  const supervisor = await readSource("backend/src/launcher/windows_activation.rs");
  assert.doesNotMatch(source, /PACKAGE_NAME_ATTRIBUTE|STARTUPINFOEXW|CreateProcessW|UpdateProcThreadAttribute/);
  assert.match(source, /settings\.EnableDebugging\(/);
  assert.match(source, /settings\s*\.DisableDebugging\(/);
  assert.match(source, /impl Drop for WindowsPackageDebugSession[\s\S]*?journal\.recover\(disable_windows_packaged_environment\)/);
  const prepare = source.indexOf("session.journal.arm(&package_full_name)?");
  const enable = source.indexOf("enable_windows_packaged_environment(", prepare);
  assert.ok(prepare >= 0 && enable > prepare);
  const setup = launcherPlatform.indexOf("WindowsPackageDebugSession::start(");
  const supervision = launcherPlatform.indexOf("tokio::spawn(async move", setup);
  const activation = launcherPlatform.indexOf("codey_runtime_core::launcher::activate_packaged_app(", supervision);
  const verification = launcherPlatform.indexOf("let package_check", activation);
  const resumed = launcherPlatform.indexOf("wait_for_resume(feedback.as_deref(), process_id).await?", verification);
  const spawned = launcherPlatform.indexOf("SpawnedCodex {", verification);
  const cleanup = launcherPlatform.indexOf("package_debug_session.finish()", verification);
  assert.ok(setup >= 0 && supervision > setup && activation > supervision && verification > activation && resumed > verification && spawned > resumed && cleanup > spawned);
  assert.match(source, /if self\.activation_pending/);
  assert.match(launcherPlatform, /wait_for_resume_failure\(feedback\.clone\(\)\)/);
  assert.match(launcherPlatform, /cancel_resume_feedback\(feedback\.as_deref\(\)\)/);
  assert.match(supervisor, /timeout\(self\.timeout, &mut activation\)/);
  assert.match(supervisor, /timeout\(self\.settle_timeout, &mut activation\)/);
  assert.match(supervisor, /result = &mut activation => break result/);
  assert.match(supervisor, /let stopped = stop\(\)\.await;[\s\S]*?let cleared = finish\(\);/);
  assert.match(launcherPlatform, /!environment_required && error\.is::<recovery::IntegrationFailure>\(\)/);
});

test("Store environment failures identify each native operation without logging values", async () => {
  const source = await readSource("backend/src/launcher/windows_packaged.rs");
  const setupStart = source.indexOf("fn with_windows_package_debug_settings<T>");
  const enableStart = source.indexOf("fn enable_windows_packaged_environment(", setupStart);
  const disableStart = source.indexOf("fn disable_windows_packaged_environment(", enableStart);
  assert.ok(setupStart >= 0 && enableStart > setupStart && disableStart > enableStart);
  const setup = source.slice(setupStart, enableStart);
  const enable = source.slice(enableStart, disableStart);
  assert.match(setup, /初始化 Windows 包调试 COM 环境失败（CoInitializeEx）/);
  assert.match(setup, /创建 Windows 包调试设置接口失败（CoCreateInstance）/);
  assert.match(enable, /\.DisableDebugging\(package\)\s*\.context\("安装兼容环境前清理包调试设置失败/);
  assert.match(enable, /EnableDebugging；环境块 UTF-16 长度=\{\}，恢复命令 UTF-16 长度=\{\}/);
  assert.match(source.slice(disableStart), /移除包调试设置失败（IPackageDebugSettings::DisableDebugging）/);

  const failureStart = source.indexOf("let detail = format!(\"{error:#}\");");
  const cleanupStart = source.indexOf("let cleared = session.finish();", failureStart);
  assert.ok(failureStart >= 0 && cleanupStart > failureStart);
  const failure = source.slice(failureStart, cleanupStart);
  assert.match(failure, /crate::error_log::record_failure\(\s*"package_environment_failed",\s*"enable_windows_packaged_environment"/);
  assert.match(failure, /"environmentVariableNames": environment\.iter\(\)\s*\.map\(\|\(name, _\)\| name\.as_str\(\)\)/);
  assert.match(failure, /error\.downcast_ref::<windows::core::Error>\(\)/);
  assert.doesNotMatch(failure, /"environment"\s*:|"environmentValues"\s*:/);
});

test("Store debugger resumes only a verified Codex process and its own thread", async () => {
  const source = await readSource("backend/src/launcher/windows_packaged.rs");
  const { startupPatch } = await loadSpawnCodexSections();
  const ownership = source.indexOf("owner != 0 && owner == process_id");
  const packageCheck = source.indexOf("validate_package_record(&package)?", ownership);
  const registered = source.indexOf("refresh_windows_packaged_app_dir", packageCheck);
  const image = source.indexOf("QueryFullProcessImageNameW(", registered);
  const verifiedImage = source.indexOf("normalized_windows_path(&image)", image);
  const resumed = source.indexOf("ResumeThread(thread_handle)", verifiedImage);
  assert.ok(ownership >= 0 && packageCheck > ownership && registered > packageCheck && image > registered && verifiedImage > image && resumed > verifiedImage);
  const confirmation = source.indexOf("serde_json::to_vec(&ResumeFeedbackState::Resumed { process_id })", resumed);
  assert.ok(confirmation > resumed);
  assert.match(startupPatch, /run_windows_package_resume_helper_if_requested\(\)\?/);
  assert.match(startupPatch, /resume_windows_packaged_thread\(\s*process_id,\s*thread_id,\s*launch_id,\s*&feedback_path,?\s*\)/);
  assert.match(source.slice(verifiedImage, resumed), /require_pending_resume\(feedback\)\?/);
  assert.match(source, /--launch-state/);
  assert.match(startupPatch, /target\.is_none\(\)/);
});

test("native recovery shares Store activation and preserves the configured home", async () => {
  const source = await readSource("backend/src/launcher/recovery.rs");
  assert.doesNotMatch(source, /spawn_with_environment|CreateProcessW/);
  assert.match(source, /windows_codex_launch_environment\(&\[\], home\)/);
  assert.match(source, /activate_windows_codex\(\s*app_dir,\s*&app_id,\s*"",\s*required\.then_some\(environment\.as_slice\(\)\),\s*required/);
});
