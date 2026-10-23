import type { Profile, RuntimeStatus } from "./App.types";

export type RouteCompactionStatus = {
  mode: "remote" | "local";
  badgeText: string;
  badgeVariant: "success" | "secondary";
  tooltip: string;
  reason?: string;
};

export function getRouteCompactionStatus(
  profile: Profile,
  runtimeStatus?: RuntimeStatus | null,
  officialAccountAvailable?: boolean,
): RouteCompactionStatus {
  const isOfficial = profile.authMode === "officialAccount" || profile.officialAccount;

  if (isOfficial) {
    if (officialAccountAvailable === false) {
      return {
        mode: "local",
        badgeText: "本地压缩",
        badgeVariant: "secondary",
        tooltip: "使用本地压缩（官方账号未就绪）",
        reason: "官方账号未就绪",
      };
    }
    return {
      mode: "remote",
      badgeText: "远程压缩",
      badgeVariant: "success",
      tooltip: "官方账号原生支持远程压缩；压缩方式随所选线路自动生效",
    };
  }

  // 检查运行时阻塞列表中是否有该线路的具体原因。未声明原生压缩的线路
  // 仍会经过网关生成等价摘要，因此这里只记录原因，不改变压缩方式。
  const blocker = runtimeStatus?.remoteCompaction?.blockingRoutes?.find(
    (b) => b.routeId === profile.id
  );

  const hasPluginTransport = Boolean(profile.pluginRouteSpec?.transport);
  if (hasPluginTransport) {
    return {
      mode: "remote",
      badgeText: "网关压缩",
      badgeVariant: "success",
      tooltip: "由 Codey 网关生成等价摘要（插件传输不支持上游原生压缩）",
      reason: "插件传输不支持原生压缩",
    };
  }

  if (profile.upstreamProtocol && profile.upstreamProtocol !== "openaiResponses") {
    return {
      mode: "remote",
      badgeText: "网关压缩",
      badgeVariant: "success",
      tooltip: "由 Codey 网关生成等价摘要（上游协议不支持原生压缩）",
      reason: "上游协议不支持原生压缩",
    };
  }

  if (!profile.supportsRemoteCompaction) {
    return {
      mode: "remote",
      badgeText: "网关压缩",
      badgeVariant: "success",
      tooltip: blocker
        ? `由 Codey 网关生成等价摘要（${blocker.reason}）`
        : "由 Codey 网关生成等价摘要（未声明原生远程压缩）",
      reason: "未开启远程压缩",
    };
  }

  const protocolLabel =
    profile.remoteCompactionProtocol === "compactEndpoint"
      ? "独立压缩接口 /responses/compact"
      : "原生 Responses";

  return {
    mode: "remote",
    badgeText: "远程压缩",
    badgeVariant: "success",
    tooltip: `原生远程压缩（接口：${protocolLabel}）；压缩方式随所选线路自动生效`,
  };
}
