import { useMemo } from "react";
import { AutoComplete, Input } from "antd";
import { IconRobot } from "@tabler/icons-react";

export type ManualModelComboboxProps = {
  ariaDescribedBy?: string;
  ariaInvalid?: boolean;
  ariaLabel?: string;
  disabled?: boolean;
  getPopupContainer?: () => HTMLElement;
  id?: string;
  onChange: (value: string) => void;
  options: string[];
  placeholder?: string;
  value: string;
  zIndex?: number;
};
export function ManualModelCombobox({ ariaDescribedBy, ariaInvalid, ariaLabel = "模型", disabled = false, getPopupContainer, id, onChange, options, placeholder = "例如 gpt-4o-mini 或 deepseek-chat", value, zIndex }: ManualModelComboboxProps) {
  const data = useMemo(() => {
    const trimmed = value.trim();
    const query = trimmed.toLocaleLowerCase();
    const matched = options.filter((option) => option.toLocaleLowerCase().includes(query));
    const suggestions = (matched.length ? matched : options).map((option) => ({ value: option, label: option }));
    return trimmed && !options.some((option) => option.toLocaleLowerCase() === query)
      ? [{ value: trimmed, label: `使用 ${trimmed}` }, ...suggestions]
      : suggestions;
  }, [options, value]);
  return <AutoComplete
    className="w-full min-w-0"
    options={data}
    filterOption={false}
    defaultActiveFirstOption={false}
    disabled={disabled}
    value={value}
    onChange={onChange}
    status={ariaInvalid ? "error" : undefined}
    getPopupContainer={getPopupContainer}
    styles={{ popup: { root: { zIndex, maxWidth: "calc(100vw - 32px)" } } }}
    virtual
    listHeight={260}
  >
    <Input id={id} allowClear placeholder={placeholder} disabled={disabled} aria-label={ariaLabel} aria-describedby={ariaDescribedBy} aria-invalid={ariaInvalid} prefix={<IconRobot size={15} aria-hidden="true" />} autoComplete="off" spellCheck={false} />
  </AutoComplete>;
}
