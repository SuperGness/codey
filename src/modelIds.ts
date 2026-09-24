export const AUTO_REVIEW_MODEL = "codex-auto-review";

export const modelKey = (model: string) => model.trim().toLowerCase();

export const modelIdsEqual = (left: string, right: string) =>
  modelKey(left) === modelKey(right);

export const includesModelId = (
  models: readonly string[],
  expected: string,
) => {
  const expectedKey = modelKey(expected);
  return Boolean(expectedKey) && models.some((model) => modelKey(model) === expectedKey);
};

export const withoutModelId = (
  models: readonly string[],
  excluded: string,
) => {
  const excludedKey = modelKey(excluded);
  return models.filter((model) => modelKey(model) !== excludedKey);
};

export const partitionModelIdsByKey = (
  models: readonly string[],
  matchingKeys: ReadonlySet<string>,
) => {
  const matching: string[] = [];
  const remaining: string[] = [];
  for (const model of models) {
    (matchingKeys.has(modelKey(model)) ? matching : remaining).push(model);
  }
  return { matching, remaining };
};

export const uniqueModelIds = (models: readonly string[]) => {
  const seenKeys = new Set<string>();
  return models.reduce<string[]>((unique, model) => {
    const normalized = model.trim();
    const key = modelKey(normalized);
    if (key && !seenKeys.has(key)) {
      seenKeys.add(key);
      unique.push(normalized);
    }
    return unique;
  }, []);
};

/** 按已保存的顺序排列模型：顺序表里有的按其位置靠前，其余保持原有相对顺序排在后面。 */
export const orderModelIdsBy = (
  models: readonly string[],
  preferredOrder: readonly string[],
) => {
  const positions = new Map<string, number>();
  preferredOrder.forEach((model, index) => {
    const key = modelKey(model);
    if (key && !positions.has(key)) positions.set(key, index);
  });
  return models
    .map((model, index) => ({
      model,
      index,
      position: positions.get(modelKey(model)) ?? Number.MAX_SAFE_INTEGER,
    }))
    .sort((left, right) => left.position - right.position || left.index - right.index)
    .map((entry) => entry.model);
};

/** 把 source 移到 target 当前所在的位置；两者相同或任一不存在时返回 null。 */
export const moveModelId = (
  models: readonly string[],
  source: string,
  target: string,
): string[] | null => {
  const next = [...models];
  const sourceIndex = next.findIndex((model) => modelIdsEqual(model, source));
  const targetIndex = next.findIndex((model) => modelIdsEqual(model, target));
  if (sourceIndex < 0 || targetIndex < 0 || sourceIndex === targetIndex) return null;
  next.splice(targetIndex, 0, next.splice(sourceIndex, 1)[0]);
  return next;
};
