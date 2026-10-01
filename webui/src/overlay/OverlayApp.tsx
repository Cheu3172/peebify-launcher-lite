// ------------ Overlay Drawer ------------
// The in-game overlay: a drawer that slides in over the game with tabs for mods, the mods gallery,
// captures and settings. It follows whichever game is running.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, LazyMotion, MotionConfig, m } from "framer-motion";
import { X } from "lucide-react";
import { getModsStatus, getOverlayConfig, onEvent, rpc, type ModsStatus } from "./ipc";
import { changedSettingKey, changeTouchesSettings, useSettingsStore } from "../store/settingsStore";
import { useModsStore } from "../store/modsStore";
import { useModProfilesStore } from "../store/modProfilesStore";
import { useNotificationStore } from "../store/notificationStore";
import { gameById } from "../data/games";
import type { GameId } from "../types/game";
import { OverlayMods } from "./panels/OverlayMods";
import { OverlayGallery } from "./panels/OverlayGallery";
import { OverlayCaptures } from "./panels/OverlayCaptures";
import { OverlaySettings } from "./panels/OverlaySettings";
import { ToastLayer } from "../components/layout/ToastLayer";
import { spacedAccelerator } from "../lib/hotkeys";
import { EASE_OUT_SOFT } from "../lib/motion";
import { useOverlayModsSession } from "./modsSession";

const loadMotionFeatures = () => import("../lib/motionFeatures").then((mod) => mod.default);

type PanelId = "mods" | "gallery" | "captures" | "settings";

const ICON = {
  mods: (
    <>
      <rect x="3" y="3" width="7" height="7" rx="1.5" />
      <rect x="14" y="3" width="7" height="7" rx="1.5" />
      <rect x="3" y="14" width="7" height="7" rx="1.5" />
      <rect x="14" y="14" width="7" height="7" rx="1.5" />
    </>
  ),
  gallery: (
    <>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="9" cy="9" r="1.6" />
      <path d="m21 15-4.35-4.35a2 2 0 0 0-2.83 0L6 21" />
    </>
  ),
  captures: (
    <>
      <path d="M3 8h4l2-3h6l2 3h4v12H3z" />
      <circle cx="12" cy="13" r="3.4" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2.5V5M12 19v2.5M2.5 12H5M19 12h2.5M5.4 5.4 7 7M17 17l1.6 1.6M18.6 5.4 17 7M7 17l-1.6 1.6" />
    </>
  ),
} as const;

const PANELS: { id: PanelId; label: string; title: string }[] = [
  { id: "mods", label: "Mods", title: "Mods" },
  { id: "gallery", label: "Mods gallery", title: "Mods gallery" },
  { id: "captures", label: "Captures", title: "Captures" },
  { id: "settings", label: "Settings", title: "Settings" },
];

function NavIcon({ id }: { id: PanelId }) {
  return (
    <span className="grid h-[18px] w-[18px] place-items-center">
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {ICON[id]}
      </svg>
    </span>
  );
}

function elapsed(ms: number): string {
  const minutes = Math.floor(ms / 60000);
  const hours = Math.floor(minutes / 60);
  return hours > 0 ? `${hours}h ${minutes % 60}m` : `${minutes}m`;
}

function holdsText(target: EventTarget | null): boolean {
  if (target instanceof HTMLTextAreaElement) return target.value !== "";
  return (
    target instanceof HTMLInputElement &&
    (target.type === "text" || target.type === "search") &&
    target.value !== ""
  );
}

