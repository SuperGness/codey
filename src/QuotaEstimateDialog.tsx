import { useEffect, useState } from "react";
import { Alert, Spinner, Table } from "@heroui/react";
import { IconRefresh } from "@tabler/icons-react";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import { formatTimestamp } from "./formatters";
import { Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./components/ui";
import { loadQuotaRows, PRICING_CHECKED, PRICING_SOURCE, projectQuota, quotaPeriod, sumQuotaRows } from "./quotaEstimate";
import type { AccountUsageSnapshot, QuotaPage, QuotaPeriod, QuotaRow } from "./quotaEstimate";

declare global {
  interface Window {
    __codeyReadQuotaAccountUsage?: (options: { forceRefresh: boolean }) => Promise<AccountUsageSnapshot>;
  }
}
// Share only in-flight reads, including React's development effect replay.
let usageRequest: Promise<AccountUsageSnapshot> | null = null;
function readAccountUsage(forceRefresh: boolean) {
  usageRequest ??= (window.__codeyReadQuotaAccountUsage
    ? window.__codeyReadQuotaAccountUsage({ forceRefresh })
    : invoke<AccountUsageSnapshot>("query_official_account_usage", { forceRefresh }))
    .finally(() => { usageRequest = null; });
  return usageRequest;
}
const integer = (value: number) => value.toLocaleString("en-US", { maximumFractionDigits: 0 });
const money = (value: number | null) => value == null ? "—" : value.toLocaleString("en-US", {
  style: "currency", currency: "USD", minimumFractionDigits: 4, maximumFractionDigits: 4,
});

export function QuotaEstimateDialog({ container, onClose }: {
  container: HTMLElement | null; onClose: () => void;
}) {
  const [period, setPeriod] = useState<QuotaPeriod | null>(null);
  const [revision, setRevision] = useState(0);
  const [rows, setRows] = useState<QuotaRow[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(0);
  const [error, setError] = useState("");
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);
  const [healthWarning, setHealthWarning] = useState(false);
  const [usageWarning, setUsageWarning] = useState("");
  const provider = "openai";
  useEffect(() => {
    let active = true;
    setLoading(true); setRows(null); setPeriod(null); setError(""); setLoaded(0); setUpdatedAt(null); setHealthWarning(false);
    setUsageWarning("");
    void (async () => {
      try {
        const snapshot = await readAccountUsage(revision > 0);
        if (!active) return;
        setUsageWarning(snapshot.stale ? snapshot.message || "当前使用上次成功获取的官方额度，统计截止时间保持不变。" : "");
        const currentPeriod = quotaPeriod(snapshot);
        const { fromUnixMs, toUnixMs } = currentPeriod;
        setPeriod(currentPeriod);
        const stats = await invoke<{ queryable: boolean; recordingHealth?: {
          active: boolean; sampleRatePerMillion: number; droppedFull: number; droppedClosed: number;
          writeDropped: number; writeFailures: number;
        } }>("query_route_request_log_stats", { provider, fromUnixMs, toUnixMs });
        if (!active) return;
        if (!stats.queryable) throw new Error("当前日志暂不可查询，请开启请求日志记录后重试。");
        const health = stats.recordingHealth;
        setHealthWarning(Boolean(health && (!health.active || health.sampleRatePerMillion < 1_000_000
          || health.droppedFull + health.droppedClosed + health.writeDropped + health.writeFailures > 0)));
        const next = await loadQuotaRows(cursor => invoke<QuotaPage>("query_route_request_logs", {
          provider, fromUnixMs, toUnixMs, cursorMode: true, cursor, pageSize: 100,
        }), () => active, setLoaded);
        if (active && next) { setRows(next); setUpdatedAt(Date.now()); }
      } catch (cause) {
        if (active) setError(`无法完成额度估算：${errorText(cause)}`);
      } finally { if (active) setLoading(false); }
    })();
    return () => { active = false; };
  }, [provider, revision]);

  const total = sumQuotaRows(rows ?? []);
  let calculationError = "";
  let result: ReturnType<typeof projectQuota> | null = null;
  try {
    if (period && rows && total.calls > total.unpriced) {
      result = projectQuota(total.cost, period.toUnixMs - period.fromUnixMs, period.usedPercent);
    }
  }
  catch (cause) { calculationError = cause instanceof Error ? cause.message : "计算失败，请刷新后重试。"; }
  const warnings = [
    usageWarning,
    healthWarning && "日志记录未完整开启或存在采样、丢弃及写入异常，估算仅覆盖已记录的请求。",
    rows && (total.unpriced > 0 || total.missing > 0) && `${integer(total.unpriced)} 次请求因档位或费率缺失未计价；${integer(total.missing)} 次请求缺少 Token 数据，缺失值按 0，结果可能偏低。`,
    rows && total.assumed > 0 && `${integer(total.assumed)} 次请求未经响应确认档位，按请求档位或默认 Standard 估算，并与已确认用量分开。`,
    rows && total.missingWrites > 0 && `${integer(total.missingWrites)} 次请求未记录缓存写入量，按 0 展示；对应输入仍按普通输入价计费，额外写入费用可能未计入。`,
  ].filter(Boolean);
  const metrics = (items: ReadonlyArray<readonly [string, string, string?]>) => <dl className="m-0 grid gap-0.5 text-xs tabular-nums">
    {items.map(([label, value, colorClass]) => <div key={label} className="flex items-center justify-between gap-2">
      <dt className="font-normal text-gray-500">{label}</dt>
      <dd className={`m-0 font-medium whitespace-nowrap ${colorClass ?? "text-gray-800"}`}>{value}</dd>
    </div>)}
  </dl>;
  const columns = [
    { title: "模型名称", key: "model", width: 155, render: (_: unknown, row: QuotaRow) => <div className="break-words">
      <div className="font-semibold text-gray-900">{row.model}</div>
      {row.unpriced > 0 && <div className="mt-0.5 text-[11px] font-medium text-amber-600">{row.note}，未计价</div>}
    </div> },
    { title: "档位 / 上下文", key: "rules", width: 155, render: (_: unknown, row: QuotaRow) => <div className="text-xs">
      <div className="font-medium text-gray-800">{row.tier}{row.context && ` · ${row.context}`}</div>
      <div className="mt-0.5 text-gray-500">{row.source}</div>
    </div> },
    { title: "调用次数", key: "calls", width: 85, align: "right" as const,
      render: (_: unknown, row: QuotaRow) => <span className="font-medium tabular-nums text-gray-800">{integer(row.calls)}</span> },
    { title: "Token 用量", key: "tokens", width: 175, render: (_: unknown, row: QuotaRow) => metrics([
      ["输入", integer(row.input)], ["输出", integer(row.output)], ["总计", integer(row.tokens)],
    ]) },
    { title: "缓存用量", key: "cache", width: 185, render: (_: unknown, row: QuotaRow) => <>
      {metrics([["命中次数", integer(row.hits)], ["读取 Token", integer(row.cached)], ["写入 Token", integer(row.writes)]])}
      {row.missingWrites > 0 && <div className="mt-1 text-[11px] font-medium text-amber-600">{integer(row.missingWrites)} 次未记录写入量</div>}
    </> },
    { title: "费用明细 (USD)", key: "fees", width: 210, render: (_: unknown, row: QuotaRow) => metrics(([
      ["inputCost", "非缓存输入"], ["outputCost", "输出"], ["readCost", "缓存读取"],
      ["writeCost", "缓存写入"], ["cacheSaving", "缓存节省（参考）"],
    ] as const).map(([key, label]) => [
      label,
      money(row.calls > 0 && row.unpriced === row.calls ? null : row[key]),
      key === "cacheSaving" && (row.cacheSaving ?? 0) > 0 ? "text-emerald-600 font-semibold" : undefined,
    ])) },
    { title: "额度估算 (USD)", key: "estimate", width: 215, render: (_: unknown, row: QuotaRow) => metrics([
      ["当前已消耗", money(row.calls > 0 && row.unpriced === row.calls ? null : row.cost), "text-blue-600 font-bold"],
      ["折算周限份额", money(result?.limit == null || !period || row.unpriced === row.calls && row.calls > 0 ? null : row.cost / (period.usedPercent / 100))],
      ["预计周消耗", money(result && period && !(row.calls > 0 && row.unpriced === row.calls) ? row.cost * 7 * 86_400_000 / (period.toUnixMs - period.fromUnixMs) : null)],
      ["占总消耗", row.calls > 0 && row.unpriced === row.calls ? "—" : `${(total.cost > 0 ? row.cost / total.cost * 100 : 0).toFixed(2)}%`, "text-blue-600 font-semibold"],
    ]) },
  ];
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent container={container} className="quota-estimate-dialog w-full sm:w-[min(1240px,calc(100vw-32px))] max-w-[min(1240px,calc(100vw-32px))]">
      <DialogHeader>
        <div className="flex flex-col gap-0.5">
          <DialogTitle className="text-lg font-bold text-gray-900">周限额度估算</DialogTitle>
          <DialogDescription className="text-xs text-gray-500">{period
            ? `本次统计：${formatTimestamp(period.fromUnixMs)} 至 ${formatTimestamp(period.toUnixMs)}（不含结束时间）`
            : "自动读取官方周额度，按上次重置以来的用量估算周限。"}</DialogDescription>
        </div>
      </DialogHeader>
      <div className="mt-3 flex min-w-0 flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-3 py-1 text-xs">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-gray-600">
            <div className="flex items-center gap-1.5 font-medium text-gray-800">
              <span>官方周额度已使用：</span>
              <span className="inline-flex items-center px-2 py-0.5 rounded-md font-semibold bg-blue-50 text-blue-600 border border-blue-200/60 tabular-nums">
                {period ? `${period.usedPercent.toFixed(2)}%` : loading ? "正在读取…" : "暂不可用"}
              </span>
            </div>
            {period && <>
              <span className="text-gray-300">·</span>
              <span>上次重置（推算）：<span className="text-gray-800 font-medium">{formatTimestamp(period.fromUnixMs)}</span></span>
              <span className="text-gray-300">·</span>
              <span>下次重置：<span className="text-gray-800 font-medium">{formatTimestamp(period.resetsAt)}</span></span>
              <span className="text-gray-300">·</span>
              <span>更新时间：<span className="text-gray-800 font-medium">{formatTimestamp(period.toUnixMs)}</span></span>
            </>}
          </div>
          <Button
            variant="outline"
            size="sm"
            className="h-7 shrink-0 gap-1.5 px-3 text-xs font-medium text-blue-600 border-blue-200 hover:bg-blue-50/80 transition-colors"
            disabled={loading}
            onClick={() => setRevision(value => value + 1)}
          >
            <IconRefresh size={13} className={loading ? "animate-spin" : ""} aria-hidden="true" />
            刷新数据
          </Button>
        </div>
        <Alert className="px-3.5 py-2.5" status={error || calculationError ? "danger" : warnings.length ? "warning" : "accent"}>
          <Alert.Indicator />
          <Alert.Content>
          <Alert.Title><span className="text-xs font-medium text-amber-900">按 OpenAI 各档位 API 单价估算等值金额（USD），不代表订阅实际扣费或官方周限。</span></Alert.Title>
          <Alert.Description><div className="text-xs leading-relaxed text-amber-800">
            {(error || calculationError) && <p className="m-0 font-medium text-red-600">{error || calculationError}</p>}
            {period?.usedPercent === 0 && <p className="m-0">官方周额度已用比例为 0%，暂时无法反推周限及剩余额度；产生用量后可刷新重算。</p>}
            {warnings.length > 0 && <p className="m-0">{warnings.join(" ")}</p>}
            <details className="mt-1">
              <summary className="cursor-pointer select-none font-medium text-amber-900 hover:text-amber-950 transition-colors">计算说明与价格来源</summary>
                <div className="mt-2 grid gap-1.5 border-t border-amber-200/60 pt-2 text-xs text-gray-600 leading-relaxed">
                  <p className="m-0">金额统一保留 4 位小数；Token 和调用次数取整数；比例保留 2 位小数。计算时使用未四舍五入的金额。</p>
                  <p className="m-0">当前消耗 =（非缓存输入 × 输入单价 + 缓存读取 × 读取单价 + 缓存写入 × 写入单价 + 输出 × 输出单价）÷ 1,000,000。各费用单独展示，推理 Token 已包含在输出中，不重复计费。</p>
                  <p className="m-0">预估周限 = 本周期已记录消耗 ÷ 官方周额度已用比例；当前预估剩余 = 预估周限 − 本周期已消耗。模型的折算周限份额按同一比例分摊。预计周消耗 = 本周期消耗 × 7 天 ÷ 已统计时长，仅表示按当前速度推算的整周消耗，不参与周限反推。</p>
                  <p className="m-0">Standard、Fast（含 priority）、Flex、Batch 各用独立价表，响应档位优先于请求档位；请求 Fast 而响应 default 按 Standard 计价。只有请求档位时单独列为推定；未记录计费档位或仅记录 auto 时按默认 Standard 档位计价，并标明默认依据。</p>
                  <p className="m-0">适用模型单次输入超过 272K 时，整次请求使用该档位的长上下文价，表格与短上下文分开。未公布价格的组合不借用其他档位价格。</p>
                  <p className="m-0">估算仅覆盖本线路已记录的 Token，不按采样率补推。官方已用比例可能包含其他设备用量，缺失日志或跨账号历史会影响推算准确性。工具调用、搜索内容特殊计价、容器和存储等缺少完整计费数据，尚未计入。合计中的未计价请求不代表实际免费。</p>
                  <p className="m-0">价格核对：{PRICING_CHECKED} · <a className="text-blue-600 hover:underline font-medium" href={PRICING_SOURCE} target="_blank" rel="noreferrer">OpenAI 官方价格</a>；Codex 历史模型价格见对应官方模型页。GPT-5.6 Sol 使用当前公开促销价。</p>
                </div>
            </details>
          </div></Alert.Description>
          </Alert.Content>
        </Alert>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3" aria-live="polite">
          {([
            ["当前预计周额度消耗", result?.weekly],
            ["预估周限", result?.limit],
            ["当前预估剩余额度", result?.remaining],
          ] as const).map(([label, value]) => <div key={label} className="flex flex-col justify-between rounded-xl border border-gray-200/90 bg-white p-3.5 shadow-xs">
            <div className="flex items-center justify-between text-xs text-gray-500">
              <span className="font-medium">{label}</span>
              {total.unpriced > 0 && <span className="rounded bg-amber-50 px-1.5 py-0.5 text-[11px] font-normal text-amber-700 border border-amber-200/60">仅含可计价用量</span>}
            </div>
            <div className="mt-1.5 flex items-baseline gap-1">
              <span className="text-2xl font-bold tracking-tight text-blue-600 tabular-nums">{money(value ?? null)}</span>
              <span className="text-xs font-semibold text-blue-500">USD</span>
            </div>
          </div>)}
        </div>
        {loading && <div role="status" className="flex items-center gap-2 text-xs text-gray-500">
          <Spinner size="sm" />
          <span>{period ? `正在读取本周统计，已加载 ${integer(loaded)} 次请求…` : "正在读取官方周额度…"}</span>
        </div>}
        <Table className="quota-estimate-table relative" variant="secondary" aria-busy={loading}>
          <Table.ScrollContainer className="max-h-[420px] overflow-auto rounded-xl border border-gray-200 bg-white shadow-2xs">
            <Table.Content aria-label="模型额度明细" className={`min-w-[1180px] ${loading ? "opacity-60" : ""}`}>
              <Table.Header>
                {columns.map((column, index) => <Table.Column key={column.key} isRowHeader={index === 0}
                  className={`sticky top-0 z-[1] bg-gray-50/95 text-xs font-semibold text-gray-700 backdrop-blur-xs border-b border-gray-200 ${column.align === "right" ? "text-right" : ""}`}
                  style={{ width: column.width, minWidth: column.width }}>{column.title}</Table.Column>)}
              </Table.Header>
              <Table.Body renderEmptyState={() => <div className="p-8 text-center text-xs text-gray-500">
                {error ? "数据读取失败，请刷新重试" : "当前周期内没有请求记录"}
              </div>}>
                {(rows ?? []).map((row) => <Table.Row key={row.key} id={row.key} className="hover:bg-blue-50/20 transition-colors border-b border-gray-100">
                  {columns.map((column) => <Table.Cell key={column.key} className={`align-top text-xs ${column.align === "right" ? "text-right" : ""}`}>
                    {column.render(undefined, row)}
                  </Table.Cell>)}
                </Table.Row>)}
                {rows && rows.length > 0 ? <Table.Row id="__total" className="border-t-2 border-blue-200 bg-blue-50/30 font-semibold">
                  {columns.map((column, index) => <Table.Cell key={column.key} className={`align-top text-xs ${column.align === "right" ? "text-right" : ""}`}>
                    {index === 0 ? <strong className="text-blue-900 font-bold">合计</strong> : column.render(undefined, total)}
                  </Table.Cell>)}
                </Table.Row> : null}
              </Table.Body>
            </Table.Content>
          </Table.ScrollContainer>
        </Table>
      </div>
    </DialogContent>
  </Dialog>;
}
