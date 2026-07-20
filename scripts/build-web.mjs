import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const vite = join(root, "node_modules", ".bin", process.platform === "win32" ? "vite.cmd" : "vite");

for (const args of [["build"], ["build", "--config", "vite.overlay.config.ts"]]) {
  const result = spawnSync(vite, args, { cwd: root, stdio: "inherit" });
  if (result.status !== 0) process.exit(result.status ?? 1);
}
