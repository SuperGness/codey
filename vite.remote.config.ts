import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  publicDir: false,
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
  build: {
    target: "es2022", minify: "esbuild",
    lib: { entry: "src/remote/main.tsx", name: "CodeyRemote", formats: ["iife"], fileName: () => "codey-remote.js", cssFileName: "codey-remote" },
    rollupOptions: { output: { inlineDynamicImports: true } },
  },
});
