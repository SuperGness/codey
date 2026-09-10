import { memo, useEffect, useSyncExternalStore } from "react";
import { toast } from "@heroui/react";

import type { Notice } from "./App.types";
import {
  createExternalStore,
  type ExternalStore,
  useExternalStore,
} from "./externalStore";

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
