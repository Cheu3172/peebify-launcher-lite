// ------------ Action Button ------------
// The standard rounded button used across pages, in neutral, danger (red) and accent styles, with an optional
// icon.
import type { ReactNode } from "react";

type ActionVariant = "neutral" | "danger" | "accent";

const VARIANTS: Record<ActionVariant, string> = {
  neutral:
    "border-white/15 bg-white/[0.05] text-white hover:border-white/25 hover:bg-white/[0.12]",
  danger:
    "border-[#ef4444]/40 bg-[#ef4444]/[0.08] text-[#fca5a5] hover:border-[#ef4444]/60 hover:bg-[#ef4444]/[0.16]",
  accent:
    "border-(--accent-b)/45 bg-(--accent-b)/12 text-(--accent-text) hover:border-(--accent-b)/60 hover:bg-(--accent-b)/22",
};

export function ActionButton({
  children,
  onClick,
  variant = "neutral",
  disabled = false,
  icon,
  type = "button",
}: {
  children: ReactNode;
  onClick?: () => void;
  variant?: ActionVariant;
  disabled?: boolean;
  icon?: ReactNode;
  type?: "button" | "submit";
}) {
  return (
    <button
      type={type}
      onClick={onClick}
      disabled={disabled}
      className={`inline-flex select-none items-center gap-2 rounded-ui border px-4 py-[9px] text-[13px] font-medium transition duration-150 active:scale-[0.97] disabled:cursor-not-allowed disabled:opacity-40 ${VARIANTS[variant]}`}
    >
      {icon}
      {children}
    </button>
  );
}
