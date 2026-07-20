import * as React from "react";
import { Check } from "lucide-react";

import { cn } from "../../lib/utils";

export type MagicButtonVariant = "default" | "secondary" | "outline" | "ghost" | "destructive";
export type MagicButtonSize = "default" | "sm" | "lg" | "icon" | "icon-sm";

const buttonVariantClasses: Record<MagicButtonVariant, string> = {
  default: "magic-button-default",
  secondary: "magic-button-secondary",
  outline: "magic-button-outline",
  ghost: "magic-button-ghost",
  destructive: "magic-button-destructive",
};

const buttonSizeClasses: Record<MagicButtonSize, string> = {
  default: "magic-button-size-default",
  sm: "magic-button-size-sm",
  lg: "magic-button-size-lg",
  icon: "magic-button-size-icon",
  "icon-sm": "magic-button-size-icon-sm",
};

export interface MagicButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: MagicButtonVariant;
  size?: MagicButtonSize;
}

export const MagicButton = React.forwardRef<HTMLButtonElement, MagicButtonProps>(
  ({ className, variant = "default", size = "default", type = "button", children, ...props }, ref) => (
    <button
      ref={ref}
      type={type}
      className={cn(
        "magic-button",
        buttonVariantClasses[variant],
        buttonSizeClasses[size],
        className,
      )}
      {...props}
    >
      <span className="magic-button-sheen" aria-hidden="true" />
      <span className="magic-button-content">{children}</span>
    </button>
  ),
);
MagicButton.displayName = "MagicButton";

export type MagicBadgeVariant =
  | "default"
  | "secondary"
  | "destructive"
  | "outline"
  | "success"
  | "warning"
  | "info"
  | "violet";

export interface MagicBadgeProps extends React.HTMLAttributes<HTMLDivElement> {
  variant?: MagicBadgeVariant;
}

export function MagicBadge({
  className,
  variant = "default",
  ...props
}: MagicBadgeProps) {
  return (
    <div
      className={cn("magic-badge", `magic-badge-${variant}`, className)}
      {...props}
    />
  );
}

export const MagicInput = React.forwardRef<HTMLInputElement, React.ComponentProps<"input">>(
  ({ className, type, ...props }, ref) => (
    <input
      ref={ref}
      type={type}
      className={cn("magic-input", className)}
      {...props}
    />
  ),
);
MagicInput.displayName = "MagicInput";

export interface MagicSwitchProps
  extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "onChange"> {
  checked?: boolean;
  onCheckedChange?: (checked: boolean) => void;
}

export const MagicSwitch = React.forwardRef<HTMLButtonElement, MagicSwitchProps>(
  ({ className, checked = false, onCheckedChange, disabled, ...props }, ref) => (
    <button
      ref={ref}
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      className={cn("magic-switch", className)}
      onClick={() => onCheckedChange?.(!checked)}
      {...props}
    >
      <span className="magic-switch-thumb" aria-hidden="true" />
    </button>
  ),
);
MagicSwitch.displayName = "MagicSwitch";

export interface MagicCheckboxProps
  extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "onChange"> {
  checked?: boolean;
  onCheckedChange?: (checked: boolean) => void;
}

export const MagicCheckbox = React.forwardRef<HTMLButtonElement, MagicCheckboxProps>(
  ({ className, checked = false, onCheckedChange, disabled, ...props }, ref) => (
    <button
      ref={ref}
      type="button"
      role="checkbox"
      aria-checked={checked}
      disabled={disabled}
      className={cn("magic-checkbox", className)}
      onClick={() => onCheckedChange?.(!checked)}
      {...props}
    >
      <span className="magic-checkbox-box" aria-hidden="true">
        {checked && <Check size={13} strokeWidth={3} />}
      </span>
    </button>
  ),
);
MagicCheckbox.displayName = "MagicCheckbox";
