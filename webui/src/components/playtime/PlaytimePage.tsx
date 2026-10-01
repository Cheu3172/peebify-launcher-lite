// ------------ Playtime Page ------------
// The Playtime tab. Shows totals, streaks and averages for the week, month or all time next to a list of every
// play session grouped by day. Can be filtered to one game, and individual sessions can be removed.
import { useEffect, useMemo, useState } from "react";
import { m } from "framer-motion";
import { ChevronDown, ChevronUp, Flame, Trash2, X } from "lucide-react";
import { type GameTotal, type ShareSlice } from "../../data/playtime";
import { useCustomizationStore, useGameColor } from "../../store/customizationStore";
import { gameById, GAMES } from "../../data/games";
import type { GameId } from "../../types/game";
import {
  deletePlaytimeSession,
  getOlderSessions,
  type SessionEntry,
} from "../../lib/ipc";
import {
  buildMiniDays,
  computeStreaks,
  endDayOffset,
  fmtDayHeading,
  fmtHoursShort,
  fmtTimeRange,
  groupSessionsByDay,
  MINI_DAYS,
  pctChange,
  rangeCutoff,
  RANGE_DAYS,
  RANGE_LABEL,
  rangeTotal,
  sessionEnd,
  totalsByGame,
  usesHour12,
  type DayGroup,
  type GameFilter,
  type PlaytimeRange,
  type SessionSpan,
} from "../../lib/playtimeStats";
import { pageVariants } from "../../lib/motion";
import { fmtClockTime, fmtHM } from "../../lib/format";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import { useNow } from "../../lib/useRelativeTime";
import { useTimeFormat } from "../../store/settingsStore";
import { useLivePlaytime, usePlaytimeStore } from "../../store/playtimeStore";
import { useSessionUiStore } from "../../store/sessionUiStore";
import { SegmentedControl } from "../ui/SegmentedControl";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";
import { MiniBars, ShareBar } from "./PlaytimeCharts";

const RANGES: PlaytimeRange[] = ["Week", "Month", "All"];
const GROUPS_COLLAPSED = 6;
const GROUPS_STEP = 8;

type OlderSessions = {
  epoch: number;
  sessions: SessionSpan[];
  hasMore: boolean;
  removed: string[];
};

const fmtSessionClock = (min: number) => (min > 0 && min < 1 ? "<1m" : fmtHM(min));

const sessionKey = (s: SessionEntry) => `${s.gameId}:${s.start}`;

const oldestStart = (sessions: SessionEntry[]) =>
  sessions.reduce((a, s) => Math.min(a, s.start), Infinity);

