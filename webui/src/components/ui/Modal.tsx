// ------------ Modal ------------
// The dialog box shell: dims the window, keeps keyboard focus inside, closes on Escape or a click outside, and
// labels itself for screen readers.
import { useId, useLayoutEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, m } from "framer-motion";
import { useEscapeKey } from "../../lib/useClickOutside";
import { useFocusTrap } from "../../lib/useFocusTrap";

const ENTER = { duration: 0.18, ease: [0.4, 0, 0.2, 1] } as const;
const EXIT = { duration: 0.14, ease: [0.4, 0, 1, 1] } as const;

// The panel's frosted glass eases in and out by its blur, tint, border and shadow rather than by opacity.
// Fading a blurred element with opacity makes the browser draw the blur differently until the fade ends, so it
// visibly snaps into place right after the dialog opens.
const GLASS_ON = {
  backgroundColor: "rgba(18, 18, 22, 0.6)",
  backdropFilter: "blur(20px) saturate(180%)",
  borderColor: "rgba(255, 255, 255, 0.1)",
  boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0.25)",
};
const GLASS_OFF = {
  backgroundColor: "rgba(18, 18, 22, 0)",
  backdropFilter: "blur(0px) saturate(100%)",
  borderColor: "rgba(255, 255, 255, 0)",
  boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0)",
};

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
            exit={{ opacity: 0, transition: EXIT }}
            transition={ENTER}
            onMouseDown={dismissOnBackdrop ? onClose : undefined}
          />
          <m.div
            ref={panelRef}
            role="dialog"
            aria-modal="true"
            aria-labelledby={labelledBy}
            aria-describedby={describedBy}
            tabIndex={-1}
            className="relative max-h-[88vh] max-w-full overflow-y-auto overscroll-contain rounded-ui border outline-none"
            style={{ width }}
            initial={{ ...GLASS_OFF, scale: 0.96, y: 10 }}
            animate={{ ...GLASS_ON, scale: 1, y: 0 }}
            exit={{ ...GLASS_OFF, scale: 0.97, y: 8, transition: EXIT }}
            transition={ENTER}
          >
            <m.div
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0, transition: EXIT }}
              transition={ENTER}
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
