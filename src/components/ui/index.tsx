import * as React from "react";
import { IconEye, IconEyeOff } from "@tabler/icons-react";
import {
  Button as HeroButton,
  Checkbox as HeroCheckbox,
  Chip,
  ComboBox,
  Input as HeroInput,
  InputGroup,
  Label,
  ListBox,
  Modal,
  Select as HeroSelect,
  Spinner,
  Switch as HeroSwitch,
  Tooltip as HeroTooltip,
  cn,
  useFilter,
} from "@heroui/react";
import type { Key } from "@heroui/react";
import { UNSAFE_PortalProvider } from "react-aria";

// 本文件是控制台各模块共用的业务控件层：把项目内统一的语义（按钮语义变体、徽章语义、
// optionList 形式的下拉、受控对话框）映射到 HeroUI 组件，避免各处重复拼装复合组件。

export type ToastContainerRegister = (element: HTMLElement | null) => () => void;
export const ToastContainerContext = React.createContext<ToastContainerRegister>(() => () => {});
export function useToastContainer(element: HTMLElement | null, active = true) {
  const register = React.useContext(ToastContainerContext);
  React.useEffect(() => {
    if (!element || !active) return;
    return register(element);
  }, [element, active, register]);
}

/* -------------------------------------------------------------------------------------------------
 * Tooltip
 * -----------------------------------------------------------------------------------------------*/
type TooltipContentProps = React.ComponentProps<typeof HeroTooltip.Content>;
export interface TooltipProps {
  children: React.ReactElement;
  content?: React.ReactNode;
  delay?: number;
  position?: TooltipContentProps["placement"];
}
export function Tooltip({ children, content, delay = 400, position = "top" }: TooltipProps) {
  if (content == null || content === "") return children;
  // 自带焦点行为的按钮可以直接作为触发器；普通元素需要 HeroUI 的触发器包裹以获得悬浮与焦点事件。
  const trigger = children.type === Button
    ? children
    : <HeroTooltip.Trigger className="inline-flex max-w-full">{children}</HeroTooltip.Trigger>;
  return (
    <HeroTooltip delay={delay} closeDelay={80}>
      {trigger}
      <HeroTooltip.Content placement={position} showArrow className="max-w-[420px] text-xs">
        {content}
      </HeroTooltip.Content>
    </HeroTooltip>
  );
}

/* -------------------------------------------------------------------------------------------------
 * Button
 * -----------------------------------------------------------------------------------------------*/
type HeroButtonProps = React.ComponentProps<typeof HeroButton>;
type HeroButtonVariant = NonNullable<HeroButtonProps["variant"]>;
type ButtonVariant =
  | "default" | "light" | "brand-outline" | "warning" | "destructive" | "destructive-light"
  | "outline" | "secondary" | "ghost" | "link" | "filled";