export function PlaytimePage() {
  const range = useSessionUiStore((s) => s.playtimeRange);
  const setRange = useSessionUiStore((s) => s.setPlaytimeRange);
  const storedFilter = useSessionUiStore((s) => s.playtimeGame);
  const setFilter = useSessionUiStore((s) => s.setPlaytimeGame);
  const dashboard = usePlaytimeStore((s) => s.data.dashboard);
  const hydrated = usePlaytimeStore((s) => s.hydrated);
  const epoch = usePlaytimeStore((s) => s.epoch);
  const updatePlaytime = usePlaytimeStore((s) => s.update);
  const [older, setOlder] = useState<OlderSessions | null>(null);
  const [loadingOlder, setLoadingOlder] = useState(false);
  useNow();
  useLivePlaytime();

  const played = useMemo<GameTotal[]>(
    () =>
      dashboard.games
        .filter((t) => t.minutes > 0 && GAMES.some((g) => g.id === t.id))
        .map((t) => {
          const g = gameById(t.id as GameId);
          return {
            id: g.id,
            name: g.name,
            minutes: t.minutes,
            sessions: t.sessions,
            longestSessionMinutes: t.longestSessionMinutes,
          };
        })
        .sort((a, b) => b.minutes - a.minutes),
    [dashboard],
  );
  const filter: GameFilter =
    storedFilter !== "all" && !played.some((t) => t.id === storedFilter) ? "all" : storedFilter;

  const summary = (() => {
    const daily = dashboard.daily;
    const inFilter = filter === "all" ? played : played.filter((t) => t.id === filter);
    const allTime = inFilter.reduce((a, t) => a + t.minutes, 0);
    const sessions = inFilter.reduce((a, t) => a + t.sessions, 0);
    return {
      allTime,
      sessions,
      week: rangeTotal(daily, 7, filter),
      month: rangeTotal(daily, 30, filter),
      prevMonth: rangeTotal(daily, 30, filter, 30),
      avgSession: sessions > 0 ? allTime / sessions : 0,
      longestSession: inFilter.reduce((a, t) => Math.max(a, t.longestSessionMinutes), 0),
      miniDays: buildMiniDays(daily, MINI_DAYS, filter),
      streaks: computeStreaks(daily, filter),
    };
  })();

  const shareTotals: ShareSlice[] = (() => {
    const days = RANGE_DAYS[range];
    if (days <= 0) return played;
    const byGame = totalsByGame(dashboard.daily, days);
    return played
      .map((t) => ({ id: t.id, name: t.name, minutes: byGame[t.id] ?? 0 }))
      .filter((t) => t.minutes > 0)
      .sort((a, b) => b.minutes - a.minutes);
  })();

  const [shownDashboard, setShownDashboard] = useState(dashboard);
  if (shownDashboard !== dashboard) {
    setShownDashboard(dashboard);
    if (older?.epoch === epoch) {
      const floor = oldestStart(dashboard.recentSessions);
      const have = new Set(older.sessions.map(sessionKey));
      const carried = shownDashboard.recentSessions.filter(
        (s: SessionSpan) => !s.live && s.start < floor && !have.has(sessionKey(s)),
      );
      if (carried.length > 0) setOlder({ ...older, sessions: [...carried, ...older.sessions] });
    }
  }
  const olderLoaded =
    older?.epoch === epoch && dashboard.sessionsTruncated === true ? older : null;
  const sessions = useMemo<SessionSpan[]>(() => {
    const recent: SessionSpan[] = dashboard.recentSessions;
    if (!olderLoaded) return recent;
    const floor = oldestStart(recent);
    const seen = new Set<string>(olderLoaded.removed);
    const earlier = olderLoaded.sessions.filter((s) => {
      const key = sessionKey(s);
      if (s.start >= floor || seen.has(key)) return false;
      seen.add(key);
      return true;
    });
    return [...recent, ...earlier];
  }, [dashboard, olderLoaded]);
  const oldestLoaded = useMemo(() => oldestStart(sessions), [sessions]);
  const hasMore = olderLoaded ? olderLoaded.hasMore : dashboard.sessionsTruncated === true;
  const cutoff = rangeCutoff(range);
  const incomplete = hasMore && Number.isFinite(oldestLoaded) && oldestLoaded >= cutoff;

  const inRange = useMemo(
    () =>
      sessions.filter(
        (s) =>
          s.start >= cutoff &&
          (filter === "all" ? GAMES.some((g) => g.id === s.gameId) : s.gameId === filter),
      ),
    [sessions, filter, cutoff],
  );
  const groups = useMemo(() => groupSessionsByDay(inRange), [inRange]);
  const rangeMinutes = useMemo(() => inRange.reduce((a, s) => a + s.minutes, 0), [inRange]);

  const [shownGroups, setShownGroups] = useState(GROUPS_COLLAPSED);
  useEffect(() => setShownGroups(GROUPS_COLLAPSED), [range, filter]);

  const loadOlder = async () => {
    if (loadingOlder || !Number.isFinite(oldestLoaded)) return;
    const requestedFor = epoch;
    const loaded = sessions.slice(dashboard.recentSessions.length);
    setLoadingOlder(true);
    try {
      const page = await getOlderSessions(oldestLoaded);
      if (page && usePlaytimeStore.getState().epoch === requestedFor) {
        setOlder((o) => ({
          epoch: requestedFor,
          sessions: [...loaded, ...page.sessions],
          hasMore: page.hasMore,
          removed: o?.epoch === requestedFor ? o.removed : [],
        }));
      }
    } finally {
      setLoadingOlder(false);
    }
  };

  const removeSession = async (session: SessionEntry) => {
    if (!(await deletePlaytimeSession(session.gameId, session.start))) return;
    const key = sessionKey(session);
    setOlder((o) => o && { ...o, removed: [...o.removed, key] });
    await updatePlaytime();
  };

  const showEarlier = () => {
    setShownGroups((n) => n + GROUPS_STEP);
    if (incomplete && groups.length <= shownGroups + GROUPS_STEP) void loadOlder();
  };
  const earlierButton = (groups.length > shownGroups || incomplete) && (
    <MoreButton
      label={loadingOlder ? "Loading earlier sessions…" : "Show earlier sessions"}
      onClick={showEarlier}
    />
  );

  const sessionNoun = inRange.length === 1 ? "session" : "sessions";
  const countLabel = incomplete
    ? `Latest ${inRange.length} ${sessionNoun}`
    : `${inRange.length} ${sessionNoun}`;
  const rangeLabel =
    range !== "All" ? ` in the last ${RANGE_DAYS[range]} days` : incomplete ? "" : " recorded";

  return (
    <m.div variants={pageVariants} className="@container absolute inset-0">
      <div className="flex h-full flex-col overflow-y-auto @[820px]:flex-row @[820px]:overflow-hidden">
        <aside className="shrink-0 border-b border-white/[0.08] px-8 pb-7 pt-8 @[820px]:w-[326px] @[820px]:overflow-y-auto @[820px]:border-b-0 @[820px]:border-r">
          <div className="w-full max-w-[420px] @[820px]:max-w-none">
            {hydrated ? (
              <SummaryRail
                summary={summary}
                share={shareTotals}
                filter={filter}
                onPick={setFilter}
              />
            ) : (
              <RailSkeleton />
            )}
          </div>
        </aside>

        <section className="min-w-0 flex-1 px-8 pb-9 pt-8 @[820px]:overflow-y-auto @[820px]:px-9 @[820px]:pt-[30px]">
          <header className="mb-6 flex flex-wrap items-end justify-between gap-x-5 gap-y-3">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-[10px]">
                <h1 className="text-[24px] font-semibold leading-tight">Sessions</h1>
                {filter !== "all" && (
                  <FilterChip
                    total={played.find((t) => t.id === filter)}
                    onClear={() => setFilter("all")}
                  />
                )}
              </div>
              <p className="mt-[6px] text-[13px] text-white/45">
                {hydrated
                  ? `${countLabel} · ${fmtHM(rangeMinutes)}${rangeLabel}`
                  : "Loading your play history…"}
              </p>
            </div>
            <SegmentedControl
              ariaLabel="Playtime range"
              options={RANGES.map((r) => ({ value: r, label: RANGE_LABEL[r] }))}
              value={range}
              onChange={(v) => setRange(v as PlaytimeRange)}
            />
          </header>

          {!hydrated ? (
            <SessionsSkeleton />
          ) : groups.length === 0 ? (
            <>
              <EmptyState filtered={filter !== "all"} range={range} />
              {earlierButton}
            </>
          ) : (
            <>
              <div className="flex flex-col gap-[26px]">
                {groups.slice(0, shownGroups).map((g) => (
                  <DaySection
                    key={g.key}
                    group={g}
                    filter={filter}
                    onPick={setFilter}
                    onRemove={removeSession}
                  />
                ))}
              </div>
              {earlierButton ||
                (groups.length > GROUPS_COLLAPSED && (
                  <MoreButton
                    label="Show less"
                    onClick={() => setShownGroups(GROUPS_COLLAPSED)}
                  />
                ))}
            </>
          )}
        </section>
      </div>
    </m.div>
  );
}

