import * as React from "react";
import { IconEye, IconEyeOff, IconX } from "@tabler/icons-react";
import {
  Button as AntButton, Card as AntCard, Checkbox as AntCheckbox,
  Input as AntInput, Modal, Select as AntSelect, Switch as AntSwitch,
  Tag, Tooltip as AntTooltip,
} from "antd";
import type { ButtonProps as AntButtonProps, InputProps as AntInputProps, SwitchProps as AntSwitchProps, TooltipProps as AntTooltipProps } from "antd";

function classNames(...names: Array<string | false | null | undefined>) {
  return names.filter(Boolean).join(" ");
}

export interface TooltipProps extends Omit<AntTooltipProps, "title" | "children" | "zIndex"> {
  children: React.ReactElement;
  content?: React.ReactNode;
  position?: AntTooltipProps["placement"];
  zIndex?: number | string;
  autoAdjustOverflow?: boolean;
  arrowPointAtCenter?: boolean;
}
export function Tooltip({ content, position, zIndex, autoAdjustOverflow = true, arrowPointAtCenter, ...props }: TooltipProps) {
  return <AntTooltip {...props} title={content} placement={position} zIndex={zIndex == null ? undefined : Number(zIndex)} autoAdjustOverflow={autoAdjustOverflow} arrow={{ pointAtCenter: arrowPointAtCenter }} />;
}

type ButtonVariant = "default" | "light" | "brand-outline" | "warning" | "destructive" | "destructive-light" | "outline" | "secondary" | "ghost" | "link";
type ButtonSize = "default" | "sm" | "xs" | "lg" | "icon" | "icon-sm";
export interface ButtonProps extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "color" | "onClick"> {
  onClick?: AntButtonProps["onClick"];
  color?: AntButtonProps["color"];
  variant?: ButtonVariant | AntButtonProps["variant"];
  size?: ButtonSize;
  loading?: AntButtonProps["loading"];
  autoInsertSpace?: boolean;
}
const buttonAppearance: Record<ButtonVariant, Pick<AntButtonProps, "color" | "variant">> = {
  default: { color: "primary", variant: "solid" }, light: { color: "primary", variant: "filled" }, "brand-outline": { color: "primary", variant: "outlined" },
  warning: { color: "orange", variant: "solid" }, destructive: { color: "danger", variant: "solid" },
  "destructive-light": { color: "danger", variant: "filled" }, outline: { color: "default", variant: "outlined" }, secondary: { color: "default", variant: "filled" }, ghost: { color: "default", variant: "text" },
  link: { color: "primary", variant: "link" },
};
const buttonSize = { default: "middle", sm: "small", xs: "small", lg: "large", icon: "middle", "icon-sm": "small" } as const;
export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(function Button({ variant = "default", color, size = "default", type = "button", loading, autoInsertSpace, children, ...props }, ref) {
  const preset = variant && variant in buttonAppearance ? buttonAppearance[variant as ButtonVariant] : undefined;
  const resolvedColor = color ?? preset?.color;
  const resolvedVariant = color
    ? (variant === "default" ? "solid" : (variant as AntButtonProps["variant"]))
    : (preset?.variant ?? (variant as AntButtonProps["variant"]));
  return <AntButton autoInsertSpace={autoInsertSpace ?? false} {...props} color={resolvedColor} variant={resolvedVariant} ref={ref} htmlType={type} size={buttonSize[size]} loading={loading}>
    <span className="inline-flex items-center justify-center gap-1 [&_svg]:max-h-4 [&_svg]:max-w-4 [&_svg]:shrink-0">{children}</span>
  </AntButton>;
});

type BadgeVariant = "default" | "secondary" | "destructive" | "outline" | "success" | "warning" | "info" | "brand";
export type BadgeProps = Omit<React.HTMLAttributes<HTMLDivElement>, "color"> & { variant?: BadgeVariant; size?: "xs" | "sm" | "md" | "lg" };
const badgeColors = { default: undefined, secondary: undefined, destructive: "error", outline: undefined, success: "success", warning: "warning", info: "processing", brand: "blue" };
export function Badge({ variant = "default", size: _size, className, ...props }: BadgeProps) {
  return <Tag {...props} className={className} color={badgeColors[variant]} variant={variant === "outline" ? "outlined" : "filled"} />;
}
export { Tag } from "antd";
export type { TagProps } from "antd";

