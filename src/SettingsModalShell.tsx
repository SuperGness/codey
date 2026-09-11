import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Modal } from "@heroui/react";
import { UNSAFE_PortalProvider } from "react-aria";
import { useToastContainer } from "./components/ui";

type SettingsModalShellProps = {
  afterClose?: () => void;
  children: ReactNode;
  container?: HTMLElement | null;
  header?: ReactNode;
  onCancel: () => void;
  title?: ReactNode;
  visible: boolean;
};

export function CodeyBrandMark() {
  return (
    <svg
      className="block size-[38px] rounded-[10px] text-[#007aff] shadow-[0_1px_2px_rgba(0,122,255,0.12),0_4px_12px_rgba(0,122,255,0.14)] max-[760px]:size-8"
      viewBox="0 0 350 350"
      aria-hidden="true"
      focusable="false"
    >
      <defs>
        <linearGradient
          id="codey-brand-mark-gradient"
          x1="0"
          x2="1"
          y1="0"
          y2="1"
        >
          <stop offset="0%" stopColor="#ffffff" />
          <stop offset="100%" stopColor="#e3efff" />
        </linearGradient>
      </defs>
      <rect
        x="0"
        y="0"
        width="350"
        height="350"
        rx="34"
        fill="url(#codey-brand-mark-gradient)"
      />
      <path
        d="M70 301c-16 0-24-18-13-30l73-77c8-8 8-20 0-28L65 101C50 86 57 61 78 57c9-2 18 1 25 8l91 91c18 18 18 46 0 64l-66 66c-6 6-2 15 7 15h183"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="22"
      />
    </svg>
  );
}

// HeroUI 的 Modal 会等退出动画结束后再卸载对话框内容，
// 借助子节点的卸载时机通知调用方“已完全关闭”。
function AfterClose({ onUnmount }: { onUnmount?: () => void }) {
  useEffect(() => () => onUnmount?.(), [onUnmount]);
  return null;
}

export function SettingsModalShell({
  afterClose,
  children,
  container,
  header,
  onCancel,
  title,
  visible,
}: SettingsModalShellProps) {
  const [toastHostEl, setToastHostEl] = useState<HTMLDivElement | null>(null);
  useToastContainer(toastHostEl, visible);
  // PortalProvider 以 getContainer 的引用作为上下文值；每次渲染新建闭包会让
  // 所有弹层 / 提示 / 组合框在每次 App 重渲染时一起重渲染。
  const getContainer = useCallback(() => container ?? null, [container]);

  // 外壳不响应遮罩点击与 Esc；关闭只能通过头部按钮，避免误触丢失未保存的更改。
  // 开关状态直接交给 Backdrop（无触发按钮的受控用法）。
  const modal = (
    <Modal.Backdrop
      isDismissable={false}
      isKeyboardDismissDisabled
      isOpen={visible}
      onOpenChange={(open) => {
        if (!open) onCancel();
      }}
      className="p-0"
    >
        <Modal.Container placement="center" className="p-3 sm:p-3">
          <Modal.Dialog
            className="settings-modal-shell relative flex h-[min(860px,calc(100dvh-24px))] w-[min(1040px,calc(100vw-24px))] max-w-[calc(100vw-24px)] flex-col overflow-hidden rounded-[14px] p-0 text-sm"
            aria-label="Codey 配置"
          >
            <AfterClose onUnmount={afterClose} />
            {header !== undefined ? (
              <div className="settings-modal-header relative z-10 flex flex-none items-center px-5 py-3">
                {header}
              </div>
            ) : (
              <>
                <Modal.Header className="settings-modal-header relative z-10 flex-none px-5 py-3">
                  <Modal.Heading className="text-base font-semibold text-foreground">{title}</Modal.Heading>
                </Modal.Header>
                <Modal.CloseTrigger aria-label="关闭配置" className="end-4 top-3" />
              </>
            )}
            <div className="settings-modal-body flex min-h-0 flex-1 flex-col overflow-hidden relative">
              <div
                ref={setToastHostEl}
                className="toast-portal-host pointer-events-none absolute inset-x-0 top-0 z-[100] h-0"
                aria-hidden="true"
              />
              {children}
            </div>
          </Modal.Dialog>
        </Modal.Container>
    </Modal.Backdrop>
  );
  return container ? (
    <UNSAFE_PortalProvider getContainer={getContainer}>{modal}</UNSAFE_PortalProvider>
  ) : (
    modal
  );
}
