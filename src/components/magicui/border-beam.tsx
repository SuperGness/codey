import type { CSSProperties } from "react";
import { motion, type MotionStyle, type Transition } from "motion/react";

import { cn } from "../../lib/utils";

export interface BorderBeamProps {
  size?: number;
  duration?: number;
  delay?: number;
  colorFrom?: string;
  colorTo?: string;
  transition?: Transition;
  className?: string;
  style?: CSSProperties;
  reverse?: boolean;
  initialOffset?: number;
  borderWidth?: number;
}

export function BorderBeam({
  className,
  size = 60,
  delay = 0,
  duration = 8,
  colorFrom = "#7c3aed",
  colorTo = "#38bdf8",
  transition,
  style,
  reverse = false,
  initialOffset = 0,
  borderWidth = 1,
}: BorderBeamProps) {
  return (
    <div
      className="magic-border-beam pointer-events-none absolute inset-0 rounded-[inherit]"
      style={{ "--beam-width": `${borderWidth}px` } as CSSProperties}
      aria-hidden="true"
    >
      <motion.div
        className={cn("magic-border-beam-light absolute aspect-square", className)}
        style={{
          width: size,
          offsetPath: `rect(0 auto auto 0 round ${size}px)`,
          "--beam-from": colorFrom,
          "--beam-to": colorTo,
          ...style,
        } as MotionStyle}
        initial={{ offsetDistance: `${initialOffset}%` }}
        animate={{
          offsetDistance: reverse
            ? [`${100 - initialOffset}%`, `${-initialOffset}%`]
            : [`${initialOffset}%`, `${100 + initialOffset}%`],
        }}
        transition={{
          repeat: Infinity,
          ease: "linear",
          duration,
          delay: -delay,
          ...transition,
        }}
      />
    </div>
  );
}
