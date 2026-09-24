import {
  useCallback,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import { invoke } from "./api";
import type {
  Config,
  ModelState,
  ModelContextConfig,
  ModelReasoningEffort,
  Notice,
  ProviderStatus,
  RuntimeStatus,
} from "./App.types";
import {
  AUTO_REVIEW_MODEL,
  includesModelId,
  modelIdsEqual,
  modelKey,
  orderModelIdsBy,
  partitionModelIdsByKey,
  uniqueModelIds,
  withoutModelId,
} from "./modelIds";
import { buildSubagentModelOptions } from "./subagentModels";
import { routeProviderId } from "./modelRoutes";
import { modelSelectionNotice, type ModelRuntimeUpdate } from "./modelSelectionNotice";
import {
  autoReasoningEfforts,
  normalizeReasoningEfforts,
  reasoningEffortsEqual,
} from "./modelReasoningEfforts";

const MAX_MODEL_ID_BYTES = 512;
const MAX_MODEL_COUNT = 10_000;
const modelIdEncoder = new TextEncoder();
const AUTO_REVIEW_MODEL_KEY = modelKey(AUTO_REVIEW_MODEL);
/// 线路模板没有给出档位时使用的默认思考强度。
const DEFAULT_THIRD_PARTY_REASONING_EFFORTS = ["low", "medium", "high", "xhigh"];

const pickerSelection = (state: ModelState) =>
  [
    ...state.officialModels
      .filter((model) => model.supported)
      .map((model) => model.slug),
    ...state.thirdPartyModels,
  ].filter((model) => modelKey(model) !== AUTO_REVIEW_MODEL_KEY);

type UseModelSelectionOptions = {
  config: Config | null;
  currentProvider: ProviderStatus["provider"] | null;
  officialAccountAvailable: boolean;
  runOperation: (name: string, action: () => Promise<void>) => Promise<void>;
  setPersistedConfig: (config: Config) => void;
  setStatus: Dispatch<SetStateAction<RuntimeStatus>>;
  setNotice: Dispatch<SetStateAction<Notice>>;
};

export function useModelSelection({
  config,
  currentProvider,
  officialAccountAvailable,
  runOperation,
  setPersistedConfig,
  setStatus,
  setNotice,
}: UseModelSelectionOptions) {
  const [modelState, setModelState] = useState<ModelState>({
    officialModels: [],
    officialModelIds: [],
    thirdPartyModels: [],
    manualThirdPartyModels: [],
    upstreamModels: [],
    defaultModel: "",
  });
  const [modelPickerVisible, setModelPickerVisible] = useState(false);
  const [modelPickerLoading, setModelPickerLoading] = useState(false);
  const modelPickerSession = useRef(0);
  const [modelPickerRouteId, setModelPickerRouteId] = useState<string | null>(null);
  const [modelPickerState, setModelPickerState] = useState<ModelState | null>(null);
  const [draftModels, setDraftModels] = useState<string[]>([]);
  const [draftReasoningEfforts, setDraftReasoningEfforts] = useState<
    Record<string, ModelReasoningEffort[]>
  >({});
  const [reasoningEffortAutoByModel, setReasoningEffortAutoByModel] = useState<
    Record<string, ModelReasoningEffort[]>
  >({});
  const [draftModelContexts, setDraftModelContexts] = useState<Record<string, ModelContextConfig>>({});
  const updateDraftModelContext = useCallback((model: string, policy: ModelContextConfig | undefined) => {
    setDraftModelContexts((current) => {
      const next = { ...current };
      for (const key of Object.keys(next)) if (modelKey(key) === modelKey(model)) delete next[key];
      if (policy) next[model] = policy;
      return next;
    });
  }, []);
  const updateDraftReasoningEffort = useCallback(
    (model: string, efforts: ModelReasoningEffort[]) => {
      setDraftReasoningEfforts((current) => ({
        ...current,
        [modelKey(model)]: efforts,
      }));
    },
    [],
  );
  const resetDraftReasoningEffort = useCallback((model: string) => {
    setDraftReasoningEfforts((current) => ({
      ...current,
      [modelKey(model)]:
        reasoningEffortAutoByModel[modelKey(model)] ??
        autoReasoningEfforts(DEFAULT_THIRD_PARTY_REASONING_EFFORTS),
    }));
  }, [reasoningEffortAutoByModel]);
  const [draftManualThirdPartyModels, setDraftManualThirdPartyModels] = useState<string[]>([]);
  const [deletedThirdPartyModels, setDeletedThirdPartyModels] = useState<string[]>([]);
  const [customModelInput, setCustomModelInput] = useState("");
  const [modelInputError, setModelInputError] = useState("");
  const [modelSyncWarning, setModelSyncWarning] = useState("");
  const [draftAutoReviewSupported, setDraftAutoReviewSupported] = useState(false);

  const modelEditorState = modelPickerState ?? modelState;
  const officialRoutePicker = Boolean(
    modelPickerRouteId &&
      config?.profiles.some(
        (profile) =>
          profile.id === modelPickerRouteId && profile.authMode === "officialAccount",
      ),
  );
  const officialOnly = Boolean(
    officialRoutePicker || (!modelPickerRouteId && currentProvider?.official),
  );
  const officialSlugKeys = useMemo(
    () =>
      new Set(
        (!modelPickerRouteId || officialRoutePicker)
          ? modelEditorState.officialModelIds.map(modelKey)
          : [],
      ),
    [modelEditorState.officialModelIds, modelPickerRouteId, officialRoutePicker],
  );
  const draftModelSet = useMemo(
    () => new Set(draftModels.map(modelKey)),
    [draftModels],
  );
  const draftManualThirdPartyModelKeys = useMemo(
    () => new Set(draftManualThirdPartyModels.map(modelKey)),
    [draftManualThirdPartyModels],
  );
  const manualThirdPartyModelKeys = useMemo(
    () => new Set(modelEditorState.manualThirdPartyModels.map(modelKey)),
    [modelEditorState.manualThirdPartyModels],
  );
  const deletedThirdPartyModelKeys = useMemo(
    () => new Set(deletedThirdPartyModels.map(modelKey)),
    [deletedThirdPartyModels],
  );
  const thirdPartyModelOptions = useMemo(
    () => {
      if (officialOnly) return [];
      const seenKeys = new Set<string>();
      return [
        ...modelEditorState.upstreamModels,
        ...modelEditorState.thirdPartyModels,
        ...draftModels,
      ].reduce<string[]>((models, model) => {
        const normalized = model.trim();
        const key = modelKey(normalized);
        if (
          normalized &&
          key !== AUTO_REVIEW_MODEL_KEY &&
          !officialSlugKeys.has(key) &&
          !deletedThirdPartyModelKeys.has(key) &&
          !seenKeys.has(key)
        ) {
          seenKeys.add(key);
          models.push(normalized);
        }
        return models;
      }, []);
    },
    [
      draftModels,
      deletedThirdPartyModelKeys,
      modelEditorState.thirdPartyModels,
      modelEditorState.upstreamModels,
      officialSlugKeys,
      officialOnly,
    ],
  );
  const subagentModelOptions = useMemo(
    () =>
      buildSubagentModelOptions(
        config,
        modelState,
        officialAccountAvailable,
        currentProvider,
      ),
    [config, currentProvider, modelState, officialAccountAvailable],
  );

  const openModelPicker = useCallback((
    state: ModelState,
    warning = "",
    routeId: string | null = null,
    autoReviewSupported = false,
  ) => {
    const profile = config?.profiles.find((candidate) => candidate.id === (routeId ?? config.activeProfileId));
    const providerId = routeId && profile ? routeProviderId(profile) : currentProvider?.id || (profile ? routeProviderId(profile) : "");
    // 官方目录按 Codex 缓存顺序返回，草稿要沿用线路已保存的模型顺序，保存时才不会打乱。
    setDraftModels(orderModelIdsBy(
      pickerSelection(state),
      config?.selectedModelsByProvider[providerId] || [],
    ));
    setDraftModelContexts(config?.modelContextByProvider?.[providerId] || {});
    const storedReasoningEfforts = config?.modelReasoningEffortsByProvider?.[providerId];
    const reasoningModels = uniqueModelIds([
      ...state.upstreamModels,
      ...state.thirdPartyModels,
      ...state.manualThirdPartyModels,
      ...pickerSelection(state),
    ]);
    const autoEfforts: Record<string, ModelReasoningEffort[]> = {};
    const draftEfforts: Record<string, ModelReasoningEffort[]> = {};
    for (const model of reasoningModels) {
      const key = modelKey(model);
      if (autoEfforts[key]) continue;
      const declared = state.thirdPartyModelMetadata?.find(
        (entry) => modelKey(entry.slug) === key,
      )?.autoSupportedReasoningEfforts;
      const base = autoReasoningEfforts(
        declared?.length ? declared : DEFAULT_THIRD_PARTY_REASONING_EFFORTS,
      );
      autoEfforts[key] = base;
      const stored = Object.entries(storedReasoningEfforts ?? {}).find(([name]) =>
        modelIdsEqual(name, model))?.[1];
      draftEfforts[key] = stored ? normalizeReasoningEfforts(stored) : base;
    }
    setReasoningEffortAutoByModel(autoEfforts);
    setDraftReasoningEfforts(draftEfforts);
    setDraftManualThirdPartyModels(state.manualThirdPartyModels);
    setDeletedThirdPartyModels([]);
    setCustomModelInput("");
    setModelInputError("");
    setModelSyncWarning(warning);
    setModelPickerRouteId(routeId);
    setModelPickerState(state);
    setDraftAutoReviewSupported(autoReviewSupported);
    setModelPickerVisible(true);
  }, [config, currentProvider]);

  const emptyModelState = useCallback((): ModelState => ({
    officialModels: [],
    officialModelIds: [],
    thirdPartyModels: [],
    manualThirdPartyModels: [],
    upstreamModels: [],
    defaultModel: "",
  }), []);

  const beginModelPickerLoad = useCallback((
    routeId: string | null = null,
    autoReviewSupported = false,
  ) => {
    const session = modelPickerSession.current + 1;
    modelPickerSession.current = session;
    setModelPickerLoading(true);
    openModelPicker(emptyModelState(), "", routeId, autoReviewSupported);
    return session;
  }, [emptyModelState, openModelPicker]);

  const completeModelPickerLoad = useCallback((
    session: number,
    state: ModelState,
    warning = "",
    routeId: string | null = null,
    autoReviewSupported = false,
  ) => {
    if (modelPickerSession.current !== session) return;
    setModelPickerLoading(false);
    openModelPicker(state, warning, routeId, autoReviewSupported);
  }, [openModelPicker]);

  const setModelPickerOpen = useCallback((open: boolean) => {
    if (!open) {
      modelPickerSession.current += 1;
      setModelPickerLoading(false);
    }
    setModelPickerVisible(open);
  }, []);

  const toggleDraftModel = useCallback((model: string | readonly string[], checked: boolean) => {
    const models = typeof model === "string" ? [model] : model;
    const keys = new Set(models.map(modelKey));
    if (checked) {
      setDeletedThirdPartyModels((current) =>
        current.filter((item) => !keys.has(modelKey(item))),
      );
    }
    setDraftModels((current) =>
      checked
        ? uniqueModelIds([...current, ...models])
        : current.filter((item) => !keys.has(modelKey(item))),
    );
    if (!checked) {
      const retainedKeys = new Set([...modelEditorState.upstreamModels.map(modelKey), ...officialSlugKeys]);
      setDraftReasoningEfforts((current) =>
        Object.fromEntries(
          Object.entries(current).filter(
            ([key]) => !keys.has(key) || retainedKeys.has(key),
          ),
        ),
      );
      setDraftManualThirdPartyModels((current) =>
        current.filter((item) => !keys.has(modelKey(item))),
      );
    }
  }, [modelEditorState.upstreamModels, officialSlugKeys]);

  const updateCustomModelInput = useCallback((value: string) => {
    setCustomModelInput(value);
    if (modelInputError) setModelInputError("");
  }, [modelInputError]);

  const addCustomModel = useCallback(() => {
    if (officialOnly) {
      setModelInputError("官方线路只能勾选当前账号可用的模型");
      return;
    }
    const model = customModelInput.trim();
    if (!model) {
      setModelInputError("请输入要添加的模型 ID");
      return;
    }
    if (modelKey(model) === AUTO_REVIEW_MODEL_KEY) {
      setModelInputError(
        `${AUTO_REVIEW_MODEL} 是线路能力，请使用上方 Auto Review 开关`,
      );
      return;
    }
    if (modelIdEncoder.encode(model).byteLength > MAX_MODEL_ID_BYTES) {
      setModelInputError(`模型 ID 不能超过 ${MAX_MODEL_ID_BYTES} 字节`);
      return;
    }
    if (
      draftModels.length >= MAX_MODEL_COUNT &&
      !draftModels.some((item) => modelKey(item) === modelKey(model))
    ) {
      setModelInputError(`模型数量不能超过 ${MAX_MODEL_COUNT} 个`);
      return;
    }
    const officialModel = modelPickerRouteId
      ? undefined
      : modelEditorState.officialModelIds.find(
          (official) => modelKey(official) === modelKey(model),
        );
    if (officialModel) {
      setModelInputError(
        `${officialModel} 已在上方官方模型列表中，请直接勾选，不可重复输入`,
      );
      return;
    }
    const existingUpstreamModel = modelEditorState.upstreamModels.find(
      (upstream) => modelKey(upstream) === modelKey(model),
    );
    setDraftModels((current) =>
      includesModelId(current, model) ? current : [...current, model],
    );
    if (!existingUpstreamModel || manualThirdPartyModelKeys.has(modelKey(model))) {
      setDraftManualThirdPartyModels((current) =>
        current.some((item) => modelKey(item) === modelKey(model))
          ? current
          : [...current, model],
      );
    }
    setDeletedThirdPartyModels((current) =>
      withoutModelId(current, model),
    );
    setCustomModelInput("");
    setModelInputError("");
  }, [
    customModelInput,
    draftModels,
    manualThirdPartyModelKeys,
    modelEditorState.officialModelIds,
    modelEditorState.upstreamModels,
    modelPickerRouteId,
    officialOnly,
  ]);

  const deleteDraftThirdPartyModel = useCallback((model: string) => {
    const normalized = model.trim();
    if (!normalized) return;
    const normalizedKey = modelKey(normalized);
    const wasManual = draftManualThirdPartyModelKeys.has(normalizedKey);
    if (!wasManual) return;
    setDraftReasoningEfforts((current) =>
      Object.fromEntries(
        Object.entries(current).filter(([key]) => key !== normalizedKey),
      ),
    );
    setDraftModels((current) =>
      withoutModelId(current, normalized),
    );
    setDraftManualThirdPartyModels((current) =>
      withoutModelId(current, normalized),
    );
    setDeletedThirdPartyModels((current) =>
      !manualThirdPartyModelKeys.has(normalizedKey) ||
      current.some((item) => modelKey(item) === normalizedKey)
        ? current
        : [...current, normalized],
    );
    setModelInputError("");
  }, [
    draftManualThirdPartyModelKeys,
    manualThirdPartyModelKeys,
  ]);

  const applyModelSelection = useCallback(async (
    officialModels: string[],
    thirdPartyModels: string[],
    manualThirdPartyModels: string[],
    deletedModels: string[],
    supportsAutoReview: boolean,
    summary: string,
    closePicker: boolean,
  ) => {
    const declaredReasoningEfforts: Record<string, ModelReasoningEffort[]> = {};
    if (config?.localRouterEnabled === true) {
      for (const [key, efforts] of Object.entries(draftReasoningEfforts)) {
        const model = thirdPartyModelOptions.find(
          (candidate) => modelKey(candidate) === key,
        );
        if (!model) continue;
        const normalized = normalizeReasoningEfforts(efforts);
        if (reasoningEffortsEqual(normalized, reasoningEffortAutoByModel[key] ?? [])) {
          continue;
        }
        declaredReasoningEfforts[model] = normalized;
      }
    }
    const result = officialRoutePicker
      ? await invoke<{
          config: Config;
          modelState: ModelState;
        } & ModelRuntimeUpdate>("save_official_route_models", {
          routeId: modelPickerRouteId,
          models: officialModels,
        })
      : await invoke<{
          config: Config;
          modelState: ModelState;
        } & ModelRuntimeUpdate>("save_selected_models", {
          officialModels,
          thirdPartyModels,
          manualThirdPartyModels,
          deletedThirdPartyModels: deletedModels,
          supportsAutoReview,
          ...(!officialOnly
            ? {
                modelContexts: Object.fromEntries(Object.entries(draftModelContexts).filter(([model]) =>
                  includesModelId(modelEditorState.officialModelIds, model) || includesModelId(thirdPartyModelOptions, model))),
              }
            : {}),
          ...(config?.localRouterEnabled === true && !officialOnly
            ? { reasoningEfforts: declaredReasoningEfforts }
            : {}),
          ...(modelPickerRouteId == null ? {} : { routeId: modelPickerRouteId }),
        });
    setPersistedConfig(result.config);
    setModelState(result.modelState);
    setStatus((current) => ({
      ...current,
      restartRequired: result.restartRequired ?? current.restartRequired,
    }));
    if (closePicker) {
      setModelPickerVisible(false);
      setModelPickerRouteId(null);
      setModelPickerState(null);
    }
    setDeletedThirdPartyModels([]);
    setNotice(modelSelectionNotice(result, summary));
  }, [
    setNotice,
    setPersistedConfig,
    setStatus,
    modelPickerRouteId,
    officialRoutePicker,
    config,
    draftReasoningEfforts,
    draftModelContexts,
    reasoningEffortAutoByModel,
    modelEditorState.officialModelIds,
    thirdPartyModelOptions,
    officialOnly,
  ]);

  const saveModelSelection = useCallback(async () => {
    await runOperation("save-models", async () => {
      const normalizedDraftModels = uniqueModelIds(draftModels);
      const {
        matching: officialModels,
        remaining: thirdPartyModels,
      } = partitionModelIdsByKey(normalizedDraftModels, officialSlugKeys);
      const thirdPartyModelKeys = new Set(thirdPartyModels.map(modelKey));
      const manualThirdPartyModels = draftManualThirdPartyModels.filter((model) =>
        thirdPartyModelKeys.has(modelKey(model))
      );
      const totalCount = officialModels.length + thirdPartyModels.length;
      await applyModelSelection(
        officialModels,
        thirdPartyModels,
        manualThirdPartyModels,
        deletedThirdPartyModels,
        draftAutoReviewSupported,
        totalCount > 0 ? `已保存 ${totalCount} 个模型` : "已保存模型",
        true,
      );
    });
  }, [
    applyModelSelection,
    deletedThirdPartyModels,
    draftManualThirdPartyModels,
    draftModels,
    draftAutoReviewSupported,
    officialSlugKeys,
    runOperation,
  ]);

  return {
    subagentModelOptions,
    modelState,
    modelEditorState,
    officialOnly,
    setModelState,
    modelPickerVisible,
    modelPickerLoading,
    setModelPickerVisible: setModelPickerOpen,
    beginModelPickerLoad,
    completeModelPickerLoad,
    customModelInput,
    modelInputError,
    modelSyncWarning,
    draftAutoReviewSupported,
    setDraftAutoReviewSupported,
    draftModelSet,
    draftModelContexts,
    updateDraftModelContext,
    draftReasoningEfforts,
    reasoningEffortAutoByModel,
    updateDraftReasoningEffort,
    resetDraftReasoningEffort,
    draftManualThirdPartyModelKeys,
    thirdPartyModelOptions,
    openModelPicker,
    toggleDraftModel,
    deleteDraftThirdPartyModel,
    updateCustomModelInput,
    addCustomModel,
    saveModelSelection,
  };
}
