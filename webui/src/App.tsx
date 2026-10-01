// ------------ Launcher Window ------------
// The main launcher window. Hooks up every backend event (games starting and stopping, the download queue,
// updates, network, wallpapers), shows the sidebar, wallpaper and current page, and pops toasts and modals.
import { Suspense, lazy, useEffect } from "react";
import { useUiStore } from "./store/uiStore";
import { useGamesStore } from "./store/gamesStore";
import { changedSettingKey, changeTouchesSettings, useSettingsStore } from "./store/settingsStore";
import { useQueueStore } from "./store/queueStore";
import { useSpeedSampler } from "./store/speedHistoryStore";
import { jobFailureKey, useNotificationStore } from "./store/notificationStore";
import { useCustomizationStore } from "./store/customizationStore";
import { useGameLinksStore, useRemoteMediaStore } from "./store/remoteContentStore";
import { usePlaytimeStore } from "./store/playtimeStore";
import { useModsStore } from "./store/modsStore";
import { useModProfilesStore } from "./store/modProfilesStore";
import { useLibraryStore } from "./store/libraryStore";
import { GAMES } from "./data/games";
import {
  appInfo,
  applyActiveGame,
  listGames,
  onGameStarted,
  onGameStopped,
  onGameLaunchFailed,
  getRunningGameIds,
  onModsLoadFailed,
  onModProgress,
  downloadState,
  onQueueState,
  checkGameUpdate,
  onGameUpdateAvailable,
  onWallpaperMediaUpdated,
  onNetworkStatusChanged,
  getNetworkStatus,
  onInstallationComplete,
  onGamesDetected,
  onWindowRestored,
  onWindowMaximizedChanged,
  getWindowMaximized,
  openLogsFolder,
  openLeftoverFolder,
  getConfigNotice,
  downloadEnqueue,
} from "./lib/ipc";
import { onEvent } from "./lib/rpc";
import { baseKindOf, KIND_NOUN, KIND_VERB, TERMINAL_PHASES } from "./lib/download";
import { useDownloadHistoryStore } from "./store/downloadHistoryStore";
import { AnimatePresence, LazyMotion, MotionConfig, m } from "framer-motion";
import { useModalStore } from "./store/modalStore";
import { revealWhenPainted } from "./lib/bootGate";
import { log } from "./lib/log";
import { useRendererSuspended } from "./lib/useRendererSuspended";
import { setNowTickerSuspended } from "./lib/useRelativeTime";
import { Sidebar } from "./components/layout/Sidebar";
import { WallpaperLayer } from "./components/layout/WallpaperLayer";
import { NotificationPanel } from "./components/layout/NotificationPanel";
import { ToastLayer } from "./components/layout/ToastLayer";
import { OfflineBanner } from "./components/layout/OfflineBanner";
import { WindowControls } from "./components/layout/WindowControls";
import { GameHome } from "./components/home/GameHome";
import { ModalRoot } from "./components/ui/ModalRoot";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ActionButton } from "./components/ui/ActionButton";

const loadMotionFeatures = () => import("./lib/motionFeatures").then((mod) => mod.default);

const FirstRunSetup = lazy(() =>
  import("./components/library/FirstRunSetup").then((mod) => ({ default: mod.FirstRunSetup })),
);

const SETUP_BACKDROP = (
  <div
    className="absolute inset-0 z-(--z-setup)"
    style={{
      background: "rgba(10,10,13,0.86)",
      backdropFilter: "blur(28px) saturate(160%)",
      WebkitBackdropFilter: "blur(28px) saturate(160%)",
    }}
  />
);

const DONE_TEXT: Record<string, string> = {
  uninstall: "The removal finished.",
  move: "The game files are in their new folder.",
};

let configNotice: Promise<boolean> | null = null;

function announceConfigNotice(): Promise<boolean> {
  configNotice ??= getConfigNotice().then((notice) => {
    if (!notice) return false;
    const { saveBlocked, ...note } = notice;
    useNotificationStore.getState().push(note);
    if (!saveBlocked) return false;
    useModalStore.getState().openConfirm({
      title: note.title,
      message: note.text,
      confirmLabel: "OK",
      cancelLabel: null,
      onConfirm: () => {},
    });
    return true;
  });
  return configNotice;
}

