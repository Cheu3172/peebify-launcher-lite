// ------------ Select ------------
// The dropdown used instead of the browser's own. Opens a styled list that flips upwards if needed, with arrow
// keys and type-to-jump.
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, m } from "framer-motion";
import { Check, ChevronDown } from "lucide-react";
import { useReducedMotionSafe } from "../../lib/motion";
import { useClickOutside } from "../../lib/useClickOutside";

const TYPEAHEAD_MS = 500;

export function Select({
  value,
  onChange,
  options,
  disabled = false,
  ariaLabel,
}: {
  value: string;
  onChange: (value: string) => void;
  options: { value: string; label: string }[];
  disabled?: boolean;
  ariaLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const [closing, setClosing] = useState(false);
  const [active, setActive] = useState(0);
  const reduce = useReducedMotionSafe();
  const listId = useId();
  const [layout, setLayout] = useState<{
    style: React.CSSProperties;
    flipUp: boolean;
    triggerH: number;
    maxH: number;
  } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const typeRef = useRef({ buf: "", at: 0 });
  const selected = options.find((o) => o.value === value);

  const openMenu = () => {
    setActive(Math.max(0, options.findIndex((o) => o.value === value)));
    setClosing(false);
    setOpen(true);
  };

  const close = () => {
    if (!open) return;
    setOpen(false);
    setClosing(true);
  };

  const dismissNow = () => {
    setOpen(false);
    setClosing(false);
  };

  const pick = (v: string) => {
    close();
    btnRef.current?.focus();
    if (v !== value) onChange(v);
  };

  useLayoutEffect(() => {
    if (!open) return;
    const el = btnRef.current!;
    const r = el.getBoundingClientRect();
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const cx = r.left + r.width / 2;
    const cy = r.top + r.height / 2;
    const top = cy - h / 2;
    const bottom = cy + h / 2;
    const estimated = Math.min(options.length * 33, 320) + 12;
    const below = window.innerHeight - bottom;
    const fitsBelow = below >= estimated + 8;
    const fitsAbove = top > estimated + 8;
    const flipUp = !fitsBelow && (fitsAbove || top > below);
    const available = flipUp ? top - 8 : below - 8;
    setLayout({
      flipUp,
      triggerH: h,
      maxH: Math.max(66, Math.min(320, available - 20)),
      style: {
        position: "fixed",
        right: window.innerWidth - (cx + w / 2),
        width: w,
        ...(flipUp
          ? { bottom: window.innerHeight - bottom }
          : { top }),
      },
    });
  }, [open, options.length]);

  useClickOutside([btnRef, menuRef], close, open);

  useEffect(() => {
    if (!open && !closing) return;
    const onScroll = (e: Event) => {
      if (menuRef.current?.contains(e.target as Node)) return;
      dismissNow();
    };
    const onResize = () => dismissNow();
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
    };
  }, [open, closing]);

  useEffect(() => {
    const list = listRef.current;
    const el = list?.children[active] as HTMLElement | undefined;
    if (!open || !list || !el) return;
    if (el.offsetTop < list.scrollTop) list.scrollTop = el.offsetTop;
    else if (el.offsetTop + el.offsetHeight > list.scrollTop + list.clientHeight) {
      list.scrollTop = el.offsetTop + el.offsetHeight - list.clientHeight;
    }
  }, [open, active, layout]);

  const typeahead = (key: string): boolean => {
    const now = Date.now();
    const t = typeRef.current;
    if (now - t.at > TYPEAHEAD_MS) t.buf = "";
    if (key === " " && t.buf === "") return false;
    t.buf += key.toLowerCase();
    t.at = now;
    const cycling = [...t.buf].every((c) => c === t.buf[0]);
    const needle = cycling ? t.buf[0] : t.buf;
    const from = open ? active : options.findIndex((o) => o.value === value);
    const start = cycling ? from + 1 : Math.max(0, from);
    for (let n = 0; n < options.length; n++) {
      const i = (start + n) % options.length;
      if (options[i].label.toLowerCase().startsWith(needle)) {
        if (!open) openMenu();
        setActive(i);
        return true;
      }
    }
    return true;
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key.length === 1 && !e.ctrlKey && !e.altKey && !e.metaKey && options.length > 0) {
      if (typeahead(e.key)) {
        e.preventDefault();
        return;
      }
    }
    if (!open) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        openMenu();
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(i + 1, options.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    } else if (e.key === "Home") {
      e.preventDefault();
      setActive(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setActive(options.length - 1);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      pick(options[active].value);
    } else if (e.key === "Tab") {
      close();
    }
  };

  const triggerRow = layout && (
    <div
      role="button"
      aria-hidden
      onClick={close}
      className="flex cursor-pointer select-none items-center gap-2 pl-[14px] pr-[11px] text-[13px] font-medium text-white transition-colors hover:bg-white/[0.05]"
      style={{ height: layout.triggerH - 2 }}
    >
      <span className="truncate">{selected?.label ?? value}</span>
      <ChevronDown
        size={15}
        className={`shrink-0 text-white/50 transition-transform duration-150 ${open ? "rotate-180" : ""}`}
      />
    </div>
  );

  const optionsPanel = (
    <div
      ref={listRef}
      className="relative overflow-y-auto py-[5px]"
      style={{ maxHeight: layout?.maxH ?? 320 }}
    >
      {options.map((o, i) => (
        <button
          key={o.value}
          id={`${listId}-${i}`}
          type="button"
          tabIndex={-1}
          role="option"
          aria-selected={o.value === value}
          onMouseEnter={() => setActive(i)}
          onClick={() => pick(o.value)}
          className={`flex w-full items-center justify-between gap-2 py-[7px] pl-[14px] pr-[11px] text-left text-[13px] font-medium transition-colors ${
            i === active ? "bg-white/10 text-white" : "text-white/80"
          }`}
        >
          <span className="truncate">{o.label}</span>
          {o.value === value && <Check size={14} className="shrink-0 text-(--accent-text)" />}
        </button>
      ))}
    </div>
  );

  const overlayVisible = (open || closing) && layout;

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        disabled={disabled}
        role="combobox"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-activedescendant={open ? `${listId}-${active}` : undefined}
        onClick={() => (open ? close() : openMenu())}
        onKeyDown={onKeyDown}
        className={`flex items-center gap-2 rounded-ui border border-white/15 bg-black/30 py-[8px] pl-[14px] pr-[11px] text-[13px] font-medium text-white transition duration-150 hover:bg-black/40 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50 ${
          overlayVisible ? "opacity-0" : ""
        }`}
      >
        <span className="truncate">{selected?.label ?? value}</span>
        <ChevronDown
          size={15}
          className={`shrink-0 text-white/50 transition-transform duration-150 ${open ? "rotate-180" : ""}`}
        />
      </button>

      {overlayVisible &&
        createPortal(
          <div
            ref={menuRef}
            id={listId}
            role="listbox"
            aria-label={ariaLabel}
            style={layout.style}
            className="z-(--z-tooltip) flex flex-col overflow-hidden rounded-ui border border-white/15 bg-[rgba(20,20,26,0.97)] shadow-2xl"
          >
            {!layout.flipUp && triggerRow}
            <AnimatePresence onExitComplete={() => setClosing(false)}>
              {open && (
                <m.div
                  key="content"
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: "auto", opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  transition={{ duration: reduce ? 0 : 0.26, ease: [0.4, 0, 0.2, 1] }}
                  className="overflow-hidden"
                >
                  {layout.flipUp ? (
                    <>
                      {optionsPanel}
                      <div className="mx-[12px] h-px bg-white/[0.08]" />
                    </>
                  ) : (
                    <>
                      <div className="mx-[12px] h-px bg-white/[0.08]" />
                      {optionsPanel}
                    </>
                  )}
                </m.div>
              )}
            </AnimatePresence>
            {layout.flipUp && triggerRow}
          </div>,
          document.body,
        )}
    </>
  );
}
