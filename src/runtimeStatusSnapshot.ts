import type { RuntimeStatus } from "./App.types";
import { reconcileConfigApply } from "./configApply";

function valuesEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => valuesEqual(value, right[index]))
    );
  }
  if (
    left === null ||
    right === null ||
    typeof left !== "object" ||
    typeof right !== "object"
  ) {
    return false;
  }
  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  const leftKeys = Object.keys(leftRecord);
  const rightKeys = Object.keys(rightRecord);
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every(
      (key) =>
        Object.prototype.hasOwnProperty.call(rightRecord, key) &&
        valuesEqual(leftRecord[key], rightRecord[key]),
    )
  );
}

function reuseEqualValue<Value>(current: Value, next: Value): Value {
  return valuesEqual(current, next) ? current : next;
}

export function reconcileRuntimeStatus(
  current: RuntimeStatus,
  next: RuntimeStatus,
): RuntimeStatus {
  if (valuesEqual(current, next)) return current;

  const reconciled = { ...next };
  const configApply = reconcileConfigApply(current.configApply, next.configApply);
  if (configApply !== undefined) reconciled.configApply = configApply;
  if (next.configApply?.state === "pending" || (current.configApply && next.configApply &&
    next.configApply.revision < current.configApply.revision)) {
    // 应用未结束或响应已过期时，不能把过渡差异当成最终重启结论。
    reconciled.restartRequired = current.restartRequired;
  }
  if (next.maintenance !== undefined) {
    reconciled.maintenance = reuseEqualValue(
      current.maintenance,
      next.maintenance,
    );
  }
  if (next.injectionScripts !== undefined) {
    reconciled.injectionScripts = reuseEqualValue(
      current.injectionScripts,
      next.injectionScripts,
    );
  }
  return reconciled;
}
