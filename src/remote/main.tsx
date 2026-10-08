import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { IconArrowLeft, IconDeviceMobile, IconLogout } from "@tabler/icons-react";
import { App } from "../App";
import { UiProvider } from "../UiProvider";
import { codeyApiPath } from "../api";
import { Workspace } from "./workspace";
import { pairingCode, remoteRequest, RemoteError, clearSubmissions } from "./transport";
import "../tailwind.css";
import "../styles.css";
import "../styles.operations.css";
import "../styles.models.css";
import "../styles.features.css";
import "../styles.diagnostics.css";
import "../styles.responsive.css";
import "./styles.css";

window.__codeyRemoteClient = true;
window.__codeyInvokeApi = async (command, args) => {
  try { return await remoteRequest(codeyApiPath(command), args); }
  catch (error) {
    if (error instanceof RemoteError && error.status === 401) window.dispatchEvent(new Event("codey-remote-unauthorized"));
    throw error;
  }
};
const initialCode = pairingCode(location.hash);
// Fragments never reach the server, and are removed before any subsequent
// navigation or panel requests. Session credentials stay in HttpOnly cookies.
history.replaceState(null, "", location.pathname);

function RemoteApp() {
  const [authenticated, setAuthenticated] = useState(false);
  const [checking, setChecking] = useState(true);
  const [code, setCode] = useState(initialCode);
  const [name, setName] = useState("我的手机");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [tab, setTab] = useState<"workspace" | "panel">("workspace");

  useEffect(() => {
    const unauthorized = () => setAuthenticated(false);
    window.addEventListener("codey-remote-unauthorized", unauthorized);
    remoteRequest("/remote/session").then(() => setAuthenticated(true))
      .catch(() => undefined).finally(() => setChecking(false));
    return () => window.removeEventListener("codey-remote-unauthorized", unauthorized);
  }, []);

  useEffect(() => {
    const viewport = window.visualViewport;
    const resize = () => {
      document.documentElement.style.setProperty("--remote-viewport-height", `${viewport?.height || window.innerHeight}px`);
      document.documentElement.style.setProperty("--remote-viewport-top", `${viewport?.offsetTop || 0}px`);
    };
    resize(); viewport?.addEventListener("resize", resize); viewport?.addEventListener("scroll", resize);
    return () => { viewport?.removeEventListener("resize", resize); viewport?.removeEventListener("scroll", resize); };
  }, []);

  async function pair(event: React.FormEvent) {
    event.preventDefault(); setBusy(true); setError("");
    try { await remoteRequest("/remote/pair", { code: code.trim(), name: name.trim() }); setCode(""); setAuthenticated(true); }
    catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  }

  if (checking) return <main className="remote-login"><p role="status">正在检查连接…</p></main>;
  if (!authenticated) return <main className="remote-login">
    <form onSubmit={pair} className="remote-login-card">
      <IconDeviceMobile size={32} /><h1>连接你的 Codey</h1>
      <p>在电脑端打开 Codey → 远程控制，扫描一次性配对码。连接后可继续 Codex 工作并管理控制面板。</p>
      <label>设备名称<input autoComplete="off" maxLength={80} required value={name} onChange={event => setName(event.target.value)} /></label>
      <label>配对码<input type="password" autoComplete="off" spellCheck={false} required value={code} onChange={event => setCode(event.target.value)} placeholder="扫码后自动填写" /></label>
      <button className="remote-primary" disabled={busy || !name.trim() || !code.trim()}>{busy ? "正在连接…" : "配对并连接"}</button>
      {error && <p className="remote-client-error" role="alert">{error}</p>}
    </form>
  </main>;

  function logout() {
      void remoteRequest("/remote/logout", {}).then(() => { clearSubmissions(); setAuthenticated(false); }).catch(error => setError(String(error)));
  }
  return <div className="remote-shell">
    {tab === "panel" && <header className="remote-topbar"><button aria-label="返回 Codex" onClick={() => setTab("workspace")}><IconArrowLeft size={18} /></button><strong>Codey 控制台</strong><button aria-label="退出此设备" onClick={logout}><IconLogout size={18} /></button></header>}
    <div className="remote-codex-workspace-container" hidden={tab !== "workspace"}><Workspace active={tab === "workspace"} onUnauthorized={() => setAuthenticated(false)} onPanel={() => setTab("panel")} onLogout={logout} /></div>
    {tab === "panel" && <div className="remote-panel"><UiProvider><App /></UiProvider></div>}
    {error && <p className="remote-client-error" role="alert">{error}</p>}
  </div>;
}

createRoot(document.getElementById("root")!).render(<RemoteApp />);
