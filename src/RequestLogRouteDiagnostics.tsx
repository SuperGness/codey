export type RouteDecision = {
  settingsRevision: number;
  reason: string;
  candidateCount: number;
  historicalAlias: boolean;
  defaultedModel: boolean;
  rejectedHint?: string | null;
  rejectedBinding?: string | null;
};

export type UpstreamAttempt = {
  sequence: number;
  transport: string;
  trigger: string;
  startedMs: number;
  durationMs?: number | null;
  headerMs?: number | null;
  statusCode?: number | null;
  outcome: string;
  phase: string;
};

export type RouteDiagnostics = {
  routeDecision?: RouteDecision | null;
  upstreamAttempts?: UpstreamAttempt[] | null;
  attemptsOmitted?: number | null;
};

const decisionLabels: Record<string, string> = {
  not_resolved: "尚未完成线路选择",
  explicit_alias: "按模型别名指定的线路选择",
  route_hint: "按请求中的线路提示选择",
  default_official: "使用默认官方线路",
  session_binding: "沿用会话绑定线路",
  unique_candidate: "使用唯一匹配线路",
  auto_review_official: "自动评审使用官方线路",
  auto_review_misc_model: "自动评审使用备用模型",
  portable_target_handoff: "向目标线路移交可迁移历史",
  portable_source_handoff: "由原线路生成交接摘要",
  portable_handoff_unavailable: "无法迁移当前历史",
  missing_model: "请求缺少模型",
  invalid_metadata: "请求元数据无效",
  ambiguous_model: "同名模型匹配多条线路，无法确定目标",
  model_not_enabled: "模型未在目标线路启用",
  route_unavailable: "目标线路不可用",
};

const rejectionLabels: Record<string, string> = {
  route_missing: "线路不存在或不可用",
  model_not_enabled: "线路未启用该模型",
};

const triggerLabels: Record<string, string> = {
  initial: "首次请求",
  plugin_retry: "插件请求重试",
  reasoning_repair: "补全思考内容后重试",
  transport_fallback: "切换传输方式",
};

const outcomeLabels: Record<string, string> = {
  pending: "结果未记录",
  succeeded: "成功",
  rejected: "上游拒绝",
  failed: "失败",
  timed_out: "超时",
  cancelled: "已中断",
  superseded: "已由后续请求替代",
  incomplete: "未完成",
  delivery_failed: "向客户端传输失败",
};

const phaseLabels: Record<string, string> = {
  send: "发送请求",
  response_headers: "接收响应头",
  response_body: "接收响应内容",
  downstream_delivery: "向客户端传输",
  complete: "处理结束",
};

const transportLabels: Record<string, string> = { http: "HTTP", http_sse: "HTTP / SSE", ws: "WebSocket" };
const cardClass = "rounded-xl border border-[rgb(var(--codey-ink-rgb,0,0,0))]/8 bg-[var(--codey-surface-muted,#fafafa)] p-3.5";
const labelClass = "text-[11px] text-[var(--codey-subtle,#8e8e93)]";

function labelFor(labels: Record<string, string>, value?: string | null): string {
  return value ? (Object.prototype.hasOwnProperty.call(labels, value) ? labels[value] : value) : "未记录";
}

function milliseconds(value?: number | null): string {
  return value != null && Number.isFinite(value) && value >= 0 ? `${value.toLocaleString()} ms` : "未记录";
}

function outcomeClass(outcome: string): string {
  if (outcome === "succeeded") return "text-emerald-700 dark:text-emerald-400";
  if (["rejected", "failed", "timed_out", "delivery_failed"].includes(outcome)) return "text-red-700 dark:text-red-400";
  return "text-[var(--codey-muted,#6e6e73)]";
}

