#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const semverPattern = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

function usage() {
  console.log(`Usage:
  pnpm run release -- <version> [options]

Examples:
  pnpm run release -- 0.2.1
  pnpm run release -- v0.2.1 --include-existing-changes

Options:
  --include-existing-changes  Commit all current working tree changes in the release commit.
  --skip-checks               Skip local validation commands.
  --no-push                   Create the local commit and tag, but do not push.
  --remote <name>             Git remote to push to. Defaults to origin.
  --help                      Show this help.
`);
}

function fail(message) {
  console.error(`release: ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  let version = "";
  const options = {
    includeExistingChanges: false,
    skipChecks: false,
    push: true,
    remote: "origin",
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--") continue;
    if (argument === "--help" || argument === "-h") {
      usage();
      process.exit(0);
    }
    if (argument === "--include-existing-changes") {
      options.includeExistingChanges = true;
      continue;
    }
    if (argument === "--skip-checks") {
      options.skipChecks = true;
      continue;
    }
    if (argument === "--no-push") {
      options.push = false;
      continue;
    }
    if (argument === "--remote") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) fail("Missing value for --remote");
      options.remote = value;
      index += 1;
      continue;
    }
    if (argument.startsWith("--")) fail(`Unknown option: ${argument}`);
    if (version) fail(`Unexpected extra argument: ${argument}`);
    version = argument;
  }

  if (!version) {
    usage();
    fail("Missing release version");
  }

  const normalizedVersion = version.replace(/^v/i, "");
  if (!semverPattern.test(normalizedVersion)) {
    fail(`Version must be SemVer without a v prefix: ${version}`);
  }

  return {
    ...options,
    version: normalizedVersion,
    tag: `v${normalizedVersion}`,
  };
}

function run(command, args, { capture = false } = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit",
  });
  if (result.error) fail(result.error.message);
  if (result.status !== 0) {
    if (capture && result.stderr) process.stderr.write(result.stderr);
    fail(`Command failed: ${command} ${args.join(" ")}`);
  }
  return capture ? result.stdout.trim() : "";
}

function tryRun(command, args, { capture = false } = {}) {
  return spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: capture ? ["ignore", "pipe", "pipe"] : "ignore",
  });
}

function gitOutput(args) {
  return run("git", args, { capture: true });
}

function gitStatus() {
  return gitOutput(["status", "--porcelain"]);
}

function assertGitRepository() {
  run("git", ["rev-parse", "--show-toplevel"], { capture: true });
}

function assertCleanWorkingTree(includeExistingChanges) {
  const status = gitStatus();
  if (!status || includeExistingChanges) return;
  fail(
    "Working tree is not clean. Commit your changes first, or pass --include-existing-changes to put them in the release commit.",
  );
}

function assertBranch() {
  const branch = gitOutput(["branch", "--show-current"]);
  if (!branch) fail("Cannot release from a detached HEAD");
  return branch;
}

function assertTagDoesNotExist(tag, remote, shouldCheckRemote) {
  const local = tryRun("git", ["rev-parse", "-q", "--verify", `refs/tags/${tag}`]);
  if (local.status === 0) fail(`Local tag already exists: ${tag}`);

  if (!shouldCheckRemote) return;
  const remoteTag = tryRun("git", ["ls-remote", "--tags", remote, `refs/tags/${tag}`], {
    capture: true,
  });
  if (remoteTag.status !== 0) {
    if (remoteTag.stderr) process.stderr.write(remoteTag.stderr);
    fail(`Could not check remote tag ${remote}/${tag}`);
  }
  if (remoteTag.stdout.trim()) fail(`Remote tag already exists: ${tag}`);
}

function readPackageJson() {
  const packagePath = join(root, "package.json");
  return {
    path: packagePath,
    value: JSON.parse(readFileSync(packagePath, "utf8")),
  };
}

function updatePackageJson(version) {
  const packageJson = readPackageJson();
  if (packageJson.value.version === version) return false;
  packageJson.value.version = version;
  writeFileSync(packageJson.path, `${JSON.stringify(packageJson.value, null, 2)}\n`);
  return true;
}

function cargoWorkspaceVersion() {
  const cargoToml = readFileSync(join(root, "Cargo.toml"), "utf8");
  const match = cargoToml.match(/^\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m);
  if (!match) fail("Could not find [workspace.package] version in Cargo.toml");
  return match[1];
}

function updateCargoToml(version) {
  const cargoPath = join(root, "Cargo.toml");
  const cargoToml = readFileSync(cargoPath, "utf8");
  const next = cargoToml.replace(
    /(^\[workspace\.package\][\s\S]*?^version\s*=\s*")[^"]+(")/m,
    `$1${version}$2`,
  );
  if (next === cargoToml) {
    if (cargoWorkspaceVersion() !== version) fail("Could not update Cargo.toml version");
    return false;
  }
  writeFileSync(cargoPath, next);
  return true;
}

function ensureVersionsMatch(version) {
  const packageVersion = readPackageJson().value.version;
  const cargoVersion = cargoWorkspaceVersion();
  if (packageVersion !== version) fail(`package.json version is ${packageVersion}, expected ${version}`);
  if (cargoVersion !== version) fail(`Cargo.toml version is ${cargoVersion}, expected ${version}`);
}

function hasStagedChanges() {
  const diff = tryRun("git", ["diff", "--cached", "--quiet"]);
  return diff.status === 1;
}

function runChecks() {
  run("pnpm", ["install", "--frozen-lockfile"]);
  run("pnpm", ["run", "check"]);
  run("pnpm", ["run", "test:js"]);
  run("pnpm", ["run", "vite:build"]);
  run("cargo", ["fmt", "--check"]);
  run("cargo", ["test", "--manifest-path", "Cargo.toml", "--quiet"]);
  run("git", ["diff", "--check"]);
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  assertGitRepository();
  const branch = assertBranch();
  assertCleanWorkingTree(options.includeExistingChanges);
  assertTagDoesNotExist(options.tag, options.remote, options.push);

  console.log(`Preparing ${options.tag} from ${branch}`);
  updatePackageJson(options.version);
  updateCargoToml(options.version);
  run("cargo", ["generate-lockfile"]);
  ensureVersionsMatch(options.version);

  if (!options.skipChecks) runChecks();

  if (options.includeExistingChanges) {
    run("git", ["add", "-A"]);
  } else {
    run("git", ["add", "package.json", "Cargo.toml", "Cargo.lock"]);
  }

  if (hasStagedChanges()) {
    run("git", ["commit", "-m", `release: ${options.tag}`]);
  } else {
    console.log("No version changes to commit; tagging the current HEAD.");
  }

  run("git", ["tag", "-a", options.tag, "-m", options.tag]);

  if (options.push) {
    run("git", ["push", options.remote, branch]);
    run("git", ["push", options.remote, options.tag]);
    console.log(`Published ${options.tag}. GitHub Actions will build and upload the R2 manifest.`);
  } else {
    console.log(`Created local tag ${options.tag}. Push with: git push ${options.remote} ${branch} && git push ${options.remote} ${options.tag}`);
  }
}

main();
