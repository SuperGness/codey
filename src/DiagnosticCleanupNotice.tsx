import { memo } from "react";
import {
  IconAlertTriangle,
  IconArrowRight,
  IconDatabase,
  IconFileText,
  IconReport,
  IconTrash,
} from "@tabler/icons-react";
import type {
  DiagnosticStorageCleanup,
  DiagnosticStorageTarget,
} from "./diagnosticStorage";
import { formatBytes } from "./formatters";

export interface DiagnosticCleanupNoticeProps {
  result: DiagnosticStorageCleanup;
  target: DiagnosticStorageTarget;
}

export const DiagnosticCleanupNotice = memo(function DiagnosticCleanupNotice({
  result,
  target,
}: DiagnosticCleanupNoticeProps) {
  const isTrace = target === "trace";
  const isCrashpad = target === "crashpad";

  // Trace 日志数据
  const trace = result.traceCleanup;
  const traceBefore =
    trace?.bytesBefore ?? result.traceLogStatsBefore?.databaseBytes;
  const traceAfter = result.traceLogStats.capturedAt
    ? result.traceLogStats.databaseBytes
    : null;
  const traceReclaimed = trace?.bytesReclaimed ?? 0;

  // Crashpad 数据
  const crashpad = result.crashpadCleanup;
  const crashpadCaptured = Boolean(result.crashpadPendingStats.capturedAt);
  const crashpadBefore = crashpadCaptured ? crashpad.bytesBefore : null;
  const crashpadAfter = crashpadCaptured ? crashpad.bytesAfter : null;
  const crashpadReclaimed = crashpad.bytesReclaimed ?? 0;

  const totalReclaimed = isTrace ? traceReclaimed : crashpadReclaimed;
  const totalBefore = isTrace ? traceBefore : crashpadBefore;
  const totalAfter = isTrace ? traceAfter : crashpadAfter;

  const hasSavings = totalReclaimed > 0;
  const isTraceFailed = isTrace && !trace;

  return (
    <div
      className="cleanup-notice-container"
      role="region"
      aria-label="诊断存储清理结果"
    >
      {/* 顶部汇总卡片 */}
      <div className="cleanup-hero-card">
        <div className="cleanup-hero-stat">
          <span className="cleanup-stat-label">释放空间</span>
          <div className="cleanup-stat-value-group">
            <span
              className={`cleanup-stat-value ${hasSavings ? "has-savings" : "no-savings"}`}
            >
              {isTraceFailed ? "未能统计" : formatBytes(totalReclaimed)}
            </span>
            {!hasSavings && !isTraceFailed && (
              <span className="cleanup-stat-subtag">无需额外释放</span>
            )}
          </div>
        </div>

        <div className="cleanup-hero-change">
          <span className="cleanup-stat-label">占用变化</span>
          <div className="cleanup-change-flow">
            <span className="cleanup-change-from">
              {totalBefore == null ? "未能统计" : formatBytes(totalBefore)}
            </span>
            <IconArrowRight
              size={11}
              className="cleanup-arrow"
              aria-hidden="true"
            />
            <span className="cleanup-change-to">
              {totalAfter == null ? "未能统计" : formatBytes(totalAfter)}
            </span>
          </div>
        </div>
      </div>

      {/* 单一目标：Trace 详情 */}
      {isTrace && (
        <>
          {trace ? (
            <div className="cleanup-details-list">
              <span className="cleanup-detail-pill">
                <IconDatabase
                  size={13}
                  className="cleanup-pill-icon"
                  aria-hidden="true"
                />
                <span>
                  已处理 <strong>{trace.databasesCleaned}</strong> 个日志库
                </span>
              </span>
              <span className="cleanup-detail-pill">
                <IconTrash
                  size={13}
                  className="cleanup-pill-icon"
                  aria-hidden="true"
                />
                <span>
                  删除 <strong>{trace.rowsDeleted}</strong> 条记录
                </span>
              </span>
            </div>
          ) : (
            <div className="cleanup-incomplete-notice">
              清理未完成，部分日志可能已清理，请查看错误详情
            </div>
          )}
        </>
      )}

      {/* 单一目标：Crashpad 详情 */}
      {isCrashpad && (
        <div className="cleanup-details-wrapper">
          <div className="cleanup-details-list">
            <span className="cleanup-detail-pill">
              <IconReport
                size={13}
                className="cleanup-pill-icon"
                aria-hidden="true"
              />
              <span>
                删除 <strong>{crashpad.reportsDeleted}</strong> 份报告
              </span>
            </span>
            <span className="cleanup-detail-pill">
              <IconFileText
                size={13}
                className="cleanup-pill-icon"
                aria-hidden="true"
              />
              <span>
                删除 <strong>{crashpad.filesDeleted}</strong> 个文件
              </span>
            </span>
          </div>
          {(Boolean(crashpad.skippedRecentReports) ||
            Boolean(crashpad.unmanagedFiles)) && (
            <div className="cleanup-retained-row">
              {Boolean(crashpad.skippedRecentReports) && (
                <span className="cleanup-retained-pill">
                  保留近期写入报告：{crashpad.skippedRecentReports} 份
                </span>
              )}
              {Boolean(crashpad.unmanagedFiles) && (
                <span className="cleanup-retained-pill">
                  保留未知文件：{crashpad.unmanagedFiles} 个
                </span>
              )}
            </div>
          )}
        </div>
      )}

      {/* 错误与未完成列表 */}
      {result.errors.length > 0 && (
        <div className="cleanup-errors-alert" role="alert">
          <div className="cleanup-errors-header">
            <IconAlertTriangle
              size={13}
              className="shrink-0"
              aria-hidden="true"
            />
            <span>未完成项（占用统计可能不完整）</span>
          </div>
          <ul className="cleanup-errors-list">
            {result.errors.map((err, idx) => (
              <li key={idx}>{err}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
});
