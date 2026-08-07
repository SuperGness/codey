import React from "react";
import ReactDOM from "react-dom/client";
import "../node_modules/@douyinfe/semi-ui/lib/es/_base/base.css";
import { App } from "./App";
import { previewOfficialModels, previewUpstreamModels } from "./previewModels";
import {
  previewCrashpadPendingStats,
  previewTraceLogStats,
} from "./previewTraceLogStats";
import "./styles.css";

// 在 Vite 开发模式下，若未通过 Codey Bridge/Token 访问，自动注入 Mock 接口方便 UI 调试
if (import.meta.env.DEV) {
  if (!window.__codeyInvokeApi) {
    console.log("[Dev Mode] Auto-injecting Codey Mock API");
    const previewClientPlatform =
      new URLSearchParams(window.location.search).get("platform") === "windows"
        ? "windows"
        : "macos";
    const previewEndpoints = {
      primary: "https://primary.example.invalid/v1",
      backup: "https://backup.example.invalid/v1",
      feishu: "https://webhook.example.invalid/feishu/preview-only",
    } as const;
    let previewConfig = {
      settingsRevision: 0,
      activeProfileId: "primary",
      profiles: [
        {
          id: "primary",
          name: "主力代理 (ChatGPT)",
          baseUrl: previewEndpoints.primary,
          apiKey: "preview-only-not-a-secret",
          protocol: "responses" as const,
          ccSwitchProviderId: "primary",
          ccSwitchReadOnly: false,
        },
        {
          id: "backup",
          name: "备用中转 (Claude)",
          baseUrl: previewEndpoints.backup,
          apiKey: "",
          protocol: "responses" as const,
          ccSwitchProviderId: "backup",
          ccSwitchReadOnly: false,
        },
      ],
      webhook: {
        channels: [
          {
            id: "preview-feishu",
            kind: "feishu" as const,
            enabled: true,
            url: previewEndpoints.feishu,
            botToken: "",
            botTokenConfigured: false,
            clearBotToken: false,
            chatId: "",
          },
          {
            id: "preview-telegram",
            kind: "telegram" as const,
            enabled: false,
            url: "",
            botToken: "",
            botTokenConfigured: true,
            clearBotToken: false,
            chatId: "preview-chat-id",
          },
        ],
      },
      codexAppPath: "/Applications/ChatGPT.app",
      userScripts: [],
      selectedModelsByProvider: {
        primary: ["provider-fast-coder", "claude-sonnet-4-5"],
      },
      manualThirdPartyModelsByProvider: {
        primary: ["provider-fast-coder"],
      },
      upstreamModelsByProvider: { primary: previewUpstreamModels },
      defaultModelByProvider: {},
      disableTraceLogWrites: true,
      protectCrashpadPending: true,
      slimCodexPet: false,
      slimCodexVoice: false,
      gpuLaunchMode: "off" as const,
      fastContextTools: false,
      fastCodexStartup: true,
      subagentOptimization: false,
      subagentModel: "gpt-5.6-terra",
      subagentReasoningEffort: "medium",
      hideFullAccessWarning: false,
      showAccountUsageInHeader: true,
    };
    const previewCcSwitch = {
      changed: false,
      provider: {
        id: "primary",
        name: "主力代理 (ChatGPT)",
        official: false,
        baseUrl: previewEndpoints.primary,
        protocol: "responses" as const,
      },
    };
    let previewModelState = {
      officialModels: previewOfficialModels.map((model) => ({
        ...model,
        supported: previewUpstreamModels.includes(model.slug),
      })),
      officialModelIds: previewOfficialModels.map((model) => model.slug),
      subagentModelIds: [
        ...previewOfficialModels.map((model) => model.slug),
        "provider-fast-coder",
        "claude-sonnet-4-5",
      ],
      thirdPartyModels: ["provider-fast-coder", "claude-sonnet-4-5"],
      manualThirdPartyModels: ["provider-fast-coder"],
      upstreamModels: previewUpstreamModels,
      defaultModel: "gpt-5.6-sol",
    };
    let previewTraceStats: typeof previewTraceLogStats | undefined;
    let previewCrashpadStats:
      | typeof previewCrashpadPendingStats
      | undefined = previewCrashpadPendingStats;

    window.__codeyInvokeApi = async (command, args) => {
      console.log(`[Mock API Call] ${command}`, args);
      // Wait a tiny bit to simulate network delay
      await new Promise((resolve) => setTimeout(resolve, 300));

      if (command === "load_codey_config") {
        return {
          config: previewConfig,
          modelState: previewModelState,
          startupError: undefined,
          ccSwitch: previewCcSwitch,
        };
      }
      if (command === "runtime_status") {
        return {
          running: true,
          appVersion: "0.2.0",
          codexAppVersion: "26.601.21317",
          clientPlatform: previewClientPlatform,
          restartRequired: false,
          restartInProgress: false,
          activeProfileId: previewConfig.activeProfileId,
          activeProfileName:
            previewConfig.profiles.find(
              (p) => p.id === previewConfig.activeProfileId,
            )?.name || "未命名代理",
          codexAppPath: previewConfig.codexAppPath,
          maintenance: {
            sessionStatus: "ready",
            sessionDetail: "会话索引与恢复链路正常 (18 线程活跃)",
            pluginStatus: "ready",
            pluginDetail: "Codex 插件已注入，且会话生命周期托管中",
            performanceStatus: "ready",
            performanceDetail:
              previewClientPlatform === "windows"
                ? "Windows 启动补丁已启用：WMI 周期采样、临时 WebView 残留和执行环境泄漏已修复"
                : "启动补丁已启用：临时 WebView 和执行环境会自动回收",
          },
          injectionScripts: [
            {
              id: "bridge-helpers",
              name: "桥接辅助",
              source: "builtin",
              status: "effective",
              detail: "桥接函数可调用",
            },
            {
              id: "model-whitelist",
              name: "模型白名单",
              source: "builtin",
              status: "effective",
              detail: "模型目录已加载（5 个模型）",
            },
            {
              id: "pet-control-shield",
              name: "宠物控制精简",
              source: "builtin",
              status: "effective",
              detail: "宠物控制精简已启用",
            },
            {
              id: "voice-control-shield",
              name: "语音控制精简",
              source: "builtin",
              status: "effective",
              detail: previewConfig.slimCodexVoice
                ? "语音 UI 与资源拦截已启用"
                : "控制器已就绪，当前精简策略关闭",
            },
            {
              id: "security-warning-shield",
              name: "安全提示控制",
              source: "builtin",
              status: "effective",
              detail: "控制器已就绪，当前屏蔽策略关闭",
            },
            {
              id: "settings-overlay-loader",
              name: "配置面板加载器",
              source: "builtin",
              status: "effective",
              detail: "配置面板按需加载器可用",
            },
            {
              id: "renderer-controls",
              name: "渲染器控制",
              source: "builtin",
              status: "effective",
              detail: "渲染器控制与按需加载 API 可用",
            },
            {
              id: "plugin-marketplace-compatibility",
              name: "插件市场兼容",
              source: "builtin",
              status: "effective",
              detail: "插件市场桥接已接管",
            },
          ],
          ...(previewTraceStats ? { traceLogStats: previewTraceStats } : {}),
          ...(previewCrashpadStats
            ? { crashpadPendingStats: previewCrashpadStats }
            : {}),
        };
      }
      if (command === "refresh_diagnostic_storage_stats") {
        previewTraceStats = previewTraceLogStats;
        previewCrashpadStats = {
          ...previewCrashpadPendingStats,
          protectionEnabled: previewConfig.protectCrashpadPending,
        };
        return {
          status: "ok",
          traceLogStats: previewTraceStats,
          crashpadPendingStats: previewCrashpadStats,
        };
      }
      if (command === "refresh_trace_log_stats") {
        previewTraceStats = previewTraceLogStats;
        return { status: "ok", traceLogStats: previewTraceStats };
      }
      if (command === "refresh_injection_status") {
        return { status: "ok" };
      }
      if (command === "save_codey_config") {
        previewConfig = {
          ...(args.config as typeof previewConfig),
          settingsRevision: previewConfig.settingsRevision + 1,
        };
        return {
          config: previewConfig,
          modelState: previewModelState,
          ccSwitch: previewCcSwitch,
          restartRequired: false,
        };
      }
      if (command === "reveal_notification_channel") {
        const channelId = String(args.channelId || "");
        const channel = previewConfig.webhook.channels.find(
          (candidate) => candidate.id === channelId,
        );
        return channel
          ? { channel }
          : { status: "failed", message: "找不到要编辑的通知渠道" };
      }
      if (command === "sync_current_provider") {
        return {
          config: previewConfig,
          modelState: previewModelState,
          ccSwitch: previewCcSwitch,
          restartRequired: false,
        };
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
      if (command === "clear_diagnostic_storage") {
        previewTraceStats = {
          ...previewTraceLogStats,
          databaseBytes: 49152,
          rowCount: 0,
          estimatedLogBytes: 0,
        };
        previewCrashpadStats = {
          ...previewCrashpadPendingStats,
          protectionEnabled: previewConfig.protectCrashpadPending,
          reportsFound: 0,
          completeReports: 0,
          filesFound: 0,
          managedFiles: 0,
          pendingBytes: 0,
          managedBytes: 0,
        };
        return {
          status: "ok",
          traceProtectionEnabled: previewConfig.disableTraceLogWrites,
          crashpadProtectionEnabled: previewConfig.protectCrashpadPending,
          errors: [],
          traceCleanup: {
            databasesFound: 1,
            databasesCleaned: 1,
            rowsDeleted: 30141,
            bytesBefore: 406921216,
            bytesAfter: 49152,
            bytesReclaimed: 406872064,
          },
          crashpadCleanup: {
            directoriesFound: 2,
            reportsFound: 13,
            reportsDeleted: 13,
            filesFound: 26,
            filesDeleted: 26,
            orphanFilesDeleted: 0,
            unmanagedFiles: 0,
            skippedRecentReports: 0,
            bytesBefore: 3448832,
            bytesAfter: 0,
            bytesReclaimed: 3448832,
            limitApplied: false,
            stillOverLimit: false,
            errors: [],
          },
          traceLogStats: previewTraceStats,
          crashpadPendingStats: previewCrashpadStats,
        };
      }
      if (command === "fetch_current_provider_models") {
        previewModelState = {
          ...previewModelState,
          officialModels: previewOfficialModels.map((model) => ({
            ...model,
            supported: previewUpstreamModels.includes(model.slug),
          })),
          thirdPartyModels: previewModelState.thirdPartyModels.filter((model) =>
            previewUpstreamModels.includes(model)
          ),
          manualThirdPartyModels: previewModelState.manualThirdPartyModels.filter((model) =>
            !previewUpstreamModels.includes(model)
          ),
          upstreamModels: previewUpstreamModels,
        };
        return {
          status: "ok",
          models: previewUpstreamModels,
          modelState: previewModelState,
          restartRequired: false,
          modelHotReloaded: true,
        };
      }
      if (command === "save_selected_models") {
        const officialModels = (args.officialModels as string[]) || [];
        const thirdPartyModels = (args.thirdPartyModels as string[]) || [];
        const manualThirdPartyModels = (args.manualThirdPartyModels as string[]) || [];
        const requestedOfficial = new Set(officialModels);
        const supportedModels = [...officialModels, ...thirdPartyModels];
        previewConfig = {
          ...previewConfig,
          selectedModelsByProvider: {
            ...previewConfig.selectedModelsByProvider,
            primary: thirdPartyModels,
          },
          manualThirdPartyModelsByProvider: {
            ...previewConfig.manualThirdPartyModelsByProvider,
            primary: manualThirdPartyModels,
          },
          upstreamModelsByProvider: {
            ...previewConfig.upstreamModelsByProvider,
            primary: supportedModels,
          },
        };
        const defaultModel = supportedModels.includes(previewModelState.defaultModel)
          ? previewModelState.defaultModel
          : supportedModels[0] || "";
        previewModelState = {
          ...previewModelState,
          officialModels: previewOfficialModels.map((model) => ({
            ...model,
            supported: requestedOfficial.has(model.slug),
          })),
          thirdPartyModels,
          manualThirdPartyModels: manualThirdPartyModels.filter((model) =>
            thirdPartyModels.includes(model)
          ),
          upstreamModels: supportedModels,
          defaultModel,
        };
        return {
          status: "ok",
          config: previewConfig,
          modelState: previewModelState,
          restartRequired: false,
          modelHotReloaded: true,
        };
      }
      if (command === "save_default_model") {
        const model = String(args.model || "");
        previewConfig = {
          ...previewConfig,
          defaultModelByProvider: {
            ...previewConfig.defaultModelByProvider,
            primary: model,
          },
        };
        previewModelState = { ...previewModelState, defaultModel: model };
        return {
          status: "ok",
          config: previewConfig,
          modelState: previewModelState,
          restartRequired: false,
          modelHotReloaded: true,
        };
      }
      if (command === "restart_codey") {
        return { status: "restarting" };
      }
      if (command === "check_for_updates") {
        return {
          currentVersion: "0.1.0",
          latestVersion: "0.2.0",
          updateAvailable: true,
          selectedAsset: {
            platform: "macos",
            arch: "arm64",
            packageType: "app-zip",
            fileName: "Codey-0.2.0-macos-arm64-unsigned.zip",
            url: "https://updates.example.com/releases/v0.2.0/Codey-0.2.0-macos-arm64-unsigned.zip",
            sha256:
              "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            size: 31_911_421,
          },
        };
      }
      if (command === "download_update") {
        return {
          latestVersion: "0.2.0",
          filePath: "/tmp/codey-updates/Codey-0.2.0-macos-arm64-unsigned.zip",
          fileName: "Codey-0.2.0-macos-arm64-unsigned.zip",
          size: 31_911_421,
          sha256:
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          asset: {
            platform: "macos",
            arch: "arm64",
            packageType: "app-zip",
            fileName: "Codey-0.2.0-macos-arm64-unsigned.zip",
            url: "https://updates.example.com/releases/v0.2.0/Codey-0.2.0-macos-arm64-unsigned.zip",
            sha256:
              "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            size: 31_911_421,
          },
        };
      }
      if (command === "install_downloaded_update") {
        return { status: "installing" };
      }
      if (command === "test_webhook") {
        return { status: 200 };
      }
      if (command === "test_notification_channel") {
        const channel = args.channel as {
          kind?: string;
          url?: string;
          botToken?: string;
          chatId?: string;
        } | undefined;
        const configured = channel?.kind === "telegram"
          ? Boolean(channel.botToken?.trim() && channel.chatId?.trim())
          : Boolean(channel?.url?.trim());
        return configured
          ? { status: "ok", eventId: "preview-notification-test" }
          : { status: "failed", message: "请先完成渠道配置" };
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