type ButtonColor = "primary" | "danger" | "default";
type ButtonSize = "default" | "sm" | "xs" | "lg" | "icon" | "icon-sm";
export interface ButtonProps extends Omit<HeroButtonProps, "variant" | "size" | "isDisabled" | "isIconOnly" | "children" | "className"> {
  children?: React.ReactNode;
  className?: string;
  color?: ButtonColor;
  disabled?: boolean;
  loading?: boolean;
  size?: ButtonSize;
  title?: string;
  variant?: ButtonVariant;
}
const buttonAppearance: Record<Exclude<ButtonVariant, "link" | "filled">, { variant: HeroButtonVariant; className?: string }> = {
  default: { variant: "primary" },
  light: { variant: "secondary" },
  "brand-outline": { variant: "outline", className: "border-accent text-accent" },
  warning: { variant: "primary", className: "bg-warning text-warning-foreground hover:bg-warning/90" },
  destructive: { variant: "danger" },
  "destructive-light": { variant: "danger-soft" },
  outline: { variant: "outline" },
  secondary: { variant: "tertiary" },
  ghost: { variant: "ghost" },
};
function resolveButtonAppearance(variant: ButtonVariant, color: ButtonColor | undefined) {
  if (variant === "link") {
    return { variant: "ghost" as const, className: cn("px-1.5", color === "danger" ? "text-danger" : "text-accent") };
  }
  if (variant === "filled") return { variant: color === "danger" ? "danger-soft" as const : "secondary" as const };
  return buttonAppearance[variant];
}
const buttonSizing: Record<ButtonSize, { size: HeroButtonProps["size"]; iconOnly?: boolean; className?: string }> = {
  default: { size: "md" },
  sm: { size: "sm" },
  xs: { size: "sm", className: "h-7 min-h-7 px-2.5 text-xs md:h-7" },
  lg: { size: "lg" },
  icon: { size: "md", iconOnly: true },
  "icon-sm": { size: "sm", iconOnly: true, className: "size-7 min-w-7 md:size-7" },
};
export function Button({
  variant = "default", color, size = "default", type = "button", loading, disabled, className, children, title, ...props
}: ButtonProps) {
  const appearance = resolveButtonAppearance(variant, color);
  const sizing = buttonSizing[size];
  const button = (
    <HeroButton
      {...props}
      type={type}
      variant={appearance.variant}
      size={sizing.size}
      isIconOnly={sizing.iconOnly}
      isDisabled={disabled || loading}
      aria-busy={loading || undefined}
      className={cn("gap-1 [&_svg]:size-4 [&_svg]:shrink-0", appearance.className, sizing.className, className)}
    >
      {loading ? <Spinner size="sm" color="current" aria-hidden="true" /> : null}
      {children}
    </HeroButton>
  );
  return title ? <Tooltip content={title}>{button}</Tooltip> : button;
}

/* -------------------------------------------------------------------------------------------------
 * Badge（状态徽章，基于 HeroUI Chip）
 * -----------------------------------------------------------------------------------------------*/
type ChipProps = React.ComponentProps<typeof Chip>;
type BadgeVariant = "default" | "secondary" | "destructive" | "outline" | "success" | "warning" | "info" | "brand";
export type BadgeProps = Omit<React.HTMLAttributes<HTMLSpanElement>, "color"> & { variant?: BadgeVariant };
const badgeAppearance: Record<BadgeVariant, { color: ChipProps["color"]; variant: ChipProps["variant"] }> = {
  default: { color: "accent", variant: "primary" },
  brand: { color: "accent", variant: "primary" },
  info: { color: "accent", variant: "soft" },
  secondary: { color: "default", variant: "secondary" },
  outline: { color: "default", variant: "tertiary" },
  success: { color: "success", variant: "soft" },
  warning: { color: "warning", variant: "soft" },
  destructive: { color: "danger", variant: "soft" },
};
export function Badge({ variant = "default", className, children, ...props }: BadgeProps) {
  const appearance = badgeAppearance[variant];
  return (
    <Chip {...props} size="sm" color={appearance.color} variant={appearance.variant} className={cn("whitespace-nowrap px-2.5", className)}>
      {children}
    </Chip>
  );
}

/* -------------------------------------------------------------------------------------------------
 * Input / PasswordInput
 * -----------------------------------------------------------------------------------------------*/
export interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> {
  error?: boolean;
  leftSection?: React.ReactNode;
  ref?: React.Ref<HTMLInputElement>;
  rightSection?: React.ReactNode;
}
export function Input({ className, leftSection, rightSection, error, disabled, value, defaultValue, ...props }: InputProps) {
  const invalid = Boolean(error) || props["aria-invalid"] === true || props["aria-invalid"] === "true";
  const inputProps = {
    ...props,
    disabled,
    "aria-invalid": invalid || undefined,
    value: value == null ? undefined : String(value),
    defaultValue: defaultValue == null ? undefined : String(defaultValue),
  };
  if (!leftSection && !rightSection) {
    return <HeroInput fullWidth {...inputProps} className={cn("min-w-0", className)} />;
  }
  return (
    <InputGroup fullWidth isDisabled={disabled} isInvalid={invalid} className={cn("min-w-0", className)}>
      {leftSection ? <InputGroup.Prefix className="px-2.5">{leftSection}</InputGroup.Prefix> : null}
      <InputGroup.Input {...inputProps} />
      {rightSection ? <InputGroup.Suffix className="px-2">{rightSection}</InputGroup.Suffix> : null}
    </InputGroup>
  );
}
export interface PasswordInputProps extends InputProps {
  visibility?: boolean;
  onVisibilityChange?: (visible: boolean) => void;
}
export function PasswordInput({ visibility, onVisibilityChange, rightSection, disabled, ...props }: PasswordInputProps) {
  const [internalVisibility, setInternalVisibility] = React.useState(false);
  const visible = visibility ?? internalVisibility;
  const setVisible = (next: boolean) => { setInternalVisibility(next); onVisibilityChange?.(next); };
  return (
    <Input
      {...props}
      disabled={disabled}
      type={visible ? "text" : "password"}
      rightSection={<>
        {rightSection}
        <HeroButton
          variant="ghost"
          size="sm"
          isIconOnly
          isDisabled={disabled}
          aria-label={visible ? "隐藏密码" : "显示密码"}
          aria-pressed={visible}
          className="size-7 min-w-7 rounded-full text-muted md:size-7"
          onPress={() => setVisible(!visible)}
        >
          {visible ? <IconEyeOff size={16} aria-hidden="true" /> : <IconEye size={16} aria-hidden="true" />}
        </HeroButton>
      </>}
    />
  );
}