type Summary = {
  allTime: number;
  sessions: number;
  week: number;
  month: number;
  prevMonth: number;
  avgSession: number;
  longestSession: number;
  miniDays: ReturnType<typeof buildMiniDays>;
  streaks: ReturnType<typeof computeStreaks>;
};

function SummaryRail({
  summary,
  share,
  filter,
  onPick,
}: {
  summary: Summary;
  share: ShareSlice[];
  filter: GameFilter;
  onPick: (id: GameFilter) => void;
}) {
  const color = useGameColor();
  const hours = Math.floor(summary.allTime / 60);
  const big = hours >= 1 ? hours : Math.round(summary.allTime);
  const unit = hours >= 1 ? (hours === 1 ? "hour" : "hours") : "minutes";
  const delta = pctChange(summary.month, summary.prevMonth);
  const { current, longest } = summary.streaks;

  return (
    <>
      <RailLabel>{filter === "all" ? "All time" : gameById(filter as GameId).name}</RailLabel>
      <div className="mt-[10px] flex items-baseline gap-[10px]">
        <span className="font-display text-[56px] font-bold leading-none tracking-[-0.02em]">
          {big.toLocaleString()}
        </span>
        <span className="text-[17px] font-medium text-white/45">{unit}</span>
      </div>
      {delta !== null && (
        <div className="mt-[14px] flex items-center gap-[6px] text-[12.5px]">
          <span
            className={`flex items-center gap-[2px] font-semibold ${
              delta > 0 ? "text-emerald-400" : "text-rose-400"
            }`}
          >
            {delta > 0 ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
            {Math.min(Math.abs(delta), 999)}%
          </span>
          <span className="text-white/55">vs. last month</span>
        </div>
      )}

      <RailDivider />
      <div className="flex flex-col">
        <StatRow label="This week" value={fmtHM(summary.week)} />
        <StatRow label="Average session" value={fmtHM(summary.avgSession)} />
        <StatRow label="Longest session" value={fmtHM(summary.longestSession)} />
        <StatRow label="Sessions logged" value={summary.sessions.toLocaleString()} />
      </div>

      <RailDivider />
      <RailLabel>Last {MINI_DAYS} days</RailLabel>
      <div className="mt-[14px]">
        <MiniBars days={summary.miniDays} />
      </div>
      <div className="mt-[14px] flex items-center gap-[7px] text-[12px] text-white/65">
        <Flame size={13} className={current > 0 ? "text-amber-400" : "text-white/30"} />
        <span>
          {current}-day streak
          {longest > 0 && <span className="text-white/55"> · best {longest}</span>}
        </span>
      </div>

      <RailDivider />
      <RailLabel>Share of play</RailLabel>
      <div className="mt-[14px]">
        <ShareBar totals={share} filter={filter} onPick={onPick} />
      </div>
      <div className="mt-[14px] flex flex-col">
        {share.map((t) => {
          const active = filter === t.id;
          return (
            <button
              key={t.id}
              onClick={() => onPick(active ? "all" : t.id)}
              aria-pressed={active}
              className={`-mx-[8px] flex items-center gap-[10px] rounded-[8px] px-[8px] py-[7px] text-left transition-colors ${
                active ? "bg-white/[0.08]" : "hover:bg-white/[0.05]"
              }`}
            >
              <span
                className="h-[7px] w-[7px] shrink-0 rounded-full"
                style={{ background: color(t.id) }}
              />
              <span
                className={`min-w-0 flex-1 truncate text-[13px] ${
                  active ? "text-white" : "text-white/70"
                }`}
              >
                {t.name}
              </span>
              <span className="shrink-0 text-[12.5px] font-medium text-white/45">
                {fmtHoursShort(t.minutes)}
              </span>
            </button>
          );
        })}
        {share.length === 0 && (
          <p className="py-2 text-[12.5px] text-white/55">Nothing played in this range.</p>
        )}
      </div>
    </>
  );
}

function RailLabel({ children }: { children: React.ReactNode }) {
  return (
    <h2 className="text-[11px] font-semibold uppercase tracking-[0.14em] text-white/55">
      {children}
    </h2>
  );
}

function RailDivider() {
  return <div className="my-[20px] h-px bg-white/[0.07]" />;
}

function StatRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between gap-3 py-[9px]">
      <span className="truncate text-[13px] text-white/45">{label}</span>
      <span className="shrink-0 text-[13px] font-semibold text-white/90">{value}</span>
    </div>
  );
}

