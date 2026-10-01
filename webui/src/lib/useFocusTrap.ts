// ------------ Focus Trap ------------
// Keeps Tab inside an open dialog or panel and hands focus back to whatever opened it when it closes.
import { useEffect, useRef, type RefObject } from "react";

const CANDIDATES =
  'a[href], area[href], button, input, select, textarea, video[controls], audio[controls], [contenteditable="true"], [tabindex]';

function isTabbable(el: HTMLElement): boolean {
  const media = !el.hasAttribute("tabindex") && el.matches("video[controls], audio[controls]");
  if (!media && el.tabIndex < 0) return false;
  if (el.matches(":disabled") || el.closest("[inert]")) return false;
  if (el.getClientRects().length === 0) return false;
  return getComputedStyle(el).visibility !== "hidden";
}

function tabbables(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(CANDIDATES)).filter(isTabbable);
}

const follows = (a: Node, b: Node) =>
  (b.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;

const traps: RefObject<HTMLElement | null>[] = [];

export function useFocusTrap(
  ref: RefObject<HTMLElement | null>,
  active = true,
  focusContainer = false,
): void {
  const opener = useRef<HTMLElement | null>(null);
  const wasActive = useRef(false);
  if (active !== wasActive.current) {
    wasActive.current = active;
    if (active) opener.current = document.activeElement as HTMLElement | null;
  }

  useEffect(() => {
    if (!active) return;
    const restore = opener.current;
    traps.push(ref);

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || traps[traps.length - 1] !== ref) return;
      const panel = ref.current;
      if (!panel) return;
      const els = tabbables(panel);
      if (els.length === 0) {
        e.preventDefault();
        panel.focus();
        return;
      }
      const first = els[0];
      const last = els[els.length - 1];
      const current = document.activeElement as HTMLElement | null;
      const inside = !!current && panel.contains(current);
      const atStart = current === panel || current === first || (!!current && follows(first, current));
      const atEnd = current === last || (!!current && follows(current, last));
      if (e.shiftKey && (!inside || atStart)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (!inside || atEnd)) {
        e.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKey);

    const t = window.setTimeout(() => {
      const panel = ref.current;
      if (!panel || panel.contains(document.activeElement)) return;
      const el = focusContainer ? null : (tabbables(panel)[0] ?? null);
      (el ?? panel).focus();
    }, 0);

    return () => {
      window.clearTimeout(t);
      window.removeEventListener("keydown", onKey);
      const at = traps.lastIndexOf(ref);
      if (at >= 0) traps.splice(at, 1);
      if (restore?.isConnected) restore.focus();
    };
  }, [ref, active, focusContainer]);
}
