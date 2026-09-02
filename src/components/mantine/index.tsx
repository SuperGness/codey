import * as React from "react";
import {
  Badge as MantineBadge,
  Button as MantineButton,
  Card as MantineCard,
  Collapse as MantineCollapse,
  Checkbox as MantineCheckbox,
  Combobox,
  Input as MantineInput,
  InputBase,
  Modal,
  PasswordInput as MantinePasswordInput,
  Select as MantineSelect,
  Switch as MantineSwitch,
  Tooltip as MantineTooltip,
  useCombobox,
} from "@mantine/core";
import type {
  BadgeProps as MantineBadgeProps,
  CardProps as MantineCardProps,
} from "@mantine/core";
import { IconX } from "@tabler/icons-react";

export { ActionIcon, Table } from "@mantine/core";
export { Combobox, InputBase, useCombobox };

function classNames(...names: Array<string | false | null | undefined>) {
  return names.filter(Boolean).join(" ");
}

export interface TooltipProps
  extends Omit<React.ComponentProps<typeof MantineTooltip>, "label" | "content"> {
  arrowPointAtCenter?: boolean;
  autoAdjustOverflow?: boolean;
  children: React.ReactElement;
  content?: React.ReactNode;
  label?: React.ReactNode;
  getPopupContainer?: () => HTMLElement;
  position?: React.ComponentProps<typeof MantineTooltip>["position"];
  zIndex?: number | string;
}

export function Tooltip({
  arrowPointAtCenter = true,
  autoAdjustOverflow = false,
  children,
  content,
  label,
  getPopupContainer,
  position,
  zIndex,
  ...props
}: TooltipProps) {
  const tooltipLabel = label ?? content;
  return (
    <MantineTooltip
      {...props}
      label={tooltipLabel}
      middlewares={{
        flip: autoAdjustOverflow,
        shift: autoAdjustOverflow,
      }}
      portalProps={
        getPopupContainer
          ? { target: getPopupContainer() }
          : props.portalProps
      }
      position={position}
      withArrow={props.withArrow ?? arrowPointAtCenter}
      withinPortal={Boolean(getPopupContainer) || (props.withinPortal ?? false)}
      zIndex={zIndex}
    >
      {children}
    </MantineTooltip>
  );
}

type ButtonVariant =
  | "default"
  | "light"
  | "brand-outline"
  | "warning"
  | "destructive"
  | "destructive-light"
  | "outline"
  | "secondary"
  | "ghost";
type ButtonSize = "default" | "sm" | "xs" | "lg" | "icon" | "icon-sm";

export interface ButtonProps
  extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "color"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

const buttonAppearance = {
  default: { color: "blue", variant: "filled" },
  light: { color: "blue", variant: "light" },
  "brand-outline": { color: "blue", variant: "outline" },
  warning: { color: "yellow", variant: "filled" },
  destructive: { color: "red", variant: "filled" },
  "destructive-light": { color: "red", variant: "light" },
  outline: { color: "gray", variant: "outline" },
  secondary: { color: "gray", variant: "light" },
  ghost: { color: "gray", variant: "subtle" },
} as const;

const buttonSize = {
  default: "sm",
  sm: "xs",
  xs: "compact-xs",
  lg: "md",
  icon: "compact-sm",
  "icon-sm": "compact-xs",
} as const;

const buttonSizeClassName = {
  default: "h-9 px-3.5",
  sm: "h-[30px] px-2.5 text-xs",
  xs: "h-[26px] px-2 text-[11px]",
  lg: "h-10 px-4",
  icon: "p-0",
  "icon-sm": "h-[30px] w-[30px] p-0",
} as const;

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  function Button(
    {
      className,
      variant = "default",
      size = "default",
      type = "button",
      ...props
    },
    ref,
  ) {
    const appearance = buttonAppearance[variant];
    return (
      <MantineButton
        {...props}
        ref={ref}
        className={classNames(
          "inline-flex items-center justify-center gap-1.5 font-semibold transition-colors duration-150 [&_svg]:max-h-4 [&_svg]:max-w-4 [&_svg]:shrink-0 [&_.mantine-Button-inner]:gap-1.5 [&_.mantine-Button-label]:inline-flex [&_.mantine-Button-label]:items-center [&_.mantine-Button-label]:gap-1.5",
          buttonSizeClassName[size],
          className,
        )}
        color={appearance.color}
        size={buttonSize[size]}
        type={type}
        variant={appearance.variant}
      />
    );
  },
);
Button.displayName = "Button";

