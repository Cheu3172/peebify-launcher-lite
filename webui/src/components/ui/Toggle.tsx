// ------------ Toggle ------------
// The on/off switch used throughout the launcher.
import { m } from "framer-motion";
import { springThumb } from "../../lib/motion";

const THUMB_TRAVEL = 22;

export function Toggle({
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
      className={`flex h-[28px] w-[52px] shrink-0 items-center rounded-ui border bg-black/30 p-[3px] transition-[border-color,transform] duration-150 ${
        disabled
          ? "cursor-not-allowed border-white/15 opacity-50"
          : "cursor-pointer border-white/15 hover:border-white/30 active:scale-[0.97]"
      }`}
    >
      <m.span
        initial={false}
        animate={{ x: checked ? THUMB_TRAVEL : 0 }}
        transition={springThumb}
        className={`h-[20px] w-[22px] rounded-[7px] ${
          checked ? "bg-white" : "border border-white/20 bg-white/10"
        }`}
      />
    </button>
  );
}
