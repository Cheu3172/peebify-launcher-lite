// ------------ Renderer Suspended ------------
// Tells components when the launcher window is hidden or in power saving mode, so they can stop
// animations and timers nobody can see.
import { useSyncExternalStore } from "react";
import { getRendererPowerSave, onRendererPowerSave, onWindowRestored } from "./ipc";

let suspended = false;
const listeners = new Set<() => void>();
let stop: (() => void) | null = null;

function publish(s: boolean) {
  if (s === suspended) return;
  suspended = s;
  listeners.forEach((l) => l());
}

function start(): () => void {
  let disposed = false;
  let heard = false;
  const unsubs: Array<() => void> = [];
  const keep = (f: () => void) => {
    if (disposed) f();
    else unsubs.push(f);
  };
  const apply = (s: boolean) => {
    heard = true;
    publish(s);
  };
  const subs = [
    onRendererPowerSave(apply).then(keep),
    onWindowRestored(() => apply(false)).then(keep),
  ];
  void Promise.all(subs)
    .then(getRendererPowerSave)
    .then((s) => {
      if (!disposed && !heard) publish(s);
    });
  return () => {
    disposed = true;
    unsubs.forEach((f) => f());
  };
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  stop ??= start();
  return () => {
    listeners.delete(listener);
    if (listeners.size > 0 || !stop) return;
    stop();
    stop = null;
    suspended = false;
  };
}

const getSnapshot = () => suspended;

export function useRendererSuspended(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
