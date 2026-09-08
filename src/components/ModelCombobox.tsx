import { useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useVirtualizedCombobox } from "@mantine/core";
import { IconAlertTriangle, IconCheck, IconSearch } from "@tabler/icons-react";

import type { SubagentModelOption } from "../subagentModels";
import { resolveSubagentModelOption } from "../subagentModels";
import {
  MODEL_GROUP_HEIGHT,
  MODEL_LIST_HEIGHT,
  MODEL_OPTION_HEIGHT,
  modelComboboxLayout,
  visibleModelGroups,
} from "../modelComboboxWindow";
import { compactSelectInputClass } from "../uiClasses";
import { Combobox, InputBase } from "./mantine";

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

function normalizedSearchText(value: string) {
  return value.trim().toLocaleLowerCase();
}

export function ModelCombobox({
  "aria-label": ariaLabel,
  disabled = false,
  getPopupContainer,
  onChange,
  options,
  placeholder = "请选择模型",
  preferredProviderId,
  value,
  zIndex,
}: ModelComboboxProps) {
  const [search, setSearch] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(-1);
  const [scrollTop, setScrollTop] = useState(0);
  const viewportRef = useRef<HTMLDivElement>(null);
  const optionId = useId();
  const selectedOption = useMemo(
    () => resolveSubagentModelOption(options, value, preferredProviderId),
    [options, preferredProviderId, value],
  );
  const validValues = useMemo(
    () => new Set(options.map((option) => option.value)),
    [options],
  );
  const searchableOptions = useMemo(
    () =>
      options.map((option) => ({
        option,
        searchText: [
          option.label,
          option.modelId,
          option.routeName,
          option.routePrefix,
          option.providerId,
        ]
          .map(normalizedSearchText)
          .join("\u0000"),
      })),
    [options],
  );
  const normalizedSearch = normalizedSearchText(search);
  const filteredOptions = useMemo(
    () =>
      normalizedSearch
        ? searchableOptions
            .filter(({ searchText }) => searchText.includes(normalizedSearch))
            .map(({ option }) => option)
        : options,
    [normalizedSearch, options, searchableOptions],
  );
  const layout = useMemo(() => modelComboboxLayout(filteredOptions), [filteredOptions]);
  const visibleGroups = visibleModelGroups(layout.groups, scrollTop);
  const activeOptionIndex = useMemo(
    () => Math.max(0, layout.options.findIndex((option) => option.value === selectedOption?.value)),
    [layout, selectedOption],
  );
  const selectedOptionMounted = visibleGroups.some(({ group, start, end }) =>
    selectedIndex >= group.startIndex + start && selectedIndex < group.startIndex + end,
  );
  const combobox = useVirtualizedCombobox({
    totalOptionsCount: layout.options.length,
    selectedOptionIndex: selectedIndex,
    activeOptionIndex,
    getOptionId: (index) => `${optionId}-${index}`,
    setSelectedOptionIndex: (index) => {
      setSelectedIndex(index);
      const top = layout.offsets[index];
      if (top === undefined) return;
      const current = viewportRef.current?.scrollTop ?? scrollTop;
      if (top < current) setScrollTop(top);
      else if (top + MODEL_OPTION_HEIGHT > current + MODEL_LIST_HEIGHT) {
        setScrollTop(top + MODEL_OPTION_HEIGHT - MODEL_LIST_HEIGHT);
      }
    },
    onSelectedOptionSubmit: (index) => {
      const option = layout.options[index];
      if (option) onChange(option.value);
      combobox.closeDropdown();
    },
    onDropdownClose: () => {
      setSelectedIndex(-1);
      setSearch("");
      setScrollTop(0);
    },
    onDropdownOpen: () => combobox.focusSearchInput(),
  });
  useLayoutEffect(() => {
    if (viewportRef.current) viewportRef.current.scrollTop = scrollTop;
  }, [combobox.dropdownOpened, scrollTop]);
  useLayoutEffect(() => {
    setSelectedIndex(-1);
    setScrollTop(0);
  }, [filteredOptions]);
  const portalTarget = getPopupContainer?.();
  const unavailableValue = value.trim() && !selectedOption ? value.trim() : "";
  const triggerText = selectedOption
    ? `[${selectedOption.routePrefix}] ${selectedOption.label}`
    : unavailableValue
      ? `${unavailableValue} · 已不可用`
      : placeholder;

  return (
    <Combobox
      classNames={{
        dropdown:
          "w-[360px]! max-w-[calc(100vw-24px)]! overflow-hidden rounded-[10px]! border-black/10! p-0! shadow-[0_12px_32px_rgba(0,0,0,0.14)]!",
        option:
          "mx-1! rounded-[7px]! px-2.5! py-2! text-xs data-[combobox-selected]:bg-blue-500/9! data-[combobox-selected]:text-[#1d1d1f]!",
      }}
      middlewares={{ flip: true, shift: true }}
      onOptionSubmit={(nextValue) => {
        if (!validValues.has(nextValue)) return;
        onChange(nextValue);
        combobox.closeDropdown();
      }}
      portalProps={portalTarget ? { target: portalTarget } : undefined}
      position="bottom-start"
      store={combobox}
      withinPortal={Boolean(portalTarget)}
      zIndex={zIndex}
    >
      <Combobox.Target>
        <InputBase
          aria-invalid={Boolean(unavailableValue) || undefined}
          aria-label={ariaLabel}
          className="w-full min-w-0"
          classNames={{
            input: `${compactSelectInputClass} flex! items-center pr-7! text-left!`,
            section: "text-[#6e6e73]",
          }}
          component="button"
          disabled={disabled}
          onClick={() => combobox.toggleDropdown()}
          pointer
          rightSection={
            unavailableValue
              ? <IconAlertTriangle size={13} color="#b7791f" aria-hidden="true" />
              : <Combobox.Chevron size="xs" />
          }
          rightSectionPointerEvents="none"
          title={triggerText}
          type="button"
        >
          <span
            className={
              unavailableValue
                ? "block min-w-0 truncate text-[#9a6700]"
                : selectedOption
                  ? "block min-w-0 truncate text-[#3a3a3c]"
                  : "block min-w-0 truncate text-[#8e8e93]"
            }
          >
            {triggerText}
          </span>
        </InputBase>
      </Combobox.Target>

      <Combobox.Dropdown>
        <Combobox.Search
          aria-activedescendant={selectedOptionMounted ? `${optionId}-${selectedIndex}` : undefined}
          aria-label={`搜索${ariaLabel}`}
          classNames={{
            input:
              "h-9! rounded-none! border-0! border-b! border-black/7! bg-[#f8f8fa]! pl-9! text-xs! focus:ring-0!",
          }}
          leftSection={<IconSearch size={14} aria-hidden="true" />}
          onChange={(event) => setSearch(event.currentTarget.value)}
          placeholder="搜索模型或线路"
          value={search}
        />
        <Combobox.Options
          ref={viewportRef}
          className="max-h-[280px] overflow-y-auto"
          onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
        >
          <div style={{ position: "relative", height: layout.height }}>
            {combobox.dropdownOpened && visibleGroups.map(({ group, start, end }) => (
              <Combobox.Group
                key={group.key}
                style={{ position: "absolute", top: group.top, left: 0, right: 0, height: group.height }}
                styles={{ groupLabel: { height: MODEL_GROUP_HEIGHT, margin: 0, padding: "4px 8px" } }}
                label={
                  <span className="flex min-w-0 items-center justify-between gap-2 px-1 text-[10px] font-semibold text-[#8e8e93]">
                    <span className="truncate">{group.routeName}</span>
                    <span className="shrink-0 font-mono font-normal text-[#aeaeb2]">
                      {group.providerId}
                    </span>
                  </span>
                }
              >
                <div style={{ paddingTop: start * MODEL_OPTION_HEIGHT }}>
                  {group.options.slice(start, end).map((option, offset) => {
                    const selected = selectedOption?.value === option.value;
                    const index = group.startIndex + start + offset;
                    return (
                      <Combobox.Option
                        active={selected}
                        selected={selectedIndex === index}
                        aria-selected={selected}
                        aria-posinset={index + 1}
                        aria-setsize={layout.options.length}
                        id={`${optionId}-${index}`}
                        style={{ height: MODEL_OPTION_HEIGHT }}
                        key={option.value}
                        value={option.value}
                      >
                        <span className="flex min-w-0 items-center gap-2">
                          <span className="grid min-w-0 flex-1 gap-0.5">
                            <span className="truncate font-semibold text-[#3a3a3c]">
                              {option.label}
                            </span>
                            <span className="truncate text-[10px] text-[#8e8e93]">
                              {option.routeName} · {option.modelId}
                            </span>
                          </span>
                          <span className="grid w-4 shrink-0 place-items-center text-blue-600">
                            {selected && <IconCheck size={14} aria-hidden="true" />}
                          </span>
                        </span>
                      </Combobox.Option>
                    );
                  })}
                </div>
              </Combobox.Group>
            ))}
          </div>
          {layout.groups.length === 0 && (
            <Combobox.Empty className="py-6 text-xs text-[#8e8e93]">
              {options.length === 0 ? "还没有可用于子代理的模型" : "没有匹配的模型或线路"}
            </Combobox.Empty>
          )}
        </Combobox.Options>
      </Combobox.Dropdown>
    </Combobox>
  );
}