export function RequestLogRouteDiagnostics({ item }: { item: RouteDiagnostics }) {
  const decision = item.routeDecision;
  const attempts = item.upstreamAttempts ?? [];
  const omitted = item.attemptsOmitted ?? 0;
  if (!decision && attempts.length === 0 && omitted <= 0) return null;

  return (
    <section className={cardClass} aria-label="路由诊断">
      <h4 className="m-0 border-b border-[rgb(var(--codey-ink-rgb,0,0,0))]/6 pb-2 text-xs font-semibold">路由诊断</h4>
      {decision ? (
        <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5">
          <div className="col-span-2 min-w-0">
            <dt className={labelClass}>选择依据</dt>
            <dd className="m-0 mt-0.5 break-words">{labelFor(decisionLabels, decision.reason)}</dd>
          </div>
          <div>
            <dt className={labelClass}>配置版本</dt>
            <dd className="m-0 mt-0.5 font-mono">{decision.settingsRevision}</dd>
          </div>
          <div>
            <dt className={labelClass}>匹配候选线路</dt>
            <dd className="m-0 mt-0.5">{decision.candidateCount} 条</dd>
          </div>
          {decision.historicalAlias || decision.defaultedModel ? (
            <div className="col-span-2">
              <dt className={labelClass}>模型处理</dt>
              <dd className="m-0 mt-0.5">{[decision.historicalAlias ? "已还原历史模型别名" : null, decision.defaultedModel ? "已使用默认模型" : null].filter(Boolean).join("；")}</dd>
            </div>
          ) : null}
          {decision.rejectedHint ? (
            <div className="col-span-2 min-w-0">
              <dt className={labelClass}>未采用请求线路提示</dt>
              <dd className="m-0 mt-0.5 break-words">{labelFor(rejectionLabels, decision.rejectedHint)}</dd>
            </div>
          ) : null}
          {decision.rejectedBinding ? (
            <div className="col-span-2 min-w-0">
              <dt className={labelClass}>未沿用会话绑定</dt>
              <dd className="m-0 mt-0.5 break-words">{labelFor(rejectionLabels, decision.rejectedBinding)}</dd>
            </div>
          ) : null}
        </dl>
      ) : null}
      {attempts.length > 0 || omitted > 0 ? (
        <div className={decision ? "mt-3.5 border-t border-[rgb(var(--codey-ink-rgb,0,0,0))]/6 pt-3.5" : "mt-3"}>
          <h5 className="m-0 text-xs font-semibold">上游请求过程</h5>
          <p className={`m-0 mt-1 ${labelClass}`}>按实际发送顺序记录，不包含连接握手。</p>
          <ol className="m-0 mt-2 list-none space-y-2 p-0">
            {attempts.map((attempt, index) => (
              <li key={`${attempt.sequence}-${index}`} className="min-w-0 rounded-lg border border-[rgb(var(--codey-ink-rgb,0,0,0))]/6 bg-[var(--codey-surface,#fff)] p-2.5">
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
                  <span className="break-all font-medium">第 {attempt.sequence} 次 · {labelFor(transportLabels, attempt.transport)}</span>
                  <span className={`break-all font-medium ${outcomeClass(attempt.outcome)}`}>{labelFor(outcomeLabels, attempt.outcome)}</span>
                </div>
                <p className="m-0 mt-1 break-words text-[var(--codey-muted,#6e6e73)]">{labelFor(triggerLabels, attempt.trigger)} · {labelFor(phaseLabels, attempt.phase)}{attempt.statusCode != null ? ` · HTTP ${attempt.statusCode}` : ""}</p>
                <dl className="mb-0 mt-2 grid grid-cols-2 gap-x-3 gap-y-1.5 sm:grid-cols-3">
                  <div><dt className={labelClass}>请求开始后</dt><dd className="m-0 mt-0.5 font-mono">{milliseconds(attempt.startedMs)}</dd></div>
                  <div><dt className={labelClass}>本次耗时</dt><dd className="m-0 mt-0.5 font-mono">{milliseconds(attempt.durationMs)}</dd></div>
                  <div><dt className={labelClass}>收到响应头</dt><dd className="m-0 mt-0.5 font-mono">{milliseconds(attempt.headerMs)}</dd></div>
                </dl>
              </li>
            ))}
          </ol>
          {omitted > 0 ? <p className={`mb-0 mt-2 ${labelClass}`}>另有 {omitted} 次请求超出记录上限，未保存明细。</p> : null}
        </div>
      ) : null}
    </section>
  );
}
