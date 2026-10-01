// ------------ RPC Calls ------------
// The lowest level call into the backend. rpc is the plain call, rpcAction also shows an error
// notification if the action fails, rpcRead is for quiet reads, and onEvent listens for backend events.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./tauri";
import { log } from "./log";
import { useNotificationStore } from "../store/notificationStore";

export async function rpc<T = unknown>(channel: string, ...args: unknown[]): Promise<T> {
  if (!isTauri()) return undefined as T;
  return tauriInvoke<T>("rpc", { channel, args });
}

function surfaceFailure(label: string, detail: unknown): void {
  const text =
    detail instanceof Error
      ? detail.message
      : typeof detail === "string" && detail
        ? detail
        : "Something went wrong. Check the logs for details.";
  log.warn(`[action] ${label} failed:`, detail ?? "(no detail)");
  useNotificationStore.getState().push({ type: "error", title: `${label} failed`, text });
}

export async function rpcAction<T = unknown>(
  label: string,
  channel: string,
  ...args: unknown[]
): Promise<T | undefined> {
  if (!isTauri()) return undefined;
  try {
    const res = await rpc<T>(channel, ...args);
    const r = res as { success?: boolean; error?: string; cancelled?: boolean } | undefined;
    if (r && typeof r === "object" && r.success === false && !r.cancelled) {
      surfaceFailure(label, r.error);
    }
    return res;
  } catch (e) {
    surfaceFailure(label, e);
    return undefined;
  }
}

export async function rpcRead<T = unknown>(
  channel: string,
  ...args: unknown[]
): Promise<T | undefined> {
  if (!isTauri()) return undefined;
  try {
    const res = await rpc<T>(channel, ...args);
    if (res && typeof res === "object" && "success" in (res as Record<string, unknown>)) {
      const r = res as { success?: boolean; error?: string; cancelled?: boolean };
      if (!r.success) {
        if (!r.cancelled) log.warn(`[read] ${channel} failed:`, r.error ?? "(no detail)");
        return undefined;
      }
    }
    return res;
  } catch (e) {
    log.warn(`[read] ${channel} failed:`, e ?? "(no detail)");
    return undefined;
  }
}

export function unwrap<T = unknown>(res: unknown): T | undefined {
  if (res && typeof res === "object" && "success" in (res as Record<string, unknown>)) {
    return (res as { success: boolean }).success ? (res as T) : undefined;
  }
  return res as T;
}

export async function onEvent<T = unknown>(
  channel: string,
  cb: (payload: T) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const un = await listen<T>(channel, (e) => cb(e.payload as T));
  return un as unknown as () => void;
}
