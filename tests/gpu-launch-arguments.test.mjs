import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { readAppStyles } from "./helpers/read-app-styles.mjs";

const root = new URL("../", import.meta.url);

test("three-position GPU slider is accessible and available only on Windows", async () => {
  const [sectionsSource, stylesSource, appSource, previewSource] = await Promise.all([
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readAppStyles(root),
    readFile(new URL("src/App.tsx", root), "utf8"),
    readFile(new URL("src/dev/mockApi.ts", root), "utf8"),
  ]);

  assert.match(sectionsSource, /isWindowsClient: boolean/);
  assert.match(sectionsSource, /\{ value: "off", label: "关闭" \}/);
  assert.match(sectionsSource, /\{ value: "disableGpu", label: "禁用 GPU" \}/);
  assert.match(
    sectionsSource,
    /\{ value: "disableGpuRasterization", label: "禁用 GPU 栅格化" \}/,
  );
  assert.match(
    sectionsSource,
    /\{isWindowsClient && \([\s\S]{0,500}<fieldset[\s\S]{0,150}disabled=\{isBusy\}/,
  );
  assert.match(sectionsSource, /type="radio"/);
  assert.match(sectionsSource, /checked=\{gpuLaunchMode\.value === mode\.value\}/);
  assert.match(sectionsSource, /gpuLaunchMode: mode\.value/);
  assert.match(sectionsSource, /<legend className="sr-only">Codex GPU 启动模式<\/legend>/);
  assert.match(sectionsSource, /aria-describedby="gpu-launch-mode-description"/);
  assert.match(stylesSource, /\.gpu-mode-slider-thumb/);
  assert.match(stylesSource, /transform: translateX\(var\(--gpu-mode-offset\)\)/);
  assert.match(stylesSource, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(
    appSource,
    /<FeaturePolicyCard[\s\S]{0,200}isWindowsClient=\{status\.clientPlatform === "windows"\}/,
  );
  assert.match(previewSource, /gpuLaunchMode: "off" as const/);
});
