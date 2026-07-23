import * as React from "react";
import { IconX } from "@tabler/icons-react";
import SemiButton from "@douyinfe/semi-ui/lib/es/button";
import SemiCard from "@douyinfe/semi-ui/lib/es/card";
import SemiCheckbox from "@douyinfe/semi-ui/lib/es/checkbox";
import SemiInput from "@douyinfe/semi-ui/lib/es/input";
import Modal from "@douyinfe/semi-ui/lib/es/modal";
import Popover from "@douyinfe/semi-ui/lib/es/popover";
import SemiSwitch from "@douyinfe/semi-ui/lib/es/switch";
import Tag from "@douyinfe/semi-ui/lib/es/tag";
import Tooltip from "@douyinfe/semi-ui/lib/es/tooltip";

export { Popover, Tooltip };

type ButtonVariant = "default" | "warning" | "destructive" | "outline" | "secondary" | "ghost";
type ButtonSize = "default" | "sm" | "xs" | "lg" | "icon" | "icon-sm";

function classNames(...names: Array<string | false | null | undefined>) {
  return names.filter(Boolean).join(" ");
}

export interface ButtonProps extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "type"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  type?: "button" | "reset" | "submit";
}

const buttonAppearance = {
  default: { theme: "solid", type: "primary" },
  warning: { theme: "solid", type: "warning" },
  destructive: { theme: "solid", type: "danger" },
  outline: { theme: "outline", type: "tertiary" },
  secondary: { theme: "light", type: "secondary" },
  ghost: { theme: "borderless", type: "tertiary" },
} as const;

const buttonSize = {
  default: "default",
  sm: "small",
  xs: "small",
  lg: "large",
  icon: "default",
  "icon-sm": "small",
} as const;

export function Button({
  className,
  variant = "default",
  size = "default",
  type = "button",
  ...props
}: ButtonProps) {
  const appearance = buttonAppearance[variant];
  return (
    <SemiButton
      {...props}
      className={classNames(
        "codey-button",
        `codey-button--${variant}`,
        `codey-button--size-${size}`,
        className,
      )}
      htmlType={type}
      size={buttonSize[size]}
      theme={appearance.theme}
      type={appearance.type}
    />
  );
}

type BadgeVariant =
  | "default"
  | "secondary"
  | "destructive"
  | "outline"
  | "success"
  | "warning"
  | "info"
  | "brand";

export interface BadgeProps {
  children?: React.ReactNode;
  className?: string;
  style?: React.CSSProperties;
  variant?: BadgeVariant;
}

const badgeAppearance = {
  default: { color: "grey", type: "light" },
  secondary: { color: "grey", type: "light" },
  destructive: { color: "red", type: "light" },
  outline: { color: "grey", type: "ghost" },
  success: { color: "green", type: "light" },
  warning: { color: "orange", type: "light" },
  info: { color: "light-blue", type: "light" },
  brand: { color: "blue", type: "light" },
} as const;

export function Badge({
  className,
  variant = "default",
  ...props
}: BadgeProps) {
  const appearance = badgeAppearance[variant];
  return (
    <Tag
      {...props}
      className={classNames("codey-tag", className)}
      color={appearance.color}
      shape="circle"
      size="small"
      type={appearance.type}
    />
  );
}

type SemiCardProps = React.ComponentProps<typeof SemiCard>;

export type CardProps = SemiCardProps & {
  "aria-busy"?: boolean;
};

export function Card({
  bodyStyle,
  loading,
  "aria-busy": ariaBusy,
  ...props
}: CardProps) {
  return (
    <SemiCard
      {...props}
      bodyStyle={{ display: "contents", ...bodyStyle }}
      loading={loading ?? ariaBusy}
    />
  );
}

export interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> {}

export const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ className, onChange, onInput, ...props }, ref) => (
    <SemiInput
      {...props}
      ref={ref}
      className={classNames("codey-input", className)}
      onChange={(_value, event) => onChange?.(event)}
      onInput={(event) => onInput?.(event as unknown as React.InputEvent<HTMLInputElement>)}
    />
  ),
);
Input.displayName = "Input";

type SemiCheckboxProps = React.ComponentProps<typeof SemiCheckbox>;

export type CheckboxProps = Omit<
  SemiCheckboxProps,
  "checked" | "defaultChecked" | "indeterminate" | "onChange"
> & {
  checked?: boolean | "indeterminate";
  defaultChecked?: boolean | "indeterminate";
  onCheckedChange?: (checked: boolean | "indeterminate") => void;
};

export function Checkbox({
  checked,
  className,
  defaultChecked,
  onCheckedChange,
  ...props
}: CheckboxProps) {
  const checkedProps = checked === undefined
    ? {}
    : { checked: checked === true };
  const defaultCheckedProps = defaultChecked === undefined
    ? {}
    : { defaultChecked: defaultChecked === true };
  return (
    <SemiCheckbox
      {...props}
      {...checkedProps}
      {...defaultCheckedProps}
      className={classNames("codey-checkbox", className)}
      indeterminate={checked === "indeterminate" || defaultChecked === "indeterminate"}
      onChange={(event) => onCheckedChange?.(event.target.checked === true)}
    />
  );
}

type SemiSwitchProps = React.ComponentProps<typeof SemiSwitch>;

