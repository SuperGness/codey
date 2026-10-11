import { useEffect, useMemo, useState } from "react";
import { ComboBox, Input, ListBox } from "@heroui/react";

import {
  CONTEXT_WINDOW_PRESETS,
  MAX_CONTEXT_WINDOW_TOKENS,
  MIN_CONTEXT_WINDOW_TOKENS,
} from "../modelContextPresets";
import { formatTokenK, parseTokenK } from "../tokenUnits";

export type ModelContextWindowComboboxProps = {
  ariaLabel?: string;
  disabled?: boolean;
  onChange: (value: number | undefined) => void;
  placeholder?: string;
  value: number | undefined;
};

export function ModelContextWindowCombobox({
  ariaLabel = "上下文窗口",
  disabled = false,
  onChange,
  placeholder = "256",
  value,
}: ModelContextWindowComboboxProps) {
  const [text, setText] = useState(value == null ? "" : formatTokenK(value));
  useEffect(() => {
    setText(value == null ? "" : formatTokenK(value));
  }, [value]);

  const data = useMemo(() => {
    const query = text.trim().toLocaleLowerCase();
    const matched = query
      ? CONTEXT_WINDOW_PRESETS.filter(
          (preset) =>
            `${formatTokenK(preset.value)}K`.toLocaleLowerCase().includes(query) ||
            preset.label.toLocaleLowerCase().includes(query),
        )
      : CONTEXT_WINDOW_PRESETS;
    return (matched.length ? matched : CONTEXT_WINDOW_PRESETS).map((preset) => ({
      id: String(preset.value),
      label: preset.label,
      textValue: formatTokenK(preset.value),
    }));
  }, [text]);

  const commit = (raw: string) => {
    setText(raw);
    const normalized = raw.trim();
    if (!normalized) {
      onChange(undefined);
      return;
    }
    const parsed = parseTokenK(normalized);
    if (
      parsed != null &&
      parsed >= MIN_CONTEXT_WINDOW_TOKENS &&
      parsed <= MAX_CONTEXT_WINDOW_TOKENS
    ) {
      onChange(parsed);
    }
  };

  return (
    <ComboBox
      aria-label={ariaLabel}
      allowsCustomValue
      fullWidth
      className="w-full min-w-0"
      items={data}
      inputValue={text}
      isDisabled={disabled}
      menuTrigger="focus"
      onInputChange={commit}
      onBlur={() => setText(value == null ? "" : formatTokenK(value))}
      onSelectionChange={(key) => {
        if (key == null) return;
        const next = Number(String(key));
        if (!Number.isFinite(next)) return;
        setText(formatTokenK(next));
        onChange(next);
      }}
    >
      <ComboBox.InputGroup>
        <Input
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          inputMode="decimal"
          className="h-7 min-h-7"
        />
        <ComboBox.Trigger />
      </ComboBox.InputGroup>
      <ComboBox.Popover className="w-(--trigger-width) max-w-[calc(100vw-32px)]">
        <ListBox aria-label={ariaLabel} className="max-h-[260px] overflow-y-auto">
          {(option: { id: string; label: string; textValue: string }) => (
            <ListBox.Item id={option.id} textValue={option.textValue}>
              <span className="min-w-0 flex-1 truncate">{option.label}</span>
            </ListBox.Item>
          )}
        </ListBox>
      </ComboBox.Popover>
    </ComboBox>
  );
}
