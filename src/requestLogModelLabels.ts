import type { Config, Profile } from "./App.types";
import { modelIdsEqual, modelKey } from "./modelIds";

type ModelLabelCatalog = Pick<Config,
  "selectedModelsByProvider" | "declaredOfficialModelsByProvider" | "upstreamModelsByProvider" | "modelAliasHistory"
> & { profiles: Array<Pick<Profile, "id" | "sourceProviderId">> };

export function requestLogModelLabels(catalog: ModelLabelCatalog) {
  const currentAliases = new Map<string, string>();
  const rawModels = new Map<string, string>();
  const directories = [catalog.upstreamModelsByProvider, catalog.selectedModelsByProvider, catalog.declaredOfficialModelsByProvider];
  for (const directory of directories) {
    for (const models of Object.values(directory)) {
      for (const model of models) {
        if (model.trim()) rawModels.set(modelKey(model), model.trim());
      }
    }
  }
  for (const profile of catalog.profiles) {
    const provider = (profile.sourceProviderId || profile.id).trim();
    // 与路由选择器一致：线路 ID 的非字母、数字、点、下划线、短横线均编码。
    const prefix = encodeURIComponent(provider).replace(/[!'()*~]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);
    for (const directory of directories) {
      for (const model of directory[provider] ?? []) {
        if (model.trim()) currentAliases.set(modelKey(`${prefix}/${model.trim()}`), model.trim());
      }
    }
  }
  const historicalAliases = new Map(Object.entries(catalog.modelAliasHistory ?? {})
    .filter(([alias, model]) => alias.trim() && model.trim())
    .map(([alias, model]) => [modelKey(alias), model.trim()]));
  return { currentAliases, rawModels, historicalAliases };
}

export type RequestLogModelLabels = ReturnType<typeof requestLogModelLabels>;

export function displayRequestLogModelName(model: string, labels: RequestLogModelLabels) {
  const key = modelKey(model);
  // 只匹配完整且可信的选择器，映射结果不再递归解析。
  return labels.currentAliases.get(key) ?? labels.rawModels.get(key)
    ?? labels.historicalAliases.get(key) ?? model.trim();
}

export function requestLogRowModelNames(item: {
  requestedModel: string;
  model?: string | null;
  upstreamResponseModel?: string | null;
}, labels: RequestLogModelLabels) {
  const requested = item.requestedModel.trim();
  const sent = item.model?.trim() ?? "";
  return {
    primaryModel: requested ? displayRequestLogModelName(requested, labels) : sent,
    // 已解析的发送模型和上游回报模型可能本身含斜杠，不能再次当作选择器解析。
    sentDisplay: sent && modelIdsEqual(sent, requested) ? displayRequestLogModelName(sent, labels) : sent,
    returnedDisplay: item.upstreamResponseModel?.trim() ?? "",
  };
}
