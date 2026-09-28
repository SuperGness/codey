import { useEffect, useMemo, useState } from "react";
import { Alert, Spinner, Table } from "@heroui/react";
import { IconRefresh } from "@tabler/icons-react";
import { invoke } from "./api";
import { errorText } from "./appUtils";
import { listOfficialAccounts } from "./officialAccountsRequests";
import { formatTimestamp } from "./formatters";
import { Button, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, Select } from "./components/ui";
import { estimateQuotaRows, PRICING_CHECKED, PRICING_SOURCE, quotaAggregateRows, quotaPeriod, sumQuotaRows, WEEK_MS } from "./quotaEstimate";
import type { AccountUsageSnapshot, QuotaEstimate, QuotaUsageResult, QuotaRow } from "./quotaEstimate";
import type { OfficialAccount } from "./App.types";
import { maskEmail } from "./sensitiveText";
import { createAccountUsageReader } from "./accountUsageRequests";

declare global {
  interface Window {
    __codeyReadQuotaAccountUsage?: (options: { forceRefresh: boolean }) => Promise<AccountUsageSnapshot>;
  }
}
// 只合并同一账号正在进行的读取，包含 React 开发模式下的重复挂载。
const readAccountUsage = createAccountUsageReader((accountId, forceRefresh) => {
  const bridge = accountId ? undefined : window.__codeyReadQuotaAccountUsage;
  return bridge
    ? bridge({ forceRefresh })
    : invoke<AccountUsageSnapshot>("query_official_account_usage", {
      ...(accountId ? { accountId } : {}), forceRefresh,
    });
});

// 每个官方账号单独读取额度、单独统计自己的请求，任何一层都不能跨账号合并。
type EstimateTarget = {
  key: string;
  label: string;
  accountId?: string;
  isDefault?: boolean;
  filter: { provider?: string; officialAccountId?: string };
  projectable: boolean;
};
type EstimateGroup = EstimateTarget & {
  rows: QuotaRow[];
  total: QuotaRow;
  estimate: QuotaEstimate | null;
  usage: AccountUsageSnapshot | null;
  usageWarning: string;
  error: string;
};

function officialAccountLabel(account: OfficialAccount) {
  const email = account.email?.trim();
  return email ? maskEmail(email) : account.routeName?.trim() || account.id;
}

function estimateTargets(accounts: OfficialAccount[]): EstimateTarget[] {
  const stored: EstimateTarget[] = accounts.map((account) => ({
    key: account.id,
    label: officialAccountLabel(account),
    accountId: account.id,
    isDefault: account.isDefault,
    filter: { officialAccountId: account.id },
    projectable: true,
  }));
  // 升级前的官方线路直接复用 Codex 登录，历史记录里没有账号字段。
  const unattributed: EstimateTarget = {
    key: "unattributed",
    label: "未区分账号的官方记录",
    filter: { provider: "openai" },
    projectable: false,
  };
  if (stored.length === 0) {
    return [{
      ...unattributed,
      key: "codex-login",
      label: "Codex 登录账号",
      isDefault: true,
      projectable: true,
    }];
  }
  return stored;
}

const integer = (value: number) => value.toLocaleString("en-US", { maximumFractionDigits: 0 });
const money = (value: number | null) => value == null ? "—" : value.toLocaleString("en-US", {
  style: "currency", currency: "USD", minimumFractionDigits: 4, maximumFractionDigits: 4,
});
const metrics = (items: ReadonlyArray<readonly [string, string, string?]>) => <dl className="m-0 grid gap-0.5 text-xs tabular-nums">
  {items.map(([label, value, colorClass]) => <div key={label} className="flex items-center justify-between gap-2">
    <dt className="font-normal text-gray-500 dark:text-gray-400">{label}</dt>
    <dd className={`m-0 font-medium whitespace-nowrap ${colorClass ?? "text-gray-800 dark:text-gray-300"}`}>{value}</dd>
  </div>)}
