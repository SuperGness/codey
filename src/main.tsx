import React from "react";
import ReactDOM from "react-dom/client";
import "../node_modules/@douyinfe/semi-ui/lib/es/_base/base.css";
import { App } from "./App";
import {
  includesModelId,
  modelIdsEqual,
  modelKey,
  uniqueModelIds,
} from "./modelIds";
import { previewOfficialModels, previewUpstreamModels } from "./previewModels";
import {
  previewCrashpadPendingStats,
  previewTraceLogStats,
} from "./previewTraceLogStats";
import "./styles.css";
import "./styles.operations.css";
import "./styles.models.css";
import "./styles.features.css";
import "./styles.diagnostics.css";
import "./styles.components.css";
import "./styles.responsive.css";
import "./styles.workflows.css";

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
    const previewApiKey = "preview-only-not-a-secret";
    let previewConfig = {
      settingsRevision: 0,
      activeProfileId: "primary",
      profiles: [
        {
          id: "primary",
          name: "主力代理 (ChatGPT)",
          baseUrl: previewEndpoints.primary,
          apiKey: previewApiKey,
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
      promptOptimization: {
        enabled: true,
        baseUrl: previewEndpoints.primary,
        apiKey: "",
        apiKeyConfigured: true,
        clearApiKey: false,
        model: "gpt-5.6-sol",
        protocol: "responses" as const,
        instruction: "",
      },
      codexAppPath: "/Applications/ChatGPT.app",
      userScripts: [],
      selectedModelsByProvider: {
        primary: ["provider-fast-coder", "claude-sonnet-4-5"],
      },
      manualThirdPartyModelsByProvider: {
        primary: ["provider-fast-coder"],
      },
      declaredOfficialModelsByProvider: {} as Record<string, string[]>,
      upstreamModelsByProvider: { primary: previewUpstreamModels },
      defaultModelByProvider: {},
      disableTraceLogWrites: true,
      protectCrashpadPending: true,
      slimCodexPet: true,
      gpuLaunchMode: "off" as const,
      fastContextTools: false,
      fastCodexStartup: true,
      subagentOptimization: false,
      subagentModel: "gpt-5.6-terra",
      subagentReasoningEffort: "medium",
      subagentRoles: {
        codey_quick_scan: { model: "gpt-5.6-sol", reasoningEffort: "low" },
        codey_deep_research: { model: "gpt-5.6-sol", reasoningEffort: "high" },
        codey_visual_analysis: { model: "gpt-5.6-sol", reasoningEffort: "high" },
        codey_worker: { model: "provider-fast-coder", reasoningEffort: "medium" },
        codey_visual_worker: { model: "gpt-5.6-sol", reasoningEffort: "high" },
        default: { model: "gpt-5.6-sol", reasoningEffort: "medium" },
      },
      workflow: {
        enabled: false,
        globalMode: true,
        profile: "qualityFirst",
        maxReadOnlyConcurrency: 4,
        maxProviderConcurrency: 2,
        maxRepoWriters: 1,
        maxDelegationDepth: 1,
        leaseSeconds: 60,
        infrastructureRetryLimit: 3,
        builderRepairLimit: 2,
        reviewerCount: 1,
        retentionDays: 30,
        roles: {
          preflight: { model: "gpt-5.6-sol", reasoningEffort: "high" },
          scout: { model: "gpt-5.6-sol", reasoningEffort: "medium" },
          builder: { model: "provider-fast-coder", reasoningEffort: "high" },
          validator: { model: "gpt-5.6-sol", reasoningEffort: "medium" },
          reviewer: { model: "gpt-5.6-sol", reasoningEffort: "high" },
          expert: { model: "gpt-5.6-terra", reasoningEffort: "high" },
        },
      },
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
        supported: includesModelId(previewUpstreamModels, model.slug),
      })),
      officialModelIds: previewOfficialModels.map((model) => model.slug),
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
          fastContextToolsStatus: {
            userConfigured: false,
            detectionFailed: false,
          },
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
            sessionFilesFixed: 3,
            sqliteRowsUpdated: 7,
            ghostTasksPruned: 2,
            performanceStatus: "ready",
            performanceDetail:
              previewClientPlatform === "windows"
                ? "Windows 启动补丁已安装：WMI 周期采样保护等待运行时确认，临时 WebView 与执行环境回收已启用"
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
              id: "windows-wmi-sampler",
              name: "Windows WMI 周期采样保护",
              source: "builtin",
              status: "effective",
              detail: "已阻止 2 次 WMI 周期进程采样",
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
          fastContextToolsStatus: {
            userConfigured: false,
            detectionFailed: false,
          },
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
      if (command === "reveal_prompt_optimization_api_key") {
        return previewConfig.promptOptimization.apiKeyConfigured
          ? { apiKey: previewApiKey }
          : {
              status: "failed",
              message: "提示词优化 API Key 尚未保存",
            };
      }
      if (command === "sync_current_provider") {
        return {
          config: previewConfig,
          modelState: previewModelState,
          ccSwitch: previewCcSwitch,
          restartRequired: false,
        };
      }
      if (command === "sync_prompt_optimization_current_provider") {
        previewConfig = {
          ...previewConfig,
          settingsRevision: previewConfig.settingsRevision + 1,
          promptOptimization: {
            ...previewConfig.promptOptimization,
            baseUrl: previewCcSwitch.provider.baseUrl,
            apiKey: "",
            apiKeyConfigured: true,
            clearApiKey: false,
            model: previewModelState.defaultModel,
            protocol: previewCcSwitch.provider.protocol,
          },
        };
        return { config: previewConfig };
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
        const declaredOfficialModels =
          previewConfig.declaredOfficialModelsByProvider.primary || [];
        const effectiveModels = uniqueModelIds([
          ...previewUpstreamModels,
          ...declaredOfficialModels,
        ]);
        previewModelState = {
          ...previewModelState,
          officialModels: previewOfficialModels.map((model) => ({
            ...model,
            supported: includesModelId(effectiveModels, model.slug),
          })),
          thirdPartyModels: previewModelState.thirdPartyModels.filter((model) =>
            includesModelId(effectiveModels, model)
          ),
          manualThirdPartyModels: previewModelState.manualThirdPartyModels.filter((model) =>
            !includesModelId(previewUpstreamModels, model)
          ),
          upstreamModels: effectiveModels,
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
        const requestedOfficial = new Set(officialModels.map(modelKey));
        const supportedModels = uniqueModelIds([
          ...officialModels,
          ...thirdPartyModels,
        ]);
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
          declaredOfficialModelsByProvider: {
            ...previewConfig.declaredOfficialModelsByProvider,
            primary: officialModels,
          },
          upstreamModelsByProvider: {
            ...previewConfig.upstreamModelsByProvider,
            primary: supportedModels,
          },
        };
        const defaultModel = supportedModels.find((model) =>
          modelIdsEqual(model, previewModelState.defaultModel)
        )
          ?? supportedModels[0]
          ?? "";
        previewModelState = {
          ...previewModelState,
          officialModels: previewOfficialModels.map((model) => ({
            ...model,
            supported: requestedOfficial.has(modelKey(model.slug)),
          })),
          thirdPartyModels,
          manualThirdPartyModels: manualThirdPartyModels.filter((model) =>
            includesModelId(thirdPartyModels, model)
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
      if (command === "fetch_prompt_optimization_models") {
        return { models: previewModelState.upstreamModels };
      }
      if (command === "test_prompt_optimization") {
        return {
          status: "ok",
          result: { httpStatus: 200, responsePreview: "preview" },
        };
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
