import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type FormEvent } from "react";
import { IconArrowUp, IconBoltFilled, IconBrandGooglePodcasts, IconBrandSpeedtest, IconCamera, IconChevronRight, IconExclamationMark, IconMicrophone, IconPhoto, IconPlayerStopFilled, IconPlus, IconSelector, IconShield, IconX } from "@tabler/icons-react";
import { effortLabels, permissionLabels, type ModelOption, type View, type ThreadSettings } from "./workspace-data";
import { IMAGE_ACCEPT, readImages, imagePickerError, type DraftImage } from "./images";

export function Composer({ draft, onDraft, images, onImages, view, models, connected, busy, uncertain, onSend, onStop, onSettings, catalogError, onReloadModels }: {
  draft: string; onDraft: (draft: string) => void; view: View | null; models: ModelOption[];
  images: DraftImage[]; onImages: (images: DraftImage[]) => void;
  connected: boolean; busy: boolean; uncertain: boolean; onSend: (event: FormEvent) => void;
  onStop: () => void; onSettings: (settings: ThreadSettings) => void; catalogError: string; onReloadModels: () => void;
}) {
  const input = useRef<HTMLTextAreaElement>(null);
  const dock = useRef<HTMLDivElement>(null);
  const gauge = useRef<HTMLButtonElement>(null);
  const add = useRef<HTMLButtonElement>(null);
  const sheet = useRef<HTMLDialogElement>(null);
  const camera = useRef<HTMLInputElement>(null);
  const photos = useRef<HTMLInputElement>(null);
  const imageRead = useRef<AbortController | null>(null);
  const swipeStart = useRef<number | null>(null);
  const [panel, setPanel] = useState<"power" | "attachments" | null>(null);
  const [advanced, setAdvanced] = useState(false);
  // Native selects may match :focus-visible after a click; only Tab navigation needs a row outline.
  const [keyboardNavigation, setKeyboardNavigation] = useState(false);
  const [notice, setNotice] = useState("");
  const [imageError, setImageError] = useState("");
  const [reading, setReading] = useState(false);
  const id = useId();
  useEffect(() => {
    const capture = camera.current;
    const library = photos.current;
    const cancelCamera = () => setNotice("已取消拍照；如无法打开相机，请检查相机权限或改用照片。");
    const cancelPhotos = () => setNotice("已取消选择照片。");
    capture?.addEventListener("cancel", cancelCamera); library?.addEventListener("cancel", cancelPhotos);
    return () => { imageRead.current?.abort(); capture?.removeEventListener("cancel", cancelCamera); library?.removeEventListener("cancel", cancelPhotos); };
  }, []);
  useLayoutEffect(() => { if (input.current) { input.current.style.height = "auto"; input.current.style.height = `${Math.min(input.current.scrollHeight, 180)}px`; } }, [draft]);
  useEffect(() => {
    if (!panel) return;
    dock.current?.querySelector<HTMLButtonElement>(panel === "power" ? ".remote-power-model" : ".remote-attachment-popover button")?.focus({ preventScroll: true });
    const dismiss = (event: PointerEvent) => { if (!dock.current?.contains(event.target as Node)) setPanel(null); };
    const blur = (event: FocusEvent) => { if (!dock.current?.contains(event.target as Node)) setPanel(null); };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setPanel(null); (panel === "power" ? gauge : add).current?.focus({ preventScroll: true });
    };
    document.addEventListener("pointerdown", dismiss); document.addEventListener("keydown", escape); document.addEventListener("focusin", blur);
    return () => { document.removeEventListener("pointerdown", dismiss); document.removeEventListener("keydown", escape); document.removeEventListener("focusin", blur); };
  }, [panel]);
  useEffect(() => {
    if (advanced) sheet.current?.showModal();
    else if (sheet.current?.open) { sheet.current.close(); gauge.current?.focus({ preventScroll: true }); }
  }, [advanced]);
  const active = view?.turns.some(turn => turn.status === "inProgress");
  const selected = models.find(model => model.id === view?.model);
  const modelName = selected?.label || view?.model || "选择模型";
  const efforts = Object.keys(effortLabels).filter(effort => selected?.efforts.includes(effort));
  const routes = [...new Set(models.map(model => model.route))];
  const disabled = busy || !connected;
  const imagesLocked = busy || uncertain || reading;
  const missingImages = images.some(image => !image.url);
  const permission = view?.permissionMode || "custom";
  const tier = view?.serviceTier || "default";
  function togglePanel(next: "power" | "attachments") { setNotice(""); setPanel(current => current === next ? null : next); }
  function placeholder(name: string) { if (panel === "attachments") add.current?.focus({ preventScroll: true }); setPanel(null); setNotice(`${name}功能暂未开放`); }
  function pickImage(source: "camera" | "photos") {
    if (imagesLocked) return;
    setPanel(null); setImageError(""); add.current?.focus({ preventScroll: true });
    const picker = (source === "camera" ? camera : photos).current;
    try {
      if (!picker || typeof FileReader === "undefined") throw new Error("unavailable");
      picker.value = "";
      // Native capture also supports LAN HTTP. The OS owns permissions and
      // devices without capture can fall back to their image picker.
      if (picker.showPicker) picker.showPicker(); else picker.click();
      setNotice(source === "camera" ? "请按系统提示授权拍照；相机不可用时可改用照片。" : "可选择 PNG、JPEG、GIF、WebP，最多 4 张、合计 4 MB。");
    } catch (error) { setImageError(imagePickerError(error)); }
  }
  async function selectImages(element: HTMLInputElement) {
    const files = Array.from(element.files || []);
    element.value = "";
    if (!files.length || imagesLocked || imageRead.current) return;
    const controller = new AbortController();
    imageRead.current = controller; setReading(true); setImageError(""); setNotice("");
    try {
      const added = await readImages(files, images, controller.signal);
      if (!controller.signal.aborted) { onImages([...images, ...added]); setNotice(`已添加 ${added.length} 张图片，发送后提交到会话。`); }
    } catch (error) {
      if (!controller.signal.aborted) setImageError(error instanceof Error ? error.message : "图片读取失败，请重新选择。");
    } finally {
      if (!controller.signal.aborted) { imageRead.current = null; setReading(false); }
    }
  }
  return <div className="remote-composer-dock" ref={dock}>
    {catalogError && <div className="remote-catalog-error" role="status">{catalogError}<button type="button" onClick={onReloadModels} disabled={busy}>重试</button></div>}
    {panel === "power" && <section id={`${id}-power`} className="remote-power-popover" aria-label="模型与思考强度">
      <EffortControl modelName={modelName} efforts={efforts} effort={view?.effort || ""} disabled={disabled} onChange={effort => onSettings({ effort })} onAdvanced={() => { setPanel(null); setKeyboardNavigation(false); setAdvanced(true); }} />
    </section>}
    {panel === "attachments" && <section id={`${id}-attachments`} className="remote-attachment-popover" aria-label="添加附件">
      <button type="button" disabled={imagesLocked} onClick={() => pickImage("camera")}><span><IconCamera size={28} stroke={1.8} /></span>相机</button>
      <button type="button" disabled={imagesLocked} onClick={() => pickImage("photos")}><span><IconPhoto size={28} stroke={1.8} /></span>照片</button>
    </section>}
    <input ref={camera} hidden type="file" aria-label="拍照添加图片" accept={IMAGE_ACCEPT} capture="environment" disabled={imagesLocked} onChange={event => void selectImages(event.currentTarget)} />
    <input ref={photos} hidden type="file" aria-label="选择照片" accept={IMAGE_ACCEPT} multiple disabled={imagesLocked} onChange={event => void selectImages(event.currentTarget)} />
    {imageError && <p className="remote-client-error" role="alert">{imageError}</p>}
    {missingImages && <p className="remote-client-error" role="alert">浏览器未能保存图片内容，请移除标记的图片并重新选择后发送。</p>}
    <form className="remote-composer" onSubmit={event => { if (reading || missingImages) { event.preventDefault(); return; } setPanel(null); setNotice(""); onSend(event); }}>
      {images.length > 0 && <div className="remote-composer-images" aria-label="待发送图片">{images.map((image, index) => <figure className="remote-composer-image" key={index}>
        {image.url ? <img src={image.url} alt={image.name} /> : <span>请重新选择</span>}<figcaption title={image.name}>{image.name}</figcaption>
        <button type="button" aria-label={`移除图片 ${image.name}`} disabled={imagesLocked} onClick={() => { onImages(images.filter((_, position) => position !== index)); setImageError(""); setNotice("已移除图片。"); }}><IconX size={15} /></button>
      </figure>)}</div>}
      <textarea ref={input} aria-label="发送给 Codex 的指令" disabled={busy} placeholder={active ? "补充当前任务的指令…" : "向 Codex 提问"} value={draft} onFocus={() => setPanel(null)} onChange={event => { onDraft(event.target.value); setNotice(""); }} rows={1} maxLength={100000} onKeyDown={event => {
        if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing && window.matchMedia("(pointer: fine)").matches) { event.preventDefault(); event.currentTarget.form?.requestSubmit(); }
      }} />
      <div className="remote-composer-toolbar">
        <div className="remote-composer-tools">
          <button ref={add} type="button" className="remote-composer-icon" aria-label="添加附件" aria-expanded={panel === "attachments"} aria-controls={`${id}-attachments`} disabled={imagesLocked} onClick={() => togglePanel("attachments")}><IconPlus size={26} stroke={1.8} /></button>
          <label className={`remote-permission-picker${permission === "full-access" ? " is-full-access" : ""}`} title={permissionLabels[permission] || "自定义权限"}><IconShield size={23} stroke={1.8} />{permission === "full-access" && <IconExclamationMark className="remote-shield-mark" size={13} stroke={2.3} />}<select aria-label="权限" value={permission} disabled={disabled} onChange={event => onSettings({ permissionMode: event.target.value })}>
            {permission === "custom" && <option value="custom" disabled>自定义权限（沿用桌面）</option>}<option value="read-only">只读 · 修改前询问</option><option value="auto">默认权限 · 工作区内读写</option><option value="full-access">完全访问 · 无沙箱限制</option>
          </select></label>
        </div>
        <div className="remote-send-actions">
          {tier === "priority" && <span className="remote-fast-indicator" role="img" aria-label="快速模式已启用" title="快速模式已启用"><IconBoltFilled size={24} /></span>}
          <button ref={gauge} type="button" className="remote-composer-icon" aria-label="调整模型与思考强度" aria-expanded={panel === "power" || advanced} aria-controls={`${id}-${advanced ? "advanced" : "power"}`} onClick={() => togglePanel("power")}><IconBrandSpeedtest size={27} stroke={1.8} /></button>
          <button type="button" className="remote-composer-icon" aria-label="语音输入" onClick={() => placeholder("语音输入")}><IconMicrophone size={25} stroke={1.8} /></button>
          {active && <button type="button" className="remote-stop" aria-label="停止任务" disabled={disabled} onClick={onStop}><IconPlayerStopFilled size={17} /></button>}
          {draft.trim() || images.length > 0 ? <button className="remote-send" aria-label={active ? "补充指令" : "发送消息"} disabled={disabled || uncertain || reading || missingImages}><IconArrowUp size={22} /></button> : !active && <button type="button" className="remote-voice" aria-label="语音对话" onClick={() => placeholder("语音对话")}><IconBrandGooglePodcasts size={22} stroke={2.2} /></button>}
        </div>
      </div>
    </form>
    <p className="remote-composer-note" role="status">{reading ? "正在读取图片…" : notice || (busy ? "正在同步…" : active ? "任务在电脑上继续执行 · 设置用于后续轮次" : permission === "full-access" ? "完全访问：可修改电脑文件并访问网络" : "任务在电脑的原工作区中执行")}</p>
    <dialog ref={sheet} id={`${id}-advanced`} className="remote-advanced-sheet" aria-label="高级设置" tabIndex={-1} data-keyboard-navigation={keyboardNavigation || undefined} onPointerDownCapture={() => setKeyboardNavigation(false)} onKeyDownCapture={event => { if (event.key === "Tab") setKeyboardNavigation(true); }} onCancel={event => { event.preventDefault(); setAdvanced(false); }} onClick={event => {
      if (event.target !== event.currentTarget) return;
      const bounds = event.currentTarget.getBoundingClientRect();
      if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) setAdvanced(false);
    }}>
      <button type="button" className="remote-sheet-handle" aria-label="关闭高级设置" onClick={() => setAdvanced(false)} onPointerDown={event => { swipeStart.current = event.clientY; event.currentTarget.setPointerCapture(event.pointerId); }} onPointerUp={event => { if (swipeStart.current !== null && event.clientY - swipeStart.current > 35) setAdvanced(false); swipeStart.current = null; }} onPointerCancel={() => { swipeStart.current = null; }}><span /></button>
      <h3>高级<IconChevronRight size={19} stroke={2.3} /></h3>
      <div className="remote-settings-group">
        <label className="remote-settings-row"><span>模型</span><span className="remote-settings-value remote-model-value" title={modelName}>{modelName}</span><IconSelector size={17} /><select aria-label="模型" value={view?.model || ""} disabled={disabled || !models.length} onChange={event => {
          const next = models.find(model => model.id === event.target.value);
          if (next) onSettings({ model: next.id, effort: next.efforts.includes(view?.effort || "") ? view!.effort! : next.defaultEffort || undefined });
        }}>{!selected && <option value={view?.model || ""} disabled>{view?.model || "选择模型"}</option>}{routes.map(route => <optgroup key={route} label={route}>{models.filter(model => model.route === route).map(model => <option key={model.id} value={model.id}>{model.label}</option>)}</optgroup>)}</select></label>
        <label className="remote-settings-row"><span>智能</span><span className="remote-settings-value">{effortLabels[view?.effort || ""] || view?.effort || "默认"}</span><IconSelector size={17} /><select aria-label="思考程度" value={view?.effort || ""} disabled={disabled || !selected?.efforts.length} onChange={event => onSettings({ effort: event.target.value })}>{!selected?.efforts.includes(view?.effort || "") && <option value={view?.effort || ""} disabled>{view?.effort || "默认"}</option>}{selected?.efforts.map(effort => <option value={effort} key={effort}>{effortLabels[effort]}</option>)}</select></label>
      </div>
      <div className="remote-settings-group remote-speed-group">
        <label className="remote-settings-row"><span>速度</span><span className="remote-settings-value">{tier === "priority" ? "快速" : tier === "default" ? "标准" : tier}</span><IconSelector size={17} /><select aria-label="速度模式" value={tier} disabled={disabled} onChange={event => onSettings({ serviceTier: event.target.value })}>{!["default", "priority"].includes(tier) && <option value={tier} disabled>{tier}</option>}<option value="default">标准</option><option value="priority">快速</option></select></label>
      </div>
    </dialog>
  </div>;
}

