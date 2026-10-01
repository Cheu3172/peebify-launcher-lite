// ------------ Playtime Pill ------------
// The Playtime pill under the Start button showing total time played. Opens upwards to show today, this week,
// this month, all time, average session and last played for the selected game.
import { Clock4 } from "lucide-react";
import { ExpandUp } from "./ExpandUp";
import { Hint } from "../ui/Tooltip";
import { useUiStore } from "../../store/uiStore";
import { useLivePlaytime, usePlaytimeStore } from "../../store/playtimeStore";
import { gameById } from "../../data/games";
import { fmtDateTime, fmtHM } from "../../lib/format";
import { rangeTotal } from "../../lib/playtimeStats";
import { useNow } from "../../lib/useRelativeTime";
import { useTimeFormat } from "../../store/settingsStore";

function fmtLastPlayed(ts: number, nowMs: number): string {
  if (Number.isNaN(ts)) return "Never";
  const mins = Math.floor((nowMs - ts) / 60000);
  if (mins < 1) return "Just now";
  if (mins < 60) return `${mins}m ago`;
  const today = new Date(nowMs);
  today.setHours(0, 0, 0, 0);
  const day = new Date(ts);
  day.setHours(0, 0, 0, 0);
  const days = Math.round((today.getTime() - day.getTime()) / 86_400_000);
  if (days <= 0) return `${Math.floor(mins / 60)}h ago`;
  if (days === 1) return "Yesterday";
  return new Date(ts).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(day.getFullYear() !== today.getFullYear() ? { year: "numeric" } : {}),
  });
}

export function PlaytimeTracker() {
  const now = useNow();
  const timeFormat = useTimeFormat();
  const activeId = useUiStore((s) => s.activeGameId);
  const game = gameById(activeId);
  const pill = usePlaytimeStore((s) => s.data.pill);
  const daily = usePlaytimeStore((s) => s.data.dashboard.daily);
  useLivePlaytime();

  const recent = {
    today: rangeTotal(daily, 1, activeId),
    week: rangeTotal(daily, 7, activeId),
    month: rangeTotal(daily, 30, activeId),
  };

  const p = pill[activeId];
  const allTime = p?.allTimeMinutes ?? 0;
  const sessions = p?.sessions ?? 0;
  const lastPlayed = p?.lastPlayed ? Date.parse(p.lastPlayed) : NaN;
  const rows: { label: string; value: string; title?: string }[] = [
    { label: "Today", value: fmtHM(recent.today) },
    { label: "This week", value: fmtHM(recent.week) },
    { label: "This month", value: fmtHM(recent.month) },
    { label: "All time", value: fmtHM(allTime) },
    { label: "Avg. session", value: fmtHM(sessions > 0 ? allTime / sessions : 0) },
    {
      label: "Last played",
      value: fmtLastPlayed(lastPlayed, now),
      title: Number.isNaN(lastPlayed) ? undefined : fmtDateTime(lastPlayed, timeFormat),
    },
  ];

  return (
    <ExpandUp
      width={200}
      triggerClass="flex h-[46px] w-full items-center gap-[8px] px-[16px] text-white transition-colors hover:bg-white/[0.05]"
      trigger={
        <>
          <Clock4 size={14} className="opacity-75" />
          <span className="text-[13px] font-medium text-white/70">Playtime</span>
          <span className="ml-auto whitespace-nowrap text-[14px] font-semibold">{fmtHM(allTime)}</span>
        </>
      }
    >
      <div className="px-[16px] pb-[12px] pt-[12px]">
        <div className="mb-[9px] text-[10px] font-medium uppercase tracking-[1px] text-white/40">
          Playtime · {game.name}
        </div>
        <div className="flex flex-col gap-[9px]">
          {rows.map((r, i) => (
            <div
              key={r.label}
              className={`flex items-center justify-between ${
                i === 3 ? "border-t border-white/[0.07] pt-[9px]" : ""
              }`}
            >
              <span className="text-[12px] text-white/55">{r.label}</span>
              <Hint tip={r.title} placement="left">
                <span className="whitespace-nowrap text-[13px] font-semibold">{r.value}</span>
              </Hint>
            </div>
          ))}
        </div>
      </div>
    </ExpandUp>
  );
}