type BadgeVariant =
  | "default"
  | "secondary"
  | "destructive"
  | "outline"
  | "success"
  | "warning"
  | "info"
  | "brand";

type BadgeNativeProps = React.ComponentPropsWithoutRef<"div">;

export type BadgeProps = Omit<BadgeNativeProps, "color"> &
  Omit<
    MantineBadgeProps,
    keyof BadgeNativeProps | "color" | "variant"
  > & {
  variant?: BadgeVariant;
};

const badgeAppearance = {
  default: { color: "gray", variant: "light" },
  secondary: { color: "gray", variant: "light" },
  destructive: { color: "red", variant: "light" },
  outline: { color: "gray", variant: "outline" },
  success: { color: "green", variant: "light" },
  warning: { color: "yellow", variant: "light" },
  info: { color: "cyan", variant: "light" },
  brand: { color: "blue", variant: "light" },
} as const;

export function Badge({
  className,
  variant = "default",
  ...props
}: BadgeProps) {
  const appearance = badgeAppearance[variant];
  return (
    <MantineBadge
      {...props}
      className={classNames(
        "select-none normal-case",
        variant === "secondary" && "badge-secondary",
        className,
      )}
      color={appearance.color}
      size={props.size ?? "sm"}
      variant={appearance.variant}
    />
  );
}

type CardNativeProps = React.ComponentPropsWithoutRef<"div">;

export type CardProps = CardNativeProps &
  Omit<MantineCardProps, keyof CardNativeProps | "p"> & {
  bodyStyle?: React.CSSProperties;
  loading?: boolean;
};

export function Card({
  bodyStyle,
  loading,
  style,
  "aria-busy": ariaBusy,
  ...props
}: CardProps) {
  return (
    <MantineCard
      {...props}
      aria-busy={loading ?? ariaBusy}
      style={{ ...bodyStyle, ...style }}
    />
  );
}

export const Collapse = MantineCollapse;

export interface InputProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> {
  error?: boolean;
  leftSection?: React.ReactNode;
  wrapperClassName?: string;
}

export const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ className, wrapperClassName, ...props }, ref) => (
    <MantineInput
      {...props}
      ref={ref}
      className={classNames("min-w-0 flex-1", wrapperClassName)}
      classNames={{
        input: className,
      }}
    />
  ),
);
Input.displayName = "Input";

export interface PasswordInputProps
  extends Omit<
    React.ComponentProps<typeof MantinePasswordInput>,
    "size"
  > {
  wrapperClassName?: string;
}

export const PasswordInput = React.forwardRef<
  HTMLInputElement,
  PasswordInputProps
>(({ className, classNames: customClassNames, wrapperClassName, ...props }, ref) => (
  <MantinePasswordInput
    {...props}
    ref={ref}
    className={classNames("min-w-0 flex-1", wrapperClassName)}
    classNames={
      typeof customClassNames === "function"
        ? customClassNames
        : {
            ...customClassNames,
            input: classNames(customClassNames?.input, className),
          }
    }
  />
));
PasswordInput.displayName = "PasswordInput";

export type SelectOption = {
  disabled?: boolean;
  label: React.ReactNode;
  value: string | number;
  [key: string]: unknown;
};

