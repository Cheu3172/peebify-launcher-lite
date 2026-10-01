// ------------ Boot Gate ------------
// Keeps the launcher window hidden until the first wallpaper and the saved data are ready, so it never flashes
// an empty window. If something is slow it shows up anyway after a short wait.
import { log } from "./log";
import { signalReady } from "./tauri";

const FIRST_PAINT_CAP_MS = 1500;

let revealed = false;
let resolveWallpaper: (() => void) | undefined;

const wallpaperPainted = new Promise<void>((resolve) => {
  resolveWallpaper = resolve;
});

export function markWallpaperPainted(): void {
  resolveWallpaper?.();
  resolveWallpaper = undefined;
}

function reveal(): void {
  if (revealed) return;
  revealed = true;
  void signalReady();
}

export function revealWhenPainted(hydration: Promise<unknown>): void {
  const cap = new Promise<void>((resolve) => {
    window.setTimeout(resolve, FIRST_PAINT_CAP_MS);
  });
  const painted = Promise.all([hydration, wallpaperPainted]).then(
    () => undefined,
    (e: unknown) => {
      log.warn("[boot] hydration failed:", e);
    },
  );
  void Promise.race([painted, cap]).then(reveal);
}
