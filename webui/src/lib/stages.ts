// ------------ Job Stages ------------
// Splits a download, repair, move or verify job into named stages (like Download then Finalize) and works
// out which stage and how much overall progress a job is at.
import type { QueueJob } from "./ipc";

export interface JobStage {
  id: string;
  label: string;
  weight: number;
}

const DOWNLOAD_STAGES: JobStage[] = [
  { id: "download", label: "Download", weight: 72 },
  { id: "finalize", label: "Finalize", weight: 28 },
];

const REPAIR_STAGES: JobStage[] = [
  { id: "scan", label: "Scan", weight: 16 },
  { id: "verify", label: "Verify", weight: 46 },
  { id: "replace", label: "Replace", weight: 38 },
];

const MOVE_STAGES: JobStage[] = [{ id: "transfer", label: "Transfer", weight: 100 }];

const VERIFY_STAGES: JobStage[] = [{ id: "verify", label: "Verify", weight: 100 }];

const REMOVE_STAGES: JobStage[] = [{ id: "remove", label: "Remove", weight: 100 }];

export function stagesFor(kind: string): JobStage[] {
  if (kind === "repair") return REPAIR_STAGES;
  if (kind === "move") return MOVE_STAGES;
  if (kind === "verify") return VERIFY_STAGES;
  if (kind === "uninstall") return REMOVE_STAGES;
  return DOWNLOAD_STAGES;
}

const PHASE_STAGES: Record<string, string[]> = {
  scanning: ["check", "verify"],
  downloading: ["download", "transfer"],
  repairing: ["replace", "finalize"],
  verifying: ["verify", "finalize"],
  validating: ["verify", "finalize"],
  extracting: ["finalize", "replace"],
  installing: ["finalize", "replace"],
  moving: ["transfer", "finalize"],
};

const PREPARING_STAGES = ["check", ...PHASE_STAGES.downloading];

// A download still runs its pre-download check, but the bar has no stage for it: the check
// sits on the first stage at 0% and adds nothing until the real download starts.
function isHiddenCheck(job: QueueJob, stages: JobStage[]): boolean {
  return job.phase === "scanning" && stages === DOWNLOAD_STAGES;
}

export function stageIndexFor(job: QueueJob, stages: JobStage[]): number {
  if (job.phase === "queued") return -1;
  if (job.phase === "done") return stages.length;
  const preparing = job.phase === "downloading" && job.total <= 0;
  for (const id of preparing ? PREPARING_STAGES : (PHASE_STAGES[job.phase] ?? [])) {
    const index = stages.findIndex((s) => s.id === id);
    if (index !== -1) return index;
  }
  return 0;
}

export function stagePercent(job: QueueJob, stages?: JobStage[]): number {
  if (stages && isHiddenCheck(job, stages)) return 0;
  return Math.max(0, Math.min(100, job.percent));
}

export function overallPercent(job: QueueJob, kind: string): number {
  const stages = stagesFor(kind);
  const index = stageIndexFor(job, stages);
  if (index < 0) return 0;
  const total = stages.reduce((sum, s) => sum + s.weight, 0);
  if (total <= 0) return 0;
  if (index >= stages.length) return 100;
  let done = 0;
  for (let i = 0; i < index; i += 1) done += stages[i].weight;
  done += (stages[index].weight * stagePercent(job, stages)) / 100;
  return (done / total) * 100;
}
