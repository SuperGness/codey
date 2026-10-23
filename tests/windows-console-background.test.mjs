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
  // 插件日志子进程可按需创建控制台，桌面入口仍应直接使用 GUI 子系统。
  assert.doesNotMatch(main, /AllocConsole|AttachConsole|ShowWindow|GetConsoleWindow/);
  assert.doesNotMatch(library, /AllocConsole|AttachConsole|ShowWindow|GetConsoleWindow/);
  assert.match(manifest, /Win32_UI_WindowsAndMessaging/);
});

test("Windows source contract: fatal startup failures remain visible", async () => {
  const library = await readSource("backend/src/lib.rs");
  const failureStart = library.indexOf("let shutdown_reason =");
  const shutdownCleanup = library.indexOf(
    "let cleanup = stop_runtime_with_retry(&state).await;",
    failureStart,
  );

  assert.notEqual(failureStart, -1);
  assert.ok(shutdownCleanup > failureStart);

  const failureBranch = library.slice(failureStart, shutdownCleanup);
  assert.match(failureBranch, /commands::launch_codey_runtime\(&state\)\.await/);
  assert.match(failureBranch, /stop_runtime_with_retry\(&state\)\.await/);
  assert.match(failureBranch, /finish_failed_startup\(/);
  assert.match(
    failureBranch,
    /if let Err\(error\) = &result \{\s*show_initial_startup_failure\(error\)\.await;/,
  );
  assert.match(
    failureBranch,
    /return result\s*\.map\(\|reason\| lifecycle\.shutdown_requested\(reason\)\)\s*\.map_err\(anyhow::Error::msg\)/,
  );

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
  const [launcherPlatform, processCleanup, runtimeAppPaths, runtimeWindowsPackages] = await Promise.all([
    readSource("backend/src/launcher/platform.rs"),
    readSource("backend/src/process_cleanup.rs"),
    readSource("vendor/CodeyRuntime/crates/codey-runtime-core/src/app_paths.rs"),
    readSource("vendor/CodeyRuntime/crates/codey-runtime-core/src/app_paths/windows_packages.rs"),
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
  assert.match(runtimeAppPaths, /packages: windows_packages::NativeWindowsPackageSnapshot/);
  assert.match(
    runtimeAppPaths,
    /fn registered_codex_app_dirs\(\)[\s\S]*?\.packages\s*\.codex_packages\(\)/,
  );
  assert.match(
    runtimeWindowsPackages,
    /impl Default for NativeWindowsPackageSnapshot \{[\s\S]*?Self::new\(enumerate_registered_windows_packages\)/,
  );
  const nativePackages = runtimeWindowsPackages.match(
    /#\[cfg\(windows\)\]\s*fn enumerate_registered_windows_packages\(\)[\s\S]*?\n\}/,
  )?.[0];
  assert.ok(nativePackages, "Windows 包发现应使用原生枚举");
  assert.match(nativePackages, /PackageManager::new\(\)/);
  assert.match(nativePackages, /FindPackagesByUserSecurityId\(&HSTRING::new\(\)\)/);
  assert.match(nativePackages, /packages\.First\(\)\.context\([^\n]+\)\?/);
  assert.match(nativePackages, /\.MoveNext\(\)\s*\.context\([^\n]+\)\?/);
  assert.doesNotMatch(nativePackages, /Command::new|CreateProcess|ShellExecute/);
  for (const source of [runtimeAppPaths, runtimeWindowsPackages]) {
    assert.doesNotMatch(
      source,
      /Command::new\(\s*"(?:powershell|pwsh|cmd)(?:\.exe)?"/i,
    );
  }
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
    /codey_runtime_core::launcher::wait_for_windows_process_exit\(process_id\)/,
  );
  assert.match(
    coreLauncher,
    /pub async fn wait_for_windows_process_exit\(process_id: u32\) -> anyhow::Result<Option<u32>>/,
  );
  assert.match(watcher, /Ok\(code\) => \{\s*exit_code = code;\s*true/);
  assert.match(coreLauncher, /WaitForSingleObject\(HANDLE\(handle\.as_raw_handle\(\)\), 0\)/);
  assert.match(coreLauncher, /OwnedHandle/);
  assert.doesNotMatch(coreLauncher, /WaitForSingleObject\([^\n]*INFINITE\)/);
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
  // 安装结果必须先复核，再决定是否重启：静默 NSIS 失败时盲目重启只会让用户
  // 反复回到旧版本。
  assert.match(
    updateHelper,
    /match outcome \{[\s\S]*?Ok\(UpdateInstallOutcome::Updated\s*\|\s*UpdateInstallOutcome::Failed\)[\s\S]*?restart_codey\(invocation, &log_path\)/,
  );
  assert.match(
    updateHelper,
    /Ok\(UpdateInstallOutcome::Unverified\) => \{[\s\S]*?restart_codey\(invocation, &log_path\)/,
  );
  assert.match(
    updateHelper,
    /Err\(install_error\) => \{[\s\S]*?finish_update_report\(&report_path, &report_version, "failed", &install_error\)[\s\S]*?Err\(install_error\)/,
  );
  assert.doesNotMatch(
    updateHelper,
    /let install_result = install_windows_update/,
    "安装失败后不得再无条件重启旧版本",
  );
  assert.match(updateHelper, /raw_arg\(nsis_install_directory_argument/);
  assert.match(
    updateHelper,
    /let outcome = verify_installed_update\(invocation, expected_version\.as_deref\(\), before\)/,
  );
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

  const recoverBeforeLaunch = launch.match(
    /ensure_windows_codex_app_path\(state\)\s*\.await\s*\.map_err\(anyhow::Error::msg\)\?/,
  );
  assert.ok(recoverBeforeLaunch, "启动前必须恢复 Codex 路径并传播失败");
  assert.ok(
    recoverBeforeLaunch.index < launch.indexOf("CodeyRuntime::start"),
  );
  assert.match(
    commands,
    /FileDialog::new\(\)[\s\S]*选择 Codex 桌面应用安装目录[\s\S]*pick_folder\(\)/,
  );
  const recoverPath = commands.match(
    /async fn ensure_windows_codex_app_path\([\s\S]*?\n\}/,
  )?.[0];
  assert.ok(recoverPath, "应存在 Windows Codex 路径恢复函数");
  assert.match(recoverPath, /config\.codex_app_path = app_dir\.to_string_lossy\(\)\.to_string\(\)/);
  assert.match(
    recoverPath,
    /let config = save_config_to_store\(state, config\)\s*\.await\s*\.map_err\([^\n]+\)\?;\s*\*state\.config\.write\(\)\.await = config;/,
  );
});
