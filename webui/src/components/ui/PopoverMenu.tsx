// ------------ Popover Menu ------------
// A small menu that pops open next to a button and flips above it if there is no room below. Supports arrow keys
// and closes on Escape or a click outside.
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, m } from "framer-motion";
import { useClickOutside, useEscapeKey } from "../../lib/useClickOutside";
import { focusFirstMenuItem, handleMenuArrowKeys } from "../../lib/menu";

function focusAnchor(anchor: HTMLElement | null) {
  const target = anchor?.matches("button") ? anchor : anchor?.querySelector<HTMLElement>("button");
  target?.focus({ preventScroll: true });
}

export function PopoverMenu({
  open,
  onClose,
  anchorRef,
  width,
  estimatedHeight,
  children,
}: {
  open: boolean;
  onClose: () => void;
  anchorRef: RefObject<HTMLElement | null>;
  width: number;
  estimatedHeight: number;
  children: ReactNode;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [style, setStyle] = useState<CSSProperties>({});
  const [up, setUp] = useState(false);

  useClickOutside([anchorRef, menuRef], onClose, open);
  useEscapeKey(() => {
    onClose();
    focusAnchor(anchorRef.current);
  }, open);

  useEffect(() => {
    if (open) focusFirstMenuItem(menuRef.current);
  }, [open]);

  useLayoutEffect(() => {
    if (!open) return;
    const rect = anchorRef.current?.getBoundingClientRect();
    if (!rect) return;
    const flipUp = rect.bottom + estimatedHeight + 8 > window.innerHeight;
    setUp(flipUp);
    setStyle({
      position: "fixed",
      width,
      right: window.innerWidth - rect.right,
      ...(flipUp ? { bottom: window.innerHeight - rect.top + 6 } : { top: rect.bottom + 6 }),
    });
  }, [open, anchorRef, estimatedHeight, width]);

  useEffect(() => {
    if (!open) return;
    const close = (e: Event) => {
      if (menuRef.current?.contains(e.target as Node)) return;
      onClose();
    };
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [open, onClose]);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Tab") {
      e.preventDefault();
      onClose();
      focusAnchor(anchorRef.current);
      return;
    }
    handleMenuArrowKeys(e);
  };

  const onClickCapture = (e: MouseEvent<HTMLDivElement>) => {
    if (!(e.target as HTMLElement).closest('[role="menuitem"]')) return;
    onClose();
    focusAnchor(anchorRef.current);
  };

  return createPortal(
    <AnimatePresence>
      {open && (
        <m.div
          ref={menuRef}
          role="menu"
          onKeyDown={onKeyDown}
          onClickCapture={onClickCapture}
          initial={{ opacity: 0, y: up ? 4 : -4 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: up ? 4 : -4 }}
          transition={{ duration: 0.12 }}
          style={style}
          className="z-(--z-tooltip) overflow-hidden rounded-ui border border-white/15 bg-[rgba(20,20,26,0.97)] p-[5px] shadow-2xl"
        >
          {children}
        </m.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
