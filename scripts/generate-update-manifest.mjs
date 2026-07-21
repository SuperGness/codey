import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, stat, writeFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";

const artifactDefinitions = [
  { platform: "macos", arch: "arm64", package_type: "app-zip", suffix: "macos-arm64-unsigned.zip" },
  { platform: "macos", arch: "x64", package_type: "app-zip", suffix: "macos-x64-unsigned.zip" },
  { platform: "windows", arch: "x64", package_type: "portable-zip", suffix: "windows-x64-portable.zip" },
  { platform: "windows", arch: "x64", package_type: "nsis", suffix: "windows-x64-setup.exe" },
];

function fail(message) {
  throw new Error(message);
}

function parseArguments(argv) {
  const options = {};
  const assets = [];

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (!argument.startsWith("--")) {
      assets.push(argument);
      continue;
    }

    const key = argument.slice(2);
    if (!new Set(["version", "tag", "download-base-url", "output"]).has(key)) {
      fail(`Unknown option: ${argument}`);
    }
    if (options[key] !== undefined) fail(`Option may only be provided once: ${argument}`);

    const value = argv[index + 1];
    if (!value || value.startsWith("--")) fail(`Missing value for ${argument}`);
    options[key] = value;
    index += 1;
  }

  for (const key of ["version", "tag", "download-base-url", "output"]) {
    if (!options[key]) fail(`Missing required option --${key}`);
  }
  if (assets.length === 0) fail("At least one release asset is required");

  return { options, assets };
}

function validateReleaseIdentity(version, tag) {
  const semver = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
  if (!semver.test(version)) fail(`Version must be SemVer without a v prefix: ${version}`);
  if (tag !== `v${version}`) fail(`Tag ${tag} must match version ${version}`);
}

function checksum(filePath) {
  return new Promise((resolveChecksum, reject) => {
    const hash = createHash("sha256");
    const input = createReadStream(filePath);
    input.on("error", reject);
    input.on("data", (chunk) => hash.update(chunk));
    input.on("end", () => resolveChecksum(hash.digest("hex")));
  });
}

function urlForAsset(downloadBaseUrl, fileName) {
  return `${downloadBaseUrl.replace(/\/+$/, "")}/${encodeURIComponent(fileName)}`;
}

async function buildManifest({ version, tag, downloadBaseUrl, assetPaths }) {
  const expectedByName = new Map(
    artifactDefinitions.map((definition) => [
      `Codey-${version}-${definition.suffix}`,
      definition,
    ]),
  );
  const providedPaths = new Map(assetPaths.map((assetPath) => [basename(assetPath), resolve(assetPath)]));

  const missing = [...expectedByName.keys()].filter((fileName) => !providedPaths.has(fileName));
  if (missing.length > 0) fail(`Missing release assets: ${missing.join(", ")}`);

  const unexpected = [...providedPaths.keys()].filter((fileName) => !expectedByName.has(fileName));
  if (unexpected.length > 0) fail(`Unexpected release assets: ${unexpected.join(", ")}`);

  const assets = await Promise.all(
    artifactDefinitions.map(async (definition) => {
      const fileName = `Codey-${version}-${definition.suffix}`;
      const filePath = providedPaths.get(fileName);
      const metadata = await stat(filePath);
      if (!metadata.isFile()) fail(`Release asset is not a file: ${filePath}`);

      return {
        platform: definition.platform,
        arch: definition.arch,
        package_type: definition.package_type,
        file_name: fileName,
        url: urlForAsset(downloadBaseUrl, fileName),
        sha256: await checksum(filePath),
        size: metadata.size,
      };
    }),
  );

  return {
    schema_version: 1,
    version,
    tag,
    generated_at: new Date().toISOString(),
    assets,
  };
}

async function main() {
  const { options, assets } = parseArguments(process.argv.slice(2));
  validateReleaseIdentity(options.version, options.tag);
  const manifest = await buildManifest({
    version: options.version,
    tag: options.tag,
    downloadBaseUrl: options["download-base-url"],
    assetPaths: assets,
  });

  const output = resolve(options.output);
  await mkdir(dirname(output), { recursive: true });
  await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`);
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
