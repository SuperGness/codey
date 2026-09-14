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
  summary = "已保存模型",
): Notice {
  if (result.modelHotReloadError) {
    return {
      tone: "info",
      text: `${summary}；模型刷新失败，需重启 Codex 后生效`,
    };
  }

  if (result.subagentConfigHotReloadError) {
    return {
      tone: "info",
      text: `${summary}；子代理配置更新失败，需重启 Codex 后生效`,
    };
  }

  if (result.restartRequired) {
    return {
      tone: "info",
      text: `${summary}，需重启 Codex 后生效`,
    };
  }

  return {
    tone: "success",
    text: summary,
  };
}
