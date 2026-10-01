// ------------ Settings Expander ------------
// A settings row that opens to reveal more settings underneath, with a one-line summary while closed.
import { useId, useState, type ReactNode } from "react";
import { AnimatePresence, m } from "framer-motion";
import { ChevronDown } from "lucide-react";
import { useReducedMotionSafe } from "../../lib/motion";

export function SettingsExpander({
  icon,
  title,
  summary,
  description,
  control,
  disabled = false,
  open: openProp,
  onOpenChange,
  children,
}: {
  icon: ReactNode;
  title: string;
  summary?: string;
  description?: string;
  control?: ReactNode;
  disabled?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  children: ReactNode;
}) {
  const [openState, setOpenState] = useState(false);
  const open = openProp ?? openState;
  const reduce = useReducedMotionSafe();
  const bodyId = useId();
  const shown = open && !disabled;
  const toggle = () => {
    if (openProp === undefined) setOpenState(!open);
    onOpenChange?.(!open);
  };
  const line = shown ? (description ?? summary) : summary;

  return (
    <div inert={disabled} className={disabled ? "pointer-events-none opacity-50" : ""}>
      <div className="flex items-center justify-between gap-4 px-5 py-4">
        <button
          type="button"
          onClick={toggle}
          aria-expanded={shown}
          aria-controls={bodyId}
          className="-mx-2 -my-1 flex min-w-0 flex-1 items-center gap-3 rounded-[8px] px-2 py-1 text-left transition-colors hover:bg-white/[0.04]"
        >
          <span
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[8px] bg-white/[0.05]"
            style={{ color: "rgba(255,255,255,0.7)" }}
          >
            {icon}
          </span>
          <span className="min-w-0">
            <span className="flex items-center gap-2">
              <span className="text-[14px] font-medium" style={{ color: "#fff" }}>
                {title}
              </span>
            </span>
            {line && (
              <span
                className={`mt-1 block max-w-[56ch] text-[12.5px] leading-snug text-white/55 ${shown ? "" : "truncate"}`}
              >
                {line}
              </span>
            )}
          </span>
        </button>
        <div className="flex shrink-0 items-center gap-2">
          {control}
          <button
            type="button"
            onClick={toggle}
            tabIndex={-1}
            aria-hidden="true"
            className="grid h-8 w-8 place-items-center rounded-[8px] text-white/50 transition hover:bg-white/[0.08] hover:text-white"
          >
            <ChevronDown
              size={16}
              className={`transition-transform duration-200 ${shown ? "rotate-180" : ""}`}
            />
          </button>
        </div>
      </div>
      <div id={bodyId}>
        <AnimatePresence initial={false}>
          {shown && (
            <m.div
              key="body"
              initial={{ height: 0, opacity: 0 }}
              animate={{ height: "auto", opacity: 1 }}
              exit={{ height: 0, opacity: 0 }}
              transition={{ duration: reduce ? 0 : 0.24, ease: [0.4, 0, 0.2, 1] }}
              className="overflow-hidden"
            >
              <div className="divide-y divide-white/[0.06] border-t border-white/[0.06] bg-black/[0.14]">
                {children}
              </div>
            </m.div>
          )}
        </AnimatePresence>
      </div>
    </div>
  );
}
