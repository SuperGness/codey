import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("macOS startup patch requires app-server runtime override validation", async () => {
  const source = (
    await readFile(
      new URL("../backend/src/launcher/process.rs", import.meta.url),
      "utf8",
    )
  ).replace(/\r\n/g, "\n");
  const start = source.indexOf('#[cfg(target_os = "macos")]\n    {');
  const end = source.indexOf("#[cfg(not(any(windows, target_os = \"macos\")))]", start);
  assert.ok(start >= 0);
  assert.ok(end > start);
  const macosSpawn = source.slice(start, end);

  assert.match(macosSpawn, /install_startup_patch_with_cli_fallback\(/);
  assert.match(
    source,
    /codex_startup_patch::install\(\s*inspector_port,\s*patch_options,\s*runtime_config_overrides,\s*!runtime_config_overrides\.is_empty\(\),\s*\)/,
  );
  assert.doesNotMatch(
    source,
    /codex_startup_patch::install\(\s*inspector_port,\s*patch_options,\s*runtime_config_overrides,\s*false,\s*\)/,
  );
});
