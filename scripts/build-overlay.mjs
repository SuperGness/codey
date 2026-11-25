import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { readFileSync, readdirSync } from "node:fs";
import { build, transformWithEsbuild } from "vite";
import { loadInjectAppIconDataUrl, writeBuildOutputs } from "./build-output.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const configFile = join(root, "vite.overlay.config.ts");

function collectOutputs(outputs, result) {
  for (const chunk of [result].flat()) {
    for (const asset of chunk.output) {
      outputs.set(asset.fileName, asset.type === "chunk" ? asset.code : asset.source);
    }
  }
}

const outputs = new Map();
const overlayResult = await build({
  root,
  configFile,
  build: { write: false },
});
collectOutputs(outputs, overlayResult);

const requestLogsResult = await build({
  root,
  configFile,
  build: {
    write: false,
    lib: {
      entry: "src/request-logs-entry.tsx",
      name: "CodeyRequestLogs",
      formats: ["iife"],
      fileName: () => "codey-request-logs.js",
      cssFileName: "codey-request-logs",
    },
  },
});
collectOutputs(outputs, requestLogsResult);

// public/ 下的注入脚本以源码形态维护，但会被逐字节嵌入 Codey 二进制并在
// Codex 渲染进程内求值。这里统一压缩到 dist-overlay/inject/，cdp.rs 只嵌入
// 压缩产物。
const publicDir = join(root, "public");
// 与控制台共用应用图标的 32x32 帧，缩小内联注入脚本体积且无需从宿主页面请求额外资源。
const appIconDataUrl = loadInjectAppIconDataUrl(root);
let rawTotal = 0;
let minifiedTotal = 0;
for (const name of readdirSync(publicDir).filter((entry) => entry.endsWith(".js"))) {
  const source = readFileSync(join(publicDir, name), "utf8")
    .replaceAll("__CODEY_APP_ICON_DATA_URL__", appIconDataUrl);
  // esbuild 在解析层就会常量折叠 `"__CODEY_X__" === "true"` 这类比较，任何
  // minify 开关都关不掉；这些占位符是 cdp.rs 按启动设置做运行时替换的锚点。
  // 因此压缩后逐一校验占位符仍在，丢失即整文件回退为源码拷贝。
  const markers = [...new Set(source.match(/__CODEY[A-Z_]*__/g) ?? [])];
  const { code } = await transformWithEsbuild(source, name, {
    minify: true,
    target: "es2022",
    sourcemap: false,
  });
  const lostMarkers = markers.filter((marker) => !code.includes(marker));
  const output = lostMarkers.length > 0 ? source : code;
  if (lostMarkers.length > 0) {
    console.log(
      `[overlay] kept ${name} unminified (folded markers: ${lostMarkers.join(", ")})`,
    );
  }
  outputs.set(`inject/${name}`, output);
  rawTotal += Buffer.byteLength(source);
  minifiedTotal += Buffer.byteLength(output);
}
writeBuildOutputs(join(root, "dist-overlay"), outputs);
console.log(
  `[overlay] minified inject scripts: ${rawTotal} -> ${minifiedTotal} bytes`,
);
