import type { ReactNode } from "react";
import { Modal } from "antd";


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

export function SettingsModalShell({
  afterClose,
  children,
  container,
  header,
  onCancel,
  title,
  visible,
}: SettingsModalShellProps) {
  return (
    <Modal
      open={visible}
      onCancel={onCancel}
      afterClose={afterClose}
      mask={{ closable: false }}
      keyboard={false}
      footer={null}
      closable={header === undefined}
      title={header ?? title}
      getContainer={container ?? undefined}
      width={1040}
      centered
      styles={{
        container: {
          height: "min(860px, calc(100dvh - 24px))",
          display: "flex",
          flexDirection: "column",
          padding: 0,
          overflow: "hidden",
          borderRadius: 14,
        },
        header: {
          padding: "12px 20px",
          marginBottom: 0,
          borderTopLeftRadius: 14,
          borderTopRightRadius: 14,
        },
        body: {
          display: "flex",
          flex: 1,
          minHeight: 0,
          flexDirection: "column",
          overflow: "hidden",
          borderBottomLeftRadius: 14,
          borderBottomRightRadius: 14,
        },
      }}
      className="settings-modal-shell"
    >
      {children}
    </Modal>
  );
}
