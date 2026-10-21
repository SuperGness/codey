import { useEffect, useRef, useState } from "react";
import { IconArrowLeft, IconChartDonut, IconInfoCircle, IconRefresh } from "@tabler/icons-react";
import { Spinner } from "@heroui/react";
import { Button } from "./components/ui";
import { invoke } from "./api";
import { withTimeout } from "./appUtils";
import { formatUsageNumber, observeUsageRequest, usageCoverage, usageQuery, usageSlices, type LogAnalytics, type UsageLoadState } from "./usageAnalysis";
import styles from "./styles.usage-analysis.css?inline";
import { UsageHeatmap } from "./UsageHeatmap";
import { UsageTimeline } from "./UsageTimeline";
import { UsageDateFilter, usageDatesLabel, usageDatesQuery, usagePresetDates, type UsageDates } from "./UsageDateFilter";

const utc = (timestamp: number) => new Date(timestamp).toLocaleString("zh-CN", { timeZone: "UTC", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false });

export function UsageAnalysisHeader({ onBack }: { onBack: () => void }) {
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus(); }, []);
  return <header className="usage-analysis-topbar">
    <Button variant="ghost" size="icon" className="usage-back" aria-label="返回线路与模型" onPress={onBack}><IconArrowLeft size={20} aria-hidden="true" /></Button>
    <div className="usage-analysis-heading"><h1 ref={heading} tabIndex={-1} id="usage-analysis-title">用量分析</h1><p>模型消耗与使用趋势</p></div>
  </header>;
}

