import * as React from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";

import { cn } from "../../lib/utils";

type DialogContextValue = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  titleId: string;
  descriptionId: string;
};

const DialogContext = React.createContext<DialogContextValue | null>(null);

function useDialogContext() {
  const context = React.useContext(DialogContext);
  if (!context) throw new Error("MagicDialog components must be used inside MagicDialog");
  return context;
}

export interface MagicDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children: React.ReactNode;
}

export function MagicDialog({ open, onOpenChange, children }: MagicDialogProps) {
  const generatedId = React.useId();
  return (
    <DialogContext.Provider
      value={{
        open,
        onOpenChange,
        titleId: `${generatedId}-title`,
        descriptionId: `${generatedId}-description`,
      }}
    >
      {children}
    </DialogContext.Provider>
  );
}

export interface MagicDialogContentProps extends React.HTMLAttributes<HTMLDivElement> {
  container?: HTMLElement | null;
  onEscapeKeyDown?: (event: Event) => void;
  onPointerDownOutside?: (event: Event) => void;
}

export const MagicDialogContent = React.forwardRef<HTMLDivElement, MagicDialogContentProps>(
  ({
    className,
    children,
    container,
    onEscapeKeyDown,
    onPointerDownOutside,
    ...props
  }, forwardedRef) => {
    const { open, onOpenChange, titleId, descriptionId } = useDialogContext();
    const localRef = React.useRef<HTMLDivElement | null>(null);

    React.useImperativeHandle(forwardedRef, () => localRef.current as HTMLDivElement);

    React.useEffect(() => {
      if (!open) return;
      const previouslyFocused = document.activeElement as HTMLElement | null;
      const content = localRef.current;
      const firstFocusable = content?.querySelector<HTMLElement>(
        "input:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex='-1'])",
      );
      (firstFocusable ?? content)?.focus();

      function handleKeyDown(event: KeyboardEvent) {
        if (event.key === "Escape") {
          const escapeEvent = new Event("magic-dialog-escape", { cancelable: true });
          onEscapeKeyDown?.(escapeEvent);
          if (!escapeEvent.defaultPrevented) onOpenChange(false);
          return;
        }
        if (event.key !== "Tab" || !content) return;
        const focusable = Array.from(content.querySelectorAll<HTMLElement>(
          "input:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex='-1'])",
        ));
        if (!focusable.length) {
          event.preventDefault();
          return;
        }
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      }

      document.addEventListener("keydown", handleKeyDown);
      return () => {
        document.removeEventListener("keydown", handleKeyDown);
        previouslyFocused?.focus();
      };
    }, [onEscapeKeyDown, onOpenChange, open]);

    if (!open) return null;

    const target = container ?? document.body;
    return createPortal(
      <div
        className="magic-dialog-layer"
        onMouseDown={(event) => {
          if (event.target !== event.currentTarget) return;
          const outsideEvent = new Event("magic-dialog-outside", { cancelable: true });
          onPointerDownOutside?.(outsideEvent);
          if (!outsideEvent.defaultPrevented) onOpenChange(false);
        }}
      >
        <div
          ref={localRef}
          role="dialog"
          aria-modal="true"
          aria-labelledby={titleId}
          aria-describedby={descriptionId}
          tabIndex={-1}
          className={cn("magic-dialog-content", className)}
          {...props}
        >
          <div className="magic-dialog-glow" aria-hidden="true" />
          <div className="relative z-10">{children}</div>
          <button
            type="button"
            className="magic-dialog-close"
            aria-label="关闭"
            onClick={() => onOpenChange(false)}
          >
            <X size={17} aria-hidden="true" />
          </button>
        </div>
      </div>,
      target,
    );
  },
);
MagicDialogContent.displayName = "MagicDialogContent";

export function MagicDialogHeader({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("magic-dialog-header", className)} {...props} />;
}

export function MagicDialogFooter({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("magic-dialog-footer", className)} {...props} />;
}

export function MagicDialogTitle({
  className,
  ...props
}: React.HTMLAttributes<HTMLHeadingElement>) {
  const { titleId } = useDialogContext();
  return <h2 id={titleId} className={cn("magic-dialog-title", className)} {...props} />;
}

export function MagicDialogDescription({
  className,
  ...props
}: React.HTMLAttributes<HTMLParagraphElement>) {
  const { descriptionId } = useDialogContext();
  return <p id={descriptionId} className={cn("magic-dialog-description", className)} {...props} />;
}
