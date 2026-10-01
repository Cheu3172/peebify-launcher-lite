// ------------ Click Outside and Escape ------------
// Hooks for closing popups when you click elsewhere or press Escape. When popups are stacked,
// Escape only closes the top one.
import { useCallback, useEffect, useRef, type RefObject } from "react";

type ElementRef = RefObject<HTMLElement | null>;

export function useClickOutside(
  ref: ElementRef | ElementRef[],
  onOutside: () => void,
  active = true,
  ignore?: (target: HTMLElement) => boolean,
): void {
  const latest = useRef({ ref, onOutside, ignore });
  latest.current = { ref, onOutside, ignore };

  useEffect(() => {
    if (!active) return;
    const handler = (e: MouseEvent) => {
      const { ref: current, onOutside: cb, ignore: skip } = latest.current;
      const refs = Array.isArray(current) ? current : [current];
      const target = e.target as HTMLElement;
      if (refs.some((r) => r.current?.contains(target))) return;
      if (skip?.(target)) return;
      cb();
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [active]);
}

type EscapeLayer = { current: () => void };

const escapeLayers: EscapeLayer[] = [];

function onEscapeKey(e: KeyboardEvent) {
  if (e.key !== "Escape") return;
  const top = escapeLayers[escapeLayers.length - 1];
  if (top) top.current();
}

export function useEscapeKey(onEscape: () => void, active = true): () => boolean {
  const latest = useRef(onEscape);
  latest.current = onEscape;

  useEffect(() => {
    if (!active) return;
    const layer = latest;
    escapeLayers.push(layer);
    if (escapeLayers.length === 1) window.addEventListener("keydown", onEscapeKey);
    return () => {
      const at = escapeLayers.lastIndexOf(layer);
      if (at >= 0) escapeLayers.splice(at, 1);
      if (escapeLayers.length === 0) window.removeEventListener("keydown", onEscapeKey);
    };
  }, [active]);

  return useCallback(() => escapeLayers[escapeLayers.length - 1] === latest, []);
}
