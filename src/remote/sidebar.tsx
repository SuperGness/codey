import { useMemo, useState } from "react";
import { IconArchive, IconChevronDown, IconChevronRight, IconDeviceLaptop, IconFolder, IconFolderOpen, IconLogout, IconRefresh, IconSearch, IconSettings, IconSquarePlus, IconX } from "@tabler/icons-react";
import { groupThreads, relativeTime, type Project, type Thread } from "./workspace-data";

type Props = {
  threads: Thread[]; projects: Project[]; selectedId?: string; busy: boolean; loading: boolean;
  search: string; archived: boolean; error: string;
  onSearch: (search: string) => void; onArchived: (archived: boolean) => void;
  onOpen: (thread: Thread) => void; onCreate: (project?: Project) => void;
  onRefresh: () => void; onPanel: () => void; onLogout: () => void;
};

export function Sidebar(props: Props) {
  const [collapsed, setCollapsed] = useState(new Set<string>());
  const [expanded, setExpanded] = useState(new Set<string>());
  const [searching, setSearching] = useState(false);
  const { groups, recent } = useMemo(() => groupThreads(props.threads, props.projects), [props.threads, props.projects]);
  const toggle = (setter: typeof setCollapsed, id: string) => setter(current => {
    const next = new Set(current); if (next.has(id)) next.delete(id); else next.add(id); return next;
  });
  function threadRow(thread: Thread) {
    return <button key={thread.id} className="remote-thread" aria-current={props.selectedId === thread.id ? "page" : undefined} disabled={props.busy} onClick={() => props.onOpen(thread)} title={thread.title}><span>{thread.title}</span><time dateTime={new Date(thread.updatedAt || 0).toISOString()}>{relativeTime(thread.updatedAt)}</time></button>;
  }
  return <aside className="remote-thread-list" aria-label="项目和会话">
    <div className="remote-list-heading"><h1>Codex</h1><button className="remote-icon-button" aria-label="搜索会话" aria-expanded={searching} onClick={() => { setSearching(!searching); if (searching) props.onSearch(""); }}><IconSearch size={21} /></button><button className="remote-icon-button" aria-label="新建会话" onClick={() => props.onCreate()} disabled={props.busy}><IconSquarePlus size={21} /></button></div>
    <div className="remote-host"><IconDeviceLaptop size={16} /><span>本地电脑</span><span className="remote-paired">已配对</span></div>
    {searching && <div className="remote-search"><IconSearch size={16} /><input type="search" autoFocus placeholder="搜索会话或项目" aria-label="搜索会话或项目" value={props.search} onChange={event => props.onSearch(event.target.value)} maxLength={200} /><button className="remote-icon-button" aria-label="清除搜索" onClick={() => props.onSearch("")}><IconX size={15} /></button></div>}
    <div className="remote-navigation" aria-busy={props.loading}>
      <div className="remote-section-heading"><h2>{props.archived ? "归档项目" : "项目"}</h2><button className="remote-icon-button" aria-label="刷新会话" onClick={props.onRefresh} disabled={props.busy || props.loading}><IconRefresh size={16} /></button></div>
      {groups.filter(group => !props.search || group.threads.length || group.project.name.toLowerCase().includes(props.search.toLowerCase())).map(({ project, threads }) => {
        const open = !collapsed.has(project.id) || !!props.search;
        const all = expanded.has(project.id) || !!props.search;
        return <section className="remote-project" key={project.id}>
          <div className="remote-project-heading"><button aria-expanded={open} onClick={() => toggle(setCollapsed, project.id)} title={project.cwd}>{open ? <IconFolderOpen size={19} /> : <IconFolder size={19} />}<span>{project.name}</span>{open ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}</button><button className="remote-icon-button" aria-label={`在 ${project.name} 中新建会话`} disabled={props.busy} onClick={() => props.onCreate(project)}><IconSquarePlus size={17} /></button></div>
          {open && <div className="remote-project-threads">{(all ? threads : threads.slice(0, 5)).map(threadRow)}{threads.length > 5 && !props.search && <button className="remote-show-more" onClick={() => toggle(setExpanded, project.id)}>{all ? "收起" : `展开显示 (${threads.length - 5})`}</button>}</div>}
        </section>;
      })}
      <div className="remote-section-heading remote-recent-heading"><h2>{props.archived ? "归档会话" : "最近"}</h2></div>
      {recent.map(threadRow)}
      {!props.loading && !props.threads.length && <p className="remote-list-empty">{props.search ? "没有找到匹配的会话" : props.archived ? "暂无归档会话" : "选择项目，开始新的对话"}</p>}
      {props.loading && !props.threads.length && <p className="remote-list-empty" role="status">正在加载会话…</p>}
      {props.error && <p role="alert" className="remote-client-error">{props.error}</p>}
    </div>
    <footer className="remote-sidebar-footer"><button aria-pressed={props.archived} onClick={() => props.onArchived(!props.archived)}><IconArchive size={18} />{props.archived ? "返回会话" : "归档会话"}</button><button onClick={props.onPanel}><IconSettings size={18} />Codey 控制台</button><button className="remote-icon-button" aria-label="退出此设备" onClick={props.onLogout}><IconLogout size={18} /></button></footer>
  </aside>;
}
