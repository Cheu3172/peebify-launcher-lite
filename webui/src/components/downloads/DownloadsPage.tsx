// ------------ Downloads Page ------------
// The Downloads tab. Shows the job running right now with its progress, the queue behind it, games with updates
// waiting, and a history of finished installs, updates, repairs and moves. Pause, resume, cancel and
// move-to-front all live here.
import { Fragment, memo, useEffect, useState, type ReactNode } from "react";
import { AnimatePresence, m } from "framer-motion";
import { listItemVariants } from "../../lib/motion";
import {
  Pause,
  Play,
  X,
  PackageOpen,
  ChevronsUp,
  RotateCcw,
  CheckCircle2,
  XCircle,
  MinusCircle,
  Zap,
  Clock,
  HardDrive,
  Download,
  Wrench,
} from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { GAMES } from "../../data/games";
import { useQueueStore, jobEquals } from "../../store/queueStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useGamesStore } from "../../store/gamesStore";
import { useNotificationStore } from "../../store/notificationStore";
import { useDownloadHistoryStore, type HistoryEntry } from "../../store/downloadHistoryStore";
import { useSpeedHistoryStore, NO_SAMPLES } from "../../store/speedHistoryStore";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import {
  pauseJob,
  resumeJob,
  prioritizeDownload,
  downloadEnqueue,
  getSteamInstall,
  repairGame,
  updateViaSteam,
  type QueueJob,
} from "../../lib/ipc";
import { requestCancelJob } from "../../lib/cancelJob";
import { GlassPage } from "../ui/GlassPage";
import { KindBadge } from "../ui/KindBadge";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";
import {
  isActiveJob,
  phaseLabel,
  canPauseJob,
  canCancelJob,
  canRetryKind,
  currentJob,
  retryJob,
  startNowPauses,
  withKind,
  baseKindOf,
  verifyFoundBrokenFiles,
  remainingDownloadBytes,
  KIND_NOUN,
  KIND_VERB,
} from "../../lib/download";
import { overallPercent } from "../../lib/stages";
import { fmtBytes, fmtSpeed, fmtEta, fmtRelTime } from "../../lib/format";
import { NOTIF_ICON_COLORS } from "../../lib/notifIcons";
import { useNow } from "../../lib/useRelativeTime";
import { StagedProgress } from "./StagedProgress";
import { SpeedGraph } from "./SpeedGraph";

const gameName = (id: string): string => GAMES.find((g) => g.id === id)?.name ?? id;

