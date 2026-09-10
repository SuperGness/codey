import { useEffect, useState } from "react";
import { Alert, Table } from "antd";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import { formatTimestamp } from "./formatters";
import { Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./components/antd";
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
    : invoke<AccountUsageSnapshot>("query_official_account_usage", { forceRefresh: true }))
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
  const provider = "openai";
  useEffect(() => {
    let active = true;
    setLoading(true); setRows(null); setPeriod(null); setError(""); setLoaded(0); setUpdatedAt(null); setHealthWarning(false);
    void (async () => {
      try {
        const snapshot = await readAccountUsage(revision > 0);
        if (!active) return;
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
    healthWarning && "日志记录未完整开启或存在采样、丢弃及写入异常，估算仅覆盖已记录的请求。",
    rows && (total.unpriced > 0 || total.missing > 0) && `${integer(total.unpriced)} 次请求因档位或费率缺失未计价；${integer(total.missing)} 次请求缺少 Token 数据，缺失值按 0，结果可能偏低。`,
    rows && total.assumed > 0 && `${integer(total.assumed)} 次请求未经响应确认档位，按请求档位或默认 Standard 估算，并与已确认用量分开。`,
    rows && total.missingWrites > 0 && `${integer(total.missingWrites)} 次请求未记录缓存写入量，按 0 展示；对应输入仍按普通输入价计费，额外写入费用可能未计入。`,
  ].filter(Boolean);
  const metrics = (items: ReadonlyArray<readonly [string, string]>) => <dl className="m-0 grid gap-0.5 text-xs tabular-nums">
    {items.map(([label, value]) => <div key={label} className="flex justify-between gap-2">
      <dt className="font-normal text-gray-500">{label}</dt><dd className="m-0 whitespace-nowrap">{value}</dd>
    </div>)}
  </dl>;
  const columns = [
    { title: "模型名称", key: "model", width: 135, render: (_: unknown, row: QuotaRow) => <div className="break-words">
      <strong>{row.model}</strong>{row.unpriced > 0 && <div className="text-xs text-amber-700">{row.note}，未计价</div>}
    </div> },
    { title: "档位 / 上下文", key: "rules", width: 145, render: (_: unknown, row: QuotaRow) => <div className="text-xs">
      <div>{row.tier}{row.context && ` · ${row.context}`}</div>
      <div className="mt-1 text-gray-500">{row.source}</div>
    </div> },
    { title: "调用次数", key: "calls", width: 75, align: "right" as const,
      render: (_: unknown, row: QuotaRow) => integer(row.calls) },
    { title: "Token 用量", key: "tokens", width: 165, render: (_: unknown, row: QuotaRow) => metrics([
      ["输入", integer(row.input)], ["输出", integer(row.output)], ["总计", integer(row.tokens)],
    ]) },
    { title: "缓存用量", key: "cache", width: 175, render: (_: unknown, row: QuotaRow) => <>
      {metrics([["命中次数", integer(row.hits)], ["读取 Token", integer(row.cached)], ["写入 Token", integer(row.writes)]])}
      {row.missingWrites > 0 && <div className="mt-0.5 text-xs text-amber-700">{integer(row.missingWrites)} 次未记录写入量</div>}
    </> },
    { title: "费用明细 (USD)", key: "fees", width: 200, render: (_: unknown, row: QuotaRow) => metrics(([
      ["inputCost", "非缓存输入"], ["outputCost", "输出"], ["readCost", "缓存读取"],
      ["writeCost", "缓存写入"], ["cacheSaving", "缓存节省（参考）"],
    ] as const).map(([key, label]) => [label, money(row.calls > 0 && row.unpriced === row.calls ? null : row[key])])) },
    { title: "额度估算 (USD)", key: "estimate", width: 205, render: (_: unknown, row: QuotaRow) => metrics([
      ["当前已消耗", money(row.calls > 0 && row.unpriced === row.calls ? null : row.cost)],
      ["折算周限份额", money(result?.limit == null || !period || row.unpriced === row.calls && row.calls > 0 ? null : row.cost / (period.usedPercent / 100))],
      ["预计周消耗", money(result && period && !(row.calls > 0 && row.unpriced === row.calls) ? row.cost * 7 * 86_400_000 / (period.toUnixMs - period.fromUnixMs) : null)],
      ["占总消耗", row.calls > 0 && row.unpriced === row.calls ? "—" : `${(total.cost > 0 ? row.cost / total.cost * 100 : 0).toFixed(2)}%`],
    ]) },
  ];
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent container={container} className="quota-estimate-dialog !w-[1200px]">
      <DialogHeader>
        <DialogTitle>周限额度估算</DialogTitle>
        <DialogDescription>{period
          ? `本次统计：${formatTimestamp(period.fromUnixMs)} 至 ${formatTimestamp(period.toUnixMs)}（不含结束时间）`
          : "自动读取官方周额度，按上次重置以来的用量估算周限。"}</DialogDescription>
      </DialogHeader>
      <div className="mt-3 grid min-w-0 grid-cols-1 max-h-[70vh] gap-3 overflow-y-auto pr-1">
        <div className="flex items-start justify-between gap-3">
          <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs">
            <strong>官方周额度已使用：{period ? `${period.usedPercent.toFixed(2)}%` : loading ? "正在读取" : "暂不可用"}</strong>
            {period && <>
              <span>上次重置（推算）：{formatTimestamp(period.fromUnixMs)}</span>
              <span>下次重置：{formatTimestamp(period.resetsAt)}</span>
              <span>额度数据更新时间：{formatTimestamp(period.toUnixMs)}</span>
            </>}
          </div>
          <Button variant="outline" size="sm" disabled={loading} onClick={() => setRevision(value => value + 1)}>刷新数据</Button>
        </div>
        <Alert className="!px-3 !py-2" type={error || calculationError ? "error" : warnings.length ? "warning" : "info"} showIcon
          title={<span className="text-xs">按 OpenAI 各档位 API 单价估算等值金额（USD），不代表订阅实际扣费或官方周限。</span>}
          description={<div className="text-xs leading-relaxed">
            {(error || calculationError) && <p className="m-0 font-medium">{error || calculationError}</p>}
            {period?.usedPercent === 0 && <p className="m-0">官方周额度已用比例为 0%，暂时无法反推周限及剩余额度；产生用量后可刷新重算。</p>}
            {warnings.length > 0 && <p className="m-0">{warnings.join(" ")}</p>}
            <details className="mt-1">
              <summary className="cursor-pointer">计算说明与价格来源</summary>
                <div className="grid gap-1 text-xs leading-relaxed text-gray-500">
                  <p>金额统一保留 4 位小数；Token 和调用次数取整数；比例保留 2 位小数。计算时使用未四舍五入的金额。</p>
                  <p>周统计周期由官方下次重置时间减去 7 天得到；请求从推算的上次重置时间读到额度数据更新时间，以对齐官方使用比例。请求数据读取完成时间：{updatedAt ? formatTimestamp(updatedAt) : "尚未完成读取"}。刷新会重新获取额度及对应周期的请求，不修改原页面筛选或记录。</p>
                  <p>当前消耗 =（非缓存输入 × 输入单价 + 缓存读取 × 读取单价 + 缓存写入 × 写入单价 + 输出 × 输出单价）÷ 1,000,000。各费用单独展示，推理 Token 已包含在输出中，不重复计费。</p>
                  <p>预估周限 = 本周期已记录消耗 ÷ 官方周额度已用比例；当前预估剩余 = 预估周限 − 本周期已消耗。模型的折算周限份额按同一比例分摊。预计周消耗 = 本周期消耗 × 7 天 ÷ 已统计时长，仅表示按当前速度推算的整周消耗，不参与周限反推。</p>
                  <p>开启额度显示时优先复用其有效数据；未开启、数据过期或主动刷新时查询官方接口。无需填写比例，查询不会开启额度显示或后台轮询。官方未返回周窗口、重置时间或有效使用比例时不进行推算。</p>
                  <p>Standard、Fast（含 priority）、Flex、Batch 各用独立价表，响应档位优先于请求档位；请求 Fast 而响应 default 按 Standard 计价。只有请求档位时单独列为推定；未记录计费档位或仅记录 auto 时按默认 Standard 档位计价，并标明默认依据。</p>
                  <p>缓存读写从输入中拆分，均已包含在当前消耗内；该档位无单独缓存价格时按输入价计算。新版模型按公开缓存写入价计费。缓存节省与普通输入价比较，负数表示写入增加费用，仅供参考，不重复加减。</p>
                  <p>适用模型单次输入超过 272K 时，整次请求使用该档位的长上下文价，表格与短上下文分开。未公布价格的组合不借用其他档位价格。</p>
                  <p>估算仅覆盖本线路已记录的 Token，不按采样率补推。官方已用比例可能包含其他设备用量，缺失日志或跨账号历史会影响推算准确性。工具调用、搜索内容特殊计价、容器和存储等缺少完整计费数据，尚未计入。合计中的未计价请求不代表实际免费。</p>
                  <p>价格核对：{PRICING_CHECKED} · <a className="text-blue-600 underline" href={PRICING_SOURCE} target="_blank" rel="noreferrer">OpenAI 官方价格</a>；Codex 历史模型价格见对应官方模型页。GPT-5.6 Sol 使用当前公开促销价。gpt-6-astra 缓存读取单价按自定义规则乘以 2，适用于所有档位及长短上下文。</p>
                </div>
            </details>
          </div>} />
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3" aria-live="polite">
          {([
            ["当前预计周额度消耗", result?.weekly],
            ["预估周限", result?.limit],
            ["当前预估剩余额度", result?.remaining],
          ] as const).map(([label, value]) => <div key={label} className="rounded-lg border border-black/10 bg-gray-50 px-3 py-2">
            <div className="text-xs text-gray-500">{label}</div>
            {total.unpriced > 0 && <div className="text-xs text-amber-700">仅含可计价用量</div>}
            <strong className="mt-1 block text-xl tabular-nums text-blue-600">{money(value ?? null)} <small className="text-xs">USD</small></strong>
          </div>)}
        </div>
        {loading && <div role="status" className="text-xs text-gray-500">{period ? `正在读取本周统计，已加载 ${integer(loaded)} 次请求…` : "正在读取官方周额度…"}</div>}
        <Table<QuotaRow> size="small" rowKey="key" loading={loading} pagination={false}
          dataSource={rows ?? []} columns={columns} scroll={{ x: 1100, y: 360 }}
          locale={{ emptyText: error ? "数据读取失败，请刷新重试" : "当前周期内没有请求记录" }}
          summary={() => rows ? <Table.Summary.Row>
            {columns.map((column, index) => <Table.Summary.Cell key={column.key} index={index} align={column.key === "calls" ? "right" : "left"}>
              <strong>{index === 0 ? "合计" : column.render(undefined, total)}</strong>
            </Table.Summary.Cell>)}
          </Table.Summary.Row> : null} />
      </div>
      <DialogFooter><Button variant="outline" onClick={onClose}>关闭</Button></DialogFooter>
    </DialogContent>
  </Dialog>;
}
