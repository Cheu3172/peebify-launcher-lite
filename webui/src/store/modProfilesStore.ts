// ------------ Mod Profiles Store ------------
// A game's mod profiles and which one is active, plus create, rename, duplicate, delete and apply.
// Overlapping refreshes are merged into one request.
import { create } from "zustand";
import {
  applyModProfile,
  createModProfile,
  deleteModProfile,
  duplicateModProfile,
  listModProfiles,
  renameModProfile,
  setModProfileMembers,
  type ApplyProfileResult,
  type ModProfile,
} from "../lib/ipc";

interface ModProfilesState {
  profiles: ModProfile[];
  activeId: string;
  loadedGameId: string | null;
  requestedGameId: string | null;
  applying: string | null;
  refresh: (gameId: string) => Promise<void>;
  refreshIfCurrent: (gameId: string) => Promise<void>;
  create: (gameId: string, name: string) => Promise<ModProfile | null>;
  rename: (gameId: string, profileId: string, name: string) => Promise<void>;
  remove: (gameId: string, profileId: string) => Promise<void>;
  duplicate: (gameId: string, profileId: string) => Promise<void>;
  apply: (gameId: string, profileId: string) => Promise<ApplyProfileResult | undefined>;
  setMembers: (
    gameId: string,
    profileId: string,
    add: string[],
    remove: string[],
  ) => Promise<boolean>;
}

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

function showing(s: ModProfilesState, gameId: string): boolean {
  return s.loadedGameId === gameId && s.requestedGameId === gameId;
}

export const useModProfilesStore = create<ModProfilesState>((set, get) => ({
  profiles: [],
  activeId: "",
  loadedGameId: null,
  requestedGameId: null,
  applying: null,

  refresh: (gameId) => {
    set({ requestedGameId: gameId });
    const wanted = () => get().requestedGameId === gameId;
    return singleFlight(gameId, wanted, async (stale) => {
      const list = await listModProfiles(gameId);
      if (stale()) return;
      set({
        profiles: list?.profiles ?? [],
        activeId: list?.activeId ?? "",
        loadedGameId: gameId,
      });
    });
  },

  refreshIfCurrent: async (gameId) => {
    if (get().requestedGameId !== gameId) return;
    await get().refresh(gameId);
  },

  create: async (gameId, name) => {
    const created = await createModProfile(gameId, name);
    if (!created) return null;
    await get().refreshIfCurrent(gameId);
    return created;
  },

  rename: async (gameId, profileId, name) => {
    const previous = get().profiles;
    set((s) => ({
      profiles: s.profiles.map((p) => (p.id === profileId ? { ...p, name } : p)),
    }));
    const ok = await renameModProfile(gameId, profileId, name);
    if (!ok) set({ profiles: previous });
  },

  remove: async (gameId, profileId) => {
    const ok = await deleteModProfile(gameId, profileId);
    if (ok) await get().refreshIfCurrent(gameId);
  },

  duplicate: async (gameId, profileId) => {
    const copy = await duplicateModProfile(gameId, profileId);
    if (copy) await get().refreshIfCurrent(gameId);
  },

  apply: async (gameId, profileId) => {
    if (get().applying) return undefined;
    set({ applying: profileId });
    try {
      const result = await applyModProfile(gameId, profileId);
      if (result && showing(get(), gameId)) set({ activeId: result.activeId });
      await get().refreshIfCurrent(gameId);
      return result;
    } finally {
      set({ applying: null });
    }
  },

  setMembers: async (gameId, profileId, add, removeIds) => {
    const updated = await setModProfileMembers(gameId, profileId, add, removeIds);
    if (!updated) return false;
    if (showing(get(), gameId)) {
      set((s) => ({
        profiles: s.profiles.map((p) =>
          p.id === profileId
            ? {
                ...p,
                ...updated,
                modCount: updated.modCount ?? updated.modIds.length,
                resolvedCount: updated.resolvedCount ?? p.resolvedCount,
              }
            : p,
        ),
      }));
    }
    await get().refreshIfCurrent(gameId);
    return true;
  },
}));
