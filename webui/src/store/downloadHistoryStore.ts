// ------------ Download History ------------
// Remembers the last few finished, cancelled or failed jobs so the Downloads page can still show them.
// Kept in the browser's local storage.
import { create } from "zustand";
import type { QueueJob } from "../lib/ipc";

export interface HistoryEntry {
  key: string;
  gameId: string;
  kind: string;
  phase: "done" | "cancelled" | "error";
  error: string | null;
  message?: string | null;
  finishedAt: number;
}

const STORAGE_KEY = "peebify.downloadHistory";
const MAX_ENTRIES = 20;
const PHASES: readonly HistoryEntry["phase"][] = ["done", "cancelled", "error"];

function load(): HistoryEntry[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(parsed)) return [];
    return (parsed as (HistoryEntry & { total?: unknown })[]).map(({ total: _total, ...e }) =>
      PHASES.includes(e.phase) ? e : { ...e, phase: "error" as const },
    );
  } catch {
    return [];
  }
}

function save(entries: HistoryEntry[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
  }
}

interface HistoryState {
  entries: HistoryEntry[];
  add: (job: QueueJob) => void;
  clear: () => void;
}

export const useDownloadHistoryStore = create<HistoryState>((set) => ({
  entries: load(),
  add: (job) =>
    set((s) => {
      const entry: HistoryEntry = {
        key: `${job.id}-${Date.now()}`,
        gameId: job.gameId,
        kind: job.kind,
        phase: job.phase === "done" || job.phase === "cancelled" ? job.phase : "error",
        error: job.error,
        message: job.message ?? null,
        finishedAt: Date.now(),
      };
      const entries = [entry, ...s.entries].slice(0, MAX_ENTRIES);
      save(entries);
      return { entries };
    }),
  clear: () => {
    save([]);
    set({ entries: [] });
  },
}));
