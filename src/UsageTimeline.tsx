import { useMemo, useRef, useState } from "react";
import { IconChevronLeft, IconChevronRight } from "@tabler/icons-react";
import { Button } from "./components/ui";
import { formatUsageNumber, usageTrend, type LogAnalytics } from "./usageAnalysis";
import { usageTimelineView } from "./usageTimelineView";

const utc = (timestamp: number) => new Date(timestamp).toLocaleString("zh-CN", { timeZone: "UTC", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false });
const shortDate = (timestamp: number, includeYear: boolean) => new Date(timestamp).toLocaleString("zh-CN", { timeZone: "UTC", ...(includeYear ? { year: "numeric" as const } : {}), month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false });
const compactNumber = (value: number | null) => value == null ? "—" : Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 }).format(value);

export function UsageTimeline({ stats }: { stats: LogAnalytics }) {
  const view = useMemo(() => usageTimelineView(stats), [stats]);
  const whole = useMemo(() => usageTrend(stats), [stats]);
  const [selectedIndex, setSelectedIndex] = useState(view?.latestIndex ?? 0);
  const [hoveredIndex, setHoveredIndex] = useState<number>();
  const [windowStart, setWindowStart] = useState(Math.max(0, (view?.latestIndex ?? 0) - (view?.focusLength ?? 24) + 1));
  const [showAll, setShowAll] = useState(false);
  const plot = useRef<HTMLDivElement>(null);
  if (!view) return <p className="usage-scope">暂无趋势数据</p>;
  const dateLabel = (timestamp: number) => shortDate(timestamp, view.spansYears);
  const window = view.windowAt(windowStart, showAll ? view.count : view.focusLength);
  const activeIndex = hoveredIndex ?? selectedIndex;
  const selected = view.bucketAt(activeIndex);
  const start = window.buckets[0].start;
  const end = window.buckets[window.length - 1].end;
  const stateLabel = selected.total === 0 ? "无请求" : selected.tokens == null ? "用量未上报" : selected.tokens === 0 ? "已知用量为零" : "已上报用量合计";
  const jump = (index: number) => {
    setSelectedIndex(index);
    setHoveredIndex(undefined);
    if (!showAll) setWindowStart(Math.max(0, index - Math.floor(view.focusLength / 2)));
  };
  const moveWindow = (offset: number) => {
    const next = view.windowAt(offset, window.length);
    setWindowStart(next.offset);
    setHoveredIndex(undefined);
    setSelectedIndex(Math.max(next.offset, Math.min(next.offset + next.length - 1, selectedIndex)));
  };
  const describe = (bucket: ReturnType<typeof view.bucketAt>) => `${utc(bucket.start)} 至 ${utc(bucket.end)} UTC，不含结束时间，${bucket.total === 0 ? "无请求" : `${formatUsageNumber(bucket.total)} 次请求，${bucket.tokens == null ? "用量未上报" : `${formatUsageNumber(bucket.tokens)} Token`}`}`;
  return <div className="usage-timeline">
    <div className="usage-timeline-summary">
      <div className="usage-timeline-peak"><span>分组峰值</span><strong>{formatUsageNumber(view.peak)}<small>Token</small></strong>{view.peakIndex != null ? <button type="button" onClick={() => jump(view.peakIndex!)} aria-label="定位分组峰值">{dateLabel(view.bucketAt(view.peakIndex).start)} UTC <span aria-hidden="true">↗</span>{view.peakCount > 1 && <small> · {view.peakCount} 组并列</small>}</button> : <small>暂无已上报用量</small>}</div>
      <div className="usage-timeline-context"><span>每 {formatUsageNumber(stats.bucketMs / 3_600_000)} 小时一组</span><span>峰值与刻度基于所选全范围</span></div>
    </div>
    <div className="usage-timeline-window-heading"><div><strong>{showAll || view.count <= view.focusLength ? "全部时段" : "时段放大"}</strong><span>{dateLabel(start)} — {dateLabel(end)} UTC</span></div><div className="usage-timeline-actions">{view.count > view.focusLength && <button type="button" onClick={() => { setShowAll(!showAll); setWindowStart(Math.max(0, selectedIndex - Math.floor(view.focusLength / 2))); }}>{showAll ? "放大选中时段" : "查看全程"}</button>}<button type="button" onClick={() => { setShowAll(false); setSelectedIndex(view.latestIndex); setWindowStart(Math.max(0, view.latestIndex - view.focusLength + 1)); }}>最近请求</button></div></div>
    <div className="usage-timeline-chart">
      <div className="usage-timeline-scale" aria-hidden="true"><span>{compactNumber(view.peak)}</span><span>{compactNumber(view.peak == null ? null : view.peak / 2)}</span><span>0</span></div>
      <div className="usage-timeline-plot" ref={plot} onPointerLeave={() => setHoveredIndex(undefined)} role="group" aria-label="选择时间分组，左右方向键切换，Home 和 End 跳至范围两端" style={{ gridTemplateColumns: window.buckets.map((bucket) => `${bucket.end - bucket.start}fr`).join(" ") }} onKeyDown={(event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        setHoveredIndex(undefined);
        const next = event.key === "Home" ? 0 : event.key === "End" ? view.count - 1 : Math.max(0, Math.min(view.count - 1, selectedIndex + (event.key === "ArrowRight" ? 1 : -1)));
        setSelectedIndex(next);
        if (next < window.offset) setWindowStart(next);
        else if (next >= window.offset + window.length) setWindowStart(next - window.length + 1);
        requestAnimationFrame(() => plot.current?.querySelector<HTMLButtonElement>(`[data-bucket-index="${next}"]`)?.focus());
      }}>
        {window.buckets.map((bucket) => <button type="button" key={bucket.timestamp} data-bucket-index={bucket.index} className={`usage-timeline-bucket${bucket.index === activeIndex ? " is-selected" : ""}${bucket.total === 0 ? " is-empty" : bucket.tokens == null ? " is-unknown" : bucket.tokens === 0 ? " is-zero" : ""}`} tabIndex={bucket.index === selectedIndex ? 0 : -1} aria-label={describe(bucket)} aria-pressed={bucket.index === selectedIndex} onPointerEnter={(event) => { if (event.pointerType === "mouse") setHoveredIndex(bucket.index); }} onFocus={() => { setHoveredIndex(undefined); setSelectedIndex(bucket.index); }} onClick={() => setSelectedIndex(bucket.index)}>
          <span className="usage-timeline-bar" style={{ height: bucket.tokens != null && bucket.tokens > 0 ? `${bucket.tokens / Math.max(1, view.peak ?? 0) * 100}%` : undefined }} />
          {bucket.index === view.peakIndex && <span className="usage-timeline-peak-dot" aria-hidden="true" />}
        </button>)}
      </div>
    </div>
    <div className="usage-timeline-axis" aria-hidden="true"><span>{dateLabel(start)}</span><span>{dateLabel(start + (end - start) / 2)}</span><span>{dateLabel(end)}</span></div>
    <div className="usage-timeline-selection" aria-live="polite" aria-atomic="true"><div className="usage-timeline-selected-time"><span>{hoveredIndex == null ? "选中时段" : "预览时段"} · UTC</span><strong>{utc(selected.start)}<span>至 {utc(selected.end)}</span></strong><small>不含结束时间</small></div><div className="usage-timeline-selected-value"><span>Token</span><strong>{selected.total === 0 ? "—" : formatUsageNumber(selected.tokens)}</strong><small>{stateLabel}</small></div><div className="usage-timeline-selected-requests"><span>请求数</span><strong>{formatUsageNumber(selected.total)}</strong><small>所选时段保留的记录</small></div></div>
    <div className="usage-timeline-navigator"><div className="usage-timeline-navigator-heading"><span>全范围定位 <small>{window.length} / {view.count} 组</small></span><div><Button variant="ghost" size="xs" aria-label="上一段时间" disabled={showAll || window.offset === 0} onPress={() => moveWindow(window.offset - window.length)}><IconChevronLeft size={14} /></Button><Button variant="ghost" size="xs" aria-label="下一段时间" disabled={showAll || window.offset + window.length >= view.count} onPress={() => moveWindow(window.offset + window.length)}><IconChevronRight size={14} /></Button></div></div><div className="usage-timeline-minimap" aria-hidden="true"><svg viewBox="0 0 800 34" preserveAspectRatio="none">{whole.bars.map((bucket) => <rect key={bucket.timestampUnixMs} x={bucket.x} y={32 - (bucket.tokensUnknown ? 3 : Math.max(1, bucket.height / 128 * 29))} width={bucket.width} height={bucket.tokensUnknown ? 3 : Math.max(1, bucket.height / 128 * 29)} className={bucket.tokensUnknown ? "is-unknown" : ""} />)}</svg><span className="usage-timeline-window-indicator" style={{ left: `${(start - stats.fromUnixMs) / (stats.toUnixMs - stats.fromUnixMs) * 100}%`, width: `${(end - start) / (stats.toUnixMs - stats.fromUnixMs) * 100}%` }} /></div>{view.count > view.focusLength && <input type="range" className="usage-timeline-range" aria-label="移动时间窗口" aria-valuetext={`${dateLabel(start)} 至 ${dateLabel(end)} UTC`} min={0} max={view.count - window.length} step={1} value={window.offset} disabled={showAll} onChange={(event) => moveWindow(Number(event.target.value))} />}<div className="usage-timeline-range-labels"><span>{dateLabel(stats.fromUnixMs)}</span><span>{dateLabel(stats.toUnixMs)} UTC</span></div></div>
    <div className="usage-timeline-legend"><span><i className="is-known" />已上报</span><span><i className="is-unknown" />未上报</span><span><i className="is-zero" />已知零</span><span><i className="is-empty" />无请求</span><small>悬停预览 · 点击保留 · 方向键切换</small></div>
  </div>;
}
