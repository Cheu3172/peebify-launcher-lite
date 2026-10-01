// ------------ Sidebar ------------
// The slim rail on the left. Has the page buttons (Home, Playtime, Gallery, Downloads, Mods, Settings) and below
// them the user's games, which can be dragged to reorder. Shows badges for running games, updates and active
// downloads.
import { useEffect, useId, type CSSProperties, type ReactNode } from "react";
import { m } from "framer-motion";
import { Home, LineChart, Images, Download, Puzzle, Settings } from "lucide-react";
import { gameById } from "../../data/games";
import { useUiStore, type View } from "../../store/uiStore";
import { useGamesStore } from "../../store/gamesStore";
import { useQueueStore } from "../../store/queueStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useLibraryStore } from "../../store/libraryStore";
import { useModsStore } from "../../store/modsStore";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import { isActiveJob } from "../../lib/download";
import { REORDER_KEY_HINT, useReorder } from "../../lib/useReorder";
import { springThumb } from "../../lib/motion";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";
import { CHANNEL_LABEL } from "../../lib/ipc";

const NAV: { view: View; icon: typeof Home; label: string }[] = [
  { view: "home", icon: Home, label: "Home" },
  { view: "playtime", icon: LineChart, label: "Playtime" },
  { view: "gallery", icon: Images, label: "Gallery" },
  { view: "downloads", icon: Download, label: "Downloads" },
  { view: "mods", icon: Puzzle, label: "Mods" },
  { view: "settings", icon: Settings, label: "Settings" },
];

const SOURCE = "sidebar";

const RAIL_STYLE: CSSProperties = {
  background: "rgba(14,14,18,0.7)",
  backdropFilter: "blur(24px) saturate(180%)",
  WebkitBackdropFilter: "blur(24px) saturate(180%)",
  borderRight: "1px solid rgba(255,255,255,0.07)",
};

export function Sidebar() {
  const activeView = useUiStore((s) => s.activeView);
  const setView = useUiStore((s) => s.setView);
  const activeGameId = useUiStore((s) => s.activeGameId);
  const setActiveGame = useUiStore((s) => s.setActiveGame);
  const appVersion = useUiStore((s) => s.appVersion);
  const buildType = useUiStore((s) => s.buildType);
  const runningGames = useGamesStore((s) => s.runningGames);
  const updates = useGamesStore((s) => s.updates);
  const activeJobs = useQueueStore((s) => s.jobs.filter(isActiveJob).length);
  const icons = useCustomizationStore((s) => s.gameIcons);
  const committed = useLibraryStore((s) => s.visible);
  const preview = useLibraryStore((s) => s.preview);
  const setPreview = useLibraryStore((s) => s.setPreview);
  const move = useLibraryStore((s) => s.move);
  const visible = preview && preview.source !== SOURCE ? preview.order : committed;
  const modsEnabled = useModsStore((s) => s.masterEnabled);
  const modsHydrated = useModsStore((s) => s.masterHydrated);
  const nav = modsEnabled ? NAV : NAV.filter((n) => n.view !== "mods");

  const reorder = useReorder(visible, move, (order) => setPreview(SOURCE, order));
  const hintId = useId();
  const moved = reorder.announcement;

  useEffect(() => {
    if (modsHydrated && !modsEnabled && activeView === "mods") setView("home");
  }, [modsHydrated, modsEnabled, activeView, setView]);

  return (
    <aside className="group relative z-20 flex w-[60px] flex-col" style={RAIL_STYLE}>
      <div className="flex min-h-0 flex-1 flex-col items-center px-0 pb-[11px] pt-[13px]">
        <img
          src="/icons/app.png"
          alt="Peebify"
          className="mb-[14px] h-[32px] w-[32px] shrink-0 rounded-ui object-contain"
        />

        <nav className="flex shrink-0 flex-col items-center gap-[6px]">
          {nav.map(({ view, icon: Icon, label }) => (
            <RailButton
              key={view}
              active={activeView === view}
              label={label}
              badge={view === "downloads" ? activeJobs : 0}
              pillId="nav-pill"
              onClick={() => setView(view)}
            >
              <Icon size={18} strokeWidth={2} />
            </RailButton>
          ))}
        </nav>

        <div className="my-[11px] h-px w-[26px] shrink-0 bg-white/10" />

        <div
          ref={reorder.containerRef}
          className="rail-scroll scrollbar-none flex min-h-0 flex-1 flex-col items-center gap-[6px] overflow-y-auto overflow-x-hidden"
        >
          {visible.map((id) => {
            const game = gameById(id);
            const running = runningGames.includes(id);
            const hasUpdate = updates.includes(id);
            const label = [game.name, running && "playing now", hasUpdate && "update available"]
              .filter(Boolean)
              .join(", ");
            const { style, ...handlers } = reorder.itemProps(id);
            const held = reorder.dragId === id;
            return (
              <div
                key={id}
                {...handlers}
                className={`relative shrink-0 ${held ? "[&_*]:cursor-grabbing" : ""}`}
                style={{
                  ...style,
                  filter: held ? "drop-shadow(0 8px 16px rgba(0,0,0,0.5))" : undefined,
                }}
              >
                <RailButton
                  active={activeGameId === id}
                  label={label}
                  dot={hasUpdate}
                  pillId="game-pill"
                  kind="game"
                  describedBy={visible.length > 1 ? hintId : undefined}
                  onClick={() => {
                    setActiveGame(id);
                    setView("home");
                  }}
                >
                  <img
                    src={iconSrcFor(id, icons)}
                    onError={iconFallback(id)}
                    alt=""
                    draggable={false}
                    className="h-[34px] w-[34px] rounded-[9px] object-cover"
                    style={
                      running
                        ? { boxShadow: "0 0 0 1.5px #34d399, 0 0 10px rgba(52,211,153,.55)" }
                        : undefined
                    }
                  />
                </RailButton>
              </div>
            );
          })}
        </div>
        <span id={hintId} className="sr-only">
          {REORDER_KEY_HINT}
        </span>
        <span className="sr-only" aria-live="polite">
          {moved ? `${gameById(moved.id).name}, position ${moved.position} of ${moved.total}` : ""}
        </span>

        <div className="mt-auto flex shrink-0 flex-col items-center gap-[5px] pt-[10px]">
          <span className="text-[9px] font-medium uppercase tracking-[0.1em] text-white/50">
            {CHANNEL_LABEL[buildType]}
          </span>
          {appVersion && (
            <span className="text-[9px] tracking-[0.06em] text-white/50">v{appVersion}</span>
          )}
        </div>
      </div>
    </aside>
  );
}