export function DownloadsPage() {
  const jobs = useQueueStore(useShallow((s) => s.jobs.filter(isActiveJob)));
  const hydrated = useQueueStore((s) => s.hydrated);
  const history = useDownloadHistoryStore((s) => s.entries);
  const clearHistory = useDownloadHistoryStore((s) => s.clear);
  const updates = useGamesStore((s) => s.updates);
  const activeGames = new Set(jobs.map((j) => j.gameId));
  const pendingUpdates = updates.filter((id) => !activeGames.has(id));

  const running = jobs.filter((j) => j.phase !== "queued");
  const queued = jobs.filter((j) => j.phase === "queued");
  const runningIds = running.map((j) => j.id).join();
  const queuedIds = queued.map((j) => j.id).join();
  const historyKeys = history.map((e) => e.key).join();
  const holder = currentJob(running);
  const holderName = holder ? gameName(holder.gameId) : undefined;
  const startHint =
    !holder || holder.paused
      ? "Start now"
      : startNowPauses(holder, baseKindOf(holder))
        ? `Start now, pauses ${holderName}`
        : "Move to front";
  const moving = running.filter((j) => !j.paused);
  const blockedBy = moving.length === 1 ? gameName(moving[0].gameId) : undefined;
  const pausedHolder = moving.length === 0 && holder?.paused ? holderName : undefined;
  const waitNote =
    moving.length === 0
      ? pausedHolder
        ? `${pausedHolder} is paused. Resume it, or use Start now to go first.`
        : "Waiting to start"
      : blockedBy
        ? `Starts when ${blockedBy} finishes`
        : "Starts when a download finishes";

  return (
    <GlassPage
      title="Downloads"
      subtitle="Installs, updates, repairs and moves."
      headerRight={running.length > 0 ? <SummaryChip jobs={running} /> : undefined}
    >
      {!hydrated ? null : jobs.length === 0 ? (
        <div className="flex items-center justify-center gap-3 rounded-ui border border-dashed border-white/15 bg-white/[0.04] px-6 py-10 text-[14px] text-white/55">
          <PackageOpen size={18} className="opacity-70" />
          Nothing running. Install or update a game from its home screen.
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <AnimatePresence initial={false}>
            {running.map((job) => (
              <m.div
                key={job.id}
                layout
                layoutDependency={runningIds}
                variants={listItemVariants}
                initial="initial"
                animate="animate"
                exit="exit"
              >
                <JobCard
                  job={job}
                  kind={baseKindOf(job)}
                  waitingFor={job.parked ? holderName : undefined}
                />
              </m.div>
            ))}
          </AnimatePresence>
        </div>
      )}

      {queued.length > 0 && (
        <section className="mt-8">
          <SectionHeader
            title="Queued"
            right={`${queued.length} waiting${
              blockedBy
                ? ` · after ${blockedBy}`
                : pausedHolder
                  ? ` · after ${pausedHolder} (paused)`
                  : ""
            }`}
          />
          <div className="flex flex-col gap-2">
            <AnimatePresence initial={false}>
              {queued.map((job, index) => (
                <m.div
                  key={job.id}
                  layout
                  layoutDependency={queuedIds}
                  variants={listItemVariants}
                  initial="initial"
                  animate="animate"
                  exit="exit"
                >
                  <QueuedRow
                    job={job}
                    kind={baseKindOf(job)}
                    position={index + 1}
                    note={index === 0 ? waitNote : "Waiting to start"}
                    startHint={startHint}
                  />
                </m.div>
              ))}
            </AnimatePresence>
          </div>
        </section>
      )}

      {hydrated && pendingUpdates.length > 0 && (
        <section className="mt-8">
          <SectionHeader
            title="Updates available"
            right={`${pendingUpdates.length} ${pendingUpdates.length === 1 ? "game" : "games"}`}
          />
          <div className="flex flex-col gap-2">
            {pendingUpdates.map((id) => (
              <UpdateRow key={id} gameId={id} />
            ))}
          </div>
        </section>
      )}

      {history.length > 0 && (
        <section className="mt-8">
          <SectionHeader
            title="Finished"
            right={
              <button
                onClick={clearHistory}
                className="rounded-ui px-[9px] py-[4px] text-[11.5px] font-medium text-white/55 transition-colors hover:bg-white/[0.06] hover:text-white/80"
              >
                Clear
              </button>
            }
          />
          <div className="flex flex-col gap-2">
            <AnimatePresence initial={false}>
              {history.map((entry) => (
                <m.div
                  key={entry.key}
                  layout
                  layoutDependency={historyKeys}
                  variants={listItemVariants}
                  initial="initial"
                  animate="animate"
                  exit="exit"
                >
                  <HistoryRow entry={entry} hasActiveJob={activeGames.has(entry.gameId)} />
                </m.div>
              ))}
            </AnimatePresence>
          </div>
        </section>
      )}
    </GlassPage>
  );
}

function SectionHeader({ title, right }: { title: string; right?: ReactNode }) {
  return (
    <div className="mb-[10px] flex items-center justify-between gap-3">
      <h2 className="text-[11px] font-semibold uppercase tracking-[0.09em] text-white/55">
        {title}
      </h2>
      {typeof right === "string" ? (
        <span className="text-[11.5px] text-white/55">{right}</span>
      ) : (
        right
      )}
    </div>
  );
}

function SummaryChip({ jobs }: { jobs: QueueJob[] }) {
  const moving = jobs.filter((j) => !j.paused);
  const remaining = remainingDownloadBytes(jobs);
  const speed = fmtSpeed(moving.reduce((sum, j) => sum + Math.max(0, j.speed), 0));
  const eta = fmtEta(moving.reduce((max, j) => Math.max(max, j.etaSecs), 0));
  const cells: { icon: ReactNode; text: string }[] = [];
  if (speed) cells.push({ icon: <Zap size={13} className="text-white/40" />, text: speed });
  if (remaining > 0) {
    cells.push({
      icon: <HardDrive size={13} className="text-white/40" />,
      text: `${fmtBytes(remaining)} left`,
    });
  }
  if (eta) cells.push({ icon: <Clock size={13} className="text-white/40" />, text: `~${eta}` });
  if (cells.length === 0) return null;

  return (
    <div className="flex items-center rounded-ui border border-white/[0.12] bg-white/[0.04]">
      {cells.map((cell, i) => (
        <Fragment key={cell.text}>
          {i > 0 && <span className="h-[15px] w-px bg-white/[0.12]" />}
          <span className="flex items-center gap-[6px] px-[11px] py-[7px] text-[12.5px] tabular-nums text-white/75">
            {cell.icon}
            {cell.text}
          </span>
        </Fragment>
      ))}
    </div>
  );
}

