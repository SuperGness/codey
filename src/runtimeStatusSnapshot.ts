import type { RuntimeStatus } from "./App.types";

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