function FilterChip({ total, onClear }: { total?: GameTotal; onClear: () => void }) {
  const icons = useCustomizationStore((s) => s.gameIcons);
  if (!total) return null;
  return (
    <button
      onClick={onClear}
      title="Show all games"
      aria-label={`Showing only ${total.name}. Show all games`}
      className="flex items-center gap-[7px] rounded-full border border-white/15 bg-white/[0.06] py-[4px] pl-[5px] pr-[9px] text-[12px] font-medium text-white/75 transition-colors hover:bg-white/[0.11] hover:text-white"
    >
      <img
        src={iconSrcFor(total.id, icons)}
        onError={iconFallback(total.id)}
        alt=""
        className="h-[17px] w-[17px] rounded-full object-cover"
      />
      {total.name}
      <X size={12} className="text-white/45" />
    </button>
  );
}

function MoreButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="mt-[22px] w-full rounded-ui border border-white/[0.09] bg-white/[0.03] py-[14px] text-[13px] font-medium text-white/60 transition-colors hover:bg-white/[0.07] hover:text-white"
    >
      {label}
    </button>
  );
}

function DaySection({
  group,
  filter,
  onPick,
  onRemove,
}: {
  group: DayGroup;
  filter: GameFilter;
  onPick: (id: GameFilter) => void;
  onRemove: (session: SessionEntry) => Promise<void>;
}) {
  return (
    <section>
      <div className="mb-[4px] flex items-center gap-[14px]">
        <h2 className="shrink-0 text-[13.5px] font-semibold text-white/85">
          {fmtDayHeading(group.date)}
        </h2>
        <div className="h-px min-w-[16px] flex-1 bg-white/[0.07]" />
        <span className="shrink-0 text-[11.5px] text-white/55">
          {group.sessions.length} {group.sessions.length === 1 ? "session" : "sessions"}
        </span>
        <span
          className="shrink-0 text-[12.5px] font-medium text-white/55"
          title="Sessions that started this day"
        >
          {fmtSessionClock(group.minutes)}
        </span>
      </div>
      <div className="flex flex-col">
        {group.sessions.map((s, i) => (
          <SessionRow
            key={`${s.gameId}-${s.start}-${i}`}
            session={s}
            filter={filter}
            onPick={onPick}
            onRemove={onRemove}
          />
        ))}
      </div>
    </section>
  );
}

