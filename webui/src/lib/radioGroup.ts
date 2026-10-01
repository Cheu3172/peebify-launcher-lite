// ------------ Radio Group Keys ------------
// Arrow, Home and End key handling for groups of radio-style buttons like the segmented controls.
import type { KeyboardEvent } from "react";

export function radioGroupKeyTarget(key: string, index: number, count: number): number | null {
  if (count <= 0) return null;
  switch (key) {
    case "ArrowLeft":
    case "ArrowUp":
      return (index - 1 + count) % count;
    case "ArrowRight":
    case "ArrowDown":
      return (index + 1) % count;
    case "Home":
      return 0;
    case "End":
      return count - 1;
    default:
      return null;
  }
}

export function handleRadioGroupKey(
  e: KeyboardEvent<HTMLElement>,
  index: number,
  count: number,
): number | null {
  const next = radioGroupKeyTarget(e.key, index, count);
  if (next === null) return null;
  e.preventDefault();
  e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]')[next]?.focus();
  return next;
}