/* -------------------------------------------------------------------------------------------------
 * Select（optionList 形式；filter 为 true 时使用 ComboBox）
 * -----------------------------------------------------------------------------------------------*/
export type SelectOption = { disabled?: boolean; label: React.ReactNode; value: string | number; [key: string]: unknown };
export interface SelectProps {
  "aria-label"?: string;
  "aria-labelledby"?: string;
  className?: string;
  disabled?: boolean;
  filter?: boolean;
  id?: string;
  onChange?: (value: string | number | null) => void;
  optionList?: SelectOption[];
  placeholder?: string;
  popoverClassName?: string;
  prefix?: React.ReactNode;
  renderOptionItem?: (option: SelectOption & { selected?: boolean }) => React.ReactNode;
  searchPlaceholder?: string;
  value?: string | number;
}
function optionText(option: SelectOption) {
  return typeof option.label === "string" || typeof option.label === "number" ? String(option.label) : String(option.value);
}
export function Select({
  optionList = [], onChange, filter = false, popoverClassName, renderOptionItem, prefix, value, disabled, className, placeholder, searchPlaceholder = "搜索…", id, ...labels
}: SelectProps) {
  const { contains } = useFilter({ sensitivity: "base" });
  const selectedKey: Key | null = value != null && value !== "" ? String(value) : null;
  const handleSelectionChange = (key: Key | null) => {
    if (key == null) {
      onChange?.(null);
      return;
    }
    const matched = optionList.find((option) => String(option.value) === String(key));
    onChange?.(matched ? matched.value : String(key));
  };
  const items = React.useMemo(() => optionList.map((option) => ({ ...option, id: String(option.value) })), [optionList]);
  const renderValue = () => {
    const selected = optionList.find((option) => option.value === value);
    return selected ? <span className="truncate">{selected.label}</span> : <span className="truncate text-field-placeholder">{placeholder ?? "请选择"}</span>;
  };
  const list = (
    <ListBox items={items} aria-label={labels["aria-label"] ?? "选项"} className="max-h-72 overflow-y-auto">
      {(option) => (
        <ListBox.Item id={option.id} textValue={optionText(option)} isDisabled={option.disabled}>
          <span className="min-w-0 flex-1 truncate">
            {renderOptionItem ? renderOptionItem({ ...option, selected: option.value === value }) : option.label}
          </span>
          <ListBox.ItemIndicator />
        </ListBox.Item>
      )}
    </ListBox>
  );
  if (filter) {
    return (
      <ComboBox
        {...labels}
        fullWidth
        className={cn("min-w-0", className)}
        defaultFilter={(textValue, inputValue) => contains(textValue, inputValue.trim())}
        isDisabled={disabled}
        selectedKey={selectedKey}
        onSelectionChange={handleSelectionChange}
        menuTrigger="focus"
      >
        <ComboBox.InputGroup>
          {prefix ? <InputGroup.Prefix className="px-2">{prefix}</InputGroup.Prefix> : null}
          <HeroInput
            id={id}
            placeholder={placeholder}
            autoComplete="off"
            spellCheck={false}
            className="min-h-8 md:min-h-8"
          />
          <ComboBox.Trigger />
        </ComboBox.InputGroup>
        <ComboBox.Popover className={cn("w-(--trigger-width) min-w-44", popoverClassName)}>
          {list}
        </ComboBox.Popover>
      </ComboBox>
    );
  }
  return (
    <HeroSelect
      {...labels}
      fullWidth
      className={cn("min-w-0", className)}
      isDisabled={disabled}
      placeholder={placeholder}
      selectedKey={selectedKey}
      onSelectionChange={handleSelectionChange}
    >
      <HeroSelect.Trigger id={id} className="min-h-8 md:min-h-8">
        {prefix}
        <HeroSelect.Value>{renderValue}</HeroSelect.Value>
        <HeroSelect.Indicator />
      </HeroSelect.Trigger>
      <HeroSelect.Popover className={popoverClassName}>{list}</HeroSelect.Popover>
    </HeroSelect>
  );
}

