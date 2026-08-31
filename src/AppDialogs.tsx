import { memo, useEffect, useMemo, useState } from "react";
import {
  IconAlertTriangle as AlertTriangle,
  IconCheck as Check,
  IconLoader2 as LoaderCircle,
  IconPlus as Plus,
  IconRefresh as RefreshCw,
  IconTrash as Trash2,
} from "@tabler/icons-react";

import type { Confirmation, ModelState } from "./App.types";
import {
  filterModelOptions,
  MODEL_PICKER_PAGE_SIZE,
  nextVisibleModelCount,
  visibleModelOptions,
} from "./modelPickerPagination";
import { modelKey } from "./modelIds";
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Switch,
} from "./components/mantine";

type ModelPickerDialogProps = {
  open: boolean;
  routeConfigReadOnly: boolean;
  isBusy: boolean;
  busy: string | null;
  container: HTMLElement | null;
  customModelInput: string;
  modelInputError: string;
  modelSyncWarning: string;
  autoReviewSupported: boolean;
  thirdPartyModelOptions: string[];
  modelState: ModelState;
  draftModelSet: Set<string>;
  manualThirdPartyModelKeys: Set<string>;
  onOpenChange: (open: boolean) => void;
  onCustomModelInputChange: (model: string) => void;
  onAddCustomModel: () => void;
  onToggleDraftModel: (model: string, checked: boolean) => void;
  onDeleteThirdPartyModel: (model: string) => void;
  onAutoReviewSupportedChange: (checked: boolean) => void;
  onSave: () => void;
};

