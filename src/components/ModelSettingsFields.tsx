import { useEffect, useState } from "react";
import { cn } from "@heroui/react";
import { IconChevronRight } from "@tabler/icons-react";

import type { ModelContextConfig, ModelReasoningEffort } from "../App.types";
import {
  DEFAULT_CONTEXT_WINDOW_TOKENS,
  MAX_CONTEXT_WINDOW_TOKENS,
} from "../modelContextPresets";
import { formatTokenK, parseTokenK } from "../tokenUnits";
import {
  MODEL_REASONING_EFFORT_LEVELS,
  normalizeReasoningEfforts,
  reasoningEffortsEqual,
} from "../modelReasoningEfforts";
import { ModelContextWindowCombobox } from "./ModelContextWindowCombobox";
import { Checkbox, Input } from "./ui";

export type ModelSettingsReasoningProps = {
  autoEfforts: readonly ModelReasoningEffort[];
  supportedLevels?: readonly string[];
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

type TokenKInputProps = {
  ariaLabel: string;
  disabled: boolean;
  maxTokens: number;
  minTokens: number;
  onChange: (tokens: number | undefined) => void;
  placeholder: string;
  value: number | null | undefined;
};

/// 以 K 为单位编辑 Token 数值，保留输入过程中的合法小数，失焦时回落到当前值。
export function TokenKInput({
  ariaLabel,
  disabled,
  maxTokens,
  minTokens,
  onChange,
  placeholder,
  value,
}: TokenKInputProps) {
  const [text, setText] = useState(value == null ? "" : formatTokenK(value));
  useEffect(() => {
    setText(value == null ? "" : formatTokenK(value));
  }, [value]);

  return (
    <Input
      type="text"
      inputMode="decimal"
      disabled={disabled}
      className="h-7 rounded-md border-[rgb(var(--codey-ink-rgb,0,0,0))]/10 bg-[var(--codey-surface,#fff)] text-xs focus:border-[var(--codey-blue,#007aff)]"
      aria-label={ariaLabel}
      placeholder={placeholder}
      value={text}
      onChange={(event) => {
        const raw = event.target.value;
        setText(raw);
        if (raw.trim() === "") {
          onChange(undefined);
          return;
        }
        const parsed = parseTokenK(raw);
        if (parsed == null || parsed < minTokens || parsed > maxTokens) return;
        onChange(parsed);
      }}
      onBlur={() => setText(value == null ? "" : formatTokenK(value))}
    />
  );
}

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
  const levelOptions = reasoning?.supportedLevels?.length
    ? MODEL_REASONING_EFFORT_LEVELS.filter((level) =>
        reasoning.supportedLevels!.includes(level),
      )
    : MODEL_REASONING_EFFORT_LEVELS;
  const summaryBadges: string[] = [];
  if (policy?.contextWindowTokens) {
    summaryBadges.push(`${formatTokenK(policy.contextWindowTokens)}K Token`);
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
      <summary className="model-settings-summary flex cursor-pointer select-none list-none items-center gap-1.5 py-0.5 pl-6 text-[11px] text-[var(--codey-muted,#6e6e73)] transition-colors hover:text-[var(--codey-text,#1d1d1f)] outline-none [&::-webkit-details-marker]:hidden">
        <IconChevronRight
          size={12}
          className="shrink-0 text-[var(--codey-subtle,#86868b)] transition-transform duration-150 group-open:rotate-90"
          aria-hidden="true"
        />
        <span className="font-medium">模型设置</span>
        {summaryBadges.length > 0 ? (
          <span className="inline-flex items-center rounded border border-blue-500/20 dark:border-blue-700/20 bg-blue-500/10 px-1.5 py-0.2 text-[10px] font-semibold text-[var(--codey-blue,#007aff)]">
            {summaryBadges.join(" · ")}
          </span>
        ) : (
          <span className="inline-flex items-center rounded border border-[rgb(var(--codey-ink-rgb,0,0,0))]/5 bg-[rgb(var(--codey-ink-rgb,0,0,0))]/[0.04] px-1.5 py-0.2 text-[10px] font-normal text-[var(--codey-subtle,#86868b)]">
            默认
          </span>
        )}
      </summary>
      <div className="mt-1.5 rounded-[9px] border border-[rgb(var(--codey-ink-rgb,0,0,0))]/[0.08] bg-[var(--codey-surface-sunken,#f8f8fa)] p-3 shadow-[0_1px_2px_rgba(0,0,0,0.02)]">
        <div className="mb-2.5 flex items-start justify-between gap-2">
          <p className="text-[11px] leading-[1.45] text-[var(--codey-muted,#6e6e73)]">
            自定义值优先于上游模板；清空窗口恢复默认。未知模型默认使用 {formatTokenK(DEFAULT_CONTEXT_WINDOW_TOKENS)}K Token 保守预算，不代表服务端容量。窗口与压缩阈值的修改需重启 Codex 生效。
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
              className="shrink-0 text-[10.5px] font-medium text-[var(--codey-blue,#007aff)] transition-colors hover:text-[var(--codey-red,#d70015)] hover:underline disabled:opacity-40"
            >
              恢复默认
            </button>
          )}
        </div>
        <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-3">
          <label className="flex flex-col gap-1">
            <span className="text-[11px] font-medium text-[var(--codey-text-soft,#4b5563)]">
              窗口 <span className="text-[10px] text-[var(--codey-subtle,#86868b)]">（K Token）</span>
            </span>
            <ModelContextWindowCombobox
              ariaLabel={`${model} 窗口 K Token`}
              disabled={disabled}
              placeholder="跟随模型目录"
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
            ["autoCompactTokenLimit", "压缩阈值", "自动"],
            ["reserveOutputTokens", "输出预留", "不单独预留"],
          ] as const).map(([field, label, placeholder]) => (
            <label key={field} className="flex flex-col gap-1">
              <span className="text-[11px] font-medium text-[var(--codey-text-soft,#4b5563)]">
                {label} <span className="text-[10px] text-[var(--codey-subtle,#86868b)]">（K Token）</span>
              </span>
              <TokenKInput
                disabled={disabled || !policy}
                ariaLabel={`${model} ${label} K Token`}
                placeholder={placeholder}
                maxTokens={MAX_CONTEXT_WINDOW_TOKENS}
                minTokens={1}
                value={policy?.[field]}
                onChange={(tokens) => {
                  if (!policy) return;
                  onChange({ ...policy, [field]: tokens });
                }}
              />
            </label>
          ))}
        </div>
        <p className="mt-2 text-[10px] leading-relaxed text-[var(--codey-subtle,#86868b)]">
          请先明确设置窗口，再调整压缩阈值和输出预留。阈值不能超过窗口的 90% 和预留后的有效空间；预留按整百分比向下取整，不是输出长度上限。
        </p>
        {reasoning && (
          <div className="mt-3 border-t border-[rgb(var(--codey-ink-rgb,0,0,0))]/6 pt-2.5">
            <div className="mb-2 flex items-center justify-between gap-2">
              <div className="flex items-center gap-1.5">
                <span className="text-[11px] font-medium text-[var(--codey-text-soft,#4b5563)]">思考强度</span>
                {followsTemplate ? (
                  <span className="rounded bg-[rgb(var(--codey-ink-rgb,0,0,0))]/[0.04] px-1.5 py-0.5 text-[9.5px] font-normal text-[var(--codey-subtle,#86868b)]">
                    跟随模板
                  </span>
                ) : (
                  <span className="rounded border border-blue-500/20 dark:border-blue-700/20 bg-blue-500/10 px-1.5 py-0.5 text-[9.5px] font-medium text-[var(--codey-blue,#007aff)]">
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
                    ? "cursor-default text-[var(--codey-subtle,#86868b)] opacity-40"
                    : "cursor-pointer text-[var(--codey-blue,#007aff)] hover:text-[var(--codey-red,#d70015)] hover:underline",
                  disabled && "cursor-not-allowed opacity-40",
                )}
              >
                {followsTemplate ? "自动适配" : "恢复自动适配"}
              </button>
            </div>
            <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-3">
              {levelOptions.map((level) => {
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
                        ? "border-[var(--codey-blue,#007aff)]/35 bg-[var(--codey-blue,#007aff)]/[0.07] shadow-2xs"
                        : "border-[rgb(var(--codey-ink-rgb,0,0,0))]/[0.08] bg-[var(--codey-surface,#fff)] hover:border-[rgb(var(--codey-ink-rgb,0,0,0))]/20 hover:bg-[rgb(var(--codey-ink-rgb,0,0,0))]/[0.015]",
                      disabled && "pointer-events-none cursor-not-allowed opacity-45",
                    )}
                  >
                    <span
                      className={cn(
                        "select-none text-[11.5px] tracking-tight transition-colors",
                        isChecked ? "font-semibold text-[var(--codey-blue,#007aff)]" : "font-medium text-[var(--codey-text,#1d1d1f)]",
                      )}
                    >
                      {level}
                    </span>
                  </Checkbox>
                );
              })}
            </div>
            <p className="mt-2 text-[10px] leading-relaxed text-[var(--codey-subtle,#86868b)]">
              勾选该模型支持的思考强度，档位名称即发送给上游的取值。未声明时跟随上游模板自动适配，保存后立即生效。
            </p>
          </div>
        )}
      </div>
    </details>
  );
}
