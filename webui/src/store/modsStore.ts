// ------------ Mods Store ------------
// The installed mods for the current game, the master mods switch, install progress, and update checks.
// Shared by the Mods page and the overlay.
import { create } from "zustand";
import {
  getModsMasterEnabled,
  getModsStatus,
  type ModEntry,
  type ModProgress,
  type ModsStatus,
} from "../lib/ipc";
import { backfillModThumbnails, checkModUpdates, type ModUpdate } from "../lib/gamebanana";
import { rpcRead } from "../lib/rpc";

interface ModsState {
  status: ModsStatus | null;
  masterEnabled: boolean;
  masterHydrated: boolean;
  mods: ModEntry[];
  modsError: boolean;
  loadedGameId: string | null;
  requestedGameId: string | null;
  progress: ModProgress | null;
  updates: ModUpdate[];
  updatesGameId: string | null;
  checkingUpdates: boolean;
  installing: Set<string>;
  refresh: (gameId: string) => Promise<void>;
  refreshIfCurrent: (gameId: string) => Promise<void>;
  beginInstall: (key: string) => boolean;
  endInstall: (key: string) => void;
  checkUpdates: (gameId: string, force: boolean) => Promise<ModUpdate[] | undefined>;
  dropUpdate: (modId: string) => void;
  hydrateMasterEnabled: () => Promise<void>;
  setMasterEnabled: (enabled: boolean) => void;
  setProgress: (progress: ModProgress | null) => void;
  setModsEnabledLocal: (folders: string[], enabled: boolean) => void;
  patchModLocal: (modId: string, patch: Partial<Pick<ModEntry, "enabled" | "folderName">>) => void;
}

let updateToken = 0;
const backfilledGames = new Set<string>();
const autoCheckedGames = new Set<string>();

interface Flight {
  done: Promise<void>;
  again: Promise<void> | null;
}

const flights = new Map<string, Flight>();

function singleFlight(
  gameId: string,
  stillWanted: () => boolean,
  run: (stale: () => boolean) => Promise<void>,
): Promise<void> {
  const current = flights.get(gameId);
  if (current) {
    const next = async () => {
      if (stillWanted()) await singleFlight(gameId, stillWanted, run);
    };
    return (current.again ??= current.done.then(next, next));
  }
  const flight = { again: null } as Flight;
  flight.done = run(() => flight.again !== null || !stillWanted()).finally(() =>
    flights.delete(gameId),
  );
  flights.set(gameId, flight);
  return flight.done.then(() => flight.again ?? undefined);
}

async function readMods(gameId: string): Promise<ModEntry[] | undefined> {
  const r = await rpcRead<{ mods?: ModEntry[] }>("list-mods", gameId);
  return r ? (r.mods ?? []) : undefined;
}

export function modInstallKey(gameId: string, gbModId: number, fileId: number): string {
  return `${gameId}:${gbModId}:${fileId}`;
}

export function activeInstallFor(installing: Set<string>, gameId: string): string | null {
  for (const key of installing) if (key.startsWith(`${gameId}:`)) return key;
  return null;
}

export const useModsStore = create<ModsState>((set, get) => ({
  status: null,
  masterEnabled: false,
  masterHydrated: false,
  mods: [],
  modsError: false,
  loadedGameId: null,
  requestedGameId: null,
  progress: null,
  updates: [],
  updatesGameId: null,
  checkingUpdates: false,
  installing: new Set(),
  beginInstall: (key) => {
    if (get().installing.has(key)) return false;
    set((s) => ({ installing: new Set(s.installing).add(key) }));
    return true;
  },
  endInstall: (key) =>
    set((s) => {
      const installing = new Set(s.installing);
      installing.delete(key);
      return { installing };
    }),
  refresh: (gameId) => {
    set({ requestedGameId: gameId });
    const wanted = () => get().requestedGameId === gameId;
    return singleFlight(gameId, wanted, async (stale) => {
      const status = await getModsStatus(gameId);
      const listed = status.supportsMods && status.masterEnabled ? await readMods(gameId) : [];
      if (stale()) return;
      const mods = listed ?? [];
      set({
        status,
        masterEnabled: status.masterEnabled,
        masterHydrated: true,
        mods,
        modsError: listed === undefined,
        loadedGameId: gameId,
      });

      const needsArt = mods.some((m) => m.source.kind === "gamebanana" && !m.thumbnailUrl);
      if (needsArt && !backfilledGames.has(gameId)) {
        backfilledGames.add(gameId);
        void backfillModThumbnails(gameId).then((updated) => {
          if (updated > 0) void get().refreshIfCurrent(gameId);
        });
      }

      const fromGameBanana = mods.some((m) => m.source.kind === "gamebanana");
      if (fromGameBanana && !autoCheckedGames.has(gameId)) {
        autoCheckedGames.add(gameId);
        void get().checkUpdates(gameId, false);
      }
    });
  },
  refreshIfCurrent: async (gameId) => {
    if (get().requestedGameId !== gameId) return;
    await get().refresh(gameId);
  },
  checkUpdates: async (gameId, force) => {
    const token = ++updateToken;
    set({ checkingUpdates: true });
    const result = await checkModUpdates(gameId, force);
    if (token !== updateToken) return result?.updates;
    set({
      checkingUpdates: false,
      ...(result
        ? { updates: result.updates, updatesGameId: gameId }
        : {}),
    });
    return result?.updates;
  },
  dropUpdate: (modId) => set((s) => ({ updates: s.updates.filter((u) => u.modId !== modId) })),
  hydrateMasterEnabled: async () => {
    try {
      set({ masterEnabled: await getModsMasterEnabled() });
    } finally {
      set({ masterHydrated: true });
    }
  },
  setMasterEnabled: (enabled) => set({ masterEnabled: enabled, masterHydrated: true }),
  setProgress: (progress) => set({ progress }),
  setModsEnabledLocal: (folders, enabled) =>
    set((s) => ({
      mods: s.mods.map((m) => (folders.includes(m.folderName) ? { ...m, enabled } : m)),
    })),
  patchModLocal: (modId, patch) =>
    set((s) => ({
      mods: s.mods.map((m) => (m.modId === modId ? { ...m, ...patch } : m)),
    })),
}));
