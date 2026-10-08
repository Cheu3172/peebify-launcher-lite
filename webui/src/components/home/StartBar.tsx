// ------------ Start Bar ------------
// The big button on the Home screen. Depending on the game it reads Install, Update, Play or Running, and while
// a download is going it turns into progress with pause and cancel.
import { useEffect, useLayoutEffect, useState } from "react";
import { AnimatePresence, m, type Variants } from "framer-motion";
import { Play, Download, Pause, X, ChevronsUp, Loader2 } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useUiStore } from "../../store/uiStore";
import { useGamesStore } from "../../store/gamesStore";
import { useQueueStore } from "../../store/queueStore";
import { useModalStore } from "../../store/modalStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useNotificationStore } from "../../store/notificationStore";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import { gameById } from "../../data/games";
import type { GameId } from "../../types/game";
import { PlaytimeTracker } from "./PlaytimeTracker";
import { KindBadge } from "../ui/KindBadge";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";
import { StagedProgress } from "../downloads/StagedProgress";
import { overallPercent } from "../../lib/stages";
import {
  launchGame,
  downloadEnqueue,
  pauseJob,
  resumeJob,
  prioritizeDownload,
  resumeInstall,
  updateViaSteam,
  type GameUpdateVersions,
  type QueueJob,
} from "../../lib/ipc";
import { requestCancelJob } from "../../lib/cancelJob";
import { warmInstall } from "../../lib/installPreload";
import { fadeVariants } from "../../lib/motion";
import {
  isActiveJob,
  phaseLabel,
  canPauseJob,
  canCancelJob,
  isJobDownloading,
  currentJob,
  startNowPauses,
  withKind,
  baseKindOf,
} from "../../lib/download";
import { fmtBytes, fmtSpeed, fmtEta } from "../../lib/format";

type ButtonState =
  | "install"
  | "resume"
  | "update"
  | "steam"
  | "launch"
  | "starting"
  | "steamWait"
  | "running";

const BUTTON_LABELS: Record<ButtonState, string> = {
  install: "Install",
  resume: "Resume",
  update: "Update",
  steam: "Update in Steam",
  launch: "Launch",
  starting: "Starting",
  steamWait: "Waiting for Steam",
  running: "Running",
};

const PLAIN_VERSION = /^\d+(\.\d+)+$/;

function updateVersionHint(versions: GameUpdateVersions | undefined): string | null {
  const latest = versions?.latestVersion?.trim();
  if (!latest || !PLAIN_VERSION.test(latest)) return null;
  const current = versions?.currentVersion?.trim();
  if (!current) return `Update to ${latest}`;
  if (!PLAIN_VERSION.test(current) || current === latest) return null;
  return `Update from ${current} to ${latest}`;
}

const EASE_SWAP = [0.2, 0.7, 0.2, 1] as const;

const iconSwap: Variants = {
  initial: { opacity: 0, scale: 0.55 },
  animate: { opacity: 1, scale: 1, transition: { duration: 0.28, ease: EASE_SWAP } },
  exit: { opacity: 0, scale: 0.55, transition: { duration: 0.22, ease: EASE_SWAP } },
};

const labelSwap: Variants = {
  initial: { opacity: 0, y: 6 },
  animate: { opacity: 1, y: 0, transition: { duration: 0.24, ease: EASE_SWAP } },
  exit: { opacity: 0, y: -6, transition: { duration: 0.18, ease: EASE_SWAP } },
};

function StateIcon({ state }: { state: ButtonState }) {
  if (state === "running") {
    return (
      <span className="flex h-[16px] items-center gap-[2.5px]">
        {[0, 1, 2].map((i) => (
          <span
            key={i}
            className="activity-bar h-[14px] w-[2.5px] rounded-[2px] bg-[#111]"
            style={{ animationDelay: `${i * 0.18}s` }}
          />
        ))}
      </span>
    );
  }
  if (state === "starting" || state === "steamWait") {
    return <Loader2 size={16} className="animate-spin" />;
  }
  if (state === "launch") return <Play size={16} fill="currentColor" strokeWidth={1.5} />;
  return <Download size={16} strokeWidth={2.4} />;
}

