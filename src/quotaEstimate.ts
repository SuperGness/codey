export const PRICING_SOURCE = "https://developers.openai.com/api/docs/pricing";
export const PRICING_CHECKED = "2026-09-09";
export const WEEK_MS = 7 * 86_400_000;
type Tier = "standard" | "fast" | "flex" | "batch";
// USD / million tokens: input, cache read, cache write, output; then optional long-context rates.
// null cache rates retain normal input billing. Missing tiers/long rates must never inherit another price.
type Rates = readonly [number, number | null, number | null, number, ...Array<number | null>];
export const MODEL_PRICES: Record<string, Partial<Record<Tier, Rates>>> = {
  // User adjustment: Astra cache reads are 2x the checked price across tiers and context lengths.
  "gpt-6-astra": {"standard":[10,2,12.5,50,20,4,25,75],"batch":[5,1,6.25,25,10,2,12.5,37.5],"flex":[5,1,6.25,25,10,2,12.5,37.5],"fast":[20,4,25,100,40,8,50,150]},
  "gpt-5.6-sol": {"standard":[4,0.4,5,20,8,0.8,10,30],"batch":[2,0.2,2.5,10,4,0.4,5,15],"flex":[2,0.2,2.5,10,4,0.4,5,15],"fast":[8,0.8,10,40,16,1.6,20,60]},
  "gpt-5.6-terra": {"standard":[2,0.2,2.5,12,4,0.4,5,18],"batch":[1,0.1,1.25,6,2,0.2,2.5,9],"flex":[1,0.1,1.25,6,2,0.2,2.5,9],"fast":[4,0.4,5,24,8,0.8,10,36]},
  "gpt-5.6-luna": {"standard":[0.2,0.02,0.25,1.2,0.4,0.04,0.5,1.8],"batch":[0.1,0.01,0.125,0.6,0.2,0.02,0.25,0.9],"flex":[0.1,0.01,0.125,0.6,0.2,0.02,0.25,0.9],"fast":[0.4,0.04,0.5,2.4,0.8,0.08,1,3.6]},
  "gpt-5.5": {"standard":[5,0.5,null,30,10,1,null,45],"batch":[2.5,0.25,null,15,5,0.5,null,22.5],"flex":[2.5,0.25,null,15,5,0.5,null,22.5],"fast":[12.5,1.25,null,75]},
  "gpt-5.5-pro": {"standard":[30,null,null,180,60,null,null,270],"batch":[15,null,null,90],"flex":[15,null,null,90]},
  "gpt-5.4": {"standard":[2.5,0.25,null,15,5,0.5,null,22.5],"batch":[1.25,0.13,null,7.5,2.5,0.25,null,11.25],"flex":[1.25,0.13,null,7.5,2.5,0.25,null,11.25],"fast":[5,0.5,null,30]},
  "gpt-5.4-mini": {"standard":[0.75,0.075,null,4.5],"batch":[0.375,0.0375,null,2.25],"flex":[0.375,0.0375,null,2.25],"fast":[1.5,0.15,null,9]},
  "gpt-5.4-nano": {"standard":[0.2,0.02,null,1.25],"batch":[0.1,0.01,null,0.625],"flex":[0.1,0.01,null,0.625]},
  "gpt-5.4-pro": {"standard":[30,null,null,180,60,null,null,270],"batch":[15,null,null,90,30,null,null,135],"flex":[15,null,null,90,30,null,null,135]},
  "gpt-5.2": {"standard":[1.75,0.175,null,14],"batch":[0.875,0.0875,null,7],"flex":[0.875,0.0875,null,7],"fast":[3.5,0.35,null,28]},
  "gpt-5.2-pro": {"standard":[21,null,null,168],"batch":[10.5,null,null,84]},
  "gpt-5.1": {"standard":[1.25,0.125,null,10],"batch":[0.625,0.0625,null,5],"flex":[0.625,0.0625,null,5],"fast":[2.5,0.25,null,20]},
  "gpt-5": {"standard":[1.25,0.125,null,10],"batch":[0.625,0.0625,null,5],"flex":[0.625,0.0625,null,5],"fast":[2.5,0.25,null,20]},
  "gpt-5-mini": {"standard":[0.25,0.025,null,2],"batch":[0.125,0.0125,null,1],"flex":[0.125,0.0125,null,1],"fast":[0.45,0.045,null,3.6]},
  "gpt-5-nano": {"standard":[0.05,0.005,null,0.4],"batch":[0.025,0.0025,null,0.2],"flex":[0.025,0.0025,null,0.2]},
  "gpt-5-pro": {"standard":[15,null,null,120],"batch":[7.5,null,null,60]},
  "gpt-4.1": {"standard":[2,0.5,null,8],"batch":[1,null,null,4],"fast":[3.5,0.875,null,14]},
  "gpt-4.1-mini": {"standard":[0.4,0.1,null,1.6],"batch":[0.2,null,null,0.8],"fast":[0.7,0.175,null,2.8]},
  "gpt-4.1-nano": {"standard":[0.1,0.025,null,0.4],"batch":[0.05,null,null,0.2],"fast":[0.2,0.05,null,0.8]},
  "gpt-4o": {"standard":[2.5,1.25,null,10],"batch":[1.25,null,null,5],"fast":[4.25,2.125,null,17]},
  "gpt-4o-2024-05-13": {"standard":[5,null,null,15],"batch":[2.5,null,null,7.5],"fast":[8.75,null,null,26.25]},
  "gpt-4o-mini": {"standard":[0.15,0.075,null,0.6],"batch":[0.075,null,null,0.3],"fast":[0.25,0.125,null,1]},
  "o1": {"standard":[15,7.5,null,60],"batch":[7.5,null,null,30]},
  "o1-pro": {"standard":[150,null,null,600],"batch":[75,null,null,300]},
  "o3-pro": {"standard":[20,null,null,80],"batch":[10,null,null,40]},
  "o3": {"standard":[2,0.5,null,8],"batch":[1,null,null,4],"flex":[1,0.25,null,4],"fast":[3.5,0.875,null,14]},
  "o4-mini": {"standard":[1.1,0.275,null,4.4],"batch":[0.55,null,null,2.2],"flex":[0.55,0.138,null,2.2],"fast":[2,0.5,null,8]},
  "o3-mini": {"standard":[1.1,0.55,null,4.4],"batch":[0.55,null,null,2.2]},
  "gpt-4-turbo-2024-04-09": {"standard":[10,null,null,30],"batch":[5,null,null,15]},
  "gpt-4-0613": {"standard":[30,null,null,60],"batch":[15,null,null,30]},
  "gpt-3.5-turbo": {"standard":[0.5,null,null,1.5]},
  "gpt-3.5-turbo-0125": {"standard":[0.5,null,null,1.5],"batch":[0.25,null,null,0.75]},
  "gpt-3.5-turbo-1106": {"standard":[1,null,null,2],"batch":[1,null,null,2]},
  "gpt-3.5-turbo-instruct": {"standard":[1.5,null,null,2]},
  "gpt-5.3-codex": {"standard":[1.75,0.175,null,14],"fast":[3.5,0.35,null,28]},
  "gpt-5.2-codex": {"standard":[1.75,0.175,null,14]},
  "gpt-5.1-codex": {"standard":[1.25,0.125,null,10]},
  "gpt-5.1-codex-max": {"standard":[1.25,0.125,null,10]},
  "gpt-5.1-codex-mini": {"standard":[0.25,0.025,null,2]},
  "gpt-5-codex": {"standard":[1.25,0.125,null,10]},
  "codex-mini-latest": {"standard":[1.5,0.375,null,6]},
  "chat-latest": {"standard":[5,0.5,null,30]},
  "gpt-5-search-api": {"standard":[1.25,0.125,null,10]},
  "gpt-5.6-cyber": {"standard":[12.5,1.25,15.625,75]},
  "gpt-5.5-cyber": {"standard":[12.5,1.25,null,75]},
};

