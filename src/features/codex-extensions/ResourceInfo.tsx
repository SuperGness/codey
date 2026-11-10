import type { EditorDraft, Inventory } from "./types";
import { configurationLabel, dependencyStatus, scopeLabel } from "./state";
import { IconFolder, IconInfoCircle } from "@tabler/icons-react";

export function ResourceInfo({
  entry,
  inventory,
}: {
  entry: NonNullable<EditorDraft["entry"]>;
  inventory?: Inventory | null;
}) {
  const scope = entry.scope ?? inventory?.scope.kind;
  const dependents =
    "transport" in entry
      ? (inventory?.skills.filter(
          (skill) =>
            skill.scope === scope &&
            skill.dependencies?.some(
              (dependency) =>
                dependency.type.toLowerCase() === "mcp" &&
                (dependency.name === entry.name ||
                  dependency.name === entry.id),
            ),
        ) ?? [])
      : [];
  const sourceDisplay =
    "origin" in entry && entry.origin ? entry.origin : entry.sourcePath;

  return (
    <section
      className="space-y-3 rounded-xl border border-black/[0.08] bg-black/[0.02] p-3.5 text-xs dark:border-white/[0.08] dark:bg-white/[0.03]"
      aria-label="资源信息"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1.5 font-semibold text-foreground">
          <IconInfoCircle
            size={15}
            className="text-blue-600 dark:text-blue-400"
            aria-hidden="true"
          />
          <span>资源信息</span>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="extension-tag">范围：{scopeLabel(scope)}</span>
          <span
            className={`extension-tag ${
              entry.configurationStatus === "valid"
                ? "is-green"
                : entry.configurationStatus === "invalid"
                  ? "bg-[var(--codey-red-soft)] text-[var(--codey-red)]"
                  : ""
            }`}
          >
            配置：{configurationLabel(entry.configurationStatus)}
          </span>
          {"version" in entry && entry.version && (
            <span className="extension-tag is-mono">v{entry.version}</span>
          )}
        </div>
      </div>

      <div className="space-y-1.5 text-muted">
        <div className="flex items-start gap-2 rounded-lg bg-black/[0.03] px-2.5 py-1.5 font-mono text-[11.5px] dark:bg-white/[0.04]">
          <IconFolder
            size={13}
            className="mt-0.5 shrink-0 opacity-70"
            aria-hidden="true"
          />
          <span className="min-w-0 flex-1 break-all select-text text-foreground/85">
            {sourceDisplay}
          </span>
        </div>
        {entry.reason && <p className="m-0 text-muted">{entry.reason}</p>}
        {entry.updatedAt && (
          <p className="m-0 text-[11.5px]">
            <span className="font-medium text-foreground/80">更新时间：</span>
            {new Date(entry.updatedAt).toLocaleString("zh-CN")}
          </p>
        )}
      </div>

      <div className="border-t border-black/[0.06] pt-2.5 dark:border-white/[0.06]">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <p className="m-0 font-semibold text-foreground">
            {"dependencies" in entry || "ownership" in entry
              ? "声明的依赖"
              : "引用此 MCP 的 Skill"}
          </p>
          <span className="text-[11px] text-muted">
            仅展示当前范围内的资源声明
          </span>
        </div>
        {"ownership" in entry ? (
          <>
            {entry.dependencies?.length ? (
              <ul className="mt-2 space-y-1.5 pl-0 list-none text-muted">
                {entry.dependencies.map((dependency, index) => (
                  <li
                    key={index}
                    className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-black/[0.025] px-2.5 py-1.5 break-words dark:bg-white/[0.03]"
                  >
                    <span className="font-mono font-medium text-foreground">
                      {dependency.type} · {dependency.name}
                    </span>
                    <span className="text-[11.5px] text-muted">
                      {dependencyStatus(dependency, scope, inventory)}
                    </span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="m-0 mt-1.5 text-muted">未声明依赖。</p>
            )}
            {entry.dependencyWarnings?.map((warning, index) => (
              <p key={index} className="m-0 mt-1.5 text-warning">
                {warning}
              </p>
            ))}
          </>
        ) : dependents.length ? (
          <ul className="mt-2 space-y-1.5 pl-0 list-none text-muted">
            {dependents.map((skill) => (
              <li
                key={skill.id}
                className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-black/[0.025] px-2.5 py-1.5 break-words dark:bg-white/[0.03]"
              >
                <span className="font-medium text-foreground">{skill.name}</span>
                <span className="extension-tag">
                  {skill.enabledKnown === false
                    ? "状态待确认"
                    : skill.enabled
                      ? "已启用"
                      : "已禁用"}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="m-0 mt-1.5 text-muted">
            当前范围未发现 Skill 声明此依赖。
          </p>
        )}
      </div>
    </section>
  );
}