function SessionRow({
  session,
  filter,
  onPick,
  onRemove,
}: {
  session: SessionSpan;
  filter: GameFilter;
  onPick: (id: GameFilter) => void;
  onRemove: (session: SessionEntry) => Promise<void>;
}) {
  const g = gameById(session.gameId as GameId);
  const color = useGameColor();
  const icons = useCustomizationStore((s) => s.gameIcons);
  const fmt = useTimeFormat();
  const picked = filter === g.id;
  const end = sessionEnd(session);
  const laterDays = endDayOffset(session.start, end);
  return (
    <div className="group -mx-[10px] flex items-center rounded-[10px] transition-colors hover:bg-white/[0.05]">
      <button
        onClick={() => onPick(picked ? "all" : g.id)}
        aria-pressed={picked}
        title={picked ? "Show all games" : `Show only ${g.name}`}
        className="flex min-w-0 flex-1 items-center gap-[14px] rounded-[10px] py-[5px] pl-[10px] pr-[8px] text-left"
      >
        <span
          className={`${usesHour12(fmt) ? "w-[156px]" : "w-[120px]"} shrink-0 whitespace-nowrap text-[12.5px] tabular-nums text-white/45`}
        >
          {session.live
            ? `${fmtClockTime(session.start, fmt)} to now`
            : fmtTimeRange(session.start, end, fmt)}
          {!session.live && laterDays > 0 && (
            <sup className="ml-[2px] text-[9.5px] text-white/55">+{laterDays}</sup>
          )}
        </span>
        <span
          className="h-[7px] w-[7px] shrink-0 rounded-full"
          style={{ background: color(g.id) }}
        />
        <img
          src={iconSrcFor(g.id, icons)}
          onError={iconFallback(g.id)}
          alt=""
          className="h-[34px] w-[34px] shrink-0 rounded-[9px] object-cover"
        />
        <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium text-white/90">
          {g.name}
        </span>
        <span className="shrink-0 text-[13.5px] font-semibold">
          {fmtSessionClock(session.minutes)}
        </span>
      </button>
      <RemoveSessionButton onConfirm={() => onRemove(session)} />
    </div>
  );
}

