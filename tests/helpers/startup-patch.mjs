import assert from "node:assert/strict";

import { readSource } from "./read-source.mjs";

// Renders backend/src/codex_startup_patch.js the way the launcher does before
// injecting it, substituting only the placeholders a test asks for so the
// remaining ones stay visible to source assertions.
export async function loadStartupPatchTemplate({
  disablePet = false,
  requireAppServerRuntimeOverrides = false,
  runtimeConfigOverrides = null,
  subagentGateActive = null,
  errorLoggerExecutable = null,
} = {}) {
  const template = await readSource("backend/src/codex_startup_patch.js");
  assert.ok(template, "startup patch template should be readable");
  let expression = template
    .replaceAll("__DISABLE_PET__", disablePet ? "true" : "false")
    .replaceAll(
      "__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__",
      requireAppServerRuntimeOverrides ? "true" : "false",
    );
  if (runtimeConfigOverrides != null) {
    const gateActive = subagentGateActive
      ?? runtimeConfigOverrides.includes("features.hooks=true");
    expression = expression
      .replaceAll(
        '"__CODEY_RUNTIME_CONFIG_OVERRIDES__"',
        JSON.stringify(runtimeConfigOverrides),
      )
      .replaceAll("__SUBAGENT_GATE_ACTIVE__", gateActive ? "true" : "false");
  }
  if (errorLoggerExecutable != null) {
    expression = expression.replaceAll(
      '"__CODEY_ERROR_LOGGER_EXECUTABLE__"',
      JSON.stringify(errorLoggerExecutable),
    );
  }
  return expression;
}

// Slices spawn_codex in backend/src/launcher/process.rs into its per-platform
// `#[cfg]` blocks so platform contracts can be asserted from any host.
export async function loadSpawnCodexSections() {
  const [launcher, launcherPlatform, startupPatch] = await Promise.all([
    readSource("backend/src/launcher/process.rs"),
    readSource("backend/src/launcher/platform.rs"),
    readSource("backend/src/codex_startup_patch.rs"),
  ]);
  const spawnStart = launcher.indexOf("async fn spawn_codex");
  const macosStart = launcher.indexOf('#[cfg(target_os = "macos")]\n    {', spawnStart);
  const otherStart = launcher.indexOf('#[cfg(not(any(windows, target_os = "macos")))]', macosStart);
  assert.ok(spawnStart >= 0 && macosStart > spawnStart && otherStart > macosStart);
  const windowsSpawn = launcher.slice(
    launcher.indexOf("#[cfg(windows)]\n    {", spawnStart),
    launcher.indexOf('#[cfg(target_os = "macos")]', spawnStart),
  );
  const macosSpawn = launcher.slice(macosStart, otherStart);
  const cleanup = launcherPlatform.slice(
    launcherPlatform.indexOf("async fn stop_windows_spawned_codex"),
    launcherPlatform.indexOf(
      '#[cfg(target_os = "macos")]\npub(super) fn build_fresh_macos_open_command',
    ),
  );
  return { cleanup, launcher, launcherPlatform, macosSpawn, startupPatch, windowsSpawn };
}
