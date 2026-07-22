import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "ui-badge inline-flex items-center gap-1 rounded-full border font-medium",
  {
    variants: {
      variant: {
        default: "ui-badge-default",
        secondary: "ui-badge-secondary",
        destructive: "ui-badge-destructive",
        outline: "ui-badge-outline",
        success: "ui-badge-success",
        warning: "ui-badge-warning",
        info: "ui-badge-info",
        brand: "ui-badge-brand",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLDivElement>, VariantProps<typeof badgeVariants> {}

function Badge({ className, variant, ...props }: BadgeProps) {
  return <div className={cn(badgeVariants({ variant }), className)} {...props} />;
}

export { Badge, badgeVariants };
