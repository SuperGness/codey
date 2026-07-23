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
  assert.doesNotMatch(manifest, /Win32_System_Console|Win32_UI_WindowsAndMessaging/);
});

test("Windows background child processes never create console windows", async () => {
  const [launcher, processCleanup] = await Promise.all([
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(new URL("../backend/src/process_cleanup.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
  ]);

  assert.equal(
    launcher.match(
      /creation_flags\(codex_plus_core::windows_create_no_window\(\)\)/g,
    )?.length,
    3,
  );
  assert.match(
    processCleanup,
    /Command::new\("taskkill"\)[\s\S]*?creation_flags\(codex_plus_core::windows_create_no_window\(\)\)/,
  );
});

test("Windows packaged Codex exit uses an OS process wait instead of polling snapshots", async () => {
  const [launcher, coreLauncher] = await Promise.all([
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8")
      .then(normalizeLineEndings),
    readFile(
      new URL(
        "../vendor/CodexPlusPlus/crates/codex-plus-core/src/launcher.rs",
        import.meta.url,
      ),
      "utf8",
    ).then(normalizeLineEndings),
  ]);
  const watcher = launcher.slice(
    launcher.indexOf("#[cfg(windows)]\nfn spawn_codex_exit_watcher"),
    launcher.indexOf("struct SpawnedCodex"),
  );

  assert.match(
    watcher,
    /codex_plus_core::launcher::wait_for_windows_process_id\(process_id\)/,
  );
  assert.doesNotMatch(watcher, /missing_streak/);
  assert.match(
    coreLauncher,
    /pub async fn wait_for_windows_process_id\(process_id: u32\)/,
  );
  assert.match(coreLauncher, /WaitForSingleObject\(handle, INFINITE\)/);
});

test("Windows updates survive shutdown through the native helper", async () => {
  const [main, commands, updateHelper] = await Promise.all([
    readFile(new URL("../backend/src/main.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(
      new URL("../backend/src/update_helper.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
  ]);

  assert.match(
    main,
    /run_update_helper_if_requested\(\)\?[\s\S]*Builder::new_multi_thread/,
  );
  assert.match(
    commands,
    /crate::update_helper::spawn_update_installer\(update_path\)/,
  );
  assert.doesNotMatch(commands, /powershell\.exe|install-codey-update\.ps1/i);
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
