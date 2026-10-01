// ------------ Cancel Job ------------
// Cancels a download or other queue job. Installs ask first, because cancelling throws away the files
// downloaded so far.
import { gameById } from "../data/games";
import type { GameId } from "../types/game";
import { useModalStore } from "../store/modalStore";
import { downloadCancel, type QueueJob } from "./ipc";
import { baseKindOf } from "./download";

export function requestCancelJob(job: QueueJob): void {
  const kind = baseKindOf(job);
  const discardsFiles = kind === "install";

  if (!discardsFiles) {
    void downloadCancel(job.id, kind);
    return;
  }

  useModalStore.getState().openConfirm({
    title: `Cancel ${gameById(job.gameId as GameId).name} install?`,
    message:
      "The files downloaded so far will be deleted and the next install starts from the beginning.",
    confirmLabel: "Cancel install",
    cancelLabel: "Keep downloading",
    danger: true,
    onConfirm: () => void downloadCancel(job.id, kind),
  });
}
