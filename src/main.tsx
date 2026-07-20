import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { previewOfficialModels, previewUpstreamModels } from "./previewModels";
import { previewTraceLogStats } from "./previewTraceLogStats";
import "./styles.css";

// 在 Vite 开发模式下，若未通过 Codey Bridge/Token 访问，自动注入 Mock 接口方便 UI 调试
if (import.meta.env.DEV) {
  if (!window.__codeyInvokeApi) {
    console.log("[Dev Mode] Auto-injecting Codey Mock API");
    let previewConfig = {
      activeProfileId: "primary",
      profiles: [
        {
          id: "primary",
          name: "主力代理 (ChatGPT)",
          baseUrl: "https://api.openai.com/v1",
          apiKey: "sk-proj-....................",
          protocol: "responses" as const,
          ccSwitchProviderId: "primary",
          ccSwitchReadOnly: false,
        },
        {
          id: "backup",
          name: "备用中转 (Claude)",
          baseUrl: "https://api.anthropic.com/v1",
          apiKey: "",
          protocol: "chatCompletions" as const,
          ccSwitchProviderId: "backup",
          ccSwitchReadOnly: false,
        },
      ],
      webhook: {
        enabled: true,
        url: "https://open.feishu.cn/open-apis/bot/v2/hook/a1b2c3d4-e5f6-7a8b-9c0d-1e2f3a4b5c6d",
      },
      codexAppPath: "/Applications/ChatGPT.app",
      userScripts: [],
      selectedModelsByProvider: { primary: ["provider-fast-coder", "claude-sonnet-4-5"] },
      upstreamModelsByProvider: { primary: previewUpstreamModels },
      disableTraceLogWrites: true,
      slimCodexPet: true,
      slimCodexVoice: true,
    };
    const previewCcSwitch = {
      available: true,
      path: "~/.cc-switch/cc-switch.db",
      changed: false,
      provider: {
        id: "primary",
        name: "主力代理 (ChatGPT)",
        official: false,
        baseUrl: "https://api.openai.com/v1",
        protocol: "responses" as const,
        source: "cc-switch" as const,
      },
    };
    let previewModelState = {
      officialModels: previewOfficialModels.map((model, index) => ({ ...model, supported: index < 3 })),
      officialModelIds: previewOfficialModels.map((model) => model.slug),
      thirdPartyModels: ["provider-fast-coder", "claude-sonnet-4-5"],
      upstreamModels: previewUpstreamModels,
    };

    window.__codeyInvokeApi = async (command, args) => {
      console.log(`[Mock API Call] ${command}`, args);
      // Wait a tiny bit to simulate network delay
      await new Promise((resolve) => setTimeout(resolve, 300));
      
      if (command === "load_codey_config") {
        return { config: previewConfig, modelState: previewModelState, startupError: undefined, ccSwitch: previewCcSwitch };
      }
      if (command === "runtime_status") {
        return {
          running: true,
          activeProfileId: previewConfig.activeProfileId,
          activeProfileName: previewConfig.profiles.find((p) => p.id === previewConfig.activeProfileId)?.name || "未命名代理",
          codexAppPath: previewConfig.codexAppPath,
          maintenance: {
            sessionStatus: "ready",
            sessionDetail: "会话索引与回复链路正常 (18 线程活跃)",
            pluginStatus: "ready",
            pluginDetail: "Codex 插件已注入，且会话生命周期托管中",
          },
          traceLogStats: previewTraceLogStats,
        };
      }
      if (command === "save_codey_config") {
        previewConfig = args.config as typeof previewConfig;
        return { config: previewConfig, modelState: previewModelState, ccSwitch: previewCcSwitch };
      }
      if (command === "sync_current_provider") {
        return { config: previewConfig, modelState: previewModelState, ccSwitch: previewCcSwitch, restartRequired: false };
      }
      if (command === "clear_codex_trace_logs") {
        return {
          status: "ok",
          protectionEnabled: previewConfig.disableTraceLogWrites,
          cleanup: {
            databasesFound: 1,
            databasesCleaned: 1,
            rowsDeleted: 30141,
            bytesBefore: 406921216,
            bytesAfter: 49152,
            bytesReclaimed: 406872064,
          },
        };
      }
      if (command === "fetch_current_provider_models") {
        return { status: "ok", models: previewUpstreamModels, modelState: previewModelState };
      }
      if (command === "save_selected_models") {
        const requested = new Set(args.models as string[]);
        previewModelState = {
          ...previewModelState,
          thirdPartyModels: previewUpstreamModels.filter((model) => requested.has(model) && !previewOfficialModels.some((official) => official.slug === model)),
        };
        return { status: "ok", config: previewConfig, modelState: previewModelState };
      }
      if (command === "test_webhook") {
        return { status: 200 };
      }
      return { status: "ok" };
    };
  }
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