function EffortControl({ modelName, efforts, effort, disabled, onChange, onAdvanced }: {
  modelName: string; efforts: string[]; effort: string; disabled: boolean; onChange: (effort: string) => void; onAdvanced: () => void;
}) {
  const [preview, setPreview] = useState<number | null>(null);
  const current = efforts.indexOf(effort);
  const index = preview ?? Math.max(0, current);
  const label = preview !== null ? effortLabels[efforts[index]] : effortLabels[effort] || effort || "默认";
  const unavailable = disabled || efforts.length < 2;
  useEffect(() => { setPreview(null); }, [disabled, effort, modelName]);
  function commit(value: number) {
    setPreview(null);
    if (!unavailable && efforts[value] && efforts[value] !== effort) onChange(efforts[value]);
  }
  return <>
    <button type="button" className="remote-power-model" aria-label={`高级设置：${modelName}`} aria-haspopup="dialog" onClick={onAdvanced}><span title={modelName}>{modelName}</span><span>{label}</span><IconSelector size={17} /></button>
    <div className={`remote-effort-slider${unavailable ? " is-disabled" : ""}`} style={{ "--effort-fraction": efforts.length > 1 ? index / (efforts.length - 1) : 0 } as CSSProperties}>
      <div className="remote-effort-track" aria-hidden="true"><span className="remote-effort-fill" /><div className="remote-effort-ticks">{efforts.map((value, position) => <span key={value} className={position < index ? "is-filled" : ""} />)}</div><span className="remote-effort-thumb" /></div>
      <input type="range" aria-label="思考强度" aria-valuetext={label} aria-disabled={unavailable} min={0} max={Math.max(1, efforts.length - 1)} step={1} value={index} disabled={efforts.length < 2} onChange={event => { if (!unavailable) setPreview(event.target.valueAsNumber); }} onPointerDown={event => { if (unavailable) event.preventDefault(); else event.currentTarget.setPointerCapture(event.pointerId); }} onPointerUp={event => commit(event.currentTarget.valueAsNumber)} onPointerCancel={() => setPreview(null)} onKeyDown={event => { if (unavailable && event.key !== "Tab") event.preventDefault(); }} onKeyUp={event => { if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) commit(event.currentTarget.valueAsNumber); }} onBlur={event => { if (preview !== null) commit(event.currentTarget.valueAsNumber); }} />
    </div>
    {!efforts.length && <p className="remote-effort-unavailable">当前模型未提供可调整的思考强度</p>}
  </>;
}
