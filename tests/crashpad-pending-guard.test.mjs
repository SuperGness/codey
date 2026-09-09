import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("Crashpad pending protection is bounded, allowlisted, and surfaced with Trace diagnostics", async () => {
  const [
    guard,
    config,
    launcher,
    commands,
    diagnosticCommands,
    runtime,
    app,
    api,
  ] = await Promise.all([
    readFile(
      new URL("../backend/src/crashpad_pending_guard.rs", import.meta.url),
      "utf8",
    ),
    readFile(new URL("../backend/src/config.rs", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/launcher.rs", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/commands.rs", import.meta.url), "utf8"),
    readFile(
      new URL("../backend/src/commands/diagnostics.rs", import.meta.url),
      "utf8",
    ),
    readFile(
      new URL("../backend/src/commands/runtime.rs", import.meta.url),
      "utf8",
    ),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/api.ts", import.meta.url), "utf8"),
  ]);

  assert.match(
    guard,
    /PENDING_HARD_LIMIT_BYTES: u64 = 512 \* 1024 \* 1024/,
  );
  assert.match(
    guard,
    /PENDING_TARGET_BYTES: u64 = 384 \* 1024 \* 1024/,
  );
  assert.match(guard, /GUARD_INTERVAL: Duration = Duration::from_secs\(5 \* 60\)/);
  assert.match(guard, /COMPLETE_REPORT_QUIET_PERIOD[\s\S]*10 \* 60/);
  assert.match(guard, /Codex\/Crashpad\/pending/);
  assert.match(guard, /com\.openai\.codex\/web\/Crashpad\/pending/);
  assert.match(guard, /strip_suffix\("\.dmp"\)/);
  assert.match(guard, /strip_suffix\("_sidecar\.json"\)/);
  assert.match(guard, /Uuid::parse_str/);
  assert.match(guard, /symlink_metadata/);
  assert.match(guard, /file_type\(\)\.is_file\(\)/);
  assert.doesNotMatch(guard, /read_dir[\s\S]{0,120}read_dir/);

  assert.match(config, /pub protect_crashpad_pending: bool/);
  assert.match(config, /protect_crashpad_pending: true/);
  assert.match(launcher, /enforce_system_limit/);
  assert.match(launcher, /spawn_crashpad_guard_watcher/);
  assert.match(commands, /"clear_diagnostic_storage"/);
  assert.match(
    diagnosticCommands,
    /clear_diagnostic_storage[\s\S]*?diagnostic_storage_operation\.lock\(\)\.await/,
  );
  assert.doesNotMatch(
    diagnosticCommands,
    /clear_diagnostic_storage[\s\S]*?diagnostic_storage_operation\.try_lock\(\)/,
  );
  assert.match(diagnosticCommands, /"status": if errors\.is_empty\(\) \{ "ok" \} else \{ "partial" \}/);
  assert.doesNotMatch(runtime, /"crashpadPendingStats"|"traceLogStats"/);

  assert.match(app, /invoke<DiagnosticStorageCleanup>\("clear_diagnostic_storage", \{ target \}\)/);
  assert.doesNotMatch(api, /"refresh_diagnostic_storage_stats"|"refresh_trace_log_stats"/);
  assert.match(api, /"clear_diagnostic_storage"/);
});