export function StartBar({ hidePlaytime = false }: { hidePlaytime?: boolean }) {
  const activeId = useUiStore((s) => s.activeGameId);
  const running = useGamesStore((s) => s.runningGames).includes(activeId);
  const installed = useGamesStore((s) => s.installed).includes(activeId);
  const pending = useGamesStore((s) => s.pendingInstalls)[activeId];
  const hasUpdate = useGamesStore((s) => s.updates).includes(activeId);
  const steamManaged = useGamesStore((s) => s.steamManaged).includes(activeId);
  const updateVersions = useGamesStore((s) => s.updateVersions[activeId]);
  const tip = useAnchoredTip<HTMLButtonElement>("right");
  const job = useQueueStore(
    useShallow((s) => s.jobs.find((j) => j.gameId === activeId && isActiveJob(j))),
  );
  const holder = useQueueStore(
    useShallow((s) => {
      const current = currentJob(s.jobs.filter((j) => j.gameId !== activeId));
      return current
        ? {
            gameId: current.gameId,
            pauses: startNowPauses(current, baseKindOf(current)),
          }
        : undefined;
    }),
  );
  const startTip = useAnchoredTip<HTMLButtonElement>("bottom");
  const pauseTip = useAnchoredTip<HTMLButtonElement>("bottom");
  const cancelTip = useAnchoredTip<HTMLButtonElement>("bottom");
  const game = gameById(activeId);
  const openInstall = useModalStore((s) => s.openInstall);
  const dialogOpen = useModalStore((s) => s.modal !== null);
  // Clicking the button leaves it hovered (lifted, with a bigger shadow). When a dialog then opens over it, the
  // hover ends and the button would sink back while the dimming fades in on top, which reads as a flicker under
  // the overlay. Keep it lifted until the dialog closes; it settles back afterwards with nothing on top of it.
  const [heldRaised, setHeldRaised] = useState(false);
  useLayoutEffect(() => {
    if (dialogOpen) {
      if (tip.anchorRef.current?.matches(":hover")) setHeldRaised(true);
    } else {
      setHeldRaised(false);
    }
  }, [dialogOpen, tip.anchorRef]);
  const needsInstall = !installed && !pending;
  useEffect(() => {
    if (needsInstall) warmInstall(activeId);
  }, [needsInstall, activeId]);
  const icons = useCustomizationStore((s) => s.gameIcons);
  const push = useNotificationStore((s) => s.push);

  const starting = useGamesStore((s) => s.launchingGames).includes(activeId);
  const waitingForSteam = useGamesStore((s) => s.launchingViaSteam).includes(activeId);

  const doLaunch = () => {
    const id = activeId;
    useGamesStore.getState().setLaunching(id, true);
    void launchGame(id).then(({ accepted, viaSteam }) => {
      const latest = useGamesStore.getState();
      if (!accepted || latest.runningGames.includes(id)) {
        latest.setLaunching(id, false);
      } else if (viaSteam && latest.launchingGames.includes(id)) {
        latest.setLaunching(id, true, true);
      }
    });
  };

  const doSteamUpdate = () => {
    void updateViaSteam(activeId).then((ok) => {
      if (!ok) return;
      push({
        title: `${game.name} update handed to Steam`,
        text: "Steam is downloading the update and will start the game when it's done. Peebify tracks that session, but mods only load when you start the game from Peebify.",
      });
    });
  };

  const renderJobCard = (job: QueueJob) => {
    const kind = baseKindOf(job);
    const effective = withKind(job, kind);
    const downloading = isJobDownloading(effective);
    const queued = job.phase === "queued";
    const holderName = holder ? gameById(holder.gameId as GameId).name : undefined;
    const startHint = !holder
      ? "Start now"
      : holder.pauses
        ? `Start now, pauses ${holderName}`
        : "Move to front";
    const label =
      job.parked && holderName ? `Waiting for ${holderName}` : phaseLabel(effective);
    const pauseHint = job.paused ? "Resume" : "Pause";

    const stats: string[] = [];
    if (downloading && job.speed > 0) {
      stats.push(fmtSpeed(job.speed));
      if (job.etaSecs > 0) stats.push(`${fmtEta(job.etaSecs)} left`);
    } else if (!queued && job.total > 0) {
      stats.push(`${fmtBytes(job.downloaded)} / ${fmtBytes(job.total)}`);
    }

    const tone = job.parked
      ? "text-white/45"
      : job.paused
        ? "text-amber-300/95"
        : queued
          ? "text-white/45"
          : "text-(--accent-text)";

    return (
      <div className="w-[414px]">
        <m.div
          variants={fadeVariants}
          className="glass job-card rounded-[11px] px-[15px] py-[14px]"
        >
          <div className="flex items-center gap-[12px]">
            <img
              src={iconSrcFor(activeId, icons)}
              onError={iconFallback(activeId)}
              alt=""
              className="job-card-icon h-[40px] w-[40px] shrink-0 rounded-[10px] object-cover"
            />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-[8px]">
                <span className="truncate text-[13.5px] font-semibold leading-[1.25]">
                  {game.name}
                </span>
                <KindBadge kind={kind} compact />
              </div>
              <div className="mt-[3px] flex items-center gap-[6px] text-[11.5px] leading-[1.3]">
                <span className={`shrink-0 font-medium ${tone}`}>{label}</span>
                {stats.length > 0 && (
                  <>
                    <span className="shrink-0 text-white/20">·</span>
                    <span className="truncate tabular-nums text-white/50">{stats.join(" · ")}</span>
                  </>
                )}
              </div>
            </div>
            {!queued && (
              <span className="shrink-0 text-[17px] font-semibold tracking-[-0.01em] tabular-nums">
                {Math.round(overallPercent(job, kind))}%
              </span>
            )}
            <div className="flex shrink-0 items-center gap-[2px]">
              {queued ? (
                <button
                  ref={startTip.anchorRef}
                  {...startTip.bind}
                  onClick={() => prioritizeDownload(job.id)}
                  aria-label={startHint}
                  className="grid h-[28px] w-[28px] place-items-center rounded-[8px] text-white/70 transition-colors hover:bg-white/10 hover:text-white active:bg-white/[0.16]"
                >
                  <ChevronsUp size={14} />
                  {startTip.shown && (
                    <TooltipPortal x={startTip.pos.x} y={startTip.pos.y} placement="bottom">
                      {startHint}
                    </TooltipPortal>
                  )}
                </button>
              ) : (
                canPauseJob(effective) && (
                  <button
                    ref={pauseTip.anchorRef}
                    {...pauseTip.bind}
                    onClick={() => void (job.paused ? resumeJob(effective) : pauseJob(effective))}
                    aria-label={pauseHint}
                    className="grid h-[28px] w-[28px] place-items-center rounded-[8px] text-white/70 transition-colors hover:bg-white/10 hover:text-white active:bg-white/[0.16]"
                  >
                    {job.paused ? <Play size={14} /> : <Pause size={14} />}
                    {pauseTip.shown && (
                      <TooltipPortal x={pauseTip.pos.x} y={pauseTip.pos.y} placement="bottom">
                        {pauseHint}
                      </TooltipPortal>
                    )}
                  </button>
                )
              )}
              {canCancelJob(effective) && (
                <button
                  ref={cancelTip.anchorRef}
                  {...cancelTip.bind}
                  onClick={() => requestCancelJob(effective)}
                  aria-label="Cancel"
                  className="grid h-[28px] w-[28px] place-items-center rounded-[8px] text-white/50 transition-colors hover:bg-red-500/15 hover:text-red-300"
                >
                  <X size={14} />
                  {cancelTip.shown && (
                    <TooltipPortal x={cancelTip.pos.x} y={cancelTip.pos.y} placement="bottom">
                      Cancel
                    </TooltipPortal>
                  )}
                </button>
              )}
            </div>
          </div>

          <StagedProgress job={job} kind={kind} className="mt-[13px]" />
        </m.div>
      </div>
    );
  };

  let state: ButtonState = "install";
  let onClick: () => void = () => openInstall(activeId);

  if (installed) {
    if (hasUpdate) {
      state = steamManaged ? "steam" : "update";
      onClick = steamManaged ? doSteamUpdate : () => void downloadEnqueue(activeId);
    } else {
      state = "launch";
      onClick = doLaunch;
    }
  } else if (pending) {
    state = "resume";
    onClick = () => void resumeInstall(activeId, pending);
  }
  if (starting) {
    state = waitingForSteam ? "steamWait" : "starting";
    onClick = () => {};
  }
  if (running) {
    state = "running";
    onClick = () => {};
  }

  const idle = !running && !starting;
  const updateHint =
    state === "update" || state === "steam" ? updateVersionHint(updateVersions) : null;

  return (
    <AnimatePresence mode="wait" initial={false}>
      {job ? (
        <m.div key="job" initial="initial" animate="animate" exit="exit">
          {renderJobCard(job)}
        </m.div>
      ) : (
        <m.div
          key="idle"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1, transition: { duration: 0.12 } }}
          exit={{ opacity: 0, transition: { duration: 0.12 } }}
        >
          <div className="flex items-end gap-[14px]">
            <button
              ref={tip.anchorRef}
              {...tip.bind}
              onClick={onClick}
              disabled={!idle}
              className={`start-button relative z-[1] grid h-[46px] w-[200px] place-items-center rounded-[12px] bg-white text-[15px] font-semibold tracking-[-0.005em] text-[#111] ${
                idle ? "start-button-idle" : ""
              } ${idle && heldRaised ? "start-button-raised" : ""}`}
            >
              <AnimatePresence initial={false}>
                <m.span
                  key={state}
                  initial="initial"
                  animate="animate"
                  exit="exit"
                  style={{ gridArea: "1 / 1" }}
                  className="pointer-events-none flex items-center gap-[10px]"
                >
                  <m.span variants={iconSwap} className="grid h-[16px] w-[16px] place-items-center">
                    <StateIcon state={state} />
                  </m.span>
                  <m.span variants={labelSwap}>{BUTTON_LABELS[state]}</m.span>
                </m.span>
              </AnimatePresence>
              {tip.shown && updateHint && (
                <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="right">
                  {updateHint}
                </TooltipPortal>
              )}
            </button>

            {!hidePlaytime && <PlaytimeTracker />}
          </div>
        </m.div>
      )}
    </AnimatePresence>
  );
}
