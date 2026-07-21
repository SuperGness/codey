import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { previewOfficialModels, previewUpstreamModels } from "./previewModels";
import { previewTraceLogStats } from "./previewTraceLogStats";
import "./styles.css";

let previewConfig = {
  activeProfileId: "primary",
  profiles: [
    { id: "primary", name: "主力代理", baseUrl: "https://api.example.com/v1", apiKey: "sk-codey-preview", protocol: "responses", ccSwitchProviderId: "primary", ccSwitchReadOnly: false },
    { id: "backup", name: "备用线路", baseUrl: "https://backup.example.com/v1", apiKey: "", protocol: "chatCompletions", ccSwitchProviderId: "backup", ccSwitchReadOnly: false },
  ],
  webhook: { enabled: true, url: "https://open.feishu.cn/open-apis/bot/v2/hook/preview" },
  codexAppPath: "",
  userScripts: [],
  selectedModelsByProvider: { primary: ["provider-fast-coder", "claude-sonnet-4-5"] },
  upstreamModelsByProvider: { primary: previewUpstreamModels },
  disableTraceLogWrites: true,
  slimCodexPet: true,
  slimCodexVoice: true,
  fastContextTools: false,
  subagentOptimization: false,
  hideFullAccessWarning: false,
};

const previewCcSwitch = {
  available: true,
  path: "~/.cc-switch/cc-switch.db",
  changed: false,
  provider: { id: "primary", name: "主力代理", official: false, baseUrl: "https://api.example.com/v1", protocol: "responses", source: "cc-switch" },
};

let previewModelState = {
  officialModels: previewOfficialModels.map((model, index) => ({ ...model, supported: index < 3 })),
  officialModelIds: previewOfficialModels.map((model) => model.slug),
  thirdPartyModels: ["provider-fast-coder", "claude-sonnet-4-5"],
  upstreamModels: previewUpstreamModels,
};

window.__codeyInvokeApi = async (command, args) => {
  if (command === "load_codey_config") return { config: previewConfig, modelState: previewModelState, ccSwitch: previewCcSwitch, path: "~/Library/Application Support/com.Codey.Codey/config.json" };
  if (command === "runtime_status") {
    return {
      running: true,
      restartRequired: false,
      closeInProgress: false,
      activeProfileId: previewConfig.activeProfileId,
      activeProfileName: "主力代理",
      traceLogStats: previewTraceLogStats,
    };
  }
  if (command === "save_codey_config") {
    previewConfig = args.config as typeof previewConfig;
    return {
      status: "ok",
      config: previewConfig,
      modelState: previewModelState,
      ccSwitch: previewCcSwitch,
      restartRequired: false,
    };
  }
  if (command === "sync_current_provider") return { status: "ok", config: previewConfig, modelState: previewModelState, ccSwitch: previewCcSwitch };
  if (command === "fetch_current_provider_models") {
    return {
      status: "ok",
      models: previewUpstreamModels,
      modelState: previewModelState,
      restartRequired: false,
    };
  }
  if (command === "save_selected_models") {
    const selected = new Set(args.models as string[]);
    previewModelState = { ...previewModelState, thirdPartyModels: previewUpstreamModels.filter((model) => selected.has(model) && !previewOfficialModels.some((official) => official.slug === model)) };
    return {
      status: "ok",
      config: previewConfig,
      modelState: previewModelState,
      restartRequired: true,
    };
  }
  if (command === "close_codex" || command === "restart_codey") return { status: "closing" };
  if (command === "clear_codex_trace_logs") return { status: "ok", protectionEnabled: previewConfig.disableTraceLogWrites, cleanup: { databasesFound: 2, databasesCleaned: 2, rowsDeleted: 318757, bytesBefore: 903634944, bytesAfter: 98304, bytesReclaimed: 903536640 } };
  return { status: "ok" };
};

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode><App /></React.StrictMode>,
);
