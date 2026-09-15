import { isCodeyApiError } from "./api";
import { errorText } from "./appUtils";

export type InjectionRepairNotice = {
  tone: "error" | "info";
  text: string;
  /** Whether the renderer must wait for a restart that the request started. */
  unconfirmed: boolean;
};

/**
 * The bridge answers `status: "failed"` when the backend rejects the request
 * before the repair starts. Losing the embedded page only takes away the reply,
 * so that case still waits for the restart and a later status refresh.
 */
export function repairOperationResult(error: unknown): InjectionRepairNotice {
  if (isCodeyApiError(error)) {
    return {
      tone: "error",
      text: `修复请求被拒绝：${errorText(error)}`,
      unconfirmed: false,
    };
  }
  return {
    tone: "info",
    text: `修复请求结果待确认：${errorText(error)}。请稍后查询运行状态；后台修复失败会弹出提示。`,
    unconfirmed: true,
  };
}
