import { useEffect, useRef, useState } from "react";
import { IconArrowDown, IconArrowLeft, IconDeviceLaptop, IconRefresh, IconSquarePlus } from "@tabler/icons-react";
import { RemoteError, remoteRequest, requestId, readSubmission, rememberSubmission, forgetSubmission } from "./transport";
import { modelOptions, defaultThreadSettings, projectForThread, type Thread, type Project, type Approval, type View, type ModelCatalog, type ThreadSettings } from "./workspace-data";
import { Sidebar } from "./sidebar";
import { Composer } from "./composer";
import { ConversationTurn, RichText } from "./messages";

export function Workspace({ active, onUnauthorized, onPanel, onLogout }: { active: boolean; onUnauthorized: () => void; onPanel: () => void; onLogout: () => void }) {
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
  const [creating, setCreating] = useState(false);
  const [draftSettings, setDraftSettings] = useState<ThreadSettings>({});
  const [creationDefaults, setCreationDefaults] = useState<{ project: string; settings: ThreadSettings } | null>(null);
  const [defaultsError, setDefaultsError] = useState("");
  const [defaultsRevision, setDefaultsRevision] = useState(0);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [catalog, setCatalog] = useState<ModelCatalog>({});
  const [catalogError, setCatalogError] = useState("");
  const [loading, setLoading] = useState(true);
  const [atBottom, setAtBottom] = useState(true);
  const [uncertain, setUncertain] = useState(false);
  const scroll = useRef<HTMLDivElement>(null);
  const listVersion = useRef(0);
  const pinned = useRef(true);
  const drafts = useRef(new Map<string, { text: string; uncertain: boolean; settings: ThreadSettings }>());
  const models = modelOptions(catalog);
  const selectedId = selected?.id;
  const draftScope = creating ? `create:${project}` : selectedId;
  const draftProject = projects.find(p => p.id === project);
  const defaultsReady = creationDefaults?.project === project;
  const composerSettings = { ...defaultThreadSettings(catalog, defaultsReady ? creationDefaults.settings : {}), ...draftSettings };
  const composerView: View | null = creating ? { id: "", turns: [], requests: [], historyComplete: true, ...composerSettings } : view;
  const activeTurn = view ? [...view.turns].reverse().find(turn => turn.status === "inProgress") : undefined;

  function fail(error: unknown) {
    if (error instanceof RemoteError && error.status === 401) onUnauthorized();
    setError(error instanceof Error ? error.message : String(error));
  }

  async function refresh() {
    const version = ++listVersion.current;
    setLoading(true);
    try {
      const [threads, projects] = await Promise.all([
        remoteRequest<Thread[]>("/remote/threads", { search, archived }),
        remoteRequest<Project[]>("/remote/projects", {}),
      ]);
      if (version !== listVersion.current) return;
      setThreads(threads); setProjects(projects);
      setProject(current => projects.some(project => project.id === current) ? current : projects[0]?.id || "");
    } catch (error) { if (version === listVersion.current) fail(error); }
    finally { if (version === listVersion.current) setLoading(false); }
  }

  useEffect(() => {
    if (!active) return;
    const timer = window.setTimeout(() => void refresh(), search ? 180 : 0);
    return () => { window.clearTimeout(timer); listVersion.current += 1; };
  }, [active, archived, search]);

  async function loadModels() {
    try { setCatalog(await remoteRequest<ModelCatalog>("/remote/models", {})); setCatalogError(""); }
    catch (error) {
      if (error instanceof RemoteError && error.status === 401) onUnauthorized();
      setCatalogError("模型列表暂时无法加载，仍沿用桌面设置。");
    }
  }
  useEffect(() => { if (active) void loadModels(); }, [active]);

  useEffect(() => {
    if (!active || !creating || !project) return;
    let disposed = false;
    setCreationDefaults(null); setDefaultsError("");
    void remoteRequest<ThreadSettings>("/remote/defaults", { projectId: project }).then(settings => {
      if (!disposed) setCreationDefaults({ project, settings });
    }).catch(error => {
      if (disposed) return;
      if (error instanceof RemoteError && error.status === 401) onUnauthorized();
      setDefaultsError("桌面默认设置暂时无法加载，请重试。");
    });
    return () => { disposed = true; };
  }, [active, creating, project, defaultsRevision]);

  useEffect(() => {
    if (!active || !selectedId) return;
    setConnected(false); setConnectionError("");
    let disposed = false;
    let unavailable = false;
    let timer: number | undefined;
    let socket: WebSocket;
    let delay = 2000;
    const connect = () => {
      if (disposed) return;
      socket = new WebSocket(`${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/remote/events/${encodeURIComponent(selectedId)}`);
      socket.onmessage = event => {
        if (disposed || unavailable) return;
        try {
          const message = JSON.parse(event.data);
          if (message.type === "disconnected") { setConnected(false); setConnectionError(message.message); unavailable = true; socket.close(); return; }
          const next = message.state as View;
          if (message.type !== "state" || next.id !== selectedId) return;
          setView(next); setConnected(true); setConnectionError(""); delay = 2000;
        } catch { setConnectionError("会话数据无效，请重新连接"); }
      };
      socket.onclose = () => {
        if (disposed) return;
        setConnected(false); setConnectionError(current => current || "连接已断开，正在重新连接…");
        void remoteRequest("/remote/session").catch(error => {
          if (!disposed && error instanceof RemoteError && error.status === 401) { disposed = true; window.clearTimeout(timer); onUnauthorized(); }
        });
        if (!unavailable) { timer = window.setTimeout(connect, delay); delay = Math.min(delay * 2, 30000); }
      };
    };
    connect();
    return () => { disposed = true; window.clearTimeout(timer); socket.close(); };
  }, [active, selectedId, revision]);

  useEffect(() => { if (pinned.current && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; }, [view]);
  useEffect(() => { if (draftScope) drafts.current.set(draftScope, { text: draft, uncertain, settings: draftSettings }); }, [draftScope, draft, uncertain, draftSettings]);

  async function action(action: string, args: Record<string, unknown> = {}) {
    if (!selectedId) return;
    return remoteRequest("/remote/action", { ...args, threadId: selectedId, action, requestId: requestId() });
  }

  async function run(operation: () => Promise<unknown>) {
    setBusy(true); setError("");
    try { await operation(); } catch (error) { fail(error); }
    finally { setBusy(false); }
  }

  function restoreDraft(scope: string) {
    const previous = drafts.current.get(scope);
    const pending = readSubmission(scope);
    setDraft(previous?.text || String(pending?.args.text || ""));
    setUncertain(previous?.uncertain || !!pending);
    setDraftSettings(previous?.settings || Object.fromEntries(["model", "effort", "permissionMode", "serviceTier"].flatMap(key => typeof pending?.args[key] === "string" || (key === "serviceTier" && pending?.args[key] === null) ? [[key, pending!.args[key]]] : [])));
  }

  function open(thread: Thread) {
    if (selectedId === thread.id) return;
    setCreating(false); setSelected(thread); setView(null); setConnected(false); setConnectionError(""); setError("");
    restoreDraft(thread.id); pinned.current = true; setAtBottom(true);
  }

  async function send(event: React.FormEvent) {
    event.preventDefault();
    if (busy || uncertain || !draft.trim()) return;
    if (creating) {
      if (!draftProject || !draftScope || !defaultsReady) return;
      const scope = draftScope;
      const text = draft;
      setDraftSettings(composerSettings);
      const submission = rememberSubmission(scope, { projectId: project, text, ...composerSettings });
      await run(async () => {
        try {
          const result = await remoteRequest<{ id: string; firstTurn: string }>("/remote/create", { ...submission.args, requestId: submission.id });
          if (!result.id) throw new Error("新建结果尚未确认，请先检查会话列表");
          forgetSubmission(scope); drafts.current.delete(scope);
          open({ id: result.id, title: "新会话", cwd: draftProject.cwd, updatedAt: Date.now() });
          if (result.firstTurn !== "accepted") {
            setDraft(text);
            if (result.firstTurn !== "not-started" && result.firstTurn !== "not-requested") {
              rememberSubmission(result.id, { action: "send", text }); setUncertain(true);
            }
            setError("会话已创建，请检查第一条消息是否已发送。");
          }
          void refresh();
        } catch (error) { setUncertain(true); throw error; }
      });
      return;
    }
    if (!selectedId || !connected) return;
    const mode = activeTurn ? "steer" : "send";
    const submission = rememberSubmission(selectedId, { action: mode, text: draft });
    await run(async () => {
      try {
        await remoteRequest("/remote/action", { threadId: selectedId, ...submission.args, requestId: submission.id });
        setDraft(""); forgetSubmission(selectedId); setUncertain(false); pinned.current = true;
      } catch (error) { setUncertain(true); throw error; }
    });
  }

  function startCreating(target?: Project) {
    const next = target?.id || project;
    setProject(next); setCreating(true); setCreationDefaults(null); setDefaultsRevision(n => n + 1); setSelected(null); setView(null); setConnected(false); setConnectionError(""); setError("");
    restoreDraft(`create:${next}`); pinned.current = true; setAtBottom(true);
  }
  const selectedProject = selected ? projectForThread(selected, projects) : undefined;
  return <div className={`remote-codex-workspace${selected || creating ? " has-selection" : ""}`}>
    <Sidebar threads={threads} projects={projects} selectedId={selectedId} busy={busy} loading={loading} search={search} archived={archived} error={!selected && !creating ? error : ""} onSearch={setSearch} onArchived={setArchived} onOpen={open} onCreate={startCreating} onRefresh={() => { void refresh(); void loadModels(); }} onPanel={onPanel} onLogout={onLogout} />
    <main className="remote-chat">{!selected && !creating ? <div className="remote-welcome"><IconDeviceLaptop size={36} stroke={1.25} /><h2>今天想做些什么？</h2><p>选择项目或会话，继续电脑上的工作。</p><button className="remote-primary" onClick={() => startCreating()}><IconSquarePlus size={18} />新建会话</button></div> : <>
      <header className="remote-chat-header"><button className="remote-back remote-icon-button" aria-label="返回会话列表" disabled={busy} onClick={() => { setSelected(null); setCreating(false); void refresh(); }}><IconArrowLeft size={22} /></button><div><h2>{creating ? "新会话" : view?.title || selected?.title}</h2>{creating ? <label className="remote-draft-project">项目<select aria-label="项目" value={project} disabled={busy} onChange={event => startCreating(projects.find(p => p.id === event.target.value))}>{projects.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label> : <p><span className={connected ? "remote-chat-status-dot is-connected" : "remote-chat-status-dot"} />{selectedProject?.name || selected?.cwd.split(/[\\/]/).filter(Boolean).pop() || "本地电脑"}<span>·</span>{connected ? activeTurn ? "正在工作" : "已连接" : connectionError ? "未连接" : "连接中"}</p>}</div><button className="remote-icon-button" disabled={busy} aria-label="新建会话" onClick={() => startCreating(selectedProject)}><IconSquarePlus size={20} /></button>{!creating && <button className="remote-icon-button" disabled={busy} aria-label="重新连接" onClick={() => setRevision(n => n + 1)}><IconRefresh size={18} /></button>}</header>
      {connectionError && <p className="remote-connection" role="status">{connectionError}</p>}
      <div className="remote-transcript">
      <div className="remote-messages" ref={scroll} onScroll={event => { const element = event.currentTarget; pinned.current = element.scrollHeight - element.scrollTop - element.clientHeight < 100; setAtBottom(pinned.current); }}>
        {view && !view.historyComplete && <button disabled={busy || !connected} onClick={() => void run(async () => { await action("history"); setRevision(n => n + 1); })}>加载完整历史</button>}
        {!creating && !view && <p className="remote-chat-loading">{connectionError ? "暂时无法读取会话，请点击重新连接。" : "正在获取桌面会话…"}</p>}
        {(creating || view?.turns.length === 0) && <div className="remote-chat-empty"><h2>从一个想法开始</h2><p>{creating && !draftProject ? "请先在电脑 Codex 中保存本地项目。" : `描述任务，让 Codex 在 ${draftProject && creating ? draftProject.name : selectedProject?.name || "当前工作区"} 中开始工作。`}</p></div>}
        {view?.turns.map((turn, index) => <ConversationTurn key={turn.id || index} turn={turn} />)}
        {view?.requests.map(request => <ApprovalCard key={String(request.id)} request={request} disabled={busy || !connected} onRespond={args => run(() => action("respond", { approvalId: request.id, ...args }))} />)}
        {activeTurn && <p className="remote-working" role="status"><span className="remote-chat-status-dot is-connected" />Codex 正在工作…</p>}
      </div>
      {!atBottom && <button className="remote-jump remote-icon-button" aria-label="滚动到最新消息" onClick={() => { if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; pinned.current = true; setAtBottom(true); }}><IconArrowDown size={20} /></button>}
      </div>
      {error && <p className="remote-client-error" role="alert">{error}</p>}
      {uncertain && <div className="remote-uncertain"><p>消息未确认。请查看会话是否已收到，确认后再决定是否重新发送。</p><button onClick={() => { if (draftScope) forgetSubmission(draftScope); setUncertain(false); setError(""); }}>已核对，允许再次发送</button></div>}
      <Composer draft={draft} onDraft={setDraft} view={composerView} models={models} connected={creating ? !!draftProject && defaultsReady : connected} busy={busy} uncertain={uncertain} onSend={event => void send(event)} onStop={() => { if (activeTurn) void run(() => action("interrupt", { turnId: activeTurn.id })); }} onSettings={settings => { if (creating) setDraftSettings({ ...composerSettings, ...settings }); else void run(async () => { await action("settings", settings); setRevision(n => n + 1); }); }} catalogError={[catalogError, creating ? defaultsError : ""].filter(Boolean).join(" ")} onReloadModels={() => { void loadModels(); setDefaultsRevision(n => n + 1); }} />
    </>}</main>
  </div>;
}

function ApprovalCard({ request, disabled, onRespond }: { request: Approval; disabled: boolean; onRespond: (args: Record<string, unknown>) => Promise<void> }) {
  const [answers, setAnswers] = useState<Record<string, string>>({});
  const [revision, setRevision] = useState("");
  if (!request.supported) return <section className="remote-approval"><p>{request.params.message || "请在电脑端处理此请求"}</p></section>;
  if (request.method === "item/plan/requestImplementation") return <section className="remote-approval"><h3>确认工作计划</h3><RichText text={request.params.planContent || ""} /><label>修改意见<textarea value={revision} disabled={disabled} onChange={event => setRevision(event.target.value)} /></label><div><button className="remote-primary" disabled={disabled || !request.params.planContent} onClick={() => void onRespond({ decision: "implement" })}>执行计划</button><button disabled={disabled || !revision.trim()} onClick={() => void onRespond({ decision: "revise", text: revision })}>修改计划</button></div></section>;
  if (request.params.questions) return <form className="remote-approval" onSubmit={event => { event.preventDefault(); void onRespond({ answers }); }}><h3>Codex 需要你的回答</h3>{request.params.questions.map(question => <label key={question.id}>{question.question}{question.options?.map(option => <button key={option.label} type="button" disabled={disabled} onClick={() => setAnswers(current => ({ ...current, [question.id]: option.label }))}>{option.label}{option.description && <small>{option.description}</small>}</button>)}<input required disabled={disabled} value={answers[question.id] || ""} onChange={event => setAnswers(current => ({ ...current, [question.id]: event.target.value }))} /></label>)}<button disabled={disabled} className="remote-primary">提交回答</button></form>;
  const available = request.params.availableDecisions || ["accept", "decline"];
  return <section className="remote-approval"><h3>等待你的批准</h3><p>{request.params.reason}</p>{request.params.command && <pre>{request.params.command}</pre>}{request.params.permissions !== undefined && <pre>{JSON.stringify(request.params.permissions, null, 2)}</pre>}{request.params.changes !== undefined && <pre>{JSON.stringify(request.params.changes, null, 2)}</pre>}<div>{available.includes("accept") && <button disabled={disabled} className="remote-primary" onClick={() => void onRespond({ decision: "accept" })}>本次允许</button>}{available.includes("decline") && <button disabled={disabled} onClick={() => void onRespond({ decision: "decline" })}>拒绝</button>}{available.includes("cancel") && <button disabled={disabled} onClick={() => void onRespond({ decision: "cancel" })}>取消</button>}</div></section>;
}
