// ------------ Appearance Editors ------------
// The cards on the Appearance settings tab for picking a custom wallpaper, a custom icon and a colour for each
// game, with a preview and a reset button for each.
import { FolderOpen, Gamepad2, Image as ImageIcon, Palette, RotateCcw } from "lucide-react";
import { GAMES, gameById } from "../../data/games";
import type { GameId } from "../../types/game";
import { useLibraryStore } from "../../store/libraryStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useRemoteMediaStore } from "../../store/remoteContentStore";
import { useSettingsStore } from "../../store/settingsStore";
import { useSessionUiStore } from "../../store/sessionUiStore";
import { selectWallpaperFile, selectGameIconFile, type MediaSlice } from "../../lib/ipc";
import { iconFallback, iconSrcFor, wallpaperFor } from "../../lib/customMedia";
import { gameColor, isGameColor } from "../../data/playtime";
import { CrossfadeMedia } from "../common/CrossfadeMedia";
import { useRendererSuspended } from "../../lib/useRendererSuspended";
import { useWindowFocused } from "../../lib/useWindowFocused";
import { useGamesStore } from "../../store/gamesStore";

const BTN =
  "flex items-center gap-[6px] rounded-ui border border-white/15 bg-white/[0.05] px-3 py-[7px] text-[12.5px] font-medium text-white transition-colors hover:bg-white/10";

const CARD = "rounded-ui border border-white/[0.08] bg-white/[0.05] p-5";

function GamePicker({
  value,
  onChange,
  icons,
}: {
  value: GameId;
  onChange: (id: GameId) => void;
  icons: Record<string, MediaSlice>;
}) {
  const visible = useLibraryStore((s) => s.visible);
  const listed =
    visible.length === 0
      ? GAMES
      : (visible.includes(value) ? visible : [...visible, value]).map(gameById);
  return (
    <div className="flex flex-wrap gap-2">
      {listed.map((g) => {
        const active = value === g.id;
        return (
          <button
            key={g.id}
            onClick={() => onChange(g.id)}
            aria-pressed={active}
            title={g.name}
            className={`flex items-center gap-[8px] rounded-ui border px-[10px] py-[7px] text-[12.5px] font-medium transition-colors ${
              active
                ? "border-(--accent-b)/60 bg-(--accent-b)/12 text-white"
                : "border-white/10 text-white/60 hover:border-white/25 hover:bg-white/[0.06] hover:text-white/90"
            }`}
          >
            <img
              src={iconSrcFor(g.id, icons)}
              onError={iconFallback(g.id)}
              alt=""
              className="h-[22px] w-[22px] rounded-[5px] object-cover"
            />
            <span>{g.short ?? g.name}</span>
          </button>
        );
      })}
    </div>
  );
}

export function CustomWallpaperCard() {
  const suspended = useRendererSuspended();
  const focused = useWindowFocused();
  const gameRunning = useGamesStore((s) => s.runningGames.length > 0);
  const gameId = useSessionUiStore((s) => s.settingsGameId);
  const setGameId = useSessionUiStore((s) => s.setSettingsGameId);
  const wallpaper = useCustomizationStore((s) => s.wallpaper);
  const gameIcons = useCustomizationStore((s) => s.gameIcons);
  const setWallpaper = useCustomizationStore((s) => s.setWallpaper);
  const game = gameById(gameId);

  const animated = useSettingsStore((s) => s.values.animatedWallpaper) !== "false";
  const slice = wallpaper[gameId];
  const remote = useRemoteMediaStore((s) => s.media[gameId]);
  const { src, video } = wallpaperFor(gameId, slice, remote, animated);
  const custom = slice?.type === "custom";

  const pick = async () => {
    const p = await selectWallpaperFile();
    if (p) setWallpaper(gameId, { type: "custom", path: p });
  };

  return (
    <div className={CARD}>
      <h5 className="flex items-center gap-2 text-[14px] font-medium text-white">
        <ImageIcon size={15} /> Custom wallpaper
      </h5>
      <p className="mb-3 mt-1 text-[12.5px] text-white/55">
        Use a local image or video as the background.
      </p>
      <div className="mb-3">
        <GamePicker value={gameId} onChange={setGameId} icons={gameIcons} />
      </div>
      <div className="relative mb-3 aspect-video w-full max-w-[420px] overflow-hidden rounded-ui bg-black/40">
        <CrossfadeMedia
          src={src}
          video={video}
          autoPlay={animated}
          playing={!suspended && (focused || !gameRunning)}
        />
      </div>
      <p className="mb-3 text-[11.5px] text-white/55">
        {custom ? `Custom wallpaper for ${game.name}` : `Default wallpaper for ${game.name}`}
      </p>
      <div className="flex gap-2">
        <button onClick={() => void pick()} className={BTN}>
          <FolderOpen size={14} /> Select file
        </button>
        {custom && (
          <button onClick={() => setWallpaper(gameId, { type: "default", path: null })} className={BTN}>
            <RotateCcw size={14} /> Reset
          </button>
        )}
      </div>
    </div>
  );
}

