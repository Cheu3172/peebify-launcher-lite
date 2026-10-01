// ------------ Settings Group ------------
// The boxes and headings that group settings together, including the red-tinted box for dangerous ones.
import type { ReactNode } from "react";

export function GroupBox({
  children,
  danger = false,
  className = "",
}: {
  children: ReactNode;
  danger?: boolean;
  className?: string;
}) {
  const tone = danger
    ? "border-[#ef4444]/[0.22] bg-[#ef4444]/[0.03]"
    : "border-white/[0.08] bg-white/[0.05]";
  return (
    <div
      className={`overflow-hidden rounded-ui border divide-y divide-white/[0.06] ${tone} ${className}`}
    >
      {children}
    </div>
  );
}

export function GroupLabel({
  label,
  hint,
  className = "",
}: {
  label: string;
  hint?: string;
  className?: string;
}) {
  return (
    <div className={`mb-[10px] flex items-center justify-between gap-3 px-1 ${className}`}>
      <h3 className="text-[12px] font-semibold uppercase tracking-[0.07em] text-white/55">
        {label}
        {hint && (
          <span className="ml-2 font-normal normal-case tracking-normal text-white/55">{hint}</span>
        )}
      </h3>
    </div>
  );
}

export function SettingsGroup({
  label,
  hint,
  danger = false,
  children,
}: {
  label?: string;
  hint?: string;
  danger?: boolean;
  children: ReactNode;
}) {
  return (
    <section>
      {label && <GroupLabel label={label} hint={hint} />}
      <GroupBox danger={danger}>{children}</GroupBox>
    </section>
  );
}
