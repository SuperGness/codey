import ReactDOM from "react-dom/client";
import "../node_modules/@douyinfe/semi-ui/lib/es/_base/base.css";
import { App } from "./App";
import appStyles from "./styles.css?inline";
import overlayStyles from "./overlay.css?inline";
import { codeyApiPath } from "./api";

const SETTINGS_OPENED_EVENT = "codey-settings-opened";
/** App listens and runs closeSettings() so unsaved edits are discarded cleanly. */
const SETTINGS_REQUEST_CLOSE_EVENT = "codey-settings-request-close";
const OVERLAY_MOTION_MS = 200;
const PAGE_LOCK_CLASS = "codey-settings-overlay-open";
const PAGE_LOCK_STYLE_ID = "codey-settings-overlay-page-lock";
const PAGE_LOCK_EVENTS = [
  "pointerdown",
  "pointerup",
  "pointermove",
  "mousedown",
  "mouseup",
  "click",
  "auxclick",
  "contextmenu",
  "touchstart",
  "touchmove",
  "touchend",
  "wheel",
  "keydown",
] as const;

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
  const componentStyles = window.__codeyComponentStyles ?? "";
  delete window.__codeyComponentStyles;
  const host = document.createElement("div");
  host.id = "codey-settings-overlay-host";
  host.setAttribute("aria-hidden", "true");
  // Full-viewport hit + paint target. Dialog inset is CSS-only.
  Object.assign(host.style, {
    position: "fixed",
    inset: "0px",
    width: "100%",
    height: "100%",
    margin: "0px",
    padding: "0px",
    border: "0px",
    zIndex: "2147483647",
    display: "none",
    pointerEvents: "none",
  });
  host.style.setProperty("-webkit-app-region", "no-drag");

  const shadow = host.attachShadow({ mode: "open" });
  const style = document.createElement("style");
  style.textContent = `${componentStyles}\n${overlayStyles}\n${appStyles}`;
  const backdrop = document.createElement("div");
  backdrop.className = "codey-overlay-backdrop";
  backdrop.setAttribute("data-codey-overlay-mask", "true");
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

  let closeTimer = 0;
  let pageLocked = false;

  const ensurePageLockStyle = () => {
    if (document.getElementById(PAGE_LOCK_STYLE_ID)) return;
    const lockStyle = document.createElement("style");
    lockStyle.id = PAGE_LOCK_STYLE_ID;
    // body is Codex UI; host is a sibling under <html>, so this freezes the page
    // without disabling the overlay itself.
    lockStyle.textContent = `
      html.${PAGE_LOCK_CLASS},
      html.${PAGE_LOCK_CLASS} body {
        pointer-events: none !important;
      }
      html.${PAGE_LOCK_CLASS} #codey-settings-overlay-host,
      html.${PAGE_LOCK_CLASS} #codey-settings-button {
        pointer-events: auto !important;
      }
    `;
    document.documentElement.appendChild(lockStyle);
  };

  const eventBelongsToOverlay = (event: Event) => {
    const path =
      typeof event.composedPath === "function" ? event.composedPath() : [];
    if (path.includes(host)) return true;
    const target = event.target;
    if (target === host) return true;
    if (target instanceof Element) {
      if (target.id === "codey-settings-button") return true;
      if (target.closest?.("#codey-settings-button")) return true;
    }
    return false;
  };

  const lockPageInteraction = (event: Event) => {
    if (
      !host.hasAttribute("data-open") &&
      !host.hasAttribute("data-closing")
    ) {
      return;
    }
    if (eventBelongsToOverlay(event)) return;
    // Esc closes settings; other keys must not reach Codex shortcuts.
    if (event.type === "keydown") {
      const keyEvent = event as KeyboardEvent;
      if (keyEvent.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        if (host.hasAttribute("data-open")) requestClose();
        return;
      }
    }
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation();
  };

  const lockPage = () => {
    if (pageLocked) return;
    pageLocked = true;
    ensurePageLockStyle();
    document.documentElement.classList.add(PAGE_LOCK_CLASS);
    try {
      document.body?.setAttribute("inert", "");
    } catch {
      // Older Chromium may lack inert; pointer-events CSS still applies.
    }
    for (const type of PAGE_LOCK_EVENTS) {
      window.addEventListener(type, lockPageInteraction, true);
    }
  };

  const unlockPage = () => {
    if (!pageLocked) return;
    pageLocked = false;
    document.documentElement.classList.remove(PAGE_LOCK_CLASS);
    try {
      document.body?.removeAttribute("inert");
    } catch {
      // ignore
    }
    for (const type of PAGE_LOCK_EVENTS) {
      window.removeEventListener(type, lockPageInteraction, true);
    }
  };

  const isOpen = () =>
    host.hasAttribute("data-open") || host.hasAttribute("data-closing");

  const finishClose = () => {
    if (closeTimer) {
      window.clearTimeout(closeTimer);
      closeTimer = 0;
    }
    host.removeAttribute("data-open");
    host.removeAttribute("data-closing");
    host.style.display = "none";
    host.style.pointerEvents = "none";
    host.setAttribute("aria-hidden", "true");
    unlockPage();
  };

  const close = () => {
    if (!isOpen()) return;
    if (host.hasAttribute("data-closing")) return;

    host.removeAttribute("data-open");
    host.setAttribute("data-closing", "");
    host.style.pointerEvents = "none";
    host.setAttribute("aria-hidden", "true");
    // Keep page locked through the exit motion so Codex cannot steal the click.
    lockPage();

    const onTransitionEnd = (event: TransitionEvent) => {
      if (event.target !== host && event.target !== dialog) return;
      host.removeEventListener("transitionend", onTransitionEnd);
      finishClose();
    };
    host.addEventListener("transitionend", onTransitionEnd);
    closeTimer = window.setTimeout(finishClose, OVERLAY_MOTION_MS + 80);
  };

  const open = () => {
    if (host.hasAttribute("data-open")) return;
    if (closeTimer) {
      window.clearTimeout(closeTimer);
      closeTimer = 0;
    }
    host.removeAttribute("data-closing");
    host.style.display = "block";
    host.style.pointerEvents = "auto";
    host.style.inset = "0px";
    host.style.width = "100%";
    host.style.height = "100%";
    host.setAttribute("aria-hidden", "false");
    lockPage();
    // Two frames so display:block applies before opacity/transform transitions.
    requestAnimationFrame(() => {
      host.setAttribute("data-open", "");
      window.dispatchEvent(new CustomEvent(SETTINGS_OPENED_EVENT));
      requestAnimationFrame(() => dialog.focus({ preventScroll: true }));
    });
  };

  const requestClose = () => {
    // Prefer App.closeSettings so dirty form state is restored first.
    window.dispatchEvent(new CustomEvent(SETTINGS_REQUEST_CLOSE_EVENT));
    // Fallback if the React tree has not attached a listener yet.
    queueMicrotask(() => {
      if (host.hasAttribute("data-open") && !host.hasAttribute("data-closing")) {
        close();
      }
    });
  };

  // Mask click closes. Clicks inside the dialog must not bubble to the mask.
  backdrop.addEventListener("click", (event) => {
    if (event.target === backdrop) {
      event.preventDefault();
      event.stopPropagation();
      requestClose();
    }
  });
  dialog.addEventListener("click", (event) => {
    event.stopPropagation();
  });

  ReactDOM.createRoot(rootElement).render(<App embedded onClose={close} />);
  window.__codeySettingsOverlay = {
    open,
    close,
    isOpen,
    toggle: () => {
      if (host.hasAttribute("data-open")) requestClose();
      else if (!host.hasAttribute("data-closing")) open();
    },
  };
}