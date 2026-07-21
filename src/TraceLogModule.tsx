import {
  CircleAlert,
  LoaderCircle,
  RefreshCw,
  Trash2,
} from "lucide-react";

import {
  MagicBadge as Badge,
  MagicButton as Button,
  MagicCard as Card,
} from "./components/magicui";

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

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / (1024 ** index);
  return `${value >= 10 || index === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[index]}`;
}

function formatCount(value: number): string {
  return new Intl.NumberFormat("zh-CN").format(Number.isFinite(value) ? value : 0);
}

function formatSnapshotTime(timestamp: number): string {
  if (!timestamp) return "本次启动";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(timestamp * 1000));
}

function formatRange(stats: TraceLogStats): string {
  if (!stats.oldestTimestamp || !stats.newestTimestamp) return "暂无日志时间范围";
  const formatter = new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit" });
  return `${formatter.format(new Date(stats.oldestTimestamp * 1000))} - ${formatter.format(new Date(stats.newestTimestamp * 1000))}`;
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
          <div className="trace-empty" role="status" aria-live="polite">
            <span className="trace-empty-icon">
              {loading
                ? <LoaderCircle className="spinner" size={24} aria-hidden="true" />
                : <RefreshCw size={24} aria-hidden="true" />}
            </span>
            <div className="trace-empty-copy">
              <strong>{loading ? "正在统计 Trace 日志" : "刷新后查看日志统计"}</strong>
              <span>{loading ? "正在读取本地日志库，请稍候" : "点击刷新按钮获取最新统计信息"}</span>
            </div>
            {!loading && (
              <Button
                variant="secondary"
                size="sm"
                disabled={disabled}
                onClick={onRefresh}
              >
                <RefreshCw aria-hidden="true" />
                刷新统计
              </Button>
            )}
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
                    <div className="trace-level-pill" key={item.name}>
                      <span className="trace-level-name">{item.name}</span>
                      <div className="trace-level-values">
                        <strong>{formatCount(item.rows)}</strong>
                        <small>{formatBytes(item.estimatedBytes)}</small>
                      </div>
                    </div>
                  )) : <span className="trace-none">暂无级别数据</span>}
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
                  )) : <span className="trace-none">暂无 Target 数据</span>}
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
