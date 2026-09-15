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
  customContextsRestored?: boolean;
};

export function modelSelectionNotice(
  result: ModelRuntimeUpdate,
  summary = "已保存模型",
): Notice {
  const notice = savedModelNotice(result, summary);
  const contextNote = customContextRestoredNote(result);
  if (!contextNote) {
    return notice;
  }
  return {
    tone: "info",
    text: `${notice.text}${contextNote}`,
  };
}

/** 保存时本机 Codex 模型缓存不完整，自定义上下文预算已恢复为默认值。 */
export function customContextRestoredNote(result: {
  customContextsRestored?: boolean;
}): string {
  return result.customContextsRestored
    ? "；本机 Codex 模型缓存不完整，自定义上下文预算已恢复为默认值"
    : "";
}

function savedModelNotice(result: ModelRuntimeUpdate, summary: string): Notice {
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
