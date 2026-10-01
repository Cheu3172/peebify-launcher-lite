// ------------ Shared Clock ------------
// One shared timer that ticks every 30 seconds so every "5m ago" label on screen updates together.
// It pauses while the window is hidden.
import { useSyncExternalStore } from "react";

const TICK_MS = 30_000;
const listeners = new Set<() => void>();
let now = Date.now();
let timer: number | undefined;
let suspended = false;

function tick(): void {
  now = Date.now();
  listeners.forEach((l) => l());
}

function startTicker(): void {
  if (timer !== undefined || suspended || listeners.size === 0) return;
  timer = window.setInterval(tick, TICK_MS);
}

function stopTicker(): void {
  if (timer === undefined) return;
  window.clearInterval(timer);
  timer = undefined;
}

export function setNowTickerSuspended(value: boolean): void {
  if (suspended === value) return;
  suspended = value;
  if (value) {
    stopTicker();
    return;
  }
  if (listeners.size > 0) tick();
  startTicker();
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  if (listeners.size === 1) {
    now = Date.now();
    startTicker();
  }
  return () => {
    listeners.delete(cb);
    if (listeners.size === 0) stopTicker();
  };
}

export function useNow(): number {
  return useSyncExternalStore(subscribe, () => now);
}
