import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";


// Static contracts keep Windows-only wiring visible on non-Windows CI jobs. Runtime
// behavior remains covered by Rust tests and the dedicated Windows build job.
test("Windows source contract: Codey uses the GUI subsystem", async () => {
  const [main, library, manifest] = await Promise.all([
    readSource("backend/src/main.rs"),
    readSource("backend/src/lib.rs"),
    readSource("backend/Cargo.toml"),
  ]);

  assert.match(
    main,
    /^#!\[cfg_attr\(target_os = "windows", windows_subsystem = "windows"\)\]/,
  );
  assert.doesNotMatch(library, /ShowWindow|GetConsoleWindow/);
  assert.doesNotMatch(manifest, /Win32_System_Console/);
  assert.match(manifest, /Win32_UI_WindowsAndMessaging/);
});

test("Windows source contract: fatal startup failures remain visible", async () => {
  const library = await readSource("backend/src/lib.rs");
  const failureStart = library.indexOf("let shutdown_reason = match");
  const fatalCleanup = library.indexOf(
    "let cleanup = stop_runtime_with_retry(&state).await;",
    failureStart,
  );
  const shutdownCleanup = library.indexOf(
    "let cleanup = stop_runtime_with_retry(&state).await;",
    fatalCleanup + 1,
  );

  assert.notEqual(failureStart, -1);
  assert.notEqual(fatalCleanup, -1);
  assert.notEqual(shutdownCleanup, -1);

  const failureBranch = library.slice(failureStart, shutdownCleanup);
  assert.match(failureBranch, /commands::launch_codey_runtime\(&state\)\.await/);
  assert.match(failureBranch, /stop_runtime_with_retry\(&state\)\.await/);
  assert.match(failureBranch, /show_initial_startup_failure\(&error\)\.await/);
  assert.match(failureBranch, /return Err\(/);

  const cleanupHelper = library.slice(
    library.indexOf("async fn stop_runtime_with_retry"),
    library.indexOf("fn initial_startup_failure_error"),
  );
  assert.match(cleanupHelper, /stop_codey_runtime\(state\)\.await/);
  assert.match(cleanupHelper, /tokio::time::sleep/);
  assert.equal(cleanupHelper.match(/stop_codey_runtime\(state\)/g)?.length, 2);

  assert.match(
    library,
    /rfd::MessageDialog::new\(\)[\s\S]*?MessageLevel::Error[\s\S]*?MessageButtons::Ok[\s\S]*?\.show\(\)/,
  );
  assert.match(library, /tokio::task::spawn_blocking/);
  assert.match(library, /\.set_title\("Codey 启动失败"\)/);
  assert.match(library, /Codey 将退出。处理上述问题后，请重新启动 Codey。/);
});

test("Windows source contract: background helpers request no-window execution", async () => {
  const [launcherPlatform, processCleanup, runtimeAppPaths] = await Promise.all([
    readSource("backend/src/launcher/platform.rs"),
    readSource("backend/src/process_cleanup.rs"),
    readSource("vendor/CodeyRuntime/crates/codey-runtime-core/src/app_paths.rs"),
  ]);

  assert.match(
    launcherPlatform,
    /Command::new\(executable\)[\s\S]*creation_flags\(codey_runtime_core::windows_create_no_window\(\)\)[\s\S]*\.spawn\(\)/,
  );
  assert.doesNotMatch(processCleanup, /Command::new\("taskkill"\)/);
  assert.match(
    processCleanup,
    /codey_runtime_core::windows_terminate_process_if_matches/,
  );
  assert.match(
    runtimeAppPaths,
    /Command::new\("powershell"\)\s*\.creation_flags\(crate::windows_create_no_window\(\)\)/,
  );
});

test("Windows source contract: packaged Codex exit uses an OS process wait", async () => {
  const [launcherProcess, coreLauncher] = await Promise.all([
    readSource("backend/src/launcher/process.rs"),
    readSource("vendor/CodeyRuntime/crates/codey-runtime-core/src/launcher.rs"),
  ]);
  const watcher = launcherProcess.slice(
    launcherProcess.indexOf("#[cfg(windows)]\npub(super) fn spawn_codex_exit_watcher"),
    launcherProcess.indexOf("struct SpawnedCodex"),
  );

  assert.match(
    watcher,
    /codey_runtime_core::launcher::wait_for_windows_process_id\(process_id\)/,
  );
  assert.match(
    coreLauncher,
    /pub async fn wait_for_windows_process_id\(process_id: u32\)/,
  );
  assert.match(coreLauncher, /WaitForSingleObject\(handle, INFINITE\)/);
});

test("Windows source contract: updates use the detached native helper", async () => {
  const [main, updates, updateHelper] = await Promise.all([
    readSource("backend/src/main.rs"),
    readSource("backend/src/commands/updates.rs"),
    readSource("backend/src/update_helper.rs"),
  ]);

  assert.match(
    main,
    /run_update_helper_if_requested\(\)\?[\s\S]*run_desktop_application\(\)/,
  );
  assert.match(
    updates,
    /crate::update_helper::spawn_update_installer\(update_path, asset\.size, &asset\.sha256\)/,
  );
  assert.doesNotMatch(updates, /powershell\.exe|install-codey-update\.ps1/i);
  assert.match(
    updateHelper,
    /std::fs::copy\(&executable, &helper_path\)[\s\S]*Command::new\(&helper_path\)/,
  );
  assert.match(
    updateHelper,
    /let install_result = install_windows_update[\s\S]*let restart_result = restart_codey/,
  );
  assert.match(updateHelper, /raw_arg\(nsis_install_directory_argument/);
});

test("Windows source contract: missing Codex paths recover before startup", async () => {
  const [commands, runtime] = await Promise.all([
    readSource("backend/src/commands.rs"),
    readSource("backend/src/commands/runtime.rs"),
  ]);
  const launch = runtime.slice(
    runtime.indexOf("async fn launch_codey_inner_locked"),
    runtime.indexOf("pub async fn launch_codey_runtime"),
  );

  assert.match(launch, /ensure_windows_codex_app_path\(state\)\.await\?/);
  assert.ok(
    launch.indexOf("ensure_windows_codex_app_path(state).await?")
      < launch.indexOf("CodeyRuntime::start"),
  );
  assert.match(
    commands,
    /FileDialog::new\(\)[\s\S]*选择 Codex 桌面应用安装目录[\s\S]*pick_folder\(\)/,
  );
  assert.match(commands, /save_config_to_store\(state, &config\)/);
});
