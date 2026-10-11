import type { Notice } from "./App.types";

export type ModelRuntimeUpdate = {
  restartRequired?: boolean;
  modelHotReloaded?: boolean;
  modelHotReloadDeferred?: boolean;
  modelHotReloadError?: string;
  subagentConfigHotReloaded?: boolean;
  subagentConfigRepaired?: boolean;
  subagentConfigHealth?: string;
  subagentConfigHotReloadError?: string;
  modelCatalogFallback?: boolean;
  customContextsRestored?: boolean;
};

export function subagentUpdateNote(result: ModelRuntimeUpdate): string {
  if (result.subagentConfigHealth === "pending_restart") {
    return result.subagentConfigHotReloadError
      || "当前 Codex 尚未注册新增的子代理角色，请重启一次以启用动态角色配置";
  }
  if (result.subagentConfigHealth === "superseded") {
    return result.subagentConfigHotReloadError || "子代理配置已被更新的设置取代，请确认当前设置";
  }
  return result.subagentConfigHotReloadError
    ? `子代理配置未能实时更新：${result.subagentConfigHotReloadError}`
    : "";
}

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

  const subagentNote = subagentUpdateNote(result);
  if (subagentNote) {
    return {
      tone: "info",
      text: `${summary}；${subagentNote}`,
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