export type CardProps = React.HTMLAttributes<HTMLDivElement> & { bodyStyle?: React.CSSProperties; loading?: boolean };
export function Card({ bodyStyle, loading, ...props }: CardProps) {
  return <AntCard {...props} loading={loading} styles={{ body: { padding: 0, display: "contents", ...bodyStyle } }} />;
}

export interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size" | "prefix"> {
  error?: boolean;
  leftSection?: React.ReactNode;
  rightSection?: React.ReactNode;
  wrapperClassName?: string;
}
function inputProps({ className, wrapperClassName, leftSection, rightSection, onChange, value, defaultValue, error, ...props }: InputProps): AntInputProps {
  return { ...props, className: classNames("min-w-0 flex-1", wrapperClassName, className), prefix: leftSection, suffix: rightSection,
    value: value == null ? undefined : String(value), defaultValue: defaultValue == null ? undefined : String(defaultValue),
    status: error || props["aria-invalid"] === true ? "error" : undefined,
    onChange,
  };
}
export const Input = React.forwardRef<HTMLInputElement, InputProps>(function Input(props, ref) {
  return <AntInput {...inputProps(props)} ref={(handle) => { if (typeof ref === "function") ref(handle?.input ?? null); else if (ref) ref.current = handle?.input ?? null; }} />;
});
export interface PasswordInputProps extends InputProps { visibility?: boolean; onVisibilityChange?: (visible: boolean) => void }
export function PasswordInput({ visibility, onVisibilityChange, ...props }: PasswordInputProps) {
  const [internalVisibility, setInternalVisibility] = React.useState(false);
  const visible = visibility ?? internalVisibility;
  const setVisible = (next: boolean) => { setInternalVisibility(next); onVisibilityChange?.(next); };
  return <AntInput.Password {...inputProps(props)} visibilityToggle={{ visible, onVisibleChange: setVisible }}
    iconRender={() => visible ? <IconEyeOff size={16} aria-hidden="true" /> : <IconEye size={16} aria-hidden="true" />} />;
}

export type SelectOption = { disabled?: boolean; label: React.ReactNode; value: string | number; [key: string]: unknown };
export interface SelectProps {
  "aria-label"?: string;
  "aria-labelledby"?: string;
  className?: string;
  disabled?: boolean;
  dropdownClassName?: string;
  emptyContent?: React.ReactNode;
  filter?: boolean;
  getPopupContainer?: () => HTMLElement;
  id?: string;
  onChange?: (value: string | number | null) => void;
  optionList?: SelectOption[];
  placeholder?: string;
  prefix?: React.ReactNode;
  renderOptionItem?: (option: SelectOption & { selected?: boolean }) => React.ReactNode;
  showClear?: boolean;
  value?: string | number;
  zIndex?: number;
}
export function Select({ optionList = [], onChange, filter = false, showClear = false, dropdownClassName, emptyContent, renderOptionItem, zIndex, ...props }: SelectProps) {
  return <AntSelect {...props} showSearch={filter ? { filterOption: (input, option) => String(option?.label ?? "").toLocaleLowerCase().includes(input.trim().toLocaleLowerCase()) } : false}
    allowClear={showClear} onChange={(value) => onChange?.(value ?? null)} options={optionList}
    classNames={{ popup: { root: dropdownClassName } }} styles={{ popup: { root: { zIndex } } }} notFoundContent={emptyContent}
    optionRender={renderOptionItem ? (option) => renderOptionItem({ ...option.data, selected: option.value === props.value }) : undefined} />;
}

export interface CheckboxProps extends Omit<React.ComponentProps<typeof AntCheckbox>, "checked" | "defaultChecked" | "onChange" | "indeterminate"> {
  label?: React.ReactNode;
  checked?: boolean | "indeterminate";
  defaultChecked?: boolean | "indeterminate";
  onCheckedChange?: (checked: boolean | "indeterminate") => void;
}
export function Checkbox({ checked, defaultChecked, onCheckedChange, label, ...props }: CheckboxProps) {
  return <AntCheckbox {...props} checked={checked === undefined ? undefined : checked === true} defaultChecked={defaultChecked === true} indeterminate={checked === "indeterminate" || defaultChecked === "indeterminate"} onChange={(event) => onCheckedChange?.(event.target.checked)}>{label ?? props.children}</AntCheckbox>;
}
export interface SwitchProps extends Omit<AntSwitchProps, "onChange" | "size"> {
  size?: "sm" | "xs";
  "aria-busy"?: React.AriaAttributes["aria-busy"];
  onCheckedChange?: (checked: boolean) => void;
}
export function Switch({ size, onCheckedChange, loading, disabled, "aria-busy": ariaBusy, ...props }: SwitchProps) {
  const busy = loading || ariaBusy === true || ariaBusy === "true";
  return <AntSwitch {...props} size={size ? "small" : "medium"} loading={busy} disabled={disabled || busy} aria-busy={busy || undefined} onChange={onCheckedChange} />;
}

