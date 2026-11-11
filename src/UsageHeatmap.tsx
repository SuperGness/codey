import { useMemo, useRef, useState } from "react";
import { Button, Select, Tooltip } from "./components/ui";
import { formatUsageNumber, usageHeatmap, type LogAnalytics } from "./usageAnalysis";

const dateLabel = (timestamp: number) => new Date(timestamp).toISOString().slice(0, 10);
const describeDay = (day: ReturnType<typeof usageHeatmap>["days"][number]) =>
  `${dateLabel(day.timestampUnixMs)} UTC · ${day.total === 0 ? "无请求" : `${formatUsageNumber(day.totalTokensSum)} Token · ${formatUsageNumber(day.total)} 次请求${day.totalTokensKnownCount < day.total ? ` · ${formatUsageNumber(day.total - day.totalTokensKnownCount)} 次未上报` : ""}`}${day.partial ? " · 仅含所选时间范围" : ""}`;

export function UsageHeatmap({ stats }: { stats: LogAnalytics }) {
  const [selectedYear, setSelectedYear] = useState<number>();
  const [selectedDay, setSelectedDay] = useState<number>();
  const [focusIndex, setFocusIndex] = useState(0);
  const grid = useRef<HTMLDivElement>(null);
  const map = useMemo(() => usageHeatmap(stats, selectedYear), [stats, selectedYear]);
  const compact = map.days.length <= 35;
  const offset = map.days.length ? (new Date(map.days[0].timestampUnixMs).getUTCDay() + 6) % 7 : 0;
  const cells = useMemo(
    () =>
      map.days.map((day) => {
        const date = new Date(day.timestampUnixMs);
        return {
          day,
          label: describeDay(day),
          utcDate: date.getUTCDate(),
          utcMonth: date.getUTCMonth() + 1,
        };
      }),
    [map.days],
  );
  const selected = cells.find(({ day }) => day.timestampUnixMs === selectedDay);
  if (!map.available) return <p className="usage-scope">暂无每日数据，请刷新或切换到直方图查看趋势。</p>;
  return <div className="usage-heatmap">
    <div className="usage-heatmap-heading"><span>每日用量 <small>UTC · 颜色越深，用量越高</small></span>{map.years.length > 1 && <Select className="usage-heatmap-year" aria-label="热力图年份" value={map.year} optionList={map.years.map((year) => ({ value: year, label: `${year} 年` }))} onChange={(year) => { setSelectedYear(Number(year)); setSelectedDay(undefined); }} />}</div>
    <div className="usage-heatmap-scroll" tabIndex={compact ? undefined : 0} aria-label={compact ? undefined : "每日用量日历，可横向滚动"}>
      <div className={compact ? "usage-heatmap-compact" : "usage-heatmap-calendar"}>
        {!compact && <div className="usage-heatmap-weekdays" aria-hidden="true">{["一", "", "三", "", "五", "", "日"].map((label, index) => <span key={index}>{label}</span>)}</div>}
        <div ref={grid} className="usage-heatmap-days" style={compact ? undefined : { gridTemplateColumns: `repeat(${Math.ceil((offset + map.days.length) / 7)}, 14px)` }} onKeyDown={(event) => {
          const step = event.key === "ArrowRight" ? (compact ? 1 : 7) : event.key === "ArrowLeft" ? (compact ? -1 : -7) : event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
          if (!step && event.key !== "Home" && event.key !== "End") return;
          event.preventDefault();
          const next = event.key === "Home" ? 0 : event.key === "End" ? map.days.length - 1 : Math.max(0, Math.min(map.days.length - 1, focusIndex + step));
          setFocusIndex(next);
          grid.current?.querySelectorAll<HTMLButtonElement>(".usage-heatmap-cell")[next]?.focus();
        }}>
          {!compact && Array.from({ length: offset }, (_, index) => <span className="usage-heatmap-spacer" key={`empty-${index}`} />)}
          {cells.map(({ day, label, utcDate, utcMonth }, index) => <div className="usage-heatmap-day" key={day.timestampUnixMs}>
            {!compact && (utcDate === 1 || index === 0) && <span className="usage-heatmap-month" aria-hidden="true" style={{ top: `${-24 - (index + offset) % 7 * 19}px` }}>{utcMonth}月</span>}
            <Tooltip delay={100} content={<span>{label}</span>}><Button variant="ghost" className={`usage-heatmap-cell usage-heat-${day.level}`} aria-label={label} aria-pressed={selectedDay === day.timestampUnixMs} excludeFromTabOrder={index !== Math.min(focusIndex, map.days.length - 1)} onFocus={() => setFocusIndex(index)} onPress={() => setSelectedDay(day.timestampUnixMs)}>{compact ? <span aria-hidden="true">{utcDate}</span> : null}</Button></Tooltip>
            {compact && <span className="usage-heatmap-date" aria-hidden="true">{utcMonth}/{utcDate}</span>}
          </div>)}
        </div>
      </div>
    </div>
    <div className="usage-heatmap-footer"><span>{map.days.length} 天 · 单日最高 {formatUsageNumber(map.peak)} Token</span><div className="usage-heatmap-key" aria-label="颜色图例"><span>少</span>{[1, 2, 3, 4].map((level) => <i key={level} className={`usage-heat-${level}`} />)}<span>多</span><i className="usage-heat-empty" /><span>无请求</span><i className="usage-heat-zero" /><span>零</span><i className="usage-heat-unknown" /><span>未上报</span></div></div>
    <p className="usage-scope usage-heatmap-selection" aria-live="polite">{selected ? selected.label : "悬停或聚焦方格查看用量，方向键切换日期，点击可保留信息。首尾日期仅统计所选范围内的请求。"}</p>
  </div>;
}
