import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { writeBuildOutputs } from "../scripts/build-output.mjs";

test("build outputs preserve unchanged assets and remove obsolete files and directories", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "codey-build-output-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const assets = new Map([
    ["codey-overlay.js", "overlay"],
    ["inject/bridge.js", "bridge"],
    ["old/nested/unused.js", "obsolete"],
  ]);
  writeBuildOutputs(directory, assets);
  const overlay = join(directory, "codey-overlay.js");
  const bridge = join(directory, "inject/bridge.js");
  for (const file of [overlay, bridge]) utimesSync(file, 1, 1);
  assets.delete("old/nested/unused.js");
  assets.set("inject/bridge.js", "updated bridge");
  writeBuildOutputs(directory, assets);
  assert.equal(statSync(overlay).mtimeMs, 1000);
  assert.notEqual(statSync(bridge).mtimeMs, 1000);
  assert.equal(readFileSync(bridge, "utf8"), "updated bridge");
  assert.deepEqual(readdirSync(directory).sort(), ["codey-overlay.js", "inject"]);
  writeBuildOutputs(directory, new Map());
  assert.deepEqual(readdirSync(directory), []);
});

test("build outputs replace symlinks without writing outside the output directory", {
  skip: process.platform === "win32" && "Windows symlink creation requires extra privileges",
}, (t) => {
  const parent = mkdtempSync(join(tmpdir(), "codey-build-symlink-"));
  t.after(() => rmSync(parent, { recursive: true, force: true }));
  const outside = join(parent, "outside.js");
  writeFileSync(outside, "untouched");
  const directory = join(parent, "dist");
  writeBuildOutputs(directory, new Map());
  symlinkSync(outside, join(directory, "codey-overlay.js"));
  symlinkSync(parent, join(directory, "inject"));
  writeBuildOutputs(directory, new Map([
    ["codey-overlay.js", "overlay"], ["inject/outside.js", "inject"],
  ]));
  assert.equal(readFileSync(outside, "utf8"), "untouched");
  assert.equal(readFileSync(join(directory, "codey-overlay.js"), "utf8"), "overlay");
  assert.equal(readFileSync(join(directory, "inject/outside.js"), "utf8"), "inject");
});
