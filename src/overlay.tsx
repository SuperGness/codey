import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import appStyles from "./styles.css?inline";
import overlayStyles from "./overlay.css?inline";

type OverlayController = {
  open: () => void;
  close: () => void;
  toggle: () => void;
  isOpen: () => boolean;
};

declare global {
  interface Window {
    __codexSessionDeleteBridge?: (path: string, payload: unknown) => Promise<unknown>;
    __codeySettingsOverlay?: OverlayController;
  }
}

window.__codeyInvokeApi = async (command, args) => {
  if (typeof window.__codexSessionDeleteBridge !== "function") {
    throw new Error("Codey bridge 尚未就绪");
  }
  return window.__codexSessionDeleteBridge(`/api/${command}`, args);
};

if (!window.__codeySettingsOverlay) {
  const host = document.createElement("div");
  host.id = "codey-settings-overlay-host";
  host.style.display = "none";
  host.setAttribute("aria-hidden", "true");
  const shadow = host.attachShadow({ mode: "open" });
  const style = document.createElement("style");
  style.textContent = `${overlayStyles}\n${appStyles}`;
  const backdrop = document.createElement("div");
  backdrop.className = "codey-overlay-backdrop";
  const dialog = document.createElement("section");
  dialog.className = "codey-overlay-dialog";
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-modal", "true");
  dialog.setAttribute("aria-label", "Codey 配置");
  dialog.tabIndex = -1;
  const rootElement = document.createElement("div");
  rootElement.id = "codey-overlay-root";
  dialog.appendChild(rootElement);
  backdrop.appendChild(dialog);
  shadow.append(style, backdrop);
  document.documentElement.appendChild(host);

  const close = () => {
    host.style.display = "none";
    host.setAttribute("aria-hidden", "true");
  };
  const open = () => {
    host.style.display = "block";
    host.setAttribute("aria-hidden", "false");
    requestAnimationFrame(() => dialog.focus({ preventScroll: true }));
  };
  const isOpen = () => host.style.display !== "none";

  backdrop.addEventListener("click", (event) => {
    if (event.target === backdrop) close();
  });
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && isOpen()) {
      event.preventDefault();
      close();
    }
  }, true);

  ReactDOM.createRoot(rootElement).render(<App embedded onClose={close} />);
  window.__codeySettingsOverlay = {
    open,
    close,
    isOpen,
    toggle: () => isOpen() ? close() : open(),
  };
}
