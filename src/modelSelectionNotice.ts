import type { Notice } from "./App.types";

export type ModelRuntimeUpdate = {
  restartRequired?: boolean;
  modelHotReloaded?: boolean;
  modelHotReloadDeferred?: boolean;
  modelHotReloadError?: string;
  subagentConfigHotReloaded?: boolean;
  subagentConfigRepaired?: boolean;
  subagentConfigHotReloadError?: string;
  modelCatalogFallback?: boolean;
};

export function modelSelectionNotice(
  result: ModelRuntimeUpdate,
  summary: string,
): Notice {
  const messages = [summary];
  if (result.modelHotReloadError) {
    messages.push("Codex 模型列表刷新失败，重启 Codex 后生效");
  } else if (result.modelHotReloaded) {
    messages.push(result.modelHotReloadDeferred
      ? "Codex 模型列表将在打开模型选择器时更新"
      : "Codex 模型列表已立即更新");
  } else if (result.restartRequired) {
    messages.push("线路运行配置需重启，Codex 模型列表将在重启后更新");
  }

  if (result.subagentConfigHotReloadError) {
    messages.push("子代理配置暂未能更新，重启 Codex 后生效");
  } else if (result.subagentConfigRepaired) {
    messages.push("已校验并修复受影响的子代理运行配置");
  } else if (result.subagentConfigHotReloaded) {
    messages.push("受影响的子代理角色也已同步");
  }
  if (result.modelHotReloaded && result.restartRequired) {
    messages.push("模型能力或其他设置需重启 Codex 后生效");
  }
  return {
    tone: result.modelHotReloadError
      || result.subagentConfigHotReloadError
      || result.restartRequired
      || result.modelHotReloadDeferred ? "info" : "success",
    text: messages.join("；"),
  };
}
