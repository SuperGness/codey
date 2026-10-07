import { useEffect, useState } from "react";
import { IconDeviceMobile, IconRefresh, IconShieldLock } from "@tabler/icons-react";
import { invoke } from "./api";
import { Button } from "./components/ui";
import { SettingsPageHeader } from "./SettingsPageHeader";
import "./remote-control.css";

type RemoteStatus = { running: boolean; address?: string; urls: string[]; tunnel?: { status: string; message: string }; devices: { id: string; name: string }[] };
type Pairing = { url: string; qrCode: string; expiresIn: number };

export function RemoteControlPanel({ active = true }: { active?: boolean }) {
  const [status, setStatus] = useState<RemoteStatus>({ running: false, urls: [], devices: [] });
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [bindAddress, setBindAddress] = useState("0.0.0.0");
  const [port, setPort] = useState("43129");
  const [publicUrl, setPublicUrl] = useState("");
  const [connection, setConnection] = useState("tunnel");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const remote = window.__codeyRemoteClient === true;

  async function refresh() {
    const next = await invoke<RemoteStatus>("remote_control_status");
    setStatus(next);
    setUrl(current => next.urls.includes(current) ? current : next.tunnel?.status === "starting" ? "" : next.urls[0] || "");
    return next;
  }

  useEffect(() => {
    if (!active || remote) return;
    let cancelled = false;
    const poll = async () => {
      try {
        const next = await invoke<RemoteStatus>("remote_control_status");
        if (cancelled) return;
        setStatus(next);
        setUrl(current => next.urls.includes(current) ? current : next.tunnel?.status === "starting" ? "" : next.urls[0] || "");
        setPairing(current => current && next.urls.some(url => current.url.startsWith(`${url}/#pair=`)) ? current : null);
      } catch (error) { if (!cancelled) setError(String(error)); }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 5000);
    return () => { cancelled = true; window.clearInterval(timer); };
  }, [active, remote]);

  async function run(operation: () => Promise<void>) {
    setBusy(true); setError("");
    try { await operation(); } catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  }

  return <>
    <SettingsPageHeader id="remote-control-title" title="远程控制" icon={<IconDeviceMobile size={16} />}
      description="在手机浏览器中管理 Codey，并继续电脑上的 Codex 会话。" />
    {remote ? <div className="remote-card"><IconShieldLock size={20} /><p>此设备已连接。配对、撤销设备和关闭远程服务需要在电脑端操作。</p></div> : <>
      <section className="remote-card">
        <div className="remote-card-heading"><h3>手机连接</h3><span className={status.running ? "remote-online" : ""}>{status.running ? "正在运行" : "未开启"}</span></div>
        <p>电脑需保持开机且 Codey 正在运行。内置隧道支持手机在外网连接，无需配置路由器。</p>
        <div className="remote-fields">
          <label className="remote-wide">连接方式<select value={connection} onChange={e => setConnection(e.target.value)} disabled={status.running || busy}><option value="tunnel">局域网 + 内置 HTTPS 隧道</option><option value="lan">仅局域网 / VPN</option><option value="custom">局域网 + 自定义 HTTPS 地址</option></select></label>
          <label>监听地址<input value={bindAddress} onChange={e => setBindAddress(e.target.value)} disabled={status.running || busy} /></label>
          <label>端口<input inputMode="numeric" value={port} onChange={e => setPort(e.target.value)} disabled={status.running || busy} /></label>
          {connection === "custom" && <label className="remote-wide">外网 HTTPS 地址<input placeholder="https://codey.example.com" value={publicUrl} onChange={e => setPublicUrl(e.target.value)} disabled={status.running || busy} /></label>}
        </div>
        {connection === "tunnel" && <p className="remote-help">首次开启会下载并校验 Cloudflare 隧道组件。连接经过 Cloudflare，临时外网地址会随重启变化；也可使用自己的 HTTPS 入口。</p>}
        {status.tunnel && <p className={status.tunnel.status === "failed" ? "remote-error" : "remote-help"} role="status">{status.tunnel.message}</p>}
        <p className="remote-help">仅在可信网络使用局域网连接。配对后可操作工作区和控制面板，授权有效期为 12 小时；关闭服务会撤销所有设备。</p>
        <div className="remote-actions">
          {status.running ? <Button disabled={busy} variant="destructive" onClick={() => void run(async () => { await invoke("stop_remote_control"); setPairing(null); await refresh(); })}>关闭远程控制</Button>
            : <Button disabled={busy} onClick={() => void run(async () => {
              const number = Number(port);
              if (!Number.isInteger(number) || number < 1 || number > 65535) throw new Error("端口须为 1–65535 的整数");
              if (connection === "custom" && !publicUrl.trim()) throw new Error("请输入已转发到本机服务的 HTTPS 地址");
              const next = await invoke<RemoteStatus>("start_remote_control", { bindAddress, port: number, publicUrl: connection === "custom" ? publicUrl : "", tunnel: connection === "tunnel" });
              setStatus(next); setUrl(connection === "tunnel" ? "" : next.urls[0] || "");
              setPairing(null);
            })}>{busy ? "正在开启…" : "开启远程控制"}</Button>}
          <Button variant="secondary" disabled={busy} onClick={() => void run(async () => { await refresh(); })}><IconRefresh size={15} />刷新状态</Button>
        </div>
      </section>
      {status.running && <section className="remote-card">
        <h3>配对手机</h3>
        <label>连接地址<select value={url} onChange={e => { setUrl(e.target.value); setPairing(null); }}><option value="" disabled>选择连接地址…</option>{status.urls.map(value => <option value={value} key={value}>{value.startsWith("https:") ? "外网 · " : "本地 · "}{value}</option>)}</select></label>
        {status.tunnel?.status === "starting" && <p className="remote-help">隧道就绪后，选择外网地址再生成配对码。</p>}
        <div className="remote-actions"><Button variant="secondary" disabled={busy || !url} onClick={() => void run(async () => setPairing(await invoke<Pairing>("pair_remote_control", { url })))}>生成一次性配对码</Button></div>
        {pairing && <div className="remote-pairing"><img src={pairing.qrCode} alt="手机远程配对二维码" width={240} height={240} />
          <div><p>用手机浏览器扫码，填写设备名称后连接。二维码 10 分钟内有效，只能使用一次。</p><label>配对链接<input readOnly value={pairing.url} onFocus={e => e.currentTarget.select()} /></label><p className="remote-help">请勿分享配对链接。若手机无法访问，检查网络及系统防火墙是否允许此端口。</p></div>
        </div>}
      </section>}
      {status.running && <section className="remote-card"><h3>已配对设备</h3>{status.devices.length === 0 ? <p>暂无设备连接</p> : status.devices.map(device => <div className="remote-device" key={device.id}><span>{device.name}</span><Button variant="destructive" disabled={busy} onClick={() => void run(async () => { await invoke("revoke_remote_device", { id: device.id }); await refresh(); })}>撤销</Button></div>)}</section>}
    </>}
    {error && <p className="remote-error" role="alert">{error}</p>}
  </>;
}
