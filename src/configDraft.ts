import type { Config, Profile } from "./App.types";

function equal(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((value, index) => equal(value, right[index]));
  }
  if (!object(left) || !object(right)) return false;
  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  return leftKeys.length === rightKeys.length &&
    leftKeys.every((key) => (
      Object.prototype.hasOwnProperty.call(right, key) && equal(left[key], right[key])
    ));
}

export function configsHaveSameDraftValues(left: Config | null, right: Config | null) {
  if (Object.is(left, right)) return true;
  if (!left || !right) return false;
  const leftKeys = Object.keys(left).filter((key) => key !== "settingsRevision");
  const rightKeys = Object.keys(right).filter((key) => key !== "settingsRevision");
  if (leftKeys.length !== rightKeys.length) return false;
  return leftKeys.every(
    (key) =>
      Object.prototype.hasOwnProperty.call(right, key) &&
      equal(left[key as keyof Config], right[key as keyof Config]),
  );
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function mergeValue(base: unknown, draft: unknown, incoming: unknown): unknown {
  if (base === draft) return incoming;
  if (equal(base, draft)) return incoming;
  if (!object(base) || !object(draft) || !object(incoming)) return draft;
  const merged: Record<string, unknown> = {};
  for (const key of new Set([...Object.keys(base), ...Object.keys(draft), ...Object.keys(incoming)])) {
    const value = mergeValue(base[key], draft[key], incoming[key]);
    if (value !== undefined) merged[key] = value;
  }
  return merged;
}

export function reconcileConfigDraft(base: Config | null, draft: Config | null, incoming: Config) {
  if (base && incoming.settingsRevision < base.settingsRevision) return null;
  if (!base || !draft) return { config: incoming, dirty: false };
  const config = { ...mergeValue(base, draft, incoming) as Config };
  const baseProfiles = new Map(base.profiles.map((profile) => [profile.id, profile]));
  const draftProfiles = new Map(draft.profiles.map((profile) => [profile.id, profile]));
  config.profiles = incoming.profiles.flatMap((profile) => {
    const previous = baseProfiles.get(profile.id);
    const edited = draftProfiles.get(profile.id);
    if (profile.officialAccount || profile.officialAccountId) return [profile];
    if (previous && !edited) return [];
    return [(edited ? mergeValue(previous, edited, profile) : profile) as Profile];
  });
  const incomingIds = new Set(incoming.profiles.map((profile) => profile.id));
  for (const profile of draft.profiles) {
    if (!baseProfiles.has(profile.id) && !incomingIds.has(profile.id)) config.profiles.push(profile);
  }
  config.settingsRevision = incoming.settingsRevision;
  if (!config.profiles.some((profile) => profile.id === config.activeProfileId)) {
    config.activeProfileId = incoming.activeProfileId;
  }
  return { config, dirty: !configsHaveSameDraftValues(config, incoming) };
}
