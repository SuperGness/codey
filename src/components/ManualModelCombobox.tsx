import { useMemo } from "react";
import { ComboBox, Input, ListBox } from "@heroui/react";

export type ManualModelComboboxProps = {
  ariaDescribedBy?: string;
  ariaInvalid?: boolean;
  ariaLabel?: string;
  disabled?: boolean;
  id?: string;
  onChange: (value: string) => void;
  options: string[];
  placeholder?: string;
  value: string;
};

export function ManualModelCombobox({
  ariaDescribedBy,
  ariaInvalid,
  ariaLabel = "模型",
  disabled = false,
  id,
  onChange,
  options,
  placeholder = "例如 gpt-4o-mini 或 deepseek-chat",
  value,
}: ManualModelComboboxProps) {
  const data = useMemo(() => {
    const rawTrimmed = value.trim();
    const trimmed = rawTrimmed.startsWith("使用 ") ? rawTrimmed.slice(3).trim() : rawTrimmed;
    const query = trimmed.toLocaleLowerCase();
    const matched = options.filter((option) => option.toLocaleLowerCase().includes(query));
    const suggestions = (matched.length ? matched : options).map((option) => ({
      id: option,
      label: option,
      textValue: option,
    }));
    return trimmed && !options.some((option) => option.toLocaleLowerCase() === query)
      ? [{ id: trimmed, label: `使用 ${trimmed}`, textValue: trimmed }, ...suggestions]
      : suggestions;
  }, [options, value]);

  return (
    <ComboBox
      aria-label={ariaLabel}
      allowsCustomValue
      fullWidth
      className="w-full min-w-0"
      items={data}
      inputValue={value}
      isDisabled={disabled}
      isInvalid={ariaInvalid}
      menuTrigger="focus"
      onInputChange={onChange}
      onSelectionChange={(key) => {
        if (key != null) {
          const raw = String(key);
          const cleaned = raw.startsWith("使用 ") ? raw.slice(3).trim() : raw;
          onChange(cleaned);
        }
      }}
    >
      <ComboBox.InputGroup>
        <Input
          id={id}
          placeholder={placeholder}
          aria-describedby={ariaDescribedBy}
          autoComplete="off"
          spellCheck={false}
          className="min-h-8 md:min-h-8"
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
