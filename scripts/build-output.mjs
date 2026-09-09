import { mkdirSync, readFileSync, readdirSync, rmdirSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

// Preserve embedded assets' mtimes so Cargo can reuse unchanged binaries.
export function writeBuildOutputs(directory, outputs) {
  const keep = new Set([...outputs.keys()].map((name) => join(directory, name)));
  mkdirSync(directory, { recursive: true });
  function prune(path) {
    let empty = true;
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) {
        if (prune(child)) rmdirSync(child);
        else empty = false;
      } else if (entry.isSymbolicLink() || !keep.has(child)) {
        unlinkSync(child);
      } else {
        empty = false;
      }
    }
    return empty;
  }
  prune(directory);
  for (const [name, source] of outputs) {
    const path = join(directory, name);
    const bytes = Buffer.from(source);
    try {
      if (readFileSync(path).equals(bytes)) continue;
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, bytes);
  }
}
