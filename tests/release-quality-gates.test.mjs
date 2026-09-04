import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const workflow = fs.readFileSync(
  new URL("../.github/workflows/build-desktop.yml", import.meta.url),
  "utf8",
);
const ciWorkflow = fs.readFileSync(
  new URL("../.github/workflows/ci.yml", import.meta.url),
  "utf8",
);
const macBuildScript = fs.readFileSync(
  new URL("../scripts/build.mjs", import.meta.url),
  "utf8",
);
const updateSource = fs.readFileSync(
  new URL("../backend/src/commands/updates.rs", import.meta.url),
  "utf8",
);
const windowsInstallerScript = fs.readFileSync(
  new URL("../scripts/installer/windows/Codey.nsi", import.meta.url),
  "utf8",
);

function assertRustQualityGates(job) {
  assert.match(job, /components: rustfmt, clippy/);
  assert.match(job, /cargo fmt --all -- --check/);
  assert.match(job, /cargo test --workspace --locked/);
  assert.match(job, /cargo clippy --workspace --all-targets --locked -- -D warnings/);
}

function workflowStep(from, to) {
  const fromIndex = workflow.indexOf(from);
  assert.notEqual(fromIndex, -1);
  const toIndex = workflow.indexOf(to, fromIndex);
  assert.notEqual(toIndex, -1);
  return workflow.slice(fromIndex, toIndex);
}

test("pull requests enforce the unified Rust quality gate", () => {
  assert.match(ciWorkflow, /^\s*RUSTFLAGS: -D warnings$/m);
  assertRustQualityGates(ciWorkflow);
  const windowsJob = ciWorkflow.slice(ciWorkflow.indexOf("\n  windows-rust:"));
  assert.match(windowsJob, /runs-on: windows-latest/);
  assert.match(windowsJob, /components: clippy/);
  assert.match(windowsJob, /cargo test --workspace --locked/);
  assert.match(
    windowsJob,
    /cargo clippy --workspace --all-targets --locked -- -D warnings/,
  );
});

test("tag-triggered desktop releases independently enforce Rust quality gates", () => {
  assert.match(workflow, /^\s*RUSTFLAGS: -D warnings$/m);
  const macosJob = workflow.slice(
    workflow.indexOf("\n  macos:"),
    workflow.indexOf("\n  windows:"),
  );
  const windowsJob = workflow.slice(
    workflow.indexOf("\n  windows:"),
    workflow.indexOf("\n  publish:"),
  );
  assertRustQualityGates(macosJob);
  assertRustQualityGates(windowsJob);
});

test("local releases run the same locked Rust checks", () => {
  const releaseScript = fs.readFileSync(new URL("../scripts/release.mjs", import.meta.url), "utf8");
  assert.match(releaseScript, /\["fmt", "--all", "--", "--check"\]/);
  assert.match(releaseScript, /\["test", "--workspace", "--locked"\]/);
  assert.match(releaseScript, /\["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"\]/);
});

test("desktop builds generate embedded overlay assets before Cargo compiles", () => {
  const overlayBuild = macBuildScript.indexOf("build-overlay.mjs");
  const cargoBuild = macBuildScript.indexOf('"cargo"');
  assert.notEqual(overlayBuild, -1);
  assert.notEqual(cargoBuild, -1);
  assert.ok(
    overlayBuild < cargoBuild,
    "the ignored dist-overlay directory must be generated before include_str! is compiled",
  );
});

test("macOS updates retain a rollback bundle until the replacement launches", () => {
  const backup = updateSource.indexOf('/bin/mv "$app_bundle" "$backup_bundle"');
  const install = updateSource.indexOf('/bin/mv "$tmp_dir/$app_name" "$app_bundle"');
  const launch = updateSource.indexOf('/usr/bin/open "$app_bundle"');
  const commit = updateSource.indexOf("replacement_committed=1");
  assert.ok(backup >= 0 && backup < install);
  assert.ok(install < launch && launch < commit);
  assert.match(
    updateSource,
    /if \[ "\$replacement_committed" -ne 1 \][\s\S]*?\/bin\/mv "\$backup_bundle" "\$app_bundle" \|\| true/,
  );
  assert.doesNotMatch(updateSource, /rm -rf "\$app_bundle"\s*\nmv "\$tmp_dir/);
});

test("desktop packages include FastCtx license and notice files", () => {
  for (const expected of [
    "README.md",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "licenses/FastCtx/LICENSE-APACHE",
    "licenses/FastCtx/NOTICE",
  ]) {
    assert.match(macBuildScript, new RegExp(expected.replaceAll("/", "\\/")));
  }

  assert.match(workflow, /Contents\/Resources\/licenses\/FastCtx\/LICENSE-APACHE/);
  assert.match(workflow, /Contents\/Resources\/licenses\/FastCtx\/NOTICE/);
  assert.match(windowsInstallerScript, /licenses\\FastCtx\\LICENSE-APACHE/);
  assert.match(windowsInstallerScript, /licenses\\FastCtx\\NOTICE/);
});

test("Windows release publishes the installer without a portable zip", () => {
  const nsisInstallStep = workflowStep(
    "- name: Install NSIS",
    "- name: Install frontend dependencies",
  );
  const windowsPackageStep = workflowStep(
    "- name: Build Windows packages",
    "- name: Upload Windows installer",
  );

  assert.match(workflow, /name: codey-windows-x64-installer/);
  assert.match(workflow, /windows-x64-setup\.exe/);
  assert.match(nsisInstallStep, /choco install nsis --yes --no-progress/);
  assert.match(nsisInstallStep, /\$maxAttempts = 3/);
  assert.match(nsisInstallStep, /\$installExitCode = \$LASTEXITCODE/);
  assert.match(nsisInstallStep, /Start-Sleep -Seconds \$delaySeconds/);
  assert.match(
    nsisInstallStep,
    /Chocolatey failed to install NSIS after \$maxAttempts attempts/,
  );
  assert.match(nsisInstallStep, /NSIS\\Bin\\makensis\.exe/);
  assert.match(nsisInstallStep, /GITHUB_PATH/);
  assert.match(nsisInstallStep, /MAKENSIS=/);
  assert.match(
    windowsPackageStep,
    /New-Item -ItemType Directory -Force "dist\\windows" \| Out-Null/,
  );
  assert.ok(
    windowsPackageStep.indexOf('New-Item -ItemType Directory -Force "dist\\windows"') <
      windowsPackageStep.indexOf("& $makensis"),
  );
  assert.match(windowsPackageStep, /\$makensis = \$env:MAKENSIS/);
  assert.match(
    windowsPackageStep,
    /Get-Command makensis -ErrorAction SilentlyContinue/,
  );
  assert.doesNotMatch(windowsPackageStep, /\$makensis = "makensis"/);
  assert.doesNotMatch(workflow, /windows-x64-portable\.zip/);
  assert.doesNotMatch(workflow, /codey-windows-x64-portable/);
});
