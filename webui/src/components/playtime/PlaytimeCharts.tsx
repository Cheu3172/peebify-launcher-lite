// ------------ Playtime Charts ------------
// The two small charts on the Playtime page: the day-by-day bars for recent play and the bar showing each game's
// share of the total. Hovering shows the exact times.
import { useState } from "react";
import type { ReactNode } from "react";
import { type ShareSlice } from "../../data/playtime";
import { useGameColor } from "../../store/customizationStore";
import { gameById } from "../../data/games";
import type { GameId } from "../../types/game";
import { fmtHM } from "../../lib/format";
import {
  fmtDayTip,
  type GameFilter,
  type MiniDay,
} from "../../lib/playtimeStats";
import { TooltipPortal } from "../ui/Tooltip";

type Tip = { x: number; y: number; body: ReactNode };

function useTip() {
  const [tip, setTip] = useState<Tip | null>(null);
  const bind = (body: ReactNode) => ({
    onMouseEnter: (e: React.MouseEvent) => setTip({ x: e.clientX, y: e.clientY, body }),
    onMouseMove: (e: React.MouseEvent) =>
      setTip((t) => (t ? { ...t, x: e.clientX, y: e.clientY } : t)),
    onMouseLeave: () => setTip(null),
  });
  const portal = tip ? (
    <TooltipPortal x={tip.x} y={tip.y} placement="cursor">
      {tip.body}
    </TooltipPortal>
  ) : null;
  return { bind, portal };
}

function TipRow({ id, minutes }: { id: string; minutes: number }) {
  const color = useGameColor();
  return (
    <span className="flex items-center gap-[7px]">
      <span
        className="inline-block h-[2px] w-[10px] rounded-full"
        style={{ background: color(id) }}
      />
      <span className="font-semibold">{fmtHM(minutes)}</span>
      <span className="font-normal text-white/60">{gameById(id as GameId).name}</span>
    </span>
  );
}

function DayTip({ day }: { day: MiniDay }) {
  const rows = Object.entries(day.byGame).sort((a, b) => b[1] - a[1]);
  return (
    <span className="flex flex-col gap-[3px]">
      <span className="text-white/60">
        {fmtDayTip(day.date)} ·{" "}
        <span className="font-semibold text-white">
          {day.minutes > 0 ? fmtHM(day.minutes) : "No playtime"}
        </span>
      </span>
      {rows.map(([id, min]) => (
        <TipRow key={id} id={id} minutes={min} />
      ))}
    </span>
  );
}

function dayLabel(day: MiniDay): string {
  if (day.minutes <= 0) return `${fmtDayTip(day.date)}: no playtime`;
  const rows = Object.entries(day.byGame)
    .filter(([, min]) => min > 0)
    .sort((a, b) => b[1] - a[1]);
  const split =
    rows.length > 1
      ? ` (${rows.map(([id, min]) => `${gameById(id as GameId).name} ${fmtHM(min)}`).join(", ")})`
      : rows.length === 1
        ? ` of ${gameById(rows[0][0] as GameId).name}`
        : "";
  return `${fmtDayTip(day.date)}: ${fmtHM(day.minutes)}${split}`;
}

const BAR_AREA = 92;
const SEG_MIN = 3;
const SEG_GAP = 1;

export function MiniBars({ days }: { days: MiniDay[] }) {
  const { bind, portal } = useTip();
  const color = useGameColor();
  const [hover, setHover] = useState<string | null>(null);
  const max = Math.max(...days.map((d) => d.minutes), 1);
  const first = days[0];

  return (
    <>
      <div
        role="group"
        aria-label={`Playtime, last ${days.length} days`}
        className="flex h-[92px] items-end justify-between gap-[4px]"
      >
        {days.map((day) => {
          const stack = Object.entries(day.byGame)
            .filter(([, min]) => min > 0)
            .sort((a, b) => b[1] - a[1]);
          const needed = stack.length * SEG_MIN + (stack.length - 1) * SEG_GAP;
          const height = Math.min(
            BAR_AREA,
            Math.max(needed, 5, (day.minutes / max) * BAR_AREA),
          );
          const dim = hover !== null && hover !== day.key;
          return (
            <div
              key={day.key}
              role="img"
              aria-label={dayLabel(day)}
              className="flex h-full min-w-0 max-w-[30px] flex-1 cursor-default items-end"
              onMouseOver={() => setHover(day.key)}
              onMouseOut={() => setHover((h) => (h === day.key ? null : h))}
              {...bind(<DayTip day={day} />)}
            >
              {stack.length > 0 ? (
                <div
                  className="flex w-full flex-col-reverse gap-[1px] overflow-hidden rounded-[3px] transition-opacity duration-150"
                  style={{ height: `${Math.round(height)}px`, opacity: dim ? 0.45 : 1 }}
                >
                  {stack.map(([id, min]) => (
                    <span
                      key={id}
                      className="block w-full"
                      style={{
                        flex: `${min} 1 0`,
                        minHeight: `${SEG_MIN}px`,
                        background: color(id),
                      }}
                    />
                  ))}
                </div>
              ) : (
                <div className="h-[2px] w-full rounded-full bg-white/[0.09]" />
              )}
            </div>
          );
        })}
      </div>
      <div className="mt-[9px] flex items-center justify-between text-[11px] text-white/55">
        <span>
          {first?.date.toLocaleDateString(undefined, { day: "numeric", month: "short" }) ?? ""}
        </span>
        <span>Today</span>
      </div>
      {portal}
    </>
  );
}

export function ShareBar({
  totals,
  filter,
  onPick,
}: {
  totals: ShareSlice[];
  filter: GameFilter;
  onPick: (id: GameFilter) => void;
}) {
  const { bind, portal } = useTip();
  const color = useGameColor();
  const sum = totals.reduce((a, t) => a + t.minutes, 0);
  if (sum <= 0) return <div className="h-[9px] w-full rounded-full bg-white/[0.07]" />;

  return (
    <>
      <div className="flex h-[9px] w-full overflow-hidden rounded-full bg-white/[0.07]">
        {totals.map((t) => {
          const pct = (t.minutes / sum) * 100;
          const dim = filter !== "all" && filter !== t.id;
          return (
            <button
              key={t.id}
              onClick={() => onPick(filter === t.id ? "all" : t.id)}
              aria-pressed={filter === t.id}
              aria-label={`${t.name} · ${Math.round(pct)}% of playtime`}
              className="h-full min-w-[3px] transition-opacity duration-150"
              style={{
                width: `${pct}%`,
                background: color(t.id),
                opacity: dim ? 0.3 : 1,
              }}
              {...bind(
                <span className="flex items-center gap-[7px]">
                  <TipRow id={t.id} minutes={t.minutes} />
                  <span className="text-white/45">{Math.round(pct)}%</span>
                </span>,
              )}
            />
          );
        })}
      </div>
      {portal}
    </>
  );
}