</dl>;
const columnsFor = (group: EstimateGroup) => {
  const total = group.total;
  const period = group.estimate?.period ?? null;
  const result = group.estimate?.result ?? null;
  return [
    { title: "模型名称", key: "model", width: 155, render: (_: unknown, row: QuotaRow) => <div className="break-words">
      <div className="font-semibold text-gray-900 dark:text-gray-300">{row.model}</div>
      {row.unpriced > 0 && <div className="mt-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400">{row.note}，未计价</div>}
    </div> },
    { title: "档位 / 上下文", key: "rules", width: 155, render: (_: unknown, row: QuotaRow) => <div className="text-xs">
      <div className="font-medium text-gray-800 dark:text-gray-300">{row.tier}{row.context && ` · ${row.context}`}</div>
      <div className="mt-0.5 text-gray-500 dark:text-gray-400">{row.source}</div>
    </div> },
    { title: "调用次数", key: "calls", width: 85, align: "right" as const,
      render: (_: unknown, row: QuotaRow) => <span className="font-medium tabular-nums text-gray-800 dark:text-gray-300">{integer(row.calls)}</span> },
    { title: "Token 用量", key: "tokens", width: 175, render: (_: unknown, row: QuotaRow) => metrics([
      ["输入", integer(row.input)], ["输出", integer(row.output)], ["总计", integer(row.tokens)],
    ]) },
    { title: "缓存用量", key: "cache", width: 185, render: (_: unknown, row: QuotaRow) => <>
      {metrics([["命中次数", integer(row.hits)], ["读取 Token", integer(row.cached)], ["写入 Token", integer(row.writes)]])}
      {row.missingWrites > 0 && <div className="mt-1 text-[11px] font-medium text-amber-600 dark:text-amber-400">{integer(row.missingWrites)} 次未记录写入量</div>}
    </> },
    { title: "费用明细 (USD)", key: "fees", width: 210, render: (_: unknown, row: QuotaRow) => metrics(([
      ["inputCost", "非缓存输入"], ["outputCost", "输出"], ["readCost", "缓存读取"],
      ["writeCost", "缓存写入"], ["cacheSaving", "缓存节省（参考）"],
    ] as const).map(([key, label]) => [
      label,
      money(row.calls > 0 && row.unpriced === row.calls ? null : row[key]),
      key === "cacheSaving" && (row.cacheSaving ?? 0) > 0 ? "text-emerald-600 dark:text-emerald-400 font-semibold" : undefined,
    ])) },
    { title: "额度估算 (USD)", key: "estimate", width: 215, render: (_: unknown, row: QuotaRow) => metrics([
      ["当前已消耗", money(row.calls > 0 && row.unpriced === row.calls ? null : row.cost), "text-blue-600 dark:text-blue-400 font-bold"],
      ["折算周限份额", money(result?.limit == null || !period || row.unpriced === row.calls && row.calls > 0 ? null : row.cost / (period.usedPercent / 100))],
      ["预计周消耗", money(result && period && !(row.calls > 0 && row.unpriced === row.calls) ? row.cost * WEEK_MS / (period.toUnixMs - period.fromUnixMs) : null)],
      ["占本账号消耗", row.calls > 0 && row.unpriced === row.calls ? "—" : `${(total.cost > 0 ? row.cost / total.cost * 100 : 0).toFixed(2)}%`, "text-blue-600 dark:text-blue-400 font-semibold"],
    ]) },
  ];
};
// 与账号列表一致，逐个账号顺序读取额度，两次请求之间留出间隔，避免同时向官方接口发起多个请求。
const USAGE_QUERY_STAGGER_MS = 200;

