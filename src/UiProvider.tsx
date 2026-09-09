import type { ReactNode } from "react";
import { StyleProvider } from "@ant-design/cssinjs";
import { ConfigProvider } from "antd";
import zhCN from "antd/locale/zh_CN";

export function UiProvider({ children, container, styleContainer }: {
  children: ReactNode;
  container?: HTMLElement;
  styleContainer?: ShadowRoot;
}) {
  return (
    <StyleProvider container={styleContainer} layer>
      <ConfigProvider locale={zhCN} button={{ autoInsertSpace: false }} getPopupContainer={container ? () => container : undefined}>
        {children}
      </ConfigProvider>
    </StyleProvider>
  );
}
