import { memo, useEffect, useMemo, useState } from "react";
import {
  IconCheck as Check,
  IconGitBranch as GitBranch,
  IconLoader2 as LoaderCircle,
  IconPlugConnected as PlugZap,
  IconRefresh as RefreshCw,
  IconServer as Server,
  IconTrash as Trash,
  IconX as X,
} from "@tabler/icons-react";

import type { CcSwitchStatus, ModelState } from "./App.types";
import { Badge, Button, Card, Switch } from "./components/semi";
import { modelIdsEqual, modelKey } from "./modelIds";
import {
  MODEL_PICKER_PAGE_SIZE,
  nextVisibleModelCount,
  visibleModelOptions,
} from "./modelPickerPagination";

type ModelSectionProps = {
  provider: CcSwitchStatus["provider"];
  modelState: ModelState;
  dirty: boolean;
  isBusy: boolean;
  busy: string | null;
  showAccountUsageInHeader: boolean;
  showTokenStatsCard: boolean;
  manualThirdPartyModelKeys: Set<string>;
  onSyncCurrentProvider: () => void;
  onFetchCurrentModels: () => void;
  onSetDefaultModel: (model: string) => void;
  onDeleteThirdPartyModel: (model: string) => void;
  onShowAccountUsageInHeaderChange: (checked: boolean) => void;
  onShowTokenStatsCardChange: (checked: boolean) => void;
};

