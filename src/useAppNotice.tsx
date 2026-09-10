import { memo, useEffect, useState, useSyncExternalStore } from "react";
import {
  IconActivity as Activity,
  IconAlertCircle as CircleAlert,
  IconCircleCheck as CircleCheck,
  IconX as X,
} from "@tabler/icons-react";

import type { Notice } from "./App.types";
import { Button } from "./components/antd";
import {
  createExternalStore,
  type ExternalStore,
  useExternalStore,
} from "./externalStore";

const NOTICE_AUTO_DISMISS_MS = 5_000;
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

type NoticeToastProps = NoticeSubscriberProps & {
  autoDismissEnabled: boolean;
};

export const NoticeToast = memo(function NoticeToast({
  autoDismissEnabled,
  controller,
}: NoticeToastProps) {
  const notice = useSyncExternalStore(
    controller.subscribe,
    controller.getSnapshot,
    controller.getSnapshot,
  );
  const [autoDismissPaused, setAutoDismissPaused] = useState(false);

  useEffect(() => {
    if (!notice.text) setAutoDismissPaused(false);
  }, [notice.text]);

  useEffect(() => {
    if (!autoDismissEnabled || !notice.text || autoDismissPaused) {
      return undefined;
    }
    const timeout = window.setTimeout(() => {
      setAutoDismissPaused(false);
      controller.set((current) =>
        current.text === notice.text && current.tone === notice.tone
          ? { tone: "info", text: "" }
          : current,
      );
    }, NOTICE_AUTO_DISMISS_MS);
    return () => window.clearTimeout(timeout);
  }, [
    autoDismissEnabled,
    autoDismissPaused,
    controller,
    notice.text,
    notice.tone,
  ]);

  if (!notice.text) return null;
  const toneBorder = notice.tone === "success"
    ? "border-l-[#34c759]"
    : notice.tone === "error"
      ? "border-l-[#ff3b30]"
      : "border-l-[#007aff]";
  return (
    <div
      className={`absolute bottom-6 right-6 z-[90] flex max-w-[min(420px,calc(100%_-_48px))] items-center gap-2.5 rounded-xl border border-black/10 border-l-4 bg-white/92 px-4 py-3 text-xs text-[#1d1d1f] shadow-[0_12px_32px_rgba(0,0,0,0.14)] backdrop-blur-2xl ${toneBorder}`}
      role="status"
      aria-live="polite"
      onMouseEnter={() => setAutoDismissPaused(true)}
      onMouseLeave={() => setAutoDismissPaused(false)}
      onFocus={() => setAutoDismissPaused(true)}
      onBlur={() => setAutoDismissPaused(false)}
    >
      {notice.tone === "success" ? (
        <CircleCheck size={17} />
      ) : notice.tone === "error" ? (
        <CircleAlert size={17} />
      ) : (
        <Activity size={17} />
      )}
      <span className="min-w-0 break-words">{notice.text}</span>
      <Button
        className="ml-auto shrink-0"
        variant="ghost"
        size="icon-sm"
        aria-label="关闭提示"
        onClick={() => {
          setAutoDismissPaused(false);
          controller.set({ tone: "info", text: "" });
        }}
      >
        <X aria-hidden="true" />
      </Button>
    </div>
  );
});
