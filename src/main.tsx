import React from "react";
import ReactDOM from "react-dom/client";
import { MantineProvider } from "@mantine/core";
import "@mantine/core/styles.css";
import { App } from "./App";
import type { ProviderStatus, Config, ModelState, Profile } from "./App.types";
import {
  AUTO_REVIEW_MODEL,
  includesModelId,
  modelIdsEqual,
  modelKey,
  uniqueModelIds,
} from "./modelIds";
import { routeModelAlias } from "./modelRoutes";
import { previewOfficialModels, previewUpstreamModels } from "./previewModels";
import { codeyMantineTheme } from "./mantine";
import "./tailwind.css";
import {
  previewCrashpadPendingStats,
  previewTraceLogStats,
} from "./previewTraceLogStats";
import "./styles.css";
import "./styles.operations.css";
import "./styles.models.css";
import "./styles.features.css";
import "./styles.diagnostics.css";
import "./styles.responsive.css";

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
      wecom: "https://webhook.example.invalid/wecom/preview-only?key=preview",
    } as const;
    let previewConfig: Config = {
      settingsRevision: 0,
      localRouterEnabled: true,
      routeRequestLog: {
        enabled: true,
        backend: "sqlite",
        queueCapacity: 8192,
        batchSize: 256,
        flushIntervalMs: 1000,
        shutdownFlushTimeoutMs: 1500,
        sampleRatePerMillion: 1_000_000,
        maxFileBytes: 134_217_728,
        retainedFiles: 7,
        retentionDays: 30,
      },
      activeProfileId: "primary",
      initialRouteImportCompleted: true,
      profiles: [
        {
          id: "primary",
          name: "主力代理 (ChatGPT)",
          shortName: "主",
          baseUrl: previewEndpoints.primary,
          apiKey: "preview-route-primary-key",
          upstreamProtocol: "openaiResponses",
          authMode: "apiKey",
          apiKeyConfigured: true,
          clearApiKey: false,
          sourceProviderId: "primary",
          officialAccount: false,
          supportsRemoteCompaction: false,
          supportsNativeWebSearch: false,
          supportsAutoReview: false,
        },
        {
          id: "backup",
          name: "备用中转 (Claude)",
          shortName: "备",
          baseUrl: previewEndpoints.backup,
          apiKey: "preview-route-backup-key",
          upstreamProtocol: "openaiChatCompletions",
          authMode: "apiKey",
          apiKeyConfigured: true,
          clearApiKey: false,
          sourceProviderId: "backup",
          officialAccount: false,
          supportsRemoteCompaction: false,
          supportsNativeWebSearch: false,
          supportsAutoReview: false,
        },
      ],
      webhook: {
        channels: [
          {
            id: "preview-feishu",
            kind: "feishu" as const,
            enabled: true,
            url: previewEndpoints.feishu,
            urlConfigured: true,
            clearUrl: false,
            botToken: "",
            botTokenConfigured: false,
            clearBotToken: false,
            contextToken: "",
            contextTokenConfigured: false,
            clearContextToken: false,
            chatId: "",
          },
          {
            id: "preview-wecom",
            kind: "wecom" as const,
            enabled: true,
            url: previewEndpoints.wecom,
            urlConfigured: true,
            clearUrl: false,
            botToken: "",
            botTokenConfigured: false,
            clearBotToken: false,
            contextToken: "",
            contextTokenConfigured: false,
            clearContextToken: false,
            chatId: "",
          },
          {
            id: "preview-telegram",
            kind: "telegram" as const,
            enabled: false,
            url: "",
            urlConfigured: false,
            clearUrl: false,
            botToken: "",
            botTokenConfigured: true,
            clearBotToken: false,
            contextToken: "",
            contextTokenConfigured: false,
            clearContextToken: false,
            chatId: "preview-chat-id",
          },
        ],
      },
      promptOptimization: {
        enabled: true,
        mode: "codeyRoute",
        baseUrl: previewEndpoints.primary,
        apiKey: "preview-prompt-optimization-key",
        apiKeyConfigured: true,
        clearApiKey: false,
        model: "primary/provider-fast-coder",
        upstreamProtocol: "openaiResponses",
        instruction: "",
      },
      codexAppPath: "/Applications/ChatGPT.app",
      userScripts: [],
      selectedModelsByProvider: {
        primary: ["provider-fast-coder", "claude-sonnet-4-5"],
        backup: ["claude-sonnet-4-5", "claude-opus-4-1"],
      },
      manualThirdPartyModelsByProvider: {
        primary: ["provider-fast-coder"],
      },
      declaredOfficialModelsByProvider: {} as Record<string, string[]>,
      upstreamModelsByProvider: {
        primary: previewUpstreamModels,
        backup: ["claude-sonnet-4-5", "claude-opus-4-1"],
      },
      defaultModel: "primary/provider-fast-coder",
      disableTraceLogWrites: true,
      protectCrashpadPending: true,
      slimCodexPet: true,
      gpuLaunchMode: "off" as const,
      fastContextTools: false,
      subagentOptimization: false,
      subagentModel: "gpt-5.6-terra",
      subagentReasoningEffort: "medium",
      subagentRoles: {
        codey_quick_scan: { enabled: true, model: "gpt-5.6-sol", reasoningEffort: "low" },
        codey_deep_research: { enabled: true, model: "gpt-5.6-sol", reasoningEffort: "high" },
        codey_visual_analysis: {
          enabled: true,
          model: "backup/claude-sonnet-4-5",
          reasoningEffort: "high",
        },
        codey_worker: { enabled: true, model: "provider-fast-coder", reasoningEffort: "medium" },
        codey_visual_worker: { enabled: true, model: "gpt-5.6-sol", reasoningEffort: "high" },
        default: { enabled: true, model: "gpt-5.6-sol", reasoningEffort: "medium" },
      },
      hideFullAccessWarning: false,
      showAccountUsageInHeader: true,
    };
    let previewModelState: ModelState = {
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
    const routeProviderId = (profile: Profile) =>
      profile.sourceProviderId || profile.id;
    const activePreviewProfile = () =>
      previewConfig.profiles.find(
        (profile) => profile.id === previewConfig.activeProfileId,
      ) || previewConfig.profiles[0];
    const previewProviderStatus = (): ProviderStatus => {
      const profile = activePreviewProfile();
      return {
        changed: false,
        provider: {
          id: profile ? routeProviderId(profile) : "openai",
          name: profile?.name || "OpenAI 官方直登",
          official: profile?.authMode === "officialAccount",
          baseUrl: profile?.baseUrl || "",
        },
      };
    };
    const previewModelStateForProfile = (profile: Profile): ModelState => {
      const providerId = routeProviderId(profile);
      const official = profile.authMode === "officialAccount";
      const upstream = previewConfig.upstreamModelsByProvider[providerId] || [];
      const selected = previewConfig.selectedModelsByProvider[providerId] || [];
      const manual = previewConfig.manualThirdPartyModelsByProvider[providerId] || [];
      const selectableOfficial = previewOfficialModels.filter(
        (model) => official && (selected.length === 0 || includesModelId(selected, model.slug)),
      );
      const thirdPartyModels = official ? [] : selected;
      const requestedDefault = previewConfig.defaultModel;
      const defaultModel =
        [
          ...selectableOfficial.map((model) => model.slug),
          ...thirdPartyModels,
        ].find(
          (model) =>
            Boolean(requestedDefault) &&
            modelIdsEqual(routeModelAlias(profile, model), requestedDefault),
        ) ||
        selectableOfficial[0]?.slug ||
        thirdPartyModels[0] ||
        "";
      return {
        officialModels: official ? previewOfficialModels.map((model) => ({
          ...model,
          supported: selected.length === 0 || includesModelId(selected, model.slug),
        })) : [],
        officialModelIds: previewOfficialModels.map((model) => model.slug),
        thirdPartyModels,
        manualThirdPartyModels: manual.filter((model) =>
          includesModelId(thirdPartyModels, model),
        ),
        upstreamModels: official ? [] : upstream,
        defaultModel,
      };
    };
    const refreshPreviewModelState = () => {
      const profile = activePreviewProfile();
      if (profile) previewModelState = previewModelStateForProfile(profile);
    };
    let previewTraceStats: typeof previewTraceLogStats | undefined;
    let previewCrashpadStats:
      | typeof previewCrashpadPendingStats
      | undefined = previewCrashpadPendingStats;
    const previewRouteRequestLogs = Array.from({ length: 47 }, (_, index) => {
      const primary = index % 3 !== 1;
      const failed = index % 9 === 4;
      const protocol = (["sse", "ws", "http"] as const)[index % 3];
      const inputTokens = 1_200 + index * 137;
      const outputTokens = failed ? undefined : 240 + index * 29;
      const cachedInputTokens = index % 4 === 0 ? 640 + index * 11 : undefined;
      const codexSessionIsParent = index % 8 === 0;
      return {
        requestId: `preview-request-${String(index + 1).padStart(4, "0")}`,
        traceId: `preview-trace-${String(index + 1).padStart(4, "0")}`,
        codexSessionId: index % 10 === 9
          ? null
          : `preview-${codexSessionIsParent ? "parent-" : ""}session-${String(index + 1).padStart(4, "0")}`,
        codexSessionIsParent,
        timestampUnixMs: Date.now() - index * 83_000,
        provider: primary ? "primary" : "backup",
        providerName: primary ? "主力代理 (ChatGPT)" : "备用中转 (Claude)",
        requestedModel: primary ? "provider-fast-coder" : "claude-sonnet-4-5",
        model: primary ? "provider-fast-coder" : "claude-sonnet-4-5",
        reasoningEffort: (["low", "medium", "high"] as const)[index % 3],
        thinkingBudgetTokens: undefined,
        ttftMs: failed ? undefined : 190 + index * 13,
        routerPreUpstreamMs: failed ? undefined : 12 + index,
        upstreamFirstByteMs: failed ? undefined : 190 + index * 13,
        downstreamFirstContentMs: failed ? undefined : 215 + index * 14,
        upstreamHeaderMs: 120 + index * 8,
        totalDurationMs: failed ? 1_640 + index * 31 : 2_350 + index * 91,
        queueDelayMs: index % 5,
        inputTokens,
        outputTokens,
        cachedInputTokens,
        cacheCreationInputTokens: undefined,
        reasoningOutputTokens: outputTokens ? Math.floor(outputTokens / 4) : undefined,
        totalTokens: outputTokens == null ? inputTokens : inputTokens + outputTokens,
        usageReported: !failed,
        usageUnavailableReason: failed ? "upstream_error" : undefined,
        requestProtocol: protocol,
        upstreamTransport: protocol === "ws" ? "ws" : "http",
        requestKind: "responses",
        status: failed ? "failed" : "succeeded",
        statusCode: failed ? 502 : 200,
        upstreamStatusCode: failed ? 429 : 200,
        errorCode: failed ? "upstream_rate_limited" : undefined,
        upstreamErrorSummary: failed
          ? "供应商额度不足，请稍后重试或切换备用线路（代码：rate_limit_exceeded）"
          : undefined,
        completionReason: failed ? undefined : "completed",
        fallbackCount: failed ? 1 : 0,
        fallbackReason: failed ? "rate_limited" : undefined,
        upstreamAuthority: primary ? "primary.example.invalid" : "backup.example.invalid",
        upstreamRequestId: `upstream-preview-${index + 1}`,
        upstreamProtocol: primary ? "openaiResponses" : "openaiChatCompletions",
        protocolBridge: primary ? undefined : "chat_completions_to_responses",
        firstByteSource: protocol === "http" ? "headers" : "stream",
        subagent: index % 6 === 0,
      };
    });

    window.__codeyInvokeApi = async (command, args) => {
      console.log(`[Mock API Call] ${command}`, args);
      // Wait a tiny bit to simulate network delay
      await new Promise((resolve) => setTimeout(resolve, 300));

      if (command === "load_codey_config") {
        return {
          config: previewConfig,
          modelState: previewModelState,
          startupError: undefined,
          providerStatus: previewProviderStatus(),
          fastContextToolsStatus: {
            userConfigured: false,
            detectionFailed: false,
          },
        };
      }
      if (command === "runtime_status") {
        const activeNotificationChannelCount =
          previewConfig.webhook.channels.filter(
            (channel) =>
              channel.enabled &&
              (channel.kind === "telegram" || channel.kind === "wechatClaw"
                ? channel.botTokenConfigured &&
                  Boolean(channel.chatId.trim()) &&
                  (channel.kind !== "wechatClaw" ||
                    (channel.sessionStatus !== "expired" &&
                      channel.urlConfigured &&
                      channel.contextTokenConfigured))
                : channel.urlConfigured),
          ).length;
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
              visibility: "internal",
              status: "effective",
              detail: "桥接函数可调用",
            },
            ...(previewClientPlatform === "windows"
              ? [
                  {
                    id: "windows-wmi-sampler",
                    name: "Windows WMI 周期采样保护",
                    source: "builtin" as const,
                    visibility: "feature" as const,
                    status: "effective" as const,
                    detail: "已阻止 2 次 WMI 周期进程采样",
                  },
                ]
              : []),
            {
              id: "model-whitelist",
              name: "模型白名单",
              source: "builtin",
              visibility: "internal",
              status: "effective",
              detail: "模型目录已加载（5 个模型）",
            },
            {
              id: "pet-control-shield",
              name: "宠物控制精简",
              source: "builtin",
              visibility: "feature",
              status: "effective",
              detail: "宠物控制精简已启用",
            },
            {
              id: "security-warning-shield",
              name: "安全提示控制",
              source: "builtin",
              visibility: "feature",
              status: "inactive",
              detail: "控制器已就绪，当前屏蔽策略关闭",
            },
            {
              id: "settings-overlay-loader",
              name: "配置面板加载器",
              source: "builtin",
              visibility: "internal",
              status: "effective",
              detail: "配置面板按需加载器可用",
            },
            {
              id: "renderer-controls",
              name: "渲染器控制",
              source: "builtin",
              visibility: "internal",
              status: "effective",
              detail: "渲染器控制与按需加载 API 可用",
            },
            {
              id: "plugin-marketplace-compatibility",
              name: "插件市场兼容",
              source: "builtin",
              visibility: "internal",
              status: "effective",
              detail: "插件市场桥接已接管",
            },
          ],
          fastContextToolsActive: previewConfig.fastContextTools,
          subagentOptimizationActive: previewConfig.subagentOptimization,
          notificationChannelsActive: activeNotificationChannelCount > 0,
          activeNotificationChannelCount,
          traceLogWriteProtectionActive: previewConfig.disableTraceLogWrites,
          crashpadDiskProtectionActive:
            previewClientPlatform === "macos" &&
            previewConfig.protectCrashpadPending,
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
      if (command === "query_route_request_logs") {
        const page = Math.max(1, Number(args.page) || 1);
        const pageSize = Math.min(100, Math.max(1, Number(args.pageSize) || 20));
        const search = String(args.search || "").trim().toLocaleLowerCase();
        const provider = String(args.provider || "");
        const model = String(args.model || "");
        const status = String(args.status || "");
        const protocol = String(args.protocol || "");
        const filtered = previewRouteRequestLogs.filter((item) => {
          if (provider && item.provider !== provider && item.providerName !== provider) return false;
          if (model && item.model !== model && item.requestedModel !== model) return false;
          if (status && item.status !== status) return false;
          if (protocol && item.requestProtocol !== protocol) return false;
          if (!search) return true;
          return [
            item.requestId,
            item.traceId,
            item.codexSessionId,
            item.provider,
            item.providerName,
            item.model,
            item.upstreamAuthority,
            item.upstreamErrorSummary,
          ].some((value) => value?.toLocaleLowerCase().includes(search));
        });
        const totalPages = Math.ceil(filtered.length / pageSize);
        return {
          status: "ok",
          backend: "sqlite",
          queryable: true,
          page,
          pageSize,
          total: filtered.length,
          totalPages,
          items: filtered.slice((page - 1) * pageSize, page * pageSize),
        };
      }
      if (command === "clear_route_request_logs") {
        const hadLogs = previewRouteRequestLogs.length > 0;
        previewRouteRequestLogs.length = 0;
        return {
          status: "ok",
          removedFileCount: hadLogs ? 1 : 0,
          removedFiles: hadLogs ? ["route-requests.sqlite3"] : [],
          recordingEnabled: true,
          recordingActive: true,
          recordingRestarted: true,
        };
      }
      if (command === "save_codey_config") {
        const incoming = args.config as Config;
        previewConfig = {
          ...incoming,
          profiles: incoming.profiles.map((profile) => ({
            ...profile,
            apiKey: profile.clearApiKey ? "" : profile.apiKey,
            apiKeyConfigured: !profile.clearApiKey && Boolean(profile.apiKey.trim()),
            clearApiKey: false,
          })),
          promptOptimization: {
            ...incoming.promptOptimization,
            apiKey: incoming.promptOptimization.clearApiKey
              ? ""
              : incoming.promptOptimization.apiKey,
            apiKeyConfigured:
              !incoming.promptOptimization.clearApiKey &&
              Boolean(incoming.promptOptimization.apiKey.trim()),
            clearApiKey: false,
          },
          settingsRevision: previewConfig.settingsRevision + 1,
        };
        refreshPreviewModelState();
        return {
          config: previewConfig,
          modelState: previewModelState,
          providerStatus: previewProviderStatus(),
          fastContextToolsStatus: {
            userConfigured: false,
            detectionFailed: false,
          },
          restartRequired: false,
        };
      }
      if (command === "sync_current_provider") {
        return {
          config: previewConfig,
          modelState: previewModelState,
          providerStatus: previewProviderStatus(),
          restartRequired: false,
        };
      }
      if (command === "delete_route" || command === "fetch_route_models") {
        const expectedRevision = Number(args.expectedRevision);
        if (expectedRevision !== previewConfig.settingsRevision) {
          return {
            status: "failed",
            message: "Codey 设置已被其他操作更新，请重新载入后再操作线路",
          };
        }
      }
      if (command === "delete_route") {
        const routeId = String(args.routeId || "");
        const route = previewConfig.profiles.find((profile) => profile.id === routeId);
        if (!route) return { status: "failed", message: "找不到要删除的线路" };
        if (previewConfig.profiles.length <= 1) {
          return { status: "failed", message: "至少需要保留一条线路" };
        }
        const providerId = routeProviderId(route);
        const profiles = previewConfig.profiles.filter((profile) => profile.id !== routeId);
        delete previewConfig.selectedModelsByProvider[providerId];
        delete previewConfig.manualThirdPartyModelsByProvider[providerId];
        delete previewConfig.declaredOfficialModelsByProvider[providerId];
        delete previewConfig.upstreamModelsByProvider[providerId];
        previewConfig = {
          ...previewConfig,
          settingsRevision: previewConfig.settingsRevision + 1,
          profiles,
          activeProfileId:
            previewConfig.activeProfileId === routeId
              ? profiles[0].id
              : previewConfig.activeProfileId,
        };
        refreshPreviewModelState();
        return {
          status: "ok",
          config: previewConfig,
          modelState: previewModelState,
          providerStatus: previewProviderStatus(),
          restartRequired: false,
          modelHotReloaded: true,
        };
      }
      if (command === "fetch_route_models") {
        const routeId = String(args.routeId || "");
        const route = previewConfig.profiles.find((profile) => profile.id === routeId);
        if (!route) return { status: "failed", message: "找不到要同步模型的线路" };
        const providerId = routeProviderId(route);
        const fetchedModels = uniqueModelIds([
          ...previewUpstreamModels,
          ...(providerId === "backup" ? ["claude-sonnet-4-5"] : []),
        ]);
        const supportsAutoReview = includesModelId(
          fetchedModels,
          AUTO_REVIEW_MODEL,
        );
        const models = fetchedModels.filter(
          (model) => !modelIdsEqual(model, AUTO_REVIEW_MODEL),
        );
        previewConfig = {
          ...previewConfig,
          settingsRevision: previewConfig.settingsRevision + 1,
          profiles: previewConfig.profiles.map((profile) =>
            profile.id === routeId
              ? { ...profile, supportsAutoReview }
              : profile,
          ),
          upstreamModelsByProvider: {
            ...previewConfig.upstreamModelsByProvider,
            [providerId]: models,
          },
        };
        refreshPreviewModelState();
        return {
          status: "ok",
          config: previewConfig,
          modelState: previewModelState,
          routeModelState: previewModelStateForProfile(route),
          providerStatus: previewProviderStatus(),
          models,
          restartRequired: false,
          modelHotReloaded: true,
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
          traceLogWriteProtectionActive: previewConfig.disableTraceLogWrites,
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
      if (command === "save_selected_models") {
        const routeId = String(args.routeId || "");
        const targetProfile = previewConfig.profiles.find(
          (profile) => profile.id === routeId,
        ) || activePreviewProfile();
        const providerId = targetProfile ? routeProviderId(targetProfile) : "primary";
        const officialModels = (args.officialModels as string[]) || [];
        const thirdPartyModels = (args.thirdPartyModels as string[]) || [];
        const manualThirdPartyModels = (args.manualThirdPartyModels as string[]) || [];
        const supportsAutoReview =
          typeof args.supportsAutoReview === "boolean"
            ? args.supportsAutoReview
            : targetProfile?.supportsAutoReview === true;
        const supportedModels = uniqueModelIds([
          ...officialModels,
          ...thirdPartyModels,
        ]).filter((model) => !modelIdsEqual(model, AUTO_REVIEW_MODEL));
        previewConfig = {
          ...previewConfig,
          settingsRevision: previewConfig.settingsRevision + 1,
          profiles: previewConfig.localRouterEnabled ? previewConfig.profiles.map((profile) =>
            profile.id === targetProfile?.id
              ? { ...profile, supportsAutoReview }
              : profile,
          ) : previewConfig.profiles,
          selectedModelsByProvider: {
            ...previewConfig.selectedModelsByProvider,
            [providerId]: (targetProfile?.authMode === "officialAccount" ? officialModels : thirdPartyModels).filter(
              (model) => !modelIdsEqual(model, AUTO_REVIEW_MODEL),
            ),
          },
          manualThirdPartyModelsByProvider: {
            ...previewConfig.manualThirdPartyModelsByProvider,
            [providerId]: manualThirdPartyModels.filter(
              (model) => !modelIdsEqual(model, AUTO_REVIEW_MODEL),
            ),
          },
          declaredOfficialModelsByProvider: {
            ...previewConfig.declaredOfficialModelsByProvider,
            [providerId]: previewConfig.localRouterEnabled ? officialModels : [],
          },
          upstreamModelsByProvider: {
            ...previewConfig.upstreamModelsByProvider,
            [providerId]: previewConfig.localRouterEnabled ? supportedModels : uniqueModelIds([
              ...(previewConfig.upstreamModelsByProvider[providerId] || []),
              ...supportedModels,
            ]),
          },
        };
        refreshPreviewModelState();
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
        const routeId = String(args.routeId || "");
        const targetProfile = previewConfig.profiles.find(
          (profile) => profile.id === routeId,
        ) || activePreviewProfile();
        if (!targetProfile) {
          return { status: "failed", message: "找不到要设置默认模型的线路" };
        }
        previewConfig = {
          ...previewConfig,
          activeProfileId: targetProfile.id,
          defaultModel: routeModelAlias(targetProfile, model),
        };
        if (targetProfile?.id === previewConfig.activeProfileId) {
          previewModelState = { ...previewModelState, defaultModel: model };
        }
        return {
          status: "ok",
          config: previewConfig,
          modelState: previewModelState,
          restartRequired: false,
          modelHotReloaded: true,
        };
      }
      if (command === "save_official_route_models") {
        const routeId = String(args.routeId || "");
        const models = uniqueModelIds((args.models as string[]) || []);
        const targetProfile = previewConfig.profiles.find(
          (profile) => profile.id === routeId,
        );
        if (!targetProfile || targetProfile.authMode !== "officialAccount" || models.length === 0) {
          return { status: "failed", message: "官方线路至少需要保留一个模型" };
        }
        const providerId = routeProviderId(targetProfile);
        previewConfig = {
          ...previewConfig,
          selectedModelsByProvider: {
            ...previewConfig.selectedModelsByProvider,
            [providerId]: models,
          },
        };
        const defaultModel = models.find((candidate) =>
          modelIdsEqual(routeModelAlias(targetProfile, candidate), previewConfig.defaultModel),
        ) || models[0];
        if (!models.some((candidate) =>
          modelIdsEqual(routeModelAlias(targetProfile, candidate), previewConfig.defaultModel),
        )) {
          previewConfig = {
            ...previewConfig,
            defaultModel: routeModelAlias(targetProfile, defaultModel),
          };
        }
        if (targetProfile.id === previewConfig.activeProfileId) {
          const selected = new Set(models.map(modelKey));
          previewModelState = {
            ...previewModelState,
            officialModels: previewOfficialModels.map((model) => ({
              ...model,
              supported: selected.has(modelKey(model.slug)),
            })),
            defaultModel,
          };
        }
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
      if (command === "test_notification_channel") {
        const channel = args.channel as {
          kind?: string;
          url?: string;
          botToken?: string;
          contextToken?: string;
          chatId?: string;
        } | undefined;
        const configured = channel?.kind === "telegram" || channel?.kind === "wechatClaw"
          ? Boolean(
            channel.botToken?.trim() &&
              channel.chatId?.trim() &&
              (channel.kind !== "wechatClaw" ||
                (channel.url?.trim() && channel.contextToken?.trim())),
          )
          : Boolean(channel?.url?.trim());
        return configured
          ? { status: "ok", eventId: "preview-notification-test" }
          : { status: "failed", message: "请先完成渠道配置" };
      }
      if (command === "start_wechat_claw_login") {
        return {
          loginId: "preview-wechat-claw-login",
          status: "wait",
          qrCode: "preview-wechat-claw-qr-code",
          qrCodeImageUrl: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='192' height='192' viewBox='0 0 12 12'%3E%3Crect width='12' height='12' fill='white'/%3E%3Cpath d='M1 1h3v3H1zm7 0h3v3H8zM1 8h3v3H1zm4-4h2v2H5zm1 3h2v2H6zm3 2h2v2H9zM4 8h1v3H4zm5-3h2v1H9z' fill='%231d1d1f'/%3E%3C/svg%3E",
        };
      }
      if (command === "poll_wechat_claw_login") {
        return {
          status: "confirmed",
          baseUrl: "https://ilinkai.weixin.qq.com",
          botToken: "preview-wechat-claw-token",
          recipientId: "preview-user@im.wechat",
          contextToken: "preview-wechat-claw-context",
        };
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
    <MantineProvider
      cssVariablesSelector="#root"
      forceColorScheme="light"
      theme={codeyMantineTheme}
    >
      <App />
    </MantineProvider>
  </React.StrictMode>,
);
