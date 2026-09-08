import { memo } from "react";

import { Button, Input } from "../components/antd";
import { inputShellClass, insetInputClass } from "../uiClasses";
import type { NotificationChannelEditorProps } from "./types";

function TelegramChannelEditorComponent({
  channel,
  disabled,
  onChange,
}: NotificationChannelEditorProps) {
  return (
    <>
      <label className="field notification-field-row">
        <span>Bot Token</span>
        <div className={inputShellClass}>
          <Input
            className={insetInputClass}
            type="text"
            value={channel.botToken}
            disabled={disabled}
            onChange={(event) =>
              onChange({
                botToken: event.target.value,
                clearBotToken: false,
              })
            }
            placeholder={
              channel.botTokenConfigured
                ? "已保存；输入新 Token 可替换"
                : "从 BotFather 获取"
            }
            autoComplete="off"
            spellCheck={false}
          />
        </div>
      </label>
      {channel.botTokenConfigured ? (
        <div className="-mt-[7px] flex justify-end">
          <Button
            className="text-[#8e8e93] hover:text-[#d70015]"
            variant="ghost"
            size="xs"
            disabled={disabled}
            onClick={() =>
              onChange({
                botToken: "",
                botTokenConfigured: false,
                clearBotToken: true,
              })
            }
          >
            清除已保存 Token
          </Button>
        </div>
      ) : null}
      <label className="field notification-field-row">
        <span>Chat ID</span>
        <div className={inputShellClass}>
          <Input
            className={insetInputClass}
            value={channel.chatId}
            disabled={disabled}
            onChange={(event) => onChange({ chatId: event.target.value })}
            placeholder="-1001234567890"
            spellCheck={false}
          />
        </div>
      </label>
    </>
  );
}

export const TelegramChannelEditor = memo(TelegramChannelEditorComponent);
