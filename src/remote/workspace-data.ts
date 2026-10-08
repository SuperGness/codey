export type Thread = { id: string; title: string; cwd: string; updatedAt: number };
export type Project = { id: string; name: string; cwd: string; rootPaths?: string[] };
export type Question = { id: string; question: string; options?: { label: string; description?: string }[] };
export type Approval = { id: string | number; method: string; supported: boolean; params: { command?: string; reason?: string; questions?: Question[]; availableDecisions?: string[]; message?: string; permissions?: unknown; changes?: unknown; planContent?: string } };
export type Attachment = { name: string; url?: string };
export type Message = { id?: string; role: string; kind: string; text: string; status?: string; attachments?: Attachment[] };
export type Turn = { id: string; status: string; error?: unknown; startedAt?: number; completedAt?: number; messages: Message[] };
export type View = { id: string; title?: string; cwd?: string; model?: string; effort?: string; permissionMode?: string; status?: string; turns: Turn[]; requests: Approval[]; historyComplete: boolean };
export type ModelOption = { id: string; label: string; route: string; efforts: string[]; defaultEffort: string };
export type ModelCatalog = { models?: string[]; clear_models?: boolean; model_metadata?: { model: string; display_name?: string; model_display_name?: string; route_name?: string; supported_reasoning_efforts?: string[]; default_reasoning_effort?: string }[] };

export const effortLabels: Record<string, string> = { none: "无", minimal: "最低", low: "低", medium: "中", high: "高", xhigh: "超高", max: "Max", ultra: "Ultra" };
export const permissionLabels: Record<string, string> = { "read-only": "只读", auto: "默认权限", "full-access": "完全访问", custom: "自定义权限" };

function normalizedPath(path: string): string {
  const normalized = path.replace(/\\/g, "/").replace(/^\/\/\?\/UNC\//i, "//").replace(/^\/\/\?\//, "").replace(/\/+$/, "");
  return /^[a-z]:/i.test(normalized) || normalized.startsWith("//") ? normalized.toLowerCase() : normalized;
}

export function projectForThread(thread: Thread, projects: readonly Project[]): Project | undefined {
  const cwd = normalizedPath(thread.cwd);
  let best: Project | undefined;
  let length = -1;
  for (const project of projects) {
    for (const path of project.rootPaths?.length ? project.rootPaths : [project.cwd]) {
      const root = normalizedPath(path);
      if (path && (cwd === root || cwd.startsWith(`${root}/`)) && root.length > length) {
        best = project; length = root.length;
      }
    }
  }
  return best;
}

export function groupThreads(threads: readonly Thread[], projects: readonly Project[]) {
  const groups = projects.map(project => ({ project, threads: [] as Thread[] }));
  const byId = new Map(groups.map(group => [group.project.id, group]));
  const recent: Thread[] = [];
  for (const thread of [...threads].sort((a, b) => b.updatedAt - a.updatedAt)) {
    const project = projectForThread(thread, projects);
    const group = project && byId.get(project.id);
    (group ? group.threads : recent).push(thread);
  }
  return { groups, recent };
}

export function modelOptions(catalog: ModelCatalog): ModelOption[] {
  if (catalog.clear_models) return [];
  const metadata = new Map(catalog.model_metadata?.map(entry => [entry.model, entry]));
  return [...new Set(catalog.models || [])].map(id => {
    const entry = metadata.get(id);
    const efforts = [...new Set(entry?.supported_reasoning_efforts || [])].filter(value => value in effortLabels);
    return { id, label: entry?.model_display_name || entry?.display_name || id, route: entry?.route_name || "模型", efforts, defaultEffort: efforts.includes(entry?.default_reasoning_effort || "") ? entry!.default_reasoning_effort! : efforts[0] || "" };
  });
}

export function relativeTime(timestamp: number, now = Date.now()): string {
  if (!Number.isFinite(timestamp) || timestamp <= 0) return "";
  const minutes = Math.max(0, Math.floor((now - timestamp) / 60000));
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)} 小时`;
  if (minutes < 10080) return `${Math.floor(minutes / 1440)} 天`;
  return new Date(timestamp).toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
}

// Desktop filesystem links are labels on the phone, never requests against the
// remote API. Keep executable protocols and embedded HTML out of message links.
export function messageUrl(url: string): string {
  return /^(https?:\/\/|mailto:)/i.test(url) ? url : "";
}

export function imageUrl(url: string): string {
  return /^https:\/\//i.test(url) || /^data:image\/(png|jpeg|gif|webp);base64,[a-z0-9+/=]+$/i.test(url) ? url : "";
}
