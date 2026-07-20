import * as React from "react";

import { cn } from "../../lib/utils";

export interface ShimmerButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  shimmerColor?: string;
  shimmerDuration?: string;
}

export const ShimmerButton = React.forwardRef<HTMLButtonElement, ShimmerButtonProps>(
  ({
    shimmerColor = "#ffffff",
    shimmerDuration = "3s",
    className,
    children,
    type = "button",
    ...props
  }, ref) => (
    <button
      ref={ref}
      type={type}
      className={cn("magic-shimmer-button", className)}
      style={{
        "--shimmer-color": shimmerColor,
        "--shimmer-speed": shimmerDuration,
      } as React.CSSProperties}
      {...props}
    >
      <span className="magic-shimmer-track" aria-hidden="true">
        <span className="magic-shimmer-spark" />
      </span>
      <span className="magic-shimmer-content">{children}</span>
    </button>
  ),
);
ShimmerButton.displayName = "ShimmerButton";
