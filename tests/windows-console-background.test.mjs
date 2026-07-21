import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("Windows hides Codey's exclusive console only after Codex starts", async () => {
  const source = await readFile(
    new URL("../backend/src/lib.rs", import.meta.url),
    "utf8",
  );

  assert.match(
    source,
    /match commands::launch_codey_runtime\(&state\)\.await \{\s*Ok\(_\) => hide_exclusive_windows_console\(\),/,
  );
  assert.match(source, /GetConsoleProcessList/);
  assert.match(source, /if process_count == 1/);
  assert.match(source, /ShowWindow\(console_window, SW_HIDE\)/);
});

test("Windows packaged Codex exit uses an OS process wait instead of polling snapshots", async () => {
  const [launcher, coreLauncher] = await Promise.all([
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8"),
    readFile(
      new URL(
        "../vendor/CodexPlusPlus/crates/codex-plus-core/src/launcher.rs",
        import.meta.url,
      ),
      "utf8",
    ),
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
