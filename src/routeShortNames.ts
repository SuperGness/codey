import type { Profile } from "./App.types";

const OFFICIAL_ROUTE_SHORT_NAME = "官";
export const MAX_ROUTE_SHORT_NAME_CHARACTERS = 2;

export function routeShortNameCharacterCount(value: string) {
  return Array.from(value.trim()).length;
}

export function validateThirdPartyRouteShortName(
  value: string,
  profiles: readonly Profile[] = [],
  currentRouteId = "",
) {
  const shortName = value.trim();
  if (!shortName) return "请输入短名称";
  if (routeShortNameCharacterCount(shortName) > MAX_ROUTE_SHORT_NAME_CHARACTERS) {
    return `短名称最多 ${MAX_ROUTE_SHORT_NAME_CHARACTERS} 个字符`;
  }
  if (shortName === OFFICIAL_ROUTE_SHORT_NAME) {
    return `“${OFFICIAL_ROUTE_SHORT_NAME}”仅供官方账号使用`;
  }
  if (
    profiles.some(
      (profile) =>
        profile.id !== currentRouteId &&
        profile.authMode !== "officialAccount" &&
        !profile.officialAccount &&
        profile.shortName?.trim() === shortName,
    )
  ) {
    return `短名称“${shortName}”已被其他线路使用`;
  }
  return "";
}

export function fallbackRouteShortName(name: string) {
  return Array.from(name.trim())
    .slice(0, MAX_ROUTE_SHORT_NAME_CHARACTERS)
    .join("");
}

export function routeDisplayPrefix(profile: Profile) {
  if (profile.authMode === "officialAccount" || profile.officialAccount) {
    return OFFICIAL_ROUTE_SHORT_NAME;
  }
  return profile.shortName?.trim() || fallbackRouteShortName(profile.name);
}

export function prefixedRouteModelName(profile: Profile, modelName: string) {
  const prefix = routeDisplayPrefix(profile);
  return prefix ? `[${prefix}] ${modelName}` : modelName;
}