type RenderOption = SelectOption & {
  className?: string;
  focused?: boolean;
  onClick?: React.MouseEventHandler<HTMLDivElement>;
  onMouseEnter?: React.MouseEventHandler<HTMLDivElement>;
  selected?: boolean;
  style?: React.CSSProperties;
};

export interface SelectProps {
  "aria-label"?: string;
  "aria-labelledby"?: string;
  allowCreate?: boolean;
  className?: string;
  disabled?: boolean;
  dropdownClassName?: string;
  emptyContent?: React.ReactNode;
  filter?: boolean;
  getPopupContainer?: () => HTMLElement;
  id?: string;
  inputClassName?: string;
  leftSectionPointerEvents?: React.CSSProperties["pointerEvents"];
  leftSectionWidth?: number | string;
  onChange?: (value: string | number | null) => void;
  onCreate?: (option: SelectOption) => void;
  optionList?: SelectOption[];
  optionClassName?: string;
  placeholder?: string;
  prefix?: React.ReactNode;
  renderCreateItem?: (
    inputValue: string,
    focused: boolean,
    style?: React.CSSProperties,
  ) => React.ReactNode;
  renderOptionItem?: (option: RenderOption) => React.ReactNode;
  searchPosition?: "trigger";
  showClear?: boolean;
  sectionClassName?: string;
  value?: string | number;
  zIndex?: number;
}

function selectClassNames(
  dropdownClassName?: string,
  inputClassName?: string,
  optionClassName?: string,
  sectionClassName?: string,
) {
  return {
    dropdown: dropdownClassName,
    input: inputClassName,
    option: optionClassName,
    section: sectionClassName,
  };
}

export function Select({
  allowCreate: _allowCreate,
  className,
  dropdownClassName,
  emptyContent,
  filter = false,
  getPopupContainer,
  inputClassName,
  leftSectionPointerEvents,
  leftSectionWidth,
  onChange,
  onCreate: _onCreate,
  optionList = [],
  optionClassName,
  prefix,
  renderCreateItem: _renderCreateItem,
  renderOptionItem,
  searchPosition: _searchPosition,
  showClear = false,
  sectionClassName,
  value,
  zIndex,
  ...props
}: SelectProps) {
  const portalTarget = getPopupContainer?.();
  const data = optionList.map((option) => ({
    disabled: option.disabled,
    label: String(option.label),
    value: String(option.value),
  }));

  return (
    <MantineSelect
      {...props}
      allowDeselect={showClear}
      className={className}
      classNames={selectClassNames(
        dropdownClassName,
        inputClassName,
        optionClassName,
        sectionClassName,
      )}
      clearable={showClear}
      comboboxProps={{
        portalProps: portalTarget ? { target: portalTarget } : undefined,
        withinPortal: Boolean(portalTarget),
        zIndex,
      }}
      data={data}
      leftSection={prefix}
      leftSectionPointerEvents={leftSectionPointerEvents}
      leftSectionWidth={leftSectionWidth}
      nothingFoundMessage={emptyContent}
      onChange={(nextValue: string | null) => onChange?.(nextValue)}
      renderOption={renderOptionItem
        ? ({
            option,
            checked,
          }: {
            option: { label: string; value: string };
            checked?: boolean;
          }) => {
            const original = optionList.find(
              (candidate) => String(candidate.value) === String(option.value),
            );
            return renderOptionItem({
              ...(original ?? option),
              label: original?.label ?? option.label,
              selected: checked,
              focused: false,
              value: option.value,
            });
          }
        : undefined}
      searchable={filter}
      value={value == null ? null : String(value)}
    />
  );
}

export interface CheckboxProps
  extends Omit<
    React.ComponentProps<typeof MantineCheckbox>,
    "checked" | "defaultChecked" | "indeterminate" | "onChange"
  > {
  checked?: boolean | "indeterminate";
  defaultChecked?: boolean | "indeterminate";
  onCheckedChange?: (checked: boolean | "indeterminate") => void;
}

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
    <MantineCheckbox
      {...props}
      {...checkedProps}
      {...defaultCheckedProps}
      className={classNames("codey-checkbox", className)}
      indeterminate={
        checked === "indeterminate" || defaultChecked === "indeterminate"
      }
      onChange={(event: React.ChangeEvent<HTMLInputElement>) =>
        onCheckedChange?.(event.currentTarget.checked)}
    />
  );
}