function ModelPickerDialogComponent({
  open,
  routeConfigReadOnly,
  isBusy,
  busy,
  container,
  customModelInput,
  modelInputError,
  modelSyncWarning,
  autoReviewSupported,
  thirdPartyModelOptions,
  modelState,
  draftModelSet,
  manualThirdPartyModelKeys,
  onOpenChange,
  onCustomModelInputChange,
  onAddCustomModel,
  onToggleDraftModel,
  onDeleteThirdPartyModel,
  onAutoReviewSupportedChange,
  onSave,
}: ModelPickerDialogProps) {
  const [modelQuery, setModelQuery] = useState("");
  const [visibleThirdPartyCount, setVisibleThirdPartyCount] = useState(
    MODEL_PICKER_PAGE_SIZE,
  );
  useEffect(() => {
    if (!open) return;
    setModelQuery("");
    setVisibleThirdPartyCount(MODEL_PICKER_PAGE_SIZE);
  }, [open]);
  const filteredThirdPartyModels = useMemo(() => {
    if (!open) return [];
    return filterModelOptions(thirdPartyModelOptions, modelQuery);
  }, [modelQuery, open, thirdPartyModelOptions]);
  const visibleThirdPartyModels = visibleModelOptions(
    filteredThirdPartyModels,
    visibleThirdPartyCount,
  );
  const selectedThirdPartyModelKeys = useMemo(
    () =>
      open
        ? new Set(modelState.thirdPartyModels.map(modelKey))
        : new Set<string>(),
    [modelState.thirdPartyModels, open],
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {open && <DialogContent
        className="w-[min(560px,calc(100vw-32px))]"
        container={container}
        onEscapeKeyDown={(event) => {
          if (isBusy) event.preventDefault();
        }}
        onPointerDownOutside={(event) => {
          if (isBusy) event.preventDefault();
        }}
      >
        <DialogHeader>
          <DialogTitle>配置当前线路支持的模型</DialogTitle>
          <DialogDescription>
            {modelState.officialModels.length > 0
              ? "请选择本次官方账号登录可用的模型。"
              : "请选择同步到的线路模型，或手动输入当前线路支持的模型 ID。"}
            {routeConfigReadOnly && " 保存只更新模型选择，线路连接配置保持只读。"}
          </DialogDescription>
        </DialogHeader>
        {modelSyncWarning && (
          <div className="mt-3.5 flex items-start gap-2 rounded-[9px] border border-amber-700/20 bg-[#fff8eb] px-3 py-2.5 text-[11px] leading-5 text-[#8a4b08]" role="alert">
            <AlertTriangle className="mt-px shrink-0" size={17} aria-hidden="true" />
            <span className="min-w-0 break-words">{modelSyncWarning}</span>
          </div>
        )}
        <div className="mt-3.5 flex items-center gap-3">
          <div className="min-w-0 flex-1">
            <Input
              value={customModelInput}
              onChange={(event) => onCustomModelInputChange(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  onAddCustomModel();
                }
              }}
              placeholder="输入当前线路模型 ID，例如 provider-model-v2"
              spellCheck={false}
              aria-label="输入线路模型 ID"
              aria-invalid={Boolean(modelInputError)}
              disabled={isBusy}
            />
          </div>
          <Button
            variant="secondary"
            size="sm"
            disabled={isBusy || !customModelInput.trim()}
            onClick={onAddCustomModel}
          >
            <Plus aria-hidden="true" />
            添加
          </Button>
        </div>
        {modelInputError && (
          <p className="mt-1.5 text-[11px] leading-[1.45] text-[#d70015]" role="alert">{modelInputError}</p>
        )}
        {!routeConfigReadOnly && <div className="mt-3 flex items-center justify-between gap-4 rounded-[9px] border border-black/8 bg-[#f7f7f8] px-3 py-2.5">
          <div className="grid min-w-0 gap-0.5">
            <strong className="text-xs font-semibold text-[#1d1d1f]">Auto Review</strong>
            <small className="text-[10px] leading-[1.45] text-[#6e6e73]">
              请确认是否支持<code>codex-auto-review</code>模型再进行修改
            </small>
          </div>
          <Switch
            size="sm"
            checked={autoReviewSupported}
            disabled={isBusy}
            onCheckedChange={onAutoReviewSupportedChange}
            aria-label="当前线路支持 auto-review"
          />
        </div>}
        <div className="my-3 max-h-[360px] overflow-y-auto rounded-[10px] border border-black/8 bg-[#fbfbfc] py-1 pl-1 pr-0.5 [scrollbar-color:rgba(99,99,104,0.46)_transparent] [scrollbar-gutter:stable] [scrollbar-width:thin] [&::-webkit-scrollbar]:w-2 [&::-webkit-scrollbar-button]:hidden [&::-webkit-scrollbar-thumb]:min-h-11 [&::-webkit-scrollbar-thumb]:rounded-full [&::-webkit-scrollbar-thumb]:border-2 [&::-webkit-scrollbar-thumb]:border-transparent [&::-webkit-scrollbar-thumb]:bg-black/40 [&::-webkit-scrollbar-thumb]:bg-clip-padding">
          {modelState.officialModels.length > 0 && (
            <>
              <div className="m-0.5 flex items-center justify-between gap-3 rounded-[7px] bg-[#f1f5fb] px-2.5 py-2">
                <div className="grid gap-0.5">
                  <strong className="text-xs font-semibold text-[#1d1d1f]">官方模型</strong>
                  <small className="text-[10px] leading-[1.35] text-[#6e6e73]">来自本次 Codex 官方账号登录</small>
                </div>
                <Badge variant="info">{modelState.officialModels.length} 个</Badge>
              </div>
              {modelState.officialModels.map((model) => (
                <div className="flex items-center gap-2.5 rounded-md bg-blue-500/[0.025] px-3 py-2 hover:bg-blue-500/[0.07]" key={model.slug}>
                  <Checkbox
                    checked={draftModelSet.has(modelKey(model.slug))}
                    disabled={isBusy}
                    onCheckedChange={(checked) =>
                      onToggleDraftModel(model.slug, checked === true)}
                    aria-label={`当前线路支持 ${model.slug}`}
                  />
                  <div className="grid min-w-0 flex-1 gap-px">
                    <strong className="break-words text-xs font-semibold text-[#1d1d1f]">{model.displayName}</strong>
                    <small className="break-words text-[11px] text-[#86868b]">{model.slug}</small>
                  </div>
                  <Badge className="ml-auto" variant="info">官方模型</Badge>
                </div>
              ))}
            </>
          )}
          <div className="mx-0.5 mb-0.5 mt-1.5 flex items-center justify-between gap-3 rounded-[7px] border-t border-black/6 bg-[#f5f5f7] px-2.5 py-2">
            <div className="grid gap-0.5">
              <strong className="text-xs font-semibold text-[#1d1d1f]">线路模型</strong>
              <small className="text-[10px] leading-[1.35] text-[#6e6e73]">全部通过当前 API Key 线路调用，可同步发现或手动输入</small>
            </div>
            <Badge variant="secondary">
              {filteredThirdPartyModels.length === thirdPartyModelOptions.length
                ? `${thirdPartyModelOptions.length} 个`
                : `${filteredThirdPartyModels.length} / ${thirdPartyModelOptions.length} 个`}
            </Badge>
          </div>
          {thirdPartyModelOptions.length > 0 && (
            <div className="px-2 pb-1.5 pt-1">
              <Input
                value={modelQuery}
                onChange={(event) => {
                  setModelQuery(event.target.value);
                  setVisibleThirdPartyCount(MODEL_PICKER_PAGE_SIZE);
                }}
                placeholder="搜索其他模型"
                spellCheck={false}
                aria-label="搜索其他模型"
                disabled={isBusy}
              />
            </div>
          )}
          {visibleThirdPartyModels.map((model) => {
            const added =
              draftModelSet.has(modelKey(model)) ||
              selectedThirdPartyModelKeys.has(modelKey(model));
            const manual = manualThirdPartyModelKeys.has(modelKey(model));
            return (
              <div className="flex items-center gap-2.5 rounded-md px-3 py-2 hover:bg-blue-500/6" key={model}>
                <Checkbox
                  checked={draftModelSet.has(modelKey(model))}
                  disabled={isBusy}
                  onCheckedChange={(checked) => onToggleDraftModel(model, checked === true)}
                  aria-label={`当前线路支持 ${model}`}
                />
                <span className="min-w-0 flex-1 break-words text-xs font-semibold text-[#1d1d1f]">{model}</span>
                {added && manual && (
                  <Button
                    variant="ghost"
                    size="xs"
                    className="shrink-0 text-[#d70015]"
                    disabled={isBusy}
                    onClick={() => onDeleteThirdPartyModel(model)}
                    aria-label={`删除其他模型 ${model}`}
                  >
                    <Trash2 aria-hidden="true" />
                    删除
                  </Button>
                )}
              </div>
            );
          })}
          {visibleThirdPartyModels.length < filteredThirdPartyModels.length && (
            <div className="flex justify-center px-2 pb-1 pt-1.5">
              <Button
                variant="ghost"
                size="sm"
                disabled={isBusy}
                onClick={() =>
                  setVisibleThirdPartyCount((count) =>
                    nextVisibleModelCount(
                      count,
                      filteredThirdPartyModels.length,
                    )
                  )}
              >
                再显示{" "}
                {Math.min(
                  MODEL_PICKER_PAGE_SIZE,
                  filteredThirdPartyModels.length - visibleThirdPartyModels.length,
                )}{" "}
                个
              </Button>
            </div>
          )}
          {filteredThirdPartyModels.length === 0 && (
            <div className="empty-state">
              {thirdPartyModelOptions.length === 0
                ? "尚无线路模型，可在上方输入模型 ID 添加"
                : "没有匹配的线路模型"}
            </div>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={isBusy} onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button
            disabled={isBusy}
            onClick={onSave}
          >
            {busy === "save-models"
              ? <LoaderCircle className="animate-spin" aria-hidden="true" />
              : <Check aria-hidden="true" />}
            保存模型声明
          </Button>
        </DialogFooter>
      </DialogContent>}
    </Dialog>
  );
}

type ConfirmationDialogProps = {
  confirmation: Confirmation | null;
  container: HTMLElement | null;
  onClose: () => void;
  onConfirm: (confirmation: Confirmation) => void;
};

function ConfirmationDialogComponent({
  confirmation,
  container,
  onClose,
  onConfirm,
}: ConfirmationDialogProps) {
  const destructive =
    confirmation?.action === "clear" ||
    confirmation?.action === "delete-notification-channel";
  return (
    <Dialog open={Boolean(confirmation)} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="confirmation-dialog" container={container}>
        <DialogHeader>
          <DialogTitle>{confirmation?.title}</DialogTitle>
          <DialogDescription>{confirmation?.description}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>取消</Button>
          <Button
            variant={
              destructive
                ? "destructive"
                : confirmation?.action === "restart"
                  ? "warning"
                  : "default"
            }
            onClick={() => {
              if (confirmation) onConfirm(confirmation);
            }}
          >
            {destructive
              ? <Trash2 aria-hidden="true" />
              : confirmation?.action === "restart"
              ? <RefreshCw aria-hidden="true" />
              : <Check aria-hidden="true" />}
            {confirmation?.confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export const ModelPickerDialog = memo(ModelPickerDialogComponent);
export const ConfirmationDialog = memo(ConfirmationDialogComponent);