export function QuotaEstimateDialog({ container, onClose }: {
  container: HTMLElement | null; onClose: () => void;
}) {
  const [groups, setGroups] = useState<EstimateGroup[]>([]);
  const [selectedKey, setSelectedKey] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(0);
  const [error, setError] = useState("");
  const [healthWarning, setHealthWarning] = useState(false);
  const [revision, setRevision] = useState(0);
  const [rangeEnd, setRangeEnd] = useState(() => Date.now());

  useEffect(() => {
    let active = true;
    const windowEnd = Date.now();
    const windowStart = windowEnd - WEEK_MS;
    setLoading(true); setGroups([]); setError(""); setLoaded(0); setHealthWarning(false);
    setRangeEnd(windowEnd);
    void (async () => {
      try {
        const result = await listOfficialAccounts();
        if (!active) return;
        const targets = estimateTargets(result.accounts ?? []);
        const queryUsage = async (target: EstimateTarget, fromUnixMs: number, toUnixMs: number) => {
          const usage = await invoke<QuotaUsageResult>("query_route_request_log_quota_usage", {
            ...target.filter, fromUnixMs, toUnixMs, unassignedOnly: !target.projectable,
          });
          if (!active) return null;
          if (!usage.queryable) throw new Error("当前日志暂不可查询，请开启请求日志记录后重试。");
          const health = usage.recordingHealth;
          if (health && (!health.active || health.sampleRatePerMillion < 1_000_000
            || health.droppedFull + health.droppedClosed + health.writeDropped + health.writeFailures > 0)) {
            setHealthWarning(true);
          }
          return usage;
        };
        const collected: EstimateGroup[] = [];
        let usageReads = 0;
        let loadedTotal = 0;
        for (const target of targets) {
          if (!active) return;
          const recent = await queryUsage(target, windowStart, windowEnd);
          if (!active || !recent) return;
          loadedTotal += recent.totalCalls;
          setLoaded(loadedTotal);
          // 未记录账号的历史记录只在确实存在时展示，也不消耗官方额度请求。
          if (!target.projectable && recent.totalCalls === 0) continue;
          const rows = quotaAggregateRows(recent.groups);
          const group: EstimateGroup = {
            ...target, rows, total: sumQuotaRows(rows),
            estimate: null, usage: null, usageWarning: "", error: "",
          };
          if (target.projectable && recent.totalCalls > 0) {
            if (usageReads > 0) {
              await new Promise((resolve) => setTimeout(resolve, USAGE_QUERY_STAGGER_MS));
              if (!active) return;
            }
            usageReads += 1;
            try {
              const snapshot = await readAccountUsage(target.accountId, revision > 0);
              if (!active) return;
              const period = quotaPeriod(snapshot);
              // 近期聚合仅用于资格判断及额度失败时展示；成功后按快照的精确截止时间重新聚合。
              const exact = await queryUsage(target, period.fromUnixMs, period.toUnixMs);
              if (!active || !exact) return;
              const periodUsageRows = quotaAggregateRows(exact.groups);
              const estimate = estimateQuotaRows(period, periodUsageRows);
              group.usage = snapshot;
              group.estimate = estimate;
              group.rows = periodUsageRows;
              group.total = estimate.total;
              group.usageWarning = snapshot.stale
                ? snapshot.message || "当前使用上次成功获取的官方额度，统计截止时间保持不变。"
                : "";
            } catch (cause) {
              if (!active) return;
              group.error = errorText(cause);
            }
          }
          if (!active) return;
          collected.push(group);
          setGroups([...collected]);
        }
      } catch (cause) {
        if (active) setError(`无法完成额度估算：${errorText(cause)}`);
      } finally { if (active) setLoading(false); }
    })();
    return () => { active = false; };
  }, [revision]);

  // 只能选到有明确账号记录的，没记录账号的不放在里面展示
  const selectableGroups = groups.filter((group) => group.projectable && group.rows.length > 0);
  const activeGroup = selectableGroups.find((group) => group.key === selectedKey)
    ?? selectableGroups.find((group) => group.isDefault)
    ?? selectableGroups[0]
    ?? null;
  const activeKey = activeGroup?.key ?? "";

  const activeTotal = activeGroup ? activeGroup.total : sumQuotaRows([]);
  const warnings = activeGroup ? [
    healthWarning && "日志记录未完整开启或存在采样、丢弃及写入异常，估算仅覆盖已记录的请求。",
    activeTotal.unpriced > 0 && `${integer(activeTotal.unpriced)} 次请求因档位或费率缺失未计价。`,
    activeTotal.missing > 0 && `${integer(activeTotal.missing)} 次请求缺少 Token 数据，缺失值按 0，结果可能偏低。`,
    activeTotal.assumed > 0 && `${integer(activeTotal.assumed)} 次请求未经响应确认档位，按请求档位或默认 Standard 估算，并与已确认用量分开。`,
    activeTotal.missingWrites > 0 && `${integer(activeTotal.missingWrites)} 次请求未记录缓存写入量，按 0 展示；对应输入仍按普通输入价计费，额外写入费用可能未计入。`,
  ].filter(Boolean) : (healthWarning ? ["日志记录未完整开启或存在采样、丢弃及写入异常，估算仅覆盖已记录的请求。"] : []);
  const estimateColumns = useMemo(
    () => (activeGroup ? columnsFor(activeGroup) : []),
    [activeGroup],
  );

  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent container={container} className="quota-estimate-dialog w-full sm:w-[min(1240px,calc(100vw-32px))] max-w-[min(1240px,calc(100vw-32px))]">
      <DialogHeader>
        <div className="flex flex-col gap-0.5">
          <DialogTitle className="text-lg font-bold text-gray-900 dark:text-gray-300">周限额度估算</DialogTitle>
          <DialogDescription className="text-xs text-gray-500 dark:text-gray-400">
            每个官方账号分别读取周额度、分别统计自己的请求，不合并其他账号的消耗。
          </DialogDescription>
        </div>
      </DialogHeader>
      <div className="mt-3 flex min-w-0 flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-3 py-1 text-xs">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-gray-600 dark:text-gray-300">
            <span>读取区间：<span className="font-medium text-gray-800 dark:text-gray-300">{formatTimestamp(rangeEnd - WEEK_MS)}</span> 至 <span className="font-medium text-gray-800 dark:text-gray-300">{formatTimestamp(rangeEnd)}</span>（不含结束时间）</span>
            {selectableGroups.length > 0 && <>
              <span className="text-gray-300 dark:text-gray-400">·</span>
              <span>有记录账号：<span className="font-medium text-gray-800 dark:text-gray-300">{integer(selectableGroups.length)} 个</span></span>
            </>}
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <div className="flex items-center gap-1.5 text-xs text-gray-600 dark:text-gray-300">
              <span className="shrink-0 font-medium text-gray-700 dark:text-gray-300">账号：</span>
              <Select
                aria-label="选择官方账号"
                className="w-60 shrink-0"
                disabled={loading || selectableGroups.length === 0}
                placeholder={loading ? "正在加载账号…" : selectableGroups.length === 0 ? "暂无账号记录" : "选择账号"}
                value={activeKey}
                onChange={(value) => {
                  if (value != null) setSelectedKey(String(value));
                }}
                optionList={selectableGroups.map((group) => ({
                  label: `${group.label}${group.isDefault ? "（默认）" : ""}${group.error ? "（额度读取失败）" : ""}`,
                  value: group.key,
                }))}
              />
            </div>
            <Button
              variant="outline"
              size="sm"
              className="h-8 shrink-0 gap-1.5 px-3 text-xs font-medium text-blue-600 dark:text-blue-400 border-blue-200 dark:border-blue-700 hover:bg-blue-50/80 transition-colors"
              disabled={loading}
              onClick={() => setRevision(value => value + 1)}
            >
              <IconRefresh size={13} className={loading ? "animate-spin" : ""} aria-hidden="true" />
              刷新数据
            </Button>
          </div>
        </div>
        <Alert className="px-3.5 py-2.5" status={error || activeGroup?.error ? "danger" : warnings.length || activeGroup?.usageWarning ? "warning" : "accent"}>
          <Alert.Indicator />
          <Alert.Content>
          <Alert.Title><span className="text-xs font-medium text-amber-900 dark:text-amber-300">按 OpenAI 各档位 API 单价估算等值金额（USD），不代表订阅实际扣费或官方周限。</span></Alert.Title>
          <Alert.Description><div className="text-xs leading-relaxed text-amber-800 dark:text-amber-300">
            {error && <p className="m-0 font-medium text-red-600 dark:text-red-400">{error}</p>}
            {activeGroup?.error && <p className="m-0 font-medium text-red-600 dark:text-red-400">账号「{activeGroup.label}」额度读取失败：{activeGroup.error}</p>}
            {activeGroup?.usageWarning && <p className="m-0">{activeGroup.usageWarning}</p>}
            {activeGroup?.estimate?.period.usedPercent === 0 && <p className="m-0">该账号官方周额度已用比例为 0%，暂时无法反推该账号的周限及剩余额度；产生用量后可刷新重算。</p>}
            {warnings.length > 0 && <p className="m-0">{warnings.join(" ")}</p>}
            <details className="mt-1">
              <summary className="cursor-pointer select-none font-medium text-amber-900 dark:text-amber-300 hover:text-amber-950 transition-colors">计算说明与价格来源</summary>
                <div className="mt-2 grid gap-1.5 border-t border-amber-200/60 dark:border-amber-700/60 pt-2 text-xs text-gray-600 dark:text-gray-300 leading-relaxed">
                  <p className="m-0">金额统一保留 4 位小数；Token 和调用次数取整数；比例保留 2 位小数。计算时使用未四舍五入的金额。</p>
                  <p className="m-0">当前消耗 =（非缓存输入 × 输入单价 + 缓存读取 × 读取单价 + 缓存写入 × 写入单价 + 输出 × 输出单价）÷ 1,000,000。各费用单独展示，推理 Token 已包含在输出中，不重复计费。</p>
                  <p className="m-0">预估周限 = 该账号本周期已记录消耗 ÷ 该账号官方周额度已用比例；当前预估剩余 = 预估周限 − 该账号本周期已消耗。模型的折算周限份额按同一比例分摊。预计周消耗 = 本周期消耗 × 7 天 ÷ 已统计时长，仅表示按当前速度推算的整周消耗，不参与周限反推。</p>
                  <p className="m-0">每个账号只使用自己的请求记录和自己的已用比例，多个账号的消耗不会相加后反推周限。账号的已用比例可能包含该账号在其他设备的用量。</p>
                  <p className="m-0">Standard、Fast（含 priority）、Flex、Batch 各用独立价表，响应档位优先于请求档位；请求 Fast 而响应 default 按 Standard 计价。只有请求档位时单独列为推定；未记录计费档位或仅记录 auto 时按默认 Standard 档位计价，并标明默认依据。</p>
                  <p className="m-0">适用模型单次输入超过 272K 时，整次请求使用该档位的长上下文价，表格与短上下文分开。未公布价格的组合不借用其他档位价格。</p>
                  <p className="m-0">估算仅覆盖已记录的 Token，不按采样率补推。工具调用、搜索内容特殊计价、容器和存储等缺少完整计费数据，尚未计入。合计中的未计价请求不代表实际免费。</p>
                  <p className="m-0">价格核对：{PRICING_CHECKED} · <a className="text-blue-600 dark:text-blue-400 hover:underline font-medium" href={PRICING_SOURCE} target="_blank" rel="noreferrer">OpenAI 官方价格</a>；Codex 历史模型价格见对应官方模型页。GPT-5.6 Sol 使用当前公开促销价。</p>
                </div>
            </details>
          </div></Alert.Description>
          </Alert.Content>
        </Alert>
        {activeGroup ? <section key={activeGroup.key} className="flex min-w-0 flex-col gap-2 rounded-xl border border-gray-200/90 dark:border-gray-700/90 bg-[var(--codey-surface,#fff)] p-3.5 shadow-xs">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <span className="font-semibold text-gray-900 dark:text-gray-300">{activeGroup.label}</span>
              {activeGroup.isDefault && <span className="rounded bg-blue-50 dark:bg-blue-950 px-1.5 py-0.5 font-normal text-blue-600 dark:text-blue-400 border border-blue-200/60 dark:border-blue-700/60">默认</span>}
              <span className="text-gray-300 dark:text-gray-400">·</span>
              <span className="text-gray-500 dark:text-gray-400">官方周额度已使用：</span>
              <span className="inline-flex items-center px-2 py-0.5 rounded-md font-semibold bg-blue-50 dark:bg-blue-950 text-blue-600 dark:text-blue-400 border border-blue-200/60 dark:border-blue-700/60 tabular-nums">
                {activeGroup.estimate ? `${activeGroup.estimate.period.usedPercent.toFixed(2)}%` : loading ? "正在读取…" : "暂不可用"}
              </span>
            </div>
            {activeGroup.estimate && <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-gray-600 dark:text-gray-300">
              <span>上次重置（推算）：<span className="text-gray-800 dark:text-gray-300 font-medium">{formatTimestamp(activeGroup.estimate.period.fromUnixMs)}</span></span>
              <span className="text-gray-300 dark:text-gray-400">·</span>
              <span>下次重置：<span className="text-gray-800 dark:text-gray-300 font-medium">{formatTimestamp(activeGroup.estimate.period.resetsAt)}</span></span>
              <span className="text-gray-300 dark:text-gray-400">·</span>
              <span>更新时间：<span className="text-gray-800 dark:text-gray-300 font-medium">{formatTimestamp(activeGroup.estimate.period.toUnixMs)}</span></span>
            </div>}
          </div>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3" aria-live="polite">
            {([
              ["当前预计周额度消耗", activeGroup.estimate?.result?.weekly],
              ["预估周限", activeGroup.estimate?.result?.limit],
              ["当前预估剩余额度", activeGroup.estimate?.result?.remaining],
            ] as const).map(([label, value]) => <div key={label} className="flex flex-col justify-between rounded-xl border border-gray-200/90 dark:border-gray-700/90 bg-[var(--codey-surface,#fff)] p-3 shadow-xs">
              <div className="flex items-center justify-between text-xs text-gray-500 dark:text-gray-400">
                <span className="font-medium">{label}</span>
                {activeGroup.total.unpriced > 0 && <span className="rounded bg-amber-50 dark:bg-amber-950 px-1.5 py-0.5 text-[11px] font-normal text-amber-700 dark:text-amber-300 border border-amber-200/60 dark:border-amber-700/60">仅含可计价用量</span>}
              </div>
              <div className="mt-1.5 flex items-baseline gap-1">
                <span className="text-xl font-bold tracking-tight text-blue-600 dark:text-blue-400 tabular-nums">{money(value ?? null)}</span>
                <span className="text-xs font-semibold text-blue-500 dark:text-blue-400">USD</span>
              </div>
            </div>)}
          </div>
          {activeGroup.error ? <p className="m-0 text-xs font-medium text-red-600 dark:text-red-400">该账号额度暂不可用，未生成周限推算。</p>
            : activeGroup.estimate === null ? <p className="m-0 text-xs text-gray-500 dark:text-gray-400">该账号本周期没有请求记录，未读取官方额度，无法推算周限。</p>
            : null}
          {activeGroup.rows.length > 0 && <Table className="quota-estimate-table relative" variant="secondary" aria-busy={loading}>
            <Table.ScrollContainer className="max-h-[420px] overflow-auto rounded-xl border border-gray-200 dark:border-gray-700 bg-[var(--codey-surface,#fff)] shadow-2xs">
              <Table.Content aria-label={`${activeGroup.label} 模型额度明细`} className={`min-w-[1180px] ${loading ? "opacity-60" : ""}`}>
                <Table.Header>
                  {estimateColumns.map((column, index) => <Table.Column key={column.key} isRowHeader={index === 0}
                    className={`sticky top-0 z-[1] bg-gray-50/95 dark:bg-gray-800/95 text-xs font-semibold text-gray-700 dark:text-gray-300 backdrop-blur-xs border-b border-gray-200 dark:border-gray-700 ${column.align === "right" ? "text-right" : ""}`}
                    style={{ width: column.width, minWidth: column.width }}>{column.title}</Table.Column>)}
                </Table.Header>
                <Table.Body renderEmptyState={() => <div className="p-8 text-center text-xs text-gray-500 dark:text-gray-400">
                  {activeGroup.error ? "数据读取失败，请刷新重试" : "当前周期内没有请求记录"}
                </div>}>
                  {activeGroup.rows.map((row) => <Table.Row key={row.key} id={row.key} className="hover:bg-blue-50/20 transition-colors border-b border-gray-100 dark:border-gray-700">
                    {estimateColumns.map((column) => <Table.Cell key={column.key} className={`align-top text-xs ${column.align === "right" ? "text-right" : ""}`}>
                      {column.render(undefined, row)}
                    </Table.Cell>)}
                  </Table.Row>)}
                  <Table.Row id="__total" className="border-t-2 border-blue-200 dark:border-blue-700 bg-blue-50/30 dark:bg-blue-950/30 font-semibold">
                    {estimateColumns.map((column, index) => <Table.Cell key={column.key} className={`align-top text-xs ${column.align === "right" ? "text-right" : ""}`}>
                      {index === 0 ? <strong className="text-blue-900 dark:text-blue-300 font-bold">本账号小计</strong> : column.render(undefined, activeGroup.total)}
                    </Table.Cell>)}
                  </Table.Row>
                </Table.Body>
              </Table.Content>
            </Table.ScrollContainer>
          </Table>}
        </section> : !loading ? (
          <section className="flex min-w-0 flex-col items-center justify-center rounded-xl border border-dashed border-gray-200 dark:border-gray-700 bg-[var(--codey-surface,#fff)] p-12 text-center text-xs text-gray-500 dark:text-gray-400">
            <p className="m-0 font-medium text-gray-700 dark:text-gray-300 text-sm">暂无可估算的官方账号</p>
            <p className="mt-1.5 m-0 text-gray-400 dark:text-gray-400">仅展示有明确账号请求记录的官方账号；当前周期内未查询到符合条件的请求记录。</p>
          </section>
        ) : null}
        {loading && <div role="status" className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400">
          <Spinner size="sm" />
          <span>{groups.length > 0 ? `正在读取账号额度，已加载 ${integer(loaded)} 次请求…` : "正在读取官方账号与日志…"}</span>
        </div>}
      </div>
    </DialogContent>
  </Dialog>;
}
