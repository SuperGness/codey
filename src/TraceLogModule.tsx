import {
  IconAlertCircle as CircleAlert,
  IconInfoCircle as InfoCircle,
  IconLoader2 as LoaderCircle,
  IconRefresh as RefreshCw,
  IconTrash as Trash2,
} from "@tabler/icons-react";

import { Badge, Button, Card } from "./components/ui";

export type TraceLogDailyStats = {
  date: string;
  rows: number;
  estimatedBytes: number;
};

export type TraceLogGroupStats = {
  name: string;
  rows: number;
  estimatedBytes: number;
};

export type TraceLogStats = {
  pending: boolean;
  capturedAt: number;
  recentDaysWindow: number;
  databasesFound: number;
  databasesScanned: number;
  databaseBytes: number;
  rowCount: number;
  estimatedLogBytes: number;
  recentRowCount: number;
  recentEstimatedBytes: number;
  oldestTimestamp?: number;
  newestTimestamp?: number;
  daily: TraceLogDailyStats[];
  levels: TraceLogGroupStats[];
  topTargets: TraceLogGroupStats[];
  errors: string[];
};

type TraceLogModuleProps = {
  stats?: TraceLogStats;
  snapshotStale: boolean;
  protectionEnabled: boolean;
  clearBusy: boolean;
  refreshing: boolean;
  disabled: boolean;
  onClear: () => void;
  onRefresh: () => void;
};

const countFormatter = new Intl.NumberFormat("zh-CN");
const snapshotTimeFormatter = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});
const rangeDateFormatter = new Intl.DateTimeFormat("zh-CN", {
  month: "2-digit",
  day: "2-digit",
});
const REFERENCE_SSD_TBW_BYTES = 300 * 1_000_000_000_000;
const MAX_WRITE_AMPLIFICATION = 4;

export type TraceDiskWearEstimate = {
  observedBytes: number;
  lowerWriteBytes: number;
  upperWriteBytes: number;
  lowerWearPercent: number;
  upperWearPercent: number;
};

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / (1024 ** index);
  return `${value >= 10 || index === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[index]}`;
}

function formatCount(value: number): string {
  return countFormatter.format(Number.isFinite(value) ? value : 0);
}

function finiteNonNegative(value: number): number {
  return Number.isFinite(value) && value > 0 ? value : 0;
}

export function estimateTraceDiskWear(
  stats: Pick<TraceLogStats, "databaseBytes" | "estimatedLogBytes">,
): TraceDiskWearEstimate {
  const observedBytes = Math.max(
    finiteNonNegative(stats.databaseBytes),
    finiteNonNegative(stats.estimatedLogBytes),
  );
  const lowerWriteBytes = observedBytes;
  const upperWriteBytes = observedBytes * MAX_WRITE_AMPLIFICATION;
  return {
    observedBytes,
    lowerWriteBytes,
    upperWriteBytes,
    lowerWearPercent: (lowerWriteBytes / REFERENCE_SSD_TBW_BYTES) * 100,
    upperWearPercent: (upperWriteBytes / REFERENCE_SSD_TBW_BYTES) * 100,
  };
}

function formatWearPercent(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0%";
  if (value < 0.000001) return "<0.000001%";
  const decimals = Math.min(6, Math.max(2, Math.ceil(-Math.log10(value)) + 1));
  return `${value.toFixed(decimals)}%`;
}

function formatWearPercentRange(lower: number, upper: number): string {
  const lowerText = formatWearPercent(lower);
  const upperText = formatWearPercent(upper);
  return lowerText === upperText ? upperText : `${lowerText}–${upperText}`;
}

function formatSnapshotTime(timestamp: number): string {
  if (!timestamp) return "本次启动";
  return snapshotTimeFormatter.format(new Date(timestamp * 1000));
}

function formatRange(stats: TraceLogStats): string {
  if (!stats.oldestTimestamp || !stats.newestTimestamp) return "暂无日志时间范围";
  return `${rangeDateFormatter.format(new Date(stats.oldestTimestamp * 1000))} - ${rangeDateFormatter.format(new Date(stats.newestTimestamp * 1000))}`;
}

