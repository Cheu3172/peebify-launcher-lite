// ------------ Motion Presets ------------
// Shared animation timings and page, fade and list transitions, so everything moves the same way.
// Respects the reduced motion setting.
import { useReducedMotion, type Transition, type Variants } from "framer-motion";

export const EASE_OUT_SOFT = [0.22, 1, 0.36, 1] as const;

export const springThumb: Transition = { type: "spring", stiffness: 500, damping: 35 };

export const pageVariants: Variants = {
  initial: { opacity: 0, y: 8 },
  animate: { opacity: 1, y: 0, transition: { duration: 0.2, ease: EASE_OUT_SOFT } },
  exit: { opacity: 0, transition: { duration: 0.2 } },
};

export const pageVariantsSolid: Variants = {
  initial: { opacity: 0, y: 8 },
  animate: {
    opacity: 1,
    y: 0,
    transition: { opacity: { duration: 0.05 }, y: { duration: 0.2, ease: EASE_OUT_SOFT } },
  },
  exit: { opacity: 0, transition: { duration: 0.2 } },
};

export const fadeVariants: Variants = {
  initial: { opacity: 0 },
  animate: { opacity: 1, transition: { duration: 0.12 } },
  exit: { opacity: 0, transition: { duration: 0.12 } },
};

export const listItemVariants: Variants = {
  initial: { opacity: 0, y: 6 },
  animate: { opacity: 1, y: 0, transition: { duration: 0.15, ease: EASE_OUT_SOFT } },
  exit: { opacity: 0, scale: 0.98, transition: { duration: 0.12 } },
};

export function useReducedMotionSafe(): boolean {
  return Boolean(useReducedMotion());
}
