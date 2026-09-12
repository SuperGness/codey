#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
if (args[0] === "--") args.shift();
if (args[0] !== "--help" && args[0] !== "-h") {
  const version = args[0]?.replace(/^v/i, "");
  if (!version || !/^\d+\.\d+\.\d+-[0-9A-Za-z.-]+(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
    console.error("Usage: pnpm run prerelease -- <version>-<identifier> [release options]");
    process.exit(1);
  }
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const result = spawnSync(process.execPath, [resolve(root, "scripts/release.mjs"), ...args], {
  cwd: root,
  stdio: "inherit",
});
process.exit(result.status ?? 1);
