// ------------ Wallpaper Layer ------------
// The full-window background for the selected game. Uses the user's custom wallpaper if set, otherwise the
// game's own, and falls back to a still image if a video fails to load. Videos pause when the launcher is hidden
// or a game is running.
import { useState } from "react";
import { useUiStore } from "../../store/uiStore";
import { useSettingsStore } from "../../store/settingsStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useRemoteMediaStore } from "../../store/remoteContentStore";
import { gameById } from "../../data/games";
import { wallpaperFor } from "../../lib/customMedia";
import { useRendererSuspended } from "../../lib/useRendererSuspended";
import { useWindowFocused } from "../../lib/useWindowFocused";
import { useGamesStore } from "../../store/gamesStore";
import { CrossfadeMedia } from "../common/CrossfadeMedia";
import { markWallpaperPainted } from "../../lib/bootGate";

export function WallpaperLayer() {
  const game = gameById(useUiStore((s) => s.activeGameId));
  const suspended = useRendererSuspended();
  const onHome = useUiStore((s) => s.activeView === "home");
  const focused = useWindowFocused();
  const gameRunning = useGamesStore((s) => s.runningGames.length > 0);
  const animated = useSettingsStore((s) => s.values.animatedWallpaper) !== "false";
  const slice = useCustomizationStore((s) => s.wallpaper[game.id]);
  const remote = useRemoteMediaStore((s) => s.media[game.id]);

  const settingsReady = useSettingsStore((s) => s.hydrated);
  const customizationReady = useCustomizationStore((s) => s.hydrated);
  const cacheReady = useRemoteMediaStore((s) => s.hydrated);
  const ready = settingsReady && customizationReady && cacheReady;

  const [failedSources, setFailedSources] = useState<ReadonlySet<string>>(() => new Set());

  const candidates = [
    wallpaperFor(game.id, slice, remote, animated),
    wallpaperFor(game.id, undefined, remote, false),
    { src: game.wallpaperStatic, video: false },
  ];
  const { src: mediaSrc, video: useVideo } =
    candidates.find((c) => !failedSources.has(c.src)) ?? candidates[candidates.length - 1];

  const onMediaError = (src: string) => {
    setFailedSources((prev) => (prev.has(src) ? prev : new Set(prev).add(src)));
  };

  return (
    <div className="absolute inset-0">
      <div className="absolute inset-0" style={{ background: game.wallpaperFallback }} />

      {ready && (
        <CrossfadeMedia
          src={mediaSrc}
          video={useVideo}
          autoPlay={animated}
          playing={!suspended && onHome && (focused || !gameRunning)}
          onReady={markWallpaperPainted}
          onError={onMediaError}
        />
      )}

      <div
        className="absolute inset-0"
        style={{
          background:
            "linear-gradient(180deg, rgba(8,8,12,.28) 0%, rgba(8,8,12,0) 30%, rgba(8,8,12,.45) 100%)",
        }}
      />
    </div>
  );
}
