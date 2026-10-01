// ------------ Expand Up Panel ------------
// A button on the Home screen that opens a panel upwards from itself, like the playtime pill. Closes on Escape
// or a click outside.
import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { AnimatePresence, m } from "framer-motion";
import { ChevronUp, type LucideIcon } from "lucide-react";
import { useClickOutside } from "../../lib/useClickOutside";
import { useReducedMotionSafe } from "../../lib/motion";

export function ExpandUp({
  width,
  className,
  icon: Icon,
  label,
  triggerClass,
  trigger,
  children,
}: {
  width?: number | string;
  className?: string;
  icon?: LucideIcon;
  label?: string;
  triggerClass?: string;
  trigger?: ReactNode;
  children: ReactNode | ((close: () => void) => ReactNode);
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const contentId = useId();
  const [triggerH, setTriggerH] = useState(42);
  const reduce = useReducedMotionSafe();
  useClickOutside(ref, () => setOpen(false), open);

  useLayoutEffect(() => {
    if (btnRef.current) setTriggerH(btnRef.current.offsetHeight);
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        btnRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <div
      ref={ref}
      className={className ? `relative ${className}` : "relative"}
      style={{ width, height: triggerH }}
    >
      <div className="glass absolute bottom-0 left-0 right-0 flex flex-col overflow-hidden rounded-ui shadow-2xl">
        <AnimatePresence initial={false}>
          {open && (
            <m.div
              key="content"
              id={contentId}
              initial={{ height: 0, opacity: 0 }}
              animate={{ height: "auto", opacity: 1 }}
              exit={{ height: 0, opacity: 0 }}
              transition={{ duration: reduce ? 0 : 0.26, ease: [0.4, 0, 0.2, 1] }}
              className="overflow-hidden"
            >
              {typeof children === "function" ? children(() => setOpen(false)) : children}
              <div className="mx-[12px] h-px bg-white/[0.08]" />
            </m.div>
          )}
        </AnimatePresence>
        <button
          ref={btnRef}
          aria-expanded={open}
          aria-controls={open ? contentId : undefined}
          onClick={() => setOpen((o) => !o)}
          className={
            triggerClass ??
            (Icon
              ? "flex h-[46px] w-full items-center gap-[10px] px-[13px] text-white transition-colors hover:bg-white/[0.05]"
              : "flex h-[42px] w-full items-center justify-center gap-[9px] px-[16px] text-[13.5px] font-medium text-white transition-colors hover:bg-white/[0.05]")
          }
        >
          {Icon ? (
            <>
              <span className="grid h-[28px] w-[28px] shrink-0 place-items-center rounded-[8px] bg-white/[0.07] text-white/70">
                <Icon size={15} />
              </span>
              <span className="flex-1 truncate text-left text-[13px] font-medium">{label}</span>
              <m.span
                className="shrink-0 text-white/45"
                animate={{ rotate: open ? 180 : 0 }}
                transition={{ duration: reduce ? 0 : 0.26, ease: [0.4, 0, 0.2, 1] }}
              >
                <ChevronUp size={16} />
              </m.span>
            </>
          ) : (
            trigger
          )}
        </button>
      </div>
    </div>
  );
}
