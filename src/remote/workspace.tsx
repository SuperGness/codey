import { useEffect, useRef, useState } from "react";
import { IconArrowLeft, IconPlayerStop, IconPlus, IconRefresh, IconSend } from "@tabler/icons-react";
import { RemoteError, remoteRequest, requestId, readSubmission, rememberSubmission, forgetSubmission } from "./transport";

type Thread = { id: string; title: string; cwd: string; updatedAt: number };
type Project = { id: string; name: string; cwd: string };
type Question = { id: string; question: string; options?: { label: string; description?: string }[] };
type Approval = { id: string | number; method: string; supported: boolean; params: { command?: string; reason?: string; questions?: Question[]; availableDecisions?: string[]; message?: string; permissions?: unknown; changes?: unknown; planContent?: string } };
type Turn = { id: string; status: string; error?: unknown; messages: { id?: string; role: string; kind: string; text: string }[] };
type View = { id: string; title?: string; cwd?: string; model?: string; effort?: string; status?: string; turns: Turn[]; requests: Approval[]; historyComplete: boolean };

export function Workspace({ active, onUnauthorized }: { active: boolean; onUnauthorized: () => void }) {
  const [threads, setThreads] = useState<Thread[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  const [selected, setSelected] = useState<Thread | null>(null);
  const [view, setView] = useState<View | null>(null);
  const [connected, setConnected] = useState(false);
  const [connectionError, setConnectionError] = useState("");
  const [revision, setRevision] = useState(0);
  const [search, setSearch] = useState("");
  const [archived, setArchived] = useState(false);
  const [project, setProject] = useState("");
  const [title, setTitle] = useState("");
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("medium");
  const [showSettings, setShowSettings] = useState(false);
  const [uncertain, setUncertain] = useState(false);
  const scroll = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const drafts = useRef(new Map<string, { text: string; uncertain: boolean }>());
  const selectedId = selected?.id;
  const activeTurn = view ? [...view.turns].reverse().find(turn => turn.status === "inProgress") : undefined;

  function fail(error: unknown) {
    if (error instanceof RemoteError && error.status === 401) onUnauthorized();
    setError(error instanceof Error ? error.message : String(error));
  }

  async function refresh() {
    const [threads, projects] = await Promise.all([
      remoteRequest<Thread[]>("/remote/threads", { search, archived }),
      remoteRequest<Project[]>("/remote/projects", {}),
    ]);
    setThreads(threads); setProjects(projects);
    setProject(current => projects.some(project => project.id === current) ? current : projects[0]?.id || "");
  }

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    void Promise.all([remoteRequest<Thread[]>("/remote/threads", { search, archived }), remoteRequest<Project[]>("/remote/projects", {})])
      .then(([threads, projects]) => { if (cancelled) return; setThreads(threads); setProjects(projects); setProject(current => current || projects[0]?.id || ""); })
      .catch(error => { if (!cancelled) fail(error); });
    return () => { cancelled = true; };
  }, [active, archived]);

  useEffect(() => {
    if (!active || !selectedId) return;
    setConnected(false); setConnectionError("");
    let disposed = false;
    let timer: number | undefined;
    let socket: WebSocket;
    let delay = 2000;
    const connect = () => {
      if (disposed) return;
      socket = new WebSocket(`${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/remote/events/${encodeURIComponent(selectedId)}`);
      socket.onmessage = event => {
        if (disposed) return;
        try {
          const message = JSON.parse(event.data);
          if (message.type === "disconnected") { setConnected(false); setConnectionError(message.message); return; }
          const next = message.state as View;
          if (message.type !== "state" || next.id !== selectedId) return;
          setView(next); setConnected(true); setConnectionError(""); delay = 2000;
          setModel(current => current || next.model || "");
        } catch { setConnectionError("会话数据无效，请重新连接"); }
      };
      socket.onclose = () => {
        if (disposed) return;
        setConnected(false); setConnectionError(current => current || "连接已断开，正在重新连接…");
        void remoteRequest("/remote/session").catch(error => {
          if (!disposed && error instanceof RemoteError && error.status === 401) { disposed = true; window.clearTimeout(timer); onUnauthorized(); }
        });
        timer = window.setTimeout(connect, delay); delay = Math.min(delay * 2, 30000);
      };
    };
    connect();
    return () => { disposed = true; window.clearTimeout(timer); socket.close(); };
  }, [active, selectedId, revision]);

  useEffect(() => { if (pinned.current && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; }, [view]);
  useEffect(() => { if (selectedId) drafts.current.set(selectedId, { text: draft, uncertain }); }, [selectedId, draft, uncertain]);

  async function action(action: string, args: Record<string, unknown> = {}) {
    if (!selectedId) return;
    return remoteRequest("/remote/action", { ...args, threadId: selectedId, action, requestId: requestId() });
  }

  async function run(operation: () => Promise<unknown>) {
    setBusy(true); setError("");
    try { await operation(); } catch (error) { fail(error); }
    finally { setBusy(false); }
  }

  async function open(thread: Thread) {
    const previous = drafts.current.get(thread.id);
    const pending = readSubmission(thread.id);
    setSelected(thread); setView(null); setConnected(false); setModel(""); setDraft(previous?.text || String(pending?.args.text || "")); setError(""); setUncertain(previous?.uncertain || !!pending); setShowSettings(false); pinned.current = true;
    await remoteRequest("/remote/action", { threadId: thread.id, action: "open", requestId: requestId() });
    setRevision(n => n + 1);
  }

  async function send(event: React.FormEvent) {
    event.preventDefault();
    if (!selectedId || busy || uncertain || !connected || !draft.trim()) return;
    const mode = activeTurn ? "steer" : "send";
    const submission = rememberSubmission(selectedId, { action: mode, text: draft });
    await run(async () => {
      try {
        await remoteRequest("/remote/action", { threadId: selectedId, ...submission.args, requestId: submission.id });
        setDraft(""); forgetSubmission(selectedId); setUncertain(false); pinned.current = true;
      } catch (error) { setUncertain(true); throw error; }
    });
  }

  return <div className={`remote-workspace${selected ? " has-selection" : ""}`}>
    <aside className="remote-thread-list">
      <div className="remote-list-heading"><h1>工作区会话</h1><button aria-label="刷新会话" onClick={() => void run(refresh)} disabled={busy}><IconRefresh size={18} /></button><button aria-label="新建会话" onClick={() => setCreating(!creating)}><IconPlus size={19} /></button></div>
      <form className="remote-search" onSubmit={event => { event.preventDefault(); void run(refresh); }}><input placeholder="搜索会话或项目" aria-label="搜索会话" value={search} onChange={event => setSearch(event.target.value)} maxLength={200} /><button>搜索</button></form>
      <label className="remote-checkbox"><input type="checkbox" checked={archived} onChange={event => setArchived(event.target.checked)} />查看归档会话</label>
      {creating && <form className="remote-create" onSubmit={event => { event.preventDefault(); void run(async () => {
        const submission = rememberSubmission("create", { projectId: project, title });
        const result = await remoteRequest<{ id: string }>("/remote/create", { ...submission.args, requestId: submission.id });
        forgetSubmission("create"); setCreating(false); setTitle(""); await refresh();
        await open({ id: result.id, title, cwd: projects.find(p => p.id === project)?.cwd || "", updatedAt: Date.now() });
      }); }}>
        <label>项目<select value={project} onChange={event => setProject(event.target.value)}>{projects.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
        <label>会话名称<input required maxLength={120} value={title} onChange={event => setTitle(event.target.value)} /></label>
        <button className="remote-primary" disabled={busy || !project || !title.trim()}>创建会话</button>
        {!projects.length && <p>请先在电脑 Codex 中保存本地项目。</p>}
      </form>}
      <div className="remote-threads">{threads.length === 0 ? <p className="remote-empty">暂无会话，可在已保存的项目中新建。</p> : threads.map(thread => <button key={thread.id} className="remote-thread" aria-current={selectedId === thread.id ? "true" : undefined} onClick={() => void run(() => open(thread))} disabled={busy}>
        <strong>{thread.title}</strong><span>{thread.cwd}</span><time>{new Date(thread.updatedAt).toLocaleString()}</time>
      </button>)}</div>
      {!selected && error && <p role="alert" className="remote-error">{error}</p>}
    </aside>
    <main className="remote-chat">{!selected ? <div className="remote-empty"><h2>从手机继续工作</h2><p>选择会话查看实时进展、发送指令或处理审批。任务在电脑的原工作区中执行。</p></div> : <>
      <header className="remote-chat-header"><button className="remote-back" aria-label="返回会话列表" disabled={busy} onClick={() => setSelected(null)}><IconArrowLeft size={20} /></button><div><h2>{view?.title || selected.title}</h2><p>{connected ? activeTurn ? "正在执行" : "已连接" : "未连接"} · {view?.model || selected.cwd}</p></div><button disabled={busy} aria-label="重新连接" onClick={() => void run(async () => { await action("open"); setRevision(n => n + 1); })}><IconRefresh size={18} /></button><button onClick={() => { setModel(view?.model || ""); setEffort(view?.effort || "medium"); setShowSettings(!showSettings); }}>模型</button></header>
      {showSettings && <form className="remote-model-form" onSubmit={event => { event.preventDefault(); void run(async () => { await action("settings", { model, effort }); setShowSettings(false); }); }}><label>模型<input required value={model} onChange={event => setModel(event.target.value)} /></label><label>思考强度<select value={effort} onChange={event => setEffort(event.target.value)}>{["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"].map(value => <option key={value}>{value}</option>)}</select></label><button disabled={busy || !connected}>应用</button></form>}
      {connectionError && <p className="remote-connection" role="status">{connectionError}</p>}
      <div className="remote-messages" ref={scroll} onScroll={event => { const element = event.currentTarget; pinned.current = element.scrollHeight - element.scrollTop - element.clientHeight < 100; }}>
        {view && !view.historyComplete && <button disabled={busy || !connected} onClick={() => void run(async () => { await action("history"); setRevision(n => n + 1); })}>加载完整历史</button>}
        {!view && <p className="remote-empty">正在获取桌面会话…</p>}
        {view?.turns.map((turn, index) => <section className="remote-turn" key={turn.id || index}>{turn.messages.map((message, index) => message.role === "activity" ? <details className="remote-activity" key={message.id || index}><summary>{message.kind === "commandExecution" ? "执行命令" : message.kind === "fileChange" ? "文件变更" : message.kind === "reasoning" ? "思考摘要" : "工具活动"}</summary><pre>{message.text}</pre></details> : <article className={`remote-message ${message.role}`} key={message.id || index}><small>{message.role === "user" ? "你" : message.role === "error" ? "错误" : "Codex"}</small><div>{message.text}</div></article>)}{turn.status === "failed" && <p className="remote-error">任务失败：{typeof turn.error === "string" ? turn.error : JSON.stringify(turn.error || "请查看电脑端详情")}</p>}</section>)}
        {view?.requests.map(request => <ApprovalCard key={String(request.id)} request={request} disabled={busy || !connected} onRespond={args => run(() => action("respond", { approvalId: request.id, ...args }))} />)}
      </div>
      {error && <p className="remote-error" role="alert">{error}</p>}
      {uncertain && <div className="remote-uncertain"><p>消息未确认。请查看会话是否已收到，确认后再决定是否重新发送。</p><button onClick={() => { if (selectedId) forgetSubmission(selectedId); setUncertain(false); setError(""); }}>已核对，允许再次发送</button></div>}
      <form className="remote-composer" onSubmit={send}><textarea aria-label="发送给 Codex 的指令" placeholder={activeTurn ? "补充当前任务的指令…" : "让 Codex 在工作区中做什么？"} value={draft} onChange={event => setDraft(event.target.value)} rows={3} maxLength={100000} /><div><span>{activeTurn ? "指令将补充到当前任务" : "沿用此会话的模型与权限"}</span>{activeTurn && <button type="button" className="remote-stop" disabled={busy || !connected} onClick={() => void run(() => action("interrupt", { turnId: activeTurn.id }))}><IconPlayerStop size={16} />停止</button>}<button className="remote-primary" disabled={busy || !connected || !draft.trim() || uncertain}><IconSend size={16} />{activeTurn ? "补充" : "发送"}</button></div></form>
    </>}</main>
  </div>;
}

function ApprovalCard({ request, disabled, onRespond }: { request: Approval; disabled: boolean; onRespond: (args: Record<string, unknown>) => Promise<void> }) {
  const [answers, setAnswers] = useState<Record<string, string>>({});
  const [revision, setRevision] = useState("");
  if (!request.supported) return <section className="remote-approval"><p>{request.params.message || "请在电脑端处理此请求"}</p></section>;
  if (request.method === "item/plan/requestImplementation") return <section className="remote-approval"><h3>确认工作计划</h3><pre>{request.params.planContent}</pre><label>修改意见<textarea value={revision} disabled={disabled} onChange={event => setRevision(event.target.value)} /></label><div><button className="remote-primary" disabled={disabled || !request.params.planContent} onClick={() => void onRespond({ decision: "implement" })}>执行计划</button><button disabled={disabled || !revision.trim()} onClick={() => void onRespond({ decision: "revise", text: revision })}>修改计划</button></div></section>;
  if (request.params.questions) return <form className="remote-approval" onSubmit={event => { event.preventDefault(); void onRespond({ answers }); }}><h3>Codex 需要你的回答</h3>{request.params.questions.map(question => <label key={question.id}>{question.question}{question.options?.map(option => <button key={option.label} type="button" disabled={disabled} onClick={() => setAnswers(current => ({ ...current, [question.id]: option.label }))}>{option.label}{option.description && <small>{option.description}</small>}</button>)}<input required disabled={disabled} value={answers[question.id] || ""} onChange={event => setAnswers(current => ({ ...current, [question.id]: event.target.value }))} /></label>)}<button disabled={disabled} className="remote-primary">提交回答</button></form>;
  const available = request.params.availableDecisions || ["accept", "decline"];
  return <section className="remote-approval"><h3>等待你的批准</h3><p>{request.params.reason}</p>{request.params.command && <pre>{request.params.command}</pre>}{request.params.permissions !== undefined && <pre>{JSON.stringify(request.params.permissions, null, 2)}</pre>}{request.params.changes !== undefined && <pre>{JSON.stringify(request.params.changes, null, 2)}</pre>}<div>{available.includes("accept") && <button disabled={disabled} className="remote-primary" onClick={() => void onRespond({ decision: "accept" })}>本次允许</button>}{available.includes("decline") && <button disabled={disabled} onClick={() => void onRespond({ decision: "decline" })}>拒绝</button>}{available.includes("cancel") && <button disabled={disabled} onClick={() => void onRespond({ decision: "cancel" })}>取消</button>}</div></section>;
}
