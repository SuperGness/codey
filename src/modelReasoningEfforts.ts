import type { ModelReasoningEffort } from "./App.types";

/// 与后端 MODEL_REASONING_EFFORT_LEVELS 保持一致。
export const MODEL_REASONING_EFFORT_LEVELS: readonly string[] = [
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

const MAX_MODEL_REASONING_EFFORT_VALUE_BYTES = 32;

const valueEncoder = new TextEncoder();

function isReasoningEffortLevel(level: string): boolean {
  return MODEL_REASONING_EFFORT_LEVELS.includes(level.trim());
}

/// 未声明时使用的档位名称，也就是发送给上游的取值。
function reasoningEffortValue(effort: ModelReasoningEffort): string {
  return effort.value.trim() || effort.level.trim();
}

/// 按界面顺序整理声明，补全空取值并丢弃无效档位。
export function normalizeReasoningEfforts(
  efforts: readonly ModelReasoningEffort[],
): ModelReasoningEffort[] {
  const byLevel = new Map<string, ModelReasoningEffort>();
  for (const effort of efforts) {
    const level = effort.level.trim();
    if (!isReasoningEffortLevel(level) || byLevel.has(level)) continue;
    const value = reasoningEffortValue(effort);
    if (!value || valueEncoder.encode(value).byteLength > MAX_MODEL_REASONING_EFFORT_VALUE_BYTES) {
      continue;
    }
    byLevel.set(level, { level, value });
  }
  return MODEL_REASONING_EFFORT_LEVELS.filter((level) => byLevel.has(level)).map(
    (level) => byLevel.get(level)!,
  );
}

/// 上游模板声明的档位，作为自动适配的基准。
export function autoReasoningEfforts(
  autoSupportedReasoningEfforts: readonly string[] | undefined,
): ModelReasoningEffort[] {
  return normalizeReasoningEfforts(
    (autoSupportedReasoningEfforts ?? []).map((value) => ({
      level: value,
      value,
    })),
  );
}

export function reasoningEffortsEqual(
  left: readonly ModelReasoningEffort[],
  right: readonly ModelReasoningEffort[],
): boolean {
  if (left.length !== right.length) return false;
  return left.every(
    (effort, index) =>
      effort.level === right[index].level && effort.value === right[index].value,
  );
}

/// 插件声明决定可用范围；用户缩小范围后仍能恢复完整的自动适配选项。
export function resolveModelReasoningEfforts(
  templateLevels: readonly string[],
  stored: readonly ModelReasoningEffort[] | undefined,
  capabilityLevels: readonly string[] | undefined,
): { autoEfforts: ModelReasoningEffort[]; efforts: ModelReasoningEffort[] } {
  const autoEfforts = autoReasoningEfforts(capabilityLevels ?? templateLevels);
  if (!stored) return { autoEfforts, efforts: autoEfforts };
  const normalized = normalizeReasoningEfforts(stored);
  if (!capabilityLevels) return { autoEfforts, efforts: normalized };
  const allowed = new Set(capabilityLevels);
  const bounded: ModelReasoningEffort[] = [];
  for (const effort of normalized) {
    if (allowed.has(effort.level)) {
      const value = ["max", "ultra"].includes(effort.value) && !allowed.has(effort.value)
        ? effort.level
        : effort.value;
      bounded.push({ level: effort.level, value });
    } else if (["max", "ultra"].includes(effort.level) && allowed.has("xhigh")) {
      bounded.push({ level: "xhigh", value: "xhigh" });
    }
  }
  const efforts = normalizeReasoningEfforts(bounded);
  return { autoEfforts, efforts: efforts.length ? efforts : autoEfforts };
}
