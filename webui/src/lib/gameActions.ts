// ------------ Game Actions ------------
// The shared logic behind a game's buttons: update, repair, move, uninstall, locate an existing install, and
// pick the download quality. Used by the home page and by settings.
import { useCallback, useEffect, useState } from "react";
import { gameById, isManaged, supportsQuickRepair } from "../data/games";
import type { GameId } from "../types/game";
import { useGamesStore } from "../store/gamesStore";
import { useCustomizationStore } from "../store/customizationStore";
import { useQueueStore } from "../store/queueStore";
import { useNotificationStore } from "../store/notificationStore";
import { useModalStore } from "../store/modalStore";
import { isActiveJob } from "./download";
import { log } from "./log";
import {
  checkGameUpdate,
  DEFAULT_RESOURCE_QUALITY,
  downloadEnqueue,
  getModsStatus,
  getResourceQuality,
  getSteamInstall,
  listMods,
  locateGameInstall,
  moveGame,
  repairGame,
  RESOURCE_QUALITY_OPTIONS,
  uninstallGame,
  type ResourceQuality,
  type ResourceQualityInfo,
  type SteamInstall,
} from "./ipc";

export const qualityLabel = (q: ResourceQuality): string =>
  RESOURCE_QUALITY_OPTIONS.find((o) => o.value === q)?.label ?? q;

export function useResourceQuality(
  gameId: GameId,
  { installed, steamCopy, jobActive }: { installed: boolean; steamCopy: boolean; jobActive: boolean },
  { sizes: withSizes = false }: { sizes?: boolean } = {},
) {
  const game = gameById(gameId);
  const quality =
    useCustomizationStore((s) => s.resourceQuality[gameId]) ?? DEFAULT_RESOURCE_QUALITY;
  const setResourceQuality = useCustomizationStore((s) => s.setResourceQuality);
  const push = useNotificationStore((s) => s.push);
  const [info, setInfo] = useState<{ gameId: GameId; data: ResourceQualityInfo } | null>(null);
  useEffect(() => {
    if (!game.resourceQualityChoice) return;
    let alive = true;
    void getResourceQuality(gameId, withSizes).then((data) => {
      if (alive) setInfo({ gameId, data });
    });
    return () => {
      alive = false;
    };
  }, [gameId, installed, jobActive, game.resourceQualityChoice, withSizes]);

  const data = info?.gameId === gameId ? info.data : null;
  const onDisk = data?.installed ?? [];
  const pending =
    installed && isManaged(gameId) && !steamCopy && onDisk.length > 0 && !onDisk.includes(quality);

  const apply = useCallback(() => {
    const from = onDisk.map(qualityLabel).join(" and ");
    void downloadEnqueue(gameId, () =>
      push({
        title: `Switching ${game.name} to ${qualityLabel(quality)}`,
        text: `Downloading the ${qualityLabel(quality)} files and removing the ${from} ones. Track it on Downloads.`,
      }),
    );
  }, [gameId, game.name, quality, onDisk, push]);

  return {
    quality,
    setQuality: (q: ResourceQuality) => setResourceQuality(gameId, q),
    installedLabel: onDisk.map(qualityLabel).join(" + "),
    loaded: data !== null,
    sizes: data?.sizes ?? null,
    pending,
    apply,
  };
}

export function useModsForceDx11(gameId: GameId): boolean {
  const [state, setState] = useState<{ gameId: GameId; force: boolean } | null>(null);
  useEffect(() => {
    let alive = true;
    void getModsStatus(gameId).then((s) => {
      if (alive) setState({ gameId, force: s.masterEnabled && s.gameEnabled && s.forcesDx11 });
    });
    return () => {
      alive = false;
    };
  }, [gameId]);
  return state?.gameId === gameId && state.force;
}

function useSteamInstall(gameId: GameId, installed: boolean) {
  const [state, setState] = useState<{ gameId: GameId; info: SteamInstall } | null>(null);
  useEffect(() => {
    let alive = true;
    void getSteamInstall(gameId).then((info) => {
      if (alive) setState({ gameId, info });
    });
    return () => {
      alive = false;
    };
  }, [gameId, installed]);
  const setSteam = useCallback((info: SteamInstall) => setState({ gameId, info }), [gameId]);
  return [state?.gameId === gameId ? state.info : null, setSteam] as const;
}

async function countMods(gameId: GameId): Promise<number> {
  try {
    const status = await getModsStatus(gameId);
    return status.supportsMods ? (await listMods(gameId)).length : 0;
  } catch (e) {
    log.warn(`Could not count the mods for ${gameId}:`, e);
    return 0;
  }
}

function uninstallMessage(name: string, steamCopy: boolean, modCount: number): string {
  const mods = modCount === 1 ? "mod" : `${modCount} mods`;
  const kept = "Your launcher settings and playtime history are kept.";
  if (steamCopy) {
    const removed = modCount
      ? ` The ${mods} you installed for it ${modCount === 1 ? "is" : "are"} removed too.`
      : "";
    return `Steam will ask you to confirm the uninstall, and Peebify removes ${name} from its library.${removed} ${kept}`;
  }
  return modCount
    ? `This permanently deletes the installed game files from your disk, along with the ${mods} you installed for it. ${kept}`
    : `This permanently deletes the installed game files from your disk. ${kept}`;
}

export const JOB_ACTIVE_BLOCKER =
  "Finish or cancel this game's current download, repair or move first.";

export const LINKED_UNCHECKED_TEXT =
  "Linked your existing installation. Run a full repair from Settings if it has problems.";

