// ------------ Window Helpers ------------
// Small wrappers over the desktop shell: minimize, close, open a link, file drag and drop, and
// a check for whether we are running inside the app at all.
import { invoke } from "@tauri-apps/api/core";
import { log } from "./log";

export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function windowRpc(channel: string): Promise<void> {
  try {
    await invoke("rpc", { channel, args: [] });
  } catch (e) {
    log.warn(`[window] ${channel} failed:`, e);
  }
}

export async function minimizeWindow(): Promise<void> {
  if (!isTauri()) return;
  await windowRpc("minimize-window");
}

export async function closeWindow(): Promise<void> {
  if (!isTauri()) return;
  await windowRpc("close-window");
}

export async function openExternal(url: string): Promise<void> {
  if (!url) return;
  if (!isTauri()) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  const { rpcAction } = await import("./rpc");
  await rpcAction("Open link", "open-external-url", url);
}

export async function signalReady(): Promise<void> {
  if (!isTauri()) return;
  await windowRpc("window-ready-to-show");
}

export async function onFileDrop(
  onDrop: (paths: string[]) => void,
  onOver?: (hovering: boolean) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  const un = await getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload as { type: string; paths?: string[] };
    if (payload.type === "over") {
      onOver?.(true);
      return;
    }
    onOver?.(false);
    if (payload.type === "drop" && payload.paths?.length) onDrop(payload.paths);
  });
  return un as unknown as () => void;
}

const EDITABLE = 'input, textarea, [contenteditable]:not([contenteditable="false"])';

export function blockContextMenu(): void {
  window.addEventListener(
    "contextmenu",
    (event) => {
      const target = event.target;
      if (target instanceof Element && target.closest(EDITABLE)) return;
      event.preventDefault();
    },
    true,
  );
}
