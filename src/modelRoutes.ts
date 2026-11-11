import type { Config, Profile } from "./App.types";
import { modelIdsEqual } from "./modelIds";

export function routeProviderId(profile: Profile) {
  return profile.sourceProviderId || profile.id;
}

export function sortRoutesByEnabled(profiles: readonly Profile[]) {
  return [...profiles].sort(
    (left, right) => Number(left.enabled === false) - Number(right.enabled === false),
  );
}

const routeComponentEncoder = new TextEncoder();

function encodeRouteComponent(value: string) {
  const bytes = routeComponentEncoder.encode(value.trim());
  return Array.from(bytes, (byte) => {
    const char = String.fromCharCode(byte);
    return /[A-Za-z0-9._-]/.test(char)
      ? char
      : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }).join("");
}

export function routeModelAlias(profile: Profile, model: string) {
  const normalized = model.trim();
  return `${encodeRouteComponent(routeProviderId(profile))}/${normalized}`;
}

export function globalDefaultForRoute(
  config: Config,
  profile: Profile,
  models: string[],
) {
  return models.find((model) =>
    modelIdsEqual(routeModelAlias(profile, model), config.defaultModel),
  ) || "";
}
