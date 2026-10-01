// ------------ Captures Store ------------
// The list of screenshots and clips for the gallery, with its filters, selection mode and deleting.
import { create } from "zustand";
import { deleteCapture, listCaptures, type MediaItem } from "../lib/ipc";
import { useNotificationStore } from "./notificationStore";

export type KindFilter = "all" | "screenshot" | "clip";

interface MediaState {
  items: MediaItem[];
  folder: string;
  hydrated: boolean;
  loading: boolean;
  error: string | null;
  kind: KindFilter;
  gameId: string;
  selectMode: boolean;
  selected: Record<string, true>;
  setKind: (kind: KindFilter) => void;
  setGame: (gameId: string) => void;
  load: () => Promise<void>;
  refresh: () => void;
  toggleSelected: (path: string) => void;
  selectMany: (paths: string[]) => void;
  clearSelection: () => void;
  setSelectMode: (on: boolean) => void;
  remove: (paths: string[]) => Promise<number>;
}

let listToken = 0;
let refreshTimer: ReturnType<typeof setTimeout> | undefined;
const REFRESH_DELAY_MS = 150;
const DELETE_CONCURRENCY = 4;

export const useMediaStore = create<MediaState>((set, get) => ({
  items: [],
  folder: "",
  hydrated: false,
  loading: false,
  error: null,
  kind: "all",
  gameId: "all",
  selectMode: false,
  selected: {},

  setKind: (kind) => set({ kind, selected: {} }),
  setGame: (gameId) => set({ gameId, selected: {} }),

  load: async () => {
    const token = ++listToken;
    set({ loading: true, error: null });
    try {
      const res = await listCaptures();
      if (token !== listToken) return;
      if (!res.ok) {
        set({ error: res.error ?? "Could not read your captures.", hydrated: true });
        return;
      }
      const known = new Set(res.items.map((i) => i.path));
      const selected: Record<string, true> = {};
      for (const path of Object.keys(get().selected)) if (known.has(path)) selected[path] = true;
      set({ items: res.items, folder: res.folder, hydrated: true, selected });
    } finally {
      if (token === listToken) set({ loading: false });
    }
  },

  refresh: () => {
    clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => void get().load(), REFRESH_DELAY_MS);
  },

  toggleSelected: (path) =>
    set((s) => {
      const selected = { ...s.selected };
      if (selected[path]) delete selected[path];
      else selected[path] = true;
      return { selected };
    }),
  selectMany: (paths) =>
    set((s) => {
      const selected = { ...s.selected };
      for (const path of paths) selected[path] = true;
      return { selected };
    }),
  clearSelection: () => set({ selected: {} }),
  setSelectMode: (on) => set({ selectMode: on, selected: on ? get().selected : {} }),

  remove: async (paths) => {
    let deleted = 0;
    let error: string | undefined;
    let next = 0;
    const worker = async () => {
      while (next < paths.length) {
        const res = await deleteCapture(paths[next++]);
        if (res.ok) deleted += 1;
        else error ??= res.error;
      }
    };
    await Promise.all(
      Array.from({ length: Math.min(DELETE_CONCURRENCY, paths.length) }, worker),
    );
    const failed = paths.length - deleted;
    if (failed > 0) {
      useNotificationStore.getState().push({
        type: deleted > 0 ? "warning" : "error",
        title: deleted > 0 ? "Some captures were not deleted" : "Delete failed",
        text:
          deleted > 0
            ? `${deleted} deleted, ${failed} failed. ${error ?? "Check the logs for details."}`
            : (error ?? "Those captures could not be deleted."),
      });
    }
    set((s) => {
      const selected = { ...s.selected };
      for (const path of paths) delete selected[path];
      return { selected };
    });
    await get().load();
    return deleted;
  },
}));
