import { useMemo } from "react";
import { Select } from "antd";
import { IconAlertTriangle } from "@tabler/icons-react";
import type { SubagentModelOption } from "../subagentModels";
import { resolveSubagentModelOption } from "../subagentModels";

type ModelOption = { label: string; value?: string; key?: string; searchText?: string; description?: string; options?: ModelOption[] };

type ModelComboboxProps = {
  "aria-label": string;
  disabled?: boolean;
  getPopupContainer?: () => HTMLElement;
  onChange: (value: string) => void;
  options: SubagentModelOption[];
  placeholder?: string;
  preferredProviderId?: string;
  value: string;
  zIndex?: number;
};

export function ModelCombobox({ "aria-label": ariaLabel, disabled = false, getPopupContainer, onChange, options, placeholder = "请选择模型", preferredProviderId, value, zIndex }: ModelComboboxProps) {
  const selectedOption = useMemo(() => resolveSubagentModelOption(options, value, preferredProviderId), [options, preferredProviderId, value]);
  const groups = useMemo<ModelOption[]>(() => {
    const result = new Map<string, SubagentModelOption[]>();
    for (const option of options) {
      const group = result.get(option.routeId);
      if (group) group.push(option);
      else result.set(option.routeId, [option]);
    }
    return Array.from(result.entries()).map(([key, group]) => ({
      label: `${group[0].routeName} · ${group[0].providerId}`,
      key,
      options: group.map((option) => ({
        value: option.value,
        label: option.label,
        description: `${option.routeName} · ${option.modelId}`,
        searchText: [option.label, option.modelId, option.routeName, option.routePrefix, option.providerId].join("\u0000").toLocaleLowerCase(),
      })),
    }));
  }, [options]);
  const unavailableValue = value.trim() && !selectedOption ? value.trim() : "";
  return <Select
    aria-label={ariaLabel}
    aria-invalid={Boolean(unavailableValue) || undefined}
    className="w-full min-w-0"
    disabled={disabled}
    showSearch={{ filterOption: (input, option) => String(option?.searchText ?? option?.label ?? "").toLocaleLowerCase().includes(input.trim().toLocaleLowerCase()) }}
    placeholder={placeholder}
    value={selectedOption?.value ?? (unavailableValue || undefined)}
    status={unavailableValue ? "warning" : undefined}
    suffixIcon={unavailableValue ? <IconAlertTriangle size={14} aria-hidden="true" /> : undefined}
    getPopupContainer={getPopupContainer}
    popupMatchSelectWidth={360}
    styles={{ popup: { root: { maxWidth: "calc(100vw - 24px)", zIndex } } }}
    virtual
    listHeight={280}
    listItemHeight={52}
    options={groups}
    labelRender={() => selectedOption ? `[${selectedOption.routePrefix}] ${selectedOption.label}` : `${unavailableValue} · 已不可用`}
    optionRender={(option) => <span className="grid min-w-0 gap-0.5">
      <span className="truncate font-medium">{option.label}</span>
      <span className="truncate text-xs text-gray-500">{option.data.description}</span>
    </span>}
    notFoundContent={options.length === 0 ? "还没有可用于子代理的模型" : "没有匹配的模型或线路"}
    onChange={(nextValue: string) => onChange(nextValue)}
  />;
}
