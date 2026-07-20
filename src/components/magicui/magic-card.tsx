import * as React from "react";
import {
  motion,
  useMotionTemplate,
  useMotionValue,
  useReducedMotion,
} from "motion/react";

import { cn } from "../../lib/utils";

export interface MagicCardProps extends Omit<
  React.HTMLAttributes<HTMLDivElement>,
  "onAnimationStart" | "onAnimationEnd" | "onDrag" | "onDragEnd" | "onDragStart"
> {
  gradientSize?: number;
  gradientColor?: string;
  gradientFrom?: string;
  gradientTo?: string;
}

export function MagicCard({
  children,
  className,
  gradientSize = 240,
  gradientColor = "rgba(124, 58, 237, 0.08)",
  gradientFrom = "#8b5cf6",
  gradientTo = "#38bdf8",
  ...props
}: MagicCardProps) {
  const mouseX = useMotionValue(-gradientSize);
  const mouseY = useMotionValue(-gradientSize);
  const reduceMotion = useReducedMotion();

  function handlePointerMove(event: React.PointerEvent<HTMLDivElement>) {
    if (reduceMotion) return;
    const bounds = event.currentTarget.getBoundingClientRect();
    mouseX.set(event.clientX - bounds.left);
    mouseY.set(event.clientY - bounds.top);
  }

  function resetPointer() {
    mouseX.set(-gradientSize);
    mouseY.set(-gradientSize);
  }

  const borderBackground = useMotionTemplate`
    linear-gradient(var(--card) 0 0) padding-box,
    radial-gradient(${gradientSize}px circle at ${mouseX}px ${mouseY}px,
      ${gradientFrom},
      ${gradientTo},
      var(--border) 100%
    ) border-box
  `;
  const glowBackground = useMotionTemplate`
    radial-gradient(${gradientSize}px circle at ${mouseX}px ${mouseY}px,
      ${gradientColor},
      transparent 100%
    )
  `;

  return (
    <motion.div
      className={cn("magic-card group relative isolate overflow-hidden rounded-2xl border border-transparent", className)}
      onPointerMove={handlePointerMove}
      onPointerLeave={resetPointer}
      style={{ background: reduceMotion ? "var(--card)" : borderBackground }}
      {...props}
    >
      <div className="magic-card-surface absolute inset-px z-0 rounded-[inherit]" aria-hidden="true" />
      {!reduceMotion && (
        <motion.div
          className="pointer-events-none absolute inset-px z-10 rounded-[inherit] opacity-0 transition-opacity duration-300 group-hover:opacity-100"
          style={{ background: glowBackground }}
          aria-hidden="true"
        />
      )}
      <div className="relative z-20 h-full">{children}</div>
    </motion.div>
  );
}
