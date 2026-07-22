import {
  IconCheck as Check,
  IconLoader2 as LoaderCircle,
  IconRefresh as RefreshCw,
  IconSearch as Search,
  IconTrash as Trash2,
} from "@tabler/icons-react";

import type { Confirmation, ModelState } from "./App.types";
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
} from "./components/ui";

type ModelPickerDialogProps = {
  open: boolean;
  isBusy: boolean;
  busy: string | null;
  container: HTMLElement | null;
  modelQuery: string;
  filteredUpstreamModels: string[];
  modelState: ModelState;
  officialSlugs: Set<string>;
  draftModelSet: Set<string>;
  onOpenChange: (open: boolean) => void;
  onModelQueryChange: (query: string) => void;
  onDraftModelsChange: (models: string[]) => void;
  onToggleDraftModel: (model: string, checked: boolean) => void;
  onSave: () => void;
};

export function ModelPickerDialog({
  open,
  isBusy,
  busy,
  container,
  modelQuery,
  filteredUpstreamModels,
  modelState,
  officialSlugs,
  draftModelSet,
  onOpenChange,
  onModelQueryChange,
  onDraftModelsChange,
  onToggleDraftModel,
  onSave,
}: ModelPickerDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="model-picker-dialog"
        container={container}
        onEscapeKeyDown={(event) => {
          if (isBusy) event.preventDefault();
        }}
        onPointerDownOutside={(event) => {
          if (isBusy) event.preventDefault();
        }}
      >
        <DialogHeader>
          <DialogTitle>添加三方模型</DialogTitle>
          <DialogDescription>从当前线路发现的上游模型中选择要显示的三方模型。</DialogDescription>
        </DialogHeader>
        <div className="model-picker-toolbar">
          <div className="input-shell">
            <Search size={15} aria-hidden="true" />
            <Input
              value={modelQuery}
              onChange={(event) => onModelQueryChange(event.target.value)}
              placeholder="搜索模型"
              spellCheck={false}
              aria-label="搜索上游模型"
            />
          </div>
          <div>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => onDraftModelsChange(
                modelState.upstreamModels.filter((model) => !officialSlugs.has(model)),
              )}
            >
              全选三方
            </Button>
            <Button variant="ghost" size="sm" onClick={() => onDraftModelsChange([])}>
              清空
            </Button>
          </div>
        </div>
        <div className="model-picker-list">
          {filteredUpstreamModels.map((model) => {
            const officialModel = officialSlugs.has(model);
            return (
              <div className={`model-picker-row${officialModel ? " official" : ""}`} key={model}>
                <Checkbox
                  checked={officialModel || draftModelSet.has(model)}
                  disabled={officialModel}
                  onCheckedChange={(checked) => onToggleDraftModel(model, checked === true)}
                  aria-label={`添加 ${model}`}
                />
                <span>{model}</span>
                {officialModel && <Badge variant="info">官方模型</Badge>}
              </div>
            );
          })}
          {filteredUpstreamModels.length === 0 && <div className="empty-state">没有匹配的模型</div>}
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={isBusy} onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button
            disabled={isBusy && busy !== "save-models"}
            onClick={onSave}
          >
            {busy === "save-models"
              ? <LoaderCircle className="spinner" aria-hidden="true" />
              : <Check aria-hidden="true" />}
            添加到模型列表
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

type ConfirmationDialogProps = {
  confirmation: Confirmation | null;
  container: HTMLElement | null;
  onClose: () => void;
  onConfirm: (confirmation: Confirmation) => void;
};

export function ConfirmationDialog({
  confirmation,
  container,
  onClose,
  onConfirm,
}: ConfirmationDialogProps) {
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
              confirmation?.action === "clear"
                ? "destructive"
                : confirmation?.action === "restart"
                  ? "warning"
                  : "default"
            }
            onClick={() => {
              if (confirmation) onConfirm(confirmation);
            }}
          >
            {confirmation?.action === "clear"
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
