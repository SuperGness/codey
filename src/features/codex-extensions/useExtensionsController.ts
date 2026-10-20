import { useCallback, useEffect, useRef, useState } from "react";
import type {
  CheckResult,
  ExtensionTransport,
  Inventory,
  MutationResult,
  Scope,
} from "./types";
import { causeText } from "./state";
import { readInventory, invalidateInventory, withTimeout } from "./requests";

/**
 * run 的结果：ok 请求完成且本地状态同步；superseded 结果已过期，未更新当前状态；
 * failed 请求未发起或失败，调用方应保留草稿。
 */
export type RunOutcome = "ok" | "superseded" | "failed";

export function useExtensionsController(
  request: ExtensionTransport,
  active: boolean,
) {
  const [scope, setScope] = useState<Scope>({ kind: "user" });
  const [inventory, setInventory] = useState<Inventory | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [busyAction, setBusyAction] = useState("");
  const [busyResourceId, setBusyResourceId] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [noticeSeq, setNoticeSeq] = useState(0);
  const [check, setCheck] = useState<
    (CheckResult & { id: string; revision?: string }) | null
  >(null);
  const [checks, setChecks] = useState<
    Record<string, CheckResult & { revision?: string }>
  >({});
  // 读写各用一条序号：run 抢占刷新不再作废在飞的清单读取，刷新也不会作废在飞的提交。
  const readEpoch = useRef(0);
  const writeEpoch = useRef(0);
  const locked = useRef<object | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      readEpoch.current++;
      writeEpoch.current++;
    };
  }, []);
  const refresh = useCallback(
    async (force = true) => {
      const generation = ++readEpoch.current;
      const operationAtStart = writeEpoch.current;
      setLoading(true);
      setError("");
      try {
        const result = await readInventory(request, scope, force);
        if (mounted.current && generation === readEpoch.current) {
          setInventory(result);
          if (operationAtStart === writeEpoch.current && !locked.current)
            setUncertain(false);
          return result;
        }
        return null;
      } catch (cause) {
        if (mounted.current && generation === readEpoch.current) {
          setError(causeText(cause));
        }
        return null;
      } finally {
        if (mounted.current && generation === readEpoch.current) setLoading(false);
      }
    },
    [request, scope],
  );
  useEffect(() => {
    setInventory(null);
    setCheck(null);
    setNotice("");
    setChecks({});
    setBusy(false);
    setBusyAction("");
    setBusyResourceId("");
    setUncertain(false);
    // 序号失效后旧请求不会再清 loading，这里显式收尾，避免切走范围时卡在加载态。
    setLoading(false);
    locked.current = null;
    if (active) void refresh(false);
    return () => {
      readEpoch.current++;
      writeEpoch.current++;
    };
  }, [active, refresh]);
  const run = useCallback(
    async <T>(
      action: Record<string, unknown>,
      onSuccess?: (result: T) => void,
      isCurrent?: () => boolean,
    ): Promise<RunOutcome> => {
      if (locked.current) return "failed";
      const token = {};
      locked.current = token;
      setBusy(true);
      setBusyAction(String(action.action));
      setBusyResourceId(typeof action.id === "string" ? action.id : "");
      setError("");
      const generation = ++writeEpoch.current;
      const mutation = ![
        "get_mcp",
        "read_skill",
        "export_skill",
        "pick_project",
        "pick_skill",
        "test_mcp",
        "validate_skill",
        "preview_skill_install",
      ].includes(String(action.action));
      if (mutation && uncertain) {
        setError("上次操作结果尚不确定，请先刷新确认后再提交修改。");
        locked.current = null;
        setBusy(false);
        setBusyAction("");
        setBusyResourceId("");
        return "failed";
      }
      if (mutation) {
        // 项目 Skill 的启停也写入用户配置，所有范围的清单均需失效。
        invalidateInventory(request);
      }
      const actionName = String(action.action);
      const timeoutMs = actionName === "test_mcp"
        ? 35000
        : ["save_mcp", "set_mcp_enabled", "set_mcps_enabled", "remove_mcp"].includes(actionName)
          ? 45000
          : actionName.startsWith("pick_")
            ? 120000
            : mutation || actionName === "preview_skill_install"
              ? 30000
              : 15000;
      try {
        const result = await withTimeout(
          request<T>({
            scope,
            revision: inventory?.revision,
            ...action,
          }),
          mutation,
          timeoutMs,
        );
        if (!mounted.current) return "failed";
        // 操作、范围或输入发生变化后，旧结果不能再更新当前页面。
        if (generation !== writeEpoch.current || isCurrent?.() === false)
          return "superseded";
        onSuccess?.(result);
        return "ok";
      } catch (cause) {
        // 输入已变化时，旧读取的错误也不能显示在新草稿中。
        if (isCurrent?.() === false) return "superseded";
        const message = causeText(cause);
        if (mounted.current && generation === writeEpoch.current) {
          setError(message);
          if (mutation && message.includes("结果尚不确定")) setUncertain(true);
        }
        return "failed";
      } finally {
        if (mutation) invalidateInventory(request);
        if (locked.current === token) {
          locked.current = null;
          if (mounted.current) {
            setBusy(false);
            setBusyAction("");
            setBusyResourceId("");
          }
        }
      }
    },
    [request, scope, inventory?.revision, uncertain],
  );
  const postNotice = useCallback((text: string) => {
    setNotice(text);
    setNoticeSeq((current) => current + 1);
  }, []);
  const mutate = useCallback(
    async (action: Record<string, unknown>) => {
      const outcome = await run<MutationResult>(action, (result) => {
        // 已提交的清单比提交前启动的读取更新，旧读取不得再覆盖它。
        readEpoch.current++;
        setLoading(false);
        setInventory(result.inventory);
        // 后端 message 已说明保存结果，这里只在仍需重启时补充生效方式，避免同一句提示重复两遍。
        postNotice(
          result.applyStatus === "restart-required"
            ? "配置已保存，重启 Codex 后生效。"
            : result.message,
        );
      });
      // 提交已落库但清单结果被抢占：不能静默丢弃，提示用户刷新确认。
      if (outcome === "superseded")
        postNotice("操作已提交，请刷新确认。");
      return outcome;
    },
    [postNotice, run],
  );
  const inspect = useCallback(
    (action: Record<string, unknown>) =>
      run<CheckResult>(action, (result) => {
        const id = String(action.id),
          revision = inventory?.revision;
        const checkedAt = new Date().toISOString();
        setCheck({ ...result, id, revision, checkedAt });
        setChecks((current) => ({
          ...current,
          [id]: { ...result, revision, checkedAt },
        }));
      }),
    [run, inventory?.revision],
  );
  return {
    scope,
    setScope,
    inventory,
    loading,
    busy,
    busyAction,
    busyResourceId,
    uncertain,
    clearError: () => setError(""),
    error,
    notice,
    noticeSeq,
    check,
    checks,
    refresh,
    run,
    mutate,
    inspect,
  };
}