export function CustomIconCard() {
  const gameId = useSessionUiStore((s) => s.settingsGameId);
  const setGameId = useSessionUiStore((s) => s.setSettingsGameId);
  const gameIcons = useCustomizationStore((s) => s.gameIcons);
  const setGameIcon = useCustomizationStore((s) => s.setGameIcon);
  const game = gameById(gameId);

  const slice = gameIcons[gameId];
  const custom = slice?.type === "custom";

  const pick = async () => {
    const p = await selectGameIconFile();
    if (p) setGameIcon(gameId, { type: "custom", path: p });
  };

  return (
    <div className={CARD}>
      <h5 className="flex items-center gap-2 text-[14px] font-medium text-white">
        <Gamepad2 size={15} /> Custom game icons
      </h5>
      <p className="mb-3 mt-1 text-[12.5px] text-white/55">
        Replace the sidebar icon for each game. Static or animated (WebP, GIF).
      </p>
      <div className="mb-3">
        <GamePicker value={gameId} onChange={setGameId} icons={gameIcons} />
      </div>
      <div className="mb-3 flex items-center gap-3 rounded-ui bg-black/30 p-3">
        <img
          src={iconSrcFor(gameId, gameIcons)}
          onError={iconFallback(gameId)}
          alt=""
          className="h-[52px] w-[52px] shrink-0 rounded-ui object-cover"
        />
        <div>
          <p className="text-[13px] font-medium text-white">{game.name}</p>
          <p className="text-[11.5px] text-white/55">{custom ? "Custom icon" : "Default icon"}</p>
        </div>
      </div>
      <div className="flex gap-2">
        <button onClick={() => void pick()} className={BTN}>
          <FolderOpen size={14} /> Select icon
        </button>
        {custom && (
          <button onClick={() => setGameIcon(gameId, { type: "default", path: null })} className={BTN}>
            <RotateCcw size={14} /> Reset
          </button>
        )}
      </div>
    </div>
  );
}

export function GameColorsCard() {
  const overrides = useCustomizationStore((s) => s.playtimeColor);
  const setPlaytimeColor = useCustomizationStore((s) => s.setPlaytimeColor);
  const gameIcons = useCustomizationStore((s) => s.gameIcons);
  const visible = useLibraryStore((s) => s.visible);

  const listed = visible.length ? GAMES.filter((g) => visible.includes(g.id)) : GAMES;
  const customCount = listed.filter((g) => isGameColor(overrides[g.id])).length;

  return (
    <div className={CARD}>
      <div className="mb-3 flex items-start justify-between gap-4">
        <div>
          <h5 className="flex items-center gap-2 text-[14px] font-medium text-white">
            <Palette size={15} /> Game colours
          </h5>
          <p className="mt-1 text-[12.5px] text-white/55">
            The colour each game is drawn in on the Playtime charts.
          </p>
        </div>
        {customCount > 0 && (
          <button
            onClick={() => listed.forEach((g) => setPlaytimeColor(g.id, null))}
            className={BTN}
            title="Put every game back to its default colour"
          >
            <RotateCcw size={14} /> Reset all
          </button>
        )}
      </div>

      <div className="flex flex-col gap-[2px]">
        {listed.map((game) => {
          const custom = isGameColor(overrides[game.id]);
          const value = gameColor(game.id, overrides);
          return (
            <div
              key={game.id}
              className="flex items-center gap-3 rounded-[8px] px-2 py-[7px] transition-colors hover:bg-white/[0.04]"
            >
              <img
                src={iconSrcFor(game.id, gameIcons)}
                onError={iconFallback(game.id)}
                alt=""
                className="h-[22px] w-[22px] shrink-0 rounded-[4px] object-cover"
              />
              <span className="min-w-0 flex-1 truncate text-[13px] text-white/80">
                {game.name}
              </span>
              <span className="shrink-0 font-mono text-[11.5px] uppercase text-white/55">
                {value}
              </span>
              <label
                className="relative h-[24px] w-[34px] shrink-0 cursor-pointer overflow-hidden rounded-[6px] border border-white/15 has-[>input:focus-visible]:outline-2 has-[>input:focus-visible]:outline-offset-2 has-[>input:focus-visible]:outline-(--accent-a)"
                style={{ background: value }}
                title={`Change the colour for ${game.name}`}
              >
                <input
                  type="color"
                  value={value}
                  onChange={(e) => setPlaytimeColor(game.id, e.target.value)}
                  aria-label={`Colour for ${game.name}`}
                  className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
                />
              </label>
              <button
                onClick={() => setPlaytimeColor(game.id, null)}
                disabled={!custom}
                title={custom ? `Reset ${game.name}` : "Already the default colour"}
                aria-label={`Reset the colour for ${game.name}`}
                className="grid h-[24px] w-[24px] shrink-0 place-items-center rounded-[6px] border border-white/[0.08] bg-white/[0.04] text-white/60 transition-colors hover:bg-white/[0.1] hover:text-white disabled:cursor-not-allowed disabled:opacity-25"
              >
                <RotateCcw size={12} />
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}