const ICON_BTN =
  "grid h-[32px] w-[32px] place-items-center rounded-[9px] text-white/70 transition-colors hover:bg-white/10 hover:text-white active:bg-white/[0.16]";
const CANCEL_BTN =
  "grid h-[32px] w-[32px] place-items-center rounded-[9px] text-white/55 transition-colors hover:bg-(--color-danger,#f87171)/[0.15] hover:text-(--color-danger-soft,#fca5a5) active:bg-(--color-danger,#f87171)/25";

const JobCard = memo(
  function JobCard({
    job,
    kind,
    waitingFor,
  }: {
    job: QueueJob;
    kind: string;
    waitingFor?: string;
  }) {
    const icons = useCustomizationStore((s) => s.gameIcons);
    const samples = useSpeedHistoryStore((s) => s.byJob[job.id] ?? NO_SAMPLES);
    const pauseTip = useAnchoredTip<HTMLButtonElement>("bottom");
    const cancelTip = useAnchoredTip<HTMLButtonElement>("bottom");
    const effective = withKind(job, kind);
    const pauseHint = job.paused ? "Resume" : "Pause";

    const eta = fmtEta(job.etaSecs);
    const idle = job.paused || !!job.waitingNetwork;
    const stats = [
      job.total > 0 ? `${fmtBytes(job.downloaded)} of ${fmtBytes(job.total)}` : "",
      idle ? "" : fmtSpeed(job.speed),
      !idle && eta ? `${eta} left` : "",
      job.waitingNetwork && !job.paused ? "Resumes automatically" : "",
    ]
      .filter(Boolean)
      .join(" · ");
    const tone = job.parked
      ? "text-white/55"
      : idle
        ? "text-(--color-warning,#fcd34d)/95"
        : "text-(--accent-text)";
    const label = job.parked && waitingFor ? `Waiting for ${waitingFor}` : phaseLabel(job);

    return (
      <div className="job-card rounded-ui border border-white/[0.08] bg-white/[0.05] px-[18px] py-[16px]">
        <div className="flex items-center gap-[14px]">
          <img
            src={iconSrcFor(job.gameId, icons)}
            onError={iconFallback(job.gameId)}
            alt=""
            className="job-card-icon h-[44px] w-[44px] shrink-0 rounded-[11px] object-cover"
          />
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-[9px]">
              <span className="truncate text-[14.5px] font-semibold">{gameName(job.gameId)}</span>
              <KindBadge kind={kind} />
            </div>
            <div className="mt-[3px] flex items-center gap-[6px] text-[12px] leading-[1.3]">
              <span className={`shrink-0 font-medium ${tone}`}>{label}</span>
              {stats && (
                <>
                  <span className="shrink-0 text-white/20">·</span>
                  <span className="truncate tabular-nums text-white/50">{stats}</span>
                </>
              )}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-[10px]">
            <span className="text-[20px] font-semibold tracking-[-0.01em] tabular-nums">
              {Math.round(overallPercent(job, kind))}%
            </span>
            {canPauseJob(effective) && (
              <button
                ref={pauseTip.anchorRef}
                {...pauseTip.bind}
                onClick={() => void (job.paused ? resumeJob(effective) : pauseJob(effective))}
                aria-label={pauseHint}
                className={ICON_BTN}
              >
                {job.paused ? <Play size={15} /> : <Pause size={15} />}
                {pauseTip.shown && (
                  <TooltipPortal x={pauseTip.pos.x} y={pauseTip.pos.y} placement="bottom">
                    {pauseHint}
                  </TooltipPortal>
                )}
              </button>
            )}
            {canCancelJob(effective) && (
              <button
                ref={cancelTip.anchorRef}
                {...cancelTip.bind}
                onClick={() => requestCancelJob(effective)}
                aria-label="Cancel"
                className={CANCEL_BTN}
              >
                <X size={15} />
                {cancelTip.shown && (
                  <TooltipPortal x={cancelTip.pos.x} y={cancelTip.pos.y} placement="bottom">
                    Cancel
                  </TooltipPortal>
                )}
              </button>
            )}
          </div>
        </div>

        <StagedProgress job={job} kind={kind} className="mt-[14px]" />

        {samples.length > 1 && samples.some((v) => v > 0) && <SpeedGraph samples={samples} />}
      </div>
    );
  },
  (prev, next) =>
    prev.kind === next.kind &&
    prev.waitingFor === next.waitingFor &&
    jobEquals(prev.job, next.job),
);

