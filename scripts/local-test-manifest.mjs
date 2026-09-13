// Local CI helper for Windows-gnu dev machines.
//
// Binaries built with the GNU toolchain link `TaskDialogIndirect` from
// comctl32.dll (via rfd). That export only exists in comctl32 v6, which
// Windows only binds when the executable requests it through an application
// manifest. CI builds with MSVC (which embeds a default manifest), but local
// GNU binaries die at startup with STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139)
// without one — lib test binaries AND the main `codey.exe`.
//
// This script runs `cargo test --no-run --message-format=json` and writes an
// external `<exe>.manifest` (the Windows loader picks it up automatically)
// beside every built test executable:
//
//   node scripts/local-test-manifest.mjs            # dev profile
//   node scripts/local-test-manifest.mjs --release  # release profile
//
// For the release main binary, also write the same manifest next to
// `target/release/codey.exe` / `codey-fastctx.exe` after `npm run build`.
import { execFileSync } from "node:child_process";
import { utimesSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const cargoArgs = process.argv.slice(2).filter((arg) => arg !== "--no-run");
let output;
try {
  output = execFileSync(
    "cargo",
    ["test", "--no-run", "--message-format=json", ...cargoArgs],
    { cwd: join(import.meta.dirname, ".."), maxBuffer: 512 * 1024 * 1024 },
  ).toString();
} catch (error) {
  console.error(String(error.stderr ?? error.message));
  process.exit(error.status ?? 1);
}

const manifest = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
`;

let written = 0;
for (const line of output.split("\n")) {
  if (!line.trim()) continue;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    continue;
  }
  if (message?.reason !== "compiler-artifact" || !message.executable) continue;
  writeFileSync(`${message.executable}.manifest`, manifest);
  // Windows caches SxS binding per exe path + timestamp: an exe that already
  // ran without a manifest keeps failing even after the manifest appears.
  // Touching the exe changes its timestamp and invalidates that cache.
  const now = new Date();
  try {
    utimesSync(message.executable, now, now);
  } catch { /* exe may have been cleaned up between build and here */ }
  written += 1;
}
console.log(`[manifest] wrote ${written} manifests`);
