import ReactDOM from "react-dom/client";
import "../node_modules/@douyinfe/semi-ui/lib/es/_base/base.css";
import { App } from "./App";
import coreStyles from "./styles.css?inline";
import operationsStyles from "./styles.operations.css?inline";
import modelStyles from "./styles.models.css?inline";
import featureStyles from "./styles.features.css?inline";
import diagnosticStyles from "./styles.diagnostics.css?inline";
import componentStyles from "./styles.components.css?inline";
import responsiveStyles from "./styles.responsive.css?inline";
import workflowStyles from "./styles.workflows.css?inline";
import overlayStyles from "./overlay.css?inline";
import { codeyApiPath } from "./api";
import { SETTINGS_OVERLAY_Z_INDEX_CSS } from "./overlay.constants";
import { SETTINGS_OPENED_EVENT } from "./useRuntimeStatus";

type OverlayController = {
  open: () => void;
  openWorkflow: (request: { threadId: string; runId?: string }) => void;
  close: () => void;
  toggle: () => void;
  isOpen: () => boolean;
};

type OverlayViewRequest = {
  view: "settings" | "workflows";
  revision: number;
  threadId?: string;
  runId?: string;
};

declare global {
  interface Window {
    __codexSessionDeleteBridge?: (
      path: string,
      payload: unknown,
    ) => Promise<unknown>;
    __codeyComponentStyles?: string;
    __codeySettingsOverlay?: OverlayController;
  }
}

window.__codeyInvokeApi = async (command, args) => {
  if (typeof window.__codexSessionDeleteBridge !== "function") {
    throw new Error("Codey bridge 尚未就绪");
  }
  return window.__codexSessionDeleteBridge(codeyApiPath(command), args);
};

if (!window.__codeySettingsOverlay) {
  const injectedComponentStyles = window.__codeyComponentStyles ?? "";
  delete window.__codeyComponentStyles;
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
  host.setAttribute("aria-hidden", "true");
  const shadow = host.attachShadow({ mode: "open" });
  const style = document.createElement("style");
  style.textContent = [
    injectedComponentStyles,
    overlayStyles,
    coreStyles,
    operationsStyles,
    modelStyles,
    featureStyles,
    diagnosticStyles,
    componentStyles,
    responsiveStyles,
    workflowStyles,
  ].join("\n");
  const rootElement = document.createElement("div");
  rootElement.id = "codey-overlay-root";
  const modalContainer = document.createElement("div");
  modalContainer.id = "codey-overlay-modal-container";
  shadow.append(style, rootElement, modalContainer);
  document.documentElement.appendChild(host);

  let hideTimer: number | undefined;
  let viewRequest: OverlayViewRequest = { view: "settings", revision: 0 };
  const hide = () => {
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    host.style.display = "none";
    host.setAttribute("aria-hidden", "true");
  };
  const reactRoot = ReactDOM.createRoot(rootElement);
  const render = (visible: boolean) => {
    reactRoot.render(
      <App
        embedded
        modalContainer={modalContainer}
        modalVisible={visible}
        requestedView={viewRequest.view}
        viewRequestRevision={viewRequest.revision}
        workflowThreadId={viewRequest.threadId}
        workflowRunId={viewRequest.runId}
        onAfterClose={hide}
        onClose={close}
      />,
    );
  };
  const close = () => {
    render(false);
    window.clearTimeout(hideTimer);
    hideTimer = window.setTimeout(hide, 250);
  };
  const show = () => {
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
    document.documentElement.appendChild(host);
    host.style.display = "block";
    host.setAttribute("aria-hidden", "false");
    render(true);
    window.dispatchEvent(new CustomEvent(SETTINGS_OPENED_EVENT));
  };
  const open = () => {
    viewRequest = {
      view: "settings",
      revision: viewRequest.revision + 1,
    };
    show();
  };
  const openWorkflow = (request: { threadId: string; runId?: string }) => {
    const threadId = request.threadId.trim();
    if (!threadId) return;
    const runId = request.runId?.trim() || undefined;
    viewRequest = {
      view: "workflows",
      revision: viewRequest.revision + 1,
      threadId,
      runId,
    };
    show();
  };
  const isOpen = () => host.style.display !== "none";

  render(false);
  window.__codeySettingsOverlay = {
    open,
    openWorkflow,
    close,
    isOpen,
    toggle: open,
  };
}
