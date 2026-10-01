// ------------ Speed History Store ------------
// Keeps the last minute of download speed per job, sampled every second, to draw the speed graph.
import { useEffect } from "react";
import { create } from "zustand";
import { useQueueStore } from "./queueStore";
import { isActiveJob } from "../lib/download";

export const SPEED_WINDOW = 60;
const SAMPLE_MS = 1000;
export const NO_SAMPLES: number[] = [];

interface SpeedHistoryState {
  byJob: Record<string, number[]>;
  sample: (speeds: Record<string, number>) => void;
}

export const useSpeedHistoryStore = create<SpeedHistoryState>((set) => ({
  byJob: {},
  sample: (speeds) =>
    set((s) => {
      const ids = Object.keys(speeds);
      if (ids.length === 0) {
        return Object.keys(s.byJob).length === 0 ? s : { byJob: {} };
      }
      const byJob: Record<string, number[]> = {};
      for (const id of ids) {
        byJob[id] = [...(s.byJob[id] ?? []), speeds[id]].slice(-SPEED_WINDOW);
      }
      return { byJob };
    }),
}));

function sampleActiveJobs(): void {
  const speeds: Record<string, number> = {};
  for (const job of useQueueStore.getState().jobs) {
    if (!isActiveJob(job) || job.phase === "queued") continue;
    speeds[job.id] = job.paused ? 0 : Math.max(0, job.speed);
  }
  useSpeedHistoryStore.getState().sample(speeds);
}

export function useSpeedSampler(): void {
  useEffect(() => {
    let id: ReturnType<typeof setInterval> | undefined;
    const sync = (active: boolean) => {
      if (active && id === undefined) {
        id = setInterval(sampleActiveJobs, SAMPLE_MS);
      } else if (!active && id !== undefined) {
        clearInterval(id);
        id = undefined;
        useSpeedHistoryStore.getState().sample({});
      }
    };
    sync(useQueueStore.getState().jobs.some(isActiveJob));
    const unsub = useQueueStore.subscribe((s, p) => {
      if (s.jobs !== p.jobs) sync(s.jobs.some(isActiveJob));
    });
    return () => {
      unsub();
      if (id !== undefined) clearInterval(id);
    };
  }, []);
}
