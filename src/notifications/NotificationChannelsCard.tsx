import { memo, useState } from "react";
import {
  IconBell,
  IconEdit,
  IconPlus,
  IconTrash,
} from "@tabler/icons-react";

import type { Config } from "../App.types";
import { Badge, Button, Card } from "../components/antd";
import { surfaceCardPaddingClass } from "../uiClasses";
import { getNotificationChannelDefinition } from "./channelRegistry";
import { NotificationChannelDialog } from "./NotificationChannelDialog";
import {
  MAX_NOTIFICATION_CHANNELS,
  type NotificationChannel,
} from "./types";

type NotificationChannelsCardProps = {
  config: Config;
  container: HTMLElement | null;
  popupContainer: HTMLElement | null;
  isBusy: boolean;
  onAddChannel: (channel: NotificationChannel) => Promise<boolean>;
  onChannelChange: (
    channelId: string,
    patch: Partial<NotificationChannel>,
  ) => Promise<boolean>;
  onRequestRemoveChannel: (channel: NotificationChannel) => void;
};

function NotificationChannelsCardComponent({
  config,
  container,
  popupContainer,
  isBusy,
  onAddChannel,
  onChannelChange,
  onRequestRemoveChannel,
}: NotificationChannelsCardProps) {
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingChannelId, setEditingChannelId] = useState<string | null>(null);
  const editingChannel = editingChannelId
    ? config.webhook.channels.find((channel) => channel.id === editingChannelId) ?? null
    : null;
  const channelLimitReached =
    config.webhook.channels.length >= MAX_NOTIFICATION_CHANNELS;

  function openAddDialog() {
    setEditingChannelId(null);
    setDialogOpen(true);
  }

  function openEditDialog(channelId: string) {
    setEditingChannelId(channelId);
    setDialogOpen(true);
  }

  function handleDialogOpenChange(open: boolean) {
    if (!open) setEditingChannelId(null);
    setDialogOpen(open);
  }

  async function saveChannel(channel: NotificationChannel) {
    if (editingChannelId) {
      return onChannelChange(editingChannelId, channel);
    }
    return onAddChannel(channel);
  }

  function channelStatus(channel: NotificationChannel) {
    if (channel.kind === "wechatClaw" && channel.sessionStatus === "expired") {
      return { label: "登录失效", variant: "warning" as const };
    }
    return channel.enabled
      ? { label: "已启用", variant: "success" as const }
      : { label: "未启用", variant: "secondary" as const };
  }

  return (
    <>
      <section className="secondary-section" aria-labelledby="notification-title">
        <div className="section-title compact">
          <div className="section-heading">
            <span className="section-icon" aria-hidden="true">
              <IconBell size={15} />
            </span>
            <div>
              <h2 id="notification-title">消息通知</h2>
              <p>已配置渠道会同时接收完成、失败和等待提醒。</p>
            </div>
          </div>
          <div className="notification-add-actions">
            <Button
              variant="default"
              size="sm"
              disabled={isBusy || channelLimitReached}
              title={
                channelLimitReached
                  ? `最多可添加 ${MAX_NOTIFICATION_CHANNELS} 个通知渠道`
                  : undefined
              }
              onClick={openAddDialog}
            >
              <IconPlus size={13} strokeWidth={2.2} aria-hidden="true" />
              <span>添加渠道</span>
            </Button>
          </div>
        </div>
        {config.webhook.channels.length === 0 ? (
          <Card className={`secondary-card notification-empty ${surfaceCardPaddingClass}`}>
            <IconBell size={20} aria-hidden="true" />
            <strong>还没有通知渠道</strong>
            <small>点击“添加渠道”选择推送方式并完成配置。</small>
          </Card>
        ) : (
          <ul className="notification-channel-list" aria-label="已配置通知渠道">
            {config.webhook.channels.map((channel) => {
              const definition = getNotificationChannelDefinition(channel.kind);
              const status = channelStatus(channel);
              const cardState =
                status.variant === "warning"
                  ? "expired"
                  : channel.enabled
                    ? "active"
                    : "inactive";
              const ChannelIcon = definition.Icon;
              return (
                <li key={channel.id}>
                  <Card
                    className={`secondary-card notification-card ${surfaceCardPaddingClass} ${cardState}`}
                  >
                    <div className="notification-card-header">
                      <div className="notification-title">
                        <span className={definition.iconClassName}>
                          <ChannelIcon size={18} aria-hidden="true" />
                        </span>
                        <div>
                          <strong>{definition.title}</strong>
                        </div>
                      </div>
                      <div className="notification-channel-controls">
                        <Badge variant={status.variant}>{status.label}</Badge>
                        <div className="notification-item-actions">
                          <Button
                            variant="ghost"
                            size="xs"
                            disabled={isBusy}
                            onClick={() => openEditDialog(channel.id)}
                            aria-label={`编辑${definition.title}通知渠道`}
                            title={`编辑${definition.title}`}
                          >
                            <IconEdit size={13} aria-hidden="true" />
                          </Button>
                          <Button
                            variant="destructive-light"
                            size="xs"
                            disabled={isBusy}
                            onClick={() => onRequestRemoveChannel(channel)}
                            aria-label={`删除${definition.addLabel}通知渠道`}
                            title={`删除${definition.title}`}
                          >
                            <IconTrash size={13} aria-hidden="true" />
                          </Button>
                        </div>
                      </div>
                    </div>
                  </Card>
                </li>
              );
            })}
          </ul>
        )}
      </section>
      <NotificationChannelDialog
        container={container}
        popupContainer={popupContainer}
        editingChannel={editingChannel}
        isBusy={isBusy}
        open={dialogOpen}
        onOpenChange={handleDialogOpenChange}
        onSave={saveChannel}
      />
    </>
  );
}

export const NotificationChannelsCard = memo(
  NotificationChannelsCardComponent,
);
