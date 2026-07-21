import { CircleAlert, LoaderCircle, ShieldCheck, Trash2 } from "lucide-react";

import {
  MagicBadge as Badge,
  MagicButton as Button,
  MagicCard as Card,
  MagicSwitch as Switch,
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
  busy: boolean;
  disabled: boolean;
  onProtectionChange: (checked: boolean) => void;
  onClear: () => void;
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
  busy,
  disabled,
  onProtectionChange,
  onClear,
}: TraceLogModuleProps) {
  const daily = stats ? normalizedDailyStats(stats) : [];
  const maxDailyBytes = Math.max(0, ...daily.map((item) => item.estimatedBytes));

  return (
    <section className="trace-section" aria-labelledby="trace-title">
      <div className="section-title compact trace-section-title">
        <div>
          <h2 id="trace-title">Trace 日志</h2>
          <p>启动时快照 · 运行期间不重复扫描</p>
        </div>
        <div className="trace-module-actions">
          <Badge variant={protectionEnabled ? "success" : "secondary"}>
            {protectionEnabled ? "写盘保护开启" : "写盘保护关闭"}
          </Badge>
          <Switch
            checked={protectionEnabled}
            disabled={disabled}
            onCheckedChange={onProtectionChange}
            aria-label="控制 Codex Trace 日志写盘保护"
          />
          <Button
            variant="ghost"
            size="sm"
            disabled={disabled}
            onClick={onClear}
          >
            {busy
              ? <LoaderCircle className="spinner" aria-hidden="true" />
              : <Trash2 aria-hidden="true" />}
            清理日志库
          </Button>
        </div>
      </div>

      <Card className="trace-card">
        {!stats || stats.pending ? (
          <div className="trace-empty">
            {stats?.pending
              ? <LoaderCircle className="spinner" size={30} />
              : <ShieldCheck size={30} />}
            <strong>{stats?.pending ? "正在后台统计日志库" : "本次启动尚无统计快照"}</strong>
          </div>
        ) : (
          <>
            <div className="trace-snapshot-row">
              <div>
                <ShieldCheck size={16} />
                <strong>{protectionEnabled ? "保护已启用" : "保护已关闭"}</strong>
                <span>{stats.databasesScanned}/{stats.databasesFound} 个日志库已统计</span>
              </div>
              <Badge variant={stats.errors.length || snapshotStale ? "warning" : "secondary"}>
                {snapshotStale ? "清理前快照 · " : ""}{formatSnapshotTime(stats.capturedAt)}
              </Badge>
            </div>

            <div className="trace-metrics">
              <div className="trace-metric">
                <span>日志条数</span>
                <strong>{formatCount(stats.rowCount)}</strong>
                <small>{formatRange(stats)}</small>
              </div>
              <div className="trace-metric">
                <span>磁盘占用</span>
                <strong>{formatBytes(stats.databaseBytes)}</strong>
                <small>主库及 WAL/SHM</small>
              </div>
              <div className="trace-metric">
                <span>近 7 天写入估算</span>
                <strong>{formatBytes(stats.recentEstimatedBytes)}</strong>
                <small>{formatCount(stats.recentRowCount)} 条日志</small>
              </div>
              <div className="trace-metric">
                <span>日志内容估算</span>
                <strong>{formatBytes(stats.estimatedLogBytes)}</strong>
                <small>按 estimated_bytes 汇总</small>
              </div>
            </div>

            <div className="trace-detail-grid">
              <section className="trace-daily">
                <div className="trace-subheading">
                  <strong>近 7 天写入</strong>
                  <span>内容估算 / 条数</span>
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
                        <span>{item.date.slice(5).replace("-", "/")}</span>
                        <div className="trace-bar-track">
                          <span style={{ width: `${width}%` }} />
                        </div>
                        <strong>{formatBytes(item.estimatedBytes)}</strong>
                        <small>{formatCount(item.rows)} 条</small>
                      </div>
                    );
                  })}
                </div>
              </section>

              <section className="trace-breakdown">
                <div className="trace-subheading">
                  <strong>级别分布</strong>
                  <span>按内容估算排序</span>
                </div>
                <div className="trace-levels">
                  {stats.levels.length ? stats.levels.map((item) => (
                    <div className="trace-level" key={item.name}>
                      <span>{item.name}</span>
                      <strong>{formatCount(item.rows)}</strong>
                      <small>{formatBytes(item.estimatedBytes)}</small>
                    </div>
                  )) : <span className="trace-none">暂无级别数据</span>}
                </div>

                <div className="trace-subheading trace-target-heading">
                  <strong>高占用 Target</strong>
                  <span>Top {stats.topTargets.length}</span>
                </div>
                <div className="trace-targets">
                  {stats.topTargets.length ? stats.topTargets.map((item) => (
                    <div className="trace-target" key={item.name} title={item.name}>
                      <span>{item.name}</span>
                      <strong>{formatBytes(item.estimatedBytes)}</strong>
                      <small>{formatCount(item.rows)} 条</small>
                    </div>
                  )) : <span className="trace-none">暂无 Target 数据</span>}
                </div>
              </section>
            </div>

            {stats.errors.length > 0 && (
              <div className="trace-warning" title={stats.errors.join("\n")}>
                <CircleAlert size={15} />
                <span>{stats.errors.length} 个日志库统计异常，已保留其余快照</span>
              </div>
            )}
          </>
        )}
      </Card>
    </section>
  );
}
