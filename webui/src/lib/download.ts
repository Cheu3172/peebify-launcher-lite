// ------------ Download Helpers ------------
// Small helpers for download queue jobs: is it still active, can it be paused or cancelled, what to call it
// on screen, and how to retry it after a failure.
import {
  downloadEnqueue,
  lastRepairMode,
  repairGame,
  resumeInstall,
  verifyGameIntegrity,
  type QueueJob,
} from "./ipc";
import { useGamesStore } from "../store/gamesStore";
import { useModalStore } from "../store/modalStore";
import { supportsQuickRepair } from "../data/games";
import type { GameId } from "../types/game";

export const TERMINAL_PHASES = ["done", "cancelled", "deferred", "error"];
export const isActiveJob = (j: QueueJob): boolean => !TERMINAL_PHASES.includes(j.phase);

export const baseKindOf = (j: QueueJob): string => j.baseKind || j.kind;

export const KIND_VERB: Record<string, string> = {
  install: "installed",
  update: "updated",
  repair: "repaired",
  verify: "verified",
  move: "moved",
  uninstall: "removed",
};

export const KIND_NOUN: Record<string, string> = {
  install: "install",
  update: "update",
  repair: "repair",
  verify: "verify",
  move: "move",
  uninstall: "removal",
};

const PHASE_LABELS: Record<string, string> = {
  verifying: "Verifying",
  scanning: "Checking",
  downloading: "Downloading",
  extracting: "Extracting",
  installing: "Finalizing",
  validating: "Finalizing",
  repairing: "Repairing",
  moving: "Finalizing",
};

export function phaseLabel(j: QueueJob): string {
  if (j.parked) return "Waiting";
  if (j.paused) return "Paused";
  if (j.waitingNetwork) return "Waiting for connection";
  if (j.phase === "queued") return "Queued";
  if (j.phase === "done") return "Done";
  if (j.phase === "cancelled") return "Cancelled";
  if (j.phase === "deferred") return "Deferred";
  if (j.phase === "error") return "Error";
  if (j.kind === "move") return j.phase === "moving" ? "Removing old copy" : "Transferring";
  if (j.kind === "uninstall") return "Removing";
  if (j.kind === "verify") return "Verifying";
  if (j.kind === "repair") {
    const mapped = PHASE_LABELS[j.phase];
    return !mapped || mapped === "Downloading" ? "Verifying" : mapped;
  }
  return PHASE_LABELS[j.phase] ?? j.phase;
}

export const isDownloadKind = (kind: string): boolean =>
  kind === "install" || kind === "update";
export const canPauseJob = (j: QueueJob): boolean =>
  (isDownloadKind(j.kind) || j.kind === "repair") && j.phase !== "extracting" && !j.parked;
export const canCancelJob = (j: QueueJob): boolean =>
  isDownloadKind(j.kind) ||
  j.kind === "repair" ||
  j.kind === "verify" ||
  (j.kind === "move" && j.phase !== "moving");
export const isJobDownloading = (j: QueueJob): boolean =>
  isDownloadKind(j.kind) && j.phase === "downloading" && !j.paused;

export const remainingDownloadBytes = (jobs: QueueJob[]): number =>
  jobs.reduce(
    (sum, j) =>
      isDownloadKind(j.kind) && j.phase === "downloading" && j.total > 0
        ? sum + Math.max(0, j.total - j.downloaded)
        : sum,
    0,
  );

export const withKind = (j: QueueJob, kind: string): QueueJob =>
  j.kind === kind ? j : { ...j, kind };

export const currentJob = (jobs: QueueJob[]): QueueJob | undefined =>
  jobs.find((j) => isActiveJob(j) && j.phase !== "queued" && !j.parked);

export const startNowPauses = (current: QueueJob | undefined, kind: string | undefined): boolean =>
  !!current && !!kind && (isDownloadKind(kind) || kind === "repair") && current.phase !== "extracting";

export const canRetryKind = (kind: string): boolean =>
  isDownloadKind(kind) || kind === "repair" || kind === "verify";

const BROKEN_FILES_PREFIX = /^\d+ files? needs? repair\./;
const BROKEN_FILES_ERRORS = [
  /^\d+ files? needs? repair\. Run a repair to replace (?:it|them)\.$/,
  /\. Repair replaces them\.$/,
];

export const verifyFoundBrokenFiles = (error: string | null | undefined): boolean =>
  !!error && BROKEN_FILES_ERRORS.some((re) => re.test(error));

export const verifyNeedsRepairFirst = (message: string | null | undefined): boolean =>
  !!message && BROKEN_FILES_PREFIX.test(message);

export async function retryJob(gameId: string, kind: string): Promise<void> {
  if (kind === "repair") {
    const mode = lastRepairMode(gameId);
    await repairGame(gameId, mode ? mode === "quick" : supportsQuickRepair(gameId as GameId));
    return;
  }
  if (kind === "verify") {
    await verifyGameIntegrity(gameId);
    return;
  }
  if (!isDownloadKind(kind)) return;
  const { installed, pendingInstalls } = useGamesStore.getState();
  if (installed.includes(gameId)) {
    await downloadEnqueue(gameId);
    return;
  }
  const pending = pendingInstalls[gameId];
  if (pending) {
    await resumeInstall(gameId, pending);
    return;
  }
  useModalStore.getState().openInstall(gameId as GameId);
}