const dismissToast = (id: string) => useNotificationStore.getState().dismissToast(id);

const openDownloadsAction = {
  label: "Open Downloads",
  run: () => useUiStore.getState().setView("downloads"),
};

const REPAIR_NEEDS_UPDATE = "An update is available. Update the game first, then repair.";

const PAGES = {
  home: GameHome,
  playtime: lazy(() =>
    import("./components/playtime/PlaytimePage").then((m) => ({ default: m.PlaytimePage })),
  ),
  downloads: lazy(() =>
    import("./components/downloads/DownloadsPage").then((m) => ({ default: m.DownloadsPage })),
  ),
  gallery: lazy(() =>
    import("./components/gallery/CapturesTab").then((m) => ({ default: m.GalleryPage })),
  ),
  mods: lazy(() => import("./components/mods/ModsPage").then((m) => ({ default: m.ModsPage }))),
  settings: lazy(() =>
    import("./components/settings/SettingsPage").then((m) => ({ default: m.SettingsPage })),
  ),
} as const;

function pageCrashContext(): Record<string, unknown> {
  const ui = useUiStore.getState();
  return { view: ui.activeView, activeGameId: ui.activeGameId, version: ui.appVersion || "unknown" };
}

function PageError({ view, onRetry }: { view: string; onRetry: () => void }) {
  const setView = useUiStore((s) => s.setView);
  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-4 px-6 text-white">
      <div className="text-lg font-semibold">This page hit an error</div>
      <div className="max-w-[420px] text-center text-sm text-white/60">
        It has been written to the log. The rest of the launcher still works.
      </div>
      <div className="flex items-center gap-2">
        <ActionButton onClick={onRetry}>Try again</ActionButton>
        {view !== "home" && (
          <ActionButton onClick={() => setView("home")}>Back to home</ActionButton>
        )}
        <ActionButton onClick={() => window.location.reload()}>Reload launcher</ActionButton>
        <ActionButton onClick={() => void openLogsFolder()}>Open logs folder</ActionButton>
      </div>
    </div>
  );
}

