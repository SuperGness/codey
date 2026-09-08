import type { SubagentModelOption } from "./subagentModels";

export const MODEL_OPTION_HEIGHT = 48;
export const MODEL_GROUP_HEIGHT = 28;
export const MODEL_LIST_HEIGHT = 280;
const OVERSCAN = MODEL_OPTION_HEIGHT * 4;

export type ModelOptionGroup = {
  key: string;
  routeName: string;
  providerId: string;
  options: SubagentModelOption[];
  startIndex: number;
  top: number;
  height: number;
};

export function modelComboboxLayout(options: readonly SubagentModelOption[]) {
  const grouped = new Map<string, ModelOptionGroup>();
  for (const option of options) {
    const group = grouped.get(option.routeId);
    if (group) group.options.push(option);
    else grouped.set(option.routeId, {
      key: option.routeId, routeName: option.routeName, providerId: option.providerId,
      options: [option], startIndex: 0, top: 0, height: 0,
    });
  }
  const ordered: SubagentModelOption[] = [];
  const offsets: number[] = [];
  let height = 0;
  const groups = [...grouped.values()];
  for (const group of groups) {
    group.startIndex = ordered.length;
    group.top = height;
    group.height = MODEL_GROUP_HEIGHT + group.options.length * MODEL_OPTION_HEIGHT;
    for (const [index, option] of group.options.entries()) {
      ordered.push(option);
      offsets.push(height + MODEL_GROUP_HEIGHT + index * MODEL_OPTION_HEIGHT);
    }
    height += group.height;
  }
  return { groups, options: ordered, offsets, height };
}

export function visibleModelGroups(groups: readonly ModelOptionGroup[], scrollTop: number) {
  const top = Math.max(0, scrollTop - OVERSCAN);
  const bottom = scrollTop + MODEL_LIST_HEIGHT + OVERSCAN;
  return groups.filter((group) => group.top + group.height > top && group.top < bottom)
    .map((group) => ({
      group,
      start: Math.max(0, Math.min(group.options.length, Math.floor((top - group.top - MODEL_GROUP_HEIGHT) / MODEL_OPTION_HEIGHT))),
      end: Math.max(0, Math.min(group.options.length, Math.ceil((bottom - group.top - MODEL_GROUP_HEIGHT) / MODEL_OPTION_HEIGHT))),
    }));
}
