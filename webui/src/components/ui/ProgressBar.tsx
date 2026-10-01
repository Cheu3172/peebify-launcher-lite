// ------------ Progress Bar ------------
// The plain progress bar used for mod installs and similar. Always shows a sliver once something has started so
// it never looks empty.
const HEIGHTS = { sm: "h-[4px]", md: "h-[6px]" } as const;

export function ProgressBar({
  value,
  size = "md",
  ariaLabel,
  className = "",
}: {
  value: number;
  size?: keyof typeof HEIGHTS;
  ariaLabel?: string;
  className?: string;
}) {
  return (
    <div
      role="progressbar"
      aria-label={ariaLabel}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(value)}
      className={`progress-track relative ${HEIGHTS[size]} overflow-hidden rounded-full ${className}`}
    >
      <div
        className="progress-fill absolute inset-y-0 left-0 overflow-hidden rounded-full transition-[width] duration-150 ease-linear"
        style={{ width: `${value <= 0 ? 0 : Math.min(Math.max(value, 1.5), 100)}%` }}
      />
    </div>
  );
}