const QueuedRow = memo(
  function QueuedRow({
    job,
    kind,
    position,
    note,
    startHint,
  }: {
    job: QueueJob;
    kind: string;
    position: number;
    note: string;
    startHint: string;
  }) {
    const icons = useCustomizationStore((s) => s.gameIcons);
    const tip = useAnchoredTip<HTMLButtonElement>("bottom");
    const cancelTip = useAnchoredTip<HTMLButtonElement>("bottom");
    const effective = withKind(job, kind);
    return (
      <div className="flex items-center gap-[12px] rounded-ui border border-white/[0.07] bg-white/[0.03] px-4 py-[11px]">
        <span className="grid h-[20px] w-[20px] shrink-0 place-items-center rounded-[6px] border border-white/[0.12] text-[11px] font-semibold tabular-nums text-white/45">
          {position}
        </span>
        <img
          src={iconSrcFor(job.gameId, icons)}
          onError={iconFallback(job.gameId)}
          alt=""
          className="h-[30px] w-[30px] shrink-0 rounded-[8px] object-cover"
        />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-[8px]">
            <span className="truncate text-[13px] font-medium">{gameName(job.gameId)}</span>
            <KindBadge kind={kind} />
          </div>
          <p className="mt-[2px] truncate text-[11.5px] text-white/55">{note}</p>
        </div>
        <button
          ref={tip.anchorRef}
          {...tip.bind}
          onClick={() => prioritizeDownload(job.id)}
          aria-label={startHint}
          className={ICON_BTN}
        >
          <ChevronsUp size={15} />
          {tip.shown && (
            <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="bottom">
              {startHint}
            </TooltipPortal>
          )}
        </button>
        {canCancelJob(effective) && (
          <button
            ref={cancelTip.anchorRef}
            {...cancelTip.bind}
            onClick={() => requestCancelJob(effective)}
            aria-label="Cancel"
            className={CANCEL_BTN}
          >
            <X size={15} />
            {cancelTip.shown && (
              <TooltipPortal x={cancelTip.pos.x} y={cancelTip.pos.y} placement="bottom">
                Cancel
              </TooltipPortal>
            )}
          </button>
        )}
      </div>
    );
  },
  (prev, next) =>
    prev.position === next.position &&
    prev.kind === next.kind &&
    prev.note === next.note &&
    prev.startHint === next.startHint &&
    jobEquals(prev.job, next.job),
);

function UpdateRow({ gameId }: { gameId: string }) {
  const icons = useCustomizationStore((s) => s.gameIcons);
  const steamManaged = useGamesStore((s) => s.steamManaged).includes(gameId);
  const running = useGamesStore((s) => s.runningGames).includes(gameId);
  const push = useNotificationStore((s) => s.push);
  const name = gameName(gameId);

  const onUpdate = () => {
    if (!steamManaged) {
      void downloadEnqueue(gameId);
      return;
    }
    void updateViaSteam(gameId).then((ok) => {
      if (!ok) return;
      push({
        title: `${name} update handed to Steam`,
        text: "Steam is downloading the update and will start the game when it's done.",
      });
    });
  };

  return (
    <div className="flex items-center gap-[12px] rounded-ui border border-white/[0.07] bg-white/[0.03] px-4 py-[11px]">
      <img
        src={iconSrcFor(gameId, icons)}
        onError={iconFallback(gameId)}
        alt=""
        className="h-[30px] w-[30px] shrink-0 rounded-[8px] object-cover"
      />
      <div className="min-w-0 flex-1">
        <span className="block truncate text-[13px] font-medium">{name}</span>
        <p className="mt-[2px] truncate text-[11.5px] text-white/55">
          {running
            ? "Close the game to update"
            : steamManaged
              ? "Updates through Steam"
              : "New version ready"}
        </p>
      </div>
      <button
        onClick={onUpdate}
        disabled={running}
        aria-label={`Update ${name}`}
        className="flex shrink-0 items-center gap-[6px] rounded-ui border border-white/10 bg-white/[0.05] px-[10px] py-[6px] text-[11.5px] font-medium transition-colors hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-40"
      >
        <Download size={12} /> Update
      </button>
    </div>
  );
}

const HISTORY_META: Record<
  HistoryEntry["phase"],
  { icon: typeof CheckCircle2; color: string; label: string }
