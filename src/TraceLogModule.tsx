import { memo } from "react";
import {
  IconAlertCircle as CircleAlert,
  IconArchive as Archive,
  IconDatabase as Database,
  IconFileDescription as FileText,
  IconListDetails as ListDetails,
  IconLoader2 as LoaderCircle,
  IconRefresh as RefreshCw,
  IconReport as Report,
  IconTrash as Trash2,
} from "@tabler/icons-react";

import { Badge, Button, Card } from "./components/antd";
import { formatBytes } from "./formatters";
import type { CrashpadPendingStats, TraceLogStats } from "./traceLogTypes";
import { surfaceCardPaddingClass } from "./uiClasses";

type TraceLogModuleProps = {
  stats?: TraceLogStats;
  crashpadStats?: CrashpadPendingStats;
  crashpadSupported: boolean;
  traceProtectionEnabled: boolean;
  crashpadProtectionEnabled: boolean;
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

function formatCount(value: number): string {
  return countFormatter.format(Number.isFinite(value) ? value : 0);
}

function formatSnapshotTime(timestamp: number): string {
  if (!timestamp) return "本次启动";
  return snapshotTimeFormatter.format(new Date(timestamp * 1000));
}

function formatRange(
  oldestTimestamp?: number,
  newestTimestamp?: number,
  fallback = "暂无时间范围",
): string {
  if (!oldestTimestamp || !newestTimestamp) return fallback;
  return `${rangeDateFormatter.format(new Date(oldestTimestamp * 1000))} - ${rangeDateFormatter.format(new Date(newestTimestamp * 1000))}`;
}

function TraceLogModuleComponent({
  stats,
  crashpadStats,
  crashpadSupported,
  traceProtectionEnabled,
  crashpadProtectionEnabled,
  clearBusy,
  refreshing,
  disabled,
  onClear,
  onRefresh,
}: TraceLogModuleProps) {
  const loading = refreshing || Boolean(stats?.pending || crashpadStats?.pending);
  const traceSnapshot =
    stats && stats.capturedAt > 0 && !stats.pending ? stats : undefined;
  const crashpadSnapshot =
    crashpadStats && crashpadStats.capturedAt > 0 && !crashpadStats.pending
      ? crashpadStats
      : undefined;
  const hasSnapshot =
    Boolean(traceSnapshot) &&
    (!crashpadSupported || Boolean(crashpadSnapshot));
  const protectionsEnabled =
    traceProtectionEnabled &&
    (!crashpadSupported || crashpadProtectionEnabled);
  const partiallyProtected =
    traceProtectionEnabled ||
    (crashpadSupported && crashpadProtectionEnabled);
  const warningCount =
    (traceSnapshot?.errors.length ?? 0) +
    (crashpadSnapshot?.errors.length ?? 0);
  const capturedAt = Math.max(
    traceSnapshot?.capturedAt ?? 0,
    crashpadSnapshot?.capturedAt ?? 0,
  );

  return (
    <section className="trace-section" aria-labelledby="trace-title">
      <div className="section-title compact trace-section-title">
        <div className="section-heading">
          <span className="section-icon" aria-hidden="true">
            <Archive size={15} />
          </span>
          <div>
            <h2 id="trace-title">诊断存储保护</h2>
            <p>
              {crashpadSupported
                ? "Trace 数据库 · Crashpad 待处理报告"
                : "Trace 数据库写盘与占用分析"}
            </p>
          </div>
        </div>
        <div className="trace-module-actions">
          <Badge
            variant={
              protectionsEnabled
                ? "success"
                : partiallyProtected
                  ? "warning"
                  : "secondary"
            }
          >
            {protectionsEnabled
              ? crashpadSupported
                ? "双重保护已开启"
                : "Trace 保护已开启"
              : partiallyProtected
                ? "部分保护已开启"
                : "存储保护关闭"}
          </Badge>
          <Button
            variant="outline"
            size="sm"
            disabled={disabled}
            onClick={onRefresh}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} aria-hidden="true" />
            <span>刷新统计</span>
          </Button>
          <Button
            variant="destructive-light"
            size="sm"
            disabled={disabled}
            onClick={onClear}
          >
            {clearBusy
              ? <LoaderCircle size={12} className="animate-spin" aria-hidden="true" />
              : <Trash2 size={12} aria-hidden="true" />}
            <span>清理诊断存储</span>
          </Button>
        </div>
      </div>

      <Card
        className={`trace-card ${surfaceCardPaddingClass}${hasSnapshot ? "" : " trace-card-empty"}`}
        aria-busy={loading}
      >
        {!hasSnapshot ? (
          <div className="trace-empty-container">
            <div className="trace-empty" role="status" aria-live="polite">
              <div className="trace-empty-badge">
                <span className="trace-empty-icon">
                  {loading
                    ? <LoaderCircle className="animate-spin" size={28} aria-hidden="true" />
                    : <RefreshCw size={26} aria-hidden="true" />}
                </span>
              </div>
              <div className="trace-empty-copy">
                <h3>
                  {loading
                    ? "正在统计诊断存储"
                    : "未获取本地诊断存储快照"}
                </h3>
                <p>
                  {loading
                    ? crashpadSupported
                      ? "正在扫描 Trace 数据库与 Crashpad 待处理报告，请稍候…"
                      : "正在扫描本地 Trace 数据库，请稍候…"
                    : crashpadSupported
                      ? "一键统计 logs_*.sqlite 与 Crashpad pending 的数量、时间范围和磁盘占用。"
                      : "一键统计 logs_*.sqlite 的数量、时间范围和磁盘占用。"}
                </p>
              </div>
              <div className="trace-empty-action">
                <Button
                  variant="default"
                  size="default"
                  disabled={disabled}
                  onClick={onRefresh}
                >
                  {loading ? (
                    <>
                      <LoaderCircle className="animate-spin" aria-hidden="true" />
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
          </div>
        ) : (
          <>
            <div className="trace-snapshot-row">
              <div className="trace-snapshot-info">
                <span
                  className={`trace-status-dot ${protectionsEnabled ? "active" : ""}`}
                />
                <strong>
                  {protectionsEnabled ? "保护状态正常" : "存在未启用的保护策略"}
                </strong>
                <span>
                  Trace {traceSnapshot?.databasesScanned ?? 0}/
                  {traceSnapshot?.databasesFound ?? 0} 个数据库
                  {crashpadSupported
                    ? ` · Crashpad ${crashpadSnapshot?.completeReports ?? 0} 份完整报告`
                    : ""}
                </span>
              </div>
              <Badge
                variant={
                  warningCount || crashpadSnapshot?.overLimit
                    ? "warning"
                    : "secondary"
                }
              >
                {formatSnapshotTime(capturedAt)}
              </Badge>
            </div>

            <div className={`trace-metrics-grid ${crashpadSupported ? "has-crashpad" : ""}`}>
              <div className="trace-metric-card trace-metric-card-rows">
                <span className="trace-metric-icon" aria-hidden="true">
                  <ListDetails size={20} />
                </span>
                <div className="trace-metric-content">
                  <span>日志总条数</span>
                  <strong>{formatCount(traceSnapshot?.rowCount ?? 0)}</strong>
                  <small>
                    {formatRange(
                      traceSnapshot?.oldestTimestamp,
                      traceSnapshot?.newestTimestamp,
                      "暂无 Trace 时间范围",
                    )}
                  </small>
                </div>
              </div>
              <div className="trace-metric-card trace-metric-card-storage">
                <span className="trace-metric-icon" aria-hidden="true">
                  <Database size={20} />
                </span>
                <div className="trace-metric-content">
                  <span>Trace 磁盘占用</span>
                  <strong>
                    {formatBytes(traceSnapshot?.databaseBytes ?? 0)}
                  </strong>
                  <small>主数据库及 WAL/SHM</small>
                </div>
              </div>
              <div className="trace-metric-card trace-metric-card-content">
                <span className="trace-metric-icon" aria-hidden="true">
                  <FileText size={20} />
                </span>
                <div className="trace-metric-content">
                  <span>内容字节估算</span>
                  <strong>
                    {formatBytes(traceSnapshot?.estimatedLogBytes ?? 0)}
                  </strong>
                  <small>按 estimated_bytes 汇总</small>
                </div>
              </div>
              {crashpadSupported && (
                <>
                  <div className="trace-metric-card trace-metric-card-crashpad-reports">
                    <span className="trace-metric-icon" aria-hidden="true">
                      <Report size={20} />
                    </span>
                    <div className="trace-metric-content">
                      <span>Crashpad 报告</span>
                      <strong>
                        {formatCount(crashpadSnapshot?.completeReports ?? 0)}
                      </strong>
                      <small>
                        {formatCount(crashpadSnapshot?.filesFound ?? 0)} 个待处理文件
                      </small>
                    </div>
                  </div>
                  <div className="trace-metric-card trace-metric-card-crashpad-storage">
                    <span className="trace-metric-icon" aria-hidden="true">
                      <Archive size={20} />
                    </span>
                    <div className="trace-metric-content">
                      <span>Crashpad 占用</span>
                      <strong>
                        {formatBytes(crashpadSnapshot?.pendingBytes ?? 0)}
                      </strong>
                      <small>
                        上限{" "}
                        {formatBytes(
                          crashpadSnapshot?.hardLimitBytes ??
                            512 * 1024 * 1024,
                        )}
                      </small>
                    </div>
                  </div>
                </>
              )}
            </div>

            {(warningCount > 0 || crashpadSnapshot?.overLimit) && (
              <div
                className="trace-warning"
                title={[
                  ...(traceSnapshot?.errors ?? []),
                  ...(crashpadSnapshot?.errors ?? []),
                ].join("\n")}
              >
                <CircleAlert size={15} />
                <span>
                  {crashpadSnapshot?.overLimit
                    ? "Crashpad 占用仍高于安全上限；最近写入的报告已保留，后台会继续收敛"
                    : `${warningCount} 项诊断存储统计异常，已保留其余快照数据`}
                </span>
              </div>
            )}
          </>
        )}
      </Card>
    </section>
  );
}

export const TraceLogModule = memo(TraceLogModuleComponent);
