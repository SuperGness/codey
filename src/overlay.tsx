import ReactDOM from "react-dom/client";
// react-aria 用同一份模块实例读取该开关，必须从 react-stately 内部路径导入才能生效。
import { enableShadowDOM } from "react-stately/private/flags/flags";
import { UiProvider } from "./UiProvider";
import utilityStyles from "./tailwind.css?inline";
import { shadowStyleSheet } from "./shadowStyles";
import { App } from "./App";
import coreStyles from "./styles.css?inline";
import operationsStyles from "./styles.operations.css?inline";
import modelStyles from "./styles.models.css?inline";
import featureStyles from "./styles.features.css?inline";
import diagnosticStyles from "./styles.diagnostics.css?inline";
import usageAnalysisStyles from "./styles.usage-analysis.css?inline";
import responsiveStyles from "./styles.responsive.css?inline";
import { codeyApiPath } from "./api";
import { SETTINGS_OVERLAY_Z_INDEX_CSS } from "./overlay.constants";
import { SETTINGS_OPENED_EVENT } from "./useRuntimeStatus";
import { installOverlayTheme } from "./overlayTheme";

type OverlayController = {
  open: () => void;
  close: () => void;
  toggle: () => void;
  isOpen: () => boolean;
};

declare global {
  interface Window {
    __codexSessionDeleteBridge?: (
      path: string,
      payload: unknown,
    ) => Promise<unknown>;
    __codeySettingsOverlay?: OverlayController;
  }
}

function getOverlayMountTarget() {
  return document.body ?? document.documentElement;
}

window.__codeyInvokeApi = async (command, args) => {
  if (typeof window.__codexSessionDeleteBridge !== "function") {
    throw new Error("Codey bridge 尚未就绪");
  }
  return window.__codexSessionDeleteBridge(codeyApiPath(command), args);
};

if (!window.__codeySettingsOverlay) {
  const host = document.createElement("div");
  host.id = "codey-settings-overlay-host";
  host.style.display = "none";
  host.style.setProperty("inset", "0", "important");
  host.style.setProperty("position", "fixed", "important");
  host.style.setProperty(
    "--codey-settings-overlay-z-index",
    SETTINGS_OVERLAY_Z_INDEX_CSS,
  );
  host.style.setProperty(
    "z-index",
    SETTINGS_OVERLAY_Z_INDEX_CSS,
    "important",
  );
  host.style.setProperty("background", "transparent", "important");
  host.setAttribute("aria-hidden", "true");
  const shadow = host.attachShadow({ mode: "open" });
  shadow.adoptedStyleSheets = [
    shadowStyleSheet(
      utilityStyles,
      coreStyles,
      operationsStyles,
      modelStyles,
      featureStyles,
      diagnosticStyles,
      usageAnalysisStyles,
      responsiveStyles,
    ),
  ];
  // HeroUI 的主题变量声明在 :root / [data-theme] 上，ShadowRoot 内没有 :root，
  // 因此在两个挂载容器上显式声明主题；react-aria 也需要开启 Shadow DOM 感知。
  enableShadowDOM();
  const rootElement = document.createElement("div");
  rootElement.id = "codey-overlay-root";
  rootElement.style.inset = "0";
  rootElement.style.pointerEvents = "none";
  rootElement.style.position = "fixed";
  rootElement.style.width = "100%";
  const modalContainer = document.createElement("div");
  modalContainer.id = "codey-overlay-modal-container";
  modalContainer.style.inset = "0";
  modalContainer.style.position = "fixed";
  modalContainer.style.width = "100%";
  shadow.append(rootElement, modalContainer);
  const theme = installOverlayTheme([rootElement, modalContainer]);

  let hideTimer: number | undefined;
  let visible = false;
  let mounted = false;

  // 关闭或预加载期间让宿主脱离文档，避免 ShadowRoot 的大样式表常驻主文档的全局样式失效集。
  const hide = () => {
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    if (visible) return;
    host.style.display = "none";
    host.setAttribute("aria-hidden", "true");
    host.remove();
  };
  const reactRoot = ReactDOM.createRoot(rootElement);
  const render = (nextVisible: boolean) => {
    mounted = true;
    reactRoot.render(
      <UiProvider container={modalContainer}>
        <App
          embedded
          modalContainer={modalContainer}
          modalVisible={nextVisible}
          onAfterClose={hide}
          onClose={close}
        />
      </UiProvider>,
    );
  };
  const close = () => {
    if (!visible) return;
    visible = false;
    render(false);
    window.clearTimeout(hideTimer);
    hideTimer = window.setTimeout(hide, 450);
  };
  const open = () => {
    if (visible) return;
    const alreadyMounted = mounted;
    visible = true;
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    getOverlayMountTarget().appendChild(host);
    theme.sync();
    host.style.display = "block";
    host.setAttribute("aria-hidden", "false");
    render(true);
    if (alreadyMounted) {
      window.dispatchEvent(new CustomEvent(SETTINGS_OPENED_EVENT));
    }
  };
  const isOpen = () => visible;

  // 预加载阶段仅准备 ShadowRoot 与样式表，首次打开时再挂载 React 树，
  // 避免未打开设置页就触发 runtime_status 探针和配置/更新请求。
  window.__codeySettingsOverlay = {
    open,
    close,
    isOpen,
    toggle: () => (visible ? close() : open()),
  };
}
