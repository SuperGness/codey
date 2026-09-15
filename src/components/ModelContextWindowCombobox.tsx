import { useEffect, useMemo, useState } from "react";
import { ComboBox, Input, ListBox } from "@heroui/react";

import {
  CONTEXT_WINDOW_PRESETS,
  MAX_CONTEXT_WINDOW_TOKENS,
  MIN_CONTEXT_WINDOW_TOKENS,
} from "../modelContextPresets";

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
  placeholder = "256K",
  value,
}: ModelContextWindowComboboxProps) {
  const [text, setText] = useState(value == null ? "" : String(value));
  useEffect(() => {
    setText(value == null ? "" : String(value));
  }, [value]);

  const data = useMemo(() => {
    const query = text.trim().toLocaleLowerCase();
    const matched = query
      ? CONTEXT_WINDOW_PRESETS.filter(
          (preset) =>
            String(preset.value).includes(query) ||
            preset.label.toLocaleLowerCase().includes(query),
        )
      : CONTEXT_WINDOW_PRESETS;
    return (matched.length ? matched : CONTEXT_WINDOW_PRESETS).map((preset) => ({
      id: String(preset.value),
      label: `${preset.label}（${preset.value} Token）`,
      textValue: preset.label,
    }));
  }, [text]);

  const commit = (raw: string) => {
    setText(raw);
    const digits = raw.replace(/[^0-9]/g, "");
    if (!digits) {
      onChange(undefined);
      return;
    }
    const parsed = Number(digits);
    if (
      Number.isFinite(parsed) &&
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
      onBlur={() => setText(value == null ? "" : String(value))}
      onSelectionChange={(key) => {
        if (key == null) return;
        const next = Number(String(key));
        if (!Number.isFinite(next)) return;
        setText(String(next));
        onChange(next);
      }}
    >
      <ComboBox.InputGroup>
        <Input
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          inputMode="numeric"
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
