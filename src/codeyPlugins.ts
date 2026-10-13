export type CodeyPlugin = {
  id: string; name: string; version: string; description?: string; enabled: boolean;
  status: string; configPath: string;
  capabilities: string[]; lastError?: string; restartRequired?: boolean;
  activeVersion?: string | null;
  pluginDir?: string; dataDir?: string; logDir?: string;
  logSizeBytes?: number | null;
};
export type CodeyPluginsResult = { plugins: CodeyPlugin[]; platform: string; arch: string };
export type CodeyPluginConfigFile = { pluginId: string; version: string; path: string; content: string; sha256: string };
export function parseCodeyPluginsResult(value: unknown): CodeyPluginsResult {
  const object = (item: unknown): item is Record<string, unknown> => !!item && typeof item === "object" && !Array.isArray(item);
  if (!object(value) || !Array.isArray(value.plugins) || typeof value.platform !== "string" || typeof value.arch !== "string") throw new Error("插件列表响应无效，请刷新后重试。");
  const ids = new Set<string>();
  for (const plugin of value.plugins) {
    if (!object(plugin) || ![plugin.id, plugin.name, plugin.version, plugin.status, plugin.configPath].every(item => typeof item === "string" && item.length > 0)
      || typeof plugin.enabled !== "boolean"
      || !Array.isArray(plugin.capabilities) || !plugin.capabilities.every(item => typeof item === "string") || ids.has(plugin.id as string)) throw new Error("插件列表响应无效，请刷新后重试。");
    ids.add(plugin.id as string);
  }
  return value as CodeyPluginsResult;
}
export type CodeyPluginPreview = {
  path: string; sha256: string;
  manifest: { id: string; name: string; version: string; description?: string;
    capabilities?: string[]; permissions?: string[]; headerNames?: string[] };
};
export function validatePluginConfigText(content: string): string | undefined {
  if (new TextEncoder().encode(content).length > 1024 * 1024) return "配置文件不能超过 1 MiB。";
  try {
    const value: unknown = JSON.parse(content);
    if (!value || typeof value !== "object" || Array.isArray(value)) return "配置文件的根节点必须是 JSON 对象。";
    const pending: { value: object; path: string }[] = [{ value, path: "$" }];
    while (pending.length) {
      const current = pending.pop()!;
      for (const [key, child] of Object.entries(current.value)) {
        const path = Array.isArray(current.value) ? `${current.path}[${key}]` : `${current.path}[${JSON.stringify(key)}]`;
        if (!Array.isArray(current.value) && key === "_comments") {
          if (!child || typeof child !== "object" || Array.isArray(child)) return `配置注释 ${path} 必须是对象，且每项说明必须是字符串。`;
          for (const [name, description] of Object.entries(child)) {
            if (typeof description !== "string") return `配置注释 ${path}[${JSON.stringify(name)}] 必须是字符串。`;
          }
        } else if (child && typeof child === "object") pending.push({ value: child, path });
      }
    }
  } catch { return "JSON 格式无效，请检查后保存。"; }
}

// 预览也按当前实例的业务配置判断是否需要重新启用，不修改编辑器原文。
export function pluginConfigBusinessValuesEqual(leftText: string, rightText: string): boolean {
  try {
    const pending: [unknown, unknown][] = [[JSON.parse(leftText), JSON.parse(rightText)]];
    while (pending.length) {
      const [left, right] = pending.pop()!;
      if (left === right) continue;
      if (!left || !right || typeof left !== "object" || typeof right !== "object" || Array.isArray(left) !== Array.isArray(right)) return false;
      const leftKeys = Object.keys(left).filter(key => Array.isArray(left) || key !== "_comments");
      const rightKeys = Object.keys(right).filter(key => Array.isArray(right) || key !== "_comments");
      if (leftKeys.length !== rightKeys.length) return false;
      for (const key of leftKeys) {
        if (!Object.prototype.hasOwnProperty.call(right, key)) return false;
        pending.push([(left as Record<string, unknown>)[key], (right as Record<string, unknown>)[key]]);
      }
    }
    return true;
  } catch { return false; }
}

export function pluginStatusLabel(plugin: CodeyPlugin): string {
  if (plugin.status === "enabling") return "正在启用";
  if (plugin.lastError || plugin.status === "failed" || plugin.status === "error") return "运行异常";
  if (plugin.restartRequired) return "待重新启用";
  return plugin.enabled ? "已启用" : "已停用";
}
