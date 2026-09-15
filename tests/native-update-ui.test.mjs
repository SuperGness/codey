import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("startup update preflight completes before the first Codex launch", async () => {
  const library = await readFile(
    new URL("backend/src/lib.rs", root),
    "utf8",
  );
  const preflight = library.indexOf("startup_update::run(&state, &ui)");
  const installExit = library.indexOf(
    "StartupUpdateOutcome::InstallScheduled",
    preflight,
  );
  const launch = library.indexOf(
    "commands::launch_codey_runtime(&state).await",
    preflight,
  );

  assert.notEqual(preflight, -1);
  assert.ok(preflight < installExit);
  assert.ok(installExit < launch);
  assert.match(
    library.slice(installExit, launch),
    /InstallScheduled \{\s*return Ok\(\(\)\);/,
  );
});

test("Windows startup update UI uses a dedicated message loop and custom task-dialog buttons", async () => {
  const [ui, manifest, cargo] = await Promise.all([
    readFile(new URL("backend/src/native_update_ui.rs", root), "utf8"),
    readFile(new URL("backend/build.rs", root), "utf8"),
    readFile(new URL("backend/Cargo.toml", root), "utf8"),
  ]);

  assert.match(ui, /name\("codey-native-update-ui"\.to_string\(\)\)/);
  assert.match(ui, /GetMessageW\(&mut message, None, 0, 0\)/);
  assert.match(ui, /PostThreadMessageW\(self\.thread\.thread_id, WM_APP/);
  assert.match(
    ui,
    /rfd::MessageButtons::OkCancelCustom\(primary_label, secondary_label\)/,
  );
  assert.match(ui, /"更新并重启"\.to_string\(\),\s*Some\("稍后"\.to_string\(\)\)/);
  assert.match(manifest, /Microsoft\.Windows\.Common-Controls/);
  assert.match(manifest, /version="6\.0\.0\.0"/);
  assert.match(cargo, /features = \["common-controls-v6"\]/);
});

test("context recovery prompt is shared by startup, restart and model saves", async () => {
  const [library, runtime, catalogRefresh] = await Promise.all([
    readFile(new URL("backend/src/lib.rs", root), "utf8"),
    readFile(new URL("backend/src/commands/runtime.rs", root), "utf8"),
    readFile(new URL("backend/src/commands/models/catalog_refresh.rs", root), "utf8"),
  ]);

  assert.match(
    library,
    /commands::recover_default_context_budgets_for_launch\(\s*&state,?\s*\)/,
  );
  assert.match(runtime, /CUSTOM_CONTEXT_CATALOG_UNAVAILABLE/);
  assert.match(
    runtime,
    /super::recover_default_context_budgets_for_launch\(\s*&restart_state,?\s*\)/,
  );
  assert.match(
    catalogRefresh,
    /confirm\(crate::native_update_ui::ContextRecoveryPurpose::ModelSync\)/,
  );
});

test("macOS keeps AppKit on the main thread without a Dock icon", async () => {
  const [ui, build] = await Promise.all([
    readFile(new URL("backend/src/native_update_ui.rs", root), "utf8"),
    readFile(new URL("scripts/build.mjs", root), "utf8"),
  ]);

  assert.match(ui, /MainThreadMarker::new\(\)/);
  assert.match(ui, /NSApplicationActivationPolicy::Accessory/);
  assert.match(ui, /NSPanel::initWithContentRect_styleMask_backing_defer/);
  assert.match(ui, /name\("codey-runtime"\.to_string\(\)\)/);
  assert.match(ui, /app\.run\(\)/);
  assert.match(build, /<key>LSUIElement<\/key><true\/>/);
});
