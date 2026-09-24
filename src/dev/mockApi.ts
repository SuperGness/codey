// Development-only preview data and a mock Codey bridge API. Loaded from
// main.tsx via a dynamic import that only exists in Vite dev builds, so this
// module never ships in the production overlay.
import type { ProviderStatus, Config, ModelState, OfficialAccount, Profile } from "../App.types";
import { pluginConfigBusinessValuesEqual, validatePluginConfigText, type CodeyPlugin } from "../codeyPlugins";
import { createCodexExtensionsPreview } from "./codexExtensionsMock";
import {
  AUTO_REVIEW_MODEL,
  includesModelId,
  modelIdsEqual,
  modelKey,
  uniqueModelIds,
} from "../modelIds";
import { routeModelAlias } from "../modelRoutes";
import { previewOfficialModels, previewUpstreamModels } from "../previewModels";
import {
  previewCrashpadPendingStats,
  previewTraceLogStats,
} from "../previewTraceLogStats";

// 在 Vite 开发模式下，若未通过 Codey Bridge/Token 访问，自动注入 Mock 接口方便 UI 调试
if (import.meta.env.DEV) {
  if (!window.__codeyInvokeApi) {
    console.log("[Dev Mode] Auto-injecting Codey Mock API");
    const previewClientPlatform =
      new URLSearchParams(window.location.search).get("platform") === "windows"
        ? "windows"
        : new URLSearchParams(window.location.search).get("platform") === "linux"
          ? "linux"
          : "macos";
    let previewInjectionMode = new URLSearchParams(window.location.search).get("injection") === "cli"
      ? "cli" : "node_options";
    let previewInjectionRepairUntil = 0;
    const configRepairPreview = new URLSearchParams(window.location.search).get("configRepair");
    let previewConfigLoadFailed = configRepairPreview === "load-failure";
    const previewEndpoints = {
      primary: "https://primary.example.invalid/v1",
      backup: "https://backup.example.invalid/v1",
      feishu: "https://webhook.example.invalid/feishu/preview-only",
      wecom: "https://webhook.example.invalid/wecom/preview-only?key=preview",
      ntfy: "https://ntfy.example.invalid",
    } as const;
    // 官方线路按存储账号逐条派生；账号没有自定义名称时，使用按添加顺序生成的
    // 默认线路名和短名称，例如第一个账号是「官方账号1」和「官1」。
    const previewOfficialRouteName = (index: number) => `官方账号${index}`;
    const previewOfficialRouteShortName = (index: number) => {
      if (index <= 9) return `官${index}`;
      const letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
      return `官${letters[Math.min(index - 10, letters.length - 1)] ?? "Z"}`;
    };
    const previewAccountRouteIndex = (account: OfficialAccount) => {
      const value = (account.routeName ?? account.routeShortName ?? "").trim();
      const digits = value.startsWith("官方账号")
        ? value.slice("官方账号".length)
        : value.startsWith("官")
          ? value.slice(1)
          : "";
      const index = Number.parseInt(digits, 10);
      return Number.isInteger(index) && index > 0 ? index : null;
    };
    // 与后端保持一致：还没有名称的账号补上当前最小编号，已有名称保持不动。
    const previewEnsureGeneratedRouteSettings = () => {
      const used = new Set<number>();
      for (const account of previewOfficialAccounts) {
        const index = previewAccountRouteIndex(account);
        if (index !== null) used.add(index);
      }
      for (const account of [...previewOfficialAccounts].sort((left, right) => left.addedAt - right.addedAt)) {
        if (account.routeName && account.routeShortName) continue;
        let index = 1;
        while (used.has(index)) index += 1;
        used.add(index);
        account.routeName ??= previewOfficialRouteName(index);
        account.routeShortName ??= previewOfficialRouteShortName(index);
      }
    };
    let previewConfig: Config = {
      settingsRevision: 0,
      autoCheckCodeyUpdates: true,
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
      streamMaxRetries: 5,
      activeProfileId: "primary",
      initialRouteImportCompleted: true,
      profiles: [
        {
          id: "primary",
          enabled: true,
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
          enabled: true,
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
          {
            id: "preview-ntfy",
            kind: "ntfy" as const,
            enabled: true,
            url: previewEndpoints.ntfy,
            urlConfigured: true,
            clearUrl: false,
            botToken: "",
            botTokenConfigured: false,
            clearBotToken: false,
            contextToken: "",
            contextTokenConfigured: false,
            clearContextToken: false,
            chatId: "preview-ntfy-topic",
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
        openai: previewOfficialModels.map((model) => model.slug),
        primary: ["provider-fast-coder", "claude-sonnet-4-5"],
        backup: ["claude-sonnet-4-5", "claude-opus-4-1"],
      },
      modelContextByProvider: {},
      modelReasoningEffortsByProvider: {},
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
      miscModel: "",
      hideFullAccessWarning: false,
      showAccountUsageInHeader: true,
    };
    let previewOfficialAccounts: OfficialAccount[] = [
      { id: "acct_preview_1", email: "preview@example.com", planType: "pro", accountId: "acct_preview_1", addedAt: 1_757_000_000, isDefault: true },
      { id: "acct_preview_2", email: "backup@example.com", planType: "plus", accountId: "acct_preview_2", addedAt: 1_757_100_000, isDefault: false, routeName: "备用官方线路", routeShortName: "备2" },
      // 预览失效账号：卡片标红、隐藏切换默认入口，额度显示失效原因。
      { id: "acct_preview_3", email: "blocked@example.com", planType: "plus", accountId: "acct_preview_3", addedAt: 1_757_200_000, isDefault: false, invalid: true, invalidReason: "官方已撤销该账号的登录凭据，需要重新添加账号" },
    ];
    let previewOfficialLoginPolls = 0;
    const previewDefaultOfficialAccountId = () => previewOfficialAccounts.find((account) => account.isDefault)?.id ?? null;
    const previewOfficialProviderId = (account: { id: string }) =>
      `codey-official-account-${account.id.replace(/[^A-Za-z0-9_-]/g, "-")}`;
    // 预览配置与启动派生保持一致：每个账号一条官方线路，默认账号排在最前。
    const previewDeriveOfficialProfiles = () => {
      previewEnsureGeneratedRouteSettings();
      // 编号按全部账号的添加顺序计算；失效账号不生成线路，也不会让其余线路改号。
      const addedOrder = [...previewOfficialAccounts].sort((left, right) => left.addedAt - right.addedAt);
      const defaultId = previewDefaultOfficialAccountId();
      const ordered = previewOfficialAccounts
        .filter((account) => !account.invalid)
        .sort((left, right) => {
          const leftDefault = left.id === defaultId ? 1 : 0;
          const rightDefault = right.id === defaultId ? 1 : 0;
          return rightDefault - leftDefault || left.addedAt - right.addedAt;
        });
      const officialProfiles: Profile[] = ordered.map((account) => {
        // 默认名称按添加顺序编号，和默认账号排在最前的显示顺序无关。
        const index = addedOrder.findIndex((item) => item.id === account.id) + 1;
        return {
          id: previewOfficialProviderId(account),
          enabled: true,
          name: account.routeName ?? previewOfficialRouteName(index),
          shortName: account.routeShortName ?? previewOfficialRouteShortName(index),
          baseUrl: account.baseUrl ?? "",
          apiKey: "",
          upstreamProtocol: "official",
          authMode: "officialAccount",
          apiKeyConfigured: false,
          clearApiKey: false,
          officialAccount: true,
          officialAccountId: account.id,
          supportsRemoteCompaction: false,
          supportsWebsockets: true,
          supportsNativeWebSearch: true,
          supportsAutoReview: true,
          upstreamProxy: account.upstreamProxy ?? "",
        };
      });
      previewConfig = {
        ...previewConfig,
        profiles: [
          ...officialProfiles,
          ...previewConfig.profiles.filter(
            (profile) => profile.authMode !== "officialAccount",
          ),
        ],
        selectedModelsByProvider: {
          ...previewConfig.selectedModelsByProvider,
          ...Object.fromEntries(
            officialProfiles.map((profile) => [
              profile.id,
              previewOfficialModels.map((model) => model.slug),
            ]),
          ),
        },
      };
    };
    previewDeriveOfficialProfiles();
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
    /// 与后端模板一致：GPT 系列模板额外开放 max/ultra，其余模型使用基础档位。
    const previewReasoningEffortTemplate = (profile: Profile, model: string) => {
      const upstream = routeModelAlias(profile, model);
      const base = ["low", "medium", "high", "xhigh"];
      return ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"].some(
        (slug) => modelIdsEqual(slug, upstream),
      )
        ? [...base, "max", "ultra"]
        : base;
    };
    const previewThirdPartyModelMetadata = (profile: Profile, models: string[]) => {
      const providerId = routeProviderId(profile);
      return models.map((model) => {
        const declared: import("../App.types").ModelReasoningEffort[] = Object.entries(
          previewConfig.modelReasoningEffortsByProvider?.[providerId] ?? {},
        ).find(([name]) => modelIdsEqual(name, model))?.[1] ?? [];
        const autoSupportedReasoningEfforts = previewReasoningEffortTemplate(profile, model);
        const supportedReasoningEfforts = declared.length
          ? uniqueModelIds(declared.map((effort) => effort.value))
          : autoSupportedReasoningEfforts;
        return {
          slug: model,
          supportedReasoningEfforts,
          autoSupportedReasoningEfforts,
          reasoningEfforts: declared,
          defaultReasoningEffort:
            supportedReasoningEfforts.find((effort) => effort === "low") ??
            supportedReasoningEfforts[0] ??
            "low",
        };
      });
    };
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
        thirdPartyModelMetadata: official
          ? []
          : previewThirdPartyModelMetadata(profile, thirdPartyModels),
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
      // 预览里按线路区分官方与第三方请求，官方请求额外带上账号，方便核对
      // 请求日志的账号筛选和周限估算的分账号统计。
      const account = index % 3 === 2
        ? previewOfficialAccounts[index % previewOfficialAccounts.length]
        : null;
      const primary = account == null && index % 3 !== 1;
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
        provider: account ? previewOfficialProviderId(account) : primary ? "primary" : "backup",
        providerName: account ? account.routeName ?? "官方线路" : primary ? "主力代理 (ChatGPT)" : "备用中转 (Claude)",
        officialAccountId: account?.id,
        // 预览同时覆盖请求模型带线路前缀与不带前缀两种形态。
        requestedModel: account ? "gpt-5.6-sol" : primary ? "primary/provider-fast-coder" : "claude-sonnet-4-5",
        model: account ? "gpt-5.6-sol" : primary ? "provider-fast-coder" : "claude-sonnet-4-5",
        // 上游回报实际模型时可能一致、可能不同，失败请求则没有该字段。
        upstreamResponseModel: failed
          ? undefined
          : account
            ? "gpt-5.6-sol"
            : primary
              ? "deepseek/deepseek-v4.1-flash"
              : "claude-sonnet-4-5-20250929",
        reasoningEffort: (["low", "medium", "high"] as const)[index % 3],
        thinkingBudgetTokens: undefined,
        ttftMs: failed ? undefined : 190 + index * 13,
        routerPreUpstreamMs: failed ? undefined : 12 + index,
        upstreamFirstByteMs: failed ? undefined : 190 + index * 13,
        downstreamFirstContentMs: failed ? undefined : 215 + index * 14,
        upstreamHeaderMs: 120 + index * 8,
        totalDurationMs: failed ? 1_640 + index * 31 : 2_350 + index * 91,
        queueDelayMs: index % 5,
        inputTokens: failed ? undefined : inputTokens,
        outputTokens,
        cachedInputTokens: failed ? undefined : cachedInputTokens,
        cacheCreationInputTokens: undefined,
        reasoningOutputTokens: outputTokens ? Math.floor(outputTokens / 4) : undefined,
        totalTokens: failed ? undefined : inputTokens + (outputTokens ?? 0),
        usageReported: !failed,
        usageUnavailableReason: failed ? "upstream_error" : undefined,
        requestProtocol: protocol,
        upstreamTransport: protocol === "sse" ? "http_sse" : protocol,
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
        // 预览同时覆盖正常输入和上游空数组两种形态，便于核对诊断展示。
        requestInputState: "array",
        requestInputItems: 12 + index,
        requestHasPreviousResponseId: index % 3 === 0,
        requestBytes: 4_096 + index * 137,
        upstreamInputState: "array",
        upstreamInputItems: failed ? 0 : 12 + index,
        upstreamHasPreviousResponseId: index % 3 === 0,
        upstreamBytes: 4_096 + index * 137,
        firstByteSource: protocol === "http" ? "headers" : "stream",
        subagent: index % 6 === 0,
      };
    });

    const pluginPreviewMode = new URLSearchParams(window.location.search).get("plugins");
    const previewPlugins: CodeyPlugin[] = ["installed", "config-error", "config-invalid", "config-conflict", "config-values"].includes(pluginPreviewMode ?? "") ? [{
      id: "dev.codey.header-demo", name: "请求头示例", version: "0.1.0",
      logSizeBytes: 1572864,
      description: "演示独立插件的请求头扩展能力。", enabled: false, status: "disabled", restartRequired: false,
      configPath: "/preview/codey-plugins/installed/dev.codey.header-demo/config.json", capabilities: ["request.lifecycle.v1"],
      pluginDir: "/preview/codey-plugins/installed/dev.codey.header-demo", dataDir: "/preview/codey-plugins/installed/dev.codey.header-demo/data", logDir: "/preview/codey-plugins/installed/dev.codey.header-demo/logs",
    }] : [];
    const previewPluginLogTerminals = new Set<string>();
    const previewPluginPackage = {
      path: "/preview/header-demo.codey-plugin",
      sha256: "0123456789abcdef".repeat(4),
      manifest: {
        id: "dev.codey.header-demo",
        name: "请求头示例",
        version: "0.2.0",
        description: "演示独立插件的请求头扩展能力。",
        capabilities: ["request.lifecycle.v1", "request.lifecycle.auth"],
        headerNames: ["x-plugin-demo"],
      },
    };
    let pluginConfigContent = pluginPreviewMode === "config-invalid" ? '{ "value": ' : pluginPreviewMode === "config-values" ? JSON.stringify({
      _comments: {
        value: "请求头使用的文本。这里只能修改值，字段名和说明保留在配置文件中。",
        maxAttempts: "每轮最多尝试次数。达到次数或总时限时停止；修改数字后保存，再重新启用插件即可应用。",
        enabled: "是否启用此项功能。",
        rules: "按账号类型和模型分别设置参数。每项的说明显示在对应字段上方。",
        "rules.accountType": "账号类型，例如 pro、plus、go、team。",
        "rules.model": "本项对应的模型。",
        "rules.allowedStateLengths": "允许的 state 字节长度；数组中的每个值可单独修改。",
        notes: "多行文本会保留换行。",
      },
      value: "demo",
      maxAttempts: 10,
      enabled: true,
      rules: [
        { accountType: "pro", model: "gpt6", allowedStateLengths: [292] },
        { _comments: { model: "本项使用单独的模型说明。" }, accountType: "plus", model: "5.6", allowedStateLengths: [292, 300] },
      ],
      notes: "第一行\n第二行",
      optional: null,
      empty: [],
    }, null, 2) + "\n" : '{\n  "_comments": { "value": "请求头的值。" },\n  "value": "demo"\n}\n';
    let activePluginConfigContent: string | null = null;
    const configHash = async (text: string) => Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))), byte => byte.toString(16).padStart(2, "0")).join("");
    const extensionsPreview = createCodexExtensionsPreview(previewClientPlatform);
    window.__codeyInvokeApi = async (command, args) => {
      console.log(`[Mock API Call] ${command}`, args);
      // Wait a tiny bit to simulate network delay
      await new Promise((resolve) => setTimeout(resolve, 300));

      if (command === "codex_extensions") return extensionsPreview(args?.request as Record<string, unknown>);

      if (command === "list_codey_plugins") {
        const pluginPreview = new URLSearchParams(window.location.search).get("plugins");
        if (pluginPreview === "error") throw new Error("预览：插件列表暂时不可用，请稍后刷新。");
        return { plugins: structuredClone(previewPlugins), platform: previewClientPlatform, arch: "aarch64" };
      }
      if (command === "set_codey_plugin_enabled" || command === "save_codey_plugin_config_file") {
        const plugin = previewPlugins.find(item => item.id === args?.pluginId);
        if (!plugin) throw new Error("预览：插件不存在");
        if (command === "set_codey_plugin_enabled") {
          if (args?.enabled) {
            const invalid = validatePluginConfigText(pluginConfigContent);
            if (invalid) throw new Error(invalid);
          }
          plugin.enabled = Boolean(args?.enabled); plugin.status = plugin.enabled ? "enabled" : "disabled"; plugin.restartRequired = false;
          activePluginConfigContent = plugin.enabled ? pluginConfigContent : null;
        }
        else {
          const content = args?.content;
          if (typeof content !== "string") throw new Error("配置内容无效");
          const invalid = validatePluginConfigText(content);
          if (invalid) throw new Error(invalid);
          if (pluginPreviewMode === "config-conflict") pluginConfigContent = '{"value":"external"}\n';
          if (args?.expectedSha256 !== await configHash(pluginConfigContent)) throw new Error("配置文件已被其他程序修改，请重新加载后再保存。");
          pluginConfigContent = content;
          plugin.restartRequired = plugin.enabled && (activePluginConfigContent === null || !pluginConfigBusinessValuesEqual(activePluginConfigContent, content));
        }
        return { plugins: structuredClone(previewPlugins), platform: previewClientPlatform, arch: "aarch64" };
      }
      if (command === "uninstall_codey_plugin") {
        const index = previewPlugins.findIndex(item => item.id === args?.pluginId);
        if (index >= 0) previewPlugins.splice(index, 1);
        return { plugins: structuredClone(previewPlugins), platform: previewClientPlatform, arch: "aarch64" };
      }
      if (command === "open_codey_plugin_directory") {
        const plugin = previewPlugins.find(item => item.id === args?.pluginId);
        if (!plugin) throw new Error("预览：插件不存在");
        return { status: "ok" };
      }
      if (command === "open_codey_plugin_logs") {
        const plugin = previewPlugins.find(item => item.id === args?.pluginId);
        if (!plugin) throw new Error("预览：插件不存在");
        if (previewPluginLogTerminals.has(plugin.id)) return { status: "already_open" };
        previewPluginLogTerminals.add(plugin.id);
        return { status: "ok" };
      }
      if (command === "clear_codey_plugin_logs") {
        const plugin = previewPlugins.find(item => item.id === args?.pluginId);
        if (!plugin) throw new Error("预览：插件不存在");
        if (args?.confirmed !== true) throw new Error("清除插件日志需要确认");
        plugin.logSizeBytes = 0;
        return { plugins: structuredClone(previewPlugins), platform: previewClientPlatform, arch: "aarch64" };
      }
      if (command === "select_codey_plugin_package") return structuredClone(previewPluginPackage);
      if (command === "inspect_codey_plugin") {
        const path = typeof args?.path === "string" ? args.path.trim() : "";
        if (!path.toLowerCase().endsWith(".codey-plugin")) throw new Error("请选择 .codey-plugin 安装包");
        return { ...structuredClone(previewPluginPackage), path };
      }
      if (command === "install_codey_plugin") {
        if (args?.sha256 !== previewPluginPackage.sha256) throw new Error("插件包在检查后发生变化，请重新检查");
        const installed = previewPlugins.find(item => item.id === previewPluginPackage.manifest.id);
        if (installed) {
          installed.version = previewPluginPackage.manifest.version;
          installed.capabilities = [...previewPluginPackage.manifest.capabilities];
          installed.description = previewPluginPackage.manifest.description;
        } else {
          previewPlugins.push({
            id: previewPluginPackage.manifest.id,
            name: previewPluginPackage.manifest.name,
            version: previewPluginPackage.manifest.version,
            description: previewPluginPackage.manifest.description,
            enabled: false,
            status: "disabled",
            restartRequired: false,
            configPath: `/preview/codey-plugins/installed/${previewPluginPackage.manifest.id}/config.json`,
            capabilities: [...previewPluginPackage.manifest.capabilities],
            pluginDir: `/preview/codey-plugins/installed/${previewPluginPackage.manifest.id}`,
            dataDir: `/preview/codey-plugins/installed/${previewPluginPackage.manifest.id}/data`,
            logDir: `/preview/codey-plugins/installed/${previewPluginPackage.manifest.id}/logs`,
          });
        }
        return { plugins: structuredClone(previewPlugins), platform: previewClientPlatform, arch: "aarch64" };
      }
      if (command === "get_codey_plugin_config_file") {
        if (pluginPreviewMode === "config-error") throw new Error("预览：配置文件暂时无法读取");
        const plugin = previewPlugins.find(item => item.id === args?.pluginId);
        if (!plugin) throw new Error("预览：插件不存在");
        return { pluginId: plugin.id, version: plugin.version, path: plugin.configPath, content: pluginConfigContent, sha256: await configHash(pluginConfigContent) };
      }

      if (command === "load_codey_config") {
        if (previewConfigLoadFailed) throw new Error("预览：配置路径不存在");
        return {
          config: previewConfig,
          modelState: previewModelState,
          startupError: undefined,
          officialAccountAvailable: previewOfficialAccounts.some(
            (account) => account.isDefault,
          ),
          providerStatus: previewProviderStatus(),
          fastContextToolsStatus: {
            userConfigured: false,
            detectionFailed: false,
          },
        };
      }
      if (command === "runtime_status") {
        const injectionRepairing = Date.now() < previewInjectionRepairUntil;
        if (previewInjectionRepairUntil && !injectionRepairing) previewInjectionMode = "node_options";
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
                : channel.kind === "ntfy"
                  ? channel.urlConfigured && Boolean(channel.chatId.trim())
                  : channel.urlConfigured),
          ).length;
        return {
          running: true,
          appVersion: "0.2.0",
          codexAppVersion: "26.601.21317",
          clientPlatform: previewClientPlatform,
          officialAccountAvailable: previewOfficialAccounts.some(
            (account) => account.isDefault,
          ),
          restartRequired: false,
          restartInProgress: injectionRepairing,
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
            performanceDetail: "Codex 启动成功",
            startupInjectionMode: previewInjectionMode,
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
        };
      }
      if (command === "list_official_accounts") {
        return { status: "ok", accounts: previewOfficialAccounts, defaultAccountId: previewDefaultOfficialAccountId(), officialAccountAvailable: true };
      }
      if (command === "refresh_official_account_routes") {
        previewDeriveOfficialProfiles();
        return { status: "ok", accounts: previewOfficialAccounts, defaultAccountId: previewDefaultOfficialAccountId(), officialAccountAvailable: true, config: previewConfig, modelState: previewModelState, restartRequired: false };
      }
      if (command === "start_official_account_login") {
        return { status: "wait", loginId: "preview-official-login", authUrl: "https://auth.openai.com/oauth/authorize?client_id=preview&state=preview", browserOpened: true };
      }
      if (command === "poll_official_account_login") {
        previewOfficialLoginPolls += 1;
        if (previewOfficialLoginPolls < 3) return { status: "wait" };
        previewOfficialLoginPolls = 0;
        const id = `acct_preview_${previewOfficialAccounts.length + 1}`;
        previewOfficialAccounts.push({ id, email: `user${previewOfficialAccounts.length + 1}@example.com`, planType: "plus", accountId: id, addedAt: Math.floor(Date.now() / 1000), isDefault: previewOfficialAccounts.length === 0 });
        previewDeriveOfficialProfiles();
        return { status: "ok", accounts: previewOfficialAccounts, defaultAccountId: previewDefaultOfficialAccountId(), officialAccountAvailable: true, config: previewConfig, modelState: previewModelState, restartRequired: false };
      }
      if (command === "cancel_official_account_login") {
        previewOfficialLoginPolls = 0;
        return { status: "ok" };
      }
      if (command === "import_current_codex_login") {
        return { status: "failed", message: "当前 Codex 没有 ChatGPT 官方账号登录，无法导入" };
      }
      if (command === "import_official_account_credential") {
        const credential = typeof args.credential === "string" ? args.credential.trim() : "";
        if (!credential) return { status: "failed", message: "请粘贴 Refresh Token 或 OAuth JSON" };
        if (credential.includes("invalid")) return { status: "failed", message: "Refresh Token 已被拒绝，请确认令牌仍然有效" };
        const id = `acct_preview_${previewOfficialAccounts.length + 1}`;
        previewOfficialAccounts.push({ id, email: `user${previewOfficialAccounts.length + 1}@example.com`, planType: "plus", accountId: id, addedAt: Math.floor(Date.now() / 1000), isDefault: previewOfficialAccounts.length === 0 });
        previewDeriveOfficialProfiles();
        return { status: "ok", accounts: previewOfficialAccounts, defaultAccountId: previewDefaultOfficialAccountId(), officialAccountAvailable: true, config: previewConfig, modelState: previewModelState, restartRequired: false };
      }
      if (command === "set_default_official_account") {
        const target = previewOfficialAccounts.find((account) => account.id === args.accountId);
        if (target?.invalid) {
          return { status: "failed", message: "该账号已失效，无法设为默认；请重新添加账号" };
        }
        for (const account of previewOfficialAccounts) account.isDefault = account.id === args.accountId;
        previewDeriveOfficialProfiles();
        return { status: "ok", accounts: previewOfficialAccounts, defaultAccountId: previewDefaultOfficialAccountId(), officialAccountAvailable: true, config: previewConfig, modelState: previewModelState, restartRequired: false };
      }
      if (command === "remove_official_account") {
        const removed = previewOfficialAccounts.find((account) => account.id === args.accountId);
        previewOfficialAccounts = previewOfficialAccounts.filter((account) => account.id !== args.accountId);
        if (removed?.isDefault && previewOfficialAccounts.length > 0) {
          previewOfficialAccounts[0].isDefault = true;
        }
        previewDeriveOfficialProfiles();
        const available = previewOfficialAccounts.some((account) => account.isDefault);
        return { status: "ok", accounts: previewOfficialAccounts, defaultAccountId: previewDefaultOfficialAccountId(), officialAccountAvailable: available, config: previewConfig, modelState: previewModelState, restartRequired: false, ...(removed?.isDefault && !available ? { warning: "当前没有默认官方账号，官方线路已停用" } : {}) };
      }
      if (command === "save_official_account_route_settings") {
        const account = previewOfficialAccounts.find((item) => item.id === args.accountId);
        if (!account) return { status: "failed", message: "找不到官方账号" };
        const routeOverride = (value: unknown) => {
          const text = String(value ?? "").trim();
          return text ? text : undefined;
        };
        account.routeName = routeOverride(args.routeName);
        account.routeShortName = routeOverride(args.routeShortName);
        account.upstreamProxy = routeOverride(args.upstreamProxy);
        account.baseUrl = routeOverride(args.baseUrl);
        // 清空设置后后端会立刻补回生成的默认名称，预览保持一致。
        previewDeriveOfficialProfiles();
        return {
          status: "ok",
          accounts: previewOfficialAccounts,
          defaultAccountId: previewDefaultOfficialAccountId(),
          officialAccountAvailable: true,
          accountId: account.id,
          config: previewConfig,
          modelState: previewModelState,
          restartRequired: false,
        };
      }
      if (command === "query_official_account_usage") {
        const fetchedAt = Math.floor(Date.now() / 1000);
        const accountId = String(args.accountId || "default");
        // 失效账号不再请求官方接口，直接返回失效原因，供卡片显示。
        const account = previewOfficialAccounts.find((item) => item.id === accountId);
        if (account?.invalid) {
          return { status: "error", reason: "official_account_invalid", message: account.invalidReason || "账号已失效" };
        }
        // 预览模式按账号返回不同的额度，避免所有线路显示同一份数据。
        let seed = 0;
        for (const character of accountId) seed = (seed * 31 + character.charCodeAt(0)) % 60;
        return { status: "ok", fetchedAt, secondary: {
          usedPercent: 20 + seed, windowMinutes: 10080, resetsAt: fetchedAt + 3 * 86400,
        } };
      }
      if (command === "query_route_request_logs" || command === "query_route_request_log_stats" || command === "query_route_request_log_models") {
        const page = Math.max(1, Number(args.page) || 1);
        const pageSize = Math.min(100, Math.max(1, Number(args.pageSize) || 20));
        const search = String(args.search || "").trim().toLocaleLowerCase();
        const provider = String(args.provider || "");
        const officialAccountId = String(args.officialAccountId || "");
        const model = String(args.model || "");
        const status = String(args.status || "");
        const protocol = String(args.protocol || "");
        const allTime = command === "query_route_request_log_stats" && args.allTime === true;
        const toUnixMs = allTime ? Date.now() : Number(args.toUnixMs) || Date.now();
        const fromUnixMs = allTime ? Math.min(toUnixMs, ...previewRouteRequestLogs.map((item) => item.timestampUnixMs)) : Number(args.fromUnixMs) || toUnixMs - 86_400_000;
        const filtered = previewRouteRequestLogs.filter((item) => {
          if (item.timestampUnixMs < fromUnixMs || item.timestampUnixMs >= toUnixMs) return false;
          if (args.requestId && item.requestId !== args.requestId) return false;
          if (args.sessionId && item.codexSessionId !== args.sessionId) return false;
          if (args.requestKind && item.requestKind !== args.requestKind) return false;
          if (provider && item.provider !== provider && item.providerName !== provider) return false;
          if (officialAccountId && item.officialAccountId !== officialAccountId) return false;
          if (model && item.model !== model && item.requestedModel !== model) return false;
          if (status && item.status !== status) return false;
          if (protocol && item.upstreamTransport !== protocol) return false;
          if (!search) return true;
          return [
            item.requestId,
            item.traceId,
            item.codexSessionId,
            item.provider,
            item.providerName,
            item.officialAccountId,
            item.requestedModel,
            item.model,
            item.upstreamAuthority,
            item.upstreamErrorSummary,
          ].some((value) => value?.toLocaleLowerCase().includes(search));
        });
        filtered.sort((left, right) => right.timestampUnixMs - left.timestampUnixMs || right.requestId.localeCompare(left.requestId));
        if (command === "query_route_request_log_models") {
          const models = [...new Set(filtered.map((item) => item.model ?? item.requestedModel))]
            .filter((model) => model && (!args.afterModel || model > String(args.afterModel))).sort();
          return { queryable: true, models: models.slice(0, 200), nextCursor: models.length > 200 ? models[199] : null };
        }
        if (command === "query_route_request_log_stats") {
          const aggregate = (rows: typeof filtered) => {
            const sum = (values: Array<number | null | undefined>) => {
              const known = values.filter((value): value is number => value != null);
              return known.length ? known.reduce((total, value) => total + value, 0) : null;
            };
            const total = rows.length;
            const succeededCount = rows.filter((item) => item.status === "succeeded").length;
            const durations = rows.map((item) => item.totalDurationMs);
            const ttfts = rows.map((item) => item.downstreamFirstContentMs ?? item.ttftMs).filter((value): value is number => value != null);
            return {
              total, succeededCount, failedCount: total - succeededCount, incompleteCount: 0, cancelledCount: 0,
              successRate: total ? succeededCount / total * 100 : null,
              avgDuration: total ? sum(durations)! / total : null,
              avgTtft: ttfts.length ? sum(ttfts)! / ttfts.length : null,
              avgRouterPreUpstream: null, avgUpstreamHeader: null, avgUpstreamFirstByte: null,
              avgDownstreamFirstContent: ttfts.length ? sum(ttfts)! / ttfts.length : null, avgQueueDelay: null,
              inputTokensSum: sum(rows.map((item) => item.inputTokens)),
              outputTokensSum: sum(rows.map((item) => item.outputTokens)),
              totalTokensSum: sum(rows.map((item) => item.totalTokens)),
              cachedTokensSum: sum(rows.map((item) => item.cachedInputTokens)),
              usageReportedCount: rows.filter((item) => item.usageReported).length,
              totalTokensKnownCount: rows.filter((item) => item.totalTokens != null).length,
            };
          };
          const bucketMs = toUnixMs - fromUnixMs <= 7 * 86_400_000 ? 3_600_000 : Math.max(1, Math.ceil((toUnixMs - fromUnixMs) / (366 * 86_400_000))) * 86_400_000;
          const grouped = new Map<string, typeof filtered>();
          const buckets = new Map<number, typeof filtered>();
          const dailyBuckets = new Map<number, typeof filtered>();
          for (const item of filtered) {
            const key = args.groupBy === "provider" ? item.provider : args.groupBy === "status" ? item.status
              : args.groupBy === "protocol" ? item.upstreamTransport : args.groupBy === "request_kind" ? item.requestKind
              : args.groupBy === "session" ? item.codexSessionId ?? ""
              : args.groupBy === "official_account" ? item.officialAccountId ?? ""
              : item.model ?? item.requestedModel;
            const bucket = Math.floor(item.timestampUnixMs / bucketMs) * bucketMs;
            grouped.set(key, [...(grouped.get(key) ?? []), item]);
            buckets.set(bucket, [...(buckets.get(bucket) ?? []), item]);
            const day = Math.floor(item.timestampUnixMs / 86_400_000) * 86_400_000;
            if (args.includeDailyTrend === true) dailyBuckets.set(day, [...(dailyBuckets.get(day) ?? []), item]);
          }
          const groups = [...grouped].map(([key, rows]) => ({ key, ...aggregate(rows) })).sort((a, b) => (args.groupSort === "tokens" ? (b.totalTokensSum ?? -1) - (a.totalTokensSum ?? -1) : b.total - a.total) || a.key.localeCompare(b.key));
          return {
            status: "ok", backend: "sqlite", queryable: true, fromUnixMs, toUnixMs,
            ...aggregate(filtered), groups: groups.slice(0, 50), groupsTruncated: groups.length > 50, bucketMs,
            trend: [...buckets].sort(([a], [b]) => a - b).map(([timestampUnixMs, rows]) => ({ timestampUnixMs, ...aggregate(rows) })),
            ...(args.includeDailyTrend === true ? { dailyTrend: [...dailyBuckets].sort(([a], [b]) => a - b).map(([timestampUnixMs, rows]) => ({ timestampUnixMs, total: rows.length, totalTokensSum: aggregate(rows).totalTokensSum, totalTokensKnownCount: aggregate(rows).totalTokensKnownCount })) } : {}),
            recordingHealth: { enabled: true, active: true, sampleRatePerMillion: 1_000_000, pendingEntries: 0,
              accepted: previewRouteRequestLogs.length, entriesWritten: previewRouteRequestLogs.length,
              sampledOut: 0, droppedFull: 0, droppedClosed: 0, writeDropped: 0, writeFailures: 0, observerPanics: 0, writerPanics: 0, shutdownTimeouts: 0 },
          };
        }
        const cursor = args.cursor as { timestampUnixMs: number; requestId: string } | null;
        const remaining = args.cursorMode && cursor ? filtered.filter((item) => item.timestampUnixMs < cursor.timestampUnixMs
          || (item.timestampUnixMs === cursor.timestampUnixMs && item.requestId < cursor.requestId)) : filtered;
        const offset = args.cursorMode ? 0 : (page - 1) * pageSize;
        const items = remaining.slice(offset, offset + pageSize);
        const hasMore = remaining.length > offset + pageSize;
        const last = items[items.length - 1];
        const totalPages = Math.ceil(filtered.length / pageSize);
        return {
          status: "ok",
          backend: "sqlite",
          queryable: true,
          page,
          pageSize,
          total: filtered.length,
          totalPages,
          items, hasMore,
          nextCursor: hasMore && last ? { timestampUnixMs: last.timestampUnixMs, requestId: last.requestId } : null,
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
      if (command === "delete_route" || command === "fetch_route_models" || command === "set_route_enabled") {
        const expectedRevision = Number(args.expectedRevision);
        if (expectedRevision !== previewConfig.settingsRevision) {
          return {
            status: "failed",
            message: "Codey 设置已被其他操作更新，请重新载入后再操作线路",
          };
        }
      }
      if (command === "set_route_enabled") {
        if (!previewConfig.localRouterEnabled) return { status: "failed", message: "本地路由已关闭，线路配置只读" };
        const routeId = String(args.routeId || "");
        const route = previewConfig.profiles.find((profile) => profile.id === routeId);
        if (!route) return { status: "failed", message: "找不到要更新的线路" };
        if (typeof args.enabled !== "boolean") return { status: "failed", message: "参数 enabled 无效" };
        const enabled = args.enabled;
        const profiles = previewConfig.profiles.map((profile) => profile.id === routeId ? { ...profile, enabled } : profile);
        previewConfig = {
          ...previewConfig,
          profiles,
          activeProfileId: profiles.find((profile) => profile.id === previewConfig.activeProfileId && profile.enabled !== false)?.id
            || profiles.find((profile) => profile.enabled !== false)?.id || previewConfig.activeProfileId,
          settingsRevision: previewConfig.settingsRevision + 1,
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
        if (previewConfig.modelContextByProvider) delete previewConfig.modelContextByProvider[providerId];
        if (previewConfig.modelReasoningEffortsByProvider) delete previewConfig.modelReasoningEffortsByProvider[providerId];
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
        if (route.enabled === false) return { status: "failed", message: "线路已禁用，不能同步模型" };
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
          modelReasoningEffortsByProvider: {
            ...previewConfig.modelReasoningEffortsByProvider,
            [providerId]: Object.fromEntries(
              Object.entries(previewConfig.modelReasoningEffortsByProvider?.[providerId] ?? {})
                .filter(([model]) => includesModelId(models, model)),
            ),
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
        if (args.target !== "trace" && args.target !== "crashpad") {
          throw new Error("无效的诊断清理目标");
        }
        previewTraceStats ??= previewTraceLogStats;
        previewCrashpadStats ??= previewCrashpadPendingStats;
        const traceBefore = previewTraceStats;
        const crashpadBefore = previewCrashpadStats;
        if (args.target !== "crashpad") previewTraceStats = {
          ...traceBefore,
          databaseBytes: 49152,
        };
        if (args.target !== "trace") previewCrashpadStats = {
          ...crashpadBefore,
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
          traceLogStatsBefore: traceBefore,
          traceCleanup: {
            databasesFound: 1,
            databasesCleaned: 1,
            rowsDeleted: traceBefore.databaseBytes > previewTraceStats.databaseBytes ? 318757 : 0,
            bytesBefore: traceBefore.databaseBytes,
            bytesAfter: previewTraceStats.databaseBytes,
            bytesReclaimed: Math.max(0, traceBefore.databaseBytes - previewTraceStats.databaseBytes),
          },
          crashpadCleanup: {
            directoriesFound: 2,
            reportsFound: crashpadBefore.reportsFound,
            reportsDeleted: crashpadBefore.completeReports - previewCrashpadStats.completeReports,
            filesFound: crashpadBefore.filesFound,
            filesDeleted: crashpadBefore.filesFound - previewCrashpadStats.filesFound,
            orphanFilesDeleted: 0,
            unmanagedFiles: 0,
            skippedRecentReports: 0,
            bytesBefore: crashpadBefore.pendingBytes,
            bytesAfter: previewCrashpadStats.pendingBytes,
            bytesReclaimed: Math.max(0, crashpadBefore.pendingBytes - previewCrashpadStats.pendingBytes),
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
        const availableModels = targetProfile?.authMode === "officialAccount"
          ? previewOfficialModels.map((model) => model.slug)
          : uniqueModelIds([
              ...(previewConfig.upstreamModelsByProvider[providerId] || []),
              ...supportedModels,
            ]);
        previewConfig.modelContextByProvider = { ...previewConfig.modelContextByProvider,
          [providerId]: Object.fromEntries(Object.entries((args.modelContexts as Record<string, import("../App.types").ModelContextConfig> | undefined)
            ?? previewConfig.modelContextByProvider?.[providerId] ?? {}).filter(([model]) => includesModelId(availableModels, model))) };
        previewConfig.modelReasoningEffortsByProvider = { ...previewConfig.modelReasoningEffortsByProvider,
          [providerId]: Object.fromEntries(Object.entries((args.reasoningEfforts as Record<string, import("../App.types").ModelReasoningEffort[]> | undefined)
            ?? previewConfig.modelReasoningEffortsByProvider?.[providerId] ?? {}).filter(([model]) => includesModelId(availableModels, model))) };
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
          customContextsRestored: false,
        };
      }
      if (command === "reorder_route_models") {
        const routeId = String(args.routeId || "");
        const targetProfile = previewConfig.profiles.find(
          (profile) => profile.id === routeId,
        );
        if (!targetProfile) {
          return { status: "failed", message: "找不到要调整模型顺序的线路" };
        }
        const providerId = routeProviderId(targetProfile);
        previewConfig = {
          ...previewConfig,
          settingsRevision: previewConfig.settingsRevision + 1,
          selectedModelsByProvider: {
            ...previewConfig.selectedModelsByProvider,
            [providerId]: uniqueModelIds((args.models as string[]) || []),
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
          settingsRevision: previewConfig.settingsRevision + 1,
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
        const availableModels = previewOfficialModels.map((model) => model.slug);
        previewConfig.modelContextByProvider = { ...previewConfig.modelContextByProvider,
          [providerId]: Object.fromEntries(Object.entries((args.modelContexts as Record<string, import("../App.types").ModelContextConfig> | undefined)
            ?? previewConfig.modelContextByProvider?.[providerId] ?? {}).filter(([model]) => includesModelId(availableModels, model))) };
        previewConfig = {
          ...previewConfig,
          settingsRevision: previewConfig.settingsRevision + 1,
          showAccountUsageInHeader: typeof args.showAccountUsageInHeader === "boolean"
            ? args.showAccountUsageInHeader
            : previewConfig.showAccountUsageInHeader,
          profiles: previewConfig.profiles.map((profile) =>
            profile.id === routeId && typeof args.enabled === "boolean"
              ? { ...profile, enabled: args.enabled }
              : profile
          ),
          selectedModelsByProvider: {
            ...previewConfig.selectedModelsByProvider,
            [providerId]: models,
          },
        };
        if (previewConfig.activeProfileId === routeId && args.enabled === false) {
          previewConfig.activeProfileId = previewConfig.profiles.find(
            (profile) => profile.enabled !== false,
          )?.id || routeId;
        }
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
        const accountId = String(args.accountId || "").trim();
        const account = accountId
          ? previewOfficialAccounts.find((item) => item.id === accountId)
          : undefined;
        if (accountId) {
          if (!account) return { status: "failed", message: "找不到官方账号" };
          const routeOverride = (value: unknown) => {
            const text = String(value ?? "").trim();
            return text ? text : undefined;
          };
          account.routeName = routeOverride(args.routeName);
          account.routeShortName = routeOverride(args.routeShortName);
          account.upstreamProxy = routeOverride(args.upstreamProxy);
          if ("baseUrl" in args) account.baseUrl = routeOverride(args.baseUrl);
          previewDeriveOfficialProfiles();
        }
        return {
          status: "ok",
          config: previewConfig,
          modelState: previewModelState,
          restartRequired: false,
          modelHotReloaded: true,
          customContextsRestored: false,
          ...(account
            ? {
                accounts: previewOfficialAccounts,
                defaultAccountId: previewDefaultOfficialAccountId(),
                officialAccountAvailable: true,
                accountId: account.id,
              }
            : {}),
        };
      }
      if (command === "restart_codey") {
        return { status: "restarting" };
      }
      if (command === "repair_main_process_injection") {
        previewInjectionRepairUntil = Date.now() + 8_000;
        return { status: "repairing" };
      }
      if (command === "repair_codex_config") {
        await new Promise((resolve) => setTimeout(resolve, 1800));
        if (configRepairPreview === "failure") throw new Error("预览：无法写入配置目录，已记录错误日志");
        previewConfigLoadFailed = false;
        return {
          message: configRepairPreview === "unchanged" ? "Codex 配置检查通过，无需修改" : "Codex 配置已修复",
          configPath: "/preview/.codex/config.toml",
          repaired: configRepairPreview !== "unchanged",
          backupPath: null,
        };
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
          : channel?.kind === "ntfy"
            ? Boolean(channel.url?.trim() && channel.chatId?.trim())
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

export {};
