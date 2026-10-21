import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Modal } from "@heroui/react";
import { UNSAFE_PortalProvider } from "react-aria";
import { useToastContainer } from "./components/ui";
import codeyAppIcon from "../backend/icons/favicon.png?inline";

type SettingsModalShellProps = {
  afterClose?: () => void;
  children: ReactNode;
  container?: HTMLElement | null;
  /** 传 null 时不渲染标题栏，由内容自行提供标题与关闭入口。 */
  header?: ReactNode | null;
  onCancel: () => void;
  title?: ReactNode;
  visible: boolean;
};

export function CodeyBrandMark() {
  return (
    <img
      className="codey-brand-mark block size-9"
      src={codeyAppIcon}
      alt=""
      width={36}
      height={36}
      aria-hidden="true"
      draggable={false}
    />
  );
}

// HeroUI 的 Modal 会等退出动画结束后再卸载对话框内容，
// 借助子节点的卸载时机通知调用方已完全关闭。
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

  // 遮罩和按钮都由调用方检查未保存的更改；Esc 不直接关闭配置。
  // 开关状态直接交给 Backdrop（无触发按钮的受控用法）。
  const modal = (
    <Modal.Backdrop
      isDismissable
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
          {header === null ? null : header !== undefined ? (
            <div className="settings-modal-header relative z-10 flex flex-none items-center py-2.5 pl-4 pr-3">
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
