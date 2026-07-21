import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const root = new URL("..", import.meta.url);
const manifestScript = new URL("../scripts/generate-update-manifest.mjs", import.meta.url);

const artifacts = [
  ["Codey-1.2.3-macos-arm64-unsigned.zip", "macos-arm64"],
  ["Codey-1.2.3-macos-x64-unsigned.zip", "macos-x64"],
  ["Codey-1.2.3-windows-x64-portable.zip", "windows-portable"],
  ["Codey-1.2.3-windows-x64-setup.exe", "windows-setup"],
];

test("generates a public update manifest with checksummed platform assets", async () => {
  const directory = await mkdtemp(join(tmpdir(), "codey-update-manifest-"));
  const paths = [];
  for (const [fileName, contents] of artifacts) {
    const filePath = join(directory, fileName);
    await writeFile(filePath, contents);
    paths.push(filePath);
  }

  const output = join(directory, "latest.json");
  const result = spawnSync(
    process.execPath,
    [
      manifestScript.pathname,
      "--version", "1.2.3",
      "--tag", "v1.2.3",
      "--download-base-url", "https://updates.example.com/releases/v1.2.3",
      "--output", output,
      ...paths,
    ],
    { cwd: root.pathname, encoding: "utf8" },
  );

  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(await readFile(output, "utf8"));
  assert.equal(manifest.schema_version, 1);
  assert.equal(manifest.version, "1.2.3");
  assert.equal(manifest.tag, "v1.2.3");
  assert.equal(manifest.assets.length, 4);

  const windowsInstaller = manifest.assets.find((asset) => asset.package_type === "nsis");
  assert.deepEqual(
    windowsInstaller,
    {
      platform: "windows",
      arch: "x64",
      package_type: "nsis",
      file_name: "Codey-1.2.3-windows-x64-setup.exe",
      url: "https://updates.example.com/releases/v1.2.3/Codey-1.2.3-windows-x64-setup.exe",
      sha256: createHash("sha256").update("windows-setup").digest("hex"),
      size: "windows-setup".length,
    },
  );
});

test("rejects a release whose tag does not match its version", async () => {
  const directory = await mkdtemp(join(tmpdir(), "codey-update-manifest-"));
  const result = spawnSync(
    process.execPath,
    [
      manifestScript.pathname,
      "--version", "1.2.3",
      "--tag", "v1.2.4",
      "--download-base-url", "https://example.invalid/releases/download/v1.2.4",
      "--output", join(directory, "latest.json"),
      join(directory, "placeholder"),
    ],
    { cwd: root.pathname, encoding: "utf8" },
  );

  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /must match version/);
});