function RailButton({
  active,
  label,
  badge = 0,
  dot = false,
  pillId,
  kind = "nav",
  describedBy,
  onClick,
  children,
}: {
  active: boolean;
  label: string;
  badge?: number;
  dot?: boolean;
  pillId: string;
  kind?: "nav" | "game";
  describedBy?: string;
  onClick: () => void;
  children: ReactNode;
}) {
  const tip = useAnchoredTip<HTMLButtonElement>("right");
  const spoken = badge > 0 ? `${label}, ${badge} active` : label;

  return (
    <button
      ref={tip.anchorRef}
      onClick={onClick}
      {...tip.bind}
      aria-label={spoken}
      aria-describedby={describedBy}
      aria-current={active ? (kind === "nav" ? "page" : "true") : undefined}
      className={`relative grid h-[40px] w-[40px] place-items-center rounded-ui transition duration-150 active:scale-[0.94] ${
        active ? "bg-white/[0.08] text-white" : "text-white/60 hover:bg-white/[0.06] hover:text-white"
      }`}
    >
      {active &&
        (kind === "game" ? (
          <m.span
            layoutId={pillId}
            transition={springThumb}
            className="accent-grad absolute left-[-10px] top-[11px] h-[18px] w-[2px] rounded-r-[2px] opacity-80"
          />
        ) : (
          <m.span
            layoutId={pillId}
            transition={springThumb}
            className="accent-grad absolute left-[-10px] top-[9px] h-[22px] w-[3px] rounded-r-[3px]"
          />
        ))}
      {children}
      {badge > 0 && (
        <span className="accent-grad absolute -right-[2px] -top-[2px] grid h-[15px] min-w-[15px] place-items-center rounded-full px-[3px] text-[9px] font-medium text-(--on-accent)">
          {badge}
        </span>
      )}
      {dot && (
        <span className="absolute right-[1px] top-[1px] h-[9px] w-[9px] rounded-full bg-white ring-2 ring-[#0e0e12]" />
      )}
      {tip.shown && (
        <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="right">
          {label}
        </TooltipPortal>
      )}
    </button>
  );
}
