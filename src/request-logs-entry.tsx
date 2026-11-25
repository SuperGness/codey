import { useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import { UiProvider } from "./UiProvider";
import utilityStyles from "./tailwind-request-logs.css?inline";
import coreStyles from "./styles.css?inline";
import requestLogStyles from "./styles.request-log.css?inline";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import { installRequestLogTheme } from "./requestLogTheme";
import {
  RequestLogDialog,
  type RequestLogCatalog,
} from "./RequestLogDialog";

const REQUEST_LOG_CATALOG_TIMEOUT_MS = 30_000;
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

installRequestLogTheme();
installBrowserBridge();
document.title = "Codey 请求日志";
const style = document.createElement("style");
style.textContent = [
  utilityStyles,
  coreStyles,
  requestLogStyles,
].join("\n");
document.head.appendChild(style);
const root = document.getElementById("root") ?? document.body.appendChild(document.createElement("div"));
ReactDOM.createRoot(root).render(
  <UiProvider>
    <RequestLogPage />
  </UiProvider>,
);