function localDateKey(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function normalizedDailyStats(stats: TraceLogStats): TraceLogDailyStats[] {
  const byDate = new Map(stats.daily.map((item) => [item.date, item]));
  const windowDays = Math.max(1, stats.recentDaysWindow || 7);
  const base = new Date((stats.capturedAt || Date.now() / 1000) * 1000);
  base.setHours(12, 0, 0, 0);
  return Array.from({ length: windowDays }, (_, index) => {
    const date = new Date(base);
    date.setDate(base.getDate() - (windowDays - index - 1));
    const key = localDateKey(date);
    return byDate.get(key) ?? { date: key, rows: 0, estimatedBytes: 0 };
  });
}

export function TraceLogModule({
  stats,
  snapshotStale,
  protectionEnabled,
  clearBusy,
  refreshing,
  disabled,
  onClear,
  onRefresh,
}: TraceLogModuleProps) {
  const loading = refreshing || Boolean(stats?.pending);
  const snapshot = stats && stats.capturedAt > 0 && !stats.pending ? stats : undefined;
  const daily = snapshot ? normalizedDailyStats(snapshot) : [];
  const maxDailyBytes = Math.max(0, ...daily.map((item) => item.estimatedBytes));
  const diskWear = snapshot ? estimateTraceDiskWear(snapshot) : undefined;

  return (
    <section className="trace-section" aria-labelledby="trace-title">
      <div className="section-title compact trace-section-title">
        <div>
          <span className="section-kicker">Diagnostics</span>
          <h2 id="trace-title">Trace 日志分析</h2>
          <p>按需快照 · 日志诊断</p>
        </div>
        <div className="trace-module-actions">
          <Badge variant={protectionEnabled ? "success" : "secondary"}>
            {protectionEnabled ? "写盘保护已开启" : "写盘保护关闭"}
          </Badge>
          <Button
            className="trace-refresh-button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            onClick={onRefresh}
          >
            <RefreshCw className={loading ? "spinner" : ""} aria-hidden="true" />
            刷新统计
          </Button>
          <Button
            className="trace-clear-button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            onClick={onClear}
          >
            {clearBusy
              ? <LoaderCircle className="spinner" aria-hidden="true" />
              : <Trash2 aria-hidden="true" />}
            清理日志库
          </Button>
        </div>
      </div>

      <Card className={`trace-card${snapshot ? "" : " trace-card-empty"}`} aria-busy={loading}>
        {!snapshot ? (
          <div className="trace-empty-container">
            <div className="trace-empty" role="status" aria-live="polite">
              <div className="trace-empty-badge">
                <span className="trace-empty-icon">
                  {loading
                    ? <LoaderCircle className="spinner" size={28} aria-hidden="true" />
                    : <RefreshCw size={26} aria-hidden="true" />}
                </span>
              </div>
              <div className="trace-empty-copy">
                <h3>{loading ? "正在统计 Trace 日志" : "未获取 Diagnostic/Trace 诊断快照"}</h3>
                <p>
                  {loading
                    ? "正在扫描本地 Codex 日志库及数据库索引，请稍候…"
                    : "一键扫描本地 logs_*.sqlite 日志数据库，分析磁盘占用、写入条数与高频 Target。"}
                </p>
              </div>
              <div className="trace-empty-action">
                <Button
                  variant="default"
                  size="default"
                  disabled={disabled}
                  onClick={onRefresh}
                  className="trace-start-btn"
                >
                  {loading ? (
                    <>
                      <LoaderCircle className="spinner" aria-hidden="true" />
                      扫描分析中…
                    </>
                  ) : (
                    <>
                      <RefreshCw aria-hidden="true" />
                      立即生成诊断快照
                    </>
                  )}
                </Button>
              </div>
            </div>

            <div className="trace-skeleton-preview" aria-hidden="true">
              <div className="skeleton-card"><div className="skeleton-line short" /><div className="skeleton-line long" /></div>
              <div className="skeleton-card"><div className="skeleton-line short" /><div className="skeleton-line long" /></div>
              <div className="skeleton-card"><div className="skeleton-line short" /><div className="skeleton-line long" /></div>
              <div className="skeleton-card"><div className="skeleton-line short" /><div className="skeleton-line long" /></div>
            </div>
          </div>
        ) : (
          <>
            <div className="trace-snapshot-row">
              <div className="trace-snapshot-info">
                <span className={`trace-status-dot ${protectionEnabled ? "active" : ""}`} />
                <strong>{protectionEnabled ? "保护状态正常" : "写盘保护未开启"}</strong>
                <span>{snapshot.databasesScanned}/{snapshot.databasesFound} 个日志数据库已完成扫描</span>
              </div>
              <Badge variant={snapshot.errors.length || snapshotStale ? "warning" : "secondary"}>
                {snapshotStale ? "清理前快照 · " : ""}{formatSnapshotTime(snapshot.capturedAt)}
              </Badge>
            </div>

            <div className="trace-metrics-grid">
              <div className="trace-metric-card">
                <div className="trace-metric-content">
                  <span>日志总条数</span>
                  <strong>{formatCount(snapshot.rowCount)}</strong>
                  <small>{formatRange(snapshot)}</small>
                </div>
              </div>
              <div className="trace-metric-card">
                <div className="trace-metric-content">
                  <span>磁盘占用空间</span>
                  <strong>{formatBytes(snapshot.databaseBytes)}</strong>
                  <small>主数据库及 WAL/SHM</small>
                </div>
              </div>
              <div className="trace-metric-card">
                <div className="trace-metric-content">
                  <span>近 7 天写入</span>
                  <strong>{formatBytes(snapshot.recentEstimatedBytes)}</strong>
                  <small>{formatCount(snapshot.recentRowCount)} 条增量日志</small>
                </div>
              </div>
              <div className="trace-metric-card">
                <div className="trace-metric-content">
                  <span>内容字节估算</span>
                  <strong>{formatBytes(snapshot.estimatedLogBytes)}</strong>
                  <small>按 estimated_bytes 汇总</small>
                </div>
              </div>
            </div>

            <div className="trace-wear-note" role="note" aria-label="SSD 写入寿命粗略估算">
              <span className="trace-wear-icon" aria-hidden="true">
                <InfoCircle size={18} />
              </span>
              <div className="trace-wear-copy">
                <div className="trace-wear-heading">
                  <strong>SSD 写入寿命粗略估算</strong>
                  <Badge variant="secondary">300 TBW 参考</Badge>
                </div>
                {diskWear && diskWear.observedBytes > 0 ? (
                  <p>
                    按当前快照，折算写入约{" "}
                    <strong>
                      {formatBytes(diskWear.lowerWriteBytes)}–{formatBytes(diskWear.upperWriteBytes)}
                    </strong>
                    ，约占标称写入寿命的{" "}
                    <strong>
                      {formatWearPercentRange(diskWear.lowerWearPercent, diskWear.upperWearPercent)}
                    </strong>
                    。
                  </p>
                ) : (
                  <p>当前快照未观察到可估算的 Trace 写入损耗。</p>
                )}
                <p className="trace-wear-scope">
                  <strong>统计范围：</strong>
                  仅包含当前 logs_*.sqlite 中仍保留的日志行；
                  不包含已清理、轮转或覆盖的历史记录。
                </p>
                <small>
                  损耗口径按当前数据库/内容体量与 1–4× SQLite、文件系统及 SSD 写放大粗估，
                  不是 SSD 实际物理写入监测值；实际值取决于硬盘型号与标称 TBW。
                </small>
              </div>
            </div>

            <div className="trace-detail-grid">
              <section className="trace-daily">
                <div className="trace-subheading">
                  <strong>近 7 天写入走势</strong>
                  <span>估算容量 / 日志条数</span>
                </div>
                <div
                  className="trace-bars"
                  role="img"
                  aria-label={`近 ${daily.length} 天 Trace 日志写入估算`}
                >
                  {daily.map((item) => {
                    const width = maxDailyBytes > 0 ? (item.estimatedBytes / maxDailyBytes) * 100 : 0;
                    return (
                      <div
                        className="trace-bar-row"
                        key={item.date}
                        aria-label={`${item.date}，${formatBytes(item.estimatedBytes)}，${formatCount(item.rows)} 条`}
                      >
                        <span className="trace-bar-date">{item.date.slice(5).replace("-", "/")}</span>
                        <div className="trace-bar-track">
                          <div
                            className="trace-bar-fill"
                            style={{ width: `${Math.max(width, 3)}%` }}
                          />
                        </div>
                        <div className="trace-bar-meta">
                          <strong>{formatBytes(item.estimatedBytes)}</strong>
                          <small>{formatCount(item.rows)} 条</small>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </section>

              <section className="trace-breakdown">
                <div className="trace-subheading">
                  <strong>级别分布 (Levels)</strong>
                  <span>按容量降序</span>
                </div>
                <div className="trace-levels">
                  {snapshot.levels.length ? snapshot.levels.map((item) => (
                    <div className={`trace-level-pill level-${item.name.toLowerCase()}`} key={item.name}>
                      <span className="trace-level-name">{item.name}</span>
                      <div className="trace-level-values">
                        <strong>{formatCount(item.rows)}</strong>
                        <small>{formatBytes(item.estimatedBytes)}</small>
                      </div>
                    </div>
                  )) : (
                    <div className="trace-none-pill">
                      <span className="trace-none-dot" />
                      <span>暂无级别分布数据</span>
                    </div>
                  )}
                </div>

                <div className="trace-subheading trace-target-heading">
                  <strong>高占用 Targets</strong>
                  <span>Top {snapshot.topTargets.length}</span>
                </div>
                <div className="trace-targets">
                  {snapshot.topTargets.length ? snapshot.topTargets.map((item) => (
                    <div className="trace-target-pill" key={item.name} title={item.name}>
                      <span className="trace-target-name">{item.name}</span>
                      <div className="trace-target-values">
                        <strong>{formatBytes(item.estimatedBytes)}</strong>
                        <small>{formatCount(item.rows)} 条</small>
                      </div>
                    </div>
                  )) : (
                    <div className="trace-none-pill">
                      <span className="trace-none-dot" />
                      <span>暂无 Target 模块数据</span>
                    </div>
                  )}
                </div>
              </section>
            </div>

            {snapshot.errors.length > 0 && (
              <div className="trace-warning" title={snapshot.errors.join("\n")}>
                <CircleAlert size={15} />
                <span>{snapshot.errors.length} 个日志库统计异常，已保留其余快照数据</span>
              </div>
            )}
          </>
        )}
      </Card>
    </section>
  );
}
