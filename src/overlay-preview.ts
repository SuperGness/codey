export {};

import { previewOfficialModels, previewUpstreamModels } from "./previewModels";
import { previewTraceLogStats } from "./previewTraceLogStats";

let previewConfig = {
  activeProfileId: "primary",
  profiles: [
    { id: "primary", name: "主力代理", baseUrl: "https://api.example.com/v1", apiKey: "sk-codey-preview", protocol: "responses", ccSwitchReadOnly: false },
    { id: "backup", name: "备用线路", baseUrl: "https://backup.example.com/v1", apiKey: "", protocol: "chatCompletions", ccSwitchReadOnly: false },
  ],
  webhook: { enabled: true, url: "https://open.feishu.cn/open-apis/bot/v2/hook/preview" },
  codexAppPath: "",
  userScripts: [],
  selectedModelsByProvider: { primary: ["provider-fast-coder", "claude-sonnet-4-5"] },
  upstreamModelsByProvider: { primary: previewUpstreamModels },
  defaultModelByProvider: {},
  disableTraceLogWrites: true,
  slimCodexPet: true,
  slimCodexVoice: true,
  gpuLaunchMode: "off" as const,
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
  officialModels: previewOfficialModels
    .filter((model) => previewUpstreamModels.includes(model.slug))
    .map((model) => ({ ...model, supported: true })),
  officialModelIds: previewOfficialModels.map((model) => model.slug),
  thirdPartyModels: ["provider-fast-coder", "claude-sonnet-4-5"],
  upstreamModels: previewUpstreamModels,
  defaultModel: "gpt-5.6-sol",
};

window.__codexSessionDeleteBridge = async (path, payload) => {
  const command = path.replace(/^\/api\//, "");
  if (command === "load_codey_config") return { config: previewConfig, modelState: previewModelState, ccSwitch: previewCcSwitch, path: "~/Library/Application Support/com.Codey.Codey/config.json" };
  if (command === "runtime_status") {
    return {
      running: true,
      clientPlatform: "macos",
      restartRequired: false,
      restartInProgress: false,
      activeProfileId: previewConfig.activeProfileId,
      traceLogStats: previewTraceLogStats,
    };
  }
  if (command === "save_codey_config") {
    previewConfig = (payload as { config: typeof previewConfig }).config;
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
    const selected = new Set((payload as { models: string[] }).models);
    previewModelState = { ...previewModelState, thirdPartyModels: previewUpstreamModels.filter((model) => selected.has(model) && !previewOfficialModels.some((official) => official.slug === model)) };
    return {
      status: "ok",
      config: previewConfig,
      modelState: previewModelState,
      restartRequired: true,
    };
  }
  if (command === "save_default_model") {
    const model = String((payload as { model?: string }).model || "");
    previewConfig = { ...previewConfig, defaultModelByProvider: { ...previewConfig.defaultModelByProvider, primary: model } };
    previewModelState = { ...previewModelState, defaultModel: model };
    return {
      status: "ok",
      config: previewConfig,
      modelState: previewModelState,
      restartRequired: true,
    };
  }
  if (command === "restart_codey") return { status: "restarting" };
  if (command === "clear_codex_trace_logs") return { status: "ok", protectionEnabled: previewConfig.disableTraceLogWrites, cleanup: { databasesFound: 2, databasesCleaned: 2, rowsDeleted: 318757, bytesBefore: 903634944, bytesAfter: 98304, bytesReclaimed: 903536640 } };
  return { status: "ok" };
};

function loadScript(source: string) {
  return new Promise<void>((resolve, reject) => {
    const script = document.createElement("script");
    script.src = source;
    script.onload = () => resolve();
    script.onerror = () => reject(new Error(`无法加载 ${source}`));
    document.head.appendChild(script);
  });
}

await loadScript("/dist-overlay/codey-overlay.js");
await loadScript("/renderer-inject.js");