export type QuotaUsage = {
  model?: string | null; requestedModel?: string;
  serviceTier?: string | null; requestedServiceTier?: string | null;
  inputTokens?: number | null; outputTokens?: number | null; totalTokens?: number | null;
  cachedInputTokens?: number | null; cacheCreationInputTokens?: number | null;
};
const numericFields = ["calls", "input", "output", "tokens", "hits", "cached", "writes",
  "inputCost", "outputCost", "readCost", "writeCost", "cacheCost", "cacheSaving",
  "cost", "unpriced", "missing", "missingWrites", "assumed"] as const;
export type QuotaRow = Record<typeof numericFields[number], number> & {
  key: string; model: string; tier: string; context: string; source: string; note: string;
};
export function emptyQuotaRow(model = "合计"): QuotaRow {
  return { key: model, model, tier: "", context: "", source: "", note: "",
    calls: 0, input: 0, output: 0, tokens: 0, hits: 0, cached: 0, writes: 0,
    inputCost: 0, outputCost: 0, readCost: 0, writeCost: 0,
    cacheCost: 0, cacheSaving: 0, cost: 0, unpriced: 0, missing: 0, missingWrites: 0, assumed: 0 };
}
const count = (value: number | null | undefined) =>
  typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.trunc(value) : 0;
