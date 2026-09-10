import { memo, useEffect, useRef, useState } from "react";
import {
  IconBrandWechat,
  IconLoader2 as LoaderCircle,
  IconQrcode,
} from "@tabler/icons-react";

import { invoke } from "../api";
import { errorText } from "../appUtils";
import { Button, Input } from "../components/ui";
import { inputShellClass, insetInputClass } from "../uiClasses";
import type { NotificationChannelEditorProps } from "./types";

type WechatClawLoginStartResult = {
  loginId: string;
  status: "wait";
  qrCode?: string;
  qrCodeImageUrl?: string;
};

type WechatClawLoginPollResult = {
  status: "wait" | "scanned" | "activating" | "confirmed" | "expired" | "failed";
  message?: string;
  baseUrl?: string;
  botToken?: string;
  recipientId?: string;
  contextToken?: string;
  getUpdatesBuf?: string;
};

type ActiveLogin = {
  loginId: string;
  qrCodeImageUrl?: string;
  phase: "waiting" | "scanned" | "activating" | "confirmed" | "expired" | "failed";
  message: string;
};

function WechatClawChannelEditorComponent({
  channel,
  disabled,
  onChange,
}: NotificationChannelEditorProps) {
  const [login, setLogin] = useState<ActiveLogin | null>(null);
  const [isStarting, setIsStarting] = useState(false);
  const mounted = useRef(true);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    const loginId = login?.loginId;
    if (!loginId) return;

    let cancelled = false;
    let timer: number | undefined;
    const scheduleNext = () => {
      // iLink can long-poll this request; the small delay only protects against
      // an immediate `wait` response and keeps the temporary binding flow quiet.
      timer = window.setTimeout(() => void poll(), 1_200);
    };
    const poll = async () => {
      try {
        const result = await invoke<WechatClawLoginPollResult>(
          "poll_wechat_claw_login",
          { loginId },
        );
        if (cancelled) return;

        if (result.status === "confirmed") {
          const token = result.botToken?.trim();
          const baseUrl = result.baseUrl?.trim();
          const recipientId = result.recipientId?.trim();
          const contextToken = result.contextToken?.trim();
          if (!token || !baseUrl || !recipientId || !contextToken) {
            setLogin((current) => current?.loginId === loginId
              ? { ...current, phase: "failed", message: "微信 ClawBot 没有返回完整的激活凭据，请重新扫码" }
              : current);
            return;
          }
          onChangeRef.current({
            url: baseUrl,
            urlConfigured: true,
            clearUrl: false,
            botToken: token,
            botTokenConfigured: true,
            clearBotToken: false,
            contextToken,
            contextTokenConfigured: true,
            clearContextToken: false,
            getUpdatesBuf: result.getUpdatesBuf?.trim() ?? "",
            chatId: recipientId,
            sessionStatus: "active",
          });
          setLogin((current) => current?.loginId === loginId
            ? { ...current, phase: "confirmed", message: "绑定并激活成功，保存后即可接收通知。" }
            : current);
          return;
        }

        if (result.status === "expired" || result.status === "failed") {
          setLogin((current) => current?.loginId === loginId
            ? {
              ...current,
              phase: result.status === "expired" ? "expired" : "failed",
              message: result.message || "微信 ClawBot 登录没有完成，请重新扫码",
            }
            : current);
          return;
        }

        setLogin((current) => current?.loginId === loginId
          ? {
            ...current,
            phase: result.status === "activating"
              ? "activating"
              : result.status === "scanned"
                ? "scanned"
                : "waiting",
            message: result.message || (result.status === "activating"
              ? "请在微信中打开 ClawBot，并发送一条消息完成激活。"
              : result.status === "scanned"
                ? "已扫码，请在微信中确认授权。"
                : "请使用微信扫描二维码。"),
          }
          : current);
        scheduleNext();
      } catch (error) {
        if (!cancelled) {
          setLogin((current) => current?.loginId === loginId
            ? { ...current, phase: "failed", message: errorText(error) }
            : current);
        }
      }
    };

    void poll();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [login?.loginId]);

  async function startLogin() {
    if (disabled || isStarting) return;
    setLogin(null);
    setIsStarting(true);
    try {
      const result = await invoke<WechatClawLoginStartResult>(
        "start_wechat_claw_login",
      );
      if (!mounted.current) return;
      setLogin({
        loginId: result.loginId,
        qrCodeImageUrl: result.qrCodeImageUrl,
        phase: "waiting",
        message: "请使用微信扫描二维码，并在手机上确认授权。",
      });
    } catch (error) {
      if (mounted.current) {
        setLogin({
          loginId: "",
          phase: "failed",
          message: errorText(error),
        });
      }
    } finally {
      if (mounted.current) setIsStarting(false);
    }
  }

  const sessionExpired = channel.sessionStatus === "expired";
  const hasBinding = Boolean(
    sessionExpired || channel.botToken.trim() || channel.botTokenConfigured,
  );
  const isActivated = Boolean(
    hasBinding &&
      !sessionExpired &&
      (channel.contextToken.trim() || channel.contextTokenConfigured) &&
      channel.chatId.trim(),
  );
  const loginMessage = login?.message || (sessionExpired
    ? "微信 ClawBot 登录已失效，请重新扫码后保存配置。"
    : isActivated
      ? "当前已完成绑定和激活。重新扫码会替换已保存的凭据。"
      : hasBinding
        ? "当前绑定缺少激活上下文，请重新扫码并按提示向 ClawBot 发送一条消息。"
        : "扫码确认后，需要在微信中向 ClawBot 发送一条消息完成激活。二维码 10 分钟内有效。");
  const bindingCardClass = sessionExpired
    ? "rounded-[10px] border border-[#f59e0b]/35 bg-[#fff8eb] p-3"
    : "rounded-[10px] border border-[#07c160]/25 bg-[#f2fff5] p-3";
  const bindingIconClass = sessionExpired
    ? "grid size-7 place-items-center rounded-full bg-[#f59e0b]/12 text-[#a15c00]"
    : "grid size-7 place-items-center rounded-full bg-[#07c160]/12 text-[#07a854]";

  return (
    <>
      <div className={bindingCardClass}>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <span className={bindingIconClass}>
              <IconQrcode size={17} aria-hidden="true" />
            </span>
            <div>
              <strong className="block text-xs text-[#1d1d1f]">微信 ClawBot 绑定</strong>
              <span className="block text-[11px] text-[#5d6b61]">无需企业微信机器人或常驻转发服务</span>
            </div>
          </div>
          <Button
            variant="secondary"
            size="xs"
            disabled={disabled || isStarting}
            onClick={() => void startLogin()}
          >
            {isStarting ? <LoaderCircle className="animate-spin" aria-hidden="true" /> : <IconBrandWechat aria-hidden="true" />}
            {isStarting ? "正在生成" : sessionExpired || hasBinding ? "重新扫码" : "扫码绑定"}
          </Button>
        </div>
        <p className="mt-2 text-[11px] leading-5 text-[#526158]" role="status" aria-live="polite">
          {loginMessage}
        </p>
        {login?.qrCodeImageUrl && (login.phase === "waiting" || login.phase === "scanned") ? (
          <div className="mt-3 flex justify-center rounded-lg bg-white p-2">
            <img
              className="size-48 rounded-md object-contain"
              src={login.qrCodeImageUrl}
              alt="微信 ClawBot 登录二维码"
              referrerPolicy="no-referrer"
              onError={() => {
                setLogin((current) => current?.loginId === login.loginId
                  ? { ...current, phase: "failed", message: "二维码加载失败，请重新生成后重试。" }
                  : current);
              }}
            />
          </div>
        ) : null}
      </div>

      <label className="field notification-field-row">
        <span>iLink 用户 ID</span>
        <div className={inputShellClass}>
          <Input
            className={insetInputClass}
            value={channel.chatId}
            disabled={disabled}
            readOnly
            placeholder="完成激活后自动填入"
            spellCheck={false}
          />
        </div>
      </label>
    </>
  );
}

export const WechatClawChannelEditor = memo(WechatClawChannelEditorComponent);