const SKIP_LINKED_CHECK: ReadonlySet<GameId> = new Set<GameId>(["bd2", "re1999"]);

export function checkLinkedInstall(gameId: GameId): boolean {
  if (SKIP_LINKED_CHECK.has(gameId)) return false;
  void repairGame(gameId, supportsQuickRepair(gameId));
  return true;
}

export function useGameActions(gameId: GameId) {
  const game = gameById(gameId);
  const managed = isManaged(gameId);
  const installed = useGamesStore((s) => s.installed.includes(gameId));
  const hasPending = useGamesStore((s) => !!s.pendingInstalls[gameId]);
  const partial = !installed && hasPending;
  const running = useGamesStore((s) => s.runningGames.includes(gameId));
  const jobActive = useQueueStore((s) => s.jobs.some((j) => j.gameId === gameId && isActiveJob(j)));
  const push = useNotificationStore((s) => s.push);
  const toast = useNotificationStore((s) => s.toast);
  const openConfirm = useModalStore((s) => s.openConfirm);
  const [steam, setSteam] = useSteamInstall(gameId, installed);
  const steamCopy = installed && !!steam?.isSteamInstall;

  const uninstallBlocker = running
    ? `Close ${game.name} first.`
    : jobActive
      ? JOB_ACTIVE_BLOCKER
      : null;

  const checkUpdates = async () => {
    toast({ title: `Checking ${game.name}`, text: "Looking for updates…" });
    const result = await checkGameUpdate(gameId, true);
    if (!result.ok) {
      push({ type: "warning", title: "Couldn't check for updates", text: result.error });
      return;
    }
    const { available, steamManaged } = result;
    useGamesStore.getState().setUpdate(gameId, available, steamManaged, result);
    if (!available) {
      toast({ type: "success", title: "Up to date", text: `${game.name} is up to date.` });
      return;
    }
    push({
      type: "info",
      title: "Update available",
      text: steamManaged
        ? `${game.name} has a new version ready. Steam installs this one, so use Update in Steam.`
        : `${game.name} has a new version ready.`,
    });
  };

  const quickRepair = () => {
    void repairGame(gameId, true).then((queued) => {
      if (!queued) return;
      push({
        title: `Quick repair for ${game.name}`,
        text: "Checking file sizes against the local manifest.",
      });
    });
  };

  const confirmFullRepair = () =>
    openConfirm({
      title: `Full repair for ${game.name}`,
      message:
        "This hashes every installed game file and re-downloads any that are missing or corrupted. It is far more thorough than a quick repair, and can take a while.",
      confirmLabel: "Full repair",
      onConfirm: () => {
        void repairGame(gameId, false).then((queued) => {
          if (!queued) return;
          push({
            title: `Repairing ${game.name}`,
            text: "Verifying game files. Track it on Downloads.",
          });
        });
      },
    });

  const locate = async () => {
    const r = await locateGameInstall(gameId);
    if (r.ok) {
      void useCustomizationStore.getState().hydrate();
      const ids = useGamesStore.getState().installed;
      if (!ids.includes(gameId)) useGamesStore.getState().setInstalled([...ids, gameId]);
      const located = await getSteamInstall(gameId);
      setSteam(located);
      const linked = managed && !located.isSteamInstall;
      const checking = linked && checkLinkedInstall(gameId);
      push({
        type: "success",
        title: `${game.name} located`,
        text: checking
          ? "Verifying your installation. Track it on the Downloads page."
          : linked
            ? LINKED_UNCHECKED_TEXT
            : "Linked your existing installation.",
      });
    } else if (r.error) {
      push({ type: "warning", title: `Couldn't locate ${game.name}`, text: r.error });
    }
  };

  const confirmMove = () =>
    openConfirm({
      title: `Move ${game.name}`,
      message:
        "Pick a new folder next. The game files are moved there and the old folder is removed once it's done.",
      confirmLabel: "Choose folder",
      onConfirm: () => void moveGame(gameId),
    });

  const confirmDiscard = () =>
    openConfirm({
      title: `Discard the ${game.name} download?`,
      message:
        "This deletes the files downloaded so far by the unfinished install. You can start the install again later.",
      confirmLabel: "Discard",
      danger: true,
      onConfirm: () => {
        push({ title: `Discarding ${game.name}`, text: "Removing the unfinished download." });
        void uninstallGame(gameId).then((ok) => {
          if (!ok) return;
          const store = useGamesStore.getState();
          store.setPendingInstalls(
            Object.fromEntries(Object.entries(store.pendingInstalls).filter(([id]) => id !== gameId)),
          );
        });
      },
    });

  const confirmUninstall = async () => {
    if (partial) {
      confirmDiscard();
      return;
    }
    const modCount = await countMods(gameId);
    openConfirm({
      title: `Uninstall ${game.name}?`,
      message: uninstallMessage(game.name, steamCopy, modCount),
      confirmLabel: "Uninstall",
      danger: true,
      onConfirm: () => {
        push({
          title: `Uninstalling ${game.name}`,
          text: steamCopy ? "Handing the uninstall to Steam." : "Removing game files.",
        });
        void uninstallGame(gameId);
      },
    });
  };

  return {
    game,
    managed,
    installed,
    partial,
    running,
    jobActive,
    steam,
    setSteam,
    steamCopy,
    uninstallBlocker,
    checkUpdates,
    quickRepair,
    confirmFullRepair,
    locate,
    confirmMove,
    confirmUninstall,
  };
}
