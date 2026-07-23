import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

test("every shutdown path reaps Codex and Codey process trees", async () => {
  const [library, launcher, commands, cleanup, processTree] = await Promise.all([
    readFile(new URL("../backend/src/lib.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(new URL("../backend/src/process_cleanup.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(new URL("../backend/src/process_tree.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
  ]);

  const finalShutdown = library.slice(
    library.indexOf("let cleanup = match commands::stop_codey_runtime"),
    library.indexOf("cleanup.map_err"),
  );
  assert.match(finalShutdown, /terminate_other_codey_processes\(\)\.await/);
  assert.doesNotMatch(finalShutdown, /if shutdown_reason == ShutdownReason::CodexExited/);

  const runtimeStop = launcher.slice(
    launcher.indexOf("pub async fn stop(&self)"),
    launcher.indexOf("fn watchdog_should_reinject"),
  );
  assert.match(runtimeStop, /terminate_unix_codex_processes/);
  assert.match(runtimeStop, /terminate_windows_codex_processes/);
  assert.doesNotMatch(runtimeStop, /if !self\.codex_exited/);
  assert.match(launcher, /child_command\.process_group\(0\)/);
  assert.match(cleanup, /process_ids_with_descendants/);
  assert.match(processTree, /matching_process_ids/);

  const stopCommand = commands.slice(
    commands.indexOf("pub async fn stop_codey_runtime"),
    commands.indexOf("#[cfg(test)]", commands.indexOf("pub async fn stop_codey_runtime")),
  );
  assert.match(stopCommand, /runtime_slot\.take\(\)/);
  assert.match(stopCommand, /\*runtime_slot = Some\(runtime\)/);
});