function normalizeTier(value: string): Tier | null {
  switch (value.trim().toLowerCase()) {
    case "default": case "standard": return "standard";
    case "priority": case "fast": return "fast";
    case "flex": return "flex";
    case "batch": return "batch";
    default: return null;
  }
}
const tierLabels: Record<Tier, string> = { standard: "Standard", fast: "Fast", flex: "Flex", batch: "Batch" };

export function addQuotaUsage(rows: Map<string, QuotaRow>, item: QuotaUsage) {
  const model = item.model?.trim() || item.requestedModel?.trim() || "未知模型";
  const modelKey = model.toLowerCase();
  const priceKey = Object.prototype.hasOwnProperty.call(MODEL_PRICES, modelKey) ? modelKey : modelKey.replace(/-\d{4}-\d{2}-\d{2}$/, "");
  const prices = Object.prototype.hasOwnProperty.call(MODEL_PRICES, priceKey) ? MODEL_PRICES[priceKey] : undefined;
  const actual = item.serviceTier?.trim() || "";
  const requested = item.requestedServiceTier?.trim() || "";
  // An explicit unknown actual tier also takes precedence: it must not fall back to the requested tier.
  const rawTier = actual || requested;
  const defaulted = !rawTier || rawTier.toLowerCase() === "auto";
  const tier = defaulted ? "standard" : normalizeTier(rawTier);
  const source = defaulted ? "默认档位（未记录）" : actual ? "响应确认" : tier ? "按请求档位推定" : "档位未确认";
  const input = count(item.inputTokens), output = count(item.outputTokens);
  const cached = count(item.cachedInputTokens), writes = count(item.cacheCreationInputTokens);
  // Context thresholds belong to the model, including tiers whose long-context price is unavailable.
  const hasLongRates = Object.values(prices ?? {}).some(rates => rates.length > 4);
  const long = hasLongRates && input > 272_000;
  const context = hasLongRates ? long ? ">272K" : "≤272K" : "统一上下文价";
  const tierLabel = tier ? tierLabels[tier] : rawTier ? `未确认（${rawTier}）` : "未确认";
  const key = JSON.stringify([modelKey, tierLabel, context, source]);
  const row = rows.get(key) ?? { ...emptyQuotaRow(model), key, tier: tierLabel, context, source };
  rows.set(key, row);
  row.calls++; row.input += input; row.output += output; row.tokens += count(item.totalTokens);
  row.hits += Number(cached > 0); row.cached += cached; row.writes += writes;
  row.missingWrites += Number(item.cacheCreationInputTokens == null);
  row.missing += Number(item.inputTokens == null || item.outputTokens == null || item.totalTokens == null);
  const rates = tier ? prices?.[tier] : undefined;
  row.note = !tier ? "缺少可确认的计费档位" : !rates ? "该模型及档位无已核对价格"
    : long && rates.length < 8 ? "该档位未公布长上下文价格" : "";
  if (row.note) { row.unpriced++; return; }
  if (!rates) return;
  row.assumed += Number(!actual || defaulted);
  const offset = long ? 4 : 0;
  const inputRate = rates[offset]!;
  const cachedRate = rates[offset + 1] ?? inputRate;
  const writeRate = rates[offset + 2] ?? inputRate;
  const outputRate = rates[offset + 3]!;
  // Cache reads/writes are portions of input; excess malformed counts cannot create extra charges.
  const billedCached = Math.min(input, cached);
  const billedWrites = Math.min(input - billedCached, writes);
  const inputCost = (input - billedCached - billedWrites) * inputRate / 1_000_000;
  const outputCost = output * outputRate / 1_000_000;
  const readCost = billedCached * cachedRate / 1_000_000;
  const writeCost = billedWrites * writeRate / 1_000_000;
  const subtotal = inputCost + outputCost + readCost + writeCost;
  row.inputCost += inputCost; row.outputCost += outputCost;
  row.readCost += readCost; row.writeCost += writeCost;
  row.cacheCost += readCost + writeCost;
  row.cacheSaving += ((billedCached + billedWrites) * inputRate / 1_000_000) - readCost - writeCost;
  row.cost += subtotal;
}

