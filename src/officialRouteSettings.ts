import type { OfficialAccount, Profile } from "./App.types";
import { validateOfficialRouteShortName } from "./routeShortNames";
import {
  validateOptionalOutboundApiUrl,
  validateOutboundProxyUrl,
} from "./urlValidation";

export const MAX_ROUTE_NAME_CHARACTERS = 15;
const MAX_OFFICIAL_ROUTE_CONCURRENCY = 4_294_967_295;
const MAX_OFFICIAL_ROUTE_NAME_CHARACTERS = MAX_ROUTE_NAME_CHARACTERS;

export type OfficialRouteSettingsDraft = {
  routeName: string;
  routeShortName: string;
  baseUrl: string;
  upstreamProxy: string;
  maxConcurrency: string;
};

export function validateOfficialRouteConcurrency(value: string): string {
  const text = value.trim();
  return /^\d+$/.test(text) && Number(text) <= MAX_OFFICIAL_ROUTE_CONCURRENCY
    ? ""
    : `请输入 0 至 ${MAX_OFFICIAL_ROUTE_CONCURRENCY} 的整数`;
}

/**
 * 官方线路的连接与并发设置保存在所属账号记录里。
 * 这里只做保存前的本地校验，后端仍会再次校验。
 */
export function validateOfficialRouteSettings(
  draft: OfficialRouteSettingsDraft,
  profiles: readonly Profile[],
  accounts: readonly OfficialAccount[] | null,
  accountId: string,
) {
  const shortName = draft.routeShortName.trim();
  const shortNameTaken =
    Boolean(shortName) &&
    (accounts ?? []).some(
      (account) =>
        account.id !== accountId && account.routeShortName?.trim() === shortName,
    );
  return {
    routeName:
      Array.from(draft.routeName.trim()).length >
      MAX_OFFICIAL_ROUTE_NAME_CHARACTERS
        ? `线路名最多 ${MAX_OFFICIAL_ROUTE_NAME_CHARACTERS} 个字符`
        : "",
    shortName:
      validateOfficialRouteShortName(draft.routeShortName, profiles) ||
      (shortNameTaken ? `短名称「${shortName}」已被其他官方账号使用` : ""),
    baseUrl: validateOptionalOutboundApiUrl(
      draft.baseUrl,
      "官方账号线路的网关地址",
    ),
    upstreamProxy: validateOutboundProxyUrl(
      draft.upstreamProxy,
      "官方账号线路的上游代理",
    ),
    maxConcurrency: validateOfficialRouteConcurrency(draft.maxConcurrency),
  };
}
