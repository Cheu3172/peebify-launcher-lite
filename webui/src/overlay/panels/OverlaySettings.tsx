// ------------ Overlay Settings ------------
// The Settings tab of the overlay: overlay shortcuts you can rebind, the mod loader's reload key, and
// the status of the overlay helper.
import { useCallback, useEffect, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { getOverlayConfig, onEvent, rpc, type OverlayHotkey } from "../ipc";
import { changedSettingKey, useSettingsStore } from "../../store/settingsStore";
import { OvLabel, OvList, OvRow, OvToggle } from "../ui";
import type { PanelProps } from "../types";
import { acceleratorOf, captureHint, spacedAccelerator } from "../../lib/hotkeys";
import { MOD_RISK_WARNING } from "../../lib/mods";

function shortcutLabel(label: string, accelerator: string, capturing: boolean): string {
  if (capturing) return `${label}: press the new shortcut, or Escape to keep the current one`;
  return `${label}: ${accelerator ? spacedAccelerator(accelerator) : "not set"}. Press to change`;
}

interface Status {
  state: string;
  error: string;
  registered: string[];
  unregistered: string[];
  invalid: string[];
}

const HELPER_NOTE: Record<string, string> = {
  running: "",
  starting: "The overlay helper is starting.",
  failed: "The overlay helper stopped. Restart the game to try again.",
  mismatched:
    "The overlay helper does not match this version of Peebify. Reinstall Peebify to fix it.",
  idle: "The overlay helper is not running.",
};

interface LoaderKey {
  id: string;
  label: string;
  description: string;
  kind: string;
  accelerator: string | null;
  defaultAccelerator: string | null;
}

export function OverlaySettings({ gameId, modsUsable }: PanelProps) {
  const values = useSettingsStore((s) => s.values);
  const set = useSettingsStore((s) => s.set);
  const [hotkeys, setHotkeys] = useState<OverlayHotkey[]>([]);
  const [loaderKeys, setLoaderKeys] = useState<LoaderKey[]>([]);
  const [status, setStatus] = useState<Status | null>(null);
  const [capturing, setCapturing] = useState<string | null>(null);
  const [rejected, setRejected] = useState<string | null>(null);
  const [warning, setWarning] = useState<string | null>(null);
  const [confirmMods, setConfirmMods] = useState(false);

  const refresh = useCallback(async () => {
    const config = await getOverlayConfig();
    setHotkeys(config?.hotkeys ?? []);
    const s = await rpc<Partial<Status>>("get-overlay-status");
    setStatus({
      state: String(s?.state ?? "idle"),
      error: String(s?.error ?? ""),
      registered: s?.registered ?? [],
      unregistered: s?.unregistered ?? [],
      invalid: s?.invalid ?? [],
    });
    const ini = await rpc<{ settings?: LoaderKey[] }>("mod-ini-settings", gameId);
    setLoaderKeys((ini?.settings ?? []).filter((row) => row.kind === "key"));
  }, [gameId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let disposed = false;
    const offs: Array<() => void> = [];
    const listen = (name: string, handler: (payload: unknown) => void) => {
      void onEvent(name, handler).then((off) => {
        if (disposed) off();
        else offs.push(off);
      });
    };
    for (const name of ["overlay-status", "overlay-opened", "overlay-hotkeys-registered"]) {
      listen(name, () => void refresh());
    }
    listen("mod-ini-changed", (payload) => {
      const changed = (payload as { gameId?: string } | null)?.gameId;
      if (!changed || changed === gameId) void refresh();
    });
    listen("settings-changed", (payload) => {
      const key = changedSettingKey(payload);
      if (key === null || key.startsWith("overlay") || key.startsWith("behavior.overlay")) {
        void refresh();
      }
    });
    return () => {
      disposed = true;
      offs.forEach((off) => off());
    };
  }, [refresh, gameId]);

  const startCapture = (id: string) => {
    setRejected(null);
    setWarning(null);
    setCapturing(id);
    void rpc("suspend-overlay-hotkeys", true);
  };

  const endCapture = useCallback(() => {
    setCapturing(null);
    setRejected(null);
    void rpc("suspend-overlay-hotkeys", false);
  }, []);

  useEffect(() => {
    if (!capturing) return;
    const onBlur = () => endCapture();
    window.addEventListener("blur", onBlur);
    const keepAlive = window.setInterval(() => void rpc("suspend-overlay-hotkeys", true), 8000);
    return () => {
      window.removeEventListener("blur", onBlur);
      window.clearInterval(keepAlive);
    };
  }, [capturing, endCapture]);

  useEffect(() => () => void rpc("suspend-overlay-hotkeys", false), []);

  const apply = async (id: string, accelerator: string) => {
    endCapture();
    const result = await rpc<{ success?: boolean; error?: string; warning?: string | null }>(
      "set-overlay-hotkey",
      id,
      accelerator,
    );
    if (result?.success === false) {
      setRejected(result.error ?? "That shortcut cannot be used.");
      return;
    }
    setRejected(null);
    setWarning(result?.warning ?? null);
    await refresh();
  };

  const applyLoader = async (id: string, accelerator: string) => {
    endCapture();
    const result = await rpc<{ success?: boolean; error?: string }>(
      "set-mod-ini-setting",
      gameId,
      id,
      accelerator,
    );
    if (result?.success === false) {
      setRejected(result.error ?? "The mod loader would not take that shortcut.");
      return;
    }
    setRejected(null);
    await refresh();
  };

  const bind = (id: string, e: React.KeyboardEvent, loader = false) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      endCapture();
      return;
    }
    const accelerator = acceleratorOf(e);
    if (!accelerator) {
      const hint = captureHint(e);
      if (hint) setRejected(hint);
      return;
    }
    void (loader ? applyLoader(id, accelerator) : apply(id, accelerator));
  };

  const write = (key: string, value: string) => {
    set(key, value);
  };

  const flag = (key: string, fallback: boolean) =>
    values[key] === undefined ? fallback : values[key] === "true";

  const blocked = status?.unregistered ?? [];
  const invalid = status?.invalid ?? [];
  const note = status && status.state !== "running" ? HELPER_NOTE[status.state] : "";

  return (
    <div className="flex flex-col gap-3">
      {(note || blocked.length > 0 || invalid.length > 0) && (
        <div className="flex items-start gap-3 rounded-[10px] border border-[#f59e0b]/30 bg-[#f59e0b]/[0.08] px-4 py-3">
          <AlertTriangle size={16} className="mt-[2px] shrink-0 text-[#fcd34d]" />
          <div className="text-[12.5px] leading-relaxed text-[#fcd34d]">
            {note && <p>{note}</p>}
            {blocked.length > 0 && (
              <p>
                Windows would not give Peebify {blocked.map(spacedAccelerator).join(", ")}. Another program
                already holds {blocked.length === 1 ? "it" : "them"}, so pick a different
                shortcut below.
              </p>
            )}
            {invalid.length > 0 && (
              <p>
                Peebify cannot use {invalid.map(spacedAccelerator).join(", ")} as{" "}
                {invalid.length === 1 ? "a shortcut" : "shortcuts"}, so pick a different one below.
              </p>
            )}
            {status?.error && <p className="mt-1 text-white/50">{status.error}</p>}
          </div>
        </div>
      )}

      <section className="flex flex-col gap-2">
        <OvLabel>General</OvLabel>
        <OvList>
          <OvRow title="Mod support" description="Applies the next time you start the game.">
            <OvToggle
              checked={values.modsEnabled === "true"}
              ariaLabel="Mod support"
              onChange={(v) => {
                if (v) {
                  setConfirmMods(true);
                  return;
                }
                setConfirmMods(false);
                write("modsEnabled", "false");
              }}
            />
          </OvRow>
          {confirmMods && values.modsEnabled !== "true" && (
            <div className="flex flex-col gap-3 border-b border-white/[0.055] bg-[#ef4444]/[0.06] px-4 py-3">
              <p className="text-[12px] leading-relaxed text-[#fca5a5]">{MOD_RISK_WARNING}</p>
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setConfirmMods(false)}
                  className="rounded-[8px] px-[13px] py-[7px] text-[12px] text-white/60 hover:text-white"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setConfirmMods(false);
                    write("modsEnabled", "true");
                  }}
                  className="rounded-[8px] border border-[#ef4444]/40 bg-[#ef4444]/15 px-[13px] py-[7px] text-[12px] font-medium text-[#fca5a5] hover:bg-[#ef4444]/25"
                >
                  I understand, turn mods on
                </button>
              </div>
            </div>
          )}
          <OvRow
            title="NSFW mods"
            description="Show mature rated mods in the gallery."
          >
            <OvToggle
              checked={flag("showNsfwMods", false)}
              disabled={!modsUsable}
              ariaLabel="NSFW mods"
              onChange={(v) => write("showNsfwMods", String(v))}
            />
          </OvRow>
        </OvList>
      </section>

      <section className="flex flex-col gap-2">
        <OvLabel>Overlay shortcuts</OvLabel>
        <OvList>
          {hotkeys.map((hotkey) => {
            const active = capturing === hotkey.id;
            return (
              <div
                key={hotkey.id}
                className="flex items-center justify-between gap-4 border-b border-white/[0.055] px-4 py-3"
              >
                <span className="text-[12.5px] text-white/[0.78]">{hotkey.label}</span>
                <div className="flex shrink-0 items-center gap-2">
                  {hotkey.accelerator !== hotkey.default && (
                    <button
                      type="button"
                      aria-label={`Reset ${hotkey.label}`}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => void apply(hotkey.id, hotkey.default)}
                      className="rounded-[6px] px-2 py-1 text-[11px] text-white/55 hover:text-white"
                    >
                      Reset
                    </button>
                  )}
                  <button
                    type="button"
                    aria-label={shortcutLabel(hotkey.label, hotkey.accelerator, active)}
                    onClick={() => (active ? endCapture() : startCapture(hotkey.id))}
                    onKeyDown={(e) => {
                      if (active) bind(hotkey.id, e);
                    }}
                    onBlur={() => {
                      if (active) endCapture();
                    }}
                    className={`ov-mono rounded-[6px] px-[9px] py-1 text-[11px] transition-colors duration-150 ${
                      active
                        ? "bg-white/[0.16] text-white ring-1 ring-white/30"
                        : blocked.includes(hotkey.accelerator)
                          ? "bg-[#ef4444]/15 text-[#fca5a5] hover:bg-[#ef4444]/25"
                          : "bg-white/[0.08] text-white/75 hover:bg-white/[0.14]"
                    }`}
                  >
                    {active ? "Press a shortcut…" : spacedAccelerator(hotkey.accelerator)}
                  </button>
                </div>
              </div>
            );
          })}
        </OvList>
      </section>

      {modsUsable && loaderKeys.length > 0 && (
        <section className="flex flex-col gap-2">
          <OvLabel>Mod loader shortcuts</OvLabel>
          <OvList>
            {loaderKeys.map((row) => {
              const active = capturing === row.id;
              const shown = row.accelerator ?? "";
              return (
                <div
                  key={row.id}
                  className="flex items-center justify-between gap-4 border-b border-white/[0.055] px-4 py-3"
                >
                  <div className="min-w-0">
                    <div className="text-[12.5px] text-white/[0.78]">{row.label}</div>
                    <div className="mt-[3px] max-w-[54ch] text-[11.5px] leading-[1.45] text-white/55">
                      {row.description} It takes effect the next time the game starts.
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    {row.defaultAccelerator && shown !== row.defaultAccelerator && (
                      <button
                        type="button"
                        aria-label={`Reset ${row.label}`}
                        onMouseDown={(e) => e.preventDefault()}
                        onClick={() => void applyLoader(row.id, row.defaultAccelerator ?? "")}
                        className="rounded-[6px] px-2 py-1 text-[11px] text-white/55 hover:text-white"
                      >
                        Reset
                      </button>
                    )}
                    <button
                      type="button"
                      aria-label={shortcutLabel(row.label, shown, active)}
                      onClick={() => (active ? endCapture() : startCapture(row.id))}
                      onKeyDown={(e) => {
                        if (active) bind(row.id, e, true);
                      }}
                      onBlur={() => {
                        if (active) endCapture();
                      }}
                      className={`ov-mono rounded-[6px] px-[9px] py-1 text-[11px] transition-colors duration-150 ${
                        active
                          ? "bg-white/[0.16] text-white ring-1 ring-white/30"
                          : "bg-white/[0.08] text-white/75 hover:bg-white/[0.14]"
                      }`}
                    >
                      {active ? "Press a shortcut…" : spacedAccelerator(shown)}
                    </button>
                  </div>
                </div>
              );
            })}
          </OvList>
        </section>
      )}

      {capturing && (
        <p className="text-[11.5px] leading-relaxed text-white/55">
          Every Peebify shortcut is released while you pick one, so the key reaches this field
          instead of the game. Press Escape to keep the current one.
        </p>
      )}

      {rejected && (
        <div
          role="alert"
          className="rounded-[10px] border border-[#ef4444]/30 bg-[#ef4444]/[0.08] px-4 py-3 text-[12.5px] text-[#fca5a5]"
        >
          {rejected}
        </div>
      )}

      {warning && (
        <div className="flex items-start gap-3 rounded-[10px] border border-[#f59e0b]/30 bg-[#f59e0b]/[0.08] px-4 py-3">
          <AlertTriangle size={16} className="mt-[2px] shrink-0 text-[#fcd34d]" />
          <p className="text-[12.5px] leading-relaxed text-[#fcd34d]">{warning}</p>
        </div>
      )}

      <div className="flex items-center justify-between gap-4 rounded-[10px] border border-white/[0.08] bg-white/[0.045] px-4 py-[14px]">
        <div>
          <div className="text-[13px] font-medium">Everything else</div>
          <div className="mt-[3px] text-[11.5px] text-white/55">
            Open the full launcher settings in the window behind the game.
          </div>
        </div>
        <button
          type="button"
          onClick={() => void rpc("overlay-open-launcher")}
          className="shrink-0 rounded-[8px] border border-white/15 bg-white/[0.06] px-[13px] py-[7px] text-[12px] font-medium hover:bg-white/10"
        >
          Open launcher
        </button>
      </div>
    </div>
  );
}