/* -------------------------------------------------------------------------------------------------
 * Checkbox / Switch
 * -----------------------------------------------------------------------------------------------*/
type HeroCheckboxProps = React.ComponentProps<typeof HeroCheckbox>;
export interface CheckboxProps extends Omit<HeroCheckboxProps, "isSelected" | "defaultSelected" | "isIndeterminate" | "onChange" | "isDisabled" | "children"> {
  checked?: boolean | "indeterminate";
  children?: React.ReactNode;
  disabled?: boolean;
  label?: React.ReactNode;
  onCheckedChange?: (checked: boolean | "indeterminate") => void;
}
export function Checkbox({ checked, onCheckedChange, label, disabled, children, className, ...props }: CheckboxProps) {
  const content = label ?? children;
  return (
    <HeroCheckbox
      {...props}
      className={cn("items-center", className)}
      isSelected={checked === undefined ? undefined : checked === true}
      isIndeterminate={checked === "indeterminate"}
      isDisabled={disabled}
      onChange={(selected) => onCheckedChange?.(selected)}
    >
      <HeroCheckbox.Content className="items-center gap-2">
        <HeroCheckbox.Control><HeroCheckbox.Indicator /></HeroCheckbox.Control>
        {content != null ? <Label className="text-xs">{content}</Label> : null}
      </HeroCheckbox.Content>
    </HeroCheckbox>
  );
}
type HeroSwitchProps = React.ComponentProps<typeof HeroSwitch>;
export interface SwitchProps extends Omit<HeroSwitchProps, "isSelected" | "defaultSelected" | "onChange" | "isDisabled" | "size" | "children"> {
  "aria-busy"?: React.AriaAttributes["aria-busy"];
  checked?: boolean;
  disabled?: boolean;
  label?: React.ReactNode;
  loading?: boolean;
  onCheckedChange?: (checked: boolean) => void;
  size?: "sm" | "xs";
}
export function Switch({ size, onCheckedChange, loading, disabled, checked, label, "aria-busy": ariaBusy, className, ...props }: SwitchProps) {
  const busy = loading || ariaBusy === true || ariaBusy === "true";
  return (
    <HeroSwitch
      {...props}
      className={cn("items-center", className)}
      size={size ? "sm" : "md"}
      isSelected={checked}
      isDisabled={disabled || busy}
      aria-busy={busy || undefined}
      onChange={onCheckedChange}
    >
      <HeroSwitch.Content className="items-center gap-2">
        <HeroSwitch.Control><HeroSwitch.Thumb /></HeroSwitch.Control>
        {label != null ? <Label className="text-xs">{label}</Label> : null}
      </HeroSwitch.Content>
    </HeroSwitch>
  );
}

/* -------------------------------------------------------------------------------------------------
 * Dialog（受控对话框，基于 HeroUI Modal）
 * -----------------------------------------------------------------------------------------------*/
