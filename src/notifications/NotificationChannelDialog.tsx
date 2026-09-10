import { memo, useLayoutEffect, useState } from "react";
import {
  IconCheck,
  IconLoader2 as LoaderCircle,
  IconSend,
} from "@tabler/icons-react";

import { invoke } from "../api";
import { errorText, withTimeout } from "../appUtils";
import { toast } from "@heroui/react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Select,
  Switch,
} from "../components/ui";
import {
  createNotificationChannel,
  getNotificationChannelDefinition,
  notificationChannelDefinitions,
} from "./channelRegistry";
import type { NotificationChannel, NotificationChannelKind } from "./types";

const defaultNotificationChannelKind = notificationChannelDefinitions[0].kind;

type NotificationChannelDialogProps = {
  container?: HTMLElement | null;
  popupContainer: HTMLElement | null;
  editingChannel: NotificationChannel | null;
  isBusy: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSave: (channel: NotificationChannel) => Promise<boolean>;
};

function NotificationChannelDialogComponent({
  container,
  popupContainer,
  editingChannel,
  isBusy,
  open,
  onOpenChange,
  onSave,
}: NotificationChannelDialogProps) {
  const [draft, setDraft] = useState<NotificationChannel | null>(null);
  const [isTesting, setIsTesting] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const isEditing = editingChannel !== null;
  const definition = draft
    ? getNotificationChannelDefinition(draft.kind)
    : null;

  useLayoutEffect(() => {
    if (!open) {
      setDraft(null);
      setIsTesting(false);
      setIsSaving(false);
      return;
    }
    setDraft(
      editingChannel
        ? { ...editingChannel }
        : createNotificationChannel(defaultNotificationChannelKind),
    );
    setIsTesting(false);
    setIsSaving(false);
  }, [editingChannel?.id, open]);

  function selectChannel(kind: NotificationChannelKind) {
    if (draft?.kind === kind) return;
    setDraft(createNotificationChannel(kind));
  }

  function updateDraft(patch: Partial<NotificationChannel>) {
    setDraft((current) =>
      current ? { ...current, ...patch } : current,
    );
  }

  function closeDialog() {
    if (!isBusy && !isTesting && !isSaving) onOpenChange(false);
  }

  async function saveChannel() {
    if (
      !draft ||
      !definition?.isConfigured(draft)
    ) {
      return;
    }
    setIsSaving(true);
    try {
      if (await onSave(draft)) onOpenChange(false);
    } finally {
      setIsSaving(false);
    }
  }

  async function testChannel() {
    if (
      !draft ||
      !definition?.isConfigured(draft) ||
      isBusy ||
      isTesting
    ) {
      return;
    }
    setIsTesting(true);
    try {
      await withTimeout(
        invoke("test_notification_channel", { channel: draft }),
        12_000,
        `${definition.addLabel}测试在 12 秒内没有完成，请检查渠道配置和网络`,
      );
      toast.success(
        draft.kind === "wechatClaw"
          ? "iLink 已接受测试消息，请在微信中确认接收"
          : "测试发送成功",
      );
    } catch (error) {
      toast.danger(errorText(error));
    } finally {
      setIsTesting(false);
    }
  }

  const ChannelEditor = definition?.Editor;
  const SelectedChannelIcon = definition?.Icon;
  const notificationChannelOptions = notificationChannelDefinitions.map((item) => ({
    Icon: item.Icon,
    label: item.displayName,
    value: item.kind,
  }));
  const formBusy = isBusy || isTesting || isSaving;
  const canTest = Boolean(draft && definition?.isConfigured(draft));
  const canSave = Boolean(draft && definition?.isConfigured(draft));

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => {
      if (nextOpen || (!isBusy && !isTesting && !isSaving)) onOpenChange(nextOpen);
    }}>
      <DialogContent
        className="notification-channel-dialog"
        container={container ?? popupContainer ?? undefined}
        onEscapeKeyDown={(event) => {
          if (isBusy || isTesting || isSaving) event.preventDefault();
        }}
        onPointerDownOutside={(event) => {
          if (isBusy || isTesting || isSaving) event.preventDefault();
        }}
      >
        {draft && definition && ChannelEditor && SelectedChannelIcon ? (
          <>
            <DialogHeader>
              <DialogTitle>
                {isEditing ? `编辑${definition.addLabel}渠道` : "添加通知渠道"}
              </DialogTitle>
              <DialogDescription>
                {isEditing
                  ? draft.kind === "wechatClaw"
                    ? "已绑定的接收用户会直接显示；重新扫码可更换绑定。"
                    : "已保存的配置可直接查看和修改；地址或 Token 留空会保留。"
                  : "选择发送渠道，并填写该渠道需要的专属配置。"}
              </DialogDescription>
            </DialogHeader>
            <div className="notification-channel-select-row mt-[18px]">
              <span
                id="notification-channel-select-label"
                className="text-[11px] font-semibold text-[#6e6e73]"
              >
                发送渠道
              </span>
              <div className="relative w-[min(100%,260px)]">
                <Select
                  className="w-full"
                  value={draft.kind}
                  disabled={isEditing || formBusy}
                  aria-labelledby="notification-channel-select-label"
                  optionList={notificationChannelOptions}
                  filter={false}
                  prefix={
                    <span className="grid size-[22px] shrink-0 place-items-center">
                      <SelectedChannelIcon size={20} aria-hidden="true" />
                    </span>
                  }
                  renderOptionItem={(option) => {
                    const optionDefinition = notificationChannelDefinitions.find(
                      (item) => item.kind === option.value,
                    );
                    const OptionIcon = optionDefinition?.Icon;
                    const label =
                      optionDefinition?.displayName ?? option.label;
                    if (!OptionIcon) return label;
                    return (
                      <div
                        className="flex items-center gap-2"
                      >
                        <span className="grid size-[22px] shrink-0 place-items-center">
                          <OptionIcon size={20} aria-hidden="true" />
                        </span>
                        <span>{label}</span>
                      </div>
                    );
                  }}
                  onChange={(value) =>
                    selectChannel(String(value) as NotificationChannelKind)
                  }
                />
              </div>
            </div>
            <div className="notification-fields notification-dialog-fields">
              <ChannelEditor
                channel={draft}
                disabled={formBusy}
                onChange={updateDraft}
              />
            </div>
            <div className="notification-dialog-actions">
              <div className="notification-enabled-control">
                <div>
                  <strong>启用此渠道</strong>
                  <small>关闭后不接收自动通知</small>
                </div>
                <Switch
                  checked={draft.enabled}
                  disabled={formBusy}
                  onCheckedChange={(enabled) => updateDraft({ enabled })}
                  aria-label={`启用${definition.addLabel}通知`}
                />
              </div>
              <div className="notification-dialog-test">
                <div>
                  <strong>测试发送</strong>
                  <span className="inline-result">
                    {canTest
                      ? "可先测试，也可直接保存"
                      : "填写配置后可测试"}
                  </span>
                </div>
                <Button
                  className="shrink-0"
                  variant="secondary"
                  size="sm"
                  disabled={formBusy || !canTest}
                  onClick={() => void testChannel()}
                >
                  {isTesting ? (
                    <LoaderCircle className="animate-spin" aria-hidden="true" />
                  ) : (
                    <IconSend aria-hidden="true" />
                  )}
                  {isTesting ? "正在测试" : "测试发送"}
                </Button>
              </div>
            </div>
            <DialogFooter>
              <Button variant="outline" disabled={formBusy} onClick={closeDialog}>
                取消
              </Button>
              <Button
                disabled={formBusy || !canSave}
                onClick={() => void saveChannel()}
              >
                {isSaving ? (
                  <LoaderCircle className="animate-spin" aria-hidden="true" />
                ) : (
                  <IconCheck aria-hidden="true" />
                )}
                {isSaving
                  ? "正在保存"
                  : isEditing
                    ? "保存配置"
                    : "添加渠道"}
              </Button>
            </DialogFooter>
          </>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

export const NotificationChannelDialog = memo(
  NotificationChannelDialogComponent,
);