export interface SwitchProps
  extends Omit<React.ComponentProps<typeof MantineSwitch>, "onChange"> {
  "aria-busy"?: React.AriaAttributes["aria-busy"];
  loading?: boolean;
  onCheckedChange?: (checked: boolean) => void;
}

export function Switch({
  "aria-busy": ariaBusy,
  className,
  disabled,
  loading = false,
  onCheckedChange,
  ...props
}: SwitchProps) {
  const busy = loading || ariaBusy === true || ariaBusy === "true";
  return (
    <MantineSwitch
      {...props}
      aria-busy={busy || undefined}
      className={className}
      disabled={disabled || busy}
      onChange={(event: React.ChangeEvent<HTMLInputElement>) =>
        onCheckedChange?.(event.currentTarget.checked)}
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
  const contextValue = React.useMemo(
    () => ({ open: currentOpen, setOpen }),
    [currentOpen, setOpen],
  );

  return (
    <DialogContext.Provider value={contextValue}>
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
  zIndex?: number;
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
  zIndex,
}: DialogContentProps) {
  const dialog = React.useContext(DialogContext);
  if (!dialog) throw new Error("DialogContent must be rendered inside Dialog");
  const generatedId = React.useId().replace(/:/g, "");
  const titleId = `codey-dialog-title-${generatedId}`;
  const descriptionId = `codey-dialog-description-${generatedId}`;
  const labelContextValue = React.useMemo(
    () => ({ descriptionId, titleId }),
    [descriptionId, titleId],
  );

  const handleCancel = () => {
    const dismissEvent = createDismissEvent();
    onEscapeKeyDown?.(dismissEvent);
    onPointerDownOutside?.(dismissEvent);
    if (!dismissEvent.defaultPrevented) dialog.setOpen(false);
  };

  return (
    <Modal
      aria-describedby={descriptionId}
      aria-labelledby={titleId}
      centered
      classNames={{
        body: "m-0 w-full overflow-visible",
        content: classNames(
          "max-w-[calc(100%_-_32px)] rounded-2xl border border-black/10 bg-white p-6 shadow-[0_24px_64px_rgba(0,0,0,0.18)] overscroll-contain",
          className,
        ),
        inner: "p-6",
      }}
      closeOnClickOutside
      closeOnEscape
      data-codey-dialog="true"
      onClose={handleCancel}
      opened={dialog.open}
      padding={0}
      portalProps={container ? { target: container } : undefined}
      size={512}
      withCloseButton={false}
      withinPortal={Boolean(container)}
      zIndex={zIndex}
    >
      <DialogLabelContext.Provider value={labelContextValue}>
        <Button
          aria-label="关闭"
          className="absolute! right-3 top-3 z-[1] h-8! w-8! p-0!"
          onClick={handleCancel}
          size="icon-sm"
          variant="ghost"
        >
          <IconX size={18} aria-hidden="true" />
        </Button>
        {children}
      </DialogLabelContext.Provider>
    </Modal>
  );
}

export function DialogHeader({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={classNames("grid gap-1.5 pr-9", className)} {...props} />;
}

export function DialogFooter({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={classNames(
        "mt-5 flex items-center justify-end gap-2",
        className,
      )}
      {...props}
    />
  );
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
      className={classNames(
        "m-0 text-[17px] font-[650] leading-[1.35] text-[#1d1d1f]",
        className,
      )}
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
      className={classNames(
        "m-0 text-xs leading-[1.55] text-[#6e6e73]",
        className,
      )}
      id={id ?? labels?.descriptionId}
    />
  );
});
DialogDescription.displayName = "DialogDescription";