export default function OverlayApp() {
  const [gameId, setGameId] = useState<GameId | null>(null);
  const [startedMs, setStartedMs] = useState<number | null>(null);
  const [mods, setMods] = useState<ModsStatus | null>(null);
  const [active, setActive] = useState<PanelId>("mods");
  const [openKey, setOpenKey] = useState("Alt + P");
  const [, setTick] = useState(0);
  const hydrate = useSettingsStore((s) => s.hydrate);
  const gameRef = useRef<GameId | null>(null);
  const loadToken = useRef(0);
  const drawerOpen = useRef(true);
  const [drawerShown, setDrawerShown] = useState(() => document.visibilityState !== "hidden");
  const navRef = useRef<HTMLDivElement>(null);
  const backdropPress = useRef(false);

  const loadOpenKey = useCallback(async () => {
    const config = await getOverlayConfig();
    const toggle = config?.hotkeys?.find((h) => h.id === "overlayHotkey");
    if (toggle?.accelerator) setOpenKey(spacedAccelerator(toggle.accelerator));
  }, []);

  const loadGame = useCallback(async (refreshStores: boolean) => {
    const token = ++loadToken.current;
    const status = await rpc<{ gameId?: string | null; sessionStartedMs?: number | null }>(
      "get-overlay-status",
    );
    if (token !== loadToken.current) return;
    const id = (status?.gameId || null) as GameId | null;
    gameRef.current = id;
    setGameId(id);
    setStartedMs(typeof status?.sessionStartedMs === "number" ? status.sessionStartedMs : null);
    if (!id) {
      setMods(null);
      return;
    }
    const next = await getModsStatus(id);
    if (token !== loadToken.current) return;
    setMods(next);
    if (next.supportsMods) void useOverlayModsSession.getState().loadReloadKey(id);
    if (!refreshStores || !next.masterEnabled || !next.gameEnabled || !next.supportsMods) return;
    void useModsStore.getState().refresh(id);
    void useModProfilesStore.getState().refresh(id);
  }, []);

  useEffect(() => {
    void hydrate();
    void loadOpenKey();
    void loadGame(false);
  }, [hydrate, loadOpenKey, loadGame]);

  useEffect(() => {
    const timer = window.setInterval(() => setTick((n) => n + 1), 30000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let disposed = false;
    const offs: Array<() => void> = [];
    const listen = (name: string, handler: (payload: unknown) => void) => {
      void onEvent(name, handler).then((off) => {
        if (disposed) off();
        else offs.push(off);
      });
    };
    listen("settings-changed", (payload) => {
      if (changeTouchesSettings(payload)) void hydrate();
      const key = changedSettingKey(payload);
      if (key === null || key === "overlayHotkey" || key === "behavior.overlayHotkey") {
        void loadOpenKey();
      }
      if (key === null || key === "modsEnabled" || key === "behavior.modsEnabled") {
        void loadGame(false);
      }
    });
    listen("overlay-closed", () => {
      drawerOpen.current = false;
      setDrawerShown(false);
    });
    listen("overlay-opened", () => {
      drawerOpen.current = true;
      setDrawerShown(true);
      (document.activeElement as HTMLElement | null)?.blur();
      navRef.current
        ?.querySelector<HTMLElement>('[aria-current="page"]')
        ?.focus({ preventScroll: true });
      void loadOpenKey();
      void loadGame(true);
    });
    listen("mods-status-changed", (payload) => {
      const changed = (payload as { gameId?: string } | null)?.gameId;
      if (changed && changed !== gameRef.current) return;
      if (!drawerOpen.current || document.visibilityState === "hidden") return;
      void loadGame(true);
    });
    const onVisible = () => {
      if (document.visibilityState === "visible") setDrawerShown(true);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      disposed = true;
      offs.forEach((off) => off());
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [hydrate, loadOpenKey, loadGame]);

  const modsUsable = !!mods?.masterEnabled && !!mods?.gameEnabled && !!mods?.supportsMods;

  const modTabsVisible = mods === null || (mods.masterEnabled && mods.supportsMods);
  const panels = useMemo(
    () =>
      modTabsVisible
        ? PANELS
        : PANELS.filter((panel) => panel.id !== "mods" && panel.id !== "gallery"),
    [modTabsVisible],
  );

  useEffect(() => {
    if (!panels.some((panel) => panel.id === active)) {
      setActive(panels[0].id);
    }
  }, [panels, active]);

  const close = useCallback(() => {
    void rpc("overlay-toggle", "close");
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      if (e.key === "Escape") {
        if (holdsText(e.target)) return;
        e.preventDefault();
        close();
        return;
      }
      const index = Number.parseInt(e.key, 10) - 1;
      if (Number.isNaN(index) || index < 0 || index >= panels.length) return;
      const target = e.target as HTMLElement | null;
      if (target?.tagName === "INPUT" || target?.tagName === "TEXTAREA") return;
      e.preventDefault();
      setActive(panels[index].id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close, panels]);

  const game = gameId ? gameById(gameId) : null;
  const subtitle = game
    ? startedMs !== null
      ? `${game.name} · ${elapsed(Math.max(0, Date.now() - startedMs))}`
      : game.name
    : "No game is running";

  const Panel = {
    mods: OverlayMods,
    gallery: OverlayGallery,
    captures: OverlayCaptures,
    settings: OverlaySettings,
  }[active];

  const title = panels.find((p) => p.id === active)?.title ?? "";

  return (
    <LazyMotion features={loadMotionFeatures} strict>
      <MotionConfig reducedMotion="user">
        <div
          className="fixed inset-0 flex"
          style={{
            background:
              "linear-gradient(90deg, rgba(4,4,7,0.46) 0%, rgba(4,4,7,0.26) 38%, rgba(4,4,7,0.16) 100%)",
          }}
          onPointerDown={(e) => {
            backdropPress.current = e.target === e.currentTarget;
          }}
          onClick={(e) => {
            const pressed = backdropPress.current;
            backdropPress.current = false;
            if (pressed && e.target === e.currentTarget) close();
          }}
        >
          <m.aside
            initial={{ x: -40, opacity: 0 }}
            animate={drawerShown ? { x: [-40, 0], opacity: [0, 1] } : { x: -40, opacity: 0 }}
            transition={drawerShown ? { duration: 0.22, ease: EASE_OUT_SOFT } : { duration: 0 }}
            className="ov-drawer flex h-full w-[min(92vw,clamp(880px,48vw,1240px))] border-r border-white/[0.09]"
          >
            <nav
              aria-label="Overlay sections"
              className="ov-rail flex min-h-0 w-[268px] shrink-0 flex-col border-r border-white/[0.07] px-4 pb-[18px] pt-6"
            >
              <div className="flex items-center gap-[11px] px-2 pb-[18px]">
                <img
                  src="/icons/app.png"
                  alt="Peebify"
                  className="h-[30px] w-[30px] rounded-[8px] object-contain"
                />
                <div className="min-w-0">
                  <div className="text-[14px] font-semibold tracking-[-0.01em]">
                    Peebify Overlay
                  </div>
                  <div className="truncate text-[12px] text-white/45">{subtitle}</div>
                </div>
              </div>

              <div
                ref={navRef}
                className="ov-scroll flex min-h-0 flex-1 flex-col gap-[2px] overflow-y-auto"
              >
                {panels.map((panel, index) => {
                  const on = panel.id === active;
                  return (
                    <button
                      key={panel.id}
                      type="button"
                      aria-current={on ? "page" : undefined}
                      onClick={() => setActive(panel.id)}
                      className={`flex w-full items-center gap-[11px] rounded-[9px] border px-[11px] py-[10px] text-left text-[13.5px] transition-colors duration-150 ${
                        on
                          ? "border-white/[0.14] bg-white/10 font-semibold text-white"
                          : "border-transparent font-medium text-white/[0.62] hover:bg-white/[0.05] hover:text-white/85"
                      }`}
                    >
                      <NavIcon id={panel.id} />
                      <span className="flex-1">{panel.label}</span>
                      <span className="ov-mono text-[10.5px] text-white/30">{index + 1}</span>
                    </button>
                  );
                })}
              </div>

              <div className="mt-auto flex flex-col gap-[9px] border-t border-white/[0.07] px-2 pt-4">
                <div className="flex items-center justify-between text-[11.5px] text-white/[0.42]">
                  <span>Open / close</span>
                  <span className="ov-mono rounded-[5px] bg-white/[0.08] px-[6px] py-[2px] text-white/70">
                    {openKey}
                  </span>
                </div>
                <div className="flex items-center justify-between text-[11.5px] text-white/[0.42]">
                  <span>Close</span>
                  <span className="ov-mono rounded-[5px] bg-white/[0.08] px-[6px] py-[2px] text-white/70">
                    Esc
                  </span>
                </div>
              </div>
            </nav>

            <div className="ov-content relative flex min-w-0 flex-1 flex-col">
              <ToastLayer onOpen={(id) => useNotificationStore.getState().dismissToast(id)} />
              <header className="flex shrink-0 items-center justify-between gap-4 border-b border-white/[0.07] px-[clamp(30px,1.6vw,44px)] pb-4 pt-[26px]">
                <h3 className="font-display text-[21px] font-semibold">{title}</h3>
                <button
                  type="button"
                  aria-label="Close overlay"
                  title="Close (Esc)"
                  onClick={close}
                  className="-mr-2 grid h-[32px] w-[32px] shrink-0 place-items-center rounded-[8px] text-white/55 transition-colors duration-150 hover:bg-white/[0.08] hover:text-white"
                >
                  <X size={18} />
                </button>
              </header>
              <div className="ov-scroll min-h-0 flex-1 overflow-y-auto px-[clamp(30px,1.6vw,44px)] pb-[26px] pt-5">
                <AnimatePresence mode="wait" initial={false}>
                  <m.div
                    key={active}
                    initial={{ opacity: 0, y: 6 }}
                    animate={{ opacity: 1, y: 0 }}
                    exit={{ opacity: 0, y: -6 }}
                    transition={{ duration: 0.15 }}
                  >
                    {gameId ? (
                      <Panel gameId={gameId} modsUsable={modsUsable} mods={mods} />
                    ) : (
                      <p className="text-[13px] text-white/45">No game is running.</p>
                    )}
                  </m.div>
                </AnimatePresence>
              </div>
            </div>
          </m.aside>
        </div>
      </MotionConfig>
    </LazyMotion>
  );
}
