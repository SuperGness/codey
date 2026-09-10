import { memo } from "react";

import { Input } from "../components/ui";
import { inputShellClass, insetInputClass } from "../uiClasses";
import type { NotificationChannelEditorProps } from "./types";

export function createWebhookChannelEditor(emptyPlaceholder: string) {
  function WebhookChannelEditor({
    channel,
    disabled,
    onChange,
  }: NotificationChannelEditorProps) {
    return (
      <>
        <label className="field notification-field-row">
          <span>Webhook 地址</span>
          <div className={inputShellClass}>
            <Input
              className={insetInputClass}
              type="text"
              value={channel.url}
              disabled={disabled}
              onChange={(event) =>
                onChange({
                  url: event.target.value,
                  clearUrl: false,
                })
              }
              placeholder={
                channel.urlConfigured
                  ? "已保存；输入新地址可替换"
                  : emptyPlaceholder
              }
              autoComplete="off"
              spellCheck={false}
            />
          </div>
        </label>
      </>
    );
  }

  return memo(WebhookChannelEditor);
}
