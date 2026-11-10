import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";
import { prepareReleaseVersion, validateReleaseVersion, validateSourceVersions } from "../scripts/prepare-release-version.mjs";

const execute = promisify(execFile);
const localNames = ["codey", "codey-codex-extensions", "codey-runtime-core"];
const thirdParty = '[[package]]\nname = "third-party"\nversion = "1.2.5"\nsource = "registry+https://example.com"\nchecksum = "abc"\ndependencies = [\n "codey-runtime-core 1.2.5 (registry+https://example.com)",\n]\n\n[[package]]\nname = "codey-runtime-core"\nversion = "1.2.5"\nsource = "registry+https://example.com"\nchecksum = "def"\n';

async function fixture(handler, { cargoVersion = "1.2.5", lockVersion = "1.2.5" } = {}) {
  const root = await mkdtemp(join(tmpdir(), "codey-release-version-"));
  try {
    const files = {
      "package.json": `${JSON.stringify({ name: "codey", version: "1.2.5", scripts: { build: "unchanged" } }, null, 2)}\n`,
      "Cargo.toml": `[workspace]\nmembers = [${localNames.map((name) => `"${name}"`).join(", ")}, "sdk"]\n\n[workspace.package]\nedition = "2024"\nversion = "${cargoVersion}"\n\n[workspace.dependencies]\nthird-party = "1.2.5"\n`,
      "Cargo.lock": `version = 4\n\n${localNames.map((name) => `[[package]]\nname = "${name}"\nversion = "${lockVersion}"\ndependencies = [\n "codey-runtime-core ${lockVersion}",\n "third-party 1.2.5",\n]\n\n`).join("")}[[package]]\nname = "sdk"\nversion = "0.1.0"\n\n${thirdParty}`,
      "sdk/Cargo.toml": '[package]\nname = "sdk"\nversion = "0.1.0"\n',
      ...Object.fromEntries(localNames.map((name) => [`${name}/Cargo.toml`, `[package]\nname = "${name}"\nversion.workspace = true\n`])),
    };
    for (const [path, contents] of Object.entries(files)) {
      await mkdir(join(root, path, ".."), { recursive: true });
      await writeFile(join(root, path), contents);
    }
    const git = async (...args) => (await execute("git", args, { cwd: root })).stdout.trim();
    await git("init", "--quiet");
    await git("add", ".");
    await git("-c", "user.name=Release Test", "-c", "user.email=release@example.com", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture");
    const sha = await git("rev-parse", "HEAD");
    await handler({ root, files, git, build: { version: "2.0.0-beta.1", source_sha: sha } });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

test("release version accepts strict SemVer and rejects unsafe or ambiguous inputs", () => {
  for (const version of ["0.0.0", "2.0.0", "2.0.0-beta.1", "2.0.0-rc-1", "2.0.0-1alpha"]) assert.equal(validateReleaseVersion(version), version);
  for (const version of [undefined, "v2.0.0", "2.0.0+build", "01.0.0", "2.00.0", "2.0.0-01", "2.0.0-beta..1", "2.0.0-", "9007199254740992.0.0", "2.0.0\n", " 2.0.0 ", `2.0.0-${"a".repeat(123)}`]) assert.throws(() => validateReleaseVersion(version), /SemVer/);
});

test("source manifests must agree before injecting the chosen version", () => {
  const cargo = '[workspace.package]\nversion = "1.2.5"\n[dependencies]\nversion = "2.0.0"\n';
  assert.equal(validateSourceVersions('{"version":"1.2.5"}', cargo), "1.2.5");
  assert.throws(() => validateSourceVersions('{"version":"2.0.0"}', cargo), /必须一致/);
  assert.throws(() => validateSourceVersions('{"version":"1.2.5"}', '[workspace.package]\n[dependencies]\nversion = "1.2.5"\n'), /无法确定/);
});

test("version injection updates four inherited local packages and explicit lock references without touching dependencies", async () => {
  await fixture(async ({ root, files, git, build }) => {
    const result = await prepareReleaseVersion(build, root);
    assert.deepEqual(result, { sourceVersion: "1.2.5", version: "2.0.0-beta.1", packages: localNames });
    assert.equal(JSON.parse(await readFile(join(root, "package.json"), "utf8")).version, build.version);
    const cargo = await readFile(join(root, "Cargo.toml"), "utf8");
    assert.equal(cargo, files["Cargo.toml"].replace('version = "1.2.5"', `version = "${build.version}"`));
    const lock = await readFile(join(root, "Cargo.lock"), "utf8");
    for (const name of localNames) assert.ok(lock.includes(`name = "${name}"\nversion = "${build.version}"`));
    assert.ok(lock.includes(`"codey-runtime-core ${build.version}"`));
    assert.ok(lock.includes(thirdParty));
    assert.ok(lock.includes('name = "sdk"\nversion = "0.1.0"'));
    assert.equal((await git("diff", "--name-only")).split("\n").sort().join(","), "Cargo.lock,Cargo.toml,package.json");
    assert.equal(await git("rev-parse", "HEAD"), build.source_sha);
    await prepareReleaseVersion(build, root);
    assert.equal(await readFile(join(root, "Cargo.lock"), "utf8"), lock);
    assert.equal(await git("rev-parse", "HEAD"), build.source_sha);
  });
});

test("invalid selected version and mismatched source commit never write any file", async () => {
  await fixture(async ({ root, git, build }) => {
    await assert.rejects(prepareReleaseVersion({ ...build, version: "2.0.0+build" }, root), /SemVer/);
    await assert.rejects(prepareReleaseVersion({ ...build, source_sha: "f".repeat(40) }, root), /固定源码提交/);
    assert.equal(await git("status", "--porcelain"), "");
  });
});

test("Windows CRLF checkouts preserve their line endings and remain idempotent", async () => {
  await fixture(async ({ root, files, build }) => {
    for (const path of ["package.json", "Cargo.toml", "Cargo.lock"]) await writeFile(join(root, path), files[path].replace(/\n/g, "\r\n"));
    await prepareReleaseVersion(build, root);
    const snapshots = [];
    for (const path of ["package.json", "Cargo.toml", "Cargo.lock"]) {
      const contents = await readFile(join(root, path), "utf8");
      assert.doesNotMatch(contents, /(?<!\r)\n/);
      snapshots.push(contents);
    }
    await prepareReleaseVersion(build, root);
    for (const [index, path] of ["package.json", "Cargo.toml", "Cargo.lock"].entries()) assert.equal(await readFile(join(root, path), "utf8"), snapshots[index]);
  });
});

test("inconsistent source manifests or local lock versions fail before any writes", async () => {
  for (const options of [{ cargoVersion: "1.2.4" }, { lockVersion: "1.2.4" }]) {
    await fixture(async ({ root, git, build }) => {
      await assert.rejects(prepareReleaseVersion(build, root), /版本.*一致/);
      assert.equal(await git("status", "--porcelain"), "");
    }, options);
  }
});

test("injection refuses unrelated changes instead of overwriting a lockfile", async () => {
  await fixture(async ({ root, files, build }) => {
    const changed = files["Cargo.lock"].replace('checksum = "abc"', 'checksum = "changed"');
    await writeFile(join(root, "Cargo.lock"), changed);
    await assert.rejects(prepareReleaseVersion(build, root), /已有其他修改/);
    assert.equal(await readFile(join(root, "package.json"), "utf8"), files["package.json"]);
    assert.equal(await readFile(join(root, "Cargo.toml"), "utf8"), files["Cargo.toml"]);
    assert.equal(await readFile(join(root, "Cargo.lock"), "utf8"), changed);
  });
});
