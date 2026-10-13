import { invoke } from "./api";
import type { OfficialAccountsResult } from "./App.types";

type Cache = {
  promise?: Promise<OfficialAccountsResult>;
  result?: OfficialAccountsResult;
  expiresAt?: number;
};

const LIST_TTL_MS = 2_000;
let cache: Cache = {};

export function rememberOfficialAccounts(result: OfficialAccountsResult) {
  if (!Array.isArray(result.accounts)) return;
  cache = {
    result,
    expiresAt: Date.now() + LIST_TTL_MS,
  };
}

export function listOfficialAccounts(options?: { force?: boolean }) {
  const force = options?.force === true;
  if (!force && cache.result && cache.expiresAt && Date.now() < cache.expiresAt) {
    return Promise.resolve(cache.result);
  }
  if (!force && cache.promise) return cache.promise;

  const promise = invoke<OfficialAccountsResult>("list_official_accounts").then(
    (result) => {
      rememberOfficialAccounts(result);
      return result;
    },
    (error) => {
      if (cache.promise === promise) {
        cache = cache.result
          ? { result: cache.result, expiresAt: cache.expiresAt }
          : {};
      }
      throw error;
    },
  );
  cache.promise = promise;
  return promise;
}