export default function App() {
  const view = useUiStore((s) => s.activeView);
  const setupOpen = useLibraryStore((s) => s.hydrated && !s.setupComplete);
  const rendererSuspended = useRendererSuspended();
  const maximized = useUiStore((s) => s.maximized);

  useSpeedSampler();

  useEffect(() => {
    document.documentElement.classList.toggle("renderer-suspended", rendererSuspended);
    setNowTickerSuspended(rendererSuspended);
  }, [rendererSuspended]);

  useEffect(() => {
    void applyActiveGame(useUiStore.getState().activeGameId);
    return useUiStore.subscribe((s, p) => {
      if (s.activeGameId !== p.activeGameId) void applyActiveGame(s.activeGameId);
    });
  }, []);

  useEffect(() => {
    let disposed = false;
    let un: (() => void) | undefined;
    void onEvent("settings-changed", (payload) => {
      const key = changedSettingKey(payload);
      if (changeTouchesSettings(payload)) void useSettingsStore.getState().hydrate();
      if (key === null || key === "modsEnabled" || key === "behavior.modsEnabled") {
        void useModsStore.getState().hydrateMasterEnabled();
      }
      if (key === null) {
        void useCustomizationStore.getState().hydrate();
        void useLibraryStore.getState().hydrate();
      } else if (key.startsWith("games.") && key.endsWith(".voicePackLanguage")) {
        void useCustomizationStore.getState().hydrate();
      }
    }).then((f) => {
      if (disposed) f();
      else un = f;
    });
    return () => {
      disposed = true;
      un?.();
    };
  }, []);

  useEffect(() => {
    revealWhenPainted(
      Promise.all([
        useSettingsStore.getState().hydrate(),
        useCustomizationStore.getState().hydrate(),
        useRemoteMediaStore.getState().hydrate(),
      ]),
    );
    void announceConfigNotice();
    void useGameLinksStore.getState().hydrate();
    void usePlaytimeStore.getState().hydrate();
    void useModsStore.getState().hydrateMasterEnabled();
    const libraryHydrated = useLibraryStore.getState().hydrate();
    void appInfo()
      .then((info) => {
        useUiStore.getState().setAppVersion(info.version);
        useUiStore.getState().setBuildType(info.buildType);
      })
      .catch((e) => log.warn("appInfo failed (version badge will show defaults):", e));

    const refreshInstalled = (onlyGameIds?: string[]) =>
      void listGames().then((games) => {
        const installedIds = games.filter((g) => g.installed).map((g) => g.id);
        useGamesStore.getState().setInstalled(installedIds);
        useGamesStore.getState().setPendingInstalls(
          Object.fromEntries(
            games.flatMap((g) => (g.pendingInstall ? [[g.id, g.pendingInstall] as const] : [])),
          ),
        );
        const toCheck = onlyGameIds
          ? installedIds.filter((id) => onlyGameIds.includes(id))
          : installedIds;
        for (const id of toCheck) {
          void checkGameUpdate(id).then((u) => {
            if (u.ok) useGamesStore.getState().setUpdate(id, u.available, u.steamManaged, u);
          });
        }
      });
    refreshInstalled();
    void downloadState().then((s) => useQueueStore.getState().setJobs(s.jobs));

    const unsubs: Array<() => void> = [];
    let disposed = false;
    const keep = (f: () => void) => {
      if (disposed) f();
      else unsubs.push(f);
    };
    let runningEpoch = 0;
    const seedRunningGames = () => {
      const epoch = runningEpoch;
      void getRunningGameIds(GAMES.map((g) => g.id)).then((ids) => {
        if (ids && epoch === runningEpoch) useGamesStore.getState().setRunningGames(ids);
      });
    };
    const startedSub = onGameStarted((id) => {
      if (!id) return;
      runningEpoch += 1;
      const ids = useGamesStore.getState().runningGames;
      if (!ids.includes(id)) useGamesStore.getState().setRunningGames([...ids, id]);
    });
    void startedSub.then(keep);
    const stoppedSub = onGameStopped((id) => {
      runningEpoch += 1;
      const ids = useGamesStore.getState().runningGames;
      useGamesStore.getState().setRunningGames(id ? ids.filter((x) => x !== id) : []);
      usePlaytimeStore.getState().refreshAfterSession();
    });
    void stoppedSub.then(keep);
    const failedSub = onGameLaunchFailed((id, reason) => {
      if (id) {
        runningEpoch += 1;
        const ids = useGamesStore.getState().runningGames;
        useGamesStore.getState().setRunningGames(ids.filter((x) => x !== id));
        useGamesStore.getState().setLaunching(id, false);
      }
      const game = GAMES.find((g) => g.id === id);
      useNotificationStore.getState().push({
        type: "warning",
        title: `${game?.name ?? "The game"} didn't start`,
        text: reason || "The game was never launched, so no playtime was recorded.",
      });
    });
    void failedSub.then(keep);
    void Promise.all([startedSub, stoppedSub, failedSub]).then(seedRunningGames);
    void onModsLoadFailed((id, error, message) => {
      const game = GAMES.find((g) => g.id === id);
      useNotificationStore.getState().push({
        type: "warning",
        title: "Mods didn't load",
        text: message || `${game?.name ?? "The game"} is starting without mods. ${error}`.trim(),
      });
    }).then(keep);
    void onEvent<{ gameId?: string; path?: string }>("custom-launcher-missing", (p) => {
      const game = GAMES.find((g) => g.id === p?.gameId);
      useNotificationStore.getState().push({
        type: "warning",
        title: "The custom launcher is missing",
        text: `${p?.path || "The program"} is no longer there, so ${game?.name ?? "the game"} is starting on its own.`,
      });
    }).then(keep);
    void onEvent<{ gameId?: string; reason?: string }>("game-launch-waiting", (p) => {
      const id = p?.gameId;
      if (id) useGamesStore.getState().setLaunching(id, false);
      const game = GAMES.find((g) => g.id === id);
      useNotificationStore.getState().push({
        type: "warning",
        title: `${game?.name ?? "The game"} hasn't started yet`,
        text: p?.reason || "Steam hasn't started the game yet.",
      });
    }).then(keep);
    void onEvent<{ gameId?: string }>("mods-tools-updating", (p) => {
      const game = GAMES.find((g) => g.id === p?.gameId);
      useNotificationStore.getState().push({
        title: "Updating the mod tools",
        text: `${game?.name ?? "The game"} starts once the mod tools finish updating.`,
      });
    }).then(keep);
    void onEvent("overlay-fullscreen-blocked", () => {
      const title = "The overlay cannot draw over exclusive fullscreen";
      const notes = useNotificationStore.getState();
      if (notes.items.some((n) => n.title === title && !n.read)) return;
      notes.push({
        type: "warning",
        title,
        text: "Switch the game to Borderless or Windowed in its display settings, then press the overlay shortcut again.",
      });
    }).then(keep);
    void onQueueState((s) => {
      useQueueStore.getState().setJobs(s.jobs);
    }).then(keep);

    let queuePhases: Record<string, string> = {};
    unsubs.push(
      useQueueStore.subscribe((state) => {
        const next: Record<string, string> = {};
        const finishedGames: string[] = [];
        for (const j of state.jobs) {
          next[j.id] = j.phase;
          const wasActive =
            queuePhases[j.id] !== undefined && !TERMINAL_PHASES.includes(queuePhases[j.id]);
          if (wasActive && TERMINAL_PHASES.includes(j.phase)) {
            const kind = baseKindOf(j);
            if (j.phase !== "deferred") useDownloadHistoryStore.getState().add({ ...j, kind });
            finishedGames.push(j.gameId);
            const name = GAMES.find((g) => g.id === j.gameId)?.name ?? j.gameId;
            const noun = KIND_NOUN[kind] ?? kind;
            if (j.phase === "done" && kind !== "verify") {
              const gameId = j.gameId;
              const leftovers = j.warning && (kind === "uninstall" || kind === "move");
              useNotificationStore.getState().push({
                type: j.warning ? "warning" : "success",
                title: `${name} ${KIND_VERB[kind] ?? "finished"}`,
                text: j.message || (DONE_TEXT[kind] ?? `The ${noun} finished successfully.`),
                action: leftovers
                  ? { label: "Open folder", run: () => void openLeftoverFolder(gameId) }
                  : undefined,
              });
            } else if (j.phase === "error" && kind !== "verify") {
              const gameId = j.gameId;
              const needsUpdate = kind === "repair" && j.error === REPAIR_NEEDS_UPDATE;
              useNotificationStore.getState().push({
                type: "error",
                title: `${name} ${noun} failed`,
                text: j.error || "Something went wrong. Check the logs for details.",
                action: needsUpdate
                  ? { label: "Update", run: () => void downloadEnqueue(gameId) }
                  : openDownloadsAction,
                key: jobFailureKey(gameId),
              });
            }
          }
        }
        queuePhases = next;
        if (finishedGames.length > 0) {
          refreshInstalled(finishedGames);
        }
      }),
    );
    void onGameUpdateAvailable((id, steamManaged, versions) =>
      useGamesStore.getState().setUpdate(id, true, steamManaged, versions),
    ).then(keep);
    void onWallpaperMediaUpdated((m) => useRemoteMediaStore.getState().apply(m)).then(keep);
    let networkEventSeen = false;
    void onNetworkStatusChanged((online) => {
      networkEventSeen = true;
      useUiStore.getState().setOnline(online);
    }).then((f) => {
      keep(f);
      void getNetworkStatus().then((online) => {
        if (online !== null && !networkEventSeen) useUiStore.getState().setOnline(online);
      });
    });
    let maximizedEventSeen = false;
    void onWindowMaximizedChanged((maximized) => {
      maximizedEventSeen = true;
      useUiStore.getState().setMaximized(maximized);
    }).then((f) => {
      keep(f);
      void getWindowMaximized().then((maximized) => {
        if (!maximizedEventSeen) useUiStore.getState().setMaximized(maximized);
      });
    });
    void onInstallationComplete((id) => refreshInstalled(id ? [id] : undefined)).then(keep);
    void onGamesDetected((ids) => {
      if (!ids.length) {
        refreshInstalled([]);
        return;
      }
      refreshInstalled(ids);
      void libraryHydrated
        .then(() => {
          const library = useLibraryStore.getState();
          if (!library.setupComplete) return;
          for (const g of GAMES) if (ids.includes(g.id)) library.show(g.id);
        })
        .catch((e) => log.warn("Could not add the detected games to the library:", e));
      const names = ids.map((id) => GAMES.find((g) => g.id === id)?.name ?? id);
      useNotificationStore.getState().push({
        type: "success",
        title: names.length === 1 ? "Found an installed game" : `Found ${names.length} installed games`,
        text: `${names.join(", ")} ${names.length === 1 ? "was" : "were"} already in your Peebify games folder and ${names.length === 1 ? "has" : "have"} been added to your library.`,
      });
    }).then(keep);
    void onEvent<{ gameId?: string } | null>("mods-status-changed", (payload) => {
      const changed = payload?.gameId;
      const mods = useModsStore.getState();
      const target = mods.requestedGameId ?? mods.loadedGameId ?? useUiStore.getState().activeGameId;
      if (!changed || changed === target) void mods.refresh(target);
      const profiles = useModProfilesStore.getState();
      const profilesTarget = profiles.requestedGameId ?? profiles.loadedGameId;
      if (profilesTarget && (!changed || changed === profilesTarget)) {
        void profiles.refresh(profilesTarget);
      }
    }).then(keep);
    void onModProgress((p) => {
      const mods = useModsStore.getState();
      mods.setProgress(p.done ? null : p);
      if (p.done && !p.failed && p.gameId === (mods.requestedGameId ?? mods.loadedGameId)) {
        void mods.refresh(p.gameId);
      }
    }).then(keep);
    let lastRestoreRefresh = 0;
    void onWindowRestored(() => {
      const now = Date.now();
      if (now - lastRestoreRefresh < 2000) return;
      lastRestoreRefresh = now;
      void downloadState().then((s) => useQueueStore.getState().setJobs(s.jobs));
      refreshInstalled();
      seedRunningGames();
    }).then(keep);

    return () => {
      disposed = true;
      unsubs.forEach((f) => f());
    };
  }, []);

  const Page = PAGES[view];

  return (
    <LazyMotion features={loadMotionFeatures} strict>
      <MotionConfig reducedMotion="user">
        <div
          className={`relative h-screen w-screen overflow-hidden ${maximized ? "" : "rounded-ui"}`}
        >
        <div
          className="drag-region absolute top-0 z-(--z-chrome) h-[32px]"
          style={{ left: 60, right: 158 }}
        />

        <WallpaperLayer />
        <div className="relative z-10 flex h-full" inert={setupOpen}>
          <Sidebar />
          <main className="relative min-w-0 flex-1">
            <AnimatePresence>
              {view !== "home" && (
                <m.div
                  key="glass-sheet"
                  className="glass-strong pointer-events-none absolute inset-0"
                  initial={{ opacity: 0 }}
                  animate={{ opacity: 1, transition: { duration: 0.2 } }}
                  exit={{ opacity: 0, transition: { duration: 0.15 } }}
                />
              )}
            </AnimatePresence>

            <AnimatePresence initial={false}>
              <m.div
                key={view}
                className="absolute inset-0"
                initial="initial"
                animate="animate"
                exit="exit"
              >
                <ErrorBoundary
                  key={view}
                  scope={`page ${view}`}
                  context={pageCrashContext}
                  fallback={(reset) => <PageError view={view} onRetry={reset} />}
                >
                  <Suspense fallback={null}>
                    <Page />
                  </Suspense>
                </ErrorBoundary>
              </m.div>
            </AnimatePresence>
          </main>
        </div>

          {!setupOpen && <WindowControls />}
          <div
            className="pointer-events-none absolute inset-0"
            style={setupOpen ? { zIndex: "calc(var(--z-setup) + 1)" } : undefined}
          >
            <OfflineBanner />
            <ToastLayer onOpen={setupOpen ? dismissToast : undefined} />
          </div>
          <NotificationPanel />
          <ModalRoot />
          <AnimatePresence>
            {setupOpen && (
              <Suspense key="setup" fallback={SETUP_BACKDROP}>
                <FirstRunSetup />
              </Suspense>
            )}
          </AnimatePresence>
        </div>
      </MotionConfig>
    </LazyMotion>
  );
}
