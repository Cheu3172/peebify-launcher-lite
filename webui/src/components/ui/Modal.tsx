// ------------ Modal ------------
// The dialog box shell: dims the window, keeps keyboard focus inside, closes on Escape or a click outside, and
// labels itself for screen readers.
import { useId, useLayoutEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, m } from "framer-motion";
import { useEscapeKey } from "../../lib/useClickOutside";
import { useFocusTrap } from "../../lib/useFocusTrap";

export function Modal({
  open,
  onClose,
  children,
  width = 430,
  labelledBy,
  describedBy,
  dismissOnBackdrop = true,
}: {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  width?: number;
  labelledBy?: string;
  describedBy?: string;
  dismissOnBackdrop?: boolean;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const autoTitleId = useId();

  useEscapeKey(onClose, open);
  useFocusTrap(panelRef, open);

  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (!open || labelledBy || !panel) return;
    const label = () => {
      const heading = panel.querySelector<HTMLElement>("h1, h2, h3");
      if (!heading) {
        panel.removeAttribute("aria-labelledby");
        return;
      }
      if (!heading.id) heading.id = autoTitleId;
      panel.setAttribute("aria-labelledby", heading.id);
    };
    label();
    const observer = new MutationObserver(label);
    observer.observe(panel, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, [open, labelledBy, autoTitleId]);

  return createPortal(
    <AnimatePresence>
      {open && (
        <div className="fixed inset-0 z-(--z-modal) grid place-items-center px-[24px]">
          <m.div
            aria-hidden
            className="absolute inset-0 bg-black/55"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.14 }}
            onMouseDown={dismissOnBackdrop ? onClose : undefined}
          />
          <m.div
            ref={panelRef}
            role="dialog"
            aria-modal="true"
            aria-labelledby={labelledBy}
            aria-describedby={describedBy}
            tabIndex={-1}
            className="glass relative max-h-[88vh] overflow-y-auto overscroll-contain rounded-ui shadow-2xl outline-none"
            style={{ width }}
            initial={{ scale: 0.96, y: 10 }}
            animate={{ scale: 1, y: 0 }}
            exit={{ scale: 0.97, y: 8 }}
            transition={{ duration: 0.18, ease: [0.4, 0, 0.2, 1] }}
          >
            <m.div
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              transition={{ duration: 0.18, ease: [0.4, 0, 0.2, 1] }}
            >
              {children}
            </m.div>
          </m.div>
        </div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