export function sumQuotaRows(rows: QuotaRow[]) {
  const total = emptyQuotaRow();
  for (const row of rows) for (const key of numericFields) total[key] += row[key];
  return total;
}
export type AccountUsageSnapshot = {
  status: string; message?: string; fetchedAt?: number; stale?: boolean;
  primary?: { usedPercent: number; windowMinutes: number; resetsAt?: number } | null;
  secondary?: { usedPercent: number; windowMinutes: number; resetsAt?: number } | null;
};
export type QuotaPeriod = { fromUnixMs: number; toUnixMs: number; resetsAt: number; usedPercent: number };
export function quotaPeriod(snapshot: AccountUsageSnapshot, now = Date.now()): QuotaPeriod {
  if (snapshot?.status !== "ok") throw new Error(snapshot?.message || "官方周额度暂不可用，请刷新后重试。");
  const weekly = [snapshot.primary, snapshot.secondary].find(window => window?.windowMinutes === 10080);
  if (!weekly) throw new Error("官方接口未返回周额度窗口，暂时无法估算周限。");
  const { usedPercent, resetsAt } = weekly;
  if (typeof usedPercent !== "number" || !Number.isFinite(usedPercent) || usedPercent < 0 || usedPercent > 100) {
    throw new Error("官方周额度使用比例无效，请刷新数据后重试。");
  }
  const end = Number(resetsAt) * 1000;
  const fetchedAt = Number(snapshot.fetchedAt) * 1000;
  const start = end - WEEK_MS;
  if (!Number.isFinite(end) || !Number.isFinite(fetchedAt) || start < 0 || fetchedAt <= start
    || fetchedAt >= end || fetchedAt > now + 1000 || end <= now) {
    throw new Error("官方周额度重置时间或更新时间无效、已过期，请刷新数据后重试。");
  }
  return { fromUnixMs: start, toUnixMs: fetchedAt, resetsAt: end, usedPercent };
}
export function projectQuota(cost: number, durationMs: number, usedPercent: number) {
  if (!Number.isFinite(durationMs) || durationMs <= 0 || !Number.isFinite(cost) || cost < 0) {
    throw new Error("统计周期或额度数据无效，请刷新数据后重试。");
  }
  const weekly = cost * WEEK_MS / durationMs;
  if (!Number.isFinite(usedPercent) || usedPercent < 0 || usedPercent > 100) {
    throw new Error("官方周额度使用比例无效，请刷新数据后重试。");
  }
  const limit = usedPercent > 0 ? cost / (usedPercent / 100) : null;
  if (![weekly, limit].every(value => value == null || Number.isFinite(value))) {
    throw new Error("计算结果超出范围，请调整统计周期或使用百分比。");
  }
  return { weekly, limit, remaining: limit == null ? null : limit - cost };
}

type Cursor = { timestampUnixMs: number; requestId: string };
export type QuotaPage = { queryable: boolean; items: QuotaUsage[]; hasMore: boolean; nextCursor: Cursor | null };
export async function loadQuotaRows(
  query: (cursor: Cursor | null) => Promise<QuotaPage>,
  active: () => boolean,
  progress: (count: number) => void,
) {
  const rows = new Map<string, QuotaRow>();
  let cursor: Cursor | null = null, loaded = 0;
  // ponytail: sequential 100-row pages reuse the existing API; move aggregation to SQL if large histories become slow.
  while (active()) {
    const page = await query(cursor);
    if (!active()) return null;
    if (!page.queryable) throw new Error("当前日志暂不可查询，请开启请求日志记录后重试。");
    for (const item of page.items) addQuotaUsage(rows, item);
    loaded += page.items.length; progress(loaded);
    if (!page.hasMore) return [...rows.values()].sort((a, b) => b.cost - a.cost || a.key.localeCompare(b.key));
    const next = page.nextCursor;
    if (!next || !page.items.length || (cursor && (next.timestampUnixMs > cursor.timestampUnixMs
      || (next.timestampUnixMs === cursor.timestampUnixMs && next.requestId >= cursor.requestId)))) {
      throw new Error("日志分页异常，请刷新数据后重试。");
    }
    cursor = next;
  }
  return null;
}
