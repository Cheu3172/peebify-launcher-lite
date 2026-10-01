// ------------ Overlay UI Kit ------------
// The small building blocks of the overlay look (toggle, segmented control, chips, cards, bars, buttons),
// kept separate from the main launcher components because the overlay is styled differently.
import { useCallback, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { handleRadioGroupKey } from "../lib/radioGroup";

export function OvToggle({
  checked,
  onChange,
  disabled = false,
  ariaLabel,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`flex h-[26px] w-[46px] shrink-0 items-center rounded-[8px] border bg-black/30 p-[3px] transition-[border-color,opacity] duration-150 ${
        checked ? "justify-end border-white/30" : "justify-start border-white/15"
      } ${disabled ? "cursor-not-allowed opacity-45" : "cursor-pointer hover:border-white/40"}`}
    >
      <span
        className={`h-[18px] w-[20px] rounded-[6px] transition-colors duration-150 ${
          checked ? "bg-white" : "bg-white/[0.28]"
        }`}
      />
    </button>
  );
}

export function OvSeg<T extends string>({
  options,
  value,
  onChange,
  className = "",
  ariaLabel,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  className?: string;
  ariaLabel?: string;
}) {
  const activeIndex = options.findIndex((o) => o.value === value);
  const focusIndex = activeIndex < 0 ? 0 : activeIndex;
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const next = handleRadioGroupKey(e, focusIndex, options.length);
    if (next !== null && options[next].value !== value) onChange(options[next].value);
  };
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      onKeyDown={onKeyDown}
      className={`flex min-w-0 gap-[4px] rounded-[8px] border border-white/15 bg-black/30 p-[3px] ${className}`}
    >
      {options.map((o, i) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={i === focusIndex ? 0 : -1}
            onClick={() => onChange(o.value)}
            className={`min-w-0 flex-auto truncate rounded-[6px] px-[10px] py-[7px] text-center text-[12px] transition-colors duration-150 ${
              on
                ? "bg-white/10 font-semibold text-white shadow-[inset_0_0_0_1px_rgba(255,255,255,0.2)]"
                : "font-medium text-white/70 hover:bg-white/[0.06] hover:text-white"
            }`}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

export function OvChip({
  active,
  onClick,
  disabled = false,
  title,
  children,
}: {
  active: boolean;
  onClick: () => void;
  disabled?: boolean;
  title?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-pressed={active}
      disabled={disabled}
      onClick={onClick}
      className={`rounded-[8px] border px-3 py-[7px] text-[12px] font-medium transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-30 ${
        active
          ? "border-white/[0.22] bg-white/10 text-white"
          : "border-white/10 bg-white/[0.04] text-white/60 hover:text-white/85"
      }`}
    >
      {children}
    </button>
  );
}

export function OvCard({
  className = "",
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      className={`rounded-[10px] border border-white/[0.08] bg-white/[0.045] ${className}`}
    >
      {children}
    </div>
  );
}

export function OvList({ children }: { children: ReactNode }) {
  return (
    <div className="overflow-hidden rounded-[10px] border border-white/[0.08] bg-white/[0.045] [&>*:last-child]:border-b-0">
      {children}
    </div>
  );
}

export function OvRow({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-white/[0.055] px-4 py-[13px]">
      <div className="min-w-0">
        <div className="text-[12.5px] font-medium">{title}</div>
        {description && (
          <div className="mt-[3px] max-w-[56ch] text-[11.5px] leading-[1.45] text-white/55">
            {description}
          </div>
        )}
      </div>
      {children}
    </div>
  );
}

export function OvLabel({ children }: { children: ReactNode }) {
  return (
    <div className="text-[11px] uppercase tracking-[0.08em] text-white/55">{children}</div>
  );
}

export function OvBar({ percent }: { percent: number }) {
  return (
    <div className="h-[6px] flex-1 overflow-hidden rounded-[3px] bg-white/[0.08]">
      <div
        className="h-full rounded-[3px] bg-white/85"
        style={{ width: `${Math.max(0, Math.min(100, percent))}%` }}
      />
    </div>
  );
}

export function OvMeter({
  min,
  max,
  step,
  value,
  onChange,
  disabled = false,
  ariaLabel,
  onDraft,
}: {
  min: number;
  max: number;
  step: number;
  value: number;
  onChange: (value: number) => void;
  disabled?: boolean;
  ariaLabel?: string;
  onDraft?: (value: number | null) => void;
}) {
  const track = useRef<HTMLDivElement>(null);
  const draftRef = useRef<number | null>(null);
  const [draft, setDraft] = useState<number | null>(null);

  const updateDraft = useCallback(
    (next: number | null) => {
      draftRef.current = next;
      setDraft(next);
      onDraft?.(next);
    },
    [onDraft],
  );

  const apply = useCallback(
    (clientX: number) => {
      const rect = track.current?.getBoundingClientRect();
      if (!rect || rect.width === 0) return;
      const ratio = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
      const raw = min + ratio * (max - min);
      const next = Math.min(max, Math.max(min, Math.round(raw / step) * step));
      if (next !== draftRef.current) updateDraft(next);
    },
    [min, max, step, updateDraft],
  );

  const finish = () => {
    const next = draftRef.current;
    if (next === null) return;
    updateDraft(null);
    if (next !== value) onChange(next);
  };

  const shown = draft ?? value;
  const percent = ((shown - min) / (max - min)) * 100;

  return (
    <div
      ref={track}
      role="slider"
      tabIndex={disabled ? -1 : 0}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={shown}
      aria-valuetext={String(shown)}
      aria-label={ariaLabel}
      aria-disabled={disabled || undefined}
      onPointerDown={(e) => {
        if (disabled) return;
        e.currentTarget.setPointerCapture(e.pointerId);
        apply(e.clientX);
      }}
      onPointerMove={(e) => {
        if (disabled || !e.currentTarget.hasPointerCapture(e.pointerId)) return;
        apply(e.clientX);
      }}
      onPointerUp={finish}
      onLostPointerCapture={finish}
      onKeyDown={(e) => {
        if (disabled || draftRef.current !== null) return;
        let next: number;
        if (e.key === "ArrowLeft" || e.key === "ArrowDown") next = value - step;
        else if (e.key === "ArrowRight" || e.key === "ArrowUp") next = value + step;
        else if (e.key === "PageDown") next = value - step * 5;
        else if (e.key === "PageUp") next = value + step * 5;
        else if (e.key === "Home") next = min;
        else if (e.key === "End") next = max;
        else return;
        e.preventDefault();
        next = Math.min(max, Math.max(min, next));
        if (next !== value) onChange(next);
      }}
      className={`-my-2 py-2 ${disabled ? "cursor-not-allowed opacity-40" : "cursor-pointer"}`}
    >
      <div className="h-[4px] rounded-[2px] bg-white/[0.12]">
        <div className="h-[4px] rounded-[2px] bg-white" style={{ width: `${percent}%` }} />
      </div>
    </div>
  );
}

export function OvKey({ children }: { children: ReactNode }) {
  return (
    <span className="ov-mono rounded-[5px] bg-white/[0.09] px-[7px] py-[3px] text-[10.5px] text-white/[0.72]">
      {children}
    </span>
  );
}

export function OvButton({
  onClick,
  disabled = false,
  tone = "normal",
  children,
}: {
  onClick: () => void;
  disabled?: boolean;
  tone?: "normal" | "danger" | "dashed";
  children: ReactNode;
}) {
  const skin =
    tone === "danger"
      ? "border-[#ef4444]/40 bg-[#ef4444]/[0.08] text-[#fca5a5] hover:bg-[#ef4444]/[0.14]"
      : tone === "dashed"
        ? "border-dashed border-white/20 bg-transparent text-white/65 hover:text-white"
        : "border-white/15 bg-white/[0.06] text-white hover:bg-white/10";
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={`shrink-0 rounded-[8px] border px-3 py-[7px] text-[12px] font-medium transition-colors duration-150 disabled:cursor-not-allowed disabled:border-white/10 disabled:bg-white/[0.04] disabled:text-white/30 ${skin}`}
    >
      {children}
    </button>
  );
}

export function OvThumb({
  src,
  size = 46,
}: {
  src?: string | null;
  size?: number;
}) {
  return src ? (
    <img
      src={src}
      alt=""
      loading="lazy"
      referrerPolicy="no-referrer"
      style={{ width: size, height: size }}
      className="shrink-0 rounded-[8px] border border-white/10 object-cover"
    />
  ) : (
    <span
      style={{
        width: size,
        height: size,
        backgroundImage:
          "repeating-linear-gradient(135deg,rgba(255,255,255,0.09) 0 5px,rgba(255,255,255,0.02) 5px 10px)",
      }}
      className="shrink-0 rounded-[8px] border border-white/10"
    />
  );
}