type DialogContextValue = { open: boolean; setOpen: (open: boolean) => void };
const DialogContext = React.createContext<DialogContextValue | null>(null);
const DialogLabelContext = React.createContext<{ descriptionId: string; titleId: string } | null>(null);
export interface DialogProps { children?: React.ReactNode; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void; open?: boolean }
export function Dialog({ children, defaultOpen = false, onOpenChange, open }: DialogProps) {
  const [internalOpen, setInternalOpen] = React.useState(defaultOpen);
  const setOpen = React.useCallback((nextOpen: boolean) => { if (open === undefined) setInternalOpen(nextOpen); onOpenChange?.(nextOpen); }, [onOpenChange, open]);
  const value = React.useMemo(() => ({ open: open ?? internalOpen, setOpen }), [open, internalOpen, setOpen]);
  return <DialogContext.Provider value={value}>{children}</DialogContext.Provider>;
}
export interface DialogDismissEvent { readonly defaultPrevented: boolean; preventDefault: () => void }
function dialogTitle(children: React.ReactNode): React.ReactNode {
  for (const child of React.Children.toArray(children)) {
    if (!React.isValidElement<{ children?: React.ReactNode }>(child)) continue;
    if (child.type === DialogTitle) return child.props.children;
    const title = dialogTitle(child.props.children);
    if (title != null) return title;
  }
  return null;
}
export interface DialogContentProps { children?: React.ReactNode; className?: string; container?: HTMLElement | null; onEscapeKeyDown?: (event: DialogDismissEvent) => void; onPointerDownOutside?: (event: DialogDismissEvent) => void; zIndex?: number }
export function DialogContent({ children, className, container, onEscapeKeyDown, onPointerDownOutside, zIndex }: DialogContentProps) {
  const dialog = React.useContext(DialogContext);
  const id = React.useId();
  if (!dialog) throw new Error("DialogContent must be rendered inside Dialog");
  const labels = { titleId: `codey-dialog-title-${id}`, descriptionId: `codey-dialog-description-${id}` };
  const handleCancel = () => {
    const event = { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
    onEscapeKeyDown?.(event);
    onPointerDownOutside?.(event);
    if (!event.defaultPrevented) dialog.setOpen(false);
  };
  return <Modal open={dialog.open} onCancel={handleCancel} footer={null} title={dialogTitle(children)} styles={{ header: { display: "none" } }} centered closable keyboard mask={{ closable: true }}
    closeIcon={<IconX size={16} aria-label="关闭" />}
    className={className} style={{ maxWidth: "calc(100vw - 32px)" }} zIndex={zIndex ?? 1050}
    getContainer={container ?? undefined}>
    <DialogLabelContext.Provider value={labels}>{children}</DialogLabelContext.Provider>
  </Modal>;
}
export function DialogHeader({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) { return <div {...props} className={classNames("grid gap-1.5 pr-9", className)} />; }
export function DialogFooter({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) { return <div {...props} className={classNames("mt-5 flex items-center justify-end gap-2", className)} />; }
export const DialogTitle = React.forwardRef<HTMLHeadingElement, React.HTMLAttributes<HTMLHeadingElement>>(function DialogTitle({ id, className, ...props }, ref) {
  const labels = React.useContext(DialogLabelContext);
  return <h2 {...props} ref={ref} id={id ?? labels?.titleId} className={classNames("m-0 text-[17px] font-semibold", className)} />;
});
export const DialogDescription = React.forwardRef<HTMLParagraphElement, React.HTMLAttributes<HTMLParagraphElement>>(function DialogDescription({ id, className, ...props }, ref) {
  const labels = React.useContext(DialogLabelContext);
  return <p {...props} ref={ref} id={id ?? labels?.descriptionId} className={classNames("m-0 text-xs leading-relaxed text-gray-500", className)} />;
});

export { Listy } from "antd";
export type { ListyProps } from "antd";
