import { memo, useEffect, useId, useRef, useState } from "react";

import {
  IconRoute,
  IconSettings,
  IconSparkles,
} from "@tabler/icons-react";

import type { Config, Notice } from "./App.types";
import { invoke } from "./api";
import { errorText, withTimeout } from "./appUtils";
import { ManualModelCombobox } from "./components/ManualModelCombobox";
import { ModelCombobox } from "./components/ModelCombobox";
import { Tabs, toast } from "@heroui/react";
import { Button, Input, Label, PasswordInput, Select, Switch, TextArea } from "./components/ui";
import type { SubagentModelOption } from "./subagentModels";
import { SettingsPageHeader } from "./SettingsPageHeader";
import { validateOutboundApiUrl } from "./urlValidation";

const TEST_TIMEOUT_MS = 65_000;
const FETCH_MODELS_TIMEOUT_MS = 20_000;
const DEFAULT_OPTIMIZER_INSTRUCTION =
  "你是一名资深提示词架构师。接收用户的原始提示词后，在完全保留其原始目标和核心意图的前提下，将其优化为更精确、逻辑严密、指令明确的高效提示词。\n\n" +
  "优化重点：\n\n" +
  "- 明确模型角色与专业立场\n" +
  "- 细化任务步骤与边界约束\n" +
  "- 规范输出格式与风格要求\n" +
  "- 消除歧义词汇与冗余表达\n\n" +
  "严格输出要求：仅输出优化后的提示词正文本身。绝不包含任何前言、开场白、结尾说明、解释性文字或代码块符号（```）。";

const MANUAL_PROTOCOL_OPTIONS = [
  { value: "openaiResponses", label: "OpenAI Responses" },
  { value: "openaiChatCompletions", label: "OpenAI Chat Completions" },
  { value: "anthropicMessages", label: "Anthropic Messages" },
] as const;

type PromptOptimizationCardProps = {
  active?: boolean;
  config: Config;
  isBusy: boolean;
  subagentModelOptions: SubagentModelOption[];
  onConfigChange: (config: Config) => void;
  onNotice: (notice: Notice) => void;
};

type TestResult = {
  httpStatus?: number;
  responsePreview?: string;
};

