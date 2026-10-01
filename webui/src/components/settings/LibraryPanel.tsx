// ------------ Library Panel ------------
// The game list on the Games settings tab. Choose which games show in the sidebar, drag to reorder them, and see
// which are installed.
import { useId, useMemo } from "react";
import { Check, CircleDashed, Plus, GripVertical, EyeOff } from "lucide-react";
import { GAMES, gameById } from "../../data/games";
import type { GameId } from "../../types/game";
import { useUiStore } from "../../store/uiStore";
import { useGamesStore } from "../../store/gamesStore";
import { useLibraryStore } from "../../store/libraryStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useSessionUiStore } from "../../store/sessionUiStore";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import { REORDER_KEY_HINT, useReorder } from "../../lib/useReorder";
import { GroupLabel } from "../ui/SettingsGroup";

const SOURCE = "library-panel";

export function LibraryPanel() {
  const committed = useLibraryStore((s) => s.visible);
  const preview = useLibraryStore((s) => s.preview);
  const setPreview = useLibraryStore((s) => s.setPreview);
  const show = useLibraryStore((s) => s.show);
  const hide = useLibraryStore((s) => s.hide);
  const move = useLibraryStore((s) => s.move);
  const visible = preview && preview.source !== SOURCE ? preview.order : committed;
  const installedIds = useGamesStore((s) => s.installed);
  const pendingInstalls = useGamesStore((s) => s.pendingInstalls);
  const activeId = useUiStore((s) => s.activeGameId);
  const setActiveGame = useUiStore((s) => s.setActiveGame);
  const selectedId = useSessionUiStore((s) => s.settingsGameId);
  const setSelectedId = useSessionUiStore((s) => s.setSettingsGameId);
  const icons = useCustomizationStore((s) => s.gameIcons);

  const hidden = useMemo(() => GAMES.filter((g) => !visible.includes(g.id)), [visible]);
  const isLastOne = visible.length <= 1;
  const reorder = useReorder(visible, move, (order) => setPreview(SOURCE, order));
  const hintId = useId();
  const moved = reorder.announcement;

  const selectGame = (id: GameId) => {
    setSelectedId(id);
    if (id === activeId) return;
    setActiveGame(id);
  };

  return (
    <div>
      <GroupLabel label="In your sidebar" hint={`${visible.length} of ${GAMES.length}`} />
      <div className="flex flex-col gap-2" ref={reorder.containerRef}>
        {visible.map((id) => {
          const game = gameById(id);
          const { style, ...handlers } = reorder.itemProps(id);
          const held = reorder.dragId === id;
          const on = id === selectedId;
          return (
            <div
              key={id}
              {...handlers}
              style={{
                ...style,
                boxShadow: held ? "0 14px 32px rgba(0,0,0,0.45)" : undefined,
                ...(on && !held
                  ? {
                      borderColor: "rgba(var(--accent-a-rgb), .45)",
                      background: "rgba(var(--accent-a-rgb), .1)",
                    }
                  : null),
              }}
              className={`relative flex cursor-grab items-center gap-[10px] rounded-ui border p-[9px] transition ${
                held
                  ? "border-white/25 bg-white/[0.1]"
                  : on
                    ? ""
                    : "border-white/[0.08] bg-white/[0.04] hover:border-white/20 hover:bg-white/[0.06]"
              }`}
            >
              <GripVertical
                size={15}
                className={`shrink-0 transition-colors ${held ? "text-white/60" : "text-white/25"}`}
              />
              <button
                onClick={() => selectGame(id)}
                aria-current={on}
                aria-describedby={isLastOne ? undefined : hintId}
                className="flex min-w-0 flex-1 items-center gap-[10px] text-left"
              >
                <img
                  src={iconSrcFor(id, icons)}
                  onError={iconFallback(id)}
                  alt=""
                  draggable={false}
                  className="h-[36px] w-[36px] shrink-0 rounded-[9px] object-cover"
                />
                <div className="min-w-0">
                  <div
                    className={`truncate text-[13px] font-medium ${on ? "text-white" : "text-white/80"}`}
                  >
                    {game.name}
                  </div>
                  <InstallState
                    installed={installedIds.includes(id)}
                    partial={!!pendingInstalls[id]}
                  />
                </div>
              </button>
              <button
                onClick={() => hide(id)}
                disabled={isLastOne}
                aria-label={`Remove ${game.name} from the sidebar`}
                title={
                  isLastOne
                    ? "Keep at least one game in the sidebar"
                    : `Remove ${game.name} from the sidebar`
                }
                className="shrink-0 rounded-[8px] border border-white/[0.1] p-[6px] text-white/55 transition hover:border-white/25 hover:text-white disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-white/[0.1] disabled:hover:text-white/55"
              >
                <EyeOff size={14} />
              </button>
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

      {hidden.length > 0 && (
        <>
          <GroupLabel label="Not in your sidebar" className="mt-6" />
          <div className="flex flex-col gap-2">
            {hidden.map((game) => (
              <button
                key={game.id}
                onClick={() => show(game.id)}
                className="flex items-center gap-[10px] rounded-ui border border-white/[0.08] bg-white/[0.03] p-[9px] text-left transition hover:border-white/20 hover:bg-white/[0.06] active:scale-[0.99]"
              >
                <span className="w-[15px] shrink-0" />
                <img
                  src={iconSrcFor(game.id, icons)}
                  onError={iconFallback(game.id)}
                  alt=""
                  className="h-[36px] w-[36px] shrink-0 rounded-[9px] object-cover opacity-55"
                />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13px] font-medium text-white/70">
                    {game.name}
                  </div>
                  <InstallState
                    installed={installedIds.includes(game.id)}
                    partial={!!pendingInstalls[game.id]}
                  />
                </div>
                <span className="shrink-0 rounded-[8px] border border-white/[0.1] p-[6px] text-white/50">
                  <Plus size={14} />
                </span>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function InstallState({ installed, partial = false }: { installed: boolean; partial?: boolean }) {
  const tone = installed
    ? "text-emerald-300/80"
    : partial
      ? "text-amber-300/80"
      : "text-white/55";
  return (
    <div className={`mt-[1px] flex items-center gap-1 text-[11px] ${tone}`}>
      {installed ? <Check size={11} /> : <CircleDashed size={11} />}
      {installed ? "Installed" : partial ? "Install unfinished" : "Not installed"}
    </div>
  );
}
