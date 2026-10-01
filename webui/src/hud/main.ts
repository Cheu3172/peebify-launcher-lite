// ------------ In-Game HUD ------------
// The tiny see-through window that sits over the game and shows the screenshot saved toast and the REC timer
// while recording. It only listens for overlay events, nothing here is clickable.

import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { blockContextMenu } from "../lib/tauri";
import { installGlobalErrorCapture, log } from "../lib/log";
import "./hud.css";

installGlobalErrorCapture("overlay-hud");
blockContextMenu();

const failedChannels = new Set<string>();

async function rpc<T>(channel: string, ...args: unknown[]): Promise<T | undefined> {
  try {
    return await invoke<T>("rpc", { channel, args });
  } catch (e) {
    if (!failedChannels.has(channel)) {
      failedChannels.add(channel);
      log.warn(`[hud] ${channel} failed:`, e);
    }
    return undefined;
  }
}

let toastsEnabled = true;

function bool(value: string | undefined, fallback: boolean): boolean {
  return value === undefined || value === "" ? fallback : value === "true";
}

async function loadSettings(): Promise<void> {
  const values = await rpc<{ settings?: Record<string, string> }>("get-settings");
  const s = values?.settings ?? {};
  toastsEnabled = bool(s.overlayShotToast, true);
}

type Tone = "info" | "warning" | "error";

const TOAST_MS: Record<Tone, number> = { info: 2200, warning: 4500, error: 4500 };
let toastHost: HTMLDivElement | null = null;
let toastTimer: number | undefined;

function toast(message: string, tone: Tone): void {
  if (tone === "info" && !toastsEnabled) return;
  if (!toastHost) {
    toastHost = document.createElement("div");
    toastHost.className = "toast";
    document.body.appendChild(toastHost);
  }
  toastHost.textContent = message;
  toastHost.dataset.tone = tone;
  toastHost.classList.remove("show");
  void toastHost.offsetWidth;
  toastHost.classList.add("show");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastHost?.classList.remove("show"), TOAST_MS[tone]);
}

let recHost: HTMLDivElement | null = null;
let recTimer: number | undefined;
let recStarted = 0;

function setRecording(on: boolean, since = Date.now()): void {
  window.clearInterval(recTimer);
  if (!on) {
    recHost?.classList.remove("show");
    return;
  }
  if (!recHost) {
    recHost = document.createElement("div");
    recHost.className = "rec";
    document.body.appendChild(recHost);
  }
  recStarted = since;
  const paint = () => {
    const seconds = Math.max(0, Math.round((Date.now() - recStarted) / 1000));
    const label = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
    if (recHost) recHost.textContent = `REC ${label}`;
  };
  paint();
  recHost.classList.add("show");
  recTimer = window.setInterval(paint, 1000);
}

document.addEventListener("visibilitychange", () => {
  log.info(`[hud] visibility changed to ${document.visibilityState}`);
});
document.addEventListener("freeze", () => {
  log.info("[hud] page frozen by the browser");
});
document.addEventListener("resume", () => {
  log.info(`[hud] page resumed (visibility: ${document.visibilityState})`);
});

type Payload = Record<string, unknown> | null | undefined;

const fileName = (path: unknown) =>
  String(path ?? "")
    .split(/[\\/]/)
    .pop();

const FEEDBACK: Record<string, (payload: Payload) => void> = {
  "overlay-capture": (p) => {
    const name = fileName(p?.path);
    toast(name ? `Saved ${name}` : "Screenshot saved", "info");
  },
  "overlay-capture-failed": (p) => {
    toast(String(p?.error ?? "The screenshot failed."), "error");
  },
  "overlay-record-started": (p) => {
    setRecording(true);
    const warnings = Array.isArray(p?.audioWarnings) ? p.audioWarnings.map(String) : [];
    if (warnings.length) toast(`Recording started. ${warnings.join(" ")}`, "warning");
    else toast("Recording started", "info");
  },
  "overlay-record-stopped": (p) => {
    setRecording(false);
    const name = fileName(p?.path);
    const seconds = Math.round(Number(p?.durationMs ?? 0) / 1000) || 0;
    const length = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
    const saved = name ? `Saved ${name} · ${length}` : `Recording saved · ${length}`;
    const reason = typeof p?.reason === "string" && p.reason ? p.reason : null;
    toast(reason ? `${reason} ${saved}` : saved, "info");
  },
  "overlay-record-failed": (p) => {
    setRecording(false);
    toast(String(p?.error ?? "Recording failed."), "error");
  },
};

let liveFeedback = false;

for (const [name, handler] of Object.entries(FEEDBACK)) {
  void listen<Payload>(name, (event) => {
    liveFeedback = true;
    handler(event.payload);
  });
}

void listen<{ key?: unknown } | null>("settings-changed", (event) => {
  const key = event.payload?.key;
  if (typeof key === "string" && key && !key.replace(/^behavior\./, "").startsWith("overlay")) {
    return;
  }
  void loadSettings();
});

type HudStatus = {
  recording?: boolean;
  recordingSinceMs?: number;
  hudFeedback?: { event?: string; payload?: Payload } | null;
};

async function restoreState(): Promise<void> {
  await loadSettings();
  const status = await rpc<HudStatus>("get-overlay-status");
  if (!status) return;
  if (liveFeedback) return;
  const pending = status.hudFeedback;
  if (pending?.event) FEEDBACK[pending.event]?.(pending.payload);
  if (status.recording) setRecording(true, status.recordingSinceMs || Date.now());
}

void restoreState();
