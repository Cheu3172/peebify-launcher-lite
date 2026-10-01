// ------------ Speed Graph ------------
// The little live line graph on the active download card, showing the last minute of download speed along with
// the current and peak speed.
import { useId } from "react";
import { fmtSpeed } from "../../lib/format";
import { SPEED_WINDOW } from "../../store/speedHistoryStore";

const VIEW_H = 32;

export function SpeedGraph({ samples }: { samples: number[] }) {
  const gradientId = `speed-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const peak = samples.reduce((a, b) => Math.max(a, b), 0);
  const current = samples.length > 0 ? samples[samples.length - 1] : 0;
  const scale = Math.max(peak, 1) * 1.15;
  const x = (i: number) => ((SPEED_WINDOW - samples.length + i) / (SPEED_WINDOW - 1)) * 100;
  const y = (v: number) => VIEW_H - (Math.max(0, v) / scale) * VIEW_H;

  const points = samples.map((v, i) => `${x(i).toFixed(2)},${y(v).toFixed(2)}`);
  const line = points.length > 1 ? `M${points.join(" L")}` : "";
  const area = line
    ? `${line} L${x(samples.length - 1).toFixed(2)},${VIEW_H} L${x(0).toFixed(2)},${VIEW_H} Z`
    : "";
  const lastX = samples.length > 0 ? x(samples.length - 1) : 0;
  const lastY = samples.length > 0 ? y(samples[samples.length - 1]) : VIEW_H;

  return (
    <div className="mt-4 rounded-ui border border-white/[0.07] bg-black/25 px-[13px] pb-[9px] pt-[9px]">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-[10.5px] font-semibold uppercase tracking-[0.09em] text-white/40">
          Speed · last 60s
        </span>
        <span className="flex items-baseline gap-[9px]">
          <span className="text-[13px] font-semibold tabular-nums text-white">
            {fmtSpeed(current) || "0 KB/s"}
          </span>
          {peak > 0 && (
            <span className="text-[11px] tabular-nums text-white/40">peak {fmtSpeed(peak)}</span>
          )}
        </span>
      </div>
      <div className="relative mt-[8px] h-[50px]">
        <svg
          viewBox={`0 0 100 ${VIEW_H}`}
          preserveAspectRatio="none"
          className="h-full w-full overflow-visible"
          aria-hidden
        >
          <defs>
            <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="rgba(255,255,255,0.20)" />
              <stop offset="100%" stopColor="rgba(255,255,255,0)" />
            </linearGradient>
          </defs>
          {area && <path d={area} fill={`url(#${gradientId})`} />}
          {line && (
            <path
              d={line}
              fill="none"
              stroke="rgba(255,255,255,0.85)"
              strokeWidth={2}
              strokeLinecap="round"
              strokeLinejoin="round"
              vectorEffect="non-scaling-stroke"
            />
          )}
        </svg>
        {samples.length > 1 && (
          <span
            className="pointer-events-none absolute h-[5px] w-[5px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-white"
            style={{ left: `${lastX}%`, top: `${(lastY / VIEW_H) * 100}%` }}
          />
        )}
      </div>
    </div>
  );
}
