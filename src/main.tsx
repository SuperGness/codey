import React from "react";
import ReactDOM from "react-dom/client";
import { MantineProvider } from "@mantine/core";
import "@mantine/core/styles.css";
import { App } from "./App";
import { codeyMantineTheme } from "./mantine";
import "./tailwind.css";
import "./styles.css";
import "./styles.operations.css";
import "./styles.models.css";
import "./styles.features.css";
import "./styles.diagnostics.css";
import "./styles.responsive.css";

// 在 Vite 开发模式下，若未通过 Codey Bridge/Token 访问，自动注入 Mock 接口方便 UI 调试。
// 预览数据与 Mock 实现放在 dev/mockApi.ts，生产构建不会打包该模块。
if (import.meta.env.DEV) {
  await import("./dev/mockApi");
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <MantineProvider
      cssVariablesSelector="#root"
      forceColorScheme="light"
      theme={codeyMantineTheme}
    >
      <App />
    </MantineProvider>
  </React.StrictMode>,
);
