import { useEffect, useId, useState } from "react";
import {
  IconAlertCircle,
  IconCheck,
  IconChevronDown,
  IconDeviceDesktop,
  IconDeviceMobile,
  IconLink,
  IconPlayerStop,
  IconQrcode,
  IconRefresh,
  IconSettings,
  IconShieldLock,
  IconWifi,
  IconWorld,
} from "@tabler/icons-react";
import { invoke } from "./api";
import { Button, Input } from "./components/ui";
import { SettingsPageHeader } from "./SettingsPageHeader";
import "./remote-control.css";

type RemoteStatus = { running: boolean; address?: string; urls: string[]; tunnel?: { status: string; message: string }; devices: { id: string; name: string }[] };
type Pairing = { url: string; qrCode: string; expiresIn: number };

const CONNECTION_MODES = [
  { value: "tunnel", title: "随处连接", description: "内置 HTTPS 隧道", icon: IconWorld },
  { value: "lan", title: "局域网 / VPN", description: "同一网络内访问", icon: IconWifi },
  { value: "custom", title: "自定义入口", description: "使用自己的 HTTPS 地址", icon: IconLink },
] as const;

export function RemoteControlPanel({ active = true }: { active?: boolean }) {
  const controlId = useId();
  const [status, setStatus] = useState<RemoteStatus>({ running: false, urls: [], devices: [] });
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [bindAddress, setBindAddress] = useState("0.0.0.0");
  const [port, setPort] = useState("43129");
  const [publicUrl, setPublicUrl] = useState("");
  const [connection, setConnection] = useState("tunnel");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState("");
  const remote = window.__codeyRemoteClient === true;

  async function refresh() {
    const next = await invoke<RemoteStatus>("remote_control_status");
    setStatus(next);
    setUrl(current => next.urls.includes(current) ? current : next.tunnel?.status === "starting" ? "" : next.urls[0] || "");
    setPairing(current => current && next.urls.some(url => current.url.startsWith(`${url}/#pair=`)) ? current : null);
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

  async function run(name: string, operation: () => Promise<void>) {
    setBusy(name); setError("");
    try { await operation(); } catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(null); }
  }

  const disabled = busy !== null;
  const tunnelStarting = status.tunnel?.status === "starting";

  return (
    <section className="remote-control-panel" aria-labelledby="remote-control-title">
      <SettingsPageHeader
        id="remote-control-title"
        title="远程控制"
        icon={<IconDeviceMobile size={16} />}
        description="用手机接续电脑上的 Codex 会话，随时管理 Codey。"
        actions={!remote && (
          <Button variant="light" size="xs" disabled={disabled} loading={busy === "refresh"}
            onClick={() => void run("refresh", async () => { await refresh(); })}>
            <IconRefresh size={14} aria-hidden="true" />刷新状态
          </Button>
        )}
      />
      {remote ? (
        <div className="remote-card remote-client-notice">
          <span className="remote-section-icon"><IconShieldLock size={22} aria-hidden="true" /></span>
          <div><h3>此设备已连接</h3><p>配对、撤销设备和关闭远程服务需要在电脑端操作。</p></div>
        </div>
      ) : (
        <div className="remote-content">
          <section className="remote-card remote-service" aria-labelledby={controlId + "-service"}>
            <div className="remote-card-heading">
              <div className="remote-heading-main">
                <span className="remote-section-icon"><IconDeviceDesktop size={21} aria-hidden="true" /></span>
                <div><h3 id={controlId + "-service"}>让电脑与手机保持连接</h3><p>电脑保持开机，Codey 持续运行，即可在手机上继续工作。</p></div>
              </div>
              <span className={`remote-status ${status.running ? "is-online" : ""}`} role="status">
                <span className="remote-status-dot" aria-hidden="true" />{status.running ? "服务已开启" : "未开启"}
              </span>
            </div>

            <fieldset className="remote-modes" disabled={status.running || disabled}>
              <legend>连接方式</legend>
              <div className="remote-mode-options">
                {CONNECTION_MODES.map(({ value, title, description, icon: Icon }) => (
                  <label key={value} className={`remote-mode ${connection === value ? "is-selected" : ""}`}>
                    <input type="radio" name={controlId + "-connection"} value={value} checked={connection === value}
                      onChange={() => setConnection(value)} />
                    <Icon size={19} className="remote-mode-icon" aria-hidden="true" />
                    <span className="remote-mode-copy"><strong>{title}</strong><small>{description}</small></span>
                    <span className="remote-mode-check" aria-hidden="true">{connection === value && <IconCheck size={12} />}</span>
                  </label>
                ))}
              </div>
            </fieldset>
            {connection === "custom" && (
              <label className="remote-field remote-custom-url">
                <span>外网 HTTPS 地址</span>
                <Input placeholder="https://codey.example.com" value={publicUrl} disabled={status.running || disabled}
                  onChange={e => setPublicUrl(e.target.value)} spellCheck={false} />
              </label>
            )}
            <p className="remote-mode-note">
              {connection === "tunnel" ? "无需配置路由器。首次开启会下载并校验隧道组件；连接经过 Cloudflare，临时地址会随重启变化。"
                : connection === "lan" ? "手机与电脑需在同一局域网或 VPN 中。请仅在可信网络下使用。"
                : "将自己的 HTTPS 入口转发到本机服务，局域网连接仍可使用。"}
            </p>

            <div className="remote-service-footer">
              <details className="remote-network-settings">
                <summary><IconSettings size={14} aria-hidden="true" />网络设置<IconChevronDown size={13} className="remote-details-chevron" aria-hidden="true" /></summary>
                <div className="remote-fields">
                  <label className="remote-field"><span>监听地址</span><Input value={bindAddress} disabled={status.running || disabled} onChange={e => setBindAddress(e.target.value)} spellCheck={false} /></label>
                  <label className="remote-field"><span>端口</span><Input inputMode="numeric" value={port} disabled={status.running || disabled} onChange={e => setPort(e.target.value)} /></label>
                </div>
                {status.running && <p>关闭远程控制后可修改连接方式和网络设置。</p>}
              </details>
              {status.running ? (
                <Button className="remote-stop-button" variant="light" size="sm" disabled={disabled} loading={busy === "stop"}
                  onClick={() => void run("stop", async () => { await invoke("stop_remote_control"); setPairing(null); await refresh(); })}>
                  <IconPlayerStop size={14} aria-hidden="true" />关闭远程控制
                </Button>
              ) : (
                <Button size="sm" disabled={disabled} loading={busy === "start"} onClick={() => void run("start", async () => {
                  const number = Number(port);
                  if (!Number.isInteger(number) || number < 1 || number > 65535) throw new Error("端口须为 1–65535 的整数");
                  if (connection === "custom" && !publicUrl.trim()) throw new Error("请输入已转发到本机服务的 HTTPS 地址");
                  const next = await invoke<RemoteStatus>("start_remote_control", { bindAddress, port: number, publicUrl: connection === "custom" ? publicUrl : "", tunnel: connection === "tunnel" });
                  setStatus(next); setUrl(connection === "tunnel" ? "" : next.urls[0] || "");
                  setPairing(null);
                })}>开启远程控制</Button>
              )}
            </div>
          </section>

          {error && <div className="remote-error" role="alert"><IconAlertCircle size={17} aria-hidden="true" /><span>{error}</span></div>}

          <div className="remote-workspace">
            <section className="remote-card remote-connect" aria-labelledby={controlId + "-pairing"}>
              <div className="remote-card-heading">
                <div><h3 id={controlId + "-pairing"}>配对手机</h3><p>扫码连接，把工作带到手机上。</p></div>
                <Button className="remote-pair-button" size="sm" disabled={disabled || !status.running || !url} loading={busy === "pair"}
                  onClick={() => void run("pair", async () => setPairing(await invoke<Pairing>("pair_remote_control", { url })))}>
                  <IconQrcode size={16} aria-hidden="true" />{pairing ? "重新生成配对码" : "生成一次性配对码"}
                </Button>
              </div>
              <div className="remote-pairing">
                <div className={`remote-qr ${pairing ? "is-ready" : ""}`}>
                  {pairing ? <img src={pairing.qrCode} alt="手机远程配对二维码" width={180} height={180} /> : (
                    <div className="remote-qr-placeholder"><IconQrcode size={46} stroke={1.25} aria-hidden="true" /><strong>等待配对</strong><span>{status.running ? "生成二维码后扫码连接" : "开启服务后即可配对"}</span></div>
                  )}
                </div>
                <ol className="remote-pairing-steps">
                  <li><span>1</span><div><strong>生成配对码</strong><p>选择手机可访问的连接地址</p></div></li>
                  <li><span>2</span><div><strong>用手机扫码</strong><p>在浏览器中打开配对页面</p></div></li>
                  <li><span>3</span><div><strong>开始工作</strong><p>填写设备名称，接续会话</p></div></li>
                </ol>
              </div>
              {status.running && (
                <label className="remote-field">
                  <span>连接地址</span>
                  <span className="remote-select">
                    <select value={url} disabled={disabled || status.urls.length === 0} onChange={e => { setUrl(e.target.value); setPairing(null); }}>
                      <option value="" disabled>{tunnelStarting ? "正在建立外网连接…" : "选择连接地址…"}</option>
                      {status.urls.map(value => <option value={value} key={value}>{value.startsWith("https:") ? "外网 · " : "本地 · "}{value}</option>)}
                    </select>
                    <IconChevronDown size={14} aria-hidden="true" />
                  </span>
                </label>
              )}
              {status.running && status.tunnel && (
                <p className={`remote-tunnel-note ${status.tunnel.status === "failed" ? "is-failed" : ""}`} role="status">
                  {status.tunnel.status === "failed" ? <IconAlertCircle size={14} aria-hidden="true" /> : <IconWorld size={14} aria-hidden="true" />}
                  <span>{status.tunnel.message}{tunnelStarting && "，就绪后选择外网地址生成配对码。"}</span>
                </p>
              )}
              {pairing ? (
                <div className="remote-pairing-link">
                  <label className="remote-field"><span>配对链接</span><Input readOnly value={pairing.url} onFocus={e => e.currentTarget.select()} /></label>
                  <p>二维码 10 分钟内有效，仅可使用一次。请勿分享配对链接。</p>
                </div>
              ) : <p className="remote-pair-caption">二维码 10 分钟内有效，仅可使用一次。</p>}
            </section>

            <div className="remote-sidebar">
              <section className="remote-card remote-devices" aria-labelledby={controlId + "-devices"}>
                <div className="remote-card-heading"><h3 id={controlId + "-devices"}>已配对设备</h3><span className="remote-device-count">{status.devices.length}</span></div>
                {status.devices.length === 0 ? (
                  <div className="remote-empty"><span className="remote-empty-icon"><IconDeviceMobile size={26} stroke={1.5} aria-hidden="true" /></span><strong>还没有配对设备</strong><p>手机完成配对后<br />会显示在这里</p></div>
                ) : (
                  <ul className="remote-device-list">{status.devices.map(device => (
                    <li className="remote-device" key={device.id}>
                      <IconDeviceMobile size={19} aria-hidden="true" />
                      <div><strong>{device.name}</strong><span>已授权访问</span></div>
                      <Button variant="link" color="danger" size="xs" disabled={disabled} aria-label={`撤销 ${device.name} 的授权`}
                        onClick={() => void run("revoke", async () => { await invoke("revoke_remote_device", { id: device.id }); await refresh(); })}>撤销</Button>
                    </li>
                  ))}</ul>
                )}
              </section>
              <aside className="remote-security">
                <div><IconShieldLock size={16} aria-hidden="true" /><strong>连接与安全</strong></div>
                <p>配对后可操作工作区和控制面板，授权有效期为 12 小时。关闭服务会撤销所有设备。</p>
                <p>仅在可信网络使用局域网连接。若手机无法访问，请检查网络和系统防火墙。</p>
              </aside>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
