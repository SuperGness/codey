import { DateField, DateRangePicker, RangeCalendar } from "@heroui/react";
import { useState } from "react";
import { CalendarDate, getLocalTimeZone, today } from "@internationalized/date";
import { IconCalendar, IconChevronDown } from "@tabler/icons-react";
import { Button } from "./components/ui";

export type UsageDates = { start: CalendarDate; end: CalendarDate } | null;
type Preset = "today" | "7d" | "30d" | "month" | "all";
const presets: { value: Preset; label: string }[] = [
  { value: "today", label: "今天" },
  { value: "7d", label: "近 7 天" },
  { value: "30d", label: "近 30 天" },
  { value: "month", label: "本月" },
  { value: "all", label: "全部历史" },
];

export function usagePresetDates(preset: Preset, end = today(getLocalTimeZone())): UsageDates {
  if (preset === "all") return null;
  const start = preset === "today" ? end : preset === "month" ? end.set({ day: 1 }) : end.subtract({ days: preset === "7d" ? 6 : 29 });
  return { start, end };
}

export function usageDatesQuery(dates: UsageDates, zone = getLocalTimeZone()) {
  if (!dates) return "all" as const;
  // 日历结束日包含全天；按当地零点转换，兼容夏令时造成的非 24 小时日期。
  return { fromUnixMs: dates.start.toDate(zone).getTime(), toUnixMs: dates.end.add({ days: 1 }).toDate(zone).getTime() };
}

export function usageDatesLabel(dates: UsageDates) {
  return dates ? `${dates.start.toString()} — ${dates.end.toString()}` : "全部历史";
}

export function usageDatesWithinLimit(dates: UsageDates) {
  // 与后端按实际毫秒时长计算的范围上限一致。
  const query = usageDatesQuery(dates);
  return query === "all" || (query.toUnixMs > query.fromUnixMs && query.toUnixMs - query.fromUnixMs <= 366 * 86_400_000);
}

export function UsageDateFilter({ value, onChange }: { value: UsageDates; onChange: (value: UsageDates) => void }) {
  const [error, setError] = useState("");
  const selectRange = (dates: UsageDates) => {
    if (!usageDatesWithinLimit(dates)) {
      setError("单次最多选择 366 天，可缩短范围或选择全部历史。");
      return;
    }
    setError("");
    onChange(dates);
  };
  const maxDate = today(getLocalTimeZone());
  const selectedPreset = presets.find(({ value: preset }) => {
    const dates = usagePresetDates(preset);
    return !dates || !value ? dates === value : dates.start.compare(value.start) === 0 && dates.end.compare(value.end) === 0;
  })?.value;

  return <div className="usage-date-filter">
    <div className="usage-date-field">
      <span className="usage-filter-label" id="usage-date-label">统计日期</span>
      <DateRangePicker aria-labelledby="usage-date-label" aria-describedby={error ? "usage-date-error" : undefined} isInvalid={Boolean(error)} value={value} maxValue={maxDate} onChange={selectRange} className="usage-date-picker">
        <DateField.Group className="usage-date-input-group">
          <DateRangePicker.Trigger className="usage-date-trigger" aria-label={`选择统计日期，当前${usageDatesLabel(value)}`}>
            <IconCalendar size={16} aria-hidden="true" />
            <span>{usageDatesLabel(value)}</span>
            <IconChevronDown size={14} aria-hidden="true" />
          </DateRangePicker.Trigger>
        </DateField.Group>
        <DateRangePicker.Popover className="usage-date-popover" placement="bottom start">
          <RangeCalendar aria-label="统计日期范围" firstDayOfWeek="mon" maxValue={maxDate}>
            <RangeCalendar.Header>
              <RangeCalendar.NavButton slot="previous" />
              <RangeCalendar.Heading />
              <RangeCalendar.NavButton slot="next" />
            </RangeCalendar.Header>
            <RangeCalendar.Grid>
              <RangeCalendar.GridHeader>{(day) => <RangeCalendar.HeaderCell>{day}</RangeCalendar.HeaderCell>}</RangeCalendar.GridHeader>
              <RangeCalendar.GridBody>{(date) => <RangeCalendar.Cell date={date} />}</RangeCalendar.GridBody>
            </RangeCalendar.Grid>
          </RangeCalendar>
          <p className="usage-calendar-hint">选择开始和结束日期，包含结束日</p>
        </DateRangePicker.Popover>
      </DateRangePicker>
      {error && <p className="usage-date-error" id="usage-date-error" role="alert">{error}</p>}
    </div>
    <div className="usage-date-presets" role="group" aria-label="常用时间段">
      {presets.map(({ label, value: preset }) => <Button key={preset} size="xs" variant="ghost" aria-pressed={selectedPreset === preset} onPress={() => selectRange(usagePresetDates(preset))}>{label}</Button>)}
    </div>
  </div>;
}
