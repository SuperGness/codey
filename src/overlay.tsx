import { useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import { MantineProvider } from "@mantine/core";
import mantineStyles from "@mantine/core/styles.css?inline";
import { App } from "./App";
import coreStyles from "./styles.css?inline";
import operationsStyles from "./styles.operations.css?inline";
import modelStyles from "./styles.models.css?inline";
import featureStyles from "./styles.features.css?inline";
import diagnosticStyles from "./styles.diagnostics.css?inline";
import responsiveStyles from "./styles.responsive.css?inline";
import { codeyApiPath, invoke } from "./api";
import { SETTINGS_OVERLAY_Z_INDEX_CSS } from "./overlay.constants";
import { SETTINGS_OPENED_EVENT } from "./useRuntimeStatus";
import { codeyMantineTheme } from "./mantine";
import tailwindStyles from "./tailwind.css?inline";
import {
  RequestLogDialog,
  type RequestLogCatalog,
} from "./RequestLogDialog";

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
    void invoke<{ config: RequestLogCatalog }>("load_codey_config")
      .then((result) => setCatalog(result.config))
      .catch((nextError: unknown) => {
        setError(nextError instanceof Error ? nextError.message : String(nextError));
      });
  }, []);

  if (error) return <main className="p-6 text-sm text-red-700">{error}</main>;
  if (!catalog) return <main className="p-6 text-sm text-[#6e6e73]">正在加载请求日志…</main>;
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
  const hashToken = decodeURIComponent(window.location.hash.slice(1));
  if (hashToken) {
    window.sessionStorage.setItem(REQUEST_LOG_TOKEN_KEY, hashToken);
    window.history.replaceState(null, "", window.location.pathname);
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
    const value = await response.json();
    if (!response.ok) throw new Error(value?.error?.message || `Codey 请求失败（${response.status}）`);
    return value;
  };
}

function getOverlayMountTarget() {
  return document.body ?? document.documentElement;
}

if (window.location.pathname === REQUEST_LOG_PATH) {
  installBrowserBridge();
  document.title = "Codey 请求日志";
  document.documentElement.setAttribute("data-mantine-color-scheme", "light");
  const style = document.createElement("style");
  style.textContent = [
    mantineStyles,
    tailwindStyles,
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
    <MantineProvider forceColorScheme="light" theme={codeyMantineTheme}>
      <RequestLogPage />
    </MantineProvider>,
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
  host.setAttribute("data-mantine-color-scheme", "light");
  host.setAttribute("aria-hidden", "true");
  const shadow = host.attachShadow({ mode: "open" });
  const style = document.createElement("style");
  style.textContent = [
    mantineStyles,
    tailwindStyles,
    coreStyles,
    operationsStyles,
    modelStyles,
    featureStyles,
    diagnosticStyles,
    responsiveStyles,
  ].join("\n");
  const rootElement = document.createElement("div");
  rootElement.id = "codey-overlay-root";
  rootElement.style.inset = "0";
  rootElement.style.pointerEvents = "none";
  rootElement.style.position = "fixed";
  rootElement.style.width = "100%";
  rootElement.setAttribute("data-mantine-color-scheme", "light");
  const modalContainer = document.createElement("div");
  modalContainer.id = "codey-overlay-modal-container";
  modalContainer.style.inset = "0";
  modalContainer.style.position = "fixed";
  modalContainer.style.width = "100%";
  modalContainer.setAttribute("data-mantine-color-scheme", "light");
  shadow.append(style, rootElement, modalContainer);
  getOverlayMountTarget().appendChild(host);

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
      <MantineProvider
        cssVariablesSelector=":host"
        forceColorScheme="light"
        getRootElement={() => host}
        theme={codeyMantineTheme}
      >
        <App
          embedded
          modalContainer={modalContainer}
          modalVisible={visible}
          onAfterClose={hide}
          onClose={close}
        />
      </MantineProvider>,
    );
  };
  const close = () => {
    if (!visible) return;
    visible = false;
    render(false);
    window.clearTimeout(hideTimer);
    hideTimer = window.setTimeout(hide, 250);
  };
  const open = () => {
    if (visible) return;
    visible = true;
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    getOverlayMountTarget().appendChild(host);
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
    toggle: open,
  };
}
}
