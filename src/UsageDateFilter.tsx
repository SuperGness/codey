import { useState } from "react";
import { IconCalendar } from "@tabler/icons-react";
import { Button } from "./components/ui";

export type UsageDates = { start: string; end: string } | null;
type Preset = "today" | "7d" | "30d" | "month" | "all";
const presets: { value: Preset; label: string }[] = [
  { value: "today", label: "今天" },
  { value: "7d", label: "近 7 天" },
  { value: "30d", label: "近 30 天" },
  { value: "month", label: "本月" },
  { value: "all", label: "全部历史" },
];

const ISO_DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;

function pad2(value: number) {
  return String(value).padStart(2, "0");
}

function formatUtcDate(date: Date): string {
  return `${date.getUTCFullYear()}-${pad2(date.getUTCMonth() + 1)}-${pad2(date.getUTCDate())}`;
}

export function localTodayIsoDate(now = new Date()): string {
  return `${now.getFullYear()}-${pad2(now.getMonth() + 1)}-${pad2(now.getDate())}`;
}

function parseIsoDate(iso: string): [number, number, number] | null {
  const match = ISO_DATE_RE.exec(iso);
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const check = new Date(Date.UTC(year, month - 1, day));
  if (
    check.getUTCFullYear() !== year ||
    check.getUTCMonth() + 1 !== month ||
    check.getUTCDate() !== day
  ) {
    return null;
  }
  return [year, month, day];
}

export function addIsoDays(iso: string, deltaDays: number): string {
  const parsed = parseIsoDate(iso);
  if (!parsed) return iso;
  const [year, month, day] = parsed;
  return formatUtcDate(new Date(Date.UTC(year, month - 1, day + deltaDays)));
}

function zonedOffsetMs(utcMs: number, formatter: Intl.DateTimeFormat): number {
  const parts = formatter.formatToParts(new Date(utcMs));
  let year = 0;
  let month = 1;
  let day = 1;
  let hour = 0;
  let minute = 0;
  let second = 0;
  for (const part of parts) {
    if (part.type === "year") year = Number(part.value);
    else if (part.type === "month") month = Number(part.value);
    else if (part.type === "day") day = Number(part.value);
    else if (part.type === "hour") hour = Number(part.value) % 24;
    else if (part.type === "minute") minute = Number(part.value);
    else if (part.type === "second") second = Number(part.value);
  }
  return Date.UTC(year, month - 1, day, hour, minute, second) - utcMs;
}

function zonedMidnightUnixMs(iso: string, zone?: string): number {
  const parsed = parseIsoDate(iso);
  if (!parsed) return Number.NaN;
  const [year, month, day] = parsed;
  if (!zone) {
    return new Date(year, month - 1, day, 0, 0, 0, 0).getTime();
  }
  const formatter = new Intl.DateTimeFormat("en-US", {
    timeZone: zone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  });
  const targetUtc = Date.UTC(year, month - 1, day, 0, 0, 0);
  let guess = targetUtc - zonedOffsetMs(targetUtc, formatter);
  const refined = targetUtc - zonedOffsetMs(guess, formatter);
  if (refined !== guess) guess = refined;
  return guess;
}

export function usagePresetDates(preset: Preset, end = localTodayIsoDate()): UsageDates {
  if (preset === "all") return null;
  const parsed = parseIsoDate(end);
  if (!parsed) return null;
  const [year, month] = parsed;
  const start =
    preset === "today"
      ? end
      : preset === "month"
        ? `${year}-${pad2(month)}-01`
        : addIsoDays(end, preset === "7d" ? -6 : -29);
  return { start, end };
}

export function usageDatesQuery(dates: UsageDates, zone?: string) {
  if (!dates) return "all" as const;
  // 日历结束日包含全天；按当地零点转换，兼容夏令时造成的非 24 小时日期。
  return {
    fromUnixMs: zonedMidnightUnixMs(dates.start, zone),
    toUnixMs: zonedMidnightUnixMs(addIsoDays(dates.end, 1), zone),
  };
}

export function usageDatesLabel(dates: UsageDates) {
  return dates ? `${dates.start} — ${dates.end}` : "全部历史";
}

export function usageDatesWithinLimit(dates: UsageDates) {
  // 与后端按实际毫秒时长计算的范围上限一致。
  const query = usageDatesQuery(dates);
  return query === "all" || (query.toUnixMs > query.fromUnixMs && query.toUnixMs - query.fromUnixMs <= 366 * 86_400_000);
}

function clampDate(iso: string, maxDate: string): string {
  return iso > maxDate ? maxDate : iso;
}

export function UsageDateFilter({ value, onChange }: { value: UsageDates; onChange: (value: UsageDates) => void }) {
  const [error, setError] = useState("");
  const maxDate = localTodayIsoDate();
  const selectRange = (dates: UsageDates) => {
    if (!usageDatesWithinLimit(dates)) {
      setError("单次最多选择 366 天，可缩短范围或选择全部历史。");
      return;
    }
    setError("");
    onChange(dates);
  };
  const handleStartChange = (raw: string) => {
    if (!raw) {
      selectRange(null);
      return;
    }
    if (!parseIsoDate(raw)) return;
    const start = clampDate(raw, maxDate);
    const currentEnd = value?.end ? clampDate(value.end, maxDate) : maxDate;
    const end = currentEnd < start ? start : currentEnd;
    selectRange({ start, end });
  };
  const handleEndChange = (raw: string) => {
    if (!raw) {
      selectRange(null);
      return;
    }
    if (!parseIsoDate(raw)) return;
    const end = clampDate(raw, maxDate);
    const currentStart = value?.start ? clampDate(value.start, maxDate) : end;
    const start = currentStart > end ? end : currentStart;
    selectRange({ start, end });
  };
  const selectedPreset = presets.find(({ value: preset }) => {
    const dates = usagePresetDates(preset, maxDate);
    return !dates || !value ? dates === value : dates.start === value.start && dates.end === value.end;
  })?.value;

  return <div className="usage-date-filter">
    <div className="usage-date-field">
      <span className="usage-filter-label" id="usage-date-label">统计日期</span>
      <div
        className={`usage-date-inputs${error ? " is-invalid" : ""}`}
        role="group"
        aria-labelledby="usage-date-label"
        aria-describedby={error ? "usage-date-error" : undefined}
      >
        <IconCalendar size={15} className="usage-date-icon" aria-hidden="true" />
        <input
          type="date"
          className="usage-date-input"
          aria-label="开始日期"
          aria-invalid={Boolean(error) || undefined}
          max={value?.end && value.end < maxDate ? value.end : maxDate}
          value={value?.start ?? ""}
          onChange={(event) => handleStartChange(event.currentTarget.value)}
        />
        <span className="usage-date-separator" aria-hidden="true">—</span>
        <input
          type="date"
          className="usage-date-input"
          aria-label="结束日期"
          aria-invalid={Boolean(error) || undefined}
          min={value?.start ?? undefined}
          max={maxDate}
          value={value?.end ?? ""}
          onChange={(event) => handleEndChange(event.currentTarget.value)}
        />
      </div>
      {error && <p className="usage-date-error" id="usage-date-error" role="alert">{error}</p>}
    </div>
    <div className="usage-date-presets" role="group" aria-label="常用时间段">
      {presets.map(({ label, value: preset }) => <Button key={preset} size="xs" variant="ghost" aria-pressed={selectedPreset === preset} onPress={() => selectRange(usagePresetDates(preset, maxDate))}>{label}</Button>)}
    </div>
  </div>;
}
