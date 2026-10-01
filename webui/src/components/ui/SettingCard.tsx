// ------------ Setting Card ------------
// One row in a settings box: an icon, a title and short description on the left and the control (switch, button,
// dropdown) on the right. Can be dimmed, red for dangerous actions, or carry a badge.
import type { ReactNode } from "react";

interface SettingCardProps {
  title: string;
  description?: string;
  control?: ReactNode;
  icon?: ReactNode;
  disabled?: boolean;
  danger?: boolean;
  badge?: ReactNode;
  detail?: ReactNode;
}

export function SettingCard({
  title,
  description,
  control,
  icon,
  disabled = false,
  danger = false,
  badge,
  detail,
}: SettingCardProps) {
  return (
    <div
      inert={disabled}
      className={`flex items-center justify-between gap-4 px-5 py-4 transition-colors duration-150 ${disabled ? "pointer-events-none opacity-50" : ""}`}
    >
      <div className="flex min-w-0 items-center gap-3">
        {icon && (
          <span
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[8px] bg-white/[0.05]"
            style={{ color: danger ? "#fca5a5" : "rgba(255,255,255,0.7)" }}
          >
            {icon}
          </span>
        )}
        <div className="min-w-0 text-left">
          <div className="flex items-center gap-2">
            <h5 className="text-[14px] font-medium" style={{ color: danger ? "#fca5a5" : "#fff" }}>
              {title}
            </h5>
            {badge}
          </div>
          {description && (
            <p className="mt-1 max-w-[56ch] text-[12.5px] leading-snug text-white/55 wrap-anywhere">
              {description}
            </p>
          )}
          {detail}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-2">{control}</div>
    </div>
  );
}