function PromptOptimizationCardComponent({
  config,
  isBusy,
  subagentModelOptions,
  onConfigChange,
  onNotice,
}: PromptOptimizationCardProps) {
  const optimization = config.promptOptimization;
  const controlId = useId();
  const requestSequenceRef = useRef(0);
  const activeOperationRef = useRef<"models" | "test" | null>(null);
  const [apiKeyVisible, setApiKeyVisible] = useState(false);
  const [testing, setTesting] = useState(false);
  const [cloudModels, setCloudModels] = useState<string[]>([]);
  const [fetchingModels, setFetchingModels] = useState(false);

  const updateOptimization = (patch: Partial<Config["promptOptimization"]>) => {
    onConfigChange({
      ...config,
      promptOptimization: { ...optimization, ...patch },
    });
  };
  const apiKeyInputId = controlId + "-api-key";
  const baseUrlInputId = controlId + "-base-url";
  const modelInputId = controlId + "-model";
  const usesCodeyRoute = optimization.mode === "codeyRoute";
  const codeyRouteAvailable = config.localRouterEnabled;
  const hasApiKey = Boolean(
    optimization.apiKey.trim() ||
      (optimization.apiKeyConfigured && !optimization.clearApiKey),
  );
  const baseUrlError =
    !usesCodeyRoute && (optimization.enabled || optimization.baseUrl.trim())
      ? validateOutboundApiUrl(optimization.baseUrl, "API 地址")
      : "";
  const apiKeyError =
    !usesCodeyRoute && optimization.enabled && !hasApiKey
      ? "请输入 API Key"
      : "";
  const modelError =
    optimization.enabled && !optimization.model.trim()
      ? usesCodeyRoute
        ? "请选择 Codey 路由模型"
        : "请选择或填写模型"
      : "";
  const connectionDraftValid = usesCodeyRoute
    ? codeyRouteAvailable
    : !baseUrlError && !apiKeyError;
  const testDraftValid = connectionDraftValid && !modelError;

  useEffect(() => {
    setApiKeyVisible(false);
  }, [config.settingsRevision]);

  const clearModelSuggestions = () => {
    setCloudModels([]);
  };

  const changeMode = (mode: "codeyRoute" | "manual") => {
    if (optimization.mode === mode) return;
    clearModelSuggestions();
    onNotice({ tone: "info", text: "" });
    updateOptimization({ mode });
  };

  const showTestNotice = (tone: "success" | "error", text: string) => {
    if (tone === "success") {
      toast.success(text);
    } else {
      toast.danger(text);
    }
    onNotice({ tone: "info", text: "" });
  };

  const handleApiKeyChange = (value: string) => {
    updateOptimization({
      apiKey: value,
      clearApiKey: false,
    });
  };

  const runFetchModels = async () => {
    if (usesCodeyRoute || activeOperationRef.current || !connectionDraftValid) return;
    activeOperationRef.current = "models";
    const requestId = requestSequenceRef.current + 1;
    requestSequenceRef.current = requestId;
    setFetchingModels(true);
    try {
      const result = await withTimeout(
        invoke<{ models?: string[] }>("fetch_prompt_optimization_models", {
          config: optimization,
        }),
        FETCH_MODELS_TIMEOUT_MS,
        "获取模型列表超时，请检查 API 地址与网络",
      );
      if (requestSequenceRef.current !== requestId) return;
      const models = result?.models ?? [];
      setCloudModels(models);
      if (models.length > 0) {
        toast.success("已获取 " + models.length + " 个模型");
      } else {
        toast.danger("服务端没有返回可用模型");
      }
    } catch (error) {
      if (requestSequenceRef.current === requestId) {
        toast.danger(errorText(error));
      }
    } finally {
      if (requestSequenceRef.current === requestId) {
        activeOperationRef.current = null;
        setFetchingModels(false);
      }
    }
  };

  const runTest = async () => {
    if (activeOperationRef.current || !testDraftValid) return;
    activeOperationRef.current = "test";
    const requestId = requestSequenceRef.current + 1;
    requestSequenceRef.current = requestId;
    setTesting(true);
    onNotice({ tone: "info", text: "" });
    try {
      const result = await withTimeout(
        invoke<{ result?: TestResult }>("test_prompt_optimization", {
          config: optimization,
        }),
        TEST_TIMEOUT_MS,
        "测试超时，请检查 API 地址与网络",
      );
      if (requestSequenceRef.current !== requestId) return;
      const httpStatus = result?.result?.httpStatus;
      const responsePreview = result?.result?.responsePreview?.trim();
      if (typeof httpStatus === "number" && httpStatus >= 400) {
        showTestNotice(
          "error",
          responsePreview
            ? "连接失败（HTTP " + httpStatus + "）：" + responsePreview
            : "连接失败（HTTP " + httpStatus + "）",
        );
        return;
      }
      showTestNotice(
        "success",
        typeof httpStatus === "number"
          ? "连接成功（HTTP " + httpStatus + "）"
          : "连接成功",
      );
    } catch (error) {
      if (requestSequenceRef.current === requestId) {
        showTestNotice("error", errorText(error));
      }
    } finally {
      if (requestSequenceRef.current === requestId) {
        activeOperationRef.current = null;
        setTesting(false);
      }
    }
  };

  return (
    <section
      className="secondary-section prompt-optimization-section"
      aria-labelledby="prompt-optimization-title"
    >
      <div className="prompt-optimization-settings">
        <SettingsPageHeader
          id="prompt-optimization-title"
          title="提示词优化"
          icon={<IconSparkles size={15} />}
          description="在 Codex 输入框旁一键重写与优化提示词。"
          actions={
            <Switch
              checked={optimization.enabled}
              disabled={isBusy}
              aria-label="启用提示词优化"
              onCheckedChange={(checked) => updateOptimization({ enabled: checked })}
            />
          }
        />
        <div className="module-card-body prompt-optimization-body">
          {optimization.enabled ? (
            <div className="prompt-optimization-content">
              <Tabs
                selectedKey={optimization.mode}
                onSelectionChange={(key) => changeMode(String(key) as "codeyRoute" | "manual")}
                className="w-full flex flex-col gap-5"
              >
                <div className="prompt-optimization-toolbar">
                  <Tabs.ListContainer className="prompt-optimization-tabs-container">
                    <Tabs.List aria-label="提示词优化配置方式" className="prompt-optimization-tabs-list">
                      <Tabs.Tab
                        id="codeyRoute"
                        isDisabled={isBusy || !codeyRouteAvailable}
                        className="prompt-optimization-tab"
                      >
                        <IconRoute size={14} className="prompt-tab-icon" />
                        <span
                          className="prompt-tab-text"
                          title={codeyRouteAvailable ? undefined : "本地路由已关闭"}
                        >
                          使用 Codey 路由
                        </span>
                        <Tabs.Indicator />
                      </Tabs.Tab>
                      <Tabs.Tab
                        id="manual"
                        isDisabled={isBusy}
                        className="prompt-optimization-tab"
                      >
                        <IconSettings size={14} className="prompt-tab-icon" />
                        <span className="prompt-tab-text">手动配置</span>
                        <Tabs.Indicator />
                      </Tabs.Tab>
                    </Tabs.List>
                  </Tabs.ListContainer>

                  <div className="prompt-optimization-toolbar-actions">
                    <Button
                      variant="outline"
                      size="sm"
                      className="prompt-test-btn"
                      loading={testing}
                      disabled={isBusy || fetchingModels || !testDraftValid}
                      onClick={() => void runTest()}
                    >
                      <span>
                        {usesCodeyRoute
                          ? "测试路由连通性"
                          : "测试 API 连通性"}
                      </span>
                    </Button>
                  </div>
                </div>

                <Tabs.Panel id="codeyRoute" className="prompt-tabs-panel">
                  <div className="prompt-form-group">
                    <div className="prompt-field">
                      <Label htmlFor={modelInputId} className="prompt-field-label">模型</Label>
                      <div className="prompt-field-control">
                        <ModelCombobox
                          aria-label="提示词优化 Codey 路由模型"
                          value={optimization.model}
                          placeholder={
                            subagentModelOptions.length === 0
                              ? "所有线路均暂无模型"
                              : "请选择模型"
                          }
                          disabled={
                            isBusy ||
                            !codeyRouteAvailable ||
                            subagentModelOptions.length === 0
                          }
                          options={subagentModelOptions}
                          onChange={(model) => updateOptimization({ model })}
                        />
                        {!codeyRouteAvailable ? (
                          <small className="field-hint">
                            本地路由已关闭。请启用并重启 Codex，或改用手动配置。
                          </small>
                        ) : null}
                        {modelError ? (
                          <small id={modelInputId + "-error"} className="field-error" role="alert">
                            {modelError}
                          </small>
                        ) : null}
                        {subagentModelOptions.length === 0 ? (
                          <small className="field-hint">
                            请先在模型管理中为任一可用线路启用模型。
                          </small>
                        ) : null}
                      </div>
                    </div>

                    <div className="prompt-field">
                      <div className="prompt-field-label-row">
                        <Label htmlFor={controlId + "-instruction"} className="prompt-field-label">优化指令</Label>
                        {optimization.instruction && optimization.instruction !== DEFAULT_OPTIMIZER_INSTRUCTION ? (
                          <Button
                            variant="link"
                            size="xs"
                            className="reset-instruction-btn h-auto p-0 text-[11.5px] font-medium text-accent hover:underline"
                            disabled={isBusy}
                            onClick={() => updateOptimization({ instruction: DEFAULT_OPTIMIZER_INSTRUCTION })}
                          >
                            恢复默认
                          </Button>
                        ) : null}
                      </div>
                      <div className="prompt-field-control">
                        <TextArea
                          id={controlId + "-instruction"}
                          className="h-[240px] min-h-[140px] resize-y text-xs leading-relaxed"
                          value={optimization.instruction || DEFAULT_OPTIMIZER_INSTRUCTION}
                          disabled={isBusy}
                          onChange={(event) =>
                            updateOptimization({ instruction: event.target.value })
                          }
                          placeholder="自定义优化指令…"
                          spellCheck={false}
                        />
                      </div>
                    </div>
                  </div>
                </Tabs.Panel>

                <Tabs.Panel id="manual" className="prompt-tabs-panel">
                  <div className="prompt-form-group">
                    <div className="prompt-field">
                      <Label htmlFor={controlId + "-protocol"} className="prompt-field-label">上游协议</Label>
                      <div className="prompt-field-control">
                        <Select
                          id={controlId + "-protocol"}
                          className="w-full min-w-0"
                          value={optimization.upstreamProtocol}
                          disabled={isBusy}
                          aria-label="提示词优化上游协议"
                          optionList={[...MANUAL_PROTOCOL_OPTIONS]}
                          filter={false}
                          onChange={(value) => {
                            clearModelSuggestions();
                            updateOptimization({
                              upstreamProtocol: String(value ?? "openaiResponses") as Config["promptOptimization"]["upstreamProtocol"],
                            });
                          }}
                        />
                      </div>
                    </div>

                    <div className="prompt-field">
                      <Label htmlFor={baseUrlInputId} className="prompt-field-label">API 地址</Label>
                      <div className="prompt-field-control">
                        <Input
                          id={baseUrlInputId}
                          value={optimization.baseUrl}
                          disabled={isBusy}
                          aria-invalid={Boolean(baseUrlError)}
                          aria-describedby={baseUrlError ? baseUrlInputId + "-error" : undefined}
                          onChange={(event) => {
                            clearModelSuggestions();
                            updateOptimization({ baseUrl: event.target.value });
                          }}
                          placeholder="https://api.openai.com/v1"
                          spellCheck={false}
                        />
                        {baseUrlError ? (
                          <small id={baseUrlInputId + "-error"} className="field-error" role="alert">
                            {baseUrlError}
                          </small>
                        ) : null}
                      </div>
                    </div>

                    <div className="prompt-field">
                      <Label htmlFor={apiKeyInputId} className="prompt-field-label">API Key</Label>
                      <div className="prompt-field-control">
                        <PasswordInput
                          id={apiKeyInputId}
                          className="w-full"
                          visibility={apiKeyVisible}
                          onVisibilityChange={() => setApiKeyVisible((visible) => !visible)}
                          value={optimization.apiKey}
                          disabled={isBusy}
                          aria-invalid={Boolean(apiKeyError)}
                          aria-describedby={apiKeyError ? apiKeyInputId + "-error" : undefined}
                          onChange={(event) => {
                            clearModelSuggestions();
                            handleApiKeyChange(event.target.value);
                          }}
                          placeholder={
                            optimization.apiKeyConfigured &&
                            optimization.apiKey.trim() === ""
                              ? "已保存（输入新 Key 可替换）"
                              : "sk-…"
                          }
                          autoComplete="new-password"
                          spellCheck={false}
                        />
                        {apiKeyError ? (
                          <small id={apiKeyInputId + "-error"} className="field-error" role="alert">
                            {apiKeyError}
                          </small>
                        ) : optimization.apiKeyConfigured &&
                          !optimization.clearApiKey &&
                          !optimization.apiKey.trim() ? (
                          <small className="field-hint">
                            Key 已保存；直接输入可替换。
                          </small>
                        ) : null}
                      </div>
                    </div>

                    <div className="prompt-field">
                      <Label htmlFor={modelInputId} className="prompt-field-label">模型</Label>
                      <div className="prompt-field-control">
                        <div className="flex min-w-0 items-center gap-2 max-[680px]:flex-col max-[680px]:items-stretch">
                          <div className="relative min-w-0 flex-1 max-[680px]:w-full">
                            <ManualModelCombobox
                              id={modelInputId}
                              value={optimization.model}
                              disabled={isBusy || fetchingModels}
                              ariaLabel="提示词优化模型"
                              ariaInvalid={Boolean(modelError)}
                              ariaDescribedBy={modelError ? modelInputId + "-error" : undefined}
                              options={cloudModels}
                              placeholder="例如 gpt-4o-mini 或 deepseek-chat"
                              onChange={(model) => updateOptimization({ model })}
                            />
                          </div>
                          <Button
                            className="h-9! min-w-[84px] shrink-0 max-[680px]:w-full!"
                            variant="outline"
                            size="sm"
                            loading={fetchingModels}
                            disabled={
                              isBusy ||
                              testing ||
                              !connectionDraftValid
                            }
                            onClick={() => void runFetchModels()}
                          >
                            获取列表
                          </Button>
                        </div>
                        {modelError ? (
                          <small id={modelInputId + "-error"} className="field-error" role="alert">
                            {modelError}
                          </small>
                        ) : null}
                      </div>
                    </div>

                    <div className="prompt-field">
                      <div className="prompt-field-label-row">
                        <Label htmlFor={controlId + "-instruction"} className="prompt-field-label">优化指令</Label>
                        {optimization.instruction && optimization.instruction !== DEFAULT_OPTIMIZER_INSTRUCTION ? (
                          <Button
                            variant="link"
                            size="xs"
                            className="reset-instruction-btn h-auto p-0 text-[11.5px] font-medium text-accent hover:underline"
                            disabled={isBusy}
                            onClick={() => updateOptimization({ instruction: DEFAULT_OPTIMIZER_INSTRUCTION })}
                          >
                            恢复默认
                          </Button>
                        ) : null}
                      </div>
                      <div className="prompt-field-control">
                        <TextArea
                          id={controlId + "-instruction"}
                          className="h-[240px] min-h-[140px] resize-y text-xs leading-relaxed"
                          value={optimization.instruction || DEFAULT_OPTIMIZER_INSTRUCTION}
                          disabled={isBusy}
                          onChange={(event) =>
                            updateOptimization({ instruction: event.target.value })
                          }
                          placeholder="自定义优化指令…"
                          spellCheck={false}
                        />
                      </div>
                    </div>
                  </div>
                </Tabs.Panel>
              </Tabs>
            </div>
          ) : (
            <div className="module-disabled-placeholder">
              <div className="module-disabled-icon">
                <IconSparkles size={20} aria-hidden="true" />
              </div>
              <div className="module-disabled-text">
                <strong>提示词优化已关闭</strong>
                <p>开启后，在 Codex 输入框旁可通过快捷按钮一键将自然语言重写为高质量提示词。</p>
              </div>
              <Button
                variant="default"
                size="sm"
                className="mt-1"
                disabled={isBusy}
                onClick={() => updateOptimization({ enabled: true })}
              >
                开启提示词优化
              </Button>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}

export const PromptOptimizationCard = memo(
  PromptOptimizationCardComponent,
  (prev, next) => {
    if (prev.active === false && next.active === false) return true;
    if (prev.active !== next.active) return false;
    return (
      prev.config === next.config &&
      prev.isBusy === next.isBusy &&
      prev.subagentModelOptions === next.subagentModelOptions &&
      prev.onConfigChange === next.onConfigChange &&
      prev.onNotice === next.onNotice
    );
  },
);
