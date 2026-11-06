// 时间图的悬停会反复重绘；复用格式器，避免为每个时间分组重新加载区域格式。
const utcMinuteOptions: Intl.DateTimeFormatOptions = {
  timeZone: "UTC", month: "2-digit", day: "2-digit",
  hour: "2-digit", minute: "2-digit", hour12: false,
};
const utcMinuteFormatter = new Intl.DateTimeFormat("zh-CN", { ...utcMinuteOptions, year: "numeric" });
const utcShortMinuteFormatter = new Intl.DateTimeFormat("zh-CN", utcMinuteOptions);
const compactNumberFormatter = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });

export function formatUsageUtcMinute(timestamp: number): string {
  return utcMinuteFormatter.format(timestamp);
}

export function formatUsageShortDate(timestamp: number, includeYear: boolean): string {
  return (includeYear ? utcMinuteFormatter : utcShortMinuteFormatter).format(timestamp);
}

export function formatCompactUsageNumber(value: number | null): string {
  return value == null ? "—" : compactNumberFormatter.format(value);
}
