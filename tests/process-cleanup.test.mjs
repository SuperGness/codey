import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const normalizeLineEndings = (source) => source.replace(/\r\n/g, "\n");

test("shutdown reaps only the runtime-owned Codex process tree", async () => {
  const [library, launcher, launcherProcess, launcherPlatform, commands] =
    await Promise.all([
    readFile(new URL("../backend/src/lib.rs", import.meta.url), "utf8").then(
      normalizeLineEndings,
    ),
    readFile(
      new URL("../backend/src/launcher.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/launcher/process.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/launcher/platform.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    readFile(
      new URL("../backend/src/commands/runtime.rs", import.meta.url),
      "utf8",
    ).then(normalizeLineEndings),
    ]);
  const launcherModules = `${launcher}\n${launcherProcess}\n${launcherPlatform}`;

  const finalShutdown = library.slice(
    library.indexOf("let shutdown_reason = 'runtime: loop"),
    library.indexOf("cleanup.map_err"),
  );
  assert.match(finalShutdown, /stop_runtime_with_retry\(&state\)\.await/);
  assert.doesNotMatch(finalShutdown, /terminate_other_codey_processes/);
  assert.match(library, /#\[cfg\(test\)\]\s*mod process_cleanup;/);
  assert.doesNotMatch(
    finalShutdown,
    /if shutdown_reason == ShutdownReason::CodexExited/,
  );

  const stopWithRetry = library.slice(
    library.indexOf("async fn stop_runtime_with_retry"),
    library.indexOf("fn initial_startup_failure_error"),
  );
  assert.equal(stopWithRetry.match(/stop_codey_runtime\(state\)/g)?.length, 2);
  assert.match(stopWithRetry, /tokio::time::sleep/);

  const runtimeStop = launcher.slice(
    launcher.indexOf("pub async fn stop(&self)"),
    launcher.indexOf("fn watchdog_should_reinject"),
  );
  assert.match(runtimeStop, /stop_codex_processes/);
  assert.match(launcher, /async fn stop_codex_processes/);
  assert.match(launcher, /terminate_unix_codex_processes/);
  assert.match(launcher, /terminate_windows_codex_processes/);
  assert.match(launcherModules, /windows_terminate_process_if_matches/);
  assert.doesNotMatch(runtimeStop, /if !self\.codex_exited/);
  assert.match(launcherModules, /child_command\.process_group\(0\)/);
  assert.match(
    launcherModules,
    /let poll_delays = \[\s*Duration::from_millis\(100\),\s*Duration::from_millis\(200\),\s*Duration::from_millis\(350\),\s*Duration::from_millis\(550\),\s*Duration::from_millis\(800\),\s*\]/,
  );
  assert.match(
    runtimeStop,
    /stop_codex_processes\(\s*&self\.codex_app_path,\s*self\.process_id,[\s\S]*?self\.process_group_id/,
  );

  const stopCommand = commands.slice(
    commands.indexOf("async fn stop_codey_runtime_locked"),
    commands.indexOf("#[cfg(test)]", commands.indexOf("pub async fn stop_codey_runtime")),
  );
  assert.match(stopCommand, /state\.runtime\.lock\(\)\.await\.take\(\)/);
  assert.match(stopCommand, /\*state\.runtime\.lock\(\)\.await = Some\(runtime\)/);
  assert.match(stopCommand, /runtime_operation\.lock\(\)\.await/);
});

test("startup stops the old Codex before permanent session maintenance", async () => {
  const launcher = await readFile(
    new URL("../backend/src/launcher.rs", import.meta.url),
    "utf8",
  ).then(normalizeLineEndings);
  const startup = launcher.slice(
    launcher.indexOf("pub async fn start("),
    launcher.indexOf("pub async fn stop(&self)"),
  );
  const storagePreparation = launcher.slice(
    launcher.indexOf("async fn prepare_startup_storage("),
    launcher.indexOf("async fn prepare_runtime_provider_state("),
  );
  const providerPreparation = launcher.slice(
    launcher.indexOf("async fn prepare_runtime_provider_state("),
    launcher.indexOf("async fn prepare_startup_patches_and_overlay("),
  );
  const stopOldCodex = storagePreparation.indexOf(
    "prepare_codex_for_launch(&app_dir).await?",
  );
  const permanentMaintenance = storagePreparation.indexOf(
    "run_startup_session_maintenance",
  );
  const storagePhase = startup.indexOf("prepare_startup_storage");
  const providerPhase = startup.indexOf("prepare_runtime_provider_state");
  const protocolProxy = providerPreparation.indexOf("start_runtime_protocol_proxy");

  assert.notEqual(stopOldCodex, -1);
  assert.notEqual(permanentMaintenance, -1);
  assert.notEqual(protocolProxy, -1);
  assert.ok(
    stopOldCodex < permanentMaintenance,
    "the old Codex writer must stop before session files are maintained",
  );
  assert.ok(
    storagePhase < providerPhase,
    "the temporary runtime must not start before permanent maintenance finishes",
  );
});
