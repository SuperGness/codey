import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

test("Windows builds Codey as a GUI process without a console window", async () => {
  const [main, library, manifest] = await Promise.all([
    readFile(new URL("../backend/src/main.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(new URL("../backend/src/lib.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(new URL("../backend/Cargo.toml", import.meta.url), "utf8")
      .then(normalizeLineEndings),
  ]);

  assert.match(
    main,
    /^#!\[cfg_attr\(target_os = "windows", windows_subsystem = "windows"\)\]/,
  );
  assert.doesNotMatch(library, /hide_exclusive_windows_console|ShowWindow|GetConsoleWindow/);
  assert.doesNotMatch(manifest, /Win32_System_Console/);
  assert.match(manifest, /Win32_UI_WindowsAndMessaging/);
});

test("Windows keeps route transitions resident but shows fatal startup failures", async () => {
  const library = normalizeLineEndings(
    await readFile(new URL("../backend/src/lib.rs", import.meta.url), "utf8"),
  );
  const failureStart = library.indexOf("let shutdown_reason = 'runtime: loop");
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
  assert.match(failureBranch, /is_cc_switch_route_recovery_error\(&error\)/);
  assert.match(failureBranch, /cc_switch_route_ready_for_recovery\(\)\.await/);
  assert.match(failureBranch, /CC_SWITCH_ROUTE_RECOVERY_STABLE_READS/);
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

test("Windows background helpers never create console windows", async () => {
  const [launcherPlatform, runtimeAppPaths] = await Promise.all([
    readFile(
      new URL("../backend/src/launcher/platform.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(
      new URL(
        "../vendor/CodeyRuntime/crates/codey-runtime-core/src/app_paths.rs",
        import.meta.url,
      ),
      "utf8",
    ).then(normalizeLineEndings),
  ]);

  assert.equal(
    launcherPlatform.match(
      /creation_flags\(codey_runtime_core::windows_create_no_window\(\)\)/g,
    )?.length,
    2,
  );
  assert.match(
    launcherPlatform,
    /codey_runtime_core::windows_terminate_process_if_matches/,
  );
  assert.match(
    launcherPlatform,
    /Command::new\("taskkill"\)[\s\S]*?creation_flags\(codey_runtime_core::windows_create_no_window\(\)\)/,
  );
  assert.match(
    runtimeAppPaths,
    /Command::new\("powershell"\)\s*\.creation_flags\(crate::windows_create_no_window\(\)\)/,
  );
});

test("Windows packaged Codex exit uses an OS process wait instead of polling snapshots", async () => {
  const [launcherProcess, coreLauncher] = await Promise.all([
    readFile(new URL("../backend/src/launcher/process.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(
      new URL(
        "../vendor/CodeyRuntime/crates/codey-runtime-core/src/launcher.rs",
        import.meta.url,
      ),
      "utf8",
    ).then(normalizeLineEndings),
  ]);
  const watcher = launcherProcess.slice(
    launcherProcess.indexOf("#[cfg(windows)]\npub(super) fn spawn_codex_exit_watcher"),
    launcherProcess.indexOf("struct SpawnedCodex"),
  );

  assert.match(
    watcher,
    /codey_runtime_core::launcher::wait_for_windows_process_id\(process_id\)/,
  );
  assert.doesNotMatch(watcher, /missing_streak/);
  assert.match(
    coreLauncher,
    /pub async fn wait_for_windows_process_id\(process_id: u32\)/,
  );
  assert.match(coreLauncher, /WaitForSingleObject\(handle, INFINITE\)/);
});

test("Windows updates survive shutdown through the native helper", async () => {
  const [main, updates, updateHelper] = await Promise.all([
    readFile(new URL("../backend/src/main.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(
      new URL("../backend/src/commands/updates.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/update_helper.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
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

test("Windows missing Codex paths are recovered before Renderer startup", async () => {
  const [commands, runtime, api, app] = await Promise.all([
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/commands/runtime.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(new URL("../src/api.ts", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8")
      .then(normalizeLineEndings),
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
  assert.doesNotMatch(api, /pick_codex_app_directory|set_codex_app_path/);
  assert.doesNotMatch(app, /CodexAppPathDialog|codexAppPathSelectionRequired/);
});