export function UsageAnalysisPanel() {
  const [range, setRange] = useState<UsageDates>(() => usagePresetDates("7d"));
  const [revision, setRevision] = useState(0);
  const [chart, setChart] = useState<"heatmap" | "bars">("bars");
  const [state, setState] = useState<UsageLoadState>({ loading: true, data: null, error: "" });
  useEffect(() => observeUsageRequest(
    () => withTimeout(invoke<LogAnalytics>("query_route_request_log_stats", usageQuery(usageDatesQuery(range))), 30_000, "统计请求超时，请稍后重试。"),
    setState,
  ), [range, revision]);
  const reload = () => { setState({ loading: true, data: null, error: "" }); setRevision((value) => value + 1); };
  const stats = state.data;
  const health = stats?.recordingHealth;
  const dropped = health ? health.droppedFull + health.droppedClosed + health.writeDropped : 0;
  const partial = health && (health.sampleRatePerMillion < 1_000_000 || health.sampledOut > 0 || dropped > 0 || health.writeFailures > 0 || health.observerPanics > 0 || health.writerPanics > 0 || health.shutdownTimeouts > 0);
  const slices = stats?.queryable ? usageSlices(stats) : [];
  const totalTokens = stats?.totalTokensSum;
  // 各字段独立汇总，仅用同一刻度比较输入、输出，不推算总量或缓存占比。
  const tokenComparisonMax = Math.max(stats?.inputTokensSum ?? 0, stats?.outputTokensSum ?? 0);
  const percent = (tokens: number | null) => tokens != null && totalTokens != null && totalTokens > 0 ? tokens / totalTokens * 100 : null;

  return <section className="usage-analysis" aria-labelledby="usage-analysis-title">
    <style>{styles}</style>
    <div className="usage-toolbar">
      <UsageDateFilter value={range} onChange={(value) => {
        if (usageDatesLabel(value) !== usageDatesLabel(range)) { setState({ loading: true, data: null, error: "" }); setRange(value); }
      }} />
      <Button variant="outline" size="sm" className="usage-refresh" loading={state.loading} onClick={reload}><IconRefresh size={14} aria-hidden="true" />刷新</Button>
    </div>
    <div aria-live="polite" aria-busy={state.loading}>
      {state.loading ? <div className="usage-state" role="status"><Spinner size="sm" /><strong>正在统计用量…</strong><span>正在读取所选范围的请求日志</span></div>
        : state.error ? <div className="usage-state usage-error" role="alert"><strong>用量分析加载失败</strong><p>{state.error}</p><Button variant="outline" size="sm" onClick={reload}>重试</Button></div>
        : stats?.queryable === false ? <div className="usage-state"><strong>当前日志暂不可统计</strong><p>{stats.reason === "ndjson_not_queryable" ? "当前日志格式不支持在线统计。返回线路与模型，重新开启日志记录后重试。" : "请检查日志记录状态后重试。"}</p><Button variant="outline" size="sm" onClick={reload}>重试</Button></div>
        : stats ? <>
          {health && !health.active && <p className="usage-notice">{health.enabled ? "日志记录已停止，请检查记录状态；" : "日志记录未开启；"}仍可查看已保留的历史用量。</p>}
          {partial && <p className="usage-notice">当前记录周期存在采样、丢弃或写入异常，统计仅反映已保存记录，不能代表全部请求。</p>}
          {health && health.pendingEntries > 0 && <p className="usage-scope">有 {formatUsageNumber(health.pendingEntries)} 条日志等待写入，可稍后刷新。</p>}
          {stats.total === 0 ? <div className="usage-state"><IconChartDonut size={32} aria-hidden="true" /><strong>所选范围暂无请求日志</strong><span>可切换到全部历史，或开启日志记录后发起请求。</span></div> : <>
            <div className="usage-overview" role="group" aria-label="用量概览">
              <div className="usage-metric usage-metric-total"><span>总 Token</span><strong>{formatUsageNumber(totalTokens)}</strong><small>已知总量合计</small></div>
              <div className="usage-token-comparison">
                {([
                  { key: "input", label: "输入 Token", value: stats.inputTokensSum, detail: "包含缓存输入" },
                  { key: "output", label: "输出 Token", value: stats.outputTokensSum, detail: "上游已上报用量" },
                ] as const).map(({ key, label, value, detail }) => <div className={`usage-token-row usage-token-${key}`} key={key}>
                  <div className="usage-token-row-heading"><span>{label}<small>{detail}</small></span><strong>{formatUsageNumber(value)}</strong></div>
                  <div className={`usage-token-track${value == null ? " usage-token-track-unknown" : ""}`} aria-hidden="true">
                    {value != null && value > 0 && <span style={{ width: `${value / tokenComparisonMax * 100}%` }} />}
                  </div>
                </div>)}
                <div className="usage-token-scale"><span>0</span><span>同一刻度，分别汇总</span><span>{formatUsageNumber(tokenComparisonMax > 0 ? tokenComparisonMax : stats.inputTokensSum === 0 || stats.outputTokensSum === 0 ? 0 : null)}</span></div>
                <div className="usage-cache-detail"><span>缓存 Token <strong>{formatUsageNumber(stats.cachedTokensSum)}</strong></span><small>已计入输入，不重复相加</small></div>
              </div>
              <div className="usage-request-summary">
                <div className="usage-metric"><span>请求数</span><strong>{formatUsageNumber(stats.total)}</strong><small>所选范围内保留的记录</small></div>
                <div className="usage-quality"><span>用量已知覆盖率 <strong>{usageCoverage(stats.total, stats.totalTokensKnownCount)}</strong></span><span>{formatUsageNumber(stats.totalTokensKnownCount)} / {formatUsageNumber(stats.total)} 条总量已知</span></div>
              </div>
            </div>
            <div className="usage-overview-notes"><p>各项按已上报数据分别汇总，上报范围可能不同，输入与输出之和不一定等于总 Token。</p>{stats.totalTokensKnownCount < stats.total && <p>部分请求未上报总 Token，用量与占比仅基于已知数据；— 表示未上报。</p>}</div>
            <div className="usage-workspace">
            <section className="usage-card usage-trend-card" aria-labelledby="usage-trend-title">
              <div className="usage-card-heading"><div><h2 id="usage-trend-title">Token 时间趋势</h2><p>{utc(stats.fromUnixMs)} 至 {utc(stats.toUnixMs)} UTC（不含结束时间）</p></div><div className="usage-chart-switch usage-segmented" role="group" aria-label="趋势图类型"><Button size="xs" variant="ghost" aria-pressed={chart === "bars"} onPress={() => setChart("bars")}>直方图</Button><Button size="xs" variant="ghost" aria-pressed={chart === "heatmap"} onPress={() => setChart("heatmap")}>热力图</Button></div></div>
              {chart === "heatmap" && <UsageHeatmap key={usageDatesLabel(range)} stats={stats} />}
              {stats.trend.length ? <>
                {chart === "bars" && <UsageTimeline key={`${usageDatesLabel(range)}:${revision}:${stats.fromUnixMs}:${stats.toUnixMs}`} stats={stats} />}
                <details className="usage-trend-details"><summary>查看趋势明细</summary><div className="usage-table-scroll"><table><thead><tr><th>分桶起点（UTC）</th><th>请求数</th><th>Token</th></tr></thead><tbody>{stats.trend.map((bucket) => <tr key={bucket.timestampUnixMs}><td>{utc(bucket.timestampUnixMs)}</td><td>{formatUsageNumber(bucket.total)}</td><td>{formatUsageNumber(bucket.totalTokensSum)}</td></tr>)}</tbody></table></div></details>
              </> : <p className="usage-scope">暂无趋势数据</p>}
            </section>
            <section className="usage-card usage-model-card" aria-labelledby="usage-distribution-title">
              <div className="usage-card-heading"><div><h2 id="usage-distribution-title">模型用量占比</h2><p>按已知 Token 从高到低排列，展示前 12 项</p></div></div>
              <table className="usage-model-table" aria-labelledby="usage-distribution-title">
                <thead><tr><th scope="col">模型</th><th scope="col">请求数</th><th scope="col">Token</th><th scope="col">占比</th></tr></thead>
                <tbody>{slices.map((slice, index) => <tr key={`${slice.other ? "other" : "group"}:${slice.key}`}>
                  <th scope="row"><span className="usage-model-name"><span className="usage-rank" aria-hidden="true">{slice.other ? "·" : String(index + 1).padStart(2, "0")}</span><span>{slice.other ? "其他" : slice.key || "未知模型"}</span></span></th>
                  <td><span className="usage-mobile-label">请求数</span>{formatUsageNumber(slice.requests)}</td>
                  <td className="usage-model-tokens"><span className="usage-mobile-label">Token</span>{formatUsageNumber(slice.tokens)}</td>
                  <td><span className="usage-mobile-label">占比</span><span className="usage-model-share"><span className="usage-share-track" aria-hidden="true"><span style={{ width: `${percent(slice.tokens) ?? 0}%` }} /></span><span>{percent(slice.tokens)?.toFixed(1) ?? "—"}{percent(slice.tokens) == null ? "" : "%"}</span></span></td>
                </tr>)}</tbody>
              </table>
              {(stats.groups.length > 12 || stats.groupsTruncated) && <p className="usage-scope">其他包含前 12 项以外的所有分组{stats.groupsTruncated ? "，包括未单独返回的分组" : ""}。</p>}
            </section>
            </div>
          </>}
        </> : null}
    </div>
    <footer className="usage-context"><IconInfoCircle size={15} aria-hidden="true" /><p><strong>统计口径</strong>数据来自已保留的请求日志，仅统计经过 Codey 路由的请求，不包含官方直连。清理日志会影响历史统计。</p></footer>
  </section>;
}
