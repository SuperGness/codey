import type { ModelReasoningEffort } from "./App.types";

/// 与后端 MODEL_REASONING_EFFORT_LEVELS 保持一致。
export const MODEL_REASONING_EFFORT_LEVELS: readonly string[] = [
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

/// 奇数档位排在第二列，界面呈现为两列四行。
export const MODEL_REASONING_EFFORT_COLUMNS: readonly (readonly string[])[] = [
  MODEL_REASONING_EFFORT_LEVELS.filter((_, index) => index % 2 === 0),
  MODEL_REASONING_EFFORT_LEVELS.filter((_, index) => index % 2 === 1),
];

export const MAX_MODEL_REASONING_EFFORTS = MODEL_REASONING_EFFORT_LEVELS.length;
export const MAX_MODEL_REASONING_EFFORT_VALUE_BYTES = 32;

const valueEncoder = new TextEncoder();

export function isReasoningEffortLevel(level: string): boolean {
  return MODEL_REASONING_EFFORT_LEVELS.includes(level.trim());
}

/// 未声明时使用的档位名称，也就是发送给上游的取值。
export function reasoningEffortValue(effort: ModelReasoningEffort): string {
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
