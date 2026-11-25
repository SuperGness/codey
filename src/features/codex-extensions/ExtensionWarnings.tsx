import { IconAlertTriangle, IconChevronDown } from "@tabler/icons-react";
import { summarizeWarnings } from "./state";

export function ExtensionWarnings({
  warnings,
  className,
}: {
  warnings?: string[];
  className?: string;
}) {
  const summary = summarizeWarnings(warnings ?? []);
  if (summary.total === 0) return null;

  const { symlinkPaths, otherWarnings } = summary;
  if (symlinkPaths.length === 0 && otherWarnings.length === 1) {
    return (
      <div
        role="status"
        className={`extension-warning-banner ${className ?? ""}`.trim()}
      >
        <IconAlertTriangle
          size={14}
          className="mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <span className="min-w-0 flex-1 break-words">{otherWarnings[0]}</span>
      </div>
    );
  }

  const headline =
    otherWarnings.length === 0
      ? `已跳过 ${symlinkPaths.length} 个符号链接条目（出于安全边界未跟随链接）`
      : symlinkPaths.length > 0
        ? `有 ${summary.total} 条提示（含 ${symlinkPaths.length} 个已跳过的符号链接条目）`
        : `有 ${otherWarnings.length} 条提示：${otherWarnings[0]}`;

  const expandLabel =
    otherWarnings.length === 0
      ? "查看路径"
      : symlinkPaths.length > 0
        ? "查看详情"
        : "查看全部";

  return (
    <div role="status" className={className}>
      <details className="extension-warning-disclosure group">
        <summary className="extension-warning-summary">
          <IconAlertTriangle
            size={14}
            className="shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0 flex-1 truncate" title={headline}>
            {headline}
          </span>
          <span className="extension-warning-toggle">
            <span className="group-open:hidden">{expandLabel}</span>
            <span className="hidden group-open:inline">收起</span>
            <IconChevronDown
              size={13}
              className="transition-transform duration-150 group-open:rotate-180"
              aria-hidden="true"
            />
          </span>
        </summary>

        <div className="extension-warning-body">
          {otherWarnings.length > 0 && (
            <ul className="extension-warning-list">
              {otherWarnings.map((warning, index) => (
                <li key={index} className="extension-warning-list-item">
                  {warning}
                </li>
              ))}
            </ul>
          )}

          {symlinkPaths.length > 0 && (
            <div className="extension-warning-symlink-section">
              <p className="extension-warning-note">
                已跳过以下 {symlinkPaths.length} 个符号链接条目；出于安全边界本页不跟随符号链接，原有外部文件不受影响。
              </p>
              <ul className="extension-warning-paths">
                {symlinkPaths.map((path) => (
                  <li key={path} className="extension-warning-path-item">
                    <span
                      className="min-w-0 flex-1 font-mono select-text break-all"
                      title={path}
                    >
                      {path}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      </details>
    </div>
  );
}
