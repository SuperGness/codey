import { useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
// react-aria 用同一份模块实例读取该开关，必须从 react-stately 内部路径导入才能生效。
import { enableShadowDOM } from "react-stately/private/flags/flags";
import { UiProvider } from "./UiProvider";
import utilityStyles from "./tailwind.css?inline";
import { shadowStyleSheet } from "./shadowStyles";
import { App } from "./App";
import { errorText } from "./appUtils";
import coreStyles from "./styles.css?inline";
import operationsStyles from "./styles.operations.css?inline";
import modelStyles from "./styles.models.css?inline";
import featureStyles from "./styles.features.css?inline";
import diagnosticStyles from "./styles.diagnostics.css?inline";
import responsiveStyles from "./styles.responsive.css?inline";
import { codeyApiPath, invoke } from "./api";
import { SETTINGS_OVERLAY_Z_INDEX_CSS } from "./overlay.constants";
import { SETTINGS_OPENED_EVENT } from "./useRuntimeStatus";
import { installOverlayTheme } from "./overlayTheme";
import { installRequestLogTheme } from "./requestLogTheme";
import {
  RequestLogDialog,
  type RequestLogCatalog,
} from "./RequestLogDialog";

const REQUEST_LOG_CATALOG_TIMEOUT_MS = 30_000;

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

const REQUEST_LOG_PATH = "/codey/request-logs";
const REQUEST_LOG_TOKEN_KEY = "codey-request-log-token";

function RequestLogPage() {
  const [catalog, setCatalog] = useState<RequestLogCatalog | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;
    let settled = false;
    const timer = window.setTimeout(() => {
      if (!cancelled && !settled) setError("加载请求日志超时，请刷新页面");
    }, REQUEST_LOG_CATALOG_TIMEOUT_MS);
    void invoke<{ config: RequestLogCatalog }>("load_codey_config")
      .then((result) => {
        settled = true;
        if (cancelled) return;
        setError("");
        setCatalog(result.config);
      })
      .catch((nextError: unknown) => {
        settled = true;
        if (cancelled) return;
        setError(errorText(nextError));
      })
      .finally(() => window.clearTimeout(timer));
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, []);

  if (error) return <main className="p-6 text-sm text-[var(--codey-red,#b91c1c)]">{error}</main>;
  if (!catalog) return <main className="p-6 text-sm text-[var(--codey-muted,#6e6e73)]">正在加载请求日志…</main>;
  return (
    <RequestLogDialog
      catalog={catalog}
      container={null}
      opened
      onClose={() => undefined}
      standalone
    />
  );
}

function installBrowserBridge() {
  let hashToken = "";
  try {
    hashToken = decodeURIComponent(window.location.hash.slice(1));
  } catch {
    // Ignore malformed external links and fall back to the last session token.
  }
  if (hashToken) {
    window.sessionStorage.setItem(REQUEST_LOG_TOKEN_KEY, hashToken);
  }
  if (window.location.hash) {
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
  }
  const token = hashToken || window.sessionStorage.getItem(REQUEST_LOG_TOKEN_KEY) || "";
  window.__codeyInvokeApi = async (command, args) => {
    const response = await fetch(`/codey/api/${command}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-codey-router-token": token,
      },
      body: JSON.stringify(args),
    });
    let value: { error?: { message?: string }; message?: string };
    try {
      value = await response.json();
    } catch {
      throw new Error(
        response.ok
          ? "Codey 返回了无法解析的响应"
          : `Codey 请求失败（${response.status}）`,
      );
    }
    if (!response.ok) {
      throw new Error(value?.error?.message || value?.message || `Codey 请求失败（${response.status}）`);
    }
    return value;
  };
}

function getOverlayMountTarget() {
  return document.body ?? document.documentElement;
}

if (window.location.pathname === REQUEST_LOG_PATH) {
  installRequestLogTheme();
  installBrowserBridge();
  document.title = "Codey 请求日志";
  const style = document.createElement("style");
  style.textContent = [
    utilityStyles,
    coreStyles,
    operationsStyles,
    modelStyles,
    featureStyles,
    diagnosticStyles,
    responsiveStyles,
  ].join("\n");
  document.head.appendChild(style);
  const root = document.getElementById("root") ?? document.body.appendChild(document.createElement("div"));
  ReactDOM.createRoot(root).render(
    <UiProvider>
      <RequestLogPage />
    </UiProvider>,
  );
} else {
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
  getOverlayMountTarget().appendChild(host);
  const theme = installOverlayTheme([rootElement, modalContainer]);

  let hideTimer: number | undefined;
  let visible = false;

  const hide = () => {
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    host.style.display = "none";
    host.setAttribute("aria-hidden", "true");
  };
  const reactRoot = ReactDOM.createRoot(rootElement);
  const render = (visible: boolean) => {
    reactRoot.render(
      <UiProvider container={modalContainer}>
        <App
          embedded
          modalContainer={modalContainer}
          modalVisible={visible}
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
    visible = true;
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    getOverlayMountTarget().appendChild(host);
    theme.sync();
    host.style.display = "block";
    host.setAttribute("aria-hidden", "false");
    render(true);
    window.dispatchEvent(new CustomEvent(SETTINGS_OPENED_EVENT));
  };
  const isOpen = () => visible;

  render(false);
  window.__codeySettingsOverlay = {
    open,
    close,
    isOpen,
    toggle: () => (visible ? close() : open()),
  };
}
}
