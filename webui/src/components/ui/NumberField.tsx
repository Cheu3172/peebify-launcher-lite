// ------------ Number Field ------------
// A number box with a unit label. Typing is only applied when you leave the box or press Enter, values are
// clamped to the allowed range, and empty means "off" where that applies.
import { useEffect, useState } from "react";

export function NumberField({
  value,
  onChange,
  min,
  max,
  unit,
  zeroLabel,
  disabled = false,
  ariaLabel,
}: {
  value: string;
  onChange: (value: string) => void;
  min: number;
  max: number;
  unit: string;
  zeroLabel: string;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  const external = value === "0" || !/^\d+$/.test(value) ? "" : value;
  const [draft, setDraft] = useState(external);

  useEffect(() => {
    setDraft(external);
  }, [external]);

  const commit = () => {
    if (draft === "" && zeroLabel === "") {
      setDraft(external);
      return;
    }
    if (draft === "" || (zeroLabel !== "" && Number(draft) === 0)) {
      setDraft("");
      if (value !== "0") onChange("0");
      return;
    }
    const clamped = String(Math.min(Math.max(Number(draft), min), max));
    setDraft(clamped);
    if (clamped !== value) onChange(clamped);
  };

  return (
    <div
      className={`flex shrink-0 items-center gap-2 rounded-ui border border-white/15 bg-black/30 py-[8px] pl-[14px] pr-[11px] text-[13px] font-medium transition-colors focus-within:border-white/30 ${
        disabled ? "pointer-events-none opacity-50" : ""
      }`}
    >
      <input
        type="text"
        inputMode="numeric"
        spellCheck={false}
        aria-label={ariaLabel}
        disabled={disabled}
        value={draft}
        placeholder={zeroLabel}
        onChange={(e) => setDraft(e.target.value.replace(/\D/g, "").slice(0, 5))}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
        }}
        className="w-[76px] bg-transparent text-white outline-none placeholder:text-white/45"
      />
      <span className="shrink-0 text-white/45">{draft === "" ? "" : unit}</span>
    </div>
  );
}
