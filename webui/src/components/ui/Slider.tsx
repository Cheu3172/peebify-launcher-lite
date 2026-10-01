// ------------ Slider ------------
// A slider with labelled tick marks and a live value. Drag or use the keyboard, and a dragged value is only
// saved when you let go.
import { useCallback, useRef, useState } from "react";
import { m } from "framer-motion";
import { springThumb, useReducedMotionSafe } from "../../lib/motion";

export function Slider({
  min,
  max,
  step,
  value,
  onChange,
  onDraft,
  unit,
  ticks,
  ariaLabel,
}: {
  min: number;
  max: number;
  step: number;
  value: number;
  onChange: (value: number) => void;
  onDraft?: (value: number | null) => void;
  unit: string;
  ticks: string[];
  ariaLabel?: string;
}) {
  const trackRef = useRef<HTMLDivElement>(null);
  const draftRef = useRef<number | null>(null);
  const [draft, setDraft] = useState<number | null>(null);
  const reduce = useReducedMotionSafe();

  const clamp = useCallback((v: number) => Math.min(max, Math.max(min, v)), [min, max]);
  const snap = useCallback(
    (v: number) => clamp(Math.round(clamp(v) / step) * step),
    [clamp, step],
  );

  const fromClientX = useCallback(
    (clientX: number) => {
      const rect = trackRef.current?.getBoundingClientRect();
      if (!rect || rect.width === 0) return draftRef.current ?? value;
      const fraction = (clientX - rect.left) / rect.width;
      return snap(min + fraction * (max - min));
    },
    [min, max, snap, value],
  );

  const updateDraft = (next: number | null) => {
    if (next === draftRef.current) return;
    draftRef.current = next;
    setDraft(next);
    onDraft?.(next);
  };

  const commit = (v: number) => {
    if (v !== value) onChange(v);
  };

  const finish = () => {
    const next = draftRef.current;
    if (next === null) return;
    updateDraft(null);
    commit(next);
  };

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    updateDraft(fromClientX(e.clientX));
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (draftRef.current === null || !e.currentTarget.hasPointerCapture(e.pointerId)) return;
    updateDraft(fromClientX(e.clientX));
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (draftRef.current !== null) return;
    if (e.key === "ArrowRight" || e.key === "ArrowUp") commit(snap(value + step));
    else if (e.key === "ArrowLeft" || e.key === "ArrowDown") commit(snap(value - step));
    else if (e.key === "PageUp") commit(snap(value + step * 5));
    else if (e.key === "PageDown") commit(snap(value - step * 5));
    else if (e.key === "Home") commit(min);
    else if (e.key === "End") commit(max);
    else return;
    e.preventDefault();
  };

  const shown = draft ?? value;
  const pct = max === min ? 0 : ((clamp(shown) - min) / (max - min)) * 100;
  const label = `${shown} ${unit}`;

  return (
    <div className="w-[220px]">
      <div className="mb-2 text-right text-[12.5px] font-medium tabular-nums text-white/85">{label}</div>
      <div
        ref={trackRef}
        role="slider"
        tabIndex={0}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={shown}
        aria-valuetext={label}
        aria-label={ariaLabel}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={finish}
        onLostPointerCapture={finish}
        onKeyDown={handleKeyDown}
        className="progress-track relative h-[6px] cursor-pointer touch-none rounded-full"
      >
        <div className="progress-fill absolute inset-y-0 left-0 rounded-full" style={{ width: `${pct}%` }} />
        <m.div
          className="absolute h-[16px] w-[16px] rounded-full border border-white/30 bg-white shadow-[0_1px_4px_rgba(0,0,0,0.4)]"
          style={{ left: `${pct}%`, top: "50%", x: "-50%", y: "-50%" }}
          whileTap={reduce ? undefined : { scale: 1.15 }}
          transition={springThumb}
        />
      </div>
      <div className="mt-2 flex justify-between text-[11px] text-white/55">
        {ticks.map((t) => (
          <span key={t}>{t}</span>
        ))}
      </div>
    </div>
  );
}
