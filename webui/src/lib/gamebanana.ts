// ------------ GameBanana Client ------------
// Talks to the backend to browse GameBanana: the mod feed, categories, mod pages, update checks, and
// installing a mod from a link.
import { isTauri } from "./tauri";
import { rpc, rpcAction, rpcRead } from "./rpc";

export interface GameBananaMod {
  id: number;
  name: string;
  profileUrl: string | null;
  version: string | null;
  updatedAt: number | null;
  likes: number;
  views: number;
  category: string | null;
  submitter: string | null;
  thumbnailUrl: string | null;
}

export interface GameBananaFile {
  id: number;
  fileName: string;
  sizeBytes: number;
  version: string | null;
  description: string | null;
  avResult: string;
  installable: boolean;
  dateAdded?: number | null;
  family?: string;
}

export type GameBananaSort =
  | "relevance"
  | "new"
  | "updated"
  | "likes"
  | "views"
  | "downloads";

export interface GameBananaCategory {
  id: number;
  name: string;
}

export interface GameBananaImage {
  url: string;
  thumbUrl: string;
  heroUrl?: string;
}

export interface GameBananaProfile {
  id: number;
  name: string;
  profileUrl: string | null;
  version: string | null;
  likes: number;
  views: number;
  updatedAt: number | null;
  submitter: string | null;
  category: string | null;
  tagline: string | null;
  description: string;
  images: GameBananaImage[];
  files: GameBananaFile[];
  filesError?: string | null;
}

export async function gameBananaFeed(
  gameId: string,
  page: number,
  sort: GameBananaSort,
  query: string,
  categoryId: number | null,
  perPage: number,
): Promise<
  | {
      mods: GameBananaMod[];
      totalPages: number;
      hasMore: boolean;
      matches: number;
      totalMatches: number;
      truncated: boolean;
    }
  | { error: string }
> {
  let r:
    | {
        success?: boolean;
        error?: string;
        mods?: GameBananaMod[];
        totalPages?: number;
        hasMore?: boolean;
        matches?: number;
        totalMatches?: number;
        truncated?: boolean;
      }
    | undefined;
  if (!isTauri()) return { error: "" };
  try {
    r = await rpc("gamebanana-feed", gameId, page, sort, query, categoryId ?? 0, perPage);
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }
  if (!r?.success) return { error: r?.error ?? "" };
  const totalPages = r.totalPages ?? 1;
  return {
    mods: r.mods ?? [],
    totalPages,
    hasMore: r.hasMore ?? page < totalPages,
    matches: r.matches ?? 0,
    totalMatches: r.totalMatches ?? 0,
    truncated: r.truncated === true,
  };
}

export function feedErrorText(error: string): { title: string; text: string } {
  if (error === "" || /^Request error|timed out|error sending request/i.test(error)) {
    return {
      title: "Couldn't reach GameBanana",
      text: "Check your internet connection, then try again.",
    };
  }
  if (/HTTP (429|5\d\d)|Invalid JSON|empty response/.test(error)) {
    return {
      title: "Couldn't load mods from GameBanana",
      text: "GameBanana is busy or down right now. Try again in a minute.",
    };
  }
  return { title: "Couldn't load mods from GameBanana", text: error };
}

export async function gameBananaCategories(gameId: string): Promise<GameBananaCategory[]> {
  const r = await rpcRead<{ success?: boolean; categories?: GameBananaCategory[] }>(
    "gamebanana-categories",
    gameId,
  );
  return r?.success ? (r.categories ?? []) : [];
}

export async function gameBananaModProfile(
  modId: number,
): Promise<GameBananaProfile | undefined> {
  const r = await rpcAction<{ success?: boolean } & GameBananaProfile>(
    "Load mod",
    "gamebanana-mod-profile",
    modId,
  );
  return r?.success ? r : undefined;
}

export async function backfillModThumbnails(gameId: string): Promise<number> {
  if (!isTauri()) return 0;
  const r = await rpcRead<{ updated?: number }>("gamebanana-backfill-thumbnails", gameId);
  return r?.updated ?? 0;
}

export interface ModUpdate {
  modId: string;
  name: string;
  fileId: number;
  version: string | null;
}

export async function checkModUpdates(
  gameId: string,
  force: boolean,
): Promise<{ updates: ModUpdate[]; cached: boolean } | undefined> {
  if (!isTauri()) return { updates: [], cached: false };
  const r = await rpcRead<{ success?: boolean; updates?: ModUpdate[]; cached?: boolean }>(
    "check-mod-updates",
    gameId,
    force,
  );
  if (!r || r.success === false) return undefined;
  return { updates: r.updates ?? [], cached: r.cached === true };
}

export async function updateGameBananaMod(
  gameId: string,
  modId: string,
  fileId?: number,
): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; folderName?: string }>(
    "Update mod",
    "update-gamebanana-mod",
    gameId,
    modId,
    fileId ?? null,
  );
  return r?.success ? r.folderName : undefined;
}

export function installLabel(
  mod: Pick<GameBananaProfile, "name" | "files">,
  file: Pick<GameBananaFile, "fileName">,
): string {
  if (mod.files.length < 2) return mod.name;
  const base = file.fileName.replace(/\.[^.]+$/, "").trim();
  return base ? `${mod.name} (${base})` : mod.name;
}

export async function installGameBananaMod(
  gameId: string,
  modId: number,
  fileId: number,
  modName: string,
  thumbnailUrl: string | null,
): Promise<{ installed: string[]; alreadyInstalled: boolean } | { error: string | null }> {
  try {
    const r = await rpc<
      | {
          success?: boolean;
          installed?: string[];
          alreadyInstalled?: boolean;
          error?: string;
          cancelled?: boolean;
        }
      | undefined
    >("install-gamebanana-mod", gameId, modId, fileId, modName, thumbnailUrl);
    if (r?.success) {
      return { installed: r.installed ?? [], alreadyInstalled: r.alreadyInstalled === true };
    }
    if (r?.cancelled) return { error: null };
    return { error: r?.error || "The install did not finish." };
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }
}

export async function cancelGameBananaInstall(gameId: string): Promise<void> {
  await rpcAction("Cancel mod download", "cancel-gamebanana-install", gameId);
}
