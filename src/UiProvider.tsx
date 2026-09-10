import { useCallback, useState, type ReactNode } from "react";
import { I18nProvider, ToastProvider } from "@heroui/react";
import { UNSAFE_PortalProvider } from "react-aria";
import { ToastContainerContext } from "./components/ui";

// 内嵌到 Codex 页面时，所有弹层（对话框、下拉、提示、Toast）都要挂到 overlay 的
// ShadowRoot 容器内，才能享受同一份样式并覆盖在设置外壳之上；独立页面则直接挂到 body。
// Toast 支持通过 ToastContainerContext 动态挂载至当前打开的弹框内部顶部。
export function UiProvider({ children, container }: {
  children: ReactNode;
  container?: HTMLElement;
}) {
  const [containerStack, setContainerStack] = useState<HTMLElement[]>([]);
  const registerContainer = useCallback((element: HTMLElement | null) => {
    if (!element) return () => {};
    setContainerStack((prev) => [...prev, element]);
    return () => {
      setContainerStack((prev) => prev.filter((el) => el !== element));
    };
  }, []);

  const activeContainer = containerStack[containerStack.length - 1] ?? container ?? null;
  const getContainer = useCallback(() => container ?? null, [container]);
  const getToastContainer = useCallback(() => activeContainer, [activeContainer]);

  const content = (
    <ToastContainerContext.Provider value={registerContainer}>
      {children}
      <UNSAFE_PortalProvider getContainer={getToastContainer}>
        <ToastProvider placement="top" width={420} maxVisibleToasts={3} />
      </UNSAFE_PortalProvider>
    </ToastContainerContext.Provider>
  );
  return (
    <I18nProvider locale="zh-CN">
      {container ? <UNSAFE_PortalProvider getContainer={getContainer}>{content}</UNSAFE_PortalProvider> : content}
    </I18nProvider>
  );
}

