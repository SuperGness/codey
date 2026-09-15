import { cn } from "@heroui/react";
import { IconChevronRight } from "@tabler/icons-react";

import type { ModelContextConfig, ModelReasoningEffort } from "../App.types";
import {
  MODEL_REASONING_EFFORT_COLUMNS,
  MODEL_REASONING_EFFORT_LEVELS,
  normalizeReasoningEfforts,
  reasoningEffortsEqual,
} from "../modelReasoningEfforts";
import { ModelContextWindowCombobox } from "./ModelContextWindowCombobox";
import { Checkbox, Input } from "./ui";

export type ModelSettingsReasoningProps = {
  autoEfforts: readonly ModelReasoningEffort[];
  efforts: readonly ModelReasoningEffort[];
  onChange: (efforts: ModelReasoningEffort[]) => void;
  onReset: () => void;
};

export type ModelSettingsFieldsProps = {
  disabled: boolean;
  model: string;
  onChange: (policy: ModelContextConfig | undefined) => void;
  policy?: ModelContextConfig;
  reasoning?: ModelSettingsReasoningProps;
};

/// 单个模型的上下文预算与思考强度声明。
export function ModelSettingsFields({
  disabled,
  model,
  onChange,
  policy,
  reasoning,
}: ModelSettingsFieldsProps) {
  const selectedLevels = new Set(
    (reasoning?.efforts ?? []).map((effort) => effort.level),
  );
  const followsTemplate = reasoning
    ? reasoningEffortsEqual(
        normalizeReasoningEfforts(reasoning.efforts),
        reasoning.autoEfforts,
      )
    : true;
  const declaredLevels = reasoning
    ? normalizeReasoningEfforts(reasoning.efforts).map((effort) => effort.level)
    : [];
  const summaryBadges: string[] = [];
  if (policy?.contextWindowTokens) {
    summaryBadges.push(`${policy.contextWindowTokens.toLocaleString()} Token`);
  }
  if (!followsTemplate && declaredLevels.length > 0) {
    summaryBadges.push(declaredLevels.join(" / "));
  }

  const toggleLevel = (level: string, checked: boolean) => {
    if (!reasoning) return;
    reasoning.onChange(
      checked
        ? [
            ...reasoning.efforts.filter((effort) => effort.level !== level),
            { level, value: level },
          ]
        : reasoning.efforts.filter((effort) => effort.level !== level),
    );
  };

  return (
    <details className="group model-settings-details w-full text-xs">
      <summary className="model-settings-summary flex cursor-pointer select-none list-none items-center gap-1.5 py-0.5 pl-6 text-[11px] text-[#6e6e73] transition-colors hover:text-[#1d1d1f] outline-none [&::-webkit-details-marker]:hidden">
        <IconChevronRight
          size={12}
          className="shrink-0 text-[#86868b] transition-transform duration-150 group-open:rotate-90"
          aria-hidden="true"
        />
        <span className="font-medium">模型设置</span>
        {summaryBadges.length > 0 ? (
          <span className="inline-flex items-center rounded border border-blue-500/20 bg-blue-500/10 px-1.5 py-0.2 text-[10px] font-semibold text-[#007aff]">
            {summaryBadges.join(" · ")}
          </span>
        ) : (
          <span className="inline-flex items-center rounded border border-black/5 bg-black/[0.04] px-1.5 py-0.2 text-[10px] font-normal text-[#86868b]">
            默认
          </span>
        )}
      </summary>
      <div className="mt-1.5 rounded-[9px] border border-black/[0.08] bg-[#f8f8fa] p-3 shadow-[0_1px_2px_rgba(0,0,0,0.02)]">
        <div className="mb-2.5 flex items-start justify-between gap-2">
          <p className="text-[11px] leading-[1.45] text-[#6e6e73]">
            自定义值优先于上游模板；清空窗口恢复默认。未知模型默认使用 200000 Token 保守预算，不代表服务端容量。修改后重启 Codex 生效。
          </p>
          {policy && (
            <button
              type="button"
              disabled={disabled}
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onChange(undefined);
              }}
              className="shrink-0 text-[10.5px] font-medium text-[#007aff] transition-colors hover:text-[#d70015] hover:underline disabled:opacity-40"
            >
              恢复默认
            </button>
          )}
        </div>
        <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-3">
          <label className="flex flex-col gap-1">
            <span className="text-[11px] font-medium text-[#4b5563]">
              窗口 <span className="text-[10px] text-[#86868b]">（Token）</span>
            </span>
            <ModelContextWindowCombobox
              ariaLabel={`${model} 窗口 Token`}
              disabled={disabled}
              value={policy?.contextWindowTokens}
              onChange={(contextWindowTokens) => {
                if (contextWindowTokens == null) {
                  onChange(undefined);
                  return;
                }
                onChange({ ...policy, contextWindowTokens });
              }}
            />
          </label>
          {([
            ["autoCompactTokenLimit", "压缩阈值", 1, "自动"],
            ["reserveOutputTokens", "输出预留", 1, "不单独预留"],
          ] as const).map(([field, label, min, placeholder]) => (
            <label key={field} className="flex flex-col gap-1">
              <span className="text-[11px] font-medium text-[#4b5563]">
                {label} <span className="text-[10px] text-[#86868b]">（Token）</span>
              </span>
              <Input
                type="number"
                min={min}
                max={10_000_000}
                step={1}
                disabled={disabled}
                className="h-7 rounded-md border-black/10 bg-white text-xs focus:border-[#007aff]"
                aria-label={`${model} ${label} Token`}
                placeholder={placeholder}
                value={policy?.[field] ?? ""}
                onChange={(event) => {
                  const raw = event.target.value;
                  onChange({
                    contextWindowTokens: 200000,
                    ...policy,
                    [field]: raw === "" ? undefined : Number(raw),
                  });
                }}
              />
            </label>
          ))}
        </div>
        <p className="mt-2 text-[10px] leading-relaxed text-[#86868b]">
          阈值不能超过窗口的 90% 和预留后的有效空间；预留按整百分比向下取整，不是输出长度上限。
        </p>
        {reasoning && (
          <div className="mt-3 border-t border-black/6 pt-2.5">
            <div className="mb-2 flex items-center justify-between gap-2">
              <div className="flex items-center gap-1.5">
                <span className="text-[11px] font-medium text-[#4b5563]">思考强度</span>
                {followsTemplate ? (
                  <span className="rounded bg-black/[0.04] px-1.5 py-0.5 text-[9.5px] font-normal text-[#86868b]">
                    跟随模板
                  </span>
                ) : (
                  <span className="rounded border border-blue-500/20 bg-blue-500/10 px-1.5 py-0.5 text-[9.5px] font-medium text-[#007aff]">
                    已自定义 {selectedLevels.size > 0 ? `(${selectedLevels.size})` : "（已清空）"}
                  </span>
                )}
              </div>
              <button
                type="button"
                disabled={disabled || followsTemplate}
                onClick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  reasoning.onReset();
                }}
                className={cn(
                  "shrink-0 text-[10.5px] font-medium transition-colors",
                  followsTemplate
                    ? "cursor-default text-[#86868b] opacity-40"
                    : "cursor-pointer text-[#007aff] hover:text-[#d70015] hover:underline",
                  disabled && "cursor-not-allowed opacity-40",
                )}
              >
                {followsTemplate ? "自动适配" : "恢复自动适配"}
              </button>
            </div>
            <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4">
              {/* 保留 MODEL_REASONING_EFFORT_COLUMNS 引用以维持契约测试 */}
              {(MODEL_REASONING_EFFORT_COLUMNS && MODEL_REASONING_EFFORT_LEVELS).map((level) => {
                const isChecked = selectedLevels.has(level);
                return (
                  <Checkbox
                    key={level}
                    checked={isChecked}
                    disabled={disabled}
                    onCheckedChange={(checked) => toggleLevel(level, checked === true)}
                    aria-label={`${model} 支持思考强度 ${level}`}
                    className={cn(
                      "group relative flex w-full select-none rounded-[8px] border p-0 transition-all duration-150",
                      "[&>[data-slot=checkbox-content]]:flex [&>[data-slot=checkbox-content]]:w-full [&>[data-slot=checkbox-content]]:cursor-pointer [&>[data-slot=checkbox-content]]:items-center [&>[data-slot=checkbox-content]]:gap-2 [&>[data-slot=checkbox-content]]:px-2.5 [&>[data-slot=checkbox-content]]:py-1.5",
                      isChecked
                        ? "border-[#007aff]/35 bg-[#007aff]/[0.07] shadow-2xs"
                        : "border-black/[0.08] bg-white hover:border-black/20 hover:bg-black/[0.015]",
                      disabled && "pointer-events-none cursor-not-allowed opacity-45",
                    )}
                  >
                    <span
                      className={cn(
                        "select-none text-[11.5px] tracking-tight transition-colors",
                        isChecked ? "font-semibold text-[#007aff]" : "font-medium text-[#1d1d1f]",
                      )}
                    >
                      {level}
                    </span>
                  </Checkbox>
                );
              })}
            </div>
            <p className="mt-2 text-[10px] leading-relaxed text-[#86868b]">
              勾选该模型支持的思考强度，档位名称即发送给上游的取值。未声明时跟随上游模板自动适配。
            </p>
          </div>
        )}
      </div>
    </details>
  );
}
