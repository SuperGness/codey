import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../../components/ui";
import { IconAlertTriangle, IconInfoCircle } from "@tabler/icons-react";
export function ConfirmationDialog({
  title,
  description,
  busy,
  destructive,
  container,
  onConfirm,
  onCancel,
}: {
  title: string;
  description: string;
  busy: boolean;
  destructive?: boolean;
  container?: HTMLElement | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onCancel();
      }}
    >
      <DialogContent
        container={container}
        className="max-w-md"
        onEscapeKeyDown={(event) => {
          if (busy) event.preventDefault();
        }}
      >
        <DialogHeader className="space-y-3">
          <div className={`flex h-10 w-10 items-center justify-center rounded-xl ${destructive ? "bg-danger/10 text-danger" : "bg-accent/10 text-accent"}`} aria-hidden="true">
            {destructive ? <IconAlertTriangle size={21} /> : <IconInfoCircle size={21} />}
          </div>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription className="break-words leading-relaxed">{description}</DialogDescription>
        </DialogHeader>
        <DialogFooter className="mt-2 border-t border-black/[0.08] pt-4 dark:border-white/[0.1]">
          <Button variant="outline" disabled={busy} onClick={onCancel}>
            取消
          </Button>
          <Button
            variant={destructive ? "destructive" : "default"}
            loading={busy}
            onClick={onConfirm}
          >
            确认
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
