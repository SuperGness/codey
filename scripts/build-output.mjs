import { mkdirSync, readFileSync, readdirSync, rmdirSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const pngMagic = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

export function extractIcoPngFrame(icoBuffer, preferredSize = 32) {
  if (!Buffer.isBuffer(icoBuffer) || icoBuffer.length < 6) return null;
  if (icoBuffer.readUInt16LE(0) !== 0 || icoBuffer.readUInt16LE(2) !== 1) return null;
  const count = icoBuffer.readUInt16LE(4);
  for (let index = 0; index < count; index += 1) {
    const entryOffset = 6 + index * 16;
    if (entryOffset + 16 > icoBuffer.length) break;
    const width = icoBuffer[entryOffset] || 256;
    const height = icoBuffer[entryOffset + 1] || 256;
    const bytesInRes = icoBuffer.readUInt32LE(entryOffset + 8);
    const imageOffset = icoBuffer.readUInt32LE(entryOffset + 12);
    if (width !== preferredSize || height !== preferredSize) continue;
    if (bytesInRes <= pngMagic.length || imageOffset + bytesInRes > icoBuffer.length) continue;
    const slice = icoBuffer.subarray(imageOffset, imageOffset + bytesInRes);
    if (slice.subarray(0, pngMagic.length).equals(pngMagic)) return slice;
  }
  return null;
}

export function loadInjectAppIconDataUrl(root) {
  try {
    const frame = extractIcoPngFrame(readFileSync(join(root, "backend/icons/Codey.ico")), 32);
    if (frame) return `data:image/png;base64,${frame.toString("base64")}`;
  } catch {}
  return `data:image/png;base64,${readFileSync(join(root, "backend/icons/favicon.png")).toString("base64")}`;
}

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
