import { chmodSync, copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const web = spawnSync(
  process.execPath,
  [join(root, "scripts", "build-web.mjs")],
  { cwd: root, stdio: "inherit" },
);
if (web.status !== 0) process.exit(web.status ?? 1);

const cargo = spawnSync(
  "cargo",
  ["build", "--release", "--manifest-path", join(root, "Cargo.toml")],
  { cwd: root, stdio: "inherit" },
);

if (cargo.status !== 0) process.exit(cargo.status ?? 1);
if (process.platform !== "darwin") process.exit(0);

const binary = join(root, "target", "release", "codey");
const app = join(root, "target", "release", "bundle", "macos", "Codey.app");
const contents = join(app, "Contents");
const macos = join(contents, "MacOS");
const resources = join(contents, "Resources");
const bundledBinary = join(macos, "codey");

rmSync(app, { recursive: true, force: true });
mkdirSync(macos, { recursive: true });
mkdirSync(resources, { recursive: true });
copyFileSync(binary, bundledBinary);
copyFileSync(join(root, "backend", "icons", "Codey.icns"), join(resources, "Codey.icns"));
chmodSync(bundledBinary, 0o755);
writeFileSync(
  join(contents, "Info.plist"),
  `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>zh_CN</string>
  <key>CFBundleDisplayName</key><string>Codey</string>
  <key>CFBundleExecutable</key><string>codey</string>
  <key>CFBundleIdentifier</key><string>com.codey.codex-fixer</string>
  <key>CFBundleIconFile</key><string>Codey</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Codey</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>10.13</string>
  <key>LSMultipleInstancesProhibited</key><true/>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
`,
);

if (process.env.CODEY_SKIP_CODESIGN !== "1") {
  const codesign = spawnSync(
    "codesign",
    ["--force", "--deep", "--sign", "-", app],
    { cwd: root, stdio: "inherit" },
  );
  if (codesign.status !== 0) process.exit(codesign.status ?? 1);
}

console.log(`Codey macOS app: ${app}`);
