// 所有配置和运行状态都留在临时目录；这些库基准不会启动 Codex。
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const directory = mkdtempSync(join(tmpdir(), "codey-performance-"));
try {
  const result = spawnSync("cargo", [
    "test", "-p", "codey", "--release", "--lib", process.argv[2] ?? "latency",
    "--", "--ignored", "--nocapture", "--test-threads=1",
  ], {
    cwd: root,
    stdio: "inherit",
    env: {
      ...process.env,
      CODEY_APP_STATE_DIR: join(directory, "state"),
      CODEX_HOME: join(directory, "codex"),
      // 保留 release 优化，缩短基准测试的链接时间；不改变正式构建配置。
      CARGO_PROFILE_RELEASE_LTO: "false",
      CARGO_PROFILE_RELEASE_CODEGEN_UNITS: "16",
    },
  });
  if (result.error) throw result.error;
  process.exitCode = result.status ?? 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