function ModelSectionComponent({
  provider,
  modelState,
  dirty,
  isBusy,
  busy,
  showAccountUsageInHeader,
  showTokenStatsCard,
  manualThirdPartyModelKeys,
  onSyncCurrentProvider,
  onFetchCurrentModels,
  onSetDefaultModel,
  onDeleteThirdPartyModel,
  onShowAccountUsageInHeaderChange,
  onShowTokenStatsCardChange,
}: ModelSectionProps) {
  const defaultModel = modelState.defaultModel;
  const supportedOfficialModelCount = modelState.officialModels.reduce(
    (count, model) => count + Number(model.supported),
    0,
  );
  const [visibleThirdPartyCount, setVisibleThirdPartyCount] = useState(
    MODEL_PICKER_PAGE_SIZE,
  );
  useEffect(() => {
    setVisibleThirdPartyCount(MODEL_PICKER_PAGE_SIZE);
  }, [provider.id, modelState.thirdPartyModels]);
  const visibleThirdPartyModels = useMemo(
    () =>
      visibleModelOptions(
        modelState.thirdPartyModels,
        visibleThirdPartyCount,
      ),
    [modelState.thirdPartyModels, visibleThirdPartyCount],
  );
  return (
    <section className="route-section" aria-labelledby="route-title">
      <div className="section-title">
        <div>
          <h2 id="route-title">线路与模型</h2>
          <p>Codex 当前配置</p>
        </div>
        <div className="route-heading-actions">
          {provider.official && (
            <div
              className={`header-usage-toggle${showAccountUsageInHeader ? " active" : ""}`}
            >
              <span>显示额度浮窗</span>
              <Switch
                checked={showAccountUsageInHeader}
                disabled={isBusy}
                onCheckedChange={onShowAccountUsageInHeaderChange}
                aria-label="显示官方账号额度浮窗"
              />
            </div>
          )}
          <div
            className={`header-usage-toggle${showTokenStatsCard ? " active" : ""}`}
          >
            <span>显示性能统计</span>
            <Switch
              checked={showTokenStatsCard}
              disabled={isBusy}
              onCheckedChange={onShowTokenStatsCardChange}
              aria-label="在每条回复下方显示性能与 Token 统计"
            />
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={dirty || isBusy}
            onClick={onSyncCurrentProvider}
          >
            <RefreshCw
              className={busy === "sync-provider" ? "spinner" : ""}
              aria-hidden="true"
            />
            重新读取 Codex 配置
          </Button>
        </div>
      </div>

      <Card className="route-card">
        <div className="provider-summary">
          <div className="provider-identity">
            <span className="column-icon">
              <Server size={16} />
            </span>
            <div className="provider-name-box">
              <strong>{provider.name}</strong>
            </div>
          </div>
          <div className="provider-meta">
            <div>
              <span>类型</span>
              <strong>
                {provider.official ? "OpenAI 官方" : "第三方 API"}
              </strong>
            </div>
            <div className="provider-endpoint">
              <span>地址</span>
              <strong>
                {provider.official ? "ChatGPT 登录" : provider.baseUrl}
              </strong>
            </div>
            <div>
              <span>默认模型</span>
              <strong>{defaultModel || "未配置"}</strong>
            </div>
          </div>
        </div>

        <div className="catalog-workspace">
          <div className="catalog-heading">
            <div className="column-heading">
              <span className="column-icon">
                <GitBranch size={16} />
              </span>
              <div>
                <strong>模型列表</strong>
                <small>
                  {provider.official
                    ? "官方目录"
                    : `已确认 ${
                        supportedOfficialModelCount + modelState.thirdPartyModels.length
                      } 个可用模型`}
                </small>
              </div>
            </div>
            {!provider.official && (
              <div className="route-heading-actions">
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={isBusy}
                  onClick={onFetchCurrentModels}
                >
                  <RefreshCw
                    className={busy === "fetch-models" ? "spinner" : ""}
                    aria-hidden="true"
                  />
                  同步模型
                </Button>
              </div>
            )}
          </div>

          <div className="model-groups">
            <section className="model-group">
              <div className="model-group-title">
                <div>
                  <strong>官方模型</strong>
                  <small>
                    {supportedOfficialModelCount}{" "}
                    / {modelState.officialModels.length}
                  </small>
                </div>
                <Badge variant="info">OpenAI</Badge>
              </div>
              <div className="catalog-model-list">
                {modelState.officialModels.map((model) => {
                  const isDefault = modelIdsEqual(defaultModel, model.slug);
                  return (
                    <div
                      className={`catalog-model-row${model.supported ? "" : " unsupported"}${isDefault ? " default-model" : ""}`}
                      key={model.slug}
                      aria-disabled={!model.supported}
                    >
                      <span className="model-availability">
                        {model.supported ? (
                          <Check size={12} />
                        ) : (
                          <X size={12} />
                        )}
                      </span>
                      <div>
                        <strong>{model.displayName}</strong>
                        <small>{model.slug}</small>
                      </div>
                      <div className="catalog-model-actions">
                        {isDefault && <Badge variant="brand">默认</Badge>}
                        {model.supported && !isDefault && (
                          <Button
                            variant="ghost"
                            size="xs"
                            disabled={isBusy}
                            onClick={() => onSetDefaultModel(model.slug)}
                          >
                            设为默认
                          </Button>
                        )}
                      </div>
                    </div>
                  );
                })}
                {modelState.officialModels.length === 0 && (
                  <div className="empty-state">暂未读取到官方模型</div>
                )}
              </div>
            </section>

            {!provider.official && (
              <section className="model-group">
                <div className="model-group-title">
                  <div>
                    <strong>其他模型</strong>
                    <small>{modelState.thirdPartyModels.length}</small>
                  </div>
                  <Badge variant="brand">API</Badge>
                </div>
                <div
                  className={
                    modelState.thirdPartyModels.length === 0
                      ? "catalog-model-list catalog-model-list-empty"
                      : "catalog-model-list"
                  }
                >
                  {visibleThirdPartyModels.map((model) => {
                    const isDefault = modelIdsEqual(defaultModel, model);
                    return (
                      <div
                        className={`catalog-model-row third-party${isDefault ? " default-model" : ""}`}
                        key={model}
                      >
                        <span className="model-availability">
                          <Check size={12} />
                        </span>
                        <div>
                          <strong>{model}</strong>
                          <small>当前线路模型</small>
                        </div>
                        <div className="catalog-model-actions">
                          {isDefault && <Badge variant="brand">默认</Badge>}
                          {!isDefault && (
                            <Button
                              variant="ghost"
                              size="xs"
                              disabled={isBusy}
                              onClick={() => onSetDefaultModel(model)}
                            >
                              设为默认
                            </Button>
                          )}
                          {manualThirdPartyModelKeys.has(modelKey(model)) && (
                            <Button
                              variant="ghost"
                              size="xs"
                              className="catalog-model-delete"
                              disabled={isBusy}
                              onClick={() => onDeleteThirdPartyModel(model)}
                              aria-label={`删除其他模型 ${model}`}
                            >
                              <Trash aria-hidden="true" />
                              删除
                            </Button>
                          )}
                        </div>
                      </div>
                    );
                  })}
                  {visibleThirdPartyModels.length <
                    modelState.thirdPartyModels.length && (
                    <div className="model-picker-load-more">
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={isBusy}
                        onClick={() =>
                          setVisibleThirdPartyCount((count) =>
                            nextVisibleModelCount(
                              count,
                              modelState.thirdPartyModels.length,
                            ),
                          )
                        }
                      >
                        再显示{" "}
                        {Math.min(
                          MODEL_PICKER_PAGE_SIZE,
                          modelState.thirdPartyModels.length -
                            visibleThirdPartyModels.length,
                        )}{" "}
                        个
                      </Button>
                    </div>
                  )}
                  {modelState.thirdPartyModels.length === 0 && (
                    <div
                      className="catalog-empty-state"
                      role="region"
                      aria-labelledby="third-party-empty-title"
                      aria-busy={busy === "fetch-models"}
                    >
                      <span className="catalog-empty-icon" aria-hidden="true">
                        <PlugZap size={22} />
                      </span>
                      <div className="catalog-empty-copy">
                        <strong id="third-party-empty-title">
                          尚未添加其他模型
                        </strong>
                        <p>可同步发现，也可手动输入模型 ID 添加。</p>
                      </div>
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={isBusy}
                        onClick={onFetchCurrentModels}
                      >
                        {busy === "fetch-models" ? (
                          <LoaderCircle
                            className="spinner"
                            aria-hidden="true"
                          />
                        ) : (
                          <RefreshCw aria-hidden="true" />
                        )}
                        {busy === "fetch-models" ? "同步中" : "同步或手动配置"}
                      </Button>
                    </div>
                  )}
                </div>
              </section>
            )}
          </div>
        </div>

        <div className="readonly-note">
          <Server size={14} />
          <span className="readonly-note-text">
            Codey 直接读取本地 Codex 登录与 API 配置
          </span>
          <Badge variant="brand" className="readonly-note-tag">
            只读
          </Badge>
        </div>
      </Card>
    </section>
  );
}

export const ModelSection = memo(ModelSectionComponent);
