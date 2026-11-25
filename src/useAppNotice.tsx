import { memo, useEffect, useRef, useSyncExternalStore } from "react";
import { toast } from "@heroui/react";

import type { ConfigApplyStatus, Notice } from "./App.types";
import {
  createExternalStore,
  type ExternalStore,
  useExternalStore,
} from "./externalStore";

const CONFIG_APPLY_ERROR_TOAST_TIMEOUT_MS = 8_000;

const INITIAL_NOTICE: Notice = {
  tone: "info",
  text: "正在连接 Codey…",
};

export type AppNoticeController = ExternalStore<Notice>;

export function useAppNoticeController(): AppNoticeController {
  return useExternalStore(() => createExternalStore(INITIAL_NOTICE));
}

type NoticeSubscriberProps = {
  controller: AppNoticeController;
};

export const NoticeLoadingText = memo(function NoticeLoadingText({
  controller,
}: NoticeSubscriberProps) {
  const notice = useSyncExternalStore(
    controller.subscribe,
    controller.getSnapshot,
    controller.getSnapshot,
  );
  return <>{notice.text}</>;
});

export const NoticeToast = memo(function NoticeToast({
  controller,
}: NoticeSubscriberProps) {
  const notice = useSyncExternalStore(
    controller.subscribe,
    controller.getSnapshot,
    controller.getSnapshot,
  );
  useEffect(() => {
    if (
      !notice.text || notice.text.startsWith("正在连接 Codey") ||
      controller.getSnapshot() !== notice
    ) {
      return;
    }
    controller.set({ ...notice, text: "" });
    if (notice.tone === "success") {
      toast.success(notice.text);
    } else if (notice.tone === "error") {
      toast.danger(notice.text);
    } else {
      toast.info(notice.text);
    }
  }, [controller, notice]);

  return null;
});

type ConfigApplyErrorToastProps = {
  active?: boolean;
  ready?: boolean;
  configApply?: ConfigApplyStatus | null;
  configApplyError?: string | null;
  onRetry?: () => void;
};

export const ConfigApplyErrorToast = memo(function ConfigApplyErrorToast({
  active = true,
  ready = true,
  configApply,
  configApplyError,
  onRetry,
}: ConfigApplyErrorToastProps) {
  const toastKeyRef = useRef<string | null>(null);
  const lastShownSignatureRef = useRef<string | null>(null);
  const onRetryRef = useRef(onRetry);
  onRetryRef.current = onRetry;

  useEffect(() => {
    const closeActiveToast = () => {
      if (toastKeyRef.current) {
        toast.close(toastKeyRef.current);
        toastKeyRef.current = null;
      }
    };

    if (!active) {
      closeActiveToast();
      return;
    }

    const hasFailure = Boolean(configApply && (configApply.state === "failed" || configApplyError));
    if (!hasFailure || !configApply) {
      closeActiveToast();
      lastShownSignatureRef.current = null;
      return;
    }

    if (!ready) {
      return;
    }

    const signature = configApplyError
      ? `poll:${configApply.revision}:${configApplyError}`
      : `failed:${configApply.revision}:${configApply.error?.trim() || ""}`;
    if (lastShownSignatureRef.current === signature) {
      return;
    }

    closeActiveToast();
    lastShownSignatureRef.current = signature;
    if (configApplyError) {
      toastKeyRef.current = toast.danger("配置应用状态未确认", {
        description: configApplyError,
        timeout: CONFIG_APPLY_ERROR_TOAST_TIMEOUT_MS,
        actionProps: onRetryRef.current
          ? {
              children: "重新查询状态",
              size: "sm",
              variant: "secondary",
              onPress: () => onRetryRef.current?.(),
            }
          : undefined,
      });
    } else {
      toastKeyRef.current = toast.danger("配置已保存，但未能完全应用", {
        description: configApply.error?.trim() || "请检查 Codex 状态后重试",
        timeout: CONFIG_APPLY_ERROR_TOAST_TIMEOUT_MS,
      });
    }
  }, [active, ready, configApply, configApplyError]);

  useEffect(() => () => {
    if (toastKeyRef.current) {
      toast.close(toastKeyRef.current);
      toastKeyRef.current = null;
    }
  }, []);

  return null;
});
