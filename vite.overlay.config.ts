import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const KEEP_REACT_ARIA_LOCALES = new Set(["zh-CN", "en-US"]);
const REACT_ARIA_INTL_LOCALE_RE =
  /[\\/](?:react-aria|react-aria-components|react-stately|@react-aria[\\/][^\\/]+|@react-stately[\\/][^\\/]+)[\\/].*[\\/]intl[\\/](?:.*[\\/])?([a-z]{2,3}-[A-Z]{2})\.(?:m?js|cjs)$/;

// Codey 固定使用 zh-CN（并以 en-US 作为回退），裁掉其余 32 份 react-aria/react-stately 语言包。
function stripUnusedReactAriaLocales(): Plugin {
  return {
    name: "codey-strip-unused-react-aria-locales",
    enforce: "pre",
    load(id) {
      const cleanId = id.split("?", 1)[0];
      const match = REACT_ARIA_INTL_LOCALE_RE.exec(cleanId);
      if (match && !KEEP_REACT_ARIA_LOCALES.has(match[1])) {
        return "export default {};";
      }
      return null;
    },
  };
}

export default defineConfig({
  plugins: [stripUnusedReactAriaLocales(), react(), tailwindcss()],
  publicDir: false,
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
      cssFileName: "codey",
    },
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
      },
    },
  },
});
