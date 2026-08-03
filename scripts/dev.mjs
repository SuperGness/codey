import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function optionValue(args, option) {
  const inline = args.find((arg) => arg.startsWith(`${option}=`));
  if (inline) return inline.slice(option.length + 1);

  const index = args.indexOf(option);
  return index === -1 ? undefined : args[index + 1];
}

export function cargoProfile(args) {
  const separator = args.indexOf("--");
  const cargoArgs = separator === -1 ? args : args.slice(0, separator);
  const profile = optionValue(cargoArgs, "--profile");
  if (profile) return profile;
  return cargoArgs.includes("--release") ? "release" : "debug";
}

export function codeyExecutablePaths({ root, args, env }) {
  const separator = args.indexOf("--");
  const cargoArgs = separator === -1 ? args : args.slice(0, separator);
  const profile = cargoProfile(cargoArgs);
  const target = optionValue(cargoArgs, "--target");

  // Cargo accepts a JSON target specification path too. Do not risk touching an
  // unrelated executable when its output directory cannot be derived safely.
  if (!/^[A-Za-z0-9_-]+$/.test(profile)) return [];
  if (target && !/^[A-Za-z0-9_.-]+$/.test(target)) return [];

  const targetDirectory = env.CARGO_TARGET_DIR
    ? resolve(root, env.CARGO_TARGET_DIR)
    : join(root, "target");
  return [join(targetDirectory, target ?? "", profile, "codey.exe").toLowerCase()];
}

export function forceKillEnabled(env) {
  return env.CODEY_DEV_FORCE_KILL === "1";
}

function powershellString(value) {
  return `'${value.replace(/'/g, "''")}'`;
}

function matchingProcessesCommand(paths) {
  return `$paths = @(${paths.map(powershellString).join(", ")}); Get-CimInstance Win32_Process -Filter \"Name = 'codey.exe'\" -ErrorAction Stop | Where-Object { $_.ExecutablePath -and $paths -contains ([string]$_.ExecutablePath).ToLowerInvariant() }`;
}

function listLockedCodeyProcesses(paths, spawn) {
  return spawn(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `${matchingProcessesCommand(paths)} | ForEach-Object { Write-Output $_.ProcessId }`,
    ],
    { encoding: "utf8" },
  );
}

function forceStopLockedCodeyProcesses(paths, spawn) {
  return spawn(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      [
        matchingProcessesCommand(paths),
        "ForEach-Object {",
        "  try {",
        "    Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop",
        "    Write-Output $_.ProcessId",
        "  } catch {",
        "    Write-Error $_",
        "    exit 1",
        "  }",
        "}",
      ].join(" | "),
    ],
    { encoding: "utf8" },
  );
}

function processIds(result) {
  return (result.stdout || "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => /^\d+$/.test(line));
}

function commandFailure(result) {
  return result.error?.message || result.stderr?.trim() || `PowerShell exited with code ${result.status}`;
}

export function ensureUnlockedCodeyOnWindows({
  args = [],
  env = process.env,
  log = console,
  platform = process.platform,
  root = projectRoot,
  spawn = spawnSync,
} = {}) {
  if (platform !== "win32") return true;

  const paths = codeyExecutablePaths({ root, args, env });
  if (paths.length === 0) {
    log.warn("[dev] 无法安全识别当前 Cargo profile，未尝试结束运行中的 Codey。");
    return true;
  }

  const listed = listLockedCodeyProcesses(paths, spawn);
  if (listed.status !== 0) {
    log.warn(`[dev] 无法检查可能锁定 codey.exe 的进程：${commandFailure(listed)}`);
    return true;
  }

  const locked = processIds(listed);
  if (locked.length === 0) return true;

  if (!forceKillEnabled(env)) {
    log.error(
      `[dev] 检测到正在运行的本地 Codey 进程：${locked.join(", ")}。请先从系统托盘或原终端正常退出，以完成 Codex 和临时配置清理。若确认它已卡死，可设置 CODEY_DEV_FORCE_KILL=1 后重试。`,
    );
    return false;
  }

  const stopped = forceStopLockedCodeyProcesses(paths, spawn);
  if (stopped.status !== 0) {
    log.error(`[dev] 无法强制结束本地 Codey 进程：${commandFailure(stopped)}`);
    return false;
  }

  const remaining = listLockedCodeyProcesses(paths, spawn);
  if (remaining.status !== 0 || processIds(remaining).length > 0) {
    const detail = remaining.status === 0
      ? `仍在运行的 PID：${processIds(remaining).join(", ")}`
      : commandFailure(remaining);
    log.error(`[dev] 未能确认 Codey 已退出，已停止启动 Cargo：${detail}`);
    return false;
  }

  const stoppedIds = processIds(stopped);
  log.log(`[dev] 已强制结束本地 Codey 进程：${stoppedIds.join(", ")}`);
  return true;
}

function main() {
  const args = process.argv.slice(2);
  if (!ensureUnlockedCodeyOnWindows({ args })) {
    process.exitCode = 1;
    return;
  }

  const cargo = spawnSync(
    "cargo",
    ["run", "--manifest-path", join(projectRoot, "Cargo.toml"), ...args],
    { cwd: projectRoot, stdio: "inherit" },
  );
  process.exitCode = cargo.status ?? 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