function RemoveSessionButton({ onConfirm }: { onConfirm: () => Promise<void> }) {
  const tip = useAnchoredTip<HTMLButtonElement>("bottom");
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const label = armed ? "Click again to remove this session" : "Remove session";

  const onClick = async () => {
    if (!armed) {
      setArmed(true);
      return;
    }
    setBusy(true);
    try {
      await onConfirm();
    } finally {
      setBusy(false);
      setArmed(false);
    }
  };

  return (
    <button
      ref={tip.anchorRef}
      {...tip.bind}
      onPointerLeave={() => {
        tip.bind.onPointerLeave();
        if (!busy) setArmed(false);
      }}
      onBlur={() => {
        tip.bind.onBlur();
        if (!busy) setArmed(false);
      }}
      onClick={() => void onClick()}
      disabled={busy}
      aria-label={label}
      className={`mr-[6px] grid h-[28px] shrink-0 place-items-center rounded-[7px] transition ${
        armed
          ? "bg-white px-[9px] text-[12px] font-semibold text-black"
          : "w-[28px] text-white/40 opacity-0 hover:bg-white/10 hover:text-white focus-visible:opacity-100 group-hover:opacity-100"
      }`}
    >
      {armed ? "Remove" : <Trash2 size={14} />}
      {tip.shown && (
        <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="bottom">
          {label}
        </TooltipPortal>
      )}
    </button>
  );
}

function EmptyState({ filtered, range }: { filtered: boolean; range: PlaytimeRange }) {
  return (
    <div className="rounded-ui border border-white/[0.08] bg-white/[0.03] px-6 py-12 text-center">
      <p className="text-[13.5px] font-medium text-white/70">No sessions to show</p>
      <p className="mt-[6px] text-[12.5px] text-white/55">
        {filtered
          ? "This game has no sessions in the selected range."
          : range === "All"
            ? "Play something and it shows up here."
            : `Nothing played in the last ${RANGE_DAYS[range]} days.`}
      </p>
    </div>
  );
}

function RailSkeleton() {
  return (
    <div className="animate-pulse" aria-hidden>
      <div className="h-[12px] w-[70px] rounded-full bg-white/[0.07]" />
      <div className="mt-[14px] h-[46px] w-[180px] rounded-[10px] bg-white/[0.07]" />
      <div className="mt-[26px] flex flex-col gap-[14px]">
        {[0, 1, 2, 3].map((i) => (
          <div key={i} className="h-[14px] rounded-full bg-white/[0.06]" />
        ))}
      </div>
      <div className="mt-[30px] h-[92px] rounded-ui bg-white/[0.06]" />
      <div className="mt-[30px] h-[9px] rounded-full bg-white/[0.07]" />
      <div className="mt-[18px] flex flex-col gap-[13px]">
        {[0, 1, 2, 3, 4].map((i) => (
          <div key={i} className="h-[14px] rounded-full bg-white/[0.06]" />
        ))}
      </div>
    </div>
  );
}

function SessionsSkeleton() {
  return (
    <div className="flex animate-pulse flex-col gap-[26px]" aria-hidden>
      {[0, 1, 2].map((g) => (
        <div key={g}>
          <div className="mb-[10px] h-[13px] w-[190px] rounded-full bg-white/[0.07]" />
          <div className="flex flex-col gap-[10px]">
            {[0, 1].map((i) => (
              <div key={i} className="h-[44px] rounded-[10px] bg-white/[0.05]" />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
