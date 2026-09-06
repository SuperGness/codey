import type {
  FastContextToolsStatus,
  InjectionScriptStatus,
  RuntimeStatus,
} from "./App.types";

type OptimizationFeatureIcon =
  | "code"
  | "database"
  | "fastctx"
  | "notifications"
  | "subagent";

export type EnabledOptimizationFeature = {
  id: string;
  icon: OptimizationFeatureIcon;
  name: string;
  detail?: string;
  sourceLabel: string;
};

type OptimizationRuntimeStatus = Pick<
  RuntimeStatus,
  | "running"
  | "injectionScripts"
  | "fastContextToolsActive"
  | "subagentOptimizationActive"
  | "notificationChannelsActive"
  | "activeNotificationChannelCount"
  | "traceLogWriteProtectionActive"
  | "crashpadDiskProtectionActive"
>;

type InjectionStatusSummary = {
  failedInjectionScriptCount: number;
  internalInjectionError: boolean;
  internalInjectionPending: boolean;
  unverifiedInjectionScriptCount: number;
};

export function buildEnabledOptimizationFeatures(
  status: OptimizationRuntimeStatus,
  fastContextToolsStatus: FastContextToolsStatus,
): EnabledOptimizationFeature[] {
  const injectedFeatures = (status.injectionScripts ?? [])
    .filter(
      (script) =>
        script.visibility === "feature" && script.status === "effective",
    )
    .map((script) => ({
      id: script.id,
      icon: "code" as const,
      name: script.name,
      detail: script.detail,
      sourceLabel: script.source === "user" ? "用户脚本" : "内置",
    }));
  const appliedFeatures: EnabledOptimizationFeature[] = [...injectedFeatures];

  if (status.subagentOptimizationActive === true) {
    appliedFeatures.push({
      id: "subagent-optimization",
      icon: "subagent",
      name: "子代理优化",
      detail: "子代理角色与调度增强已随当前运行实例加载",
      sourceLabel: "Codey",
    });
  }

  const activeNotificationChannelCount =
    status.activeNotificationChannelCount ?? 0;
  if (
    status.notificationChannelsActive === true &&
    activeNotificationChannelCount > 0
  ) {
    appliedFeatures.push({
      id: "notification-channels",
      icon: "notifications",
      name: "消息通知",
      detail: `已启用 ${activeNotificationChannelCount} 个通知渠道`,
      sourceLabel: "Codey",
    });
  }

  const traceWriteProtectionActive =
    status.traceLogWriteProtectionActive === true;
  const crashpadDiskProtectionActive =
    status.crashpadDiskProtectionActive === true;
  if (traceWriteProtectionActive || crashpadDiskProtectionActive) {
    const protectionDetail =
      traceWriteProtectionActive && crashpadDiskProtectionActive
        ? "Codex Trace 日志与 Crashpad 磁盘保护均已生效"
        : traceWriteProtectionActive
          ? "Codex Trace 日志写盘保护已生效"
          : "Codex Crashpad 磁盘保护已生效";
    appliedFeatures.push({
      id: "disk-write-protection",
      icon: "database",
      name: "写盘保护",
      detail: protectionDetail,
      sourceLabel: "Codey",
    });
  }

  const fastContextToolsActive =
    status.fastContextToolsActive === true ||
    (status.running && fastContextToolsStatus.userConfigured);
  if (!fastContextToolsActive) return appliedFeatures;

  const externalFastContextTools = fastContextToolsStatus.userConfigured;
  return [
    {
      id: "fastctx-context-tools",
      icon: "fastctx",
      name: "FastCtx 上下文加速",
      detail: externalFastContextTools
        ? `Codex 已配置 FastCtx${
            fastContextToolsStatus.serverId
              ? `（${fastContextToolsStatus.serverId}）`
              : ""
          }`
        : "Codey 内置 FastCtx 已随当前运行实例加载",
      sourceLabel: externalFastContextTools ? "外部配置" : "Codey",
    },
    ...appliedFeatures,
  ];
}

export function summarizeInjectionScripts(
  injectionScripts: readonly InjectionScriptStatus[],
): InjectionStatusSummary {
  let failedInjectionScriptCount = 0;
  let internalInjectionError = false;
  let internalInjectionPending = false;
  let unverifiedInjectionScriptCount = 0;

  for (const script of injectionScripts) {
    const failedOrUnknown =
      script.status === "failed" || script.status === "unknown";
    if (script.visibility === "internal") {
      internalInjectionPending ||= script.status === "executed";
      internalInjectionError ||= failedOrUnknown;
    } else if (script.status === "executed") {
      unverifiedInjectionScriptCount += 1;
    } else if (failedOrUnknown) {
      failedInjectionScriptCount += 1;
    }
  }

  return {
    failedInjectionScriptCount,
    internalInjectionError,
    internalInjectionPending,
    unverifiedInjectionScriptCount,
  };
}
