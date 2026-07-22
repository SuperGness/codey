import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  // The overlay is evaluated directly inside Codex's renderer instead of
  // being loaded by a normal Vite HTML entry. Replace React's CommonJS
  // environment check explicitly so the IIFE never expects Node's `process`
  // global to exist in that renderer.
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
  build: {
    target: "es2022",
    outDir: "dist-overlay",
    emptyOutDir: true,
    minify: "esbuild",
    lib: {
      entry: "src/overlay.tsx",
      name: "CodeySettingsOverlay",
      formats: ["iife"],
      fileName: () => "codey-overlay.js",
    },
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
      },
    },
  },
});
