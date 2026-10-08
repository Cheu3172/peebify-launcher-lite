// ------------ Install Preload ------------
// Loads everything the install dialog shows (default folder, disk space, sizes, quality sizes, voice packs) ahead of
// time and keeps it for a minute, so the dialog opens at its final size instead of filling in and resizing.
import type { GameId } from "../types/game";
import { gameById, isManaged } from "../data/games";
import { useCustomizationStore } from "../store/customizationStore";
import { log } from "./log";
import {
  getContentPacks,
  getDefaultInstallPath,
  getDiskSpace,
  getInstallPreview,
  getResourceQuality,
  type ContentPack,
  type InstallPreview,
  type ResourceQualityInfo,
} from "./ipc";

export interface InstallSnapshot {
  path: string;
  maxRootLength: number;
  folderName: string;
  free: number;
  total: number;
  preview: InstallPreview | null;
  previewFailed: boolean;
  previewError: string | null;
  packs: ContentPack[] | null;
  packsFailed: boolean;
  quality: ResourceQualityInfo | null;
}

const FRESH_MS = 60_000;
const FAILED_MS = 5_000;
const OPEN_WAIT_MS = 1000;

interface Entry {
  at: number;
  ttl: number;
  promise: Promise<InstallSnapshot>;
  value: InstallSnapshot | null;
}

const entries = new Map<GameId, Entry>();

async function build(gameId: GameId): Promise<InstallSnapshot> {
  const game = gameById(gameId);
  const customization = useCustomizationStore.getState();
  const hydrated = customization.hydrated ? Promise.resolve() : customization.hydrate();
  void import("./motionFeatures").catch((e) => log.warn("[install] motion preload failed:", e));

  const local = (async () => {
    const def = await getDefaultInstallPath(gameId);
    const disk = await getDiskSpace(def?.path);
    return { def, disk };
  })();
  const quality = game.resourceQualityChoice
    ? getResourceQuality(gameId, true).catch(() => null)
    : Promise.resolve(null);
  const remote = (async () => {
    const { preview, error } = await getInstallPreview(gameId);
    const usable = preview && !preview.notSupported ? preview : null;
    let packs: ContentPack[] | null = null;
    let packsFailed = false;
    if (game.contentPackChoice) {
      const r = await getContentPacks(gameId);
      if (r.supported) {
        if (r.error) packsFailed = true;
        else packs = r.packs;
      }
    }
    return { usable, error, packs, packsFailed };
  })();

  const [{ def, disk }, q, r] = await Promise.all([local, quality, remote, hydrated]);
  return {
    path: def?.path ?? "",
    maxRootLength: def?.maxRootLength ?? 0,
    folderName: def?.folderName ?? "",
    free: disk?.free ?? 0,
    total: disk?.total ?? 0,
    preview: r.usable,
    previewFailed: !r.usable,
    previewError: r.usable ? null : r.error,
    packs: r.packs,
    packsFailed: r.packsFailed,
    quality: q,
  };
}

export function preloadInstall(gameId: GameId, { force = false } = {}): Promise<InstallSnapshot> {
  if (!isManaged(gameId)) return Promise.reject(new Error("not managed"));
  const existing = entries.get(gameId);
  if (existing && !force && Date.now() - existing.at < existing.ttl) return existing.promise;
  const entry: Entry = { at: Date.now(), ttl: FRESH_MS, promise: null as unknown as Promise<InstallSnapshot>, value: null };
  entry.promise = build(gameId).then(
    (snap) => {
      entry.value = snap;
      // A failed lookup is only remembered briefly so the next open tries again.
      if (snap.previewFailed || snap.packsFailed) entry.ttl = FAILED_MS;
      return snap;
    },
    (e) => {
      entries.delete(gameId);
      throw e;
    },
  );
  entries.set(gameId, entry);
  return entry.promise;
}

export function warmInstall(gameId: GameId): void {
  if (isManaged(gameId)) void preloadInstall(gameId).catch((e) => log.warn("[install] preload failed:", e));
}

export function peekInstall(gameId: GameId): InstallSnapshot | null {
  const entry = entries.get(gameId);
  return entry?.value && Date.now() - entry.at < entry.ttl ? entry.value : null;
}

// Drop the cached answer after the user changes something it depended on (voice pack, quality, packs), then fetch
// the new one in the background so the next open is still instant.
export function refreshInstall(gameId: GameId): void {
  entries.delete(gameId);
  warmInstall(gameId);
}

// Resolves once the dialog can open without anything popping in: either everything is loaded, or the wait ran out
// and the dialog will fill in the rest itself.
export async function whenInstallReady(gameId: GameId): Promise<void> {
  if (!isManaged(gameId) || peekInstall(gameId)) return;
  let timer: number | undefined;
  const timeout = new Promise<void>((resolve) => {
    timer = window.setTimeout(resolve, OPEN_WAIT_MS);
  });
  try {
    await Promise.race([preloadInstall(gameId).then(() => undefined), timeout]);
  } catch {
    // The dialog shows its own unavailable state.
  } finally {
    window.clearTimeout(timer);
  }
}