type DialogContextValue = { open: boolean; setOpen: (open: boolean) => void };
const DialogContext = React.createContext<DialogContextValue | null>(null);
const DialogLabelContext = React.createContext<{ descriptionId: string; titleId: string } | null>(null);
export interface DialogProps { children?: React.ReactNode; onOpenChange?: (open: boolean) => void; open: boolean }
export function Dialog({ children, onOpenChange, open }: DialogProps) {
  const setOpen = React.useCallback((nextOpen: boolean) => { onOpenChange?.(nextOpen); }, [onOpenChange]);
  const value = React.useMemo(() => ({ open, setOpen }), [open, setOpen]);
  return <DialogContext.Provider value={value}>{children}</DialogContext.Provider>;
}
export interface DialogDismissEvent { readonly defaultPrevented: boolean; preventDefault: () => void }
export interface DialogContentProps {
  children?: React.ReactNode;
  className?: string;
  /** 指定弹层挂载容器；未指定时沿用 UiProvider 的弹层容器。 */
  container?: HTMLElement | null;
  onEscapeKeyDown?: (event: DialogDismissEvent) => void;
  onPointerDownOutside?: (event: DialogDismissEvent) => void;
}
export function DialogContent({ children, className, container, onEscapeKeyDown, onPointerDownOutside }: DialogContentProps) {
  const dialog = React.useContext(DialogContext);
  const [toastHostEl, setToastHostEl] = React.useState<HTMLDivElement | null>(null);
  useToastContainer(toastHostEl, dialog?.open ?? false);
  const id = React.useId();
  const getContainer = React.useCallback(() => container ?? null, [container]);
  if (!dialog) throw new Error("DialogContent must be rendered inside Dialog");
  const labels = { titleId: `codey-dialog-title-${id}`, descriptionId: `codey-dialog-description-${id}` };
  // Esc 与遮罩点击都经由 onOpenChange(false) 到达；调用方可通过 preventDefault 在忙碌时阻止关闭。
  const handleOpenChange = (nextOpen: boolean) => {
    if (nextOpen) return;
    const event = { defaultPrevented: false, preventDefault() { this.defaultPrevented = true; } };
    onEscapeKeyDown?.(event);
    onPointerDownOutside?.(event);
    if (!event.defaultPrevented) dialog.setOpen(false);
  };
  // 对话框由外部状态控制、没有触发按钮，直接从 Backdrop 层接管开关状态，
  // 避免 Modal 根节点（DialogTrigger）因缺少触发器而发出警告。
  // 标题与描述使用显式 id 关联：react-aria 的插槽 id 检测依赖 document.getElementById，
  // 在 ShadowRoot 内找不到标题元素。
  const hasCustomWidth = Boolean(className && /\b(sm:)?(w-|max-w-)/.test(className));
  const modal = (
    <Modal.Backdrop isOpen={dialog.open} onOpenChange={handleOpenChange} isDismissable className="p-0">
      <Modal.Container placement="center" scroll="outside" className="p-4 data-[entering=true]:zoom-in-95">
        <Modal.Dialog
          className={cn("w-full max-w-[calc(100vw-32px)] text-sm relative overflow-hidden", !hasCustomWidth && "sm:w-[480px]", className)}
          aria-labelledby={labels.titleId}
          aria-describedby={labels.descriptionId}
        >
          <div
            ref={setToastHostEl}
            className="toast-portal-host pointer-events-none absolute inset-x-0 top-0 z-[100] h-0"
            aria-hidden="true"
          />
          <Modal.CloseTrigger aria-label="关闭" />
          <DialogLabelContext.Provider value={labels}>{children}</DialogLabelContext.Provider>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
  return container ? <UNSAFE_PortalProvider getContainer={getContainer}>{modal}</UNSAFE_PortalProvider> : modal;
}
export function DialogHeader({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <Modal.Header {...props} className={cn("gap-1.5 pr-9", className)} />;
}
export function DialogFooter({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <Modal.Footer {...props} className={cn("mt-5 gap-2", className)} />;
}
export function DialogTitle({ id, className, ...props }: React.ComponentProps<typeof Modal.Heading>) {
  const labels = React.useContext(DialogLabelContext);
  return <Modal.Heading {...props} id={id ?? labels?.titleId} className={cn("text-[17px] font-semibold text-foreground", className)} />;
}
export function DialogDescription({ id, className, ...props }: React.HTMLAttributes<HTMLParagraphElement>) {
  const labels = React.useContext(DialogLabelContext);
  return <p {...props} id={id ?? labels?.descriptionId} className={cn("m-0 text-xs leading-relaxed text-muted", className)} />;
}
