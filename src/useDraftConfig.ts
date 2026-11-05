import { useCallback, useMemo, useRef, useState } from "react";

import type { Config } from "./App.types";
import { configsHaveSameDraftValues, reconcileConfigDraft } from "./configDraft";

function onlyLocalRouterToggleChanged(current: Config, persisted: Config) {
  if (current.localRouterEnabled === persisted.localRouterEnabled) return false;
  const currentKeys = Object.keys(current) as Array<keyof Config>;
  if (currentKeys.length === Object.keys(persisted).length
    && currentKeys.every((key) => (
      key === "localRouterEnabled" || key === "settingsRevision" || Object.is(current[key], persisted[key])
    ))) {
    return true;
  }
  return JSON.stringify({
    ...current,
    localRouterEnabled: persisted.localRouterEnabled,
    settingsRevision: 0,
  }) === JSON.stringify({ ...persisted, settingsRevision: 0 });
}

export function useDraftConfig() {
  const [config, setConfig] = useState<Config | null>(null);
  const [dirty, setDirty] = useState(false);
  const persistedConfigRef = useRef<Config | null>(null);
  const draftConfigRef = useRef(config);
  draftConfigRef.current = config;

  const setPersistedConfig = useCallback((next: Config) => {
    persistedConfigRef.current = next;
    draftConfigRef.current = next;
    setConfig(next);
    setDirty(false);
  }, []);

  const pendingNativeRouterToggle = useMemo(() => Boolean(
    config &&
      persistedConfigRef.current &&
      !config.localRouterEnabled &&
      onlyLocalRouterToggleChanged(config, persistedConfigRef.current),
  ), [config]);

  const editConfig = useCallback((next: Config) => {
    draftConfigRef.current = next;
    setConfig(next);
    setDirty(!configsHaveSameDraftValues(next, persistedConfigRef.current));
  }, []);

  function isOnlyNativeRouterToggle(current: Config) {
    const persisted = persistedConfigRef.current;
    return Boolean(persisted && onlyLocalRouterToggleChanged(current, persisted));
  }

  function adoptRouteConfig(incoming: Config) {
    const merged = reconcileConfigDraft(
      persistedConfigRef.current,
      draftConfigRef.current,
      incoming,
    );
    if (!merged) return null;
    persistedConfigRef.current = incoming;
    draftConfigRef.current = merged.config;
    setConfig(merged.config);
    return merged;
  }

  function adoptSavedConfig(incoming: Config, submittedDraft: Config | null) {
    if (persistedConfigRef.current && incoming.settingsRevision < persistedConfigRef.current.settingsRevision) return null;
    const merged = reconcileConfigDraft(submittedDraft, draftConfigRef.current, incoming);
    if (!merged) return null;
    persistedConfigRef.current = incoming;
    draftConfigRef.current = merged.config;
    setConfig(merged.config);
    setDirty(merged.dirty);
    return merged;
  }

  const updateDraft = useCallback((
    update: (current: Config) => Config,
  ) => {
    const current = draftConfigRef.current;
    if (!current) return;
    editConfig(update(current));
  }, [editConfig]);

  function discardDraft() {
    if (persistedConfigRef.current) {
      draftConfigRef.current = persistedConfigRef.current;
      setConfig(persistedConfigRef.current);
    }
    setDirty(false);
  }

  return {
    config,
    dirty,
    setDirty,
    persistedConfigRef,
    setPersistedConfig,
    pendingNativeRouterToggle,
    canSyncCurrentProvider: !dirty || pendingNativeRouterToggle,
    editConfig,
    updateDraft,
    isOnlyNativeRouterToggle,
    adoptRouteConfig,
    adoptSavedConfig,
    discardDraft,
  };
}
