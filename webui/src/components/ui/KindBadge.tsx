// ------------ Kind Badge ------------
// The small label on a download saying what the job is: Install, Update, Repair, Verify, Move or Remove.
const LABELS: Record<string, string> = {
  install: "Install",
  download: "Install",
  update: "Update",
  repair: "Repair",
  verify: "Verify",
  move: "Move",
  uninstall: "Remove",
};

export function KindBadge({ kind, compact = false }: { kind: string; compact?: boolean }) {
  return (
    <span
      className={`flex shrink-0 items-center bg-white/10 font-semibold uppercase tracking-[0.08em] text-white/75 ${
        compact ? "rounded-[5px] px-[6px] py-[2px] text-[9px]" : "rounded-[6px] px-[7px] py-[3px] text-[10px]"
      }`}
    >
      {LABELS[kind] ?? LABELS.install}
    </span>
  );
}
