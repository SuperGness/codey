import { useLayoutEffect, useRef, type FormEvent } from "react";
import { IconArrowUp, IconBrain, IconChevronDown, IconCpu, IconPlayerStop, IconShield, IconShieldExclamation } from "@tabler/icons-react";
import { effortLabels, permissionLabels, type ModelOption, type View } from "./workspace-data";

export function Composer({ draft, onDraft, view, models, connected, busy, uncertain, onSend, onStop, onSettings, catalogError, onReloadModels }: {
  draft: string; onDraft: (draft: string) => void; view: View | null; models: ModelOption[];
  connected: boolean; busy: boolean; uncertain: boolean; onSend: (event: FormEvent) => void;
  onStop: () => void; onSettings: (settings: Record<string, string>) => void; catalogError: string; onReloadModels: () => void;
}) {
  const input = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => { if (input.current) { input.current.style.height = "auto"; input.current.style.height = `${Math.min(input.current.scrollHeight, 180)}px`; } }, [draft]);
  const active = view?.turns.some(turn => turn.status === "inProgress");
  const selected = models.find(model => model.id === view?.model);
  const routes = [...new Set(models.map(model => model.route))];
  const disabled = busy || !connected;
  const permission = view?.permissionMode || "custom";
  return <div className="remote-composer-dock">
    {catalogError && <div className="remote-catalog-error" role="status">{catalogError}<button type="button" onClick={onReloadModels} disabled={busy}>重试</button></div>}
    <form className="remote-composer" onSubmit={onSend}>
      <textarea ref={input} aria-label="发送给 Codex 的指令" placeholder={active ? "补充当前任务的指令…" : "向 Codex 提问，或描述一个任务"} value={draft} onChange={event => onDraft(event.target.value)} rows={2} maxLength={100000} onKeyDown={event => {
        if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing && window.matchMedia("(pointer: fine)").matches) { event.preventDefault(); event.currentTarget.form?.requestSubmit(); }
      }} />
      <div className="remote-composer-toolbar">
        <div className="remote-composer-settings">
          <label className={`remote-picker remote-permission-picker${permission === "full-access" ? " is-full-access" : ""}`} title="权限设置">{permission === "full-access" ? <IconShieldExclamation size={17} /> : <IconShield size={17} />}<span>{permissionLabels[permission] || "自定义权限"}</span><IconChevronDown size={12} /><select aria-label="权限" value={permission} disabled={disabled} onChange={event => onSettings({ permissionMode: event.target.value })}>
            {permission === "custom" && <option value="custom" disabled>自定义权限（沿用桌面）</option>}<option value="read-only">只读 · 修改前询问</option><option value="auto">默认权限 · 工作区内读写</option><option value="full-access">完全访问 · 无沙箱限制</option>
          </select></label>
          <label className="remote-picker remote-model-picker" title={selected?.label || view?.model || "模型"}><IconCpu size={16} /><span>{selected?.label || view?.model || "模型"}</span><IconChevronDown size={12} /><select aria-label="模型" value={view?.model || ""} disabled={disabled || !models.length} onChange={event => {
            const next = models.find(model => model.id === event.target.value);
            if (next) onSettings({ model: next.id, ...(next.efforts.length ? { effort: next.efforts.includes(view?.effort || "") ? view!.effort! : next.defaultEffort } : {}) });
          }}>{!selected && <option value={view?.model || ""} disabled>{view?.model || "选择模型"}</option>}{routes.map(route => <optgroup key={route} label={route}>{models.filter(model => model.route === route).map(model => <option key={model.id} value={model.id}>{model.label}</option>)}</optgroup>)}</select></label>
          <label className="remote-picker remote-effort-picker" title="思考程度"><IconBrain size={16} /><span>{effortLabels[view?.effort || ""] || view?.effort || "思考"}</span><IconChevronDown size={12} /><select aria-label="思考程度" value={view?.effort || ""} disabled={disabled || !selected?.efforts.length} onChange={event => onSettings({ effort: event.target.value })}>{!selected?.efforts.includes(view?.effort || "") && <option value={view?.effort || ""} disabled>{view?.effort || "默认"}</option>}{selected?.efforts.map(effort => <option value={effort} key={effort}>{effortLabels[effort]}</option>)}</select></label>
        </div>
        <div className="remote-send-actions">{active && <button type="button" className="remote-stop" aria-label="停止任务" disabled={disabled} onClick={onStop}><IconPlayerStop size={17} /></button>}<button className="remote-send" aria-label={active ? "补充指令" : "发送消息"} disabled={disabled || !draft.trim() || uncertain}><IconArrowUp size={21} /></button></div>
      </div>
    </form>
    <p className="remote-composer-note">{busy ? "正在同步…" : active ? "任务在电脑上继续执行 · 设置用于后续轮次" : permission === "full-access" ? "完全访问：可修改电脑文件并访问网络" : "任务在电脑的原工作区中执行"}</p>
  </div>;
}
