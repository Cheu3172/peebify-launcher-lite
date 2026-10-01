// ------------ Overlay Mods Session ------------
// Remembers, while the overlay stays open, how many mods were changed and need a reload in the game, and
// what the reload shortcut is.
import { create } from "zustand";
import { getLoaderSettings } from "../lib/ipc";
import { spacedAccelerator } from "../lib/hotkeys";

interface OverlayModsSession {
  pending: Record<string, number>;
  reloadKeys: Record<string, string>;
  addPending: (gameId: string, count: number) => void;
  clearPending: (gameId: string) => void;
  loadReloadKey: (gameId: string) => Promise<void>;
}

const requested = new Set<string>();

export const useOverlayModsSession = create<OverlayModsSession>((set) => ({
  pending: {},
  reloadKeys: {},
  addPending: (gameId, count) => {
    if (count <= 0) return;
    set((s) => ({ pending: { ...s.pending, [gameId]: (s.pending[gameId] ?? 0) + count } }));
  },
  clearPending: (gameId) => set((s) => ({ pending: { ...s.pending, [gameId]: 0 } })),
  loadReloadKey: async (gameId) => {
    if (requested.has(gameId)) return;
    requested.add(gameId);
    const rows = await getLoaderSettings(gameId);
    if (rows.length === 0) {
      requested.delete(gameId);
      return;
    }
    const accelerator = rows.find((row) => row.id === "reloadMods")?.accelerator?.trim();
    if (!accelerator) return;
    set((s) => ({ reloadKeys: { ...s.reloadKeys, [gameId]: spacedAccelerator(accelerator) } }));
  },
}));
