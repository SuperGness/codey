import { useMemo } from "react";
import {
  Collection,
  ComboBox,
  Header,
  Input,
  InputGroup,
  ListBox,
  useFilter,
} from "@heroui/react";
import type { Key } from "@heroui/react";
import { IconAlertTriangle } from "@tabler/icons-react";
import type { SubagentModelOption } from "../subagentModels";
import { resolveSubagentModelOption } from "../subagentModels";

type ModelOption = {
  id: string;
  label: string;
  textValue: string;
};
type ModelGroup = { id: string; label: string; options: ModelOption[] };

type ModelComboboxProps = {
  "aria-label": string;
  disabled?: boolean;
  onChange: (value: string) => void;
  options: SubagentModelOption[];
  placeholder?: string;
  preferredProviderId?: string;
  value: string;
};

export function ModelCombobox({
  "aria-label": ariaLabel,
  disabled = false,
  onChange,
  options,
  placeholder = "请选择模型",
  preferredProviderId,
  value,
}: ModelComboboxProps) {
  const { contains } = useFilter({ sensitivity: "base" });
  const selectedOption = useMemo(
    () => resolveSubagentModelOption(options, value, preferredProviderId),
    [options, preferredProviderId, value],
  );

  const trimmedValue = value.trim();
  const unavailableValue = selectedOption ? "" : trimmedValue;

  const groups = useMemo<ModelGroup[]>(() => {
    const result = new Map<string, SubagentModelOption[]>();
    for (const option of options) {
      const group = result.get(option.routeId);
      if (group) group.push(option);
      else result.set(option.routeId, [option]);
    }
    const list = Array.from(result.entries()).map(([id, group]) => ({
      id,
      label: `[${group[0].routePrefix}]${group[0].routeName}`,
      options: group.map((option) => ({
        id: option.value,
        label: option.label,
        textValue: `[${option.routePrefix}] ${option.label}`,
      })),
    }));

    if (unavailableValue) {
      list.unshift({
        id: "__unavailable__",
        label: "不可用模型",
        options: [
          {
            id: unavailableValue,
            label: `${unavailableValue} · 已不可用`,
            textValue: `${unavailableValue} · 已不可用`,
          },
        ],
      });
    }

    return list;
  }, [options, unavailableValue]);

  const selectedKey: Key | null = selectedOption?.value ?? (unavailableValue || null);
  const emptyText = options.length === 0 ? "还没有可用于子代理的模型" : "没有匹配的模型或线路";

  return (
    <ComboBox
      aria-label={ariaLabel}
      fullWidth
      className="w-full min-w-0"
      defaultFilter={(textValue, inputValue) => contains(textValue, inputValue.trim())}
      isDisabled={disabled}
      isInvalid={Boolean(unavailableValue)}
      selectedKey={selectedKey}
      onSelectionChange={(key) => {
        if (key != null) onChange(String(key));
      }}
      menuTrigger="focus"
    >
      <ComboBox.InputGroup>
        {unavailableValue ? (
          <InputGroup.Prefix className="px-2">
            <IconAlertTriangle size={14} className="shrink-0 text-warning" aria-hidden="true" />
          </InputGroup.Prefix>
        ) : null}
        <Input
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          className="min-h-8 md:min-h-8"
        />
        <ComboBox.Trigger />
      </ComboBox.InputGroup>
      <ComboBox.Popover className="w-(--trigger-width) max-w-[calc(100vw-24px)]">
        <ListBox
          aria-label={ariaLabel}
          items={groups}
          className="max-h-[280px] overflow-y-auto"
          renderEmptyState={() => <div className="px-3 py-2 text-xs text-muted">{emptyText}</div>}
        >
          {(group) => (
            <ListBox.Section id={group.id}>
              <Header className="flex items-center gap-2 font-bold text-accent">
                <span className="min-w-0 truncate">{group.label}</span>
                <span className="min-w-4 flex-1 border-t border-current" aria-hidden="true" />
              </Header>
              <Collection items={group.options}>
                {(option) => (
                  <ListBox.Item id={option.id} textValue={option.textValue}>
                    <span className="min-w-0 flex-1 truncate font-medium">{option.label}</span>
                  </ListBox.Item>
                )}
              </Collection>
            </ListBox.Section>
          )}
        </ListBox>
      </ComboBox.Popover>
    </ComboBox>
  );
}
