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