export type SwitchProps = Omit<SemiSwitchProps, "loading" | "onChange"> & {
  "aria-busy"?: React.AriaAttributes["aria-busy"];
  loading?: boolean;
  onCheckedChange?: (checked: boolean) => void;
};

export function Switch({
  "aria-busy": ariaBusy,
  className,
  loading = false,
  onCheckedChange,
  ...props
}: SwitchProps) {
  return (
    <SemiSwitch
      {...props}
      className={classNames("codey-switch", className)}
      loading={loading || ariaBusy === true || ariaBusy === "true"}
      onChange={(checked) => onCheckedChange?.(checked)}
    />
  );
}

type DialogContextValue = {
  open: boolean;
  setOpen: (open: boolean) => void;
};

type DialogLabelContextValue = {
  descriptionId: string;
  titleId: string;
};

const DialogContext = React.createContext<DialogContextValue | null>(null);
const DialogLabelContext = React.createContext<DialogLabelContextValue | null>(null);

export interface DialogProps {
  children?: React.ReactNode;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  open?: boolean;
}

export function Dialog({
  children,
  defaultOpen = false,
  onOpenChange,
  open,
}: DialogProps) {
  const [internalOpen, setInternalOpen] = React.useState(defaultOpen);
  const currentOpen = open ?? internalOpen;
  const setOpen = React.useCallback((nextOpen: boolean) => {
    if (open === undefined) setInternalOpen(nextOpen);
    onOpenChange?.(nextOpen);
  }, [onOpenChange, open]);

  return (
    <DialogContext.Provider value={{ open: currentOpen, setOpen }}>
      {children}
    </DialogContext.Provider>
  );
}

export interface DialogDismissEvent {
  readonly defaultPrevented: boolean;
  readonly originalEvent?: Event;
  preventDefault: () => void;
}

export interface DialogContentProps {
  children?: React.ReactNode;
  className?: string;
  container?: HTMLElement | null;
  onEscapeKeyDown?: (event: DialogDismissEvent) => void;
  onPointerDownOutside?: (event: DialogDismissEvent) => void;
}

function createDismissEvent(originalEvent?: Event): DialogDismissEvent {
  let defaultPrevented = false;
  return {
    get defaultPrevented() {
      return defaultPrevented;
    },
    originalEvent,
    preventDefault() {
      defaultPrevented = true;
    },
  };
}

export function DialogContent({
  children,
  className,
  container,
  onEscapeKeyDown,
  onPointerDownOutside,
}: DialogContentProps) {
  const dialog = React.useContext(DialogContext);
  if (!dialog) throw new Error("DialogContent must be rendered inside Dialog");
  const generatedId = React.useId().replace(/:/g, "");
  const titleId = `codey-dialog-title-${generatedId}`;
  const descriptionId = `codey-dialog-description-${generatedId}`;

  const handleCancel = (event: React.MouseEvent) => {
    const originalEvent = event?.nativeEvent as Event | undefined;
    const keyboardEvent = originalEvent as KeyboardEvent | undefined;
    const handler = keyboardEvent?.key === "Escape"
      ? onEscapeKeyDown
      : onPointerDownOutside;
    const dismissEvent = createDismissEvent(originalEvent);
    handler?.(dismissEvent);
    if (!dismissEvent.defaultPrevented) dialog.setOpen(false);
  };

  return (
    <Modal
      centered
      className="codey-dialog-layer"
      closeOnEsc
      closable={false}
      footer={null}
      getPopupContainer={container ? () => container : undefined}
      mask
      maskClosable
      modalRender={(node) => (
        React.isValidElement<Record<string, unknown>>(node)
          ? React.cloneElement(node, {
              "aria-describedby": descriptionId,
              "aria-labelledby": titleId,
            })
          : node
      )}
      modalContentClass={classNames("codey-dialog-content", className)}
      onCancel={handleCancel}
      visible={dialog.open}
      width={512}
    >
      <DialogLabelContext.Provider value={{ descriptionId, titleId }}>
        <SemiButton
          aria-label="关闭"
          className="codey-dialog-close"
          icon={<IconX size={18} aria-hidden="true" />}
          onClick={() => dialog.setOpen(false)}
          theme="borderless"
          type="tertiary"
        />
        {children}
      </DialogLabelContext.Provider>
    </Modal>
  );
}

export function DialogHeader({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={classNames("codey-dialog-header", className)} {...props} />;
}

export function DialogFooter({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={classNames("codey-dialog-footer", className)} {...props} />;
}

export const DialogTitle = React.forwardRef<
  HTMLHeadingElement,
  React.HTMLAttributes<HTMLHeadingElement>
>(({ className, id, ...props }, ref) => {
  const labels = React.useContext(DialogLabelContext);
  return (
    <h2
      {...props}
      ref={ref}
      className={classNames("codey-dialog-title", className)}
      id={id ?? labels?.titleId}
    />
  );
});
DialogTitle.displayName = "DialogTitle";

export const DialogDescription = React.forwardRef<
  HTMLParagraphElement,
  React.HTMLAttributes<HTMLParagraphElement>
>(({ className, id, ...props }, ref) => {
  const labels = React.useContext(DialogLabelContext);
  return (
    <p
      {...props}
      ref={ref}
      className={classNames("codey-dialog-description", className)}
      id={id ?? labels?.descriptionId}
    />
  );
});
DialogDescription.displayName = "DialogDescription";
