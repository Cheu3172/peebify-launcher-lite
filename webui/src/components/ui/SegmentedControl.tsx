// ------------ Segmented Control ------------
// A short row of options where exactly one is picked, with a sliding highlight. Works with the arrow keys.
import { useId, type KeyboardEvent } from "react";
import { m } from "framer-motion";
import { springThumb } from "../../lib/motion";
import { handleRadioGroupKey } from "../../lib/radioGroup";

interface Option {
  value: string;
  label: string;
}

export function SegmentedControl({
  options,
  value,
  onChange,
  ariaLabel,
  disabled = false,
}: {
  options: Option[];
  value: string;
  onChange: (value: string) => void;
  ariaLabel?: string;
  disabled?: boolean;
}) {
  const layoutId = useId();
  const activeIndex = options.findIndex((o) => o.value === value);
  const focusIndex = activeIndex < 0 ? 0 : activeIndex;

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (disabled) return;
    const next = handleRadioGroupKey(e, focusIndex, options.length);
    if (next !== null && options[next].value !== value) onChange(options[next].value);
  };

  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      aria-disabled={disabled || undefined}
      onKeyDown={onKeyDown}
      className={`flex gap-[3px] rounded-ui border border-white/15 bg-black/30 p-[3px] ${
        disabled ? "cursor-not-allowed opacity-50" : ""
      }`}
    >
      {options.map((o, i) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={active}
            tabIndex={i === focusIndex ? 0 : -1}
            disabled={disabled}
            onClick={() => onChange(o.value)}
            className={`relative rounded-[7px] px-[14px] py-[7px] text-[13px] font-medium leading-none transition-colors disabled:cursor-not-allowed ${
              active ? "text-white" : "text-white/70 enabled:hover:bg-white/[0.07] enabled:hover:text-white"
            }`}
          >
            {active && (
              <m.span
                layoutId={layoutId}
                className="absolute inset-0 rounded-[7px]"
                style={{
                  background: "rgba(255,255,255,0.1)",
                  border: "1px solid rgba(255,255,255,0.2)",
                }}
                transition={springThumb}
              />
            )}
            <span className="relative z-[1]">{o.label}</span>
          </button>
        );
      })}
    </div>
  );
}
