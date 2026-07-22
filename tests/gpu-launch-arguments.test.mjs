import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("GPU launch modes are mutually exclusive, opt-in, and persisted", async () => {
  const [typesSource, configSource, commandsSource, launcherSource] = await Promise.all([
    readFile(new URL("src/App.types.ts", root), "utf8"),
    readFile(new URL("backend/src/config.rs", root), "utf8"),
    readFile(new URL("backend/src/commands.rs", root), "utf8"),
    readFile(new URL("backend/src/launcher.rs", root), "utf8"),
  ]);

  assert.match(
    typesSource,
    /gpuLaunchMode: "off" \| "disableGpu" \| "disableGpuRasterization"/,
  );
  assert.match(configSource, /pub enum GpuLaunchMode/);
  assert.match(configSource, /pub gpu_launch_mode: GpuLaunchMode/);
  assert.match(configSource, /gpu_launch_mode: GpuLaunchMode::Off/);
  assert.doesNotMatch(configSource, /pub disable_gpu_sandbox: bool/);
  assert.doesNotMatch(configSource, /pub disable_hardware_acceleration: bool/);
  assert.match(
    commandsSource,
    /config\.gpu_launch_mode = config_input\.gpu_launch_mode/,
  );
  assert.match(
    commandsSource,
    /applied\.gpu_launch_mode != current\.gpu_launch_mode/,
  );
  assert.match(
    launcherSource,
    /const DISABLE_GPU_ARGUMENT: &str = "--disable-gpu"/,
  );
  assert.match(
    launcherSource,
    /const DISABLE_GPU_RASTERIZATION_ARGUMENT: &str = "--disable-gpu-rasterization"/,
  );
  assert.match(
    launcherSource,
    /GpuLaunchMode::DisableGpu => vec!\[DISABLE_GPU_ARGUMENT\.to_string\(\)\]/,
  );
  assert.match(
    launcherSource,
    /GpuLaunchMode::DisableGpuRasterization[\s\S]{0,120}DISABLE_GPU_RASTERIZATION_ARGUMENT/,
  );
  assert.match(
    launcherSource,
    /let gpu_arguments = gpu_launch_arguments\([\s\S]{0,300}!cfg!\(target_os = "macos"\)/,
  );
  assert.doesNotMatch(launcherSource, /--disable-gpu-sandbox/);
  assert.doesNotMatch(launcherSource, /--disable-hardware-acceleration/);
});

test("three-position GPU slider is accessible and disabled on macOS", async () => {
  const [sectionsSource, stylesSource, appSource, launcherSource, previewSource] = await Promise.all([
    readFile(new URL("src/AppSections.tsx", root), "utf8"),
    readFile(new URL("src/styles.css", root), "utf8"),
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("backend/src/launcher.rs", root), "utf8"),
    readFile(new URL("src/main.tsx", root), "utf8"),
  ]);

  assert.match(sectionsSource, /const isMacClient = status\.clientPlatform === "macos"/);
  assert.match(sectionsSource, /\{ value: "off", label: "关闭" \}/);
  assert.match(sectionsSource, /\{ value: "disableGpu", label: "禁用 GPU" \}/);
  assert.match(
    sectionsSource,
    /\{ value: "disableGpuRasterization", label: "禁用 GPU 栅格化" \}/,
  );
  assert.match(sectionsSource, /<fieldset[\s\S]{0,150}disabled=\{isMacClient\}/);
  assert.match(sectionsSource, /type="radio"/);
  assert.match(sectionsSource, /checked=\{gpuLaunchMode\.value === mode\.value\}/);
  assert.match(sectionsSource, /gpuLaunchMode: mode\.value/);
  assert.match(sectionsSource, /<legend className="sr-only">Codex GPU 启动模式<\/legend>/);
  assert.match(sectionsSource, /aria-describedby="gpu-launch-mode-description"/);
  assert.match(sectionsSource, /macOS 下已禁用，不会向 Codex 传递 GPU 诊断参数/);
  assert.match(stylesSource, /\.gpu-mode-slider-thumb/);
  assert.match(stylesSource, /transform: translateX\(var\(--gpu-mode-offset\)\)/);
  assert.match(stylesSource, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(appSource, /<FeaturePolicyCard[\s\S]{0,200}status=\{status\}/);
  assert.match(
    launcherSource,
    /gpu_launch_arguments\(GpuLaunchMode::DisableGpu, false\)\.is_empty\(\)/,
  );
  assert.match(previewSource, /gpuLaunchMode: "off" as const/);
});
