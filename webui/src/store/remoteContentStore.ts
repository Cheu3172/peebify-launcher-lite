// ------------ Remote Content Store ------------
// Things fetched from the network rather than set by the user: each game's links and its
// official wallpaper media.
import { create } from "zustand";
import { getGameLinks, getWallpaperMedia, type GameLinks, type RemoteMedia } from "../lib/ipc";

interface GameLinksState {
  links: Record<string, GameLinks>;
  hydrate: () => Promise<void>;
}

export const useGameLinksStore = create<GameLinksState>((set) => ({
  links: {},
  hydrate: async () => {
    set({ links: await getGameLinks() });
  },
}));

interface RemoteMediaState {
  media: Record<string, RemoteMedia>;
  hydrated: boolean;
  hydrate: () => Promise<void>;
  apply: (media: Record<string, RemoteMedia>) => void;
}

export const useRemoteMediaStore = create<RemoteMediaState>((set) => ({
  media: {},
  hydrated: false,
  hydrate: async () => {
    try {
      set({ media: await getWallpaperMedia() });
    } finally {
      set({ hydrated: true });
    }
  },
  apply: (media) => set({ media, hydrated: true }),
}));
