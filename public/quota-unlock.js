// Codey 本地第三方线路的发送判断辅助接口。
// 官方额度数据始终原样保留；调用方只在发送判断处使用此接口。
// 旧版曾替换全局 JSON.parse / WebSocket，无法在此安全恢复原函数。
// 升级后须加载新 document，才能移除旧版已安装的拦截。
(() => {
  "use strict";
  if (window.__codeyQuotaUnlock) return;
  let enabled = true;
  try { enabled = localStorage.getItem("codeyQuotaUnlock") !== "off"; } catch {}
  // 运行中的 Codex 会话按启动时的官方账号显示额度；切换默认账号后这些线路
  // 凭据已经换掉，发送判断由后端按 provider id 放行。
  const normalizeBypassIds = (providerIds) => new Set(
    (Array.isArray(providerIds) ? providerIds : [])
      .map((id) => (typeof id === "string" ? id.trim() : ""))
      .filter(Boolean),
  );
  let officialSendBypass = new Set();
  // 本脚本注入在模型补丁之后，安装时先回读补丁已收到的名单。
  try {
    const pending = window.__codeyModelWhitelistPatch?.officialSendBypassProviderIds?.();
    if (Array.isArray(pending)) officialSendBypass = normalizeBypassIds(pending);
  } catch {}
  const version = "2.1.0";
  const installedAt = new Date().toISOString();
  window.__codeyQuotaUnlock = {
    version,
    status() {
      return {
        version,
        enabled,
        installedAt,
        officialSendBypass: officialSendBypass.size,
      };
    },
    setOfficialSendBypass(providerIds) {
      officialSendBypass = normalizeBypassIds(providerIds);
      return officialSendBypass.size;
    },
    allowsLocalSend(context) {
      if (!enabled || !context || context.hostId !== "local" ||
          typeof context.model !== "string" || context.model.trim() === "") return false;
      try {
        const patch = window.__codeyModelWhitelistPatch;
        if (patch?.isThirdPartyModel(context) === true) return true;
        if (!officialSendBypass.size) return false;
        const providerId = patch?.officialRouteProviderId?.(context);
        return typeof providerId === "string" && officialSendBypass.has(providerId);
      } catch { return false; }
    },
    enable() {
      enabled = true;
      try { localStorage.removeItem("codeyQuotaUnlock"); } catch {}
      return "已启用本地第三方线路发送判断";
    },
    disable() {
      enabled = false;
      try { localStorage.setItem("codeyQuotaUnlock", "off"); } catch {}
      return "已停用本地第三方线路发送判断";
    },
  };
})();