> = {
  done: { icon: CheckCircle2, color: NOTIF_ICON_COLORS.success.fg, label: "Done" },
  cancelled: { icon: MinusCircle, color: "rgba(255,255,255,.45)", label: "Stopped" },
  error: { icon: XCircle, color: NOTIF_ICON_COLORS.error.fg, label: "Failed" },
};

function outcomeText(entry: HistoryEntry): string {
  if (entry.phase === "done") return KIND_VERB[entry.kind] ?? "finished";
  const noun = KIND_NOUN[entry.kind] ?? entry.kind;
  return entry.phase === "cancelled" ? `${noun} cancelled` : `${noun} failed`;
}

function useIsSteamCopy(gameId: string, needed: boolean): boolean | null {
  const [state, setState] = useState<{ gameId: string; steam: boolean } | null>(null);
  useEffect(() => {
    if (!needed) return;
    let alive = true;
    void getSteamInstall(gameId).then((info) => {
      if (alive) setState({ gameId, steam: info.isSteamInstall });
    });
    return () => {
      alive = false;
    };
  }, [gameId, needed]);
  return state?.gameId === gameId ? state.steam : null;
}

const HistoryRow = memo(function HistoryRow({
  entry,
  hasActiveJob,
}: {
  entry: HistoryEntry;
  hasActiveJob: boolean;
}) {
  const icons = useCustomizationStore((s) => s.gameIcons);
  const now = useNow();
  const meta = HISTORY_META[entry.phase];
  const Icon = meta.icon;
  const installed = useGamesStore((s) => s.installed.includes(entry.gameId));
  const updatePending = useGamesStore((s) => s.updates.includes(entry.gameId));
  const repairCandidate =
    entry.phase === "error" &&
    entry.kind === "verify" &&
    installed &&
    !updatePending &&
    verifyFoundBrokenFiles(entry.error);
  const steamCopy = useIsSteamCopy(entry.gameId, repairCandidate);
  const offerRepair = repairCandidate && steamCopy === false;
  const offerRetry =
    entry.phase === "error" && canRetryKind(entry.kind) && (!repairCandidate || steamCopy === true);

  return (
    <div className="flex items-center gap-[12px] rounded-ui border border-white/[0.06] bg-white/[0.03] px-4 py-[10px]">
      <img
        src={iconSrcFor(entry.gameId, icons)}
        onError={iconFallback(entry.gameId)}
        alt=""
        className="h-[28px] w-[28px] shrink-0 rounded-[8px] object-cover"
      />
      <div className="min-w-0 flex-1">
        <p className="truncate text-[12.5px]">
          <span className="font-medium">{gameName(entry.gameId)}</span>
          <span className="text-white/55"> · {outcomeText(entry)}</span>
        </p>
        {entry.phase === "error" && entry.error && (
          <p
            className="mt-[2px] truncate text-[11.5px] text-(--color-danger-soft,#fca5a5)/85"
            title={entry.error}
          >
            {entry.error}
          </p>
        )}
        {entry.phase === "done" && entry.message && (
          <p className="mt-[2px] truncate text-[11.5px] text-white/55" title={entry.message}>
            {entry.message}
          </p>
        )}
      </div>
      <span className="shrink-0 text-[11.5px] text-white/55">
        {fmtRelTime(entry.finishedAt, now)}
      </span>
      <span
        className="flex w-[74px] shrink-0 items-center justify-end gap-[6px] text-[11.5px]"
        style={{ color: meta.color }}
      >
        <Icon size={13} />
        {meta.label}
      </span>
      {offerRepair && (
        <button
          onClick={() => void repairGame(entry.gameId, false)}
          disabled={hasActiveJob}
          title={hasActiveJob ? "This game already has an active job" : "Replace the broken files"}
          className="flex shrink-0 items-center gap-[6px] rounded-ui border border-white/10 bg-white/[0.05] px-[10px] py-[6px] text-[11.5px] font-medium transition-colors hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-40"
        >
          <Wrench size={12} /> Repair
        </button>
      )}
      {offerRetry && (
        <button
          onClick={() => void retryJob(entry.gameId, entry.kind)}
          disabled={hasActiveJob}
          title={hasActiveJob ? "This game already has an active job" : "Try again"}
          className="flex shrink-0 items-center gap-[6px] rounded-ui border border-white/10 bg-white/[0.05] px-[10px] py-[6px] text-[11.5px] font-medium transition-colors hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-40"
        >
          <RotateCcw size={12} /> Retry
        </button>
      )}
    </div>
  );
});
