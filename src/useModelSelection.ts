import {
  useCallback,
  useMemo,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import { invoke } from "./api";
import type {
  Config,
  ModelState,
  ModelContextConfig,
  Notice,
  ProviderStatus,
  RuntimeStatus,
} from "./App.types";
import {
  AUTO_REVIEW_MODEL,
  includesModelId,
  modelKey,
  partitionModelIdsByKey,
  uniqueModelIds,
  withoutModelId,
} from "./modelIds";
import { buildSubagentModelOptions } from "./subagentModels";
import { routeProviderId } from "./modelRoutes";
import { modelSelectionNotice, type ModelRuntimeUpdate } from "./modelSelectionNotice";

const MAX_MODEL_ID_BYTES = 512;
const MAX_MODEL_COUNT = 10_000;
const modelIdEncoder = new TextEncoder();
const AUTO_REVIEW_MODEL_KEY = modelKey(AUTO_REVIEW_MODEL);

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
  const [modelPickerRouteId, setModelPickerRouteId] = useState<string | null>(null);
  const [modelPickerState, setModelPickerState] = useState<ModelState | null>(null);
  const [draftModels, setDraftModels] = useState<string[]>([]);
  const [draft1MModels, setDraft1MModels] = useState<string[]>([]);
  const [draftModelContexts, setDraftModelContexts] = useState<Record<string, ModelContextConfig>>({});
  const updateDraftModelContext = useCallback((model: string, policy: ModelContextConfig | undefined) => {
    setDraftModelContexts((current) => {
      const next = { ...current };
      for (const key of Object.keys(next)) if (modelKey(key) === modelKey(model)) delete next[key];
      if (policy) next[model] = policy;
      return next;
    });
  }, []);
  const draft1MModelSet = useMemo(() => new Set(draft1MModels.map(modelKey)), [draft1MModels]);
  const toggleDraft1MModel = useCallback((model: string, checked: boolean) => {
    setDraft1MModels((current) => checked ? uniqueModelIds([...current, model]) : withoutModelId(current, model));
  }, []);
  const [draftManualThirdPartyModels, setDraftManualThirdPartyModels] = useState<string[]>([]);
  const [deletedThirdPartyModels, setDeletedThirdPartyModels] = useState<string[]>([]);
  const [customModelInput, setCustomModelInput] = useState("");
  const [modelInputError, setModelInputError] = useState("");
  const [modelSyncWarning, setModelSyncWarning] = useState("");
  const [draftAutoReviewSupported, setDraftAutoReviewSupported] = useState(false);

  const modelEditorState = modelPickerState ?? modelState;
  const officialSlugKeys = useMemo(
    () =>
      new Set(
        modelPickerRouteId ? [] : modelEditorState.officialModelIds.map(modelKey),
      ),
    [modelEditorState.officialModelIds, modelPickerRouteId],
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
    setDraftModels(pickerSelection(state));
    const profile = config?.profiles.find((candidate) => candidate.id === (routeId ?? config.activeProfileId));
    const providerId = routeId && profile ? routeProviderId(profile) : currentProvider?.id || (profile ? routeProviderId(profile) : "");
    setDraft1MModels(config?.supports1MContextByProvider?.[providerId] || []);
    setDraftModelContexts(config?.modelContextByProvider?.[providerId] || {});
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
      setDraft1MModels((current) => current.filter((item) => !keys.has(modelKey(item)) || retainedKeys.has(modelKey(item))));
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
  ]);

  const deleteDraftThirdPartyModel = useCallback((model: string) => {
    const normalized = model.trim();
    if (!normalized) return;
    const normalizedKey = modelKey(normalized);
    const wasManual = draftManualThirdPartyModelKeys.has(normalizedKey);
    if (!wasManual) return;
    setDraft1MModels((current) => withoutModelId(current, normalized));
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
    const result = await invoke<{
      config: Config;
      modelState: ModelState;
    } & ModelRuntimeUpdate>("save_selected_models", {
      officialModels,
      thirdPartyModels,
      manualThirdPartyModels,
      deletedThirdPartyModels: deletedModels,
      supportsAutoReview,
      modelContexts: Object.fromEntries(Object.entries(draftModelContexts).filter(([model]) =>
        includesModelId(modelEditorState.officialModelIds, model) || includesModelId(thirdPartyModelOptions, model))),
      supports1MContextModels: draft1MModels.filter(
        (model) =>
          includesModelId(modelEditorState.officialModelIds, model) ||
          includesModelId(thirdPartyModelOptions, model),
      ),
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
    draft1MModels,
    draftModelContexts,
    modelEditorState.officialModelIds,
    thirdPartyModelOptions,
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
      await applyModelSelection(
        officialModels,
        thirdPartyModels,
        manualThirdPartyModels,
        deletedThirdPartyModels,
        draftAutoReviewSupported,
        `已保存 ${officialModels.length + thirdPartyModels.length} 个当前线路模型`,
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
    setModelState,
    modelPickerVisible,
    setModelPickerVisible,
    customModelInput,
    modelInputError,
    modelSyncWarning,
    draftAutoReviewSupported,
    setDraftAutoReviewSupported,
    draftModelSet,
    draft1MModelSet,
    draftModelContexts,
    updateDraftModelContext,
    toggleDraft1MModel,
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
